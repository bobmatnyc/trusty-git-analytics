//! `tga eval repredict`: re-derive an existing sample's predictions under a
//! given config (#111).
//!
//! Why: `tga eval score` scores the `predicted_category` frozen in
//! `sample.jsonl`, so a sample drawn and labelled under old rules could not be
//! scored against new rules without drawing, and labelling, a new sample.
//! What: [`run_repredict`] reads each row's commit from a read-only database
//! copy, re-classifies it with the config's rules exactly as `tga eval sample`
//! does ([`super::verdict::resolve_verdicts`]), and writes the sample back with
//! only `predicted_category`, `method`, `rule_id` and `confidence` replaced.
//! Rows keep their order, SHA and stratum. A provenance file next to the output
//! records the config and rules files with their BLAKE3 hashes and the tga
//! version.
//! Test: `tests/eval_harness.rs::repredict_keeps_rows_and_follows_the_new_rules`,
//! `tests/eval_harness.rs::repredict_refuses_a_sha_missing_from_the_db`.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::population::{load_commits, CommitRow};
use super::records::{SampleRecord, Stratum};
use super::sample::{write_json, write_jsonl};
use super::score::read_sample;
use super::subsample::{create_private_dir, create_private_file};
use super::verdict::{resolve_verdicts, CarryPolicy};
use super::{io_err, open_eval_db, EvalError, Result};
use crate::classify::ClassificationPipeline;
use crate::core::config::Config;

/// Inputs to [`run_repredict`].
#[derive(Debug, Clone)]
pub struct RepredictParams {
    /// `sample.jsonl` whose predictions are re-derived.
    pub sample: PathBuf,
    /// Database copy the sample was drawn from (opened read-only).
    pub db: PathBuf,
    /// Configuration whose rules produce the new predictions.
    pub config: Config,
    /// File `config` was loaded from; hashed into the provenance.
    pub config_path: PathBuf,
    /// Output `.jsonl`; must not exist, nor its provenance file.
    pub out: PathBuf,
}

/// A file named in the provenance, with the BLAKE3 hash of its bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HashedFile {
    /// Absolute path when it could be resolved, else as given.
    pub path: String,
    /// Hex BLAKE3 of the file's contents.
    pub blake3: String,
}

/// Contents of the provenance file written beside the output.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    /// `tga` version that re-derived the predictions.
    pub tga_version: String,
    /// The config file.
    pub config: HashedFile,
    /// The rules files the config names, in load order.
    pub rules_files: Vec<HashedFile>,
    /// The sample whose rows were re-predicted.
    pub source_sample: HashedFile,
    /// Database the commits were read from.
    pub db: String,
    /// Rows written.
    pub rows: u64,
    /// Rows whose `predicted_category` changed.
    pub changed: u64,
    /// Rows that now predict no category (`uncategorized`/`unknown`, or no
    /// tier matched); `tga eval score` treats them as it always has.
    pub abstentions: u64,
    /// Rows whose stored verdict was carried from the database rather than
    /// re-derived, by method (`manual`, `llm`, `external_source`,
    /// `repo_category`). #111: only tiers the config's cascade still reaches.
    pub carried: BTreeMap<String, u64>,
    /// Rows whose stored manual, LLM, external or repo-fallback verdict the
    /// config no longer reaches, so the re-derived verdict replaced it.
    pub superseded: u64,
}

/// What [`run_repredict`] wrote.
#[derive(Debug, Clone)]
pub struct RepredictSummary {
    /// Contents of the provenance file.
    pub provenance: Provenance,
    /// Files written: the sample, then the provenance.
    pub files: Vec<PathBuf>,
}

/// Provenance file for `out`: `x.jsonl` → `x.provenance.json`.
pub fn provenance_path(out: &Path) -> PathBuf {
    out.with_extension("provenance.json")
}

fn hashed(path: &Path) -> Result<HashedFile> {
    let bytes = fs::read(path).map_err(io_err(path))?;
    let shown = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    Ok(HashedFile {
        path: shown.display().to_string(),
        blake3: blake3::hash(&bytes).to_hex().to_string(),
    })
}

/// Re-derive every row's prediction and write the new sample and provenance.
///
/// Why: see the module doc. What: looks each row up by `(sha, repo)` in `db`,
/// resolves its verdict with the rule engine built from `config` — message and
/// merge flag, the inputs `tga classify` gives the cascade — and replaces the
/// four prediction fields. `stratum`, `weight` and every commit field are kept;
/// a row without `is_merge` takes it from the database. Nothing is written to
/// the database.
/// Test: `tests/eval_harness.rs::repredict_keeps_rows_and_follows_the_new_rules`,
/// `tests/eval_harness.rs::repredict_refuses_a_sha_missing_from_the_db`.
///
/// # Errors
///
/// [`EvalError::Invalid`] for an empty sample, a row whose commit is not in
/// `db`, or an output or provenance file that already exists; database,
/// rule-engine and I/O failures.
pub fn run_repredict(params: &RepredictParams) -> Result<RepredictSummary> {
    let sample = read_sample(&params.sample)?;
    if sample.is_empty() {
        return Err(EvalError::Invalid(format!(
            "{} holds no rows",
            params.sample.display()
        )));
    }
    let prov_path = provenance_path(&params.out);
    for f in [&params.out, &prov_path] {
        if f.exists() {
            return Err(EvalError::Invalid(format!(
                "{} already exists; pick a new --out",
                f.display()
            )));
        }
    }

    let conn = open_eval_db(&params.db)?;
    let all = load_commits(&conn)?;
    let by_key: HashMap<(&str, &str), &CommitRow> = all
        .iter()
        .map(|c| ((c.sha.as_str(), c.repo.as_str()), c))
        .collect();
    let found: Vec<Option<&CommitRow>> = sample
        .iter()
        .map(|r| by_key.get(&(r.sha.as_str(), r.repo.as_str())).copied())
        .collect();
    // #111: fail closed — a row that cannot be re-derived is never skipped.
    let missing: Vec<&SampleRecord> = sample
        .iter()
        .zip(&found)
        .filter(|(_, c)| c.is_none())
        .map(|(r, _)| r)
        .collect();
    if let Some(first) = missing.first() {
        return Err(EvalError::Invalid(format!(
            "{} sample rows are not in {} (first: {} in {}); pass the database the sample \
             was drawn from",
            missing.len(),
            params.db.display(),
            first.sha,
            first.repo
        )));
    }
    let commits: Vec<&CommitRow> = found.into_iter().flatten().collect();

    let engine = ClassificationPipeline::new(params.config.clone()).build_rule_engine()?;
    let policy = CarryPolicy::from_config(&params.config)?;
    let (resolved, _drifted) = resolve_verdicts(&engine, &policy, &commits);

    let (mut changed, mut abstentions, mut superseded) = (0u64, 0u64, 0u64);
    let mut carried: BTreeMap<String, u64> = BTreeMap::new();
    let records: Vec<SampleRecord> = sample
        .iter()
        .zip(&commits)
        .zip(&resolved)
        .map(|((r, c), v)| {
            changed += u64::from(r.predicted_category != v.category);
            // #111: abstention means what it means in `tga eval sample`.
            abstentions +=
                u64::from(Stratum::classify(v.tier, &v.category, v.confidence) == Stratum::Unknown);
            if v.carried {
                *carried.entry(v.tier.as_str().to_string()).or_default() += 1;
            }
            superseded += u64::from(v.superseded);
            SampleRecord {
                method: v.tier.as_str().to_string(),
                rule_id: v.rule_id.clone(),
                predicted_category: v.category.clone(),
                confidence: v.confidence,
                is_merge: r.is_merge.or(Some(c.is_merge)),
                ..r.clone()
            }
        })
        .collect();

    let rules_files = params
        .config
        .classification
        .as_ref()
        .map(|c| {
            c.rules_files
                .iter()
                .map(|p| hashed(p))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?
        .unwrap_or_default();
    let provenance = Provenance {
        tga_version: env!("CARGO_PKG_VERSION").to_string(),
        config: hashed(&params.config_path)?,
        rules_files,
        source_sample: hashed(&params.sample)?,
        db: params.db.display().to_string(),
        rows: records.len() as u64,
        changed,
        abstentions,
        carried,
        superseded,
    };

    if let Some(dir) = params.out.parent().filter(|d| !d.as_os_str().is_empty()) {
        create_private_dir(dir)?;
    }
    create_private_file(&params.out)?;
    // #111 review: a failed write removes the files this run created, so a
    // retry is not refused with "already exists".
    let mut created = vec![params.out.as_path()];
    let written = create_private_file(&prov_path).and_then(|()| {
        created.push(prov_path.as_path());
        write_jsonl(&params.out, &records)?;
        write_json(&prov_path, &provenance)
    });
    if let Err(e) = written {
        for f in created {
            let _ = fs::remove_file(f);
        }
        return Err(e);
    }
    Ok(RepredictSummary {
        provenance,
        files: vec![params.out.clone(), prov_path],
    })
}

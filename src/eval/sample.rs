//! `tga eval sample`: re-classify a window with rule tracing and draw a
//! stratified sample for human labelling.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write as _};
use std::path::{Path, PathBuf};

use chrono::Duration;
use tracing::{info, warn};

use super::draw::{draw, Candidate, DrawParams};
use super::population::{load_commits, load_issue_types, load_paths, load_pr_titles, CommitRow};
use super::records::{Diffstat, SampleRecord, StrataSummary, Stratum, StratumCounts};
use super::redact::{redact_emails, strip_trailers};
use super::verdict::resolve_verdicts;
use super::{io_err, open_eval_db, EvalError, Result};
use crate::classify::ClassificationPipeline;
use crate::core::config::Config;

/// Characters of the message body shown to raters.
const BODY_EXCERPT: usize = 600;
/// Paths shown to raters.
const PATHS_EXCERPT: usize = 8;

/// Inputs to [`run_sample`].
#[derive(Debug, Clone)]
pub struct SampleParams {
    /// Database copy to read (opened read-only).
    pub db: PathBuf,
    /// Configuration whose rules are evaluated.
    pub config: Config,
    /// Window length in weeks, ending at the newest commit.
    pub weeks: u32,
    /// Total sample size.
    pub size: usize,
    /// RNG seed.
    pub seed: u64,
    /// Per-repo and per-author cap within a stratum.
    pub cap: usize,
    /// Salt for author hashes; generated when `None`.
    pub salt: Option<String>,
    /// Output directory (created if missing).
    pub out: PathBuf,
}

/// What [`run_sample`] wrote.
#[derive(Debug, Clone)]
pub struct SampleSummary {
    /// Contents of `strata.json`.
    pub strata: StrataSummary,
    /// Files written.
    pub files: Vec<PathBuf>,
    /// Path of the generated salt file, when the salt was generated.
    pub salt_file: Option<PathBuf>,
    /// Rule-engine commits whose stored category differs from today's rules.
    pub drifted: u64,
    /// Commits skipped for an unparseable timestamp.
    pub bad_timestamps: u64,
}

fn author_hash(salt: &str, email: &str) -> String {
    let mut h = blake3::Hasher::new();
    h.update(salt.as_bytes());
    h.update(&[0]);
    h.update(email.trim().to_lowercase().as_bytes());
    h.finalize().to_hex()[..16].to_string()
}

fn excerpt(s: &str, max: usize) -> String {
    let mut out: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        out.push('…');
    }
    out
}

/// Draw the sample and write `sample.jsonl`, `labels.csv` and `strata.json`.
///
/// Why: see [`crate::eval`]. What: opens `params.db` read-only, keeps commits
/// in the `weeks` window ending at the newest commit (so a rerun on the same
/// copy draws the same sample), re-classifies them with
/// [`ClassificationPipeline::build_rule_engine`] and the traced batch API,
/// takes manual / external / LLM / repo-fallback verdicts from the stored
/// `method`, stratifies, and draws with [`draw`]. Merge commits (2+ parents)
/// in the window are counted in `merges_excluded` and never drawn (#111).
/// Test: `tests/eval_harness.rs::sample_then_score_end_to_end`,
/// `tests/eval_harness.rs::sample_never_draws_a_merge`.
///
/// # Errors
///
/// Database, rule-engine and I/O failures; [`EvalError::Invalid`] for a zero
/// size or cap, or a window without commits.
pub fn run_sample(params: &SampleParams) -> Result<SampleSummary> {
    if params.size == 0 || params.cap == 0 {
        return Err(EvalError::Invalid(
            "--size and --cap must be at least 1".into(),
        ));
    }
    let conn = open_eval_db(&params.db)?;
    let all = load_commits(&conn)?;
    let bad_timestamps = all.iter().filter(|c| c.ts.is_none()).count() as u64;
    let end = all
        .iter()
        .filter_map(|c| c.ts)
        .max()
        .ok_or_else(|| EvalError::Invalid("the database holds no commits".into()))?;
    let start = end - Duration::weeks(i64::from(params.weeks));
    let mut window: Vec<CommitRow> = all
        .into_iter()
        .filter(|c| c.ts.is_some_and(|t| t >= start))
        .collect();
    // #111: a commit with 2+ parents is a merge and never enters the eval.
    // Squash and rebase commits have one parent and stay in.
    let merges_excluded = window.iter().filter(|c| c.is_merge).count() as u64;
    window.retain(|c| !c.is_merge);
    info!(commits = window.len(), merges_excluded, %start, %end, "eval window");

    let engine = ClassificationPipeline::new(params.config.clone()).build_rule_engine()?;
    // #111: `sample` and `repredict` share one verdict resolution.
    let refs: Vec<&CommitRow> = window.iter().collect();
    let (resolved, drifted) = resolve_verdicts(&engine, &refs);
    if drifted > 0 {
        warn!(
            drifted,
            "stored categories differ from the current rules; the sample evaluates the current rules"
        );
    }

    let candidates: Vec<Candidate> = window
        .iter()
        .zip(&resolved)
        .map(|(c, r)| Candidate {
            sha: c.sha.clone(),
            repo: c.repo.clone(),
            author: c.author_email.trim().to_lowercase(),
            stratum: Stratum::classify(r.tier, &r.category, r.confidence),
        })
        .collect();
    let drawn = draw(
        &candidates,
        &DrawParams {
            size: params.size,
            cap: params.cap,
            seed: params.seed,
        },
    );

    let (salt, salt_generated) = match &params.salt {
        Some(s) => (s.clone(), false),
        None => (uuid::Uuid::new_v4().simple().to_string(), true),
    };

    let ids: Vec<i64> = drawn.selected.iter().map(|&i| window[i].id).collect();
    let wanted: HashSet<&str> = drawn
        .selected
        .iter()
        .map(|&i| window[i].sha.as_str())
        .collect();
    let paths = load_paths(&conn, &ids)?;
    let prs = load_pr_titles(&conn, &wanted)?;
    let issue_types = load_issue_types(&conn, &wanted)?;

    let records: Vec<SampleRecord> = drawn
        .selected
        .iter()
        .map(|&i| {
            let c = &window[i];
            let r = &resolved[i];
            let stratum = candidates[i].stratum;
            let pop = drawn.population.get(&stratum).copied().unwrap_or(0) as f64;
            let n = drawn.sampled.get(&stratum).copied().unwrap_or(1).max(1) as f64;
            let (subject, body) = c.message.split_once('\n').unwrap_or((&c.message, ""));
            SampleRecord {
                sha: c.sha.clone(),
                repo: c.repo.clone(),
                date: c.timestamp.clone(),
                author_hash: author_hash(&salt, &c.author_email),
                subject: subject.trim().to_string(),
                body: body.trim().to_string(),
                paths: paths.get(&c.id).cloned().unwrap_or_default(),
                diffstat: Diffstat {
                    files: c.files,
                    insertions: c.insertions,
                    deletions: c.deletions,
                },
                pr_title: prs.get(&c.sha).cloned(),
                ticket_id: c.ticket_id.clone(),
                issue_type: issue_types.get(&c.sha).cloned(),
                stratum,
                method: r.tier.as_str().to_string(),
                rule_id: r.rule_id.clone(),
                predicted_category: r.category.clone(),
                confidence: r.confidence,
                weight: pop / n,
                is_merge: Some(c.is_merge),
            }
        })
        .collect();

    let mut categories: BTreeMap<String, String> = BTreeMap::new();
    let names = engine.taxonomy().all().iter().map(|d| d.name.clone());
    for name in names.chain(resolved.iter().map(|r| r.category.clone())) {
        categories.entry(name.to_lowercase()).or_insert(name);
    }
    for label in super::score::NO_ANSWER_LABELS {
        categories.remove(label);
    }

    let strata = StrataSummary {
        seed: params.seed,
        weeks: params.weeks,
        window_start: start.to_rfc3339(),
        window_end: end.to_rfc3339(),
        requested_size: params.size as u64,
        cap: params.cap as u64,
        population: window.len() as u64,
        merges_excluded,
        strata: Stratum::ALL
            .iter()
            .map(|s| {
                let counts = StratumCounts {
                    population: drawn.population.get(s).copied().unwrap_or(0) as u64,
                    sampled: drawn.sampled.get(s).copied().unwrap_or(0) as u64,
                };
                (s.as_str().to_string(), counts)
            })
            .collect(),
        categories: categories.into_keys().collect(),
        subsample: None,
    };

    fs::create_dir_all(&params.out).map_err(io_err(&params.out))?;
    let mut files = vec![
        write_jsonl(&params.out.join("sample.jsonl"), &records)?,
        write_labels(&params.out.join("labels.csv"), &records, &salt)?,
        write_json(&params.out.join("strata.json"), &strata)?,
    ];
    let salt_file = if salt_generated {
        let p = params.out.join("salt.txt");
        fs::write(&p, format!("{salt}\n")).map_err(io_err(&p))?;
        files.push(p.clone());
        Some(p)
    } else {
        None
    };
    Ok(SampleSummary {
        strata,
        files,
        salt_file,
        drifted,
        bad_timestamps,
    })
}

pub(crate) fn write_jsonl(path: &Path, records: &[SampleRecord]) -> Result<PathBuf> {
    let file = fs::File::create(path).map_err(io_err(path))?;
    let mut w = BufWriter::new(file);
    for r in records {
        serde_json::to_writer(&mut w, r).map_err(|source| EvalError::Json {
            path: path.to_path_buf(),
            source,
        })?;
        w.write_all(b"\n").map_err(io_err(path))?;
    }
    w.flush().map_err(io_err(path))?;
    Ok(path.to_path_buf())
}

pub(crate) fn write_json<T: serde::Serialize>(path: &Path, value: &T) -> Result<PathBuf> {
    let text = serde_json::to_string_pretty(value).map_err(|source| EvalError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    fs::write(path, text + "\n").map_err(io_err(path))?;
    Ok(path.to_path_buf())
}

/// Rater sheet: the prediction is hidden, and rows are ordered by a salted
/// hash of the SHA so the stratum cannot be read off the row order.
pub(crate) fn write_labels(path: &Path, records: &[SampleRecord], salt: &str) -> Result<PathBuf> {
    let csv_err = |source| EvalError::Csv {
        path: path.to_path_buf(),
        source,
    };
    let mut order: Vec<&SampleRecord> = records.iter().collect();
    order.sort_by_cached_key(|r| blake3::hash(format!("{salt}\0{}", r.sha).as_bytes()).to_hex());
    let mut w = csv::Writer::from_path(path).map_err(csv_err)?;
    w.write_record([
        "sha",
        "repo",
        "subject",
        "body_excerpt",
        "paths_excerpt",
        "pr_title",
        "issue_type",
        "label",
        "note",
    ])
    .map_err(csv_err)?;
    for r in order {
        let mut paths = r
            .paths
            .iter()
            .take(PATHS_EXCERPT)
            .cloned()
            .collect::<Vec<_>>()
            .join("; ");
        if r.paths.len() > PATHS_EXCERPT {
            paths.push_str(&format!("; (+{} more)", r.paths.len() - PATHS_EXCERPT));
        }
        w.write_record([
            r.sha.as_str(),
            r.repo.as_str(),
            &redact_emails(&r.subject),
            // #111: the sheet names nobody; sample.jsonl keeps the full body.
            &excerpt(&redact_emails(&strip_trailers(&r.body)), BODY_EXCERPT),
            &redact_emails(&paths),
            &redact_emails(r.pr_title.as_deref().unwrap_or("")),
            r.issue_type.as_deref().unwrap_or(""),
            "",
            "",
        ])
        .map_err(csv_err)?;
    }
    w.flush().map_err(io_err(path))?;
    Ok(path.to_path_buf())
}

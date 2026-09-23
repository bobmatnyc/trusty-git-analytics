//! `tga eval score`: join rater labels to a sample and measure precision.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::records::{SampleRecord, StrataSummary, Stratum};
use super::sample::write_json;
use super::stats::{cohen_kappa, wilson_interval, Kappa, Z_95};
use super::{io_err, EvalError, Result};

/// Label meaning the rater could not decide.
pub const UNCLEAR: &str = "unclear";
/// Label meaning the commit spans several categories.
pub const MIXED: &str = "mixed";

/// Inputs to [`run_score`].
#[derive(Debug, Clone)]
pub struct ScoreParams {
    /// `sample.jsonl` from `tga eval sample`.
    pub sample: PathBuf,
    /// `strata.json`; defaults to the file next to the sample.
    pub strata: Option<PathBuf>,
    /// One or two rater files (`sha`, `label`, optional `note`).
    pub labels: Vec<PathBuf>,
    /// Adjudicated labels; they win over the raters' where non-empty.
    pub adjudicated: Option<PathBuf>,
    /// Valid categories from a config; `None` uses those in `strata.json`.
    pub categories: Option<Vec<String>>,
    /// Output directory for `report.md` and `report.json`.
    pub out: PathBuf,
}

/// Precision of one group (rule, method or stratum).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrecisionRow {
    /// Group key.
    pub key: String,
    /// Labelled commits scored (unclear/mixed excluded).
    pub n: u64,
    /// Of those, labelled with the predicted category.
    pub correct: u64,
    /// `correct / n`; `None` when `n == 0`.
    pub precision: Option<f64>,
    /// Wilson 95% lower bound.
    pub ci_low: Option<f64>,
    /// Wilson 95% upper bound.
    pub ci_high: Option<f64>,
    /// Labels of `unclear` or `mixed` in this group.
    pub excluded: u64,
}

/// One point of the coverage-at-precision curve.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoveragePoint {
    /// Minimum confidence kept.
    pub threshold: f64,
    /// Weighted share of the population at or above the threshold.
    pub coverage: f64,
    /// Weighted precision of scored commits at or above it.
    pub precision: Option<f64>,
    /// Scored commits at or above it.
    pub n: u64,
}

/// Stratum-weighted accuracy with a normal-approximation 95% interval.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedAccuracy {
    /// Σ W_h · p_h over strata with scored labels.
    pub estimate: f64,
    /// Lower bound.
    pub ci_low: f64,
    /// Upper bound.
    pub ci_high: f64,
    /// Share of the population the estimate covers (strata with labels).
    pub population_covered: f64,
}

/// Share of the population the cascade abstains on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Abstention {
    /// Catch-all population.
    pub catch_all: u64,
    /// Uncategorized / Unknown population.
    pub unknown: u64,
    /// Window population.
    pub population: u64,
    /// `(catch_all + unknown) / population`.
    pub share: f64,
}

/// Everything `report.json` holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScoreReport {
    /// Sampled commits.
    pub sample_size: u64,
    /// Commits with a final label.
    pub labelled: u64,
    /// Commits scored for precision.
    pub scored: u64,
    /// Final label `unclear`.
    pub unclear: u64,
    /// Final label `mixed`.
    pub mixed: u64,
    /// Two raters disagreed and no adjudication resolved it.
    pub unresolved_disagreements: u64,
    /// Precision per rule id.
    pub per_rule: Vec<PrecisionRow>,
    /// Precision per method (tier).
    pub per_method: Vec<PrecisionRow>,
    /// Precision per stratum.
    pub per_stratum: Vec<PrecisionRow>,
    /// Stratum-weighted accuracy; `None` without scored labels.
    pub weighted_accuracy: Option<WeightedAccuracy>,
    /// Coverage at each confidence threshold present in the sample.
    pub coverage_curve: Vec<CoveragePoint>,
    /// Counts by predicted category, then final label.
    pub confusion: BTreeMap<String, BTreeMap<String, u64>>,
    /// Abstention share over the population.
    pub abstention: Abstention,
    /// Inter-rater agreement, when two label files were given.
    pub kappa: Option<Kappa>,
}

#[derive(Debug, Deserialize)]
struct LabelRow {
    sha: String,
    #[serde(default)]
    label: String,
}

fn read_labels(path: &Path, valid: &BTreeSet<String>) -> Result<BTreeMap<String, String>> {
    let csv_err = |source| EvalError::Csv {
        path: path.to_path_buf(),
        source,
    };
    let mut reader = csv::Reader::from_path(path).map_err(csv_err)?;
    let mut out = BTreeMap::new();
    for row in reader.deserialize::<LabelRow>() {
        let row = row.map_err(csv_err)?;
        let label = row.label.trim().to_lowercase();
        if label.is_empty() {
            continue;
        }
        if !valid.contains(&label) {
            return Err(EvalError::Invalid(format!(
                "{}: label {label:?} for {} is not a known category, `unclear` or `mixed`",
                path.display(),
                row.sha
            )));
        }
        out.insert(row.sha.trim().to_string(), label);
    }
    Ok(out)
}

fn read_sample(path: &Path) -> Result<Vec<SampleRecord>> {
    let text = fs::read_to_string(path).map_err(io_err(path))?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            serde_json::from_str(l).map_err(|source| EvalError::Json {
                path: path.to_path_buf(),
                source,
            })
        })
        .collect()
}

fn precision_rows<'a>(
    rows: impl Iterator<Item = (String, Option<bool>)> + 'a,
) -> Vec<PrecisionRow> {
    let mut acc: BTreeMap<String, (u64, u64, u64)> = BTreeMap::new();
    for (key, outcome) in rows {
        let e = acc.entry(key).or_default();
        match outcome {
            Some(correct) => {
                e.0 += 1;
                e.1 += u64::from(correct);
            }
            None => e.2 += 1,
        }
    }
    acc.into_iter()
        .map(|(key, (n, correct, excluded))| {
            let ci = wilson_interval(correct, n, Z_95);
            PrecisionRow {
                key,
                n,
                correct,
                precision: (n > 0).then(|| correct as f64 / n as f64),
                ci_low: ci.map(|c| c.0),
                ci_high: ci.map(|c| c.1),
                excluded,
            }
        })
        .collect()
}

/// Score the labels and write `report.md` and `report.json`.
///
/// Why: see [`crate::eval`]. What: final label per SHA is the adjudicated
/// one, else the single rater's, else the two raters' shared label; a
/// disagreement stays unresolved and unscored. `unclear` and `mixed` are
/// counted but excluded from precision. Precision rows use Wilson 95%
/// intervals; the weighted accuracy is the stratified estimator
/// Σ W_h · p_h with W_h from `strata.json`.
/// Test: `tests/eval_harness.rs::score_computes_expected_metrics`.
///
/// # Errors
///
/// I/O and parse failures; [`EvalError::Invalid`] for an unknown label, a
/// label for a SHA outside the sample, or more than two rater files.
pub fn run_score(params: &ScoreParams) -> Result<ScoreReport> {
    if params.labels.is_empty() || params.labels.len() > 2 {
        return Err(EvalError::Invalid("pass one or two --labels files".into()));
    }
    let sample = read_sample(&params.sample)?;
    let strata_path = params.strata.clone().unwrap_or_else(|| {
        params
            .sample
            .parent()
            .unwrap_or(Path::new("."))
            .join("strata.json")
    });
    let strata_text = fs::read_to_string(&strata_path).map_err(io_err(&strata_path))?;
    let strata: StrataSummary =
        serde_json::from_str(&strata_text).map_err(|source| EvalError::Json {
            path: strata_path.clone(),
            source,
        })?;

    let mut valid: BTreeSet<String> = params
        .categories
        .clone()
        .unwrap_or_else(|| strata.categories.clone())
        .into_iter()
        .chain(sample.iter().map(|r| r.predicted_category.clone()))
        .map(|c| c.to_lowercase())
        .collect();
    valid.insert(UNCLEAR.into());
    valid.insert(MIXED.into());

    let raters: Vec<BTreeMap<String, String>> = params
        .labels
        .iter()
        .map(|p| read_labels(p, &valid))
        .collect::<Result<_>>()?;
    let adjudicated = match &params.adjudicated {
        Some(p) => read_labels(p, &valid)?,
        None => BTreeMap::new(),
    };
    let in_sample: BTreeSet<&str> = sample.iter().map(|r| r.sha.as_str()).collect();
    for sha in raters.iter().chain([&adjudicated]).flat_map(|m| m.keys()) {
        if !in_sample.contains(sha.as_str()) {
            return Err(EvalError::Invalid(format!(
                "label for {sha}, which is not in the sample"
            )));
        }
    }

    let kappa = if raters.len() == 2 {
        let pairs: Vec<(String, String)> = raters[0]
            .iter()
            .filter_map(|(sha, a)| raters[1].get(sha).map(|b| (a.clone(), b.clone())))
            .collect();
        cohen_kappa(&pairs)
    } else {
        None
    };

    // Final label per record: Some(label), or None when unlabelled/unresolved.
    let mut unresolved = 0u64;
    let finals: Vec<Option<String>> = sample
        .iter()
        .map(|r| {
            if let Some(l) = adjudicated.get(&r.sha) {
                return Some(l.clone());
            }
            let given: Vec<&String> = raters.iter().filter_map(|m| m.get(&r.sha)).collect();
            match given.as_slice() {
                [one] => Some((*one).clone()),
                [a, b] if a == b => Some((*a).clone()),
                [_, _] => {
                    unresolved += 1;
                    None
                }
                _ => None,
            }
        })
        .collect();

    // outcome: Some(Some(correct)) scored, Some(None) unclear/mixed, None unlabelled.
    let outcomes: Vec<Option<Option<bool>>> = sample
        .iter()
        .zip(&finals)
        .map(|(r, l)| {
            l.as_ref().map(|l| {
                (l != UNCLEAR && l != MIXED).then(|| l.eq_ignore_ascii_case(&r.predicted_category))
            })
        })
        .collect();
    let labelled_rows = || {
        sample
            .iter()
            .zip(&outcomes)
            .filter_map(|(r, o)| o.map(|o| (r, o)))
    };

    let per_rule = precision_rows(labelled_rows().map(|(r, o)| (r.rule_id.clone(), o)));
    let per_method = precision_rows(labelled_rows().map(|(r, o)| (r.method.clone(), o)));
    let per_stratum =
        precision_rows(labelled_rows().map(|(r, o)| (r.stratum.as_str().to_string(), o)));

    let weighted_accuracy = weighted_accuracy(&per_stratum, &strata);
    let coverage_curve = coverage_curve(&sample, &outcomes);

    let mut confusion: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for (r, l) in sample.iter().zip(&finals) {
        if let Some(l) = l {
            *confusion
                .entry(r.predicted_category.to_lowercase())
                .or_default()
                .entry(l.clone())
                .or_default() += 1;
        }
    }

    let catch_all = strata.population_of(Stratum::CatchAll);
    let unknown = strata.population_of(Stratum::Unknown);
    let abstention = Abstention {
        catch_all,
        unknown,
        population: strata.population,
        share: if strata.population == 0 {
            0.0
        } else {
            (catch_all + unknown) as f64 / strata.population as f64
        },
    };

    let count = |f: &dyn Fn(&str) -> bool| finals.iter().flatten().filter(|l| f(l)).count() as u64;
    let report = ScoreReport {
        sample_size: sample.len() as u64,
        labelled: finals.iter().flatten().count() as u64,
        scored: outcomes
            .iter()
            .filter(|o| matches!(o, Some(Some(_))))
            .count() as u64,
        unclear: count(&|l| l == UNCLEAR),
        mixed: count(&|l| l == MIXED),
        unresolved_disagreements: unresolved,
        per_rule,
        per_method,
        per_stratum,
        weighted_accuracy,
        coverage_curve,
        confusion,
        abstention,
        kappa,
    };

    fs::create_dir_all(&params.out).map_err(io_err(&params.out))?;
    write_json(&params.out.join("report.json"), &report)?;
    let md_path = params.out.join("report.md");
    fs::write(&md_path, super::report_md::render(&report, &strata)).map_err(io_err(&md_path))?;
    Ok(report)
}

fn weighted_accuracy(
    per_stratum: &[PrecisionRow],
    strata: &StrataSummary,
) -> Option<WeightedAccuracy> {
    let scored: Vec<(f64, f64, f64)> = per_stratum
        .iter()
        .filter(|row| row.n > 0)
        .map(|row| {
            let pop = strata
                .strata
                .get(&row.key)
                .map(|c| c.population)
                .unwrap_or(0) as f64;
            (pop, row.correct as f64 / row.n as f64, row.n as f64)
        })
        .collect();
    let covered: f64 = scored.iter().map(|s| s.0).sum();
    if covered <= 0.0 {
        return None;
    }
    let estimate: f64 = scored.iter().map(|(pop, p, _)| pop / covered * p).sum();
    let variance: f64 = scored
        .iter()
        .map(|(pop, p, n)| (pop / covered).powi(2) * p * (1.0 - p) / n)
        .sum();
    let half = Z_95 * variance.sqrt();
    Some(WeightedAccuracy {
        estimate,
        ci_low: (estimate - half).max(0.0),
        ci_high: (estimate + half).min(1.0),
        population_covered: if strata.population == 0 {
            0.0
        } else {
            covered / strata.population as f64
        },
    })
}

fn coverage_curve(
    sample: &[SampleRecord],
    outcomes: &[Option<Option<bool>>],
) -> Vec<CoveragePoint> {
    let mut thresholds: Vec<f64> = sample.iter().map(|r| r.confidence).collect();
    thresholds.sort_by(f64::total_cmp);
    thresholds.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let total_weight: f64 = sample.iter().map(|r| r.weight).sum();
    thresholds
        .into_iter()
        .map(|t| {
            let (mut kept_weight, mut w_scored, mut w_correct, mut n) = (0.0, 0.0, 0.0, 0u64);
            for (r, o) in sample.iter().zip(outcomes) {
                if r.confidence < t - 1e-9 {
                    continue;
                }
                kept_weight += r.weight;
                if let Some(Some(correct)) = o {
                    w_scored += r.weight;
                    w_correct += if *correct { r.weight } else { 0.0 };
                    n += 1;
                }
            }
            CoveragePoint {
                threshold: t,
                coverage: if total_weight > 0.0 {
                    kept_weight / total_weight
                } else {
                    0.0
                },
                precision: (w_scored > 0.0).then(|| w_correct / w_scored),
                n,
            }
        })
        .collect()
}

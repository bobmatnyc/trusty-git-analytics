//! Classifier precision harness (#111): `tga eval sample`, `tga eval
//! subsample` and `tga eval score`.
//!
//! Why: the classification cascade reports a category and a confidence for
//! every commit, but nothing measured how often those verdicts are right. A
//! precision number per rule tells an operator which rules to trust, which to
//! fix, and how much of the population the cascade abstains on.
//! What: [`sample`] re-classifies a window of commits from a read-only
//! database copy with the traced rule engine, stratifies the verdicts, and
//! draws a capped, seeded sample for human raters; [`subsample`] draws a
//! proportional, seeded subset of that sample. [`score`] joins the raters'
//! labels to that sample and computes per-rule, per-method and per-stratum
//! precision with Wilson intervals, a stratum-weighted accuracy, a
//! coverage-at-precision curve, a confusion matrix, the abstention share and
//! Cohen's kappa. [`stats`] holds the estimators.
//! Test: `eval::stats::tests`, `eval::draw::tests`, `tests/eval_harness.rs`.
//!
//! Every output file carries commit text. Nothing here picks a default output
//! location: callers pass the directory explicitly.

pub mod draw;
pub mod records;
pub mod sample;
pub mod score;
pub mod stats;
// #111: a proportional, seeded subset of a sample for a second rater.
pub mod subsample;

// #111: resolve each sample row's merge flag, from the row or a tga DB.
mod merges;
mod population;
// #111: strip identity trailers and e-mails from the rater sheet.
mod redact;
mod report_md;

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use thiserror::Error;

pub use records::{SampleRecord, StrataSummary, Stratum};
pub use sample::{run_sample, SampleParams, SampleSummary};
pub use score::{run_score, ScoreParams, ScoreReport};
pub use subsample::{run_subsample, SubsampleParams, SubsampleSummary};

/// Errors raised by the eval harness.
#[derive(Debug, Error)]
pub enum EvalError {
    /// The database could not be opened or queried.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),
    /// The shared read-only opener or config layer refused.
    #[error(transparent)]
    Core(#[from] crate::core::errors::TgaError),
    /// Building the rule engine failed.
    #[error("rule engine: {0}")]
    Classify(#[from] crate::classify::ClassifyError),
    /// The database handle accepted writes, so the harness refuses to run.
    #[error("refusing to run: {0} is not opened read-only")]
    NotReadOnly(PathBuf),
    /// A file could not be read or written.
    #[error("{path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A JSON document could not be parsed or written.
    #[error("{path}: {source}")]
    Json {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        source: serde_json::Error,
    },
    /// A CSV file could not be parsed or written.
    #[error("{path}: {source}")]
    Csv {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        source: csv::Error,
    },
    /// Inputs are inconsistent (bad label, empty sample, bad parameter).
    #[error("{0}")]
    Invalid(String),
}

/// Result alias for the eval harness.
pub type Result<T> = std::result::Result<T, EvalError>;

pub(crate) fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> EvalError + '_ {
    move |source| EvalError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Category names a config knows: its taxonomy plus every rule's category.
///
/// Used by `tga eval score --config` as the valid-label vocabulary; the
/// sample's own predicted categories and the no-answer labels are always
/// added on top. #111: a rules file's categories count even when the
/// taxonomy does not declare them, so a scheme such as v2 is accepted from
/// the rules file alone and no category list is hardcoded here.
///
/// # Errors
///
/// A rules file that fails to load or compile.
pub fn config_categories(config: &crate::core::config::Config) -> Result<Vec<String>> {
    let pipeline = crate::classify::ClassificationPipeline::new(config.clone());
    let engine = pipeline.build_rule_engine()?;
    let taxonomy = engine.taxonomy().all().iter().map(|d| d.name.clone());
    Ok(taxonomy.chain(pipeline.rule_categories()?).collect())
}

/// Open a tga database for the harness, refusing any handle that can write.
///
/// Why: the harness reads an operator's database; it must never migrate,
/// vacuum or otherwise alter it, even by accident.
/// What: opens through [`crate::core::inspect::open_read_only`], verifies with
/// `sqlite3_db_readonly` that the main schema is read-only, and sets
/// `PRAGMA query_only` as a second guard.
/// Test: `tests/eval_harness.rs::eval_handle_rejects_writes`.
///
/// # Errors
///
/// [`EvalError::NotReadOnly`] when SQLite reports the handle writable, or the
/// opener's own error for a missing or non-SQLite file.
pub fn open_eval_db(path: &Path) -> Result<Connection> {
    let conn = crate::core::inspect::open_read_only(path)?;
    if !conn.is_readonly(rusqlite::MAIN_DB)? {
        return Err(EvalError::NotReadOnly(path.to_path_buf()));
    }
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

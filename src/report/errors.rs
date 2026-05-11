//! Error types for the `report` module.

use thiserror::Error;

/// Errors produced by report aggregation and formatting.
#[derive(Debug, Error)]
pub enum ReportError {
    /// Underlying core error (DB, config, model).
    #[error("core error: {0}")]
    Core(#[from] crate::core::TgaError),

    /// I/O failure reading or writing report files.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// CSV writer failure.
    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    /// JSON (de)serialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// Tera template engine failure.
    #[error("template error: {0}")]
    Template(#[from] tera::Error),

    /// Domain-level reporting failure (bad input, unsupported format).
    #[error("report error: {0}")]
    Report(String),
}

/// Module-wide `Result` alias.
pub type Result<T> = std::result::Result<T, ReportError>;

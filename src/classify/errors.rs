//! Error types for the `classify` module.

use thiserror::Error;

/// Top-level error type for classification operations.
#[derive(Debug, Error)]
pub enum ClassifyError {
    /// Wraps a core error (DB, config, etc.).
    #[error("core error: {0}")]
    Core(#[from] crate::core::TgaError),

    /// Failed to load or parse a rules file.
    #[error("rule load error: {0}")]
    RuleLoad(String),

    /// A regex pattern in the ruleset failed to compile.
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),

    /// HTTP request to an LLM provider failed.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// JSON serialization/deserialization failed.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// YAML deserialization failed (rule files).
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),

    /// Filesystem I/O failed.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Module-wide `Result` alias.
pub type Result<T> = std::result::Result<T, ClassifyError>;

//! Error types for the `collect` module.
//!
//! `CollectError` aggregates failures from git operations, HTTP requests,
//! the core database layer, and identity resolution.

use thiserror::Error;

/// Top-level error type for collection-stage operations.
#[derive(Debug, Error)]
pub enum CollectError {
    /// A `git2`/libgit2 error occurred during repository operations.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// An HTTP transport or response error occurred.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// A core error bubbled up from the `core` module (DB, config, validation).
    #[error("core error: {0}")]
    Core(#[from] crate::core::TgaError),

    /// A direct `rusqlite` error from inline SQL in this module.
    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    /// Identity resolution failed for the given context.
    #[error("identity resolution failed: {0}")]
    Identity(String),

    /// An underlying `std::io` error (file not found, permission denied, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization failure.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A configuration value required for this operation was missing.
    #[error("configuration error: {0}")]
    Config(String),
}

/// Module-wide `Result` alias.
pub type Result<T> = std::result::Result<T, CollectError>;

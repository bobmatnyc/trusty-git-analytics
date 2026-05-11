//! Error types for the `tga-core` crate.
//!
//! All library code in this crate returns [`Result<T>`], which is a type alias
//! for `std::result::Result<T, TgaError>`. Errors implement `std::error::Error`
//! via [`thiserror::Error`] so they integrate cleanly with both `anyhow`
//! (in binary crates) and direct error matching.

use thiserror::Error;

/// Top-level error type for all `tga-core` operations.
///
/// Variants intentionally cover the surface area of I/O, serialization,
/// database, migration, validation, and lookup failures. Add new variants
/// rather than overloading [`TgaError::ValidationError`] for unrelated
/// failure modes.
#[derive(Debug, Error)]
pub enum TgaError {
    /// A `rusqlite`/SQLite error occurred.
    #[error("database error: {0}")]
    DbError(#[from] rusqlite::Error),

    /// Configuration is structurally valid but semantically wrong
    /// (e.g. missing required field, contradictory values).
    #[error("configuration error: {0}")]
    ConfigError(String),

    /// An underlying `std::io` error (file not found, permission denied, etc.).
    #[error("I/O error: {0}")]
    IoError(#[from] std::io::Error),

    /// YAML deserialization failure.
    #[error("YAML deserialization error: {0}")]
    SerdeYamlError(#[from] serde_yaml::Error),

    /// JSON serialization/deserialization failure.
    #[error("JSON serialization error: {0}")]
    SerdeJsonError(#[from] serde_json::Error),

    /// A validation rule on otherwise well-formed data failed.
    #[error("validation error: {0}")]
    ValidationError(String),

    /// A requested entity was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// A database migration failed to apply or could not be reconciled.
    #[error("migration error: {0}")]
    MigrationError(String),
}

/// Crate-wide `Result` alias.
///
/// Prefer `tga_core::Result<T>` over `std::result::Result<T, TgaError>`
/// for brevity in signatures.
pub type Result<T> = std::result::Result<T, TgaError>;

//! Versioned SQL migrations.
//!
//! Migrations are stored as a static list of `(version, name, sql)` tuples
//! and applied in order. Each migration is wrapped in a transaction along
//! with the corresponding row insert into `schema_migrations`, so partial
//! application is impossible.
//!
//! Adding a new migration:
//! 1. Append a new entry to [`MIGRATIONS`] with a strictly increasing version.
//! 2. Never edit an existing migration in place — write a follow-up migration.

use rusqlite::Connection;
use tracing::{debug, info};

use crate::core::errors::{Result, TgaError};

/// A single migration step.
pub struct Migration {
    /// Strictly increasing version number; must be unique.
    pub version: i64,
    /// Human-readable label, recorded for audit/debugging.
    pub name: &'static str,
    /// The SQL to execute. May contain multiple statements separated by `;`.
    pub sql: &'static str,
}

/// All migrations known to this binary, in order of application.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: include_str!("sql/0001_initial_schema.sql"),
    },
    Migration {
        version: 2,
        name: "linear_issues",
        sql: include_str!("sql/0002_linear_issues.sql"),
    },
    Migration {
        version: 3,
        name: "commits_ticketed",
        sql: include_str!("sql/0003_commits_ticketed.sql"),
    },
    Migration {
        version: 4,
        name: "collection_runs",
        sql: include_str!("sql/0004_collection_runs.sql"),
    },
    Migration {
        version: 5,
        name: "work_items",
        sql: include_str!("sql/0005_work_items.sql"),
    },
    Migration {
        version: 6,
        name: "classification_overrides",
        sql: include_str!("sql/0006_classification_overrides.sql"),
    },
    Migration {
        version: 7,
        name: "pr_metrics_and_backfill",
        sql: include_str!("sql/0007_pr_metrics_and_backfill.sql"),
    },
    Migration {
        version: 8,
        name: "azdo_iterations",
        sql: include_str!("sql/0008_azdo_iterations.sql"),
    },
    Migration {
        version: 9,
        name: "collection_runs_repo_count",
        sql: include_str!("sql/0009_collection_runs_repo_count.sql"),
    },
];

/// Ensure the `schema_migrations` bookkeeping table exists.
fn ensure_migrations_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations ( \
            version    INTEGER PRIMARY KEY, \
            name       TEXT NOT NULL, \
            applied_at TEXT NOT NULL \
        );",
    )?;
    Ok(())
}

/// Return the highest applied migration version, or 0 if none have been applied.
fn current_version(conn: &Connection) -> Result<i64> {
    let v: Option<i64> = conn
        .query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    Ok(v.unwrap_or(0))
}

/// Apply all migrations whose version is greater than the current schema version.
///
/// Idempotent: running it twice in a row is a no-op the second time.
///
/// # Errors
///
/// Returns [`TgaError::MigrationError`] if a migration's SQL fails. The
/// transaction guarantees partial application cannot occur.
pub fn run(conn: &mut Connection) -> Result<()> {
    ensure_migrations_table(conn)?;
    let current = current_version(conn)?;
    debug!(current_version = current, "running migrations");

    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        info!(version = m.version, name = m.name, "applying migration");
        let tx = conn.transaction().map_err(TgaError::from)?;
        tx.execute_batch(m.sql).map_err(|e| {
            TgaError::MigrationError(format!("migration {} ({}) failed: {e}", m.version, m.name))
        })?;
        tx.execute(
            "INSERT INTO schema_migrations(version, name, applied_at) VALUES (?1, ?2, ?3)",
            rusqlite::params![m.version, m.name, chrono::Utc::now().to_rfc3339()],
        )
        .map_err(TgaError::from)?;
        tx.commit().map_err(TgaError::from)?;
    }
    Ok(())
}

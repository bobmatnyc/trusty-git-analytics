//! Bookkeeping helpers for the `collection_runs` table.
//!
//! The collector uses these to determine which `(repo, ISO-week)` pairs have
//! already been collected so that re-runs over the same range are silent
//! no-ops unless `--force` is supplied.

use rusqlite::params;
use tracing::debug;

use crate::core::db::Database;
use crate::core::errors::{Result, TgaError};

/// Returns true if `(repo_name, iso_year, iso_week)` already has a row in
/// `collection_runs`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL query fails.
pub fn is_week_collected(
    db: &Database,
    repo_name: &str,
    iso_year: i32,
    iso_week: u32,
) -> Result<bool> {
    let n: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM collection_runs \
             WHERE repo_name = ?1 AND iso_year = ?2 AND iso_week = ?3",
            params![repo_name, iso_year, iso_week as i64],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    Ok(n > 0)
}

/// Record (or overwrite) a completed collection run for the given week.
///
/// Uses `INSERT OR REPLACE` against the `UNIQUE (repo_name, iso_year, iso_week)`
/// constraint, so recording the same week twice is idempotent and refreshes
/// `collected_at` and `commit_count`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the insert fails.
pub fn record_collection_run(
    db: &Database,
    repo_name: &str,
    iso_year: i32,
    iso_week: u32,
    commit_count: usize,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    db.connection()
        .execute(
            "INSERT OR REPLACE INTO collection_runs \
             (repo_name, iso_year, iso_week, collected_at, commit_count) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                repo_name,
                iso_year,
                iso_week as i64,
                now,
                commit_count as i64,
            ],
        )
        .map_err(TgaError::from)?;
    debug!(
        repo = repo_name,
        year = iso_year,
        week = iso_week,
        commits = commit_count,
        "recorded collection run"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_week_collected_false_initially() {
        let db = Database::open_in_memory().expect("open in-memory db");
        let got = is_week_collected(&db, "demo-repo", 2026, 11).expect("query ok");
        assert!(!got);
    }

    #[test]
    fn record_and_check_collection_run() {
        let db = Database::open_in_memory().expect("open in-memory db");
        record_collection_run(&db, "demo-repo", 2026, 11, 42).expect("record ok");
        assert!(is_week_collected(&db, "demo-repo", 2026, 11).expect("query ok"));
        // Different week is still uncollected.
        assert!(!is_week_collected(&db, "demo-repo", 2026, 12).expect("query ok"));
        // Different repo is still uncollected.
        assert!(!is_week_collected(&db, "other-repo", 2026, 11).expect("query ok"));
    }

    #[test]
    fn record_is_idempotent() {
        let db = Database::open_in_memory().expect("open in-memory db");
        record_collection_run(&db, "demo-repo", 2026, 11, 42).expect("first record ok");
        // Second insert of the same (repo, year, week) must not error thanks
        // to INSERT OR REPLACE on the UNIQUE constraint.
        record_collection_run(&db, "demo-repo", 2026, 11, 50).expect("second record ok");
        // Exactly one row should exist for this tuple.
        let n: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM collection_runs \
                 WHERE repo_name = ?1 AND iso_year = ?2 AND iso_week = ?3",
                params!["demo-repo", 2026, 11i64],
                |row| row.get(0),
            )
            .expect("count ok");
        assert_eq!(n, 1);
        // commit_count should reflect the most recent write.
        let cc: i64 = db
            .connection()
            .query_row(
                "SELECT commit_count FROM collection_runs \
                 WHERE repo_name = ?1 AND iso_year = ?2 AND iso_week = ?3",
                params!["demo-repo", 2026, 11i64],
                |row| row.get(0),
            )
            .expect("read commit_count");
        assert_eq!(cc, 50);
    }
}

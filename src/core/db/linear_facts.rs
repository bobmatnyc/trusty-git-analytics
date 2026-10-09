//! Incremental-sync cursor bookkeeping for `tga linear sync` /
//! `tga linear freshness` (issue #7139).
//!
//! Why: `linear_issues` (migration v2) is written by two independent paths —
//! the per-commit-reference lookup (`linear.fetch_on_reference`) and, since
//! this issue, a bulk team sync — so `MAX(linear_issues.fetched_at)` cannot
//! tell "the bulk sync ran" apart from "a commit happened to mention a
//! ticket". `linear_sync_cursor` is the bulk sync's own bookkeeping table,
//! mirroring `jira_sync_cursor` (`core::db::jira_facts`, migration v23): one
//! row per team key, holding the `updatedAt >=` cursor for the next
//! incremental run and the wall-clock time of the last successful run.
//! What: [`LinearSyncCursor`], [`get_linear_cursor`], [`set_linear_cursor`],
//! and [`list_linear_cursor_teams`] — the same shape as their JIRA
//! counterparts, scoped by `team_key` instead of `project_key`.
//! Test: this module's own `tests`; `commands::linear::tests` covers the
//! command-level freshness reporting built on top of these primitives.

use rusqlite::{params, Connection, OptionalExtension};

use crate::core::errors::{Result, TgaError};

/// Stored incremental-sync cursor for a Linear team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearSyncCursor {
    /// RFC3339 timestamp: the `updatedAt >=` cursor for the next incremental
    /// run.
    pub last_synced_at: String,
    /// RFC3339 timestamp of the last successful sync invocation's wall-clock
    /// completion — the freshness signal `tga linear freshness` reads.
    pub last_run_at: String,
    /// Number of issues processed in the last successful run.
    pub issues_synced: i64,
}

/// Fetch the stored sync cursor for `team_key`, or `None` if this team has
/// never completed a bulk sync.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn get_linear_cursor(conn: &Connection, team_key: &str) -> Result<Option<LinearSyncCursor>> {
    conn.query_row(
        "SELECT last_synced_at, last_run_at, issues_synced \
         FROM linear_sync_cursor WHERE team_key = ?1",
        params![team_key],
        |row| {
            Ok(LinearSyncCursor {
                last_synced_at: row.get(0)?,
                last_run_at: row.get(1)?,
                issues_synced: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(TgaError::from)
}

/// Record (overwrite) the sync cursor for `team_key` after a successful run.
///
/// `last_run_at` is stamped with the current wall-clock time; callers supply
/// only the cursor (`last_synced_at`) and issue count. A new row records
/// field-set version 0; an existing row keeps its version (#190 step 3), which
/// only [`set_linear_fields_version`] changes.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn set_linear_cursor(
    conn: &Connection,
    team_key: &str,
    last_synced_at: &str,
    issues_synced: i64,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    // #190 step 3: an upsert, not INSERT OR REPLACE, which would reset
    // `fields_version` on every run.
    conn.execute(
        "INSERT INTO linear_sync_cursor \
         (team_key, last_synced_at, last_run_at, issues_synced) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(team_key) DO UPDATE SET last_synced_at = excluded.last_synced_at, \
         last_run_at = excluded.last_run_at, issues_synced = excluded.issues_synced",
        params![team_key, last_synced_at, now, issues_synced],
    )
    .map_err(TgaError::from)?;
    Ok(())
}

/// The issue field-set version `team_key` was last read in full under, or
/// `None` when the team has no cursor row.
///
/// Why: #190 step 3 — an incremental sync never re-reads an unchanged issue,
/// so a field added to the query needs one full read per team to reach every
/// stored row. This is the record of whether that read happened.
/// What: reads `linear_sync_cursor.fields_version` (migration v35; 0 for a
/// cursor written before it).
/// Test: `fields_version_survives_a_cursor_update`.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn get_linear_fields_version(conn: &Connection, team_key: &str) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT fields_version FROM linear_sync_cursor WHERE team_key = ?1",
        params![team_key],
        |row| row.get(0),
    )
    .optional()
    .map_err(TgaError::from)
}

/// Record that `team_key`'s whole history was read under field-set `version`.
///
/// Updates the team's existing cursor row and returns how many rows changed
/// (0 when the team has no cursor row yet, which leaves the next run a full
/// read anyway).
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the update fails.
pub fn set_linear_fields_version(conn: &Connection, team_key: &str, version: i64) -> Result<usize> {
    conn.execute(
        "UPDATE linear_sync_cursor SET fields_version = ?2 WHERE team_key = ?1",
        params![team_key, version],
    )
    .map_err(TgaError::from)
}

/// Every Linear team key that has ever recorded a sync cursor.
///
/// Mirrors [`crate::core::db::jira_facts::list_cursor_projects`]: the
/// freshness guard defaults to checking every team with recorded state,
/// rather than an aggregate that lets one healthy team mask another team's
/// dead sync.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the query fails.
pub fn list_linear_cursor_teams(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT team_key FROM linear_sync_cursor ORDER BY team_key")
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(TgaError::from)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    /// #190 step 3: re-setting the cursor keeps the recorded field-set
    /// version; `INSERT OR REPLACE` reset it to 0 on every run.
    #[test]
    fn fields_version_survives_a_cursor_update() {
        let db = Database::open_in_memory().expect("open");
        let conn = db.connection();
        assert_eq!(get_linear_fields_version(conn, "ENG").expect("read"), None);
        set_linear_cursor(conn, "ENG", "2026-01-01T00:00:00+00:00", 1).expect("set");
        assert_eq!(
            get_linear_fields_version(conn, "ENG").expect("read"),
            Some(0)
        );
        assert_eq!(set_linear_fields_version(conn, "ENG", 1).expect("mark"), 1);
        set_linear_cursor(conn, "ENG", "2026-01-02T00:00:00+00:00", 2).expect("set again");
        assert_eq!(
            get_linear_fields_version(conn, "ENG").expect("read"),
            Some(1)
        );
        assert_eq!(
            set_linear_fields_version(conn, "OPS", 1).expect("no row"),
            0
        );
    }

    #[test]
    fn get_cursor_is_none_before_any_sync() {
        let db = Database::open_in_memory().expect("open");
        assert_eq!(
            get_linear_cursor(db.connection(), "ENG").expect("query"),
            None
        );
    }

    #[test]
    fn set_then_get_cursor_roundtrips() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 42).expect("set");
        let cursor = get_linear_cursor(db.connection(), "ENG")
            .expect("query")
            .expect("present");
        assert_eq!(cursor.last_synced_at, "2026-01-01T00:00:00+00:00");
        assert_eq!(cursor.issues_synced, 42);
        assert!(!cursor.last_run_at.is_empty());
    }

    #[test]
    fn set_cursor_overwrites_the_prior_row() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 10).expect("first");
        set_linear_cursor(db.connection(), "ENG", "2026-02-01T00:00:00+00:00", 20).expect("second");
        let cursor = get_linear_cursor(db.connection(), "ENG")
            .expect("query")
            .expect("present");
        assert_eq!(cursor.last_synced_at, "2026-02-01T00:00:00+00:00");
        assert_eq!(cursor.issues_synced, 20);
    }

    #[test]
    fn list_cursor_teams_returns_every_recorded_team_sorted() {
        let db = Database::open_in_memory().expect("open");
        set_linear_cursor(db.connection(), "FE", "2026-01-01T00:00:00+00:00", 1).expect("fe");
        set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 1).expect("eng");
        let teams = list_linear_cursor_teams(db.connection()).expect("list");
        assert_eq!(teams, vec!["ENG".to_string(), "FE".to_string()]);
    }
}

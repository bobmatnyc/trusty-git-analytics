//! Persistence helpers for the `work_items` and `commit_work_items` tables.
//!
//! Work items are PM-system tickets (ADO, JIRA, GitHub, Linear) referenced by
//! commits. The `(id, source)` composite primary key scopes IDs by source so
//! the same numeric/string ID can coexist across PM systems without collision.
//!
//! The companion `commit_work_items` join table records which commits mention
//! which work items, enabling fast lookups in both directions (commit → items
//! and item → commits).

use rusqlite::{params, Connection, OptionalExtension};
use tracing::debug;

use crate::core::errors::{Result, TgaError};

/// Row in the `work_items` table.
///
/// Mirrors the columns in `sql/0005_work_items.sql`. `(id, source)` is the
/// composite primary key — IDs are not globally unique, they are unique per
/// PM source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItemRow {
    /// The PM-system-native identifier (e.g. `"42"` for ADO, `"ENG-7"` for
    /// Linear). Stored as text to accommodate non-numeric IDs.
    pub id: String,
    /// PM source identifier: `"azdo"`, `"jira"`, `"github"`, or `"linear"`.
    pub source: String,
    /// Work item title (`System.Title` for ADO, summary for JIRA, etc.).
    pub title: String,
    /// Workflow state (`"Active"`, `"Closed"`, `"Done"`, ...).
    pub status: String,
    /// Item type (`"Bug"`, `"User Story"`, `"Task"`, `"Epic"`, ...).
    pub item_type: String,
    /// Comma-separated tags. `None` if the source provided no tags.
    pub tags: Option<String>,
    /// Source-specific project / team identifier.
    pub project: Option<String>,
    /// Canonical URL to the work item, if known.
    pub url: Option<String>,
    /// Full source-system JSON payload for forensic / extension use. `None`
    /// when callers don't have a raw payload to persist.
    pub raw_json: Option<String>,
}

/// Insert or replace a work item.
///
/// Uses `INSERT OR REPLACE` against the `(id, source)` primary key, so calling
/// this twice for the same work item refreshes all columns (and the
/// `fetched_at` default). Idempotent.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn upsert_work_item(conn: &Connection, item: &WorkItemRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO work_items \
         (id, source, title, status, item_type, tags, project, url, raw_json, fetched_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, datetime('now'))",
        params![
            item.id,
            item.source,
            item.title,
            item.status,
            item.item_type,
            item.tags,
            item.project,
            item.url,
            item.raw_json,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(id = %item.id, source = %item.source, "upserted work item");
    Ok(())
}

/// Link a commit to a work item in `commit_work_items`.
///
/// Uses `INSERT OR IGNORE` so duplicate links are silently dropped — the
/// `(commit_sha, work_item_id, work_item_source)` primary key enforces
/// uniqueness. Idempotent.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails. A
/// foreign-key violation surfaces here if the referenced work item has not
/// been upserted first.
pub fn link_commit_work_item(
    conn: &Connection,
    commit_sha: &str,
    work_item_id: &str,
    source: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO commit_work_items \
         (commit_sha, work_item_id, work_item_source) \
         VALUES (?1, ?2, ?3)",
        params![commit_sha, work_item_id, source],
    )
    .map_err(TgaError::from)?;
    Ok(())
}

/// Fetch every work item linked to the given commit SHA.
///
/// Returns rows in stable order by `(work_item_source, work_item_id)`. An
/// empty `Vec` is returned if the commit has no links.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the SQL query or row decoding fails.
pub fn get_work_items_for_commit(conn: &Connection, commit_sha: &str) -> Result<Vec<WorkItemRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT w.id, w.source, w.title, w.status, w.item_type, \
                    w.tags, w.project, w.url, w.raw_json \
             FROM work_items w \
             JOIN commit_work_items cwi \
               ON cwi.work_item_id = w.id AND cwi.work_item_source = w.source \
             WHERE cwi.commit_sha = ?1 \
             ORDER BY w.source, w.id",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map(params![commit_sha], row_to_work_item)
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// List all work items for a given source (e.g. `"azdo"`).
///
/// Returns rows ordered by `id` ascending. An empty `Vec` is returned if no
/// rows match.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the SQL query or row decoding fails.
pub fn list_work_items(conn: &Connection, source: &str) -> Result<Vec<WorkItemRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, source, title, status, item_type, tags, project, url, raw_json \
             FROM work_items \
             WHERE source = ?1 \
             ORDER BY id",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map(params![source], row_to_work_item)
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// Fetch a single work item by `(id, source)`, or `None` if absent.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the SQL query or row decoding fails.
pub fn get_work_item(conn: &Connection, id: &str, source: &str) -> Result<Option<WorkItemRow>> {
    conn.query_row(
        "SELECT id, source, title, status, item_type, tags, project, url, raw_json \
         FROM work_items WHERE id = ?1 AND source = ?2",
        params![id, source],
        row_to_work_item,
    )
    .optional()
    .map_err(TgaError::from)
}

/// Decode a `work_items` row in the canonical column order.
fn row_to_work_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkItemRow> {
    Ok(WorkItemRow {
        id: row.get(0)?,
        source: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        item_type: row.get(4)?,
        tags: row.get(5)?,
        project: row.get(6)?,
        url: row.get(7)?,
        raw_json: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    fn sample(id: &str) -> WorkItemRow {
        WorkItemRow {
            id: id.to_string(),
            source: "azdo".into(),
            title: format!("Item {id}"),
            status: "Active".into(),
            item_type: "Bug".into(),
            tags: Some("frontend,urgent".into()),
            project: Some("MyProject".into()),
            url: Some(format!("https://x/{id}")),
            raw_json: Some(r#"{"id":1}"#.into()),
        }
    }

    #[test]
    fn upsert_then_get_roundtrips() {
        let db = Database::open_in_memory().expect("open in-memory");
        let item = sample("42");
        upsert_work_item(db.connection(), &item).expect("upsert");
        let got = get_work_item(db.connection(), "42", "azdo")
            .expect("get")
            .expect("present");
        assert_eq!(got, item);
    }

    #[test]
    fn upsert_is_idempotent_and_refreshes() {
        let db = Database::open_in_memory().expect("open in-memory");
        let mut item = sample("42");
        upsert_work_item(db.connection(), &item).expect("first");
        item.title = "Updated title".into();
        upsert_work_item(db.connection(), &item).expect("second");
        let got = get_work_item(db.connection(), "42", "azdo")
            .expect("get")
            .expect("present");
        assert_eq!(got.title, "Updated title");
    }

    #[test]
    fn link_and_lookup_by_commit() {
        let db = Database::open_in_memory().expect("open in-memory");
        upsert_work_item(db.connection(), &sample("7")).expect("upsert 7");
        upsert_work_item(db.connection(), &sample("9")).expect("upsert 9");
        let sha = "deadbeefcafe";
        link_commit_work_item(db.connection(), sha, "7", "azdo").expect("link 7");
        link_commit_work_item(db.connection(), sha, "9", "azdo").expect("link 9");
        // Duplicate link is silently ignored.
        link_commit_work_item(db.connection(), sha, "7", "azdo").expect("link 7 again");

        let items = get_work_items_for_commit(db.connection(), sha).expect("lookup");
        assert_eq!(items.len(), 2);
        let ids: Vec<&str> = items.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(ids, vec!["7", "9"]);
    }

    #[test]
    fn list_work_items_filters_by_source() {
        let db = Database::open_in_memory().expect("open in-memory");
        upsert_work_item(db.connection(), &sample("1")).expect("upsert");
        let mut jira = sample("2");
        jira.source = "jira".into();
        upsert_work_item(db.connection(), &jira).expect("upsert jira");

        let azdo = list_work_items(db.connection(), "azdo").expect("list");
        assert_eq!(azdo.len(), 1);
        assert_eq!(azdo[0].id, "1");
        let jira_rows = list_work_items(db.connection(), "jira").expect("list jira");
        assert_eq!(jira_rows.len(), 1);
        assert_eq!(jira_rows[0].id, "2");
    }

    #[test]
    fn get_work_item_returns_none_when_absent() {
        let db = Database::open_in_memory().expect("open in-memory");
        let got = get_work_item(db.connection(), "nope", "azdo").expect("query");
        assert!(got.is_none());
    }

    #[test]
    fn link_without_work_item_fails_fk() {
        let db = Database::open_in_memory().expect("open in-memory");
        // FK enforcement is on; linking before upserting must fail.
        let err = link_commit_work_item(db.connection(), "abc", "999", "azdo")
            .expect_err("FK violation expected");
        assert!(matches!(err, TgaError::DbError(_)));
    }
}

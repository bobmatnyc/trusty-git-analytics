//! Write one Linear issue's `work_items` row, keyed by Linear's issue id
//! (#190 step 3).
//!
//! Why: `work_items.id` is the identifier a commit names (ENG-123), and Linear
//! changes it when an issue moves team. Keyed by identifier alone, a moved
//! issue became a second row, and its commit links and fact rows stayed on
//! the stale one. Migration v35 adds `work_items.stable_id`, the Linear issue
//! id, so the writer can find the issue's row whatever it is called now.
//! What: [`write_work_item`] upserts the row under the issue's current
//! identifier and folds into it every row the issue held under an earlier
//! one — the row its `stable_id` already keys, and any unkeyed row under one
//! of its `previousIdentifiers` (written before the key existed). Folding
//! moves the stale row's `commit_work_items`, `fact_pm_work` and
//! `fact_pm_effort` rows, then deletes it. [`identifier_aliases`] maps every
//! identifier a commit may name to the issue's current one.
//! Test: `collect::linear_pipeline::tests` (`moved_issue_keeps_one_work_items_row_and_its_links`
//! and the tests beside it).

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};

use super::issue::LinearIssue;
use crate::core::db::work_items::upsert_work_item;
use crate::core::db::WorkItemRow;
use crate::core::errors::{Result, TgaError};

/// Every table whose `(work_item_id, work_item_source)` references
/// `work_items(id, source)` (migrations v5, v26, v27).
const REFERENCING_TABLES: [&str; 3] = ["commit_work_items", "fact_pm_work", "fact_pm_effort"];

/// Upsert `row` for `issue`, keyed by the issue's Linear id.
///
/// Why: see the module docs.
/// What: with no Linear id (a payload written before #190) this is a plain
/// upsert. Otherwise it refuses an identifier whose row carries a different
/// Linear id, upserts `row`, folds the issue's stale rows into it, and stores
/// the Linear id in `stable_id`. Run it inside the caller's transaction: a
/// failure part-way leaves the folds half-done until the caller rolls back.
/// Test: `moved_issue_keeps_one_work_items_row_and_its_links`,
/// `legacy_work_item_under_a_previous_identifier_is_merged`,
/// `identifier_held_by_another_linear_id_errors`, `failed_merge_rolls_back`.
///
/// # Errors
///
/// [`TgaError::ValidationError`] when `row.id` is held by another Linear id;
/// [`TgaError::DbError`] on any failed read or write.
pub(crate) fn write_work_item(
    conn: &Connection,
    row: &WorkItemRow,
    issue: &LinearIssue,
) -> Result<()> {
    let Some(lid) = issue.linear_id.as_deref() else {
        return upsert_work_item(conn, row);
    };
    if let Some(Some(other)) = stable_id_of(conn, &row.id, &row.source)? {
        if other != lid {
            return Err(TgaError::ValidationError(format!(
                "Linear identifier {} is held by issue {other} in work_items but Linear now \
                 returns it for issue {lid}; no row was changed",
                row.id
            )));
        }
    }
    let stale = stale_rows(conn, row, lid, &issue.previous_identifiers)?;
    upsert_work_item(conn, row)?;
    for old in &stale {
        fold_into(conn, &row.source, old, &row.id)?;
    }
    conn.execute(
        "UPDATE work_items SET stable_id = ?1 WHERE id = ?2 AND source = ?3",
        params![lid, row.id, row.source],
    )?;
    Ok(())
}

/// Map every identifier a commit may name — current or previous — to the
/// issue's current identifier.
///
/// Linear resolves an old identifier to the moved issue, so a commit naming
/// `ENG-5` links to the issue now called `OPS-12`. A current identifier wins
/// over another issue's previous one.
/// Test: `commit_naming_a_previous_identifier_links_the_moved_issue`.
pub(crate) fn identifier_aliases(issues: &[LinearIssue]) -> HashMap<&str, &str> {
    let mut out = HashMap::new();
    for issue in issues {
        for previous in &issue.previous_identifiers {
            out.insert(previous.as_str(), issue.identifier.as_str());
        }
    }
    for issue in issues {
        out.insert(issue.identifier.as_str(), issue.identifier.as_str());
    }
    out
}

/// `stable_id` of the row `(id, source)`: `None` when no such row exists,
/// `Some(None)` for a row with no key.
fn stable_id_of(conn: &Connection, id: &str, source: &str) -> Result<Option<Option<String>>> {
    Ok(conn
        .query_row(
            "SELECT stable_id FROM work_items WHERE id = ?1 AND source = ?2",
            params![id, source],
            |r| r.get(0),
        )
        .optional()?)
}

/// The identifiers of the rows to fold into `row`: the row `lid` already
/// keys under another identifier, and each unkeyed row under a previous
/// identifier. A row keyed by a different Linear id belongs to that issue
/// and is never folded.
fn stale_rows(
    conn: &Connection,
    row: &WorkItemRow,
    lid: &str,
    previous: &[String],
) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let keyed: Option<String> = conn
        .query_row(
            "SELECT id FROM work_items WHERE source = ?1 AND stable_id = ?2",
            params![row.source, lid],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = keyed.filter(|id| *id != row.id) {
        out.push(id);
    }
    for id in previous {
        if *id == row.id || out.contains(id) {
            continue;
        }
        if let Some(None) = stable_id_of(conn, id, &row.source)? {
            out.push(id.clone());
        }
    }
    Ok(out)
}

/// Move every reference from `(old, source)` to `(new, source)`, then
/// delete the `old` row.
///
/// A reference `new` already has wins: `UPDATE OR IGNORE` leaves the
/// colliding `old` reference in place, and the `DELETE` after it removes it.
/// `REFERENCING_TABLES` holds literals only, so formatting a name into the
/// SQL carries no injection risk.
fn fold_into(conn: &Connection, source: &str, old: &str, new: &str) -> Result<()> {
    for table in REFERENCING_TABLES {
        conn.execute(
            &format!(
                "UPDATE OR IGNORE {table} SET work_item_id = ?1 \
                 WHERE work_item_id = ?2 AND work_item_source = ?3"
            ),
            params![new, old, source],
        )?;
        conn.execute(
            &format!("DELETE FROM {table} WHERE work_item_id = ?1 AND work_item_source = ?2"),
            params![old, source],
        )?;
    }
    conn.execute(
        "DELETE FROM work_items WHERE id = ?1 AND source = ?2",
        params![old, source],
    )?;
    tracing::info!(from = %old, to = %new, source, "folded a moved issue's work_items row");
    Ok(())
}

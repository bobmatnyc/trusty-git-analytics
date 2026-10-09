//! Persist [`LinearIssue`] rows into `linear_issues`, keyed by Linear's
//! stable issue id (#190).
//!
//! Why: the writer used `INSERT OR REPLACE` keyed on `identifier`. Linear
//! changes the identifier when an issue moves team, so a moved issue became a
//! second row, and the per-commit lookup's REPLACE would drop every column it
//! did not write. Migration v33 adds `linear_id` (UUID) with a UNIQUE index.
//! What: [`plan_linear_issues`] classifies each issue against the stored rows
//! without writing (the dry-run report); [`upsert_linear_issues`] applies the
//! same plan in one transaction; [`store_linear_issues`] is the count-only
//! wrapper the per-commit path uses. The lookup order is: the row with this
//! `linear_id`; else the row with this `identifier` whose `linear_id` is NULL
//! (a pre-v33 row, filled in place); else (#190 step 3) a NULL-`linear_id`
//! row under one of the issue's `previousIdentifiers` — an issue that moved
//! team before its first post-v33 sync — adopted in place. Any other
//! NULL-`linear_id` row under a previous identifier is a stale copy of the
//! same issue and is deleted. An unchanged issue — same `linear_id`,
//! identifier and stored node — is not written.
//! Test: `tests` below.

use rusqlite::{params, Connection, OptionalExtension};

use super::activity::due::queue_comment_reads;
use super::issue::LinearIssue;
use crate::core::db::Database;
use crate::core::errors::{Result, TgaError};

/// What a sync did, or would do, to one issue's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum IssueChange {
    /// No row held this issue; one is inserted.
    New,
    /// The row exists and its content changed.
    Changed,
    /// The row exists under another identifier: the issue moved team. The
    /// row is updated in place, never duplicated.
    Moved,
    /// The row already holds this `updatedAt`; nothing is written.
    Unchanged,
}

/// Per-change totals for a batch, for the sync's report line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ChangeCounts {
    /// [`IssueChange::New`] count.
    pub new: usize,
    /// [`IssueChange::Changed`] count.
    pub changed: usize,
    /// [`IssueChange::Moved`] count.
    pub moved: usize,
    /// [`IssueChange::Unchanged`] count.
    pub unchanged: usize,
}

impl ChangeCounts {
    /// Tally `changes`.
    #[must_use]
    pub fn of(changes: &[IssueChange]) -> Self {
        let mut c = Self::default();
        for change in changes {
            match change {
                IssueChange::New => c.new += 1,
                IssueChange::Changed => c.changed += 1,
                IssueChange::Moved => c.moved += 1,
                IssueChange::Unchanged => c.unchanged += 1,
            }
        }
        c
    }

    /// Rows inserted or updated.
    #[must_use]
    pub fn written(&self) -> usize {
        self.new + self.changed + self.moved
    }
}

/// The row action [`plan_one`] resolved for one issue.
struct Action {
    change: IssueChange,
    /// The row to update; `None` inserts.
    row_id: Option<i64>,
    /// Pre-v33 rows (NULL `linear_id`) that are stale copies of this issue:
    /// the one holding the identifier a moved issue now carries, and (#190
    /// step 3) any under one of its `previousIdentifiers`. Linear never gives
    /// one identifier to two issues, so each is deleted — the first so the
    /// UNIQUE `identifier` index admits the move.
    evict: Vec<i64>,
}

/// Resolve the action for `issue` against `conn`, reading only.
fn plan_one(conn: &Connection, issue: &LinearIssue) -> Result<Action> {
    let mut action = plan_row(conn, issue)?;
    // #190 step 3: an issue moved before its first post-v33 sync left a
    // pre-v33 row under the identifier it moved from.
    for previous in &issue.previous_identifiers {
        if let Some(id) = legacy_row_under(conn, previous)? {
            if action.row_id != Some(id) && !action.evict.contains(&id) {
                action.evict.push(id);
            }
        }
    }
    Ok(action)
}

/// The row `issue` writes to, and how it changes; [`plan_one`] adds the
/// stale copies under previous identifiers.
fn plan_row(conn: &Connection, issue: &LinearIssue) -> Result<Action> {
    if let Some(lid) = &issue.linear_id {
        let found: Option<(i64, String, Option<String>)> = conn
            .query_row(
                "SELECT id, identifier, raw_json FROM linear_issues WHERE linear_id = ?1",
                params![lid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        if let Some((row_id, identifier, stored_raw)) = found {
            if identifier != issue.identifier {
                let evict = identifier_holder(conn, &issue.identifier, row_id, lid)?;
                return Ok(Action {
                    change: IssueChange::Moved,
                    row_id: Some(row_id),
                    evict: evict.into_iter().collect(),
                });
            }
            // #190 step 3: compare the stored node, not `updatedAt` alone, so
            // a field added to the query rewrites an issue that did not change.
            let fresh = issue.raw.as_ref().map(serde_json::Value::to_string);
            let same = fresh.is_some() && stored_raw == fresh;
            return Ok(Action {
                change: if same {
                    IssueChange::Unchanged
                } else {
                    IssueChange::Changed
                },
                row_id: Some(row_id),
                evict: Vec::new(),
            });
        }
    }
    let by_identifier: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, linear_id FROM linear_issues WHERE identifier = ?1",
            params![issue.identifier],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match (by_identifier, &issue.linear_id) {
        (None, Some(_)) => adopt_previous(conn, issue),
        (None, None) => Ok(Action {
            change: IssueChange::New,
            row_id: None,
            evict: Vec::new(),
        }),
        // A pre-v33 row, or an id-less payload: update the row in place.
        (Some((row_id, None)), _) | (Some((row_id, Some(_))), None) => Ok(Action {
            change: IssueChange::Changed,
            row_id: Some(row_id),
            evict: Vec::new(),
        }),
        (Some((_, Some(other))), Some(lid)) => {
            Err(identifier_conflict(&issue.identifier, &other, lid))
        }
    }
}

/// An issue no row holds yet: adopt the first pre-v33 row under one of its
/// `previousIdentifiers` (#190 step 3), else insert.
fn adopt_previous(conn: &Connection, issue: &LinearIssue) -> Result<Action> {
    for previous in &issue.previous_identifiers {
        if let Some(row_id) = legacy_row_under(conn, previous)? {
            return Ok(Action {
                change: IssueChange::Moved,
                row_id: Some(row_id),
                evict: Vec::new(),
            });
        }
    }
    Ok(Action {
        change: IssueChange::New,
        row_id: None,
        evict: Vec::new(),
    })
}

/// The row holding `identifier` when its `linear_id` is NULL (a pre-v33
/// row). A row with a `linear_id` belongs to the issue that id names and is
/// never returned.
fn legacy_row_under(conn: &Connection, identifier: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM linear_issues WHERE identifier = ?1 AND linear_id IS NULL",
            params![identifier],
            |r| r.get(0),
        )
        .optional()?)
}

/// The row other than `row_id` that holds `identifier`, when it is a pre-v33
/// row safe to evict.
///
/// # Errors
///
/// [`TgaError::ValidationError`] when that row carries a different
/// `linear_id`: two issues claim one identifier, and neither row is dropped.
fn identifier_holder(
    conn: &Connection,
    identifier: &str,
    row_id: i64,
    lid: &str,
) -> Result<Option<i64>> {
    let holder: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT id, linear_id FROM linear_issues WHERE identifier = ?1 AND id != ?2",
            params![identifier, row_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match holder {
        None => Ok(None),
        Some((id, None)) => Ok(Some(id)),
        Some((_, Some(other))) => Err(identifier_conflict(identifier, &other, lid)),
    }
}

fn identifier_conflict(identifier: &str, stored: &str, incoming: &str) -> TgaError {
    TgaError::ValidationError(format!(
        "Linear identifier {identifier} is held by issue {stored} in linear_issues but Linear \
         now returns it for issue {incoming}; no row was changed"
    ))
}

/// Classify every issue against the stored rows without writing.
///
/// Why: `tga linear sync --dry-run` opens the database read-only (#189) and
/// reports what a real run would change.
/// What: runs the same lookup [`upsert_linear_issues`] runs, one issue at a
/// time against the rows as they are now.
/// Test: `plan_reports_without_writing`.
///
/// # Errors
///
/// [`TgaError::DbError`] on a failed read; [`TgaError::ValidationError`] on
/// an identifier held by a different issue id.
pub fn plan_linear_issues(conn: &Connection, issues: &[LinearIssue]) -> Result<Vec<IssueChange>> {
    issues
        .iter()
        .map(|i| plan_one(conn, i).map(|a| a.change))
        .collect()
}

/// Upsert `issues` into `linear_issues` in one transaction, keyed by
/// `linear_id`.
///
/// Why: see the module docs — a moved issue must update one row, and a
/// re-sync with no remote change must write nothing.
/// What: resolves each issue with [`plan_one`] inside the transaction, so a
/// batch sees its own earlier writes; deletes the stale pre-v33 copies the
/// plan names, then inserts, updates in place, or skips. Returns one
/// [`IssueChange`] per input, in order. A failure rolls the whole batch back.
/// Test: `moved_issue_updates_one_row`, `legacy_row_is_filled_in_place`,
/// `unchanged_issue_is_not_rewritten`, `identifier_held_by_another_id_errors`,
/// `orphan_under_a_previous_identifier_is_adopted`,
/// `stale_copy_under_a_previous_identifier_is_evicted`,
/// `widened_field_set_rewrites_an_issue_with_the_same_updated_at`.
///
/// # Errors
///
/// [`TgaError::DbError`] on SQL failure; [`TgaError::ValidationError`] when an
/// identifier is held by a different issue id.
pub fn upsert_linear_issues(db: &Database, issues: &[LinearIssue]) -> Result<Vec<IssueChange>> {
    // #7139: `unchecked_transaction` because this takes `&Database`; `tga` is
    // single-process and nothing else holds the connection, which is the
    // precondition `unchecked_transaction` documents.
    let tx = db.connection().unchecked_transaction()?;
    let out = upsert_linear_issues_in(&tx, issues)?;
    tx.commit()?;
    Ok(out)
}

/// [`upsert_linear_issues`] without its own transaction: the caller owns it.
///
/// Why: #190 — `tga linear sync` skips the `work_items` projection for an
/// [`IssueChange::Unchanged`] issue, so the issue rows and their projection
/// must commit together. Two commits let a crash between them leave issues
/// that every later sync reports unchanged and never projects.
/// What: the same per-issue plan, evict and write as [`upsert_linear_issues`],
/// run on `conn`, which should be an open transaction; nothing commits here.
/// Then [`queue_comment_reads`] records, on the same connection, the moved
/// and older-new issues whose comments their team's next incremental walk
/// cannot see (#190).
/// Test: `commands::linear::sync_tests::failed_work_items_write_is_repaired_by_the_next_sync`,
/// `store_linear_issues_queues_comment_reads_for_moved_and_old_new_issues`.
///
/// # Errors
///
/// As [`upsert_linear_issues`]. The caller's transaction is left open; drop
/// it to roll back.
pub fn upsert_linear_issues_in(
    conn: &Connection,
    issues: &[LinearIssue],
) -> Result<Vec<IssueChange>> {
    let fetched_at = chrono::Utc::now().to_rfc3339();
    let mut out = Vec::with_capacity(issues.len());
    for issue in issues {
        let action = plan_one(conn, issue)?;
        for evict in &action.evict {
            conn.execute("DELETE FROM linear_issues WHERE id = ?1", params![evict])?;
        }
        if action.change != IssueChange::Unchanged {
            write_row(conn, issue, &fetched_at, action.row_id)?;
        }
        out.push(action.change);
    }
    // #190: queue the comment re-read here, so every writer of
    // `linear_issues` (`tga collect` and `tga linear sync`) records a moved or
    // older-new issue in the same transaction as its row.
    queue_comment_reads(conn, issues, &out)?;
    Ok(out)
}

/// Persist Linear issues and return how many the table now holds current.
///
/// The per-commit-reference path's entry point; a thin wrapper over
/// [`upsert_linear_issues`], so it shares the `linear_id` key (#190) instead
/// of the old `INSERT OR REPLACE` on `identifier`, which dropped every column
/// it did not write.
///
/// # Errors
///
/// As [`upsert_linear_issues`].
pub fn store_linear_issues(db: &Database, issues: &[LinearIssue]) -> Result<usize> {
    Ok(upsert_linear_issues(db, issues)?.len())
}

/// Insert (`row_id = None`) or update one row with every column.
///
/// A new row is inserted with the NOT NULL columns only, then takes the same
/// full UPDATE an existing row takes, so one column list serves both.
fn write_row(
    conn: &Connection,
    issue: &LinearIssue,
    fetched_at: &str,
    row_id: Option<i64>,
) -> Result<()> {
    // The API's team key; the identifier prefix only for an id-less payload.
    let team_key = issue
        .team_key
        .clone()
        .unwrap_or_else(|| issue.identifier.split('-').next().unwrap_or("").to_string());
    let row_id = match row_id {
        Some(id) => id,
        None => {
            conn.execute(
                "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    issue.identifier,
                    issue.title,
                    issue.state,
                    issue.team,
                    team_key,
                    fetched_at
                ],
            )?;
            conn.last_insert_rowid()
        }
    };
    let ts = |d: &Option<chrono::DateTime<chrono::Utc>>| d.map(|d| d.to_rfc3339());
    let json_list = |v: &Vec<String>| serde_json::to_string(v).ok();
    let values = params![
        issue.identifier,
        issue.title,
        issue.state,
        issue.team,
        team_key,
        issue.assignee,
        issue.priority as i64,
        issue.url,
        fetched_at,
        ts(&issue.created_at),
        ts(&issue.updated_at),
        ts(&issue.started_at),
        ts(&issue.completed_at),
        ts(&issue.canceled_at),
        issue.linear_id,
        issue.state_type,
        issue.team_id,
        issue.estimate,
        issue.project_id,
        issue.cycle_id,
        issue.parent_id,
        json_list(&issue.label_ids),
        json_list(&issue.label_names),
        issue.due_date,
        issue.assignee_id,
        issue.assignee_name,
        issue.assignee_email,
        issue.creator_id,
        i64::from(issue.is_archived()),
        ts(&issue.archived_at),
        issue.raw.as_ref().map(|r| r.to_string()),
        row_id,
    ];
    // `linear_id` is COALESCEd so an id-less payload never clears it.
    conn.execute(
        "UPDATE linear_issues SET identifier = ?1, title = ?2, state = ?3, team = ?4, \
         team_key = ?5, assignee = ?6, priority = ?7, url = ?8, fetched_at = ?9, \
         created_at = ?10, updated_at = ?11, started_at = ?12, completed_at = ?13, \
         canceled_at = ?14, linear_id = COALESCE(?15, linear_id), state_type = ?16, \
         team_id = ?17, estimate = ?18, project_id = ?19, cycle_id = ?20, parent_id = ?21, \
         label_ids = ?22, label_names = ?23, due_date = ?24, assignee_id = ?25, \
         assignee_name = ?26, assignee_email = ?27, creator_id = ?28, archived = ?29, \
         archived_at = ?30, raw_json = ?31 WHERE id = ?32",
        values,
    )?;
    Ok(())
}

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

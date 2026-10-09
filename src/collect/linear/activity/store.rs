//! Persist Linear issue history and comments into the v36 tables (#190
//! step 6).
//!
//! Why: an issue's history and comments are re-read only when the issue
//! changes, and a failed read must leave that issue due for the next run,
//! whatever the issue cursor did meanwhile.
//! What: [`activity_candidates`] lists a team's issues whose stored
//! `updated_at` differs from the one their activity was last read at
//! (`linear_issue_activity_state`); [`commit_issue_activity`] replaces one
//! issue's rows for one [`ActivityKind`] and records that `updated_at`, in
//! one transaction; [`mark_issue_missing`] tombstones an issue Linear no
//! longer has, so it is not requested again until its stored row changes or
//! `--backfill` asks. A history entry that changed the workflow state is one
//! `fact_linear_transitions` row; a comment is one
//! `fact_linear_comment_detail` row holding its body length, never its body.
//! Test: `activity::tests`; `commands::linear::activity_sync_tests`.

use std::collections::BTreeMap;

use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use super::{ActivityKind, ActivityTarget};
use crate::collect::linear::LinearIssue;
use crate::core::db::Database;
use crate::core::errors::{Result, TgaError};

/// The marker column for `kind` in `linear_issue_activity_state`.
fn marker_column(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::History => "history_for",
        ActivityKind::Comments => "comments_for",
    }
}

/// The wall-clock column written beside [`marker_column`].
fn synced_column(kind: ActivityKind) -> &'static str {
    match kind {
        ActivityKind::History => "history_synced_at",
        ActivityKind::Comments => "comments_synced_at",
    }
}

/// The issues of `team_key` whose `kind` activity is out of date.
///
/// Why: #190 step 6 — the issue cursor moves forward on every successful
/// issue pass, so an issue whose history read failed would never come back
/// in an incremental walk. The out-of-date test reads the stored rows, not
/// the walk.
/// What: every `linear_issues` row of `team_key` with a `linear_id` whose
/// `updated_at` differs from the `kind` marker in
/// `linear_issue_activity_state` (no marker counts as different), ordered by
/// identifier. A row tombstoned at its current `updated_at` (see
/// [`mark_issue_missing`]) is left out. `force` returns every such row
/// regardless of the marker or a tombstone.
/// `overlay` holds issues fetched but not stored (the dry run): each replaces
/// the stored row with its `linear_id`, or adds one. Pre-v33 rows with no
/// `linear_id` are skipped until a sync fills it.
/// Test: `activity::tests::candidates_are_the_rows_whose_marker_lags`,
/// `activity::tests::overlay_counts_fetched_but_unstored_issues`,
/// `activity::tests::a_tombstone_holds_until_the_row_changes`.
///
/// # Errors
///
/// [`crate::core::errors::TgaError::DbError`] on a failed read.
pub fn activity_candidates(
    conn: &Connection,
    team_key: &str,
    kind: ActivityKind,
    force: bool,
    overlay: &[LinearIssue],
) -> Result<Vec<ActivityTarget>> {
    let marker = marker_column(kind);
    // `IS NOT` treats a missing marker (NULL) as different. #190 step 6
    // (D28): a tombstone set at the row's current `updated_at` skips it.
    let sql = format!(
        "SELECT li.linear_id, li.identifier, li.team_key, li.updated_at \
         FROM linear_issues li \
         LEFT JOIN linear_issue_activity_state s ON s.issue_id = li.linear_id \
         WHERE li.team_key = ?1 AND li.linear_id IS NOT NULL \
           AND (?2 OR (s.{marker} IS NOT li.updated_at \
                AND (s.missing_at IS NULL OR s.missing_for IS NOT li.updated_at)))"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![team_key, force], |r| {
        Ok(ActivityTarget::new(
            &r.get::<_, String>(0)?,
            &r.get::<_, String>(1)?,
            &r.get::<_, String>(2)?,
            r.get(3)?,
        ))
    })?;
    // Keyed by identifier so the result is in identifier order.
    let mut due: BTreeMap<String, ActivityTarget> = BTreeMap::new();
    for row in rows {
        let target = row?;
        due.insert(target.identifier.clone(), target);
    }
    // #190 step 6: a dry run stored nothing; its fetched issues stand in for
    // the rows a real run would have written.
    for issue in overlay {
        let Some(issue_id) = issue.linear_id.as_deref() else {
            continue;
        };
        let team = issue
            .team_key
            .clone()
            .unwrap_or_else(|| team_key.to_string());
        if team != team_key {
            continue;
        }
        due.retain(|_, t| t.issue_id != issue_id);
        let updated_at = issue.updated_at.map(|d| d.to_rfc3339());
        let state: Option<(Option<String>, Option<String>, Option<String>)> = conn
            .query_row(
                &format!(
                    "SELECT {marker}, missing_for, missing_at \
                     FROM linear_issue_activity_state WHERE issue_id = ?1"
                ),
                params![issue_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let (current, missing_for, missing_at) = state.unwrap_or_default();
        let tombstoned = missing_at.is_some() && missing_for == updated_at;
        if force || (current != updated_at && !tombstoned) {
            let target = ActivityTarget::new(issue_id, &issue.identifier, &team, updated_at);
            due.insert(target.identifier.clone(), target);
        }
    }
    Ok(due.into_values().collect())
}

/// Replace one issue's `kind` rows with `nodes` and mark it current.
///
/// Why: #190 step 6 — the rows and the marker must commit together, or a
/// failed write would mark an issue current with no rows behind it.
/// What: in one transaction, deletes the issue's rows from `kind`'s table,
/// inserts one row per node (history: only entries with a `toState`), and
/// sets the issue's `kind` marker to `target.updated_at` and clears any
/// tombstone (Linear returned the issue). Returns the rows inserted. `nodes` must be the complete list: the walk fails rather than
/// return a cut one.
/// Test: `activity::tests::history_rows_hold_state_transitions_only`,
/// `activity::tests::comment_rows_hold_metadata_not_body`,
/// `activity::tests::rewrite_replaces_rows_in_place`.
///
/// # Errors
///
/// [`crate::core::errors::TgaError::DbError`] on SQL failure;
/// [`crate::core::errors::TgaError::ValidationError`] on a node without an
/// `id` or `createdAt`.
pub fn commit_issue_activity(
    db: &mut Database,
    target: &ActivityTarget,
    kind: ActivityKind,
    nodes: &[Json],
) -> Result<usize> {
    let synced_at = Utc::now().timestamp();
    let tx = db.connection_mut().transaction()?;
    tx.execute(
        &format!("DELETE FROM {} WHERE issue_id = ?1", kind.table()),
        params![target.issue_id],
    )?;
    let mut written = 0;
    for node in nodes {
        let inserted = match kind {
            ActivityKind::History => insert_transition(&tx, target, node, synced_at)?,
            ActivityKind::Comments => insert_comment(&tx, target, node, synced_at)?,
        };
        written += usize::from(inserted);
    }
    let (marker, synced) = (marker_column(kind), synced_column(kind));
    // #190 step 6: same transaction as the rows, so a failed write leaves the
    // issue due. Linear returned the issue, so a tombstone no longer holds.
    tx.execute(
        &format!(
            "INSERT INTO linear_issue_activity_state (issue_id, {marker}, {synced}) \
             VALUES (?1, ?2, ?3) ON CONFLICT(issue_id) DO UPDATE SET \
             {marker} = excluded.{marker}, {synced} = excluded.{synced}, \
             missing_for = NULL, missing_at = NULL"
        ),
        params![target.issue_id, target.updated_at, Utc::now().to_rfc3339()],
    )?;
    tx.commit()?;
    Ok(written)
}

/// Tombstone an issue Linear says it no longer has.
///
/// Why: #190 step 6 (owner ruling D28: not found = counted warning) — the
/// `linear_issues` row is never removed, so without a record the issue would
/// be requested, and reported missing, on every run.
/// What: sets the issue's `missing_for` to `target.updated_at` and
/// `missing_at` to now. Neither activity marker and no stored history or
/// comment row changes. [`activity_candidates`] skips the issue while its
/// stored `updated_at` equals `missing_for`; a successful
/// [`commit_issue_activity`] clears both columns.
/// Test: `activity::tests::a_tombstone_holds_until_the_row_changes`.
///
/// # Errors
///
/// [`crate::core::errors::TgaError::DbError`] on a failed write.
pub fn mark_issue_missing(conn: &Connection, target: &ActivityTarget) -> Result<()> {
    conn.execute(
        "INSERT INTO linear_issue_activity_state (issue_id, missing_for, missing_at) \
         VALUES (?1, ?2, ?3) ON CONFLICT(issue_id) DO UPDATE SET \
         missing_for = excluded.missing_for, missing_at = excluded.missing_at",
        params![target.issue_id, target.updated_at, Utc::now().to_rfc3339()],
    )?;
    Ok(())
}

/// A required string field of an activity node.
fn required<'a>(node: &'a Json, field: &str, kind: ActivityKind) -> Result<&'a str> {
    node[field].as_str().ok_or_else(|| {
        TgaError::ValidationError(format!(
            "Linear {} node has no `{field}`",
            kind.connection()
        ))
    })
}

/// Insert one history entry if it changed the workflow state; `false` when
/// it did not.
fn insert_transition(
    conn: &Connection,
    target: &ActivityTarget,
    node: &Json,
    synced_at: i64,
) -> Result<bool> {
    let id = required(node, "id", ActivityKind::History)?;
    let at = required(node, "createdAt", ActivityKind::History)?;
    let Some(to_state) = node["toState"]["name"].as_str() else {
        return Ok(false);
    };
    conn.execute(
        "INSERT OR REPLACE INTO fact_linear_transitions (history_id, issue_id, identifier, \
         team_key, from_state, from_state_type, to_state, to_state_type, actor_id, actor_name, \
         transitioned_at, synced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            id,
            target.issue_id,
            target.identifier,
            target.team_key,
            node["fromState"]["name"].as_str(),
            node["fromState"]["type"].as_str(),
            to_state,
            node["toState"]["type"].as_str(),
            node["actor"]["id"].as_str(),
            node["actor"]["name"].as_str(),
            at,
            synced_at,
        ],
    )?;
    Ok(true)
}

/// Insert one comment's metadata; the body contributes only its length.
fn insert_comment(
    conn: &Connection,
    target: &ActivityTarget,
    node: &Json,
    synced_at: i64,
) -> Result<bool> {
    let id = required(node, "id", ActivityKind::Comments)?;
    let at = required(node, "createdAt", ActivityKind::Comments)?;
    let body_len = node["body"].as_str().map_or(0, |b| b.chars().count());
    conn.execute(
        "INSERT OR REPLACE INTO fact_linear_comment_detail (comment_id, issue_id, identifier, \
         team_key, parent_id, author_id, author_name, created_at, updated_at, body_len, \
         synced_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            id,
            target.issue_id,
            target.identifier,
            target.team_key,
            node["parent"]["id"].as_str(),
            node["user"]["id"].as_str(),
            node["user"]["name"].as_str(),
            at,
            node["updatedAt"].as_str(),
            i64::try_from(body_len).unwrap_or(i64::MAX),
            synced_at,
        ],
    )?;
    Ok(true)
}

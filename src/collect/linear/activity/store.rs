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
//! one transaction. A history entry that changed the workflow state is one
//! `fact_linear_transitions` row; a comment is one
//! `fact_linear_comment_detail` row holding its body length, never its body.
//! Test: `activity::tests`; `commands::linear::activity_sync_tests`.

use rusqlite::Connection;
use serde_json::Value as Json;

use super::{ActivityKind, ActivityTarget};
use crate::collect::linear::LinearIssue;
use crate::core::db::Database;
use crate::core::errors::Result;

/// The issues of `team_key` whose `kind` activity is out of date.
///
/// Why: #190 step 6 — the issue cursor moves forward on every successful
/// issue pass, so an issue whose history read failed would never come back
/// in an incremental walk. The out-of-date test reads the stored rows, not
/// the walk.
/// What: every `linear_issues` row of `team_key` with a `linear_id` whose
/// `updated_at` differs from the `kind` marker in
/// `linear_issue_activity_state` (no marker counts as different), ordered by
/// identifier. `force` returns every such row regardless of the marker.
/// `overlay` holds issues fetched but not stored (the dry run): each replaces
/// the stored row with its `linear_id`, or adds one. Pre-v33 rows with no
/// `linear_id` are skipped until a sync fills it.
/// Test: `activity::tests::candidates_are_the_rows_whose_marker_lags`,
/// `activity::tests::overlay_counts_fetched_but_unstored_issues`.
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
    // Red skeleton (#190 step 6).
    let _ = (conn, team_key, kind, force, overlay);
    Ok(Vec::new())
}

/// Replace one issue's `kind` rows with `nodes` and mark it current.
///
/// Why: #190 step 6 — the rows and the marker must commit together, or a
/// failed write would mark an issue current with no rows behind it.
/// What: in one transaction, deletes the issue's rows from `kind`'s table,
/// inserts one row per node (history: only entries with a `toState`), and
/// sets the issue's `kind` marker to `target.updated_at`. Returns the rows
/// inserted. `nodes` must be the complete list: the walk fails rather than
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
    // Red skeleton (#190 step 6).
    let _ = (db, target, kind, nodes);
    Ok(0)
}

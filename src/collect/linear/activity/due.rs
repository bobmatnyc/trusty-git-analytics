//! Issues whose whole comment set the next incremental comments walk reads
//! (#190, migration v38).
//!
//! Why: the incremental walk ([`super::comments`]) filters on the issue's
//! current team and the comment's own `updatedAt`. Moving an issue does not
//! change its comments' `updatedAt`, so a comment written while the issue sat
//! in another team, older than the team's cursor minus the overlap, would
//! match no walk.
//! What: every `linear_issues` write (`upsert_linear_issues_in`) calls
//! [`queue_comment_reads`] in its own transaction;
//! the comments pass lists the queue with [`due_comment_issues`] and
//! [`super::store::commit_team_comments_with_due`] empties it.
//! Test: `commands::linear::comment_cursor_tests::a_moved_issues_old_comments_are_read_in_full`,
//! `commands::linear::comment_cursor_tests::a_move_seen_by_an_issue_only_run_is_read_by_the_next_comments_run`.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};

use super::comments::COMMENT_OVERLAP;
use super::store::comment_cursor;
use crate::collect::linear::{IssueChange, LinearIssue};
use crate::core::errors::Result;

/// Record the issues of one `linear_issues` write whose comments their
/// team's next incremental comments walk cannot see; returns how many were
/// recorded.
///
/// Why: #190 — the walk's `updatedAt` bound skips a moved issue's older
/// comments, and the move is visible only to the issue write. Recording it in
/// the write's transaction keeps it visible to a later `--comments` run whose
/// issue pass sees the issue unchanged. `upsert_linear_issues_in` calls this,
/// so `tga collect` and `tga linear sync` both queue.
/// What: for each issue, reads the comment cursor of the issue's OWN team
/// (its `team_key`, else its identifier prefix), never the team a caller is
/// syncing: one batch from `tga collect` spans teams. A team with no cursor
/// queues nothing (its next walk reads every comment of the team). Otherwise,
/// with `since` = cursor minus [`COMMENT_OVERLAP`], upserts a
/// `linear_comment_due` row for each issue classified [`IssueChange::Moved`],
/// and each [`IssueChange::New`] issue whose `createdAt` is at or before
/// `since` or unknown. An issue created after `since` is left out: every
/// comment on it is newer than the bound. `issues` and `changes` are
/// parallel. An issue with no `linear_id` is skipped (the walk filters by
/// id). The upsert keys on the issue id, so a re-run adds no second row.
/// Test: `commands::linear::comment_cursor_tests::a_moved_issues_old_comments_are_read_in_full`,
/// `collect::linear::store::tests::store_linear_issues_queues_comment_reads_for_moved_and_old_new_issues`.
///
/// # Errors
///
/// [`crate::core::errors::TgaError::DbError`] on SQL failure;
/// [`crate::core::errors::TgaError::ValidationError`] on a stored cursor that
/// is not RFC 3339.
pub fn queue_comment_reads(
    conn: &Connection,
    issues: &[LinearIssue],
    changes: &[IssueChange],
) -> Result<usize> {
    debug_assert_eq!(
        issues.len(),
        changes.len(),
        "issues and changes are parallel"
    );
    let queued_at = Utc::now().to_rfc3339();
    // #190: one cursor read per team, keyed on the issue's own team.
    let mut cursors: HashMap<String, Option<DateTime<Utc>>> = HashMap::new();
    let mut queued = 0;
    for (issue, change) in issues.iter().zip(changes) {
        if !matches!(change, IssueChange::Moved | IssueChange::New) {
            continue;
        }
        let Some(issue_id) = issue.linear_id.as_deref() else {
            continue;
        };
        let team = issue
            .team_key
            .clone()
            .unwrap_or_else(|| issue.identifier.split('-').next().unwrap_or("").to_string());
        let cursor = match cursors.get(&team) {
            Some(c) => *c,
            None => {
                let c = comment_cursor(conn, &team)?;
                cursors.insert(team.clone(), c);
                c
            }
        };
        let Some(cursor) = cursor else {
            continue;
        };
        let since = cursor - COMMENT_OVERLAP;
        let reason = match change {
            IssueChange::Moved => "moved",
            IssueChange::New if issue.created_at.is_none_or(|c| c <= since) => "new",
            _ => continue,
        };
        conn.execute(
            "INSERT INTO linear_comment_due (issue_id, team_key, reason, queued_at) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT(issue_id) DO UPDATE SET \
             team_key = excluded.team_key, reason = excluded.reason, \
             queued_at = excluded.queued_at",
            params![issue_id, team, reason, queued_at],
        )?;
        queued += 1;
    }
    Ok(queued)
}

/// The issue ids queued for `team_key`'s next comments walk, in id order.
///
/// # Errors
///
/// [`crate::core::errors::TgaError::DbError`] on a failed read.
pub fn due_comment_issues(conn: &Connection, team_key: &str) -> Result<Vec<String>> {
    let mut stmt = conn
        .prepare("SELECT issue_id FROM linear_comment_due WHERE team_key = ?1 ORDER BY issue_id")?;
    let ids = stmt
        .query_map(params![team_key], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

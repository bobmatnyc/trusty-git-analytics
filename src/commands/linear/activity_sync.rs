//! The issue history and comments pass of `tga linear sync --history
//! --comments` (#190 step 6).
//!
//! Why: JIRA parity — state transitions and comment metadata per issue. The
//! pass runs after a team's issue pass, behind its own flags, so the issue
//! path is unchanged without them.
//! What: [`sync_activity`] and the [`ActivityOutcome`] line it reports.
//! Test: `super::activity_sync_tests`.

use tga::collect::linear::activity::store::{activity_candidates, commit_issue_activity};
use tga::collect::linear::{ActivityKind, LinearClient, LinearIssue};
use tga::core::db::Database;

/// What one team's history or comments pass found and did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActivityOutcome {
    /// Team key.
    pub team_key: String,
    /// The connection read.
    pub kind: ActivityKind,
    /// Issues whose activity was out of date (or, with `--backfill`, every
    /// issue of the team).
    pub due: usize,
    /// Issues read and written; 0 on a dry run, which reads nothing.
    pub read: usize,
    /// Rows written across those issues.
    pub rows: usize,
}

impl ActivityOutcome {
    /// The one report line for this pass.
    pub(crate) fn summary_line(&self, dry_run: bool) -> String {
        let what = match self.kind {
            ActivityKind::History => "state transition",
            _ => "comment",
        };
        if dry_run {
            format!(
                "Linear {} ({}): would read {} issue(s) [dry-run: no requests, no writes].",
                self.kind.connection(),
                self.team_key,
                self.due
            )
        } else {
            format!(
                "Linear {} ({}): read {} of {} due issue(s); wrote {} {what} row(s).",
                self.kind.connection(),
                self.team_key,
                self.read,
                self.due,
                self.rows
            )
        }
    }
}

/// Read and store one team's issue history or comments.
///
/// Why: #190 step 6 — an issue's activity is read again only when the issue
/// changed, and a failed read must leave the issue due (Fail-Open check: an
/// API error is never a warning while the state moves on).
/// What: lists the due issues with [`activity_candidates`] (`force` =
/// `--backfill`: every issue of the team; `overlay` = the issues a dry run
/// fetched but did not store). A dry run stops there and reports the count.
/// Otherwise each issue's connection is walked in full and written with its
/// marker in one transaction, in identifier order. The first issue that fails
/// stops the pass with an error naming it; issues written before it keep
/// their rows and markers, it and the rest stay due.
/// Test: `super::activity_sync_tests::history_and_comments_land_for_every_issue`,
/// `super::activity_sync_tests::a_failed_history_read_fails_the_sync_and_stays_due`,
/// `super::activity_sync_tests::unchanged_issues_are_not_read_again`,
/// `super::activity_sync_tests::dry_run_counts_due_issues_and_sends_no_activity_request`.
///
/// # Errors
///
/// Linear HTTP/auth and paging errors, naming the issue; database errors.
pub(super) async fn sync_activity(
    client: &LinearClient,
    db: &mut Database,
    team_key: &str,
    kind: ActivityKind,
    force: bool,
    dry_run: bool,
    overlay: &[LinearIssue],
) -> anyhow::Result<ActivityOutcome> {
    // Red skeleton (#190 step 6).
    let _ = (
        client,
        db,
        force,
        dry_run,
        overlay,
        activity_candidates,
        commit_issue_activity,
    );
    Ok(ActivityOutcome {
        team_key: team_key.to_string(),
        kind,
        due: 0,
        read: 0,
        rows: 0,
    })
}

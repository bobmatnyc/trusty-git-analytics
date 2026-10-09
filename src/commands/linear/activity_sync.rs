//! The issue history and comments pass of `tga linear sync --history
//! --comments` (#190 step 6).
//!
//! Why: JIRA parity — state transitions and comment metadata per issue. The
//! pass runs after a team's issue pass, behind its own flags, so the issue
//! path is unchanged without them.
//! What: [`sync_activity`] and the [`ActivityOutcome`] line it reports.
//! Test: `super::activity_sync_tests`.

use tga::collect::linear::activity::store::{
    activity_candidates, commit_issue_activity, mark_issue_missing,
};
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
    /// #190 step 6 (D28): identifiers of the issues Linear answered "not
    /// found" for, tombstoned and not read.
    pub missing: Vec<String>,
}

/// Identifiers a summary line lists before it says how many more there are.
const MISSING_LISTED: usize = 10;

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
            let mut line = format!(
                "Linear {} ({}): read {} of {} due issue(s); wrote {} {what} row(s).",
                self.kind.connection(),
                self.team_key,
                self.read,
                self.due,
                self.rows
            );
            // #190 step 6 (D28): a missing issue is reported, never silent.
            if !self.missing.is_empty() {
                let mut listed = self.missing[..self.missing.len().min(MISSING_LISTED)].join(", ");
                if self.missing.len() > MISSING_LISTED {
                    listed.push_str(&format!(
                        " and {} more",
                        self.missing.len() - MISSING_LISTED
                    ));
                }
                line.push_str(&format!(
                    " Warning: {} issue(s) no longer in Linear, tombstoned and skipped until \
                     they change or --backfill: {listed}.",
                    self.missing.len()
                ));
            }
            line
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
/// marker in one transaction, in identifier order. An issue Linear answers
/// "not found" for is tombstoned with [`mark_issue_missing`], logged at warn
/// and counted in `missing`; the pass goes on (owner ruling D28). Any other
/// failure stops the pass with an error naming the issue; issues written
/// before it keep their rows and markers, it and the rest stay due.
/// Test: `super::activity_sync_tests::history_and_comments_land_for_every_issue`,
/// `super::activity_sync_tests::a_failed_history_read_fails_the_sync_and_stays_due`,
/// `super::activity_sync_tests::a_missing_issue_is_tombstoned_and_the_run_goes_on`,
/// `super::activity_sync_tests::a_tombstoned_issue_is_skipped_until_backfill`,
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
    let targets = activity_candidates(db.connection(), team_key, kind, force, overlay)?;
    let mut outcome = ActivityOutcome {
        team_key: team_key.to_string(),
        kind,
        due: targets.len(),
        read: 0,
        rows: 0,
        missing: Vec::new(),
    };
    tracing::info!(
        team = %team_key,
        connection = kind.connection(),
        due = targets.len(),
        force,
        dry_run,
        "starting Linear issue activity pass"
    );
    // #190 step 6: a dry run reports the count; it sends no per-issue request.
    if dry_run {
        return Ok(outcome);
    }
    for target in &targets {
        // #190 step 6 (Fail-Open check): an API error stops the pass. The
        // issue's marker stays where it was, so the issue stays due.
        let nodes = client
            .fetch_issue_activity(target, kind)
            .await
            .map_err(|e| {
                anyhow::Error::new(e).context(format!(
                    "reading Linear {} of {} failed after {} of {} due issue(s) were written",
                    kind.connection(),
                    target.identifier,
                    outcome.read,
                    outcome.due
                ))
            })?;
        // #190 step 6 (D28): Linear's explicit "not found" for this issue is
        // a counted warning. The tombstone keeps it from being requested on
        // every run; its stored rows and markers are left as they are.
        let Some(nodes) = nodes else {
            tracing::warn!(
                team = %team_key,
                issue = %target.identifier,
                connection = kind.connection(),
                "Linear no longer has this issue; tombstoned"
            );
            mark_issue_missing(db.connection(), target)?;
            outcome.missing.push(target.identifier.clone());
            continue;
        };
        outcome.rows += commit_issue_activity(db, target, kind, &nodes)?;
        outcome.read += 1;
    }
    Ok(outcome)
}

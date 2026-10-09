//! The incremental comments pass of `tga linear sync --comments` (#190).
//!
//! Why: live Linear data shows `Issue.updatedAt` does not reliably move when
//! a comment is created or edited, so the v36 pass, keyed on the issue
//! marker, missed comments. This pass is keyed on the comment's own
//! `updatedAt`. `--backfill` keeps the per-issue walk
//! (`super::activity_sync`), which also removes deleted comments.
//! What: [`sync_comments`] and the [`CommentsOutcome`] line it reports.
//! Test: `super::comment_cursor_tests`.

use chrono::{DateTime, SecondsFormat, Utc};

use tga::collect::linear::activity::comments::COMMENT_OVERLAP;
use tga::collect::linear::activity::store::{comment_cursor, commit_team_comments};
use tga::collect::linear::LinearClient;
use tga::core::db::Database;

/// What one team's incremental comments pass read and wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CommentsOutcome {
    /// Team key.
    pub team_key: String,
    /// The `updatedAt` lower bound sent (stored cursor minus the overlap);
    /// `None` is a full sweep of the team (no cursor yet).
    pub since: Option<DateTime<Utc>>,
    /// Comments Linear returned; 0 on a dry run, which sends no request.
    pub read: usize,
    /// Comment rows upserted.
    pub written: usize,
    /// Stored comments whose issue has no `linear_issues` row yet.
    pub unlinked: usize,
    /// Comments with no issue, not stored.
    pub no_issue: usize,
    /// The team's cursor after the pass.
    pub cursor: Option<DateTime<Utc>>,
}

/// RFC 3339 with milliseconds, as the filter and the cursor table hold it.
fn instant(d: DateTime<Utc>) -> String {
    d.to_rfc3339_opts(SecondsFormat::Millis, true)
}

impl CommentsOutcome {
    /// The one report line for this pass.
    pub(crate) fn summary_line(&self, dry_run: bool) -> String {
        let scope = match self.since {
            Some(since) => format!("comments updated after {}", instant(since)),
            None => "every comment of the team (no cursor yet)".to_string(),
        };
        if dry_run {
            return format!(
                "Linear comments ({}): would read {scope} [dry-run: no requests, no writes].",
                self.team_key
            );
        }
        let mut line = format!(
            "Linear comments ({}): read {} {scope}; wrote {} comment row(s)",
            self.team_key, self.read, self.written
        );
        // #190: no comment is dropped silently.
        if self.unlinked > 0 {
            line.push_str(&format!(
                ", {} on issue(s) not yet in linear_issues",
                self.unlinked
            ));
        }
        if self.no_issue > 0 {
            line.push_str(&format!("; skipped {} not on an issue", self.no_issue));
        }
        match self.cursor {
            Some(cursor) => line.push_str(&format!("; cursor {}.", instant(cursor))),
            None => line.push_str("; no cursor yet."),
        }
        line
    }
}

/// Read and store one team's comments updated since its cursor.
///
/// Why: #190 — a comment's create or edit does not reliably move its issue's
/// `updatedAt`, so the pass reads comments by their own `updatedAt`. The
/// cursor moves only after the whole walk and its writes commit (Fail-Open
/// check).
/// What: reads the team's stored cursor ([`comment_cursor`]); the lower
/// bound is the cursor minus [`COMMENT_OVERLAP`], or none for a full sweep
/// when there is no cursor. A dry run stops there and reports the bound.
/// Otherwise [`LinearClient::fetch_team_comments`] walks every page, and
/// [`commit_team_comments`] upserts the comments by id and moves the cursor
/// in one transaction. Any walk failure — a failed page, transport error,
/// spent rate-limit retries, a malformed page — fails the pass with nothing
/// written and the cursor unmoved. Comments Linear deleted are not seen by
/// this walk; their rows stay until a `--backfill` run.
/// Test: `super::comment_cursor_tests::an_edited_comment_is_read_without_an_issue_change`,
/// `super::comment_cursor_tests::a_new_comment_on_an_unchanged_issue_is_stored`,
/// `super::comment_cursor_tests::an_overlap_re_read_writes_no_duplicate`,
/// `super::comment_cursor_tests::a_failed_comments_page_fails_the_run_and_keeps_the_cursor`,
/// `super::comment_cursor_tests::first_run_sweeps_the_team_without_a_bound`,
/// `super::comment_cursor_tests::unlinked_comments_are_stored_and_counted`,
/// `super::comment_cursor_tests::dry_run_sends_no_comments_request`.
///
/// # Errors
///
/// Linear HTTP/auth, rate-limit and paging errors; database errors.
pub(super) async fn sync_comments(
    client: &LinearClient,
    db: &mut Database,
    team_key: &str,
    dry_run: bool,
) -> anyhow::Result<CommentsOutcome> {
    let cursor = comment_cursor(db.connection(), team_key)?;
    let since = cursor.map(|c| c - COMMENT_OVERLAP);
    let mut outcome = CommentsOutcome {
        team_key: team_key.to_string(),
        since,
        read: 0,
        written: 0,
        unlinked: 0,
        no_issue: 0,
        cursor,
    };
    tracing::info!(
        team = %team_key,
        since = ?since,
        dry_run,
        "starting Linear incremental comments pass"
    );
    if dry_run {
        return Ok(outcome);
    }
    let walk_started = Utc::now();
    // #190 (Fail-Open check): the walk completes before anything is written.
    let nodes = client.fetch_team_comments(team_key, since).await?;
    let write = commit_team_comments(db, team_key, &nodes, walk_started)?;
    outcome.read = nodes.len();
    outcome.written = write.written;
    outcome.unlinked = write.unlinked;
    outcome.no_issue = write.no_issue;
    outcome.cursor = write.cursor;
    Ok(outcome)
}

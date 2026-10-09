//! One team's pass of `tga linear sync` (#190).
//!
//! Why: the all-teams mode runs the same fetch, store and cursor steps once
//! per team, and `mod.rs` would pass the line cap holding them inline.
//! What: [`sync_team`] and the [`TeamOutcome`] it reports.
//! Test: `super::sync_tests`.

use chrono::{DateTime, Utc};
use std::collections::HashMap;

use super::activity_sync::ActivityOutcome;
use super::comment_sync::CommentsOutcome;
use super::LinearSyncArgs;
use tga::collect::linear::issue::ISSUE_FIELDS_VERSION;
use tga::collect::linear::sync::{next_cursor, resolve_scope};
use tga::collect::linear::{
    plan_linear_issues, upsert_linear_issues_in, ChangeCounts, IssueChange, IssueQuery,
    LinearClient, LinearIssue,
};
use tga::collect::linear_pipeline::persist_work_items_in;
use tga::core::db::{
    get_linear_cursor, get_linear_fields_version, set_linear_cursor, set_linear_fields_version,
    Database,
};

/// What one team's sync fetched and did (or, under `--dry-run`, would do).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamOutcome {
    /// Team key.
    pub team_key: String,
    /// Issues Linear returned for the window.
    pub fetched: usize,
    /// How many of them are archived.
    pub archived: usize,
    /// New / changed / moved / unchanged tallies against the stored rows.
    pub counts: ChangeCounts,
    /// #190 step 6: the history and comments passes run after the issues.
    /// Comments land here only under `--backfill` (the per-issue walk).
    pub activity: Vec<ActivityOutcome>,
    /// #190: the incremental comments pass (`--comments` without
    /// `--backfill`).
    pub comments: Option<CommentsOutcome>,
}

impl TeamOutcome {
    /// The one report line `tga linear sync` prints for this team.
    pub(crate) fn summary_line(&self, dry_run: bool) -> String {
        let c = &self.counts;
        format!(
            "Linear sync ({}): {} issue(s) fetched, {} archived; {} {} \
             ({} new, {} changed, {} moved, {} unchanged){}.",
            self.team_key,
            self.fetched,
            self.archived,
            if dry_run { "would write" } else { "wrote" },
            c.written(),
            c.new,
            c.changed,
            c.moved,
            c.unchanged,
            if dry_run { " [dry-run: no writes]" } else { "" }
        )
    }
}

/// Fetch, store and advance the cursor for one team; returns the outcome and
/// the issues fetched.
///
/// Why: #190 — one per-team unit, so `--all-teams` keeps a cursor per team
/// and a re-sync with no remote change writes no issue row.
/// What: resolves the `updatedAt` bound from `--since` / `--backfill` / the
/// team's stored cursor, walks the team's issues (archived included when
/// `include_archived`), then either classifies them read-only (`--dry-run`,
/// which opens the database read-only per #189) or, in one transaction,
/// upserts them by `linear_id`, projects the new and changed ones into
/// `work_items`, and advances the team's cursor to the newest `updatedAt`
/// seen. A failure commits none of the three. The cursor never moves
/// backward.
///
/// #190 step 3: a team whose cursor records an older field set than
/// [`ISSUE_FIELDS_VERSION`] ignores the cursor and reads its whole history,
/// so unchanged issues get the new fields too. A run that read the whole
/// history with archived issues included records the current version in the
/// same transaction; a `--since` or `--exclude-archived` run does not. A
/// refresh run projects every issue it read into `work_items`, unchanged ones
/// included.
/// Test: `super::sync_tests` (every test there runs through this);
/// `field_refresh_reprojects_unchanged_issues`,
/// `full_read_over_the_cap_fails_and_writes_nothing`,
/// `older_field_set_refetches_full_history_once`,
/// `since_bounded_run_does_not_mark_the_field_set_current`,
/// `failed_field_set_write_rolls_the_team_back`.
///
/// # Errors
///
/// Linear HTTP/auth and cap errors from the walk; database errors.
pub(super) async fn sync_team(
    client: &LinearClient,
    db: &mut Database,
    team_key: &str,
    args: &LinearSyncArgs,
    explicit_since: Option<DateTime<Utc>>,
    include_archived: bool,
) -> anyhow::Result<(TeamOutcome, Vec<LinearIssue>)> {
    let stored_cursor = get_linear_cursor(db.connection(), team_key)?
        .and_then(|c| DateTime::parse_from_rfc3339(&c.last_synced_at).ok())
        .map(|d| d.with_timezone(&Utc));
    // #190 step 3: a cursor stored under an older field set is not a valid
    // lower bound — issues it skips would never get the new fields.
    let stored_version = get_linear_fields_version(db.connection(), team_key)?.unwrap_or(0);
    let refresh = stored_version < ISSUE_FIELDS_VERSION;
    let cursor_bound = if refresh { None } else { stored_cursor };
    let scope = resolve_scope(team_key, explicit_since, args.backfill, cursor_bound);
    let reads_everything = scope.since.is_none() && include_archived;
    tracing::info!(
        team = %team_key,
        since = ?scope.since,
        include_archived,
        backfill = args.backfill,
        dry_run = args.dry_run,
        fields_version = stored_version,
        refresh,
        "starting tga linear sync"
    );

    let query = IssueQuery::new(team_key, scope.since, include_archived);
    // #190: no default cap; a configured cap that is reached is an error.
    let issues = client.fetch_team_issues(&query, args.max_issues).await?;
    let archived = issues.iter().filter(|i| i.is_archived()).count();

    let changes = if args.dry_run {
        // #189: the handle is read-only here; classify, never write.
        plan_linear_issues(db.connection(), &issues)?
    } else {
        // #190: one transaction for the issue rows, their `work_items`
        // projection and the cursor. `Unchanged` skips the projection, so an
        // issue row committed without it would never be projected by a later
        // sync. Dropping `tx` on any `?` below rolls all three back.
        let tx = db.connection_mut().transaction()?;
        let changes = upsert_linear_issues_in(&tx, &issues)?;
        // #7139: the same issues land in `work_items`; no commit correlation
        // here, so `commit_refs` is empty. #190: only new or changed issues,
        // so a re-sync with no remote change writes nothing — except during
        // a field-set refresh, which re-projects every issue it reads so a
        // projection change reaches unchanged issues too.
        let written: Vec<LinearIssue> = issues
            .iter()
            .zip(&changes)
            .filter(|(_, c)| refresh || **c != IssueChange::Unchanged)
            .map(|(i, _)| i.clone())
            .collect();
        persist_work_items_in(&tx, &written, &HashMap::new())?;

        let observed: Vec<DateTime<Utc>> = issues.iter().filter_map(|i| i.updated_at).collect();
        if let Some(next) = next_cursor(&observed) {
            let advance = stored_cursor.map_or(next, |s| s.max(next));
            set_linear_cursor(&tx, team_key, &advance.to_rfc3339(), issues.len() as i64)?;
        }
        // #190 step 3: only a read of the whole history may record the field
        // set as current; its failure rolls the team back like any other.
        if reads_everything && refresh {
            set_linear_fields_version(&tx, team_key, ISSUE_FIELDS_VERSION)?;
        }
        tx.commit()?;
        changes
    };

    let outcome = TeamOutcome {
        team_key: team_key.to_string(),
        fetched: issues.len(),
        archived,
        counts: ChangeCounts::of(&changes),
        activity: Vec::new(),
        comments: None,
    };
    // #190 step 6: the dry run's activity count reads the fetched issues.
    Ok((outcome, issues))
}

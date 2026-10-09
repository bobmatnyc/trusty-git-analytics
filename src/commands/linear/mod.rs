//! `tga linear sync` / `tga linear freshness` — Linear bulk team ingestion
//! (issue #7139) — and `tga linear stats`, the metrics over it (#190).
//!
//! Why: `linear.fetch_on_reference` (`collect::linear_pipeline`) only ever
//! resolves issues a commit message names, so an engagement registered with
//! `[boards.linear]` and no JIRA board had no path to a team's full issue set
//! and no ticket-linked metrics. This module is the Linear counterpart to
//! `commands::jira` (issue #3966): a paginated bulk sync scoped to one team
//! key, and a freshness check reading the same `linear_sync_cursor`
//! bookkeeping the sync writes.
//!
//! ## Differences from `tga jira sync`, deliberately
//!
//! JIRA's sync walks tickets one at a time (a changelog fetch, then a
//! separate comment fetch per ticket), so a single bad ticket needs
//! isolation — the per-ticket circuit breaker and cursor clamp in
//! `commands::jira::run_sync`. Linear's `issues` query is already a bulk
//! paginated read: one page IS the unit of work, and a page either succeeds
//! or the whole run fails and propagates the error before any cursor
//! decision is made. There is no per-issue partial-failure state to isolate,
//! so this module carries none of that machinery.
//!
//! ## Scope
//!
//! `--team` overrides; `--all-teams` syncs every team the API key can see,
//! each under its own cursor (#190); otherwise the sync requires exactly one
//! configured `linear.team_keys` entry.

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Args;

use tga::collect::errors::CollectError;
use tga::collect::linear::sync::validate_team_key;
use tga::collect::linear::{ActivityKind, LinearClient};
use tga::core::config::Config;
use tga::core::db::{get_linear_cursor, list_linear_cursor_teams, Database};

// #190 step 6: per-issue history and comments.
mod activity_sync;
// #190: the incremental comments pass, keyed on the comment's updatedAt.
mod comment_sync;
// #190 step 5: the bulk sync `tga collect` runs under `linear.sync_on_collect`.
pub mod collect_sync;
mod entity_sync;
// #190 step 4: the read-only metrics command.
mod stats;
mod team_sync;
pub use stats::{render_stats, run_stats, run_stats_at, LinearStatsArgs};
use team_sync::{sync_team, TeamOutcome};

/// Arguments for `tga linear sync`.
#[derive(Args, Debug, Default)]
#[command(
    about = "Bulk-sync Linear issues (one team or every team) into linear_issues / work_items.",
    long_about = "Fetch every issue for the configured (or --team-overridden) Linear team,\n\
or for every team the API key can see (--all-teams), and persist it into\n\
`linear_issues` (keyed by Linear's issue id, with every issue field and the\n\
raw GraphQL node) and the source-agnostic `work_items` corpus.\n\n\
Archived issues are included unless --exclude-archived is given.\n\n\
Incremental by default: resumes from the stored `linear_sync_cursor` for each\n\
team. Pass --backfill for a full historical pull (first-ever sync of a team\n\
is always a full pull automatically, even without --backfill). An issue whose\n\
`updatedAt` is already stored is not rewritten.\n\n\
--entities also stores the workspace's teams, users, labels, projects,\n\
project milestones and cycles (archived included) in their own tables, one\n\
transaction per entity set, after the issues.\n\n\
--history stores each issue's workflow-state transitions, one request walk per\n\
issue, for issues that changed since their last read. --comments stores\n\
comment metadata (author, times, body length; never the body): each team's\n\
comments updated since its comment cursor (minus a 10-minute overlap), read\n\
through Linear's workspace `comments` query, or every comment of the team on\n\
the first run, plus every comment of each issue that moved into the team\n\
since the last comments run. With --backfill, --comments walks each issue\n\
instead and also removes comments Linear deleted; the incremental walk cannot\n\
see deletions.\n\n\
Requires `linear.api_key` (or a shared-credential fallback) configured, and\n\
--team, --all-teams, or exactly one entry in `linear.team_keys`.",
    after_help = "EXAMPLES:\n\
  # Incremental sync using the stored cursor (or full history on first run)\n\
  tga linear sync --team ENG\n\n\
  # Every team the API key can see, archived issues included\n\
  tga linear sync --all-teams\n\n\
  # Issues plus teams, users, labels, projects, milestones and cycles\n\
  tga linear sync --all-teams --entities\n\n\
  # Issues plus each changed issue's state transitions and comment metadata\n\
  tga linear sync --team ENG --history --comments\n\n\
  # Full historical backfill, ignoring any stored cursor\n\
  tga linear sync --team ENG --backfill\n\n\
  # Preview without writing to the database\n\
  tga linear sync --all-teams --dry-run"
)]
pub struct LinearSyncArgs {
    /// Restrict sync to a single Linear team key. Overrides
    /// `linear.team_keys` in config.yaml.
    #[arg(long, value_name = "KEY", conflicts_with = "all_teams")]
    pub team: Option<String>,
    /// Sync every team the API key can see, each under its own cursor.
    /// Archived issues are always included in this mode.
    #[arg(long, default_value_t = false)]
    pub all_teams: bool,
    /// Leave archived issues out (single-team mode only).
    #[arg(long, default_value_t = false, conflicts_with = "all_teams")]
    pub exclude_archived: bool,
    /// Only sync issues updated on/after this date (ISO8601 YYYY-MM-DD).
    #[arg(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Full historical backfill: ignore the stored cursor and (unless
    /// --since is also given) sync the entire team history.
    #[arg(long, default_value_t = false)]
    pub backfill: bool,
    /// Fail when a team holds more than N issues in the sync window. Nothing
    /// is written for that team. Default: no cap.
    #[arg(long, value_name = "N")]
    pub max_issues: Option<usize>,
    /// Fetch from Linear and report what would change without writing to
    /// the database or advancing the cursor. Opens the database read-only.
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
    /// After the issues, also sync the workspace's reference entities:
    /// teams, users, labels, projects, project milestones and cycles,
    /// archived included. Each set is a full refresh.
    #[arg(long, default_value_t = false)]
    pub entities: bool,
    /// After each team's issues, read the workflow-state history of every
    /// issue that changed since its history was last read (all of the team's
    /// issues with --backfill) into `fact_linear_transitions`. One request
    /// walk per issue.
    #[arg(long, default_value_t = false)]
    pub history: bool,
    /// After each team's issues, read the team's comments updated since its
    /// comment cursor (every comment on the first run), and every comment of
    /// issues that moved into the team, into
    /// `fact_linear_comment_detail`, 250 per request. With --backfill, walk
    /// each issue's comments instead, which also removes deleted comments.
    /// Comment bodies are never stored.
    #[arg(long, default_value_t = false)]
    pub comments: bool,
}

impl LinearSyncArgs {
    /// The per-issue connections this run reads, in order (#190 step 6).
    fn activity_kinds(&self) -> Vec<ActivityKind> {
        let mut kinds = Vec::new();
        if self.history {
            kinds.push(ActivityKind::History);
        }
        if self.comments {
            kinds.push(ActivityKind::Comments);
        }
        kinds
    }
}

/// Arguments for `tga linear freshness`.
#[derive(Args, Debug)]
#[command(
    about = "Check freshness of the Linear bulk-sync cursor (fails loudly if stale/never run).",
    long_about = "Report the last successful `tga linear sync` run per team, reading\n\
`linear_sync_cursor.last_run_at` — deliberately NOT `linear_issues.fetched_at`,\n\
which is also written by the unrelated per-commit-reference lookup and so\n\
cannot tell \"the bulk sync ran\" apart from \"a commit happened to mention a\n\
ticket\". Exits non-zero (unless --report-only) if any checked team has never\n\
synced or is older than --max-age-days.",
    after_help = "EXAMPLES:\n\
  # Every team with a sync cursor, plus every configured team_keys entry\n\
  tga linear freshness\n\n\
  # Check one team only\n\
  tga linear freshness --team ENG\n\n\
  # Report only, never fail the process\n\
  tga linear freshness --report-only --max-age-days 7"
)]
pub struct LinearFreshnessArgs {
    /// Maximum allowed age (days) since the last successful sync before a
    /// team is considered stale.
    #[arg(long, default_value_t = 2)]
    pub max_age_days: i64,
    /// Always exit 0, even when a team is stale or has never synced.
    #[arg(long, default_value_t = false)]
    pub report_only: bool,
    /// Check only this Linear team. Default: every team with a sync cursor,
    /// plus every configured `linear.team_keys` entry.
    #[arg(long, value_name = "KEY")]
    pub team: Option<String>,
}

fn parse_cli_date(s: &str) -> anyhow::Result<DateTime<Utc>> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("invalid --since date '{s}' (expected YYYY-MM-DD): {e}"))?;
    let ndt = d
        .and_hms_opt(0, 0, 0)
        .ok_or_else(|| anyhow::anyhow!("invalid time-of-day for date '{s}'"))?;
    Ok(Utc.from_utc_datetime(&ndt))
}

/// Resolve the effective Linear team key from `--team` or a single
/// `linear.team_keys` entry.
///
/// # Errors
///
/// Returns an error when neither yields exactly one team: `tga linear sync`
/// needs one unambiguous scope per run, the same way `tga jira sync` needs
/// one `project_key`.
fn resolve_team_key(config: &Config, cli_team: Option<&str>) -> anyhow::Result<String> {
    let key = match cli_team {
        Some(t) => t.to_string(),
        None => {
            let configured = config
                .linear
                .as_ref()
                .map(|l| l.team_keys.clone())
                .unwrap_or_default();
            match configured.as_slice() {
                [one] => one.clone(),
                [] => anyhow::bail!(
                    "no Linear team scope: pass --team <KEY> or --all-teams, or set exactly one \
                     entry in linear.team_keys in config.yaml"
                ),
                many => anyhow::bail!(
                    "ambiguous Linear team scope: linear.team_keys has {} entries ({}); \
                     pass --team <KEY> to pick one, or --all-teams",
                    many.len(),
                    many.join(", ")
                ),
            }
        }
    };
    validate_team_key(&key).map_err(|e| anyhow::anyhow!(e))?;
    Ok(key)
}

fn build_client(config: &Config) -> anyhow::Result<LinearClient> {
    let linear_config = config
        .linear
        .clone()
        .ok_or_else(|| anyhow::anyhow!("`linear:` section is missing from config.yaml"))?;
    LinearClient::new(&linear_config).map_err(|e| match e {
        CollectError::Config(msg) => anyhow::anyhow!("{msg}"),
        other => anyhow::anyhow!(other),
    })
}

/// Dispatch entry point for `tga linear sync`.
///
/// # Errors
///
/// Propagates Linear HTTP/auth failures and database errors.
pub async fn run_sync(
    config: Config,
    db: &mut Database,
    args: LinearSyncArgs,
) -> anyhow::Result<()> {
    // Resolve a single-team scope before the client, so a scope error is
    // reported even when no API key is configured.
    if !args.all_teams {
        resolve_team_key(&config, args.team.as_deref())?;
    }
    let client = build_client(&config)?;
    run_sync_with(&client, &config, db, &args).await?;
    Ok(())
}

/// [`run_sync`] with the client supplied, returning one outcome per team.
///
/// Why: #190 — the all-teams mode, the cap failure and the dry-run report
/// all sit above the HTTP layer, and a test needs to drive them against a
/// mock server.
/// What: resolves the team list (`--all-teams` lists every team through the
/// API; otherwise one key), then syncs each team in order with
/// [`team_sync::sync_team`], printing one line per team. Archived issues are
/// included unless `--exclude-archived`. The first team that fails stops the
/// run with an error naming it; teams already synced keep their rows and
/// cursors. #190 step 6: with `--history` / `--comments`,
/// [`activity_sync::sync_activity`] runs after each team's issue pass —
/// except an incremental `--comments`, which runs
/// [`comment_sync::sync_comments`] (#190: keyed on the comment's own
/// `updatedAt`, not the issue's). An
/// issue Linear no longer has is counted and tombstoned, not a failure
/// (owner ruling D28); any other failure stops the run the same way, after
/// the team's issue rows and cursor are committed. With `--entities`,
/// [`entity_sync::sync_entities`] runs last.
/// Test: `sync_tests::all_teams_syncs_each_team_under_its_own_cursor`,
/// `activity_sync_tests::a_failed_history_read_fails_the_sync_and_stays_due`,
/// `activity_sync_tests::a_missing_issue_is_tombstoned_and_the_run_goes_on`,
/// `comment_cursor_tests::a_failed_comments_page_fails_the_run_and_keeps_the_cursor`,
/// `comment_cursor_tests::backfill_keeps_the_per_issue_walk`,
/// `sync_tests::cap_exceeded_fails_and_writes_nothing`,
/// `sync_tests::dry_run_reports_and_writes_nothing`,
/// `sync_tests::second_sync_with_no_remote_change_writes_nothing`,
/// `sync_tests::moved_issue_keeps_one_row_across_runs`,
/// `sync_tests::raw_json_estimate_reaches_the_effort_extractor`.
///
/// # Errors
///
/// Team resolution, Linear HTTP/auth, cap and database failures.
pub(crate) async fn run_sync_with(
    client: &LinearClient,
    config: &Config,
    db: &mut Database,
    args: &LinearSyncArgs,
) -> anyhow::Result<Vec<TeamOutcome>> {
    let explicit_since = args.since.as_deref().map(parse_cli_date).transpose()?;
    // #190: archived issues are part of the data set unless asked otherwise;
    // the all-teams mode always includes them.
    let include_archived = args.all_teams || !args.exclude_archived;
    let team_keys = if args.all_teams {
        let teams = client.fetch_teams(include_archived).await?;
        let mut keys = Vec::with_capacity(teams.len());
        for team in teams {
            validate_team_key(&team.key).map_err(|e| anyhow::anyhow!(e))?;
            keys.push(team.key);
        }
        keys
    } else {
        vec![resolve_team_key(config, args.team.as_deref())?]
    };

    let mut outcomes = Vec::with_capacity(team_keys.len());
    for team_key in &team_keys {
        let earlier = outcomes.len();
        let (mut outcome, fetched) =
            sync_team(client, db, team_key, args, explicit_since, include_archived)
                .await
                .map_err(|e| {
                    e.context(format!(
                        "tga linear sync failed for team {team_key}; {earlier} earlier team(s) \
                         in this run kept their rows and cursors"
                    ))
                })?;
        println!("{}", outcome.summary_line(args.dry_run));
        // #190 step 6: a dry run stored nothing, so its fetched issues are
        // the overlay the due count reads.
        let overlay = if args.dry_run {
            fetched.as_slice()
        } else {
            &[]
        };
        for kind in args.activity_kinds() {
            // #190: an incremental run reads comments by their own
            // updatedAt; `--backfill` keeps the per-issue walk.
            if kind == ActivityKind::Comments && !args.backfill {
                let comments = comment_sync::sync_comments(client, db, team_key, args.dry_run)
                    .await
                    .map_err(|e| {
                        e.context(format!(
                            "tga linear sync failed reading Linear comments for team {team_key}; \
                             the team's issue rows and cursor were kept, no comment from this \
                             walk was written, and the comments cursor did not move"
                        ))
                    })?;
                println!("{}", comments.summary_line(args.dry_run));
                outcome.comments = Some(comments);
                continue;
            }
            let activity = activity_sync::sync_activity(
                client,
                db,
                team_key,
                kind,
                args.backfill,
                args.dry_run,
                overlay,
            )
            .await
            .map_err(|e| {
                e.context(format!(
                    "tga linear sync failed reading Linear {} for team {team_key}; the team's \
                     issue rows and cursor were kept, and every issue not yet read stays due \
                     for the next run",
                    kind.connection()
                ))
            })?;
            println!("{}", activity.summary_line(args.dry_run));
            outcome.activity.push(activity);
        }
        outcomes.push(outcome);
    }
    if args.all_teams {
        let fetched: usize = outcomes.iter().map(|o| o.fetched).sum();
        println!(
            "Linear sync: {} team(s), {fetched} issue(s) fetched{}.",
            outcomes.len(),
            if args.dry_run {
                " [dry-run: no writes]"
            } else {
                ""
            }
        );
    }
    // #190: the reference entities run after the issues, behind a flag, so
    // the issue path is unchanged without it.
    if args.entities {
        entity_sync::sync_entities(client, db, args.dry_run).await?;
    }
    Ok(outcomes)
}

/// Dispatch entry point for `tga linear freshness`.
///
/// Scoping mirrors `tga jira freshness`: with no `--team`, every team
/// carrying a sync cursor is checked individually, plus every configured
/// `linear.team_keys` entry — so a team that has *never* synced (no cursor
/// row at all) is still reported, not silently skipped.
///
/// # Errors
///
/// Returns an error (non-zero exit) if any checked team has never synced or
/// is older than `--max-age-days`, unless `--report-only` was passed.
pub fn run_freshness(
    config: &Config,
    db: &Database,
    args: LinearFreshnessArgs,
) -> anyhow::Result<()> {
    let scopes: Vec<String> = match &args.team {
        Some(t) => {
            validate_team_key(t).map_err(|e| anyhow::anyhow!(e))?;
            vec![t.clone()]
        }
        None => {
            let mut teams = list_linear_cursor_teams(db.connection())?;
            if let Some(cfg) = &config.linear {
                for key in &cfg.team_keys {
                    if !teams.contains(key) {
                        teams.push(key.clone());
                    }
                }
            }
            teams.sort();
            teams
        }
    };

    if scopes.is_empty() {
        anyhow::bail!(
            "no Linear team to check: pass --team <KEY> or configure linear.team_keys / run \
             `tga linear sync` at least once"
        );
    }

    let now = Utc::now();
    let mut any_stale = false;
    for team in &scopes {
        let cursor = get_linear_cursor(db.connection(), team)?;
        let (age_desc, stale) = match &cursor {
            Some(c) => match DateTime::parse_from_rfc3339(&c.last_run_at) {
                Ok(parsed) => {
                    let age_days =
                        (now - parsed.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0;
                    (
                        format!("{age_days:.1}d old"),
                        age_days > args.max_age_days as f64,
                    )
                }
                Err(_) => ("unparseable last_run_at".to_string(), true),
            },
            None => ("never synced".to_string(), true),
        };
        let verdict = if stale { "STALE" } else { "OK" };
        println!(
            "{team:<10} linear_sync_cursor       issues={:<8} last_run={:<20} [{verdict}]",
            cursor.as_ref().map_or(0, |c| c.issues_synced),
            age_desc,
        );
        if stale {
            any_stale = true;
        }
    }

    if any_stale {
        let msg = format!(
            "one or more Linear teams have never synced or are stale (threshold: {} day(s), \
             teams checked: {}); see rows above",
            args.max_age_days,
            scopes.join(", ")
        );
        if args.report_only {
            tracing::warn!("{msg}");
            return Ok(());
        }
        anyhow::bail!(msg);
    }
    Ok(())
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

#[cfg(test)]
mod sync_tests;

#[cfg(test)]
mod entity_sync_tests;

#[cfg(test)]
mod activity_sync_tests;

#[cfg(test)]
mod comment_cursor_tests;

//! The pre-flight API call plan for `tga linear sync` (#272, owner ruling
//! D94: the call count is reported before any backfill).
//!
//! Why: a full-history read of a large team costs hundreds of requests. The
//! operator sees the count before the first issue request, and `--plan`
//! prints it without fetching any issue.
//! What: [`team_plan`] decides per team whether the run reads the whole
//! history ([`FullRead`]) and counts the stored issues; [`TeamPlan::calls`]
//! is the arithmetic:
//!
//! `calls = 1 (the team) + ceil(issues / 50) (issue pages) + issues (with
//! --history, one walk per issue)`
//!
//! where `issues` is the team's stored issue count (0 on a first sync, when
//! nothing is stored yet; the notice says the count is unknown). A run with
//! `--since`, or an incremental run, reads less than the plan. `--all-teams`
//! adds one team-list query.
//! Test: `plan_tests`.

use chrono::{DateTime, Utc};

use super::LinearSyncArgs;
use tga::collect::linear::bulk::ISSUES_PAGE_SIZE;
use tga::collect::linear::issue::ISSUE_FIELDS_VERSION;
use tga::core::db::{get_linear_cursor, get_linear_fields_version, Database};

/// Why a team's run reads its whole history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FullRead {
    /// `--backfill` without `--since`.
    Backfill,
    /// The team has no sync cursor yet.
    FirstSync,
    /// The team's cursor records an older field set (#190 step 3).
    FieldRefresh {
        /// Stored field-set version.
        old: i64,
        /// Current field-set version.
        new: i64,
    },
}

/// One team's planned requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TeamPlan {
    /// Team key.
    pub team_key: String,
    /// `Some` when the run reads the whole history.
    pub full_read: Option<FullRead>,
    /// Issues stored for the team.
    pub stored_issues: usize,
    /// Whether `--history` adds one walk per issue.
    pub history: bool,
}

impl TeamPlan {
    /// Issue pages: `ceil(stored_issues / 50)`.
    pub(crate) fn issue_pages(&self) -> usize {
        self.stored_issues.div_ceil(ISSUES_PAGE_SIZE)
    }

    /// History walks: one per stored issue with `--history`, else 0.
    pub(crate) fn history_walks(&self) -> usize {
        if self.history {
            self.stored_issues
        } else {
            0
        }
    }

    /// Planned calls: 1 for the team, plus the pages, plus the walks.
    pub(crate) fn calls(&self) -> usize {
        1 + self.issue_pages() + self.history_walks()
    }

    /// The stderr notice a sync prints before its first issue request, or
    /// `None` when the run reads neither the whole history nor `--history`.
    ///
    /// #272: a `--since` run and a first sync no longer claim a field-set
    /// re-read; the notice names the actual reason.
    pub(crate) fn notice(&self) -> Option<String> {
        let reason = match self.full_read {
            Some(FullRead::Backfill) => "backfill; re-reading the team's full history".to_string(),
            Some(FullRead::FirstSync) => {
                "first sync; reading the team's full history (issue count unknown until read)"
                    .to_string()
            }
            Some(FullRead::FieldRefresh { old, new }) => {
                format!("field set v{old} -> v{new}; re-reading the team's full history")
            }
            None if self.history => "incremental, with --history".to_string(),
            None => return None,
        };
        Some(format!(
            "Linear sync ({}): {reason}; planned API calls: about {} \
             (1 team + {} issue page(s) for {} stored issue(s), {ISSUES_PAGE_SIZE} per page{}).",
            self.team_key,
            self.calls(),
            self.issue_pages(),
            self.stored_issues,
            if self.history {
                format!(" + {} history walk(s)", self.history_walks())
            } else {
                String::new()
            }
        ))
    }
}

/// Plan one team's run from its stored cursor, field-set version and issue
/// count.
///
/// # Errors
///
/// Database read failures.
pub(crate) fn team_plan(
    db: &Database,
    team_key: &str,
    args: &LinearSyncArgs,
    explicit_since: Option<DateTime<Utc>>,
) -> anyhow::Result<TeamPlan> {
    let has_cursor = get_linear_cursor(db.connection(), team_key)?.is_some();
    let stored_version = get_linear_fields_version(db.connection(), team_key)?.unwrap_or(0);
    let stored: i64 = db.connection().query_row(
        "SELECT COUNT(*) FROM linear_issues WHERE team_key = ?1",
        [team_key],
        |r| r.get(0),
    )?;
    let full_read = if explicit_since.is_some() {
        None
    } else if args.backfill {
        Some(FullRead::Backfill)
    } else if !has_cursor {
        Some(FullRead::FirstSync)
    } else if stored_version < ISSUE_FIELDS_VERSION {
        Some(FullRead::FieldRefresh {
            old: stored_version,
            new: ISSUE_FIELDS_VERSION,
        })
    } else {
        None
    };
    Ok(TeamPlan {
        team_key: team_key.to_string(),
        full_read,
        stored_issues: usize::try_from(stored)?,
        history: args.history,
    })
}

/// The whole run's plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyncPlan {
    /// 1 with `--all-teams` (the team-list query), else 0.
    pub team_list_queries: usize,
    /// One plan per team.
    pub teams: Vec<TeamPlan>,
}

impl SyncPlan {
    /// Total planned calls.
    pub(crate) fn total_calls(&self) -> usize {
        self.team_list_queries + self.teams.iter().map(TeamPlan::calls).sum::<usize>()
    }

    /// The report `tga linear sync --plan` prints.
    pub(crate) fn render(&self) -> String {
        let mut out = format!(
            "Linear sync plan: about {} API call(s) across {} team(s){}.\n",
            self.total_calls(),
            self.teams.len(),
            if self.team_list_queries > 0 {
                ", including 1 team-list query"
            } else {
                ""
            }
        );
        for t in &self.teams {
            let mode = match t.full_read {
                Some(FullRead::Backfill) => "full history (backfill)".to_string(),
                Some(FullRead::FirstSync) => "full history (first sync)".to_string(),
                Some(FullRead::FieldRefresh { old, new }) => {
                    format!("full history (field set v{old} -> v{new})")
                }
                None => "incremental (upper bound)".to_string(),
            };
            out.push_str(&format!(
                "  {:<10} {mode}: {} call(s) = 1 team + {} issue page(s) ({} stored issue(s)) \
                 + {} history walk(s)\n",
                t.team_key,
                t.calls(),
                t.issue_pages(),
                t.stored_issues,
                t.history_walks()
            ));
        }
        out.push_str("No issue was fetched (--plan).");
        out
    }
}

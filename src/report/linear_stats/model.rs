//! The `tga linear stats` output shape (#190 step 4).
//!
//! Every struct serializes to the JSON `tga linear stats --json` prints. Maps
//! are `BTreeMap`, so the output is deterministic. The whole-workspace figure
//! sits in [`Grouped::all`], never under a team key, so no team key can collide
//! with it. Percent fields end in `_pct` and are `None` when the denominator
//! is 0.

use std::collections::BTreeMap;

use serde::Serialize;

use super::dist::Dist;
use super::tolerance::NamedTolerance;

/// A figure for the whole workspace and for each team, by current team key.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Grouped<T> {
    /// Every team together.
    pub all: T,
    /// One entry per team key that has data.
    pub teams: BTreeMap<String, T>,
}

/// Every Linear delivery metric `tga linear stats` computes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[non_exhaustive]
pub struct LinearStats {
    /// [`super::LINEAR_STATS_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The as-of instant (RFC 3339) for windows, ages and overdue measures.
    pub as_of: String,
    /// Lower bound on `createdAt` for [`LinearStats::field_use`], when set.
    pub field_use_created_since: Option<String>,
    /// Lower bound on `completedAt` for the concentration figures, when set.
    pub concentration_completed_since: Option<String>,
    /// C0: population and entity counts.
    pub population: Population,
    /// C1, C1b, C1c: completions per quarter and the four-quarter trend.
    pub completions: Completions,
    /// C2: lead and cycle time by completion year.
    pub lead_cycle: Grouped<BTreeMap<i32, LeadCycle>>,
    /// C3: cycle completion and carry-over by year of `endsAt`.
    pub cycles: CycleStats,
    /// C5: issues by the number of bracketed tags their title starts with.
    pub title_tags: Grouped<BTreeMap<usize, usize>>,
    /// C9: field use.
    pub field_use: FieldUse,
    /// C11: projects.
    pub projects: ProjectStats,
    /// C12: cancel rate by closure year.
    pub cancel_rate: Grouped<BTreeMap<i32, CancelYear>>,
    /// C14: contributors and concentration.
    pub people: People,
    /// Where the reference's code and its published text differ, and how
    /// this output carries both.
    pub tolerances: Vec<NamedTolerance>,
}

/// C0: what the database holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Population {
    /// Issues in the metrics (rows with a Linear id).
    pub issues: usize,
    /// `linear_issues` rows without a Linear id, written before the id was
    /// stored. Left out of every metric; the next sync fills them in.
    pub rows_without_linear_id: usize,
    /// Issues with `archivedAt` set.
    pub archived: usize,
    /// `archived` over `issues`.
    pub archived_pct: Option<f64>,
    /// Issues per `state.type`.
    pub by_state_type: BTreeMap<String, usize>,
    /// Issues with no `state.type`; they count in no state-based metric.
    pub without_state_type: usize,
    /// Completed issues with no `completedAt`; they count in no
    /// completion-dated metric.
    pub completed_without_completed_at: usize,
    /// Issues per current team key.
    pub issues_by_team: BTreeMap<String, usize>,
    /// Entity rows not marked removed, per table.
    pub teams: usize,
    /// See [`Population::teams`].
    pub users: usize,
    /// See [`Population::teams`].
    pub labels: usize,
    /// See [`Population::teams`].
    pub projects: usize,
    /// See [`Population::teams`].
    pub milestones: usize,
    /// See [`Population::teams`].
    pub cycles: usize,
}

/// C1, C1b, C1c.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Completions {
    /// Completed issues per `YYYY-Qn` of `completedAt`.
    pub per_quarter: Grouped<BTreeMap<String, usize>>,
    /// The four quarters before the recent window.
    pub prior_window: Vec<String>,
    /// The four whole quarters before the as-of quarter.
    pub recent_window: Vec<String>,
    /// Prior vs recent window, per team with any completion.
    pub trend: Grouped<Trend>,
    /// The same comparison over the stable teams only.
    pub stable: StableTrend,
}

/// Completions in the prior and recent windows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Trend {
    /// Sum over the prior window.
    pub prior: usize,
    /// Sum over the recent window.
    pub recent: usize,
    /// `(recent - prior) / prior`; `None` when prior is 0.
    pub change_pct: Option<f64>,
}

/// C1c: teams with a completion in every quarter of both windows.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct StableTrend {
    /// The stable team keys, sorted.
    pub teams: Vec<String>,
    /// Their summed trend.
    pub trend: Trend,
}

/// C2 for one group and year.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct LeadCycle {
    /// Days from `createdAt` to `completedAt`.
    pub lead_days: Dist,
    /// Days from `startedAt` to `completedAt`, issues with a start only.
    pub cycle_days: Dist,
    /// Completed issues with no `startedAt`.
    pub no_started_at: usize,
}

/// C3.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CycleStats {
    /// Cycles not marked removed.
    pub total: usize,
    /// Completed cycles with a last issue count above 0. Each is measured
    /// unless it is also in [`CycleStats::missing_completed_count`].
    pub completed_non_empty: usize,
    /// Completed, non-empty cycles with no completed-issue count. Counted
    /// here, measured nowhere.
    pub missing_completed_count: usize,
    /// Per group, by UTC year of `endsAt`.
    pub by_year: Grouped<BTreeMap<i32, CycleYear>>,
}

/// C3 for one group and year.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CycleYear {
    /// Cycles measured.
    pub cycles: usize,
    /// Sum of each cycle's last issue count.
    pub scope: i64,
    /// Sum of each cycle's last completed-issue count.
    pub done: i64,
    /// `done / scope`, pooled over issue slots.
    pub completion_rate_pooled_pct: Option<f64>,
    /// `(scope - done) / cycles`. Issue slots: an issue carried through
    /// several cycles counts once in each (tolerance `c3-carryover-slots`).
    pub carryover_issue_slots_per_cycle: Option<f64>,
    /// Median of each cycle's `done / scope`.
    pub completion_rate_median_cycle_pct: Option<f64>,
    /// Median days from `startsAt` to `endsAt`.
    pub cycle_length_median_days: Option<f64>,
}

/// C9.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FieldUse {
    /// Share of issues with each field set, per group.
    pub shares: Grouped<FieldShares>,
    /// The same population, teams that use estimates only.
    pub estimating: EstimatingTeams,
}

/// C9 for one group.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct FieldShares {
    /// Issues in the group.
    pub n: usize,
    /// `estimate` set (0 counts as set).
    pub estimate_pct: Option<f64>,
    /// At least one label.
    pub label_pct: Option<f64>,
    /// An assignee.
    pub assignee_pct: Option<f64>,
    /// A project.
    pub project_pct: Option<f64>,
    /// A due date.
    pub due_date_pct: Option<f64>,
    /// A cycle.
    pub cycle_pct: Option<f64>,
    /// A parent issue.
    pub parent_pct: Option<f64>,
}

/// C9: teams whose current estimation type is not `notUsed`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct EstimatingTeams {
    /// Their keys, sorted.
    pub teams: Vec<String>,
    /// Their issues in the population.
    pub n: usize,
    /// Of those, with an estimate.
    pub estimated_pct: Option<f64>,
    /// Their completed issues in the population.
    pub completed_n: usize,
    /// Of those, with an estimate.
    pub completed_estimated_pct: Option<f64>,
}

/// C11.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ProjectStats {
    /// Projects not marked removed.
    pub total: usize,
    /// Per project `state`.
    pub by_state: BTreeMap<String, usize>,
    /// Projects with no target date, per state.
    pub no_target_by_state: BTreeMap<String, usize>,
    /// Started projects whose target date is before the as-of date.
    pub started_past_target: usize,
    /// Their days from midnight UTC of the target date to the as-of instant.
    pub started_overdue_days: Dist,
    /// Completed projects with a target date and a `completedAt`.
    pub completed_with_target: usize,
    /// Of those, completed on a later UTC calendar day than the target
    /// (the published rule).
    pub late: usize,
    /// Whole calendar days late, over [`ProjectStats::late`].
    pub late_slip_days: Dist,
    /// Completed on the target day itself, after midnight UTC. Late under
    /// the reference's code rule, on time under the published rule.
    pub late_same_day: usize,
    /// Completed after midnight UTC of the target date: `late` plus
    /// `late_same_day` (the reference's code rule).
    pub late_code_rule: usize,
    /// Fractional days from midnight UTC of the target date, over
    /// [`ProjectStats::late_code_rule`].
    pub late_code_rule_slip_days: Dist,
    /// Projects with a lead.
    pub lead_set_pct: Option<f64>,
    /// Issues that belong to a project.
    pub issues_in_project_pct: Option<f64>,
    /// Milestones not marked removed.
    pub milestones: usize,
    /// Of those, with a target date.
    pub milestones_with_target_pct: Option<f64>,
}

/// C12 for one group and closure year.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CancelYear {
    /// `completed + canceled + duplicate`.
    pub closed: usize,
    /// State type `completed`.
    pub completed: usize,
    /// State type `canceled`.
    pub canceled: usize,
    /// State type `duplicate`: in the denominator, never the numerator.
    pub duplicate: usize,
    /// `canceled / closed`.
    pub cancel_pct: Option<f64>,
}

/// C14.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct People {
    /// Users not marked removed.
    pub users: usize,
    /// Of those, active.
    pub active_users: usize,
    /// Distinct assignees of completed issues, per completion year.
    pub contributors: Grouped<BTreeMap<i32, usize>>,
    /// Who closed the completed work, per group.
    pub concentration: Grouped<Concentration>,
}

/// C14 concentration for one group.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[non_exhaustive]
pub struct Concentration {
    /// Completed issues in the window.
    pub completed: usize,
    /// Of those, with an assignee: the share denominator.
    pub assigned: usize,
    /// `completed - assigned`.
    pub unassigned: usize,
    /// Distinct assignees.
    pub assignees: usize,
    /// The busiest assignee's share of `assigned`.
    pub top1_pct: Option<f64>,
    /// The three busiest assignees' share.
    pub top3_pct: Option<f64>,
    /// The five busiest assignees' share.
    pub top5_pct: Option<f64>,
}

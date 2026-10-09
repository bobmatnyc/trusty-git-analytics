//! Linear delivery metrics computed from the synced Linear tables (#190
//! step 4).
//!
//! Why: `tga linear sync` stores issues, teams, users, labels, projects,
//! milestones and cycles, but no report read them, so a Linear-only
//! engagement had no completions, lead time, cycle or project figures. These
//! metrics follow a published set of definitions; [`tolerance`] names the
//! places where that text and the code behind its numbers differ.
//! What: [`compute_linear_stats`] reads the tables once and returns
//! [`LinearStats`]; [`render_markdown`] renders it as a report block. Every
//! figure is per current team key and for the whole workspace, archived
//! issues included, with the conventions in [`dist`]: UTC calendar quarters
//! and years, fractional days, type-7 percentiles, one-decimal half-even
//! rounding. Completed means `state.type == completed`.
//! Test: `tests` below (golden figures against an independent recomputation).

pub mod dist;
mod issues;
mod load;
mod markdown;
pub mod model;
mod projects;
pub mod tolerance;
mod usage;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;

pub use dist::Dist;
pub use markdown::render_markdown;
pub use model::{
    CancelYear, Completions, Concentration, CycleStats, CycleYear, EstimatingTeams, FieldShares,
    FieldUse, Grouped, LeadCycle, LinearStats, People, Population, ProjectStats, StableTrend,
    Trend,
};
pub use tolerance::{NamedTolerance, TOLERANCES};

use crate::core::config::LinearStatsConfig;
use crate::report::errors::Result;

/// Version of the [`LinearStats`] JSON shape. Bump it when a field is
/// renamed, removed or changes meaning.
pub const LINEAR_STATS_SCHEMA_VERSION: u32 = 1;

/// What [`compute_linear_stats`] measures against.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StatsOptions {
    /// The instant windows, ages and overdue measures are taken at. The
    /// as-of quarter is partial and left out of the trend windows.
    pub as_of: DateTime<Utc>,
    /// Field use (C9) counts only issues created on or after this UTC date.
    /// `None`: every issue.
    pub field_use_created_since: Option<NaiveDate>,
    /// Concentration (C14) counts only issues completed on or after this
    /// UTC date. `None`: every completed issue.
    pub concentration_completed_since: Option<NaiveDate>,
}

impl StatsOptions {
    /// Options at `as_of` with no lower bounds.
    #[must_use]
    pub fn new(as_of: DateTime<Utc>) -> Self {
        Self {
            as_of,
            field_use_created_since: None,
            concentration_completed_since: None,
        }
    }

    /// Options at `as_of` with the lower bounds from `linear.stats`.
    #[must_use]
    pub fn from_config(as_of: DateTime<Utc>, config: &LinearStatsConfig) -> Self {
        Self {
            as_of,
            field_use_created_since: config.field_use_created_since,
            concentration_completed_since: config.concentration_completed_since,
        }
    }
}

/// Compute every Linear delivery metric from the tables on `conn`.
///
/// Why: one read-only pass that `tga linear stats` prints as JSON or as a
/// Markdown block.
/// What: loads `linear_issues` rows that carry a Linear id, and the entity
/// rows not marked removed, then computes C0 (population), C1/C1b/C1c
/// (completions and trend), C2 (lead and cycle time), C3 (cycles), C5 (title
/// tags), C9 (field use), C11 (projects), C12 (cancel rate) and C14 (people).
/// Writes nothing.
/// Test: `golden_completions`, `golden_lead_cycle`, `golden_cycles`,
/// `golden_projects`, `golden_field_use`, `golden_cancel_rate`,
/// `golden_people`, `unparseable_timestamp_fails_with_the_issue_named`.
///
/// # Errors
///
/// [`crate::report::ReportError::Core`] on a failed read;
/// [`crate::report::ReportError::Report`] naming the row when a stored
/// timestamp, date or label list does not parse.
pub fn compute_linear_stats(conn: &Connection, opts: &StatsOptions) -> Result<LinearStats> {
    let snap = load::load(conn)?;
    Ok(LinearStats {
        schema_version: LINEAR_STATS_SCHEMA_VERSION,
        as_of: opts.as_of.to_rfc3339(),
        field_use_created_since: opts.field_use_created_since.map(|d| d.to_string()),
        concentration_completed_since: opts.concentration_completed_since.map(|d| d.to_string()),
        population: issues::population(&snap),
        completions: issues::completions(&snap.issues, opts.as_of),
        lead_cycle: issues::lead_cycle(&snap.issues),
        cycles: projects::cycles(&snap.cycles),
        title_tags: issues::title_tags(&snap.issues),
        field_use: usage::field_use(&snap, opts.field_use_created_since),
        projects: projects::projects(&snap, opts.as_of),
        cancel_rate: issues::cancel_rate(&snap.issues),
        people: usage::people(&snap, opts.concentration_completed_since),
        tolerances: TOLERANCES.to_vec(),
    })
}

#[cfg(test)]
mod test_fixture;
#[cfg(test)]
mod tests;

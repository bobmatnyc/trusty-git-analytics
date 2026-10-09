//! Read the Linear tables into the rows the metrics use (#190 step 4).
//!
//! Why: every metric is a pass over issues, cycles or projects; reading each
//! table once and parsing its timestamps once keeps the metrics plain loops.
//! What: [`load`] returns a [`Snapshot`]. A stored timestamp, date or label
//! list that does not parse fails the load with the row named: a figure
//! computed around a bad row would be wrong without saying so. So does a
//! missing value Linear always sends: an issue's `createdAt`, a cycle's
//! `startsAt` or `endsAt`. Any other NULL is not an error; each metric states
//! what it does with it.
//! Test: `report::linear_stats::tests::unparseable_timestamp_fails_with_the_issue_named`,
//! `report::linear_stats::tests::missing_created_at_fails_with_the_issue_named`,
//! `report::linear_stats::tests::missing_cycle_end_fails_with_the_cycle_named`,
//! `report::linear_stats::tests::unparseable_label_list_fails_with_the_issue_named`,
//! `report::linear_stats::tests::unparseable_target_date_fails_with_the_project_named`.

use std::collections::HashMap;

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::Connection;

use crate::core::errors::TgaError;
use crate::report::errors::{ReportError, Result};

/// One `linear_issues` row with a Linear id.
#[derive(Debug, Clone)]
pub(crate) struct IssueRow {
    pub team: String,
    pub title: String,
    pub state_type: Option<String>,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub canceled_at: Option<DateTime<Utc>>,
    pub has_estimate: bool,
    pub has_labels: bool,
    pub assignee_id: Option<String>,
    pub has_project: bool,
    pub has_due_date: bool,
    pub has_cycle: bool,
    pub has_parent: bool,
}

impl IssueRow {
    /// `state.type` equals `t`.
    pub fn is(&self, t: &str) -> bool {
        self.state_type.as_deref() == Some(t)
    }
}

/// One `linear_cycles` row.
#[derive(Debug, Clone)]
pub(crate) struct CycleRow {
    pub team: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub completed: bool,
    pub scope: Option<i64>,
    pub done: Option<i64>,
}

/// One `linear_projects` row.
#[derive(Debug, Clone)]
pub(crate) struct ProjectRow {
    pub state: String,
    pub target_date: Option<NaiveDate>,
    pub completed_at: Option<DateTime<Utc>>,
    pub has_lead: bool,
}

/// One `linear_teams` row.
#[derive(Debug, Clone)]
pub(crate) struct TeamRow {
    pub key: String,
    pub estimation_type: Option<String>,
}

/// Everything the metrics read.
#[derive(Debug, Clone, Default)]
pub(crate) struct Snapshot {
    pub issues: Vec<IssueRow>,
    pub rows_without_linear_id: usize,
    pub cycles: Vec<CycleRow>,
    pub projects: Vec<ProjectRow>,
    /// Current teams (not removed).
    pub teams: Vec<TeamRow>,
    pub users: usize,
    pub active_users: usize,
    pub labels: usize,
    pub milestones: usize,
    pub milestones_with_target: usize,
}

fn bad(what: &str, row: &str, value: &str, err: impl std::fmt::Display) -> ReportError {
    ReportError::Report(format!(
        "linear stats: {what} {row} holds an unreadable value '{value}': {err}"
    ))
}

fn ts(raw: Option<String>, what: &str, row: &str) -> Result<Option<DateTime<Utc>>> {
    raw.map(|s| {
        DateTime::parse_from_rfc3339(&s)
            .map(|d| d.with_timezone(&Utc))
            .map_err(|e| bad(what, row, &s, e))
    })
    .transpose()
}

/// A value Linear always sends. Its absence means the row is damaged, and a
/// figure computed without it would be wrong without saying so.
fn required<T>(v: Option<T>, what: &str, row: &str) -> Result<T> {
    v.ok_or_else(|| ReportError::Report(format!("linear stats: {what} {row} is missing")))
}

fn db(e: rusqlite::Error) -> ReportError {
    ReportError::Core(TgaError::from(e))
}

/// Read every table the metrics need.
///
/// # Errors
///
/// [`ReportError::Core`] on a failed read; [`ReportError::Report`] naming the
/// row when a stored timestamp, date or label list does not parse.
pub(crate) fn load(conn: &Connection) -> Result<Snapshot> {
    let mut snap = Snapshot {
        issues: load_issues(conn)?,
        rows_without_linear_id: count(conn, "linear_issues", "linear_id IS NULL")?,
        projects: load_projects(conn)?,
        teams: Vec::new(),
        users: count(conn, "linear_users", "removed_at IS NULL")?,
        active_users: count(conn, "linear_users", "removed_at IS NULL AND active = 1")?,
        labels: count(conn, "linear_labels", "removed_at IS NULL")?,
        milestones: count(conn, "linear_milestones", "removed_at IS NULL")?,
        milestones_with_target: count(
            conn,
            "linear_milestones",
            "removed_at IS NULL AND target_date IS NOT NULL",
        )?,
        ..Snapshot::default()
    };
    let (teams, key_by_id) = load_teams(conn)?;
    snap.teams = teams;
    snap.cycles = load_cycles(conn, &key_by_id)?;
    Ok(snap)
}

fn count(conn: &Connection, table: &str, filter: &str) -> Result<usize> {
    let n: i64 = conn
        .query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE {filter}"),
            [],
            |r| r.get(0),
        )
        .map_err(db)?;
    Ok(usize::try_from(n).unwrap_or(0))
}

type RawIssue = (
    String,
    String,
    String,
    Option<String>,
    i64,
    [Option<String>; 4],
    Option<f64>,
    Option<String>,
    Option<String>,
    [Option<String>; 4],
);

fn load_issues(conn: &Connection) -> Result<Vec<IssueRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT identifier, team_key, title, state_type, archived, \
                    created_at, started_at, completed_at, canceled_at, \
                    estimate, label_ids, assignee_id, \
                    project_id, due_date, cycle_id, parent_id \
               FROM linear_issues WHERE linear_id IS NOT NULL ORDER BY identifier",
        )
        .map_err(db)?;
    let raw: Vec<RawIssue> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                [r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?],
                r.get(9)?,
                r.get(10)?,
                r.get(11)?,
                [r.get(12)?, r.get(13)?, r.get(14)?, r.get(15)?],
            ))
        })
        .map_err(db)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db)?;
    raw.into_iter().map(issue_row).collect()
}

fn issue_row(raw: RawIssue) -> Result<IssueRow> {
    let (identifier, team, title, state_type, archived, stamps, estimate, labels, assignee, refs) =
        raw;
    let [created, started, completed, canceled] = stamps;
    let id = identifier.as_str();
    let has_labels = match labels {
        None => false,
        Some(s) => !serde_json::from_str::<Vec<String>>(&s)
            .map_err(|e| bad("label_ids of issue", id, &s, e))?
            .is_empty(),
    };
    let [project, due, cycle, parent] = refs;
    Ok(IssueRow {
        created_at: required(
            ts(created, "created_at of issue", id)?,
            "created_at of issue",
            id,
        )?,
        started_at: ts(started, "started_at of issue", id)?,
        completed_at: ts(completed, "completed_at of issue", id)?,
        canceled_at: ts(canceled, "canceled_at of issue", id)?,
        team,
        title,
        state_type,
        archived: archived != 0,
        has_estimate: estimate.is_some(),
        has_labels,
        assignee_id: assignee,
        has_project: project.is_some(),
        has_due_date: due.is_some(),
        has_cycle: cycle.is_some(),
        has_parent: parent.is_some(),
    })
}

fn load_projects(conn: &Connection) -> Result<Vec<ProjectRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, state, target_date, completed_at, lead_id FROM linear_projects \
              WHERE removed_at IS NULL ORDER BY id",
        )
        .map_err(db)?;
    type Raw = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let raw: Vec<Raw> = stmt
        .query_map([], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .map_err(db)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db)?;
    raw.into_iter()
        .map(|(id, state, target, completed, lead)| {
            let target_date = target
                .map(|s| {
                    NaiveDate::parse_from_str(&s, "%Y-%m-%d")
                        .map_err(|e| bad("target_date of project", &id, &s, e))
                })
                .transpose()?;
            Ok(ProjectRow {
                state: state.unwrap_or_default(),
                target_date,
                completed_at: ts(completed, "completed_at of project", &id)?,
                has_lead: lead.is_some(),
            })
        })
        .collect()
}

/// Current teams, and every team's key by id (removed teams included, so an
/// older cycle still resolves its team).
fn load_teams(conn: &Connection) -> Result<(Vec<TeamRow>, HashMap<String, String>)> {
    let mut stmt = conn
        .prepare(
            "SELECT id, key, issue_estimation_type, removed_at IS NULL FROM linear_teams \
              ORDER BY key",
        )
        .map_err(db)?;
    type Raw = (String, Option<String>, Option<String>, bool);
    let raw: Vec<Raw> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .map_err(db)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db)?;
    let mut teams = Vec::new();
    let mut key_by_id = HashMap::new();
    for (id, key, estimation_type, current) in raw {
        let key = key.unwrap_or_else(|| id.clone());
        if current {
            teams.push(TeamRow {
                key: key.clone(),
                estimation_type,
            });
        }
        key_by_id.insert(id, key);
    }
    Ok((teams, key_by_id))
}

fn load_cycles(conn: &Connection, key_by_id: &HashMap<String, String>) -> Result<Vec<CycleRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, team_id, starts_at, ends_at, completed_at, scope_count, \
                    completed_count \
               FROM linear_cycles WHERE removed_at IS NULL ORDER BY id",
        )
        .map_err(db)?;
    type Raw = (
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<i64>,
        Option<i64>,
    );
    let raw: Vec<Raw> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })
        .map_err(db)?
        .collect::<std::result::Result<_, _>>()
        .map_err(db)?;
    raw.into_iter()
        .map(|(id, team_id, starts, ends, completed, scope, done)| {
            // A cycle whose team row is missing keeps its team id as the key,
            // so it shows up under a visible name instead of disappearing.
            let team = team_id
                .map(|t| key_by_id.get(&t).cloned().unwrap_or(t))
                .unwrap_or_default();
            Ok(CycleRow {
                team,
                starts_at: required(
                    ts(starts, "starts_at of cycle", &id)?,
                    "starts_at of cycle",
                    &id,
                )?,
                ends_at: required(ts(ends, "ends_at of cycle", &id)?, "ends_at of cycle", &id)?,
                completed: ts(completed, "completed_at of cycle", &id)?.is_some(),
                scope,
                done,
            })
        })
        .collect()
}

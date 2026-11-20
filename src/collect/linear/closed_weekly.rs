//! `fact_linear_closed_weekly`: closed Linear issues per developer per ISO
//! week (#272, migration v42).
//!
//! Why: cto-reports reads FLYR ticket throughput per engineer per week.
//! What: [`closed_issues`] reads the closed issues with their assignee
//! matched to a tga developer ([`super::developer`]); [`rebuild_closed_weekly`]
//! buckets them by the ISO week of `completed_at` in UTC and replaces the
//! table in one transaction. Rules (also in the migration header):
//!
//! - closed = `state_type = 'completed'` and `completed_at` not NULL;
//!   canceled issues are excluded; archived completed issues count;
//! - the assignee is the issue's stored assignee, its email and name filled
//!   from `linear_users` when the issue row lacks them;
//! - an issue with no assignee is skipped and counted;
//! - an unmatched assignee keeps a row under its Linear name with a NULL
//!   `canonical_developer_id`.
//!
//! Refresh: `tga linear sync` calls [`rebuild_closed_weekly`] at the end of
//! every non-dry run, `sync_on_collect` included.
//! Test: `closed_weekly_tests`.

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rusqlite::{params, Connection};

use super::developer::{DeveloperIndex, DeveloperMatch};
use crate::core::errors::{Result, TgaError};

/// One closed issue with its assignee resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClosedIssue {
    /// Issue identifier (`PLA-12`).
    pub identifier: String,
    /// Team key.
    pub team_key: String,
    /// `completed_at`, UTC.
    pub completed_at: DateTime<Utc>,
    /// Assignee email (issue row, else `linear_users`).
    pub assignee_email: Option<String>,
    /// Assignee name (issue row, else `linear_users`).
    pub assignee_name: Option<String>,
    /// The matched developer, `None` when unmatched.
    pub developer: Option<DeveloperMatch>,
}

impl ClosedIssue {
    /// The name the issue is credited under: the developer's canonical name,
    /// else the Linear name, else the Linear email.
    pub fn credited_name(&self) -> String {
        self.developer
            .as_ref()
            .map(|d| d.name.clone())
            .or_else(|| self.assignee_name.clone())
            .or_else(|| self.assignee_email.clone())
            .unwrap_or_default()
    }
}

/// What [`closed_issues`] read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClosedIssues {
    /// Closed issues with an assignee.
    pub issues: Vec<ClosedIssue>,
    /// Closed issues with no assignee; not credited.
    pub unassigned: usize,
}

/// What [`rebuild_closed_weekly`] wrote.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ClosedWeeklyOutcome {
    /// Rows written.
    pub rows: usize,
    /// Closed issues credited.
    pub credited: usize,
    /// Closed issues with no assignee, not credited.
    pub unassigned: usize,
    /// Closed issues whose assignee matched no tga developer.
    pub unmatched_issues: usize,
}

/// `YYYY-Www` and the Monday of the ISO week of `at` (UTC).
///
/// The ISO year can differ from the calendar year: 2026-01-01 is in
/// `2026-W01`, 2027-01-01 in `2026-W53`.
pub fn iso_week(at: DateTime<Utc>) -> (String, NaiveDate) {
    let week = at.iso_week();
    let label = format!("{}-W{:02}", week.year(), week.week());
    let date = at.date_naive();
    let monday = date - chrono::Days::new(u64::from(date.weekday().num_days_from_monday()));
    (label, monday)
}

/// Read the closed issues, assignees matched through `index`, optionally
/// bounded to `completed_at` in `[since, until)`.
///
/// # Errors
///
/// [`TgaError::DbError`] on a failed read; [`TgaError::ValidationError`]
/// when a `completed_at` does not parse as RFC 3339 (the error names the
/// issue).
pub fn closed_issues(
    conn: &Connection,
    index: &DeveloperIndex,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<ClosedIssues> {
    // #272: canceled issues carry `state_type = 'canceled'` and fall out here.
    let mut stmt = conn.prepare(
        "SELECT li.identifier, li.team_key, li.completed_at, \
                COALESCE(li.assignee_email, lu.email), \
                COALESCE(li.assignee_name, lu.name, lu.display_name, li.assignee), \
                li.assignee_id \
         FROM linear_issues li LEFT JOIN linear_users lu ON lu.id = li.assignee_id \
         WHERE li.state_type = 'completed' AND li.completed_at IS NOT NULL \
         ORDER BY li.identifier",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
        ))
    })?;
    // #272 (D1f/D09): one Linear user resolves to one identity, whichever of
    // their issues carries the email, so one person never gets two rows.
    let users = assignee_identities(conn)?;
    let mut matched: HashMap<String, Option<DeveloperMatch>> = HashMap::new();
    let mut out = ClosedIssues::default();
    for row in rows {
        let (identifier, team_key, completed, email, name, assignee_id) = row?;
        let (email, name) = match assignee_id.as_ref().and_then(|id| users.get(id)) {
            Some((e, n)) => (e.clone(), n.clone()),
            None => (email, name),
        };
        let completed_at = DateTime::parse_from_rfc3339(&completed)
            .map_err(|e| {
                TgaError::ValidationError(format!(
                    "linear_issues.completed_at for {identifier} is not RFC 3339 ({completed}): {e}"
                ))
            })?
            .with_timezone(&Utc);
        if since.is_some_and(|s| completed_at < s) || until.is_some_and(|u| completed_at >= u) {
            continue;
        }
        let email = email.filter(|e| !e.trim().is_empty());
        let name = name.filter(|n| !n.trim().is_empty());
        if email.is_none() && name.is_none() && assignee_id.is_none() {
            out.unassigned += 1;
            continue;
        }
        let developer = match &assignee_id {
            Some(id) => matched
                .entry(id.clone())
                .or_insert_with(|| index.match_identity(email.as_deref(), name.as_deref()))
                .clone(),
            None => index.match_identity(email.as_deref(), name.as_deref()),
        };
        out.issues.push(ClosedIssue {
            identifier,
            team_key,
            completed_at,
            assignee_email: email,
            assignee_name: name.or(assignee_id),
            developer,
        });
    }
    Ok(out)
}

/// `(email, name)` of one Linear assignee.
pub type AssigneeIdentity = (Option<String>, Option<String>);

/// One identity per Linear `assignee_id`: the first non-empty email and name
/// across all of that user's issues (in row order), each filled from
/// `linear_users` when no issue carries it (#272).
///
/// # Errors
///
/// [`TgaError::DbError`] on a failed read.
pub fn assignee_identities(conn: &Connection) -> Result<HashMap<String, AssigneeIdentity>> {
    fn first(slot: &mut Option<String>, value: Option<String>) {
        if slot.is_none() {
            *slot = value.filter(|v| !v.trim().is_empty());
        }
    }
    let mut out: HashMap<String, AssigneeIdentity> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT assignee_id, assignee_email, COALESCE(assignee_name, assignee) \
         FROM linear_issues WHERE assignee_id IS NOT NULL ORDER BY id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (id, email, name) = row?;
        let slot = out.entry(id).or_default();
        first(&mut slot.0, email);
        first(&mut slot.1, name);
    }
    let mut users =
        conn.prepare("SELECT id, email, COALESCE(name, display_name) FROM linear_users")?;
    let rows = users.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        let (id, email, name) = row?;
        if let Some(slot) = out.get_mut(&id) {
            first(&mut slot.0, email);
            first(&mut slot.1, name);
        }
    }
    Ok(out)
}

/// Rebuild `fact_linear_closed_weekly` from every closed issue.
///
/// Why/What: see the module doc. One transaction deletes every row and
/// inserts the current set, so a re-run writes the same rows and a reopened
/// or reassigned issue leaves no stale credit.
/// Test: `closed_weekly_tests::rebuild_is_idempotent`,
/// `closed_weekly_tests::canceled_issues_are_excluded`,
/// `closed_weekly_tests::weeks_cross_the_year_boundary`.
///
/// # Errors
///
/// See [`closed_issues`]; [`TgaError::DbError`] on a failed write.
pub fn rebuild_closed_weekly(
    conn: &mut Connection,
    index: &DeveloperIndex,
) -> Result<ClosedWeeklyOutcome> {
    let read = closed_issues(conn, index, None, None)?;
    // (developer id, credited name, team, week) → (monday, keys).
    type Key = (Option<i64>, String, String, String);
    let mut buckets: BTreeMap<Key, (NaiveDate, Vec<String>)> = BTreeMap::new();
    let mut outcome = ClosedWeeklyOutcome {
        unassigned: read.unassigned,
        ..Default::default()
    };
    for issue in &read.issues {
        let (week, monday) = iso_week(issue.completed_at);
        if issue.developer.is_none() {
            outcome.unmatched_issues += 1;
        }
        let key = (
            issue.developer.as_ref().map(|d| d.id),
            issue.credited_name(),
            issue.team_key.clone(),
            week,
        );
        buckets
            .entry(key)
            .or_insert_with(|| (monday, Vec::new()))
            .1
            .push(issue.identifier.clone());
        outcome.credited += 1;
    }

    let tx = conn.transaction()?;
    tx.execute("DELETE FROM fact_linear_closed_weekly", [])?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO fact_linear_closed_weekly \
             (canonical_developer_id, developer_name, team_key, iso_week, week_start, \
              closed_count, closed_issue_keys) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for ((id, name, team, week), (monday, mut keys)) in buckets {
            keys.sort();
            insert.execute(params![
                id,
                name,
                team,
                week,
                monday.format("%Y-%m-%d").to_string(),
                keys.len() as i64,
                serde_json::to_string(&keys)?,
            ])?;
            outcome.rows += 1;
        }
    }
    tx.commit()?;
    Ok(outcome)
}

#[cfg(test)]
#[path = "closed_weekly_tests.rs"]
pub(crate) mod tests;

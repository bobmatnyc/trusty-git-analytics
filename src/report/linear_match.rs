//! The data behind `tga linear match-report` (#272).
//!
//! Why: before cto-reports trusts per-engineer Linear figures, an operator
//! needs to see how much of the work links to a ticket and whose identities
//! did not match.
//! What: [`compute_match_report`] reads stored rows only (no network) and
//! returns a [`MatchReport`]:
//!
//! - PR coverage: pull requests in Linear-bound repos (`linear.repos`) with
//!   at least one `pr_work_items` link against those with none, plus the
//!   PRs linked per source;
//! - per engineer: PRs, PRs with a ticket, and closed Linear issues in the
//!   window (the rules of [`crate::collect::linear::closed_weekly`]);
//! - unmatched identities: Linear assignees with no tga developer, and tga
//!   authors with commits in bound repos that match no Linear user.
//!
//! A PR is credited to the developer its author login matches
//! ([`DeveloperIndex::match_login`]); failing that, to the author of most of
//! its stored commits; failing that, to the raw login, unmatched.
//! The report carries names only, never an email or a key.
//! Test: `linear_match_tests`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use rusqlite::Connection;
use serde::Serialize;

use crate::collect::linear::closed_weekly::{assignee_identities, closed_issues, ClosedIssue};
use crate::collect::linear::developer::DeveloperIndex;
use crate::collect::pm_adapter::scope::bound_source;
use crate::core::config::{split_github_slug, Config};
use crate::core::errors::{Result, TgaError};

/// PR coverage in Linear-bound repos.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct PrCoverage {
    /// PRs in bound repos in the window.
    pub total: usize,
    /// PRs with at least one `pr_work_items` link.
    pub with_ticket: usize,
    /// PRs with none.
    pub without_ticket: usize,
    /// PRs with a link from each source (a PR can count under several).
    pub by_source: BTreeMap<String, usize>,
}

/// One engineer's counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineerRow {
    /// Canonical name, or the raw Linear name / PR login when unmatched.
    pub name: String,
    /// Whether the row is a matched tga developer.
    pub matched: bool,
    /// PRs in bound repos in the window.
    pub prs: usize,
    /// Of those, PRs with a ticket link.
    pub prs_with_ticket: usize,
    /// Closed Linear issues in the window.
    pub closed_issues: usize,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MatchReport {
    /// Window start (inclusive), RFC 3339, if bounded.
    pub since: Option<String>,
    /// Window end (exclusive), RFC 3339, if bounded.
    pub until: Option<String>,
    /// PR coverage in bound repos.
    pub prs: PrCoverage,
    /// Per-engineer counts, matched first, then by name.
    pub engineers: Vec<EngineerRow>,
    /// Closed issues in the window with no assignee.
    pub unassigned_closed: usize,
    /// Linear assignee names with no tga developer, sorted.
    pub unmatched_linear_assignees: Vec<String>,
    /// tga author names with commits in bound repos and no Linear user, sorted.
    pub unmatched_tga_authors: Vec<String>,
}

/// Whether a stored `pull_requests.repository` (`owner/name`) is bound to
/// Linear: its owner or full slug is a `linear.repos` entry, or its name
/// half is bound as a repository label.
pub fn pr_repository_bound(config: &Config, repository: &str) -> bool {
    let entries = config
        .linear
        .as_ref()
        .map(|l| l.repos.as_slice())
        .unwrap_or(&[]);
    if entries.is_empty() {
        return false;
    }
    if entries.iter().any(|e| e.eq_ignore_ascii_case(repository)) {
        return true;
    }
    match split_github_slug(repository) {
        Some((owner, name)) => {
            entries.iter().any(|e| e.eq_ignore_ascii_case(owner))
                || bound_source(config, name).is_some()
        }
        None => bound_source(config, repository).is_some(),
    }
}

fn parse_time(what: &str, raw: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| TgaError::ValidationError(format!("{what} is not RFC 3339 ({raw}): {e}")))
}

fn in_window(
    at: DateTime<Utc>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> bool {
    since.is_none_or(|s| at >= s) && until.is_none_or(|u| at < u)
}

/// Key of an engineer row: `Some(author id)` or the unmatched raw name.
type EngineerKey = (Option<i64>, String);

/// Build the report from stored rows.
///
/// # Errors
///
/// [`TgaError::DbError`] on a failed read; [`TgaError::ValidationError`] when
/// a stored timestamp does not parse (the error names the row).
pub fn compute_match_report(
    conn: &Connection,
    config: &Config,
    index: &DeveloperIndex,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<MatchReport> {
    let mut rows: BTreeMap<EngineerKey, EngineerRow> = BTreeMap::new();
    let prs = pr_coverage(conn, config, index, since, until, &mut rows)?;

    let closed = closed_issues(conn, index, since, until)?;
    for issue in &closed.issues {
        let key = (issue.developer.as_ref().map(|d| d.id), report_name(issue));
        engineer(&mut rows, key).closed_issues += 1;
    }

    let (linear_ids, unmatched_linear_assignees) = linear_identities(conn, index)?;
    let unmatched_tga_authors = unmatched_authors(conn, config, &linear_ids, since, until)?;

    let mut engineers: Vec<EngineerRow> = rows.into_values().collect();
    engineers.sort_by(|a, b| b.matched.cmp(&a.matched).then_with(|| a.name.cmp(&b.name)));
    Ok(MatchReport {
        since: since.map(|s| s.to_rfc3339()),
        until: until.map(|u| u.to_rfc3339()),
        prs,
        engineers,
        unassigned_closed: closed.unassigned,
        unmatched_linear_assignees,
        unmatched_tga_authors,
    })
}

/// The label an assignee with neither a tga identity nor a display name
/// renders under.
const UNNAMED_ASSIGNEE: &str = "unnamed assignee";

/// The name a closed issue is credited under in the match report: the
/// resolved tga developer's name, else the Linear display name, else
/// [`UNNAMED_ASSIGNEE`].
// #280: never fall back to the assignee email, as `credited_name` does.
fn report_name(issue: &ClosedIssue) -> String {
    issue
        .developer
        .as_ref()
        .map(|d| d.name.clone())
        .or_else(|| issue.assignee_name.clone())
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| UNNAMED_ASSIGNEE.to_string())
}

fn engineer(rows: &mut BTreeMap<EngineerKey, EngineerRow>, key: EngineerKey) -> &mut EngineerRow {
    let name = key.1.clone();
    let matched = key.0.is_some();
    rows.entry(key).or_insert(EngineerRow {
        name,
        matched,
        prs: 0,
        prs_with_ticket: 0,
        closed_issues: 0,
    })
}

/// Commit SHA → `authors` (id, name), for crediting a PR by its commits.
fn commit_authors(conn: &Connection) -> Result<HashMap<String, (i64, String)>> {
    let mut stmt = conn.prepare(
        "SELECT c.sha, a.id, a.canonical_name FROM commits c \
         JOIN authors a ON a.id = c.author_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(TgaError::from)
}

/// The PR coverage block; adds each PR to its engineer's row.
fn pr_coverage(
    conn: &Connection,
    config: &Config,
    index: &DeveloperIndex,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
    rows: &mut BTreeMap<EngineerKey, EngineerRow>,
) -> Result<PrCoverage> {
    // (provider, repository, number) → sources that link it.
    let mut links: HashMap<(String, String, i64), BTreeSet<String>> = HashMap::new();
    {
        let mut stmt =
            conn.prepare("SELECT provider, repository, pr_number, source FROM pr_work_items")?;
        let found = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        for f in found {
            let (provider, repository, number, source) = f?;
            links
                .entry((provider, repository, number))
                .or_default()
                .insert(source);
        }
    }
    let by_sha = commit_authors(conn)?;

    let mut stmt = conn.prepare(
        "SELECT provider, repository, pr_number, author, created_at, commit_shas \
         FROM pull_requests ORDER BY provider, repository, pr_number",
    )?;
    let prs = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut cov = PrCoverage::default();
    for pr in prs {
        let (provider, repository, number, author, created, shas) = pr?;
        if !pr_repository_bound(config, &repository) {
            continue;
        }
        let created_at = parse_time(
            &format!("pull_requests.created_at for {repository}#{number}"),
            &created,
        )?;
        if !in_window(created_at, since, until) {
            continue;
        }
        cov.total += 1;
        let sources = links.get(&(provider, repository, number));
        let linked = sources.is_some_and(|s| !s.is_empty());
        if linked {
            cov.with_ticket += 1;
        } else {
            cov.without_ticket += 1;
        }
        for s in sources.into_iter().flatten() {
            *cov.by_source.entry(s.clone()).or_default() += 1;
        }

        let key = match index.match_login(&author) {
            Some(m) => (Some(m.id), m.name),
            None => match majority_author(&shas, &by_sha) {
                Some((id, name)) => (Some(id), name),
                None => (None, author.clone()),
            },
        };
        let row = engineer(rows, key);
        row.prs += 1;
        if linked {
            row.prs_with_ticket += 1;
        }
    }
    Ok(cov)
}

/// The author of most of a PR's stored commits; ties go to the lower id.
/// A malformed `commit_shas` names no commit (same rule as `pr_links`).
fn majority_author(
    commit_shas: &str,
    by_sha: &HashMap<String, (i64, String)>,
) -> Option<(i64, String)> {
    let shas: Vec<String> = serde_json::from_str(commit_shas).ok()?;
    let mut counts: BTreeMap<i64, (usize, String)> = BTreeMap::new();
    for sha in shas {
        if let Some((id, name)) = by_sha.get(&sha) {
            counts.entry(*id).or_insert((0, name.clone())).0 += 1;
        }
    }
    counts
        .into_iter()
        .max_by(|a, b| a.1 .0.cmp(&b.1 .0).then_with(|| b.0.cmp(&a.0)))
        .map(|(id, (_, name))| (id, name))
}

/// Every Linear identity (issue assignees and `linear_users`): the developer
/// ids they match, and the names of assignees that match none.
fn linear_identities(
    conn: &Connection,
    index: &DeveloperIndex,
) -> Result<(HashSet<i64>, Vec<String>)> {
    let mut matched = HashSet::new();
    let mut unmatched = BTreeSet::new();
    // #272 (D1f/D09): an assignee with a Linear user id resolves once, the
    // same way the weekly table does; rows without one use their own fields.
    let mut identities: Vec<(Option<String>, Option<String>, i64)> = assignee_identities(conn)?
        .into_values()
        .map(|(e, n)| (e, n, 1))
        .collect();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT assignee_email, COALESCE(assignee_name, assignee), 1 \
         FROM linear_issues WHERE assignee_id IS NULL \
            AND (assignee_email IS NOT NULL OR assignee_name IS NOT NULL \
                 OR assignee IS NOT NULL) \
         UNION \
         SELECT DISTINCT email, COALESCE(name, display_name), 0 FROM linear_users",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, Option<String>>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;
    for row in rows {
        identities.push(row?);
    }
    for (email, name, is_assignee) in identities {
        match index.match_identity(email.as_deref(), name.as_deref()) {
            Some(m) => {
                matched.insert(m.id);
            }
            // Names only: an unmatched assignee with no name is reported as
            // such, never by its email.
            None if is_assignee == 1 => {
                unmatched.insert(
                    name.filter(|n| !n.trim().is_empty())
                        .unwrap_or_else(|| UNNAMED_ASSIGNEE.to_string()),
                );
            }
            None => {}
        }
    }
    Ok((matched, unmatched.into_iter().collect()))
}

/// tga authors with commits in bound repos (in the window) that match no
/// Linear identity.
fn unmatched_authors(
    conn: &Connection,
    config: &Config,
    linear_ids: &HashSet<i64>,
    since: Option<DateTime<Utc>>,
    until: Option<DateTime<Utc>>,
) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT c.repository, c.timestamp, a.id, a.canonical_name FROM commits c \
         JOIN authors a ON a.id = c.author_id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, i64>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut out = BTreeSet::new();
    for row in rows {
        let (repository, ts, id, name) = row?;
        if linear_ids.contains(&id) || bound_source(config, &repository).is_none() {
            continue;
        }
        let at = parse_time(&format!("commits.timestamp in {repository}"), &ts)?;
        if in_window(at, since, until) {
            out.insert(name);
        }
    }
    Ok(out.into_iter().collect())
}

/// The plain-text form `tga linear match-report` prints.
pub fn render_text(r: &MatchReport) -> String {
    let mut out = String::new();
    let window = format!(
        "{} .. {}",
        r.since.as_deref().unwrap_or("start"),
        r.until.as_deref().unwrap_or("now")
    );
    out.push_str(&format!("Linear match report ({window})\n\n"));
    out.push_str(&format!(
        "PRs in Linear-bound repos: {} total, {} with a ticket, {} without\n",
        r.prs.total, r.prs.with_ticket, r.prs.without_ticket
    ));
    let sources: Vec<String> = ["attachment", "branch", "title", "body", "commit"]
        .iter()
        .map(|s| format!("{s} {}", r.prs.by_source.get(*s).copied().unwrap_or(0)))
        .collect();
    out.push_str(&format!("  linked by source: {}\n\n", sources.join(", ")));
    out.push_str(&format!(
        "{:<32} {:>5} {:>12} {:>14}\n",
        "ENGINEER", "PRS", "WITH TICKET", "CLOSED ISSUES"
    ));
    for e in &r.engineers {
        let name = if e.matched {
            e.name.clone()
        } else {
            format!("{} (unmatched)", e.name)
        };
        out.push_str(&format!(
            "{name:<32} {:>5} {:>12} {:>14}\n",
            e.prs, e.prs_with_ticket, e.closed_issues
        ));
    }
    out.push_str(&format!(
        "\nClosed issues with no assignee: {}\n",
        r.unassigned_closed
    ));
    out.push_str(&format!(
        "Linear assignees with no tga developer ({}): {}\n",
        r.unmatched_linear_assignees.len(),
        list(&r.unmatched_linear_assignees)
    ));
    out.push_str(&format!(
        "tga authors in bound repos with no Linear user ({}): {}\n",
        r.unmatched_tga_authors.len(),
        list(&r.unmatched_tga_authors)
    ));
    out
}

fn list(names: &[String]) -> String {
    if names.is_empty() {
        "none".to_string()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
#[path = "linear_match_tests.rs"]
pub(crate) mod tests;

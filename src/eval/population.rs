//! Read-only queries behind `tga eval sample`.

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::Connection;

use super::Result;

// #111: the path / PR-title / issue-type joins are shared with `llm.context`.
pub(crate) use crate::core::db::commit_context::{load_issue_types, load_paths, load_pr_titles};

/// A commit in the database with its stored verdict, if any.
#[derive(Debug, Clone)]
pub(crate) struct CommitRow {
    pub id: i64,
    pub sha: String,
    pub repo: String,
    pub author_email: String,
    pub timestamp: String,
    pub ts: Option<DateTime<Utc>>,
    pub message: String,
    pub is_merge: bool,
    pub files: i64,
    pub insertions: i64,
    pub deletions: i64,
    pub ticket_id: Option<String>,
    /// Stored `(category, confidence, method)`.
    pub stored: Option<(String, f64, String)>,
}

/// Parse a stored commit timestamp in any format tga has written.
pub(crate) fn parse_ts(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(t) = DateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&Utc));
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f%:z", "%Y-%m-%d %H:%M:%S%.f%z"] {
        if let Ok(t) = DateTime::parse_from_str(s, fmt) {
            return Some(t.with_timezone(&Utc));
        }
    }
    for fmt in ["%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%dT%H:%M:%S%.f"] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(t.and_utc());
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .map(|t| t.and_utc())
}

/// Every commit with its stored classification.
pub(crate) fn load_commits(conn: &Connection) -> Result<Vec<CommitRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.sha, c.repository, c.author_email, c.timestamp, c.message, \
                c.is_merge, c.files_changed, c.insertions, c.deletions, \
                COALESCE(c.ticket_id, cl.ticket_id), cl.category, cl.confidence, cl.method \
         FROM commits c LEFT JOIN classifications cl ON cl.id = c.classification_id",
    )?;
    let rows = stmt.query_map([], |r| {
        let timestamp: String = r.get(4)?;
        let category: Option<String> = r.get(11)?;
        let confidence: Option<f64> = r.get(12)?;
        let method: Option<String> = r.get(13)?;
        Ok(CommitRow {
            id: r.get(0)?,
            sha: r.get(1)?,
            repo: r.get(2)?,
            author_email: r.get(3)?,
            ts: parse_ts(&timestamp),
            timestamp,
            message: r.get(5)?,
            is_merge: r.get::<_, i64>(6)? != 0,
            files: r.get(7)?,
            insertions: r.get(8)?,
            deletions: r.get(9)?,
            ticket_id: r.get(10)?,
            stored: match (category, method) {
                (Some(c), Some(m)) => Some((c, confidence.unwrap_or(0.0), m)),
                _ => None,
            },
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

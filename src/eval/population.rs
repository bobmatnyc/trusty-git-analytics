//! Read-only queries behind `tga eval sample`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use rusqlite::{params_from_iter, Connection};

use super::Result;

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

/// Changed paths per commit id, for the given ids only.
pub(crate) fn load_paths(conn: &Connection, ids: &[i64]) -> Result<HashMap<i64, Vec<String>>> {
    let mut out: HashMap<i64, Vec<String>> = HashMap::new();
    for chunk in ids.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT commit_id, path FROM files WHERE commit_id IN ({placeholders}) \
             ORDER BY commit_id, path"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (id, path) = row?;
            out.entry(id).or_default().push(path);
        }
    }
    Ok(out)
}

/// Title of the lowest-numbered pull request containing each wanted SHA.
pub(crate) fn load_pr_titles(
    conn: &Connection,
    wanted: &HashSet<&str>,
) -> Result<HashMap<String, String>> {
    let mut stmt =
        conn.prepare("SELECT title, commit_shas FROM pull_requests ORDER BY pr_number, id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (title, shas) = row?;
        // A malformed list is skipped rather than failing the sample.
        let Ok(list) = serde_json::from_str::<Vec<String>>(&shas) else {
            continue;
        };
        for sha in list {
            if wanted.contains(sha.as_str()) {
                out.entry(sha).or_insert_with(|| title.clone());
            }
        }
    }
    Ok(out)
}

/// Issue type of the first linked work item per wanted SHA.
pub(crate) fn load_issue_types(
    conn: &Connection,
    wanted: &HashSet<&str>,
) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare(
        "SELECT cw.commit_sha, w.item_type FROM commit_work_items cw \
         JOIN work_items w ON w.id = cw.work_item_id AND w.source = cw.work_item_source \
         ORDER BY cw.commit_sha, w.source, w.id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (sha, item_type) = row?;
        if wanted.contains(sha.as_str()) {
            out.entry(sha).or_insert(item_type);
        }
    }
    Ok(out)
}

//! Read-only joins for the facts around a commit: changed paths, the linked
//! PR title and the linked issue type.
//!
//! Why (#111): `tga eval sample` shows these to the human rater, and
//! `llm.context` shows the same facts to the LLM tier; one set of queries
//! keeps the two from drifting.
//! What: [`load_paths`], [`load_pr_titles`] and [`load_issue_types`].
//! Test: `eval::sample` tests and `classify::llm_context_tests`.

use std::collections::{HashMap, HashSet};

use rusqlite::{params_from_iter, Connection, Result};

/// Changed paths per commit id, for the given ids only, in path order.
///
/// # Errors
///
/// A database read fails.
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
///
/// # Errors
///
/// A database read fails. A malformed `commit_shas` list is skipped.
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
///
/// # Errors
///
/// A database read fails.
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

//! Pull request ↔ work item links (#272).
//!
//! Why: a report needs "which ticket does this PR work on" per PR, not only
//! per commit. Linear attachments name the PR directly; the branch, title,
//! body and commit keys name it indirectly.
//!
//! What: [`link_pull_requests`] fills `pr_work_items` with one row per piece
//! of evidence. Sources, most direct first (the order a reader should trust
//! them in; every source is stored, none displaces another):
//!
//! 1. `attachment` — a Linear issue attachment whose URL is this GitHub PR.
//! 2. `branch` — a key in the PR's head ref.
//! 3. `title` — a key in the PR's title.
//! 4. `body` — the key extracted from the PR body at fetch time.
//! 5. `commit` — a key in a commit the PR carried.
//!
//! An attachment URL names the repository in GitHub's spelling, which can
//! differ in letter case from the stored `pull_requests.repository` (#266).
//! The URL is matched case-insensitively and the stored row's spelling is
//! written, so the link joins back to its PR. An attachment for a PR that is
//! not stored is skipped; the next pass links it once the PR is collected.
//!
//! No network, no LLM; SQL plus the existing key extractors.
//! Test: `tests` below.

use std::collections::HashMap;

use rusqlite::{params, Connection};
use tracing::debug;

use crate::collect::correlate::title_ticket_key;
use crate::collect::linear::attachment::parse_github_pr_url;
use crate::collect::ticket::{branch_ticket_key, extract_ticket_id};
use crate::core::errors::{Result, TgaError};

/// One stored pull request, as the link pass reads it.
struct PrRow {
    provider: String,
    repository: String,
    pr_number: i64,
    head_ref: String,
    title: String,
    body_key: Option<String>,
    commit_shas: String,
}

/// Read every stored pull request.
fn load_prs(conn: &Connection) -> Result<Vec<PrRow>> {
    let mut stmt = conn.prepare(
        "SELECT provider, repository, pr_number, head_ref, title, body_ticket_id, commit_shas \
         FROM pull_requests ORDER BY provider, repository, pr_number",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(PrRow {
            provider: r.get(0)?,
            repository: r.get(1)?,
            pr_number: r.get(2)?,
            head_ref: r.get(3)?,
            title: r.get(4)?,
            body_key: r.get(5)?,
            commit_shas: r.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(TgaError::from)
}

/// Every commit's ticket key, by SHA: `ticket_id`, else the message.
fn load_commit_keys(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn.prepare("SELECT sha, ticket_id, message FROM commits")?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        let (sha, ticket, message) = row?;
        let key = ticket
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .or_else(|| extract_ticket_id(&message));
        if let Some(key) = key {
            out.insert(sha, key);
        }
    }
    Ok(out)
}

/// `(work_item_id, attachment url)` for every stored Linear attachment.
fn load_attachments(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare(
        "SELECT li.identifier, a.url FROM linear_issue_attachments a \
         JOIN linear_issues li ON li.linear_id = a.issue_id ORDER BY li.identifier, a.url",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect::<std::result::Result<_, _>>()
        .map_err(TgaError::from)
}

/// Rebuild the PR-to-work-item links; return how many the table now holds.
///
/// Why/What: see the module doc for the sources and the repository match.
/// Key sources link to every `work_items` row with that id, across sources,
/// as [`crate::collect::correlate_commits`] does; a key with no work item is
/// not linked. Attachment links use `work_item_source = 'linear'`. One
/// transaction deletes every `pr_work_items` row and inserts the current set
/// (`INSERT OR IGNORE` drops duplicate evidence), so a re-run yields the same
/// rows and evidence that no longer holds is removed (#272).
/// Test: `attachment_alone_links_a_pr`, `title_alone_links_a_pr`,
/// `attachment_repository_matches_case_insensitively`,
/// `relinking_is_idempotent`, `removed_attachment_drops_its_link`,
/// `changed_title_drops_the_old_title_link`.
///
/// # Errors
///
/// [`TgaError::DbError`] when a query, an insert or the commit fails.
pub fn link_pull_requests(conn: &mut Connection) -> Result<u64> {
    let prs = load_prs(conn)?;
    let commit_keys = load_commit_keys(conn)?;
    let attachments = load_attachments(conn)?;

    // #272/#266: GitHub PRs by lower-cased `owner/name` and number, keeping
    // the stored spelling.
    let mut github: HashMap<(String, i64), &PrRow> = HashMap::new();
    for pr in prs.iter().filter(|p| p.provider == "github") {
        github
            .entry((pr.repository.to_ascii_lowercase(), pr.pr_number))
            .or_insert(pr);
    }

    let tx = conn.transaction()?;
    // #272: every row is derived from the current attachments, PRs and
    // commits, so the pass rebuilds the table. A removed attachment or a
    // changed title, branch or commit key leaves no stale link behind.
    tx.execute("DELETE FROM pr_work_items", [])?;
    let mut written = 0u64;
    {
        let mut lookup =
            tx.prepare("SELECT source FROM work_items WHERE id = ?1 ORDER BY source")?;
        let mut insert = tx.prepare(
            "INSERT OR IGNORE INTO pr_work_items \
             (provider, repository, pr_number, work_item_id, work_item_source, source) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;

        for (identifier, url) in &attachments {
            // #272: a URL that is not a GitHub PR is skipped with a debug log.
            // This filters attachment data (Slack, Figma, a malformed link);
            // it is not an error path.
            let Some(pr_ref) = parse_github_pr_url(url) else {
                debug!(%identifier, "Linear attachment is not a GitHub PR URL; skipped");
                continue;
            };
            let key = (pr_ref.repository.to_ascii_lowercase(), pr_ref.pr_number);
            let Some(pr) = github.get(&key) else {
                debug!(%identifier, repository = %pr_ref.repository, pr = pr_ref.pr_number,
                    "Linear attachment names a PR that is not collected; skipped");
                continue;
            };
            written += insert.execute(params![
                pr.provider,
                pr.repository,
                pr.pr_number,
                identifier,
                "linear",
                "attachment"
            ])? as u64;
        }

        for pr in &prs {
            let mut keys: Vec<(String, &str)> = Vec::new();
            if let Some(k) = branch_ticket_key(&pr.head_ref) {
                keys.push((k, "branch"));
            }
            if let Some(k) = title_ticket_key(&pr.title) {
                keys.push((k, "title"));
            }
            if let Some(k) = pr.body_key.clone() {
                keys.push((k, "body"));
            }
            // A provider-written JSON column; a malformed one contributes no
            // commit keys (same rule as the commit correlation pass).
            if let Ok(shas) = serde_json::from_str::<Vec<String>>(&pr.commit_shas) {
                for sha in shas {
                    if let Some(k) = commit_keys.get(&sha) {
                        keys.push((k.clone(), "commit"));
                    }
                }
            }
            for (key, source) in keys {
                let item_sources: Vec<String> = lookup
                    .query_map(params![key], |r| r.get(0))?
                    .collect::<std::result::Result<_, _>>()?;
                for item_source in item_sources {
                    written += insert.execute(params![
                        pr.provider,
                        pr.repository,
                        pr.pr_number,
                        key,
                        item_source,
                        source
                    ])? as u64;
                }
            }
        }
    }
    tx.commit()?;
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::work_items::{upsert_work_item, WorkItemRow};
    use crate::core::db::Database;

    fn insert_pr(conn: &Connection, repository: &str, n: i64, title: &str, head_ref: &str) {
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, \
              commit_shas, head_ref) \
             VALUES ('github', ?1, ?2, ?3, 'ada', 'merged', '2026-01-01T00:00:00Z', '[]', ?4)",
            params![repository, n, title, head_ref],
        )
        .expect("insert pr");
    }

    fn insert_linear_issue(conn: &Connection, linear_id: &str, identifier: &str, url: &str) {
        conn.execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
                                        linear_id) \
             VALUES (?1, 't', 'Done', 'Pace', 'PLA', '2026-01-01T00:00:00Z', ?2)",
            params![identifier, linear_id],
        )
        .expect("insert issue");
        conn.execute(
            "INSERT INTO linear_issue_attachments (issue_id, url, source_type, title) \
             VALUES (?1, ?2, 'github', 'PR')",
            params![linear_id, url],
        )
        .expect("insert attachment");
    }

    fn links(db: &Database) -> Vec<(String, i64, String, String)> {
        let conn = db.connection();
        let mut stmt = conn
            .prepare(
                "SELECT repository, pr_number, work_item_id, source FROM pr_work_items \
                 ORDER BY repository, pr_number, work_item_id, source",
            )
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("rows")
    }

    /// #272: a PR whose branch, title, body and commits carry no key is
    /// linked by the Linear attachment alone; a non-PR attachment is skipped.
    #[test]
    fn attachment_alone_links_a_pr() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(
            db.connection(),
            "pacerevenue/rates",
            7,
            "fix rounding",
            "fix-rounding",
        );
        insert_linear_issue(
            db.connection(),
            "uuid-1",
            "PLA-1",
            "https://github.com/pacerevenue/rates/pull/7",
        );
        insert_linear_issue(
            db.connection(),
            "uuid-2",
            "PLA-2",
            "https://figma.com/file/x",
        );

        let n = link_pull_requests(db.connection_mut()).expect("link");
        assert_eq!(n, 1);
        assert_eq!(
            links(&db),
            vec![(
                "pacerevenue/rates".into(),
                7,
                "PLA-1".into(),
                "attachment".into()
            )]
        );
    }

    /// #272: a PR linked by its title alone.
    #[test]
    fn title_alone_links_a_pr() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(
            db.connection(),
            "pacerevenue/rates",
            8,
            "PLA-3: fix rounding",
            "x",
        );
        let item = WorkItemRow {
            id: "PLA-3".into(),
            source: "linear".into(),
            title: "t".into(),
            status: "Done".into(),
            item_type: "Issue".into(),
            tags: None,
            project: None,
            url: None,
            raw_json: None,
        };
        upsert_work_item(db.connection(), &item).expect("work item");

        link_pull_requests(db.connection_mut()).expect("link");
        assert_eq!(
            links(&db),
            vec![(
                "pacerevenue/rates".into(),
                8,
                "PLA-3".into(),
                "title".into()
            )]
        );
    }

    /// #272/#266: the attachment URL's spelling differs in case from the
    /// stored row; the link carries the stored spelling.
    #[test]
    fn attachment_repository_matches_case_insensitively() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(db.connection(), "PaceRevenue/Rates", 9, "misc", "misc");
        insert_linear_issue(
            db.connection(),
            "uuid-1",
            "PLA-1",
            "https://github.com/pacerevenue/rates/pull/9",
        );

        link_pull_requests(db.connection_mut()).expect("link");
        assert_eq!(
            links(&db),
            vec![(
                "PaceRevenue/Rates".into(),
                9,
                "PLA-1".into(),
                "attachment".into()
            )]
        );
    }

    #[test]
    fn relinking_is_idempotent() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(db.connection(), "pacerevenue/rates", 7, "misc", "misc");
        insert_linear_issue(
            db.connection(),
            "uuid-1",
            "PLA-1",
            "https://github.com/pacerevenue/rates/pull/7",
        );
        assert_eq!(link_pull_requests(db.connection_mut()).expect("first"), 1);
        assert_eq!(link_pull_requests(db.connection_mut()).expect("second"), 1);
        assert_eq!(links(&db).len(), 1);
    }

    /// #272: an attachment removed in Linear drops its link on the next pass.
    #[test]
    fn removed_attachment_drops_its_link() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(db.connection(), "pacerevenue/rates", 7, "misc", "misc");
        insert_linear_issue(
            db.connection(),
            "uuid-1",
            "PLA-1",
            "https://github.com/pacerevenue/rates/pull/7",
        );
        assert_eq!(link_pull_requests(db.connection_mut()).expect("first"), 1);

        db.connection()
            .execute("DELETE FROM linear_issue_attachments", [])
            .expect("remove attachment");
        assert_eq!(link_pull_requests(db.connection_mut()).expect("second"), 0);
        assert!(links(&db).is_empty(), "stale link: {:?}", links(&db));
    }

    /// #272: a PR retitled to another key drops the old title link.
    #[test]
    fn changed_title_drops_the_old_title_link() {
        let mut db = Database::open_in_memory().expect("db");
        insert_pr(db.connection(), "pacerevenue/rates", 8, "PLA-3: fix", "x");
        for id in ["PLA-3", "PLA-4"] {
            upsert_work_item(db.connection(), &work_item(id)).expect("work item");
        }
        link_pull_requests(db.connection_mut()).expect("first");

        db.connection()
            .execute("UPDATE pull_requests SET title = 'PLA-4: fix'", [])
            .expect("retitle");
        assert_eq!(link_pull_requests(db.connection_mut()).expect("second"), 1);
        assert_eq!(
            links(&db),
            vec![(
                "pacerevenue/rates".into(),
                8,
                "PLA-4".into(),
                "title".into()
            )]
        );
    }

    fn work_item(id: &str) -> WorkItemRow {
        WorkItemRow {
            id: id.into(),
            source: "linear".into(),
            title: "t".into(),
            status: "Done".into(),
            item_type: "Issue".into(),
            tags: None,
            project: None,
            url: None,
            raw_json: None,
        }
    }
}

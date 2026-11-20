//! Linear issue attachments: storage and GitHub PR URL parsing (#272).
//!
//! Why: Linear's GitHub integration attaches each pull request that works on
//! an issue. Those attachments link a PR to its issue even when no key is in
//! the branch, title, body or commits.
//! What: [`store_attachments_in`] writes each issue's attachments to
//! `linear_issue_attachments`; [`parse_github_pr_url`] reads a
//! `https://github.com/<owner>/<repo>/pull/<n>` URL.
//! Test: `tests` below.

use rusqlite::{params, Connection};

use crate::collect::linear::issue::LinearIssue;
use crate::core::errors::Result;

/// A GitHub pull request named by a URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubPrRef {
    /// `owner/repo` as spelled in the URL.
    pub repository: String,
    /// The pull request number.
    pub pr_number: i64,
}

/// Parse a GitHub pull request URL.
///
/// Why: an attachment is any link; only a GitHub PR link names a PR.
/// What: accepts `http(s)://github.com/<owner>/<repo>/pull/<n>`, with any
/// trailing path, query or fragment (`/files`, `#discussion`). Every other
/// URL returns `None`: a non-PR link or another host is not a PR, and a
/// malformed URL is skipped by the caller as data filtering, not an error.
/// Test: `parses_a_github_pr_url`, `rejects_a_non_pr_github_url`,
/// `rejects_another_host`.
#[must_use]
pub fn parse_github_pr_url(url: &str) -> Option<GithubPrRef> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let path = rest.strip_prefix("github.com/")?;
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut parts = path.split('/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    if parts.next()? != "pull" {
        return None;
    }
    let pr_number: i64 = parts.next()?.parse().ok().filter(|n| *n > 0)?;
    Some(GithubPrRef {
        repository: format!("{owner}/{repo}"),
        pr_number,
    })
}

/// Replace the stored attachments of every issue that carries them.
///
/// Why: a re-sync must not duplicate rows, and an attachment removed in
/// Linear must disappear here.
/// What: for each issue with a `linear_id` and `Some` attachments, deletes
/// its rows and inserts the current set (`INSERT OR IGNORE`, so a URL listed
/// twice is one row). An issue with `None` attachments (a pre-#272 payload)
/// or no `linear_id` is left alone. Runs on `conn`, which should be an open
/// transaction. Returns the number of rows inserted.
/// Test: `storing_attachments_twice_keeps_one_set`.
///
/// # Errors
///
/// [`crate::core::TgaError::DbError`] when a delete or insert fails.
pub fn store_attachments_in(conn: &Connection, issues: &[LinearIssue]) -> Result<usize> {
    let mut written = 0;
    for issue in issues {
        let (Some(id), Some(atts)) = (issue.linear_id.as_deref(), issue.attachments.as_ref())
        else {
            continue;
        };
        conn.execute(
            "DELETE FROM linear_issue_attachments WHERE issue_id = ?1",
            params![id],
        )?;
        for a in atts {
            written += conn.execute(
                "INSERT OR IGNORE INTO linear_issue_attachments \
                 (issue_id, url, source_type, title) VALUES (?1, ?2, ?3, ?4)",
                params![id, a.url, a.source_type, a.title],
            )?;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collect::linear::issue::{parse_issue_node, tests::full_node};
    use crate::core::db::Database;

    #[test]
    fn parses_a_github_pr_url() {
        let want = Some(GithubPrRef {
            repository: "Acme/Widgets".into(),
            pr_number: 42,
        });
        assert_eq!(
            parse_github_pr_url("https://github.com/Acme/Widgets/pull/42"),
            want
        );
        assert_eq!(
            parse_github_pr_url("https://github.com/Acme/Widgets/pull/42/files#x"),
            want
        );
    }

    #[test]
    fn rejects_a_non_pr_github_url() {
        assert_eq!(
            parse_github_pr_url("https://github.com/acme/widgets/issues/42"),
            None
        );
        assert_eq!(parse_github_pr_url("https://github.com/acme/widgets"), None);
        assert_eq!(
            parse_github_pr_url("https://github.com/acme/widgets/pull/abc"),
            None
        );
        assert_eq!(parse_github_pr_url("not a url"), None);
    }

    #[test]
    fn rejects_another_host() {
        assert_eq!(
            parse_github_pr_url("https://gitlab.com/acme/widgets/pull/42"),
            None
        );
        assert_eq!(
            parse_github_pr_url("https://github.com.evil.test/a/b/pull/1"),
            None
        );
        assert_eq!(
            parse_github_pr_url("https://bitbucket.org/acme/widgets/pull-requests/42"),
            None
        );
    }

    fn issue_with(urls: &[&str]) -> LinearIssue {
        let mut node = full_node("uuid-1", "PLA-1", "PLA");
        node["attachments"]["nodes"] = serde_json::Value::Array(
            urls.iter()
                .map(|u| serde_json::json!({"url": u, "sourceType": "github", "title": "t"}))
                .collect(),
        );
        parse_issue_node("", &node)
    }

    fn stored(db: &Database) -> Vec<String> {
        let conn = db.connection();
        let mut stmt = conn
            .prepare("SELECT url FROM linear_issue_attachments ORDER BY url")
            .expect("prepare");
        stmt.query_map([], |r| r.get(0))
            .expect("query")
            .collect::<std::result::Result<_, _>>()
            .expect("rows")
    }

    #[test]
    fn storing_attachments_twice_keeps_one_set() {
        let db = Database::open_in_memory().expect("db");
        let a = "https://github.com/acme/w/pull/1";
        let b = "https://github.com/acme/w/pull/2";
        let first = issue_with(&[a, b, a]);
        store_attachments_in(db.connection(), std::slice::from_ref(&first)).expect("first");
        store_attachments_in(db.connection(), &[first]).expect("second");
        assert_eq!(stored(&db), vec![a.to_string(), b.to_string()]);

        // A removed attachment disappears; a pre-#272 payload changes nothing.
        store_attachments_in(db.connection(), &[issue_with(&[b])]).expect("third");
        assert_eq!(stored(&db), vec![b.to_string()]);
        let mut legacy = issue_with(&[]);
        legacy.attachments = None;
        store_attachments_in(db.connection(), &[legacy]).expect("legacy");
        assert_eq!(stored(&db), vec![b.to_string()]);
    }
}

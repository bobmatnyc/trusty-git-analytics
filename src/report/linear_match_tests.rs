//! Tests for the `tga linear match-report` data (#272).

use chrono::TimeZone;
use rusqlite::params;

use super::*;
use crate::collect::identity::IdentityResolver;
use crate::collect::linear::closed_weekly::tests::insert_issue;
use crate::collect::linear::developer::tests::insert_author;
use crate::core::db::Database;

fn config() -> Config {
    serde_yaml::from_str(
        r#"
repositories:
  - path: /src/rates
    github_repo: pacerevenue/rates
  - path: /src/duetto
    name: duetto
linear:
  repos: [pacerevenue]
"#,
    )
    .expect("yaml")
}

fn insert_pr(conn: &Connection, repo: &str, n: i64, author: &str, created: &str, shas: &str) {
    conn.execute(
        "INSERT INTO pull_requests (provider, repository, pr_number, title, author, state, \
                                    created_at, commit_shas) \
         VALUES ('github', ?1, ?2, 'x', ?3, 'merged', ?4, ?5)",
        params![repo, n, author, created, shas],
    )
    .expect("insert pr");
}

fn link(conn: &Connection, repo: &str, n: i64, key: &str, source: &str) {
    conn.execute(
        "INSERT INTO pr_work_items (provider, repository, pr_number, work_item_id, \
                                    work_item_source, source) \
         VALUES ('github', ?1, ?2, ?3, 'linear', ?4)",
        params![repo, n, key, source],
    )
    .expect("link");
}

fn commit(conn: &Connection, sha: &str, repo: &str, author_id: i64) {
    conn.execute(
        "INSERT INTO commits (sha, author_id, author_name, author_email, timestamp, message, \
                              repository) \
         VALUES (?1, ?2, 'n', 'e', '2026-03-03T00:00:00Z', 'm', ?3)",
        params![sha, author_id, repo],
    )
    .expect("commit");
}

/// The fixture the match-report test and the command test share.
pub(crate) fn seeded() -> Database {
    let db = Database::open_in_memory().expect("db");
    let c = db.connection();
    insert_author(c, "Ada Lovelace", "ada@example.com", r#"["ada-gh"]"#);
    let bob = insert_author(c, "Bob Smith", "bob@example.com", "[]");
    let cy = insert_author(c, "Cy Young", "cy@example.com", "[]");
    // Ada: 2 PRs by login, one linked by attachment and title.
    insert_pr(
        c,
        "pacerevenue/rates",
        1,
        "ada-gh",
        "2026-03-02T00:00:00Z",
        "[]",
    );
    insert_pr(
        c,
        "pacerevenue/rates",
        2,
        "ada-gh",
        "2026-03-04T00:00:00Z",
        "[]",
    );
    link(c, "pacerevenue/rates", 1, "PLA-1", "attachment");
    link(c, "pacerevenue/rates", 1, "PLA-1", "title");
    // Bob: credited through his commit, linked by branch.
    commit(c, "s1", "rates", bob);
    insert_pr(
        c,
        "pacerevenue/rates",
        3,
        "bobgh",
        "2026-03-05T00:00:00Z",
        r#"["s1"]"#,
    );
    link(c, "pacerevenue/rates", 3, "PLA-2", "branch");
    // Out of the window, and in an unbound repo: not counted.
    insert_pr(
        c,
        "pacerevenue/rates",
        4,
        "ada-gh",
        "2025-12-01T00:00:00Z",
        "[]",
    );
    insert_pr(
        c,
        "duettoresearch/duetto",
        5,
        "ada-gh",
        "2026-03-02T00:00:00Z",
        "[]",
    );
    // Cy commits in a bound repo and has no Linear user.
    commit(c, "s2", "rates", cy);
    // Closed issues: Ada (email), a stranger, one unassigned, one canceled.
    let a = Some(("Ada L.", "ADA@example.com"));
    insert_issue(c, "PLA-1", "completed", Some("2026-03-03T00:00:00Z"), a);
    insert_issue(c, "PLA-3", "canceled", Some("2026-03-03T00:00:00Z"), a);
    insert_issue(
        c,
        "PLA-4",
        "completed",
        Some("2026-03-03T00:00:00Z"),
        Some(("Zed Stranger", "zed@example.org")),
    );
    insert_issue(c, "PLA-5", "completed", Some("2026-03-03T00:00:00Z"), None);
    db
}

/// #272: the report's counts on the shared fixture.
#[test]
fn match_report_counts_on_a_fixture() {
    let db = seeded();
    let index = DeveloperIndex::load(db.connection(), IdentityResolver::new(None)).expect("idx");
    let since = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).single();
    let until = Utc.with_ymd_and_hms(2026, 4, 1, 0, 0, 0).single();
    let r = compute_match_report(db.connection(), &config(), &index, since, until).expect("report");

    assert_eq!(
        (r.prs.total, r.prs.with_ticket, r.prs.without_ticket),
        (3, 2, 1)
    );
    assert_eq!(r.prs.by_source.get("attachment"), Some(&1));
    assert_eq!(r.prs.by_source.get("title"), Some(&1));
    assert_eq!(r.prs.by_source.get("branch"), Some(&1));
    let rows: Vec<(&str, bool, usize, usize, usize)> = r
        .engineers
        .iter()
        .map(|e| {
            (
                e.name.as_str(),
                e.matched,
                e.prs,
                e.prs_with_ticket,
                e.closed_issues,
            )
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            ("Ada Lovelace", true, 2, 1, 1),
            ("Bob Smith", true, 1, 1, 0),
            ("Zed Stranger", false, 0, 0, 1),
        ]
    );
    assert_eq!(r.unassigned_closed, 1);
    assert_eq!(
        r.unmatched_linear_assignees,
        vec!["Zed Stranger".to_string()]
    );
    // Bob matches no Linear identity either: he has commits in a bound repo.
    assert_eq!(
        r.unmatched_tga_authors,
        vec!["Bob Smith".to_string(), "Cy Young".to_string()]
    );
    let text = render_text(&r);
    assert!(
        text.contains("3 total, 2 with a ticket, 1 without"),
        "{text}"
    );
    assert!(!text.contains('@'), "names only, no email: {text}");
}

#[test]
fn pr_repository_binding_follows_linear_repos() {
    let cfg = config();
    assert!(pr_repository_bound(&cfg, "PaceRevenue/rates"));
    assert!(!pr_repository_bound(&cfg, "duettoresearch/duetto"));
    assert!(!pr_repository_bound(
        &Config::default(),
        "pacerevenue/rates"
    ));
}

//! #245: the GitHub reviewer pass selects newest-first, bounded, and by a
//! per-PR "reviews checked at" marker.
//!
//! Why: the pass re-selected every PR with no `pr_reviewers` row, oldest
//! first. Zero-review PRs were re-fetched on every run, used up the rate
//! budget, and newer PRs never got reviews. These tests pin the selection
//! order, the cap, the marker, and the rate-limited arm.

use rusqlite::{params, Connection};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::ingest_reviews;
use crate::collect::collector::CollectionStats;
use crate::collect::github::reviewer_store::{mark_reviews_checked, select_review_candidates};
use crate::collect::github::GitHubClient;
use crate::core::config::GithubConfig;
use crate::core::db::Database;

/// Seed a GitHub PR in `acme/widgets` with the given `updated_at`.
fn seed(conn: &Connection, pr_number: i64, updated_at: &str) -> i64 {
    conn.execute(
        "INSERT INTO pull_requests \
         (provider, repository, pr_number, title, author, state, created_at, commit_shas, \
          updated_at) \
         VALUES ('github', 'acme/widgets', ?1, 'T', 'u', 'open', \
                 '2024-01-01T00:00:00+00:00', '[]', ?2)",
        params![pr_number, updated_at],
    )
    .expect("seed pr");
    conn.last_insert_rowid()
}

/// The PR numbers `select_review_candidates` returns, in order.
fn selected(conn: &Connection, force: bool, cap: u32) -> Vec<u64> {
    select_review_candidates(conn, force, cap)
        .expect("select")
        .into_iter()
        .map(|(_, _, n)| n)
        .collect()
}

/// The `reviews_checked_at` marker of PR `pr_number`.
fn marker(conn: &Connection, pr_number: i64) -> Option<String> {
    conn.query_row(
        "SELECT reviews_checked_at FROM pull_requests WHERE pr_number = ?1",
        params![pr_number],
        |r| r.get(0),
    )
    .expect("read marker")
}

/// Why: #245(d) — a rate-limited pass must not mark the PRs it never fetched,
/// and a PR fetched with zero reviews must not be re-selected forever.
/// What: PRs 1 (oldest), 2, 3 (newest). The mock answers PR 3 with zero
/// reviews and PR 2 with a secondary rate limit (`403`, `Retry-After: 0`),
/// which latches the run budget so PR 1 is never sent. Asserts the first
/// selection is newest-first, the pass does not fail the stage, only PR 3 is
/// marked, and the next selection is exactly PRs 2 and 1, newest first.
/// Test: this test itself. Against the pre-fix selection (`NOT EXISTS
/// pr_reviewers ... ORDER BY p.id`, no marker) the first assertion fails with
/// `[1, 2, 3]` instead of `[3, 2, 1]`; with the order fixed but no marker, the
/// second selection still returns PR 3 (`[3, 2, 1]` instead of `[2, 1]`).
#[tokio::test]
async fn a_rate_limited_pass_leaves_the_unfetched_prs_selectable_next_run() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widgets/pulls/3/reviews"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    for n in [1, 2] {
        Mock::given(method("GET"))
            .and(path(format!("/repos/acme/widgets/pulls/{n}/reviews")))
            .respond_with(ResponseTemplate::new(403).insert_header("retry-after", "0"))
            .mount(&server)
            .await;
    }

    let mut db = Database::open_in_memory().expect("open db");
    {
        let conn = db.connection();
        seed(conn, 1, "2024-01-01T00:00:00+00:00");
        seed(conn, 2, "2024-02-01T00:00:00+00:00");
        seed(conn, 3, "2024-03-01T00:00:00+00:00");
    }

    let first = select_review_candidates(db.connection(), false, 0).expect("select");
    let first_numbers: Vec<u64> = first.iter().map(|(_, _, n)| *n).collect();
    assert_eq!(
        first_numbers,
        vec![3, 2, 1],
        "the pass must run newest first"
    );

    let client = GitHubClient::new_for_reviews(&GithubConfig::default())
        .expect("client builds")
        .with_api_base(server.uri());
    let mut stats = CollectionStats::default();
    ingest_reviews(&mut db, &client, 1, &first, &mut stats).await;

    assert!(
        stats.stage_failures().is_empty(),
        "a rate limit must not fail the run; got: {:?}",
        stats.errors
    );
    let conn = db.connection();
    assert!(
        marker(conn, 3).is_some(),
        "PR 3 was fetched; it must be marked"
    );
    assert_eq!(
        marker(conn, 2),
        None,
        "PR 2 was rate-limited; it must stay unmarked"
    );
    assert_eq!(
        marker(conn, 1),
        None,
        "PR 1 was never sent; it must stay unmarked"
    );
    assert_eq!(
        selected(conn, false, 0),
        vec![2, 1],
        "the next run must select exactly the PRs this run did not fetch"
    );
}

/// Why: #245(c) — reviews added to a PR after its last check were never read.
/// What: marks PR 1 checked, asserts it is not selected, then bumps its
/// `updated_at` past the marker and asserts it is selected again. Also covers
/// a marker in the wall-clock spelling (milliseconds) that the migration
/// backfill and a NULL `updated_at` produce.
/// Test: this test itself. Pre-fix, a PR with a `pr_reviewers` row was never
/// re-selected whatever its `updated_at`.
#[test]
fn a_pr_updated_after_its_check_is_selected_again() {
    let db = Database::open_in_memory().expect("open db");
    let conn = db.connection();
    let id = seed(conn, 1, "2024-01-01T00:00:00+00:00");
    mark_reviews_checked(conn, id, "2030-01-01T00:00:00.000+00:00").expect("mark");
    assert_eq!(
        marker(conn, 1).as_deref(),
        Some("2024-01-01T00:00:00+00:00"),
        "the marker is the PR's own updated_at"
    );
    assert!(
        selected(conn, false, 0).is_empty(),
        "a checked PR is not re-selected"
    );

    conn.execute(
        "UPDATE pull_requests SET updated_at = '2024-01-05T09:30:00Z' WHERE id = ?1",
        params![id],
    )
    .expect("bump updated_at");
    assert_eq!(
        selected(conn, false, 0),
        vec![1],
        "an updated PR is re-read"
    );

    conn.execute(
        "UPDATE pull_requests SET reviews_checked_at = '2024-01-05T09:30:00.500+00:00' \
         WHERE id = ?1",
        params![id],
    )
    .expect("wall-clock marker");
    assert!(
        selected(conn, false, 0).is_empty(),
        "a marker later than updated_at, in a different spelling, still holds"
    );
}

/// Why: #245(b) — the pass must be bounded and spend its budget on the newest
/// PRs first.
/// What: five PRs with distinct `updated_at`, seeded out of order. Cap 2
/// returns the two newest, newest first; cap 0 returns all five.
/// Test: this test itself. Pre-fix the selection had no cap and ordered by
/// `p.id`, oldest first.
#[test]
fn selection_is_newest_first_and_capped() {
    let db = Database::open_in_memory().expect("open db");
    let conn = db.connection();
    seed(conn, 4, "2024-04-01T00:00:00+00:00");
    seed(conn, 1, "2024-01-01T00:00:00+00:00");
    seed(conn, 5, "2024-05-01T00:00:00+00:00");
    seed(conn, 2, "2024-02-01T00:00:00+00:00");
    seed(conn, 3, "2024-03-01T00:00:00+00:00");

    assert_eq!(selected(conn, false, 2), vec![5, 4]);
    assert_eq!(selected(conn, false, 0), vec![5, 4, 3, 2, 1]);
}

/// Why: `--force-refresh-prs` must stay the full re-fetch (#245).
/// What: marks every PR checked, then asserts a force selection with cap 1
/// still returns all three, newest first.
/// Test: this test itself.
#[test]
fn force_refresh_selects_every_github_pr_ignoring_marker_and_cap() {
    let db = Database::open_in_memory().expect("open db");
    let conn = db.connection();
    for (n, ts) in [
        (1, "2024-01-01T00:00:00+00:00"),
        (2, "2024-02-01T00:00:00+00:00"),
        (3, "2024-03-01T00:00:00+00:00"),
    ] {
        let id = seed(conn, n, ts);
        mark_reviews_checked(conn, id, "2030-01-01T00:00:00.000+00:00").expect("mark");
    }
    assert!(selected(conn, false, 1).is_empty());
    assert_eq!(selected(conn, true, 1), vec![3, 2, 1]);
}

/// Why: #245 — the v39 backfill must keep an upgraded database from
/// re-fetching every already-reviewed PR at once, while a PR with no reviewer
/// rows stays selectable.
/// What: builds a v38 database with two GitHub PRs, one with a reviewer row,
/// migrates to the head, and asserts only the unreviewed PR is selected.
/// Test: this test itself.
#[test]
fn migration_v39_backfills_the_marker_for_already_reviewed_prs() {
    let mut conn = Connection::open_in_memory().expect("open");
    crate::core::db::migrations::run_through(&mut conn, 38).expect("migrate to v38");
    conn.execute_batch(
        "INSERT INTO pull_requests \
         (provider, repository, pr_number, title, author, state, created_at, commit_shas, \
          fetched_at) \
         VALUES ('github','acme/widgets',1,'T','u','open','2024-01-01T00:00:00Z','[]', \
                 '2024-06-01T00:00:00Z'), \
                ('github','acme/widgets',2,'T','u','open','2024-01-02T00:00:00Z','[]', \
                 '2024-06-01T00:00:00Z'); \
         INSERT INTO pr_reviewers \
         (pr_id, provider, reviewer_id, vote, is_required, is_container, review_state) \
         SELECT id, 'github', 'alice', 0, 0, 0, 'APPROVED' FROM pull_requests \
         WHERE pr_number = 1;",
    )
    .expect("seed v38 rows");

    crate::core::db::migrations::run(&mut conn).expect("migrate to head");

    assert_eq!(
        marker(&conn, 1).as_deref(),
        Some("2024-06-01T00:00:00Z"),
        "a reviewed PR starts at its fetched_at"
    );
    assert_eq!(marker(&conn, 2), None, "an unreviewed PR keeps no marker");
    assert_eq!(selected(&conn, false, 0), vec![2]);
}

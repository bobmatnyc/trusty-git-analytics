//! `tga linear sync --history --comments` end to end against a mock Linear
//! (#190 step 6): rows per issue, the failed read that must stay due, the
//! re-run that reads nothing, `--backfill`, and the dry run. Hand-built
//! GraphQL pages; no network.

use super::*;
use tga::collect::linear::activity::tests::{
    activity_call, activity_page, comment_node, history_node,
};
use tga::collect::linear::bulk::tests::{mock_client, node, page_response};
use tga::core::db::get_linear_cursor;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ISSUES: [&str; 2] = ["ENG-1", "ENG-2"];

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

/// Answer ENG's issue walk with ENG-1 and ENG-2.
async fn mount_issues(server: &MockServer) {
    let nodes = ISSUES
        .iter()
        .map(|i| node(i, "2026-01-01T00:01:00.000Z", None))
        .collect();
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .respond_with(page_response(nodes, false, None))
        .mount(server)
        .await;
}

/// Answer `identifier`'s history (two transitions, one other entry) and
/// comments (one comment).
async fn mount_activity(server: &MockServer, identifier: &str) {
    let id = format!("uuid-{identifier}");
    activity_call(ActivityKind::History, &id)
        .respond_with(activity_page(
            ActivityKind::History,
            &[
                history_node(
                    &format!("{id}-h1"),
                    "2026-01-01T00:00:00.000Z",
                    None,
                    Some("Todo"),
                ),
                history_node(&format!("{id}-h2"), "2026-01-01T00:00:30.000Z", None, None),
                history_node(
                    &format!("{id}-h3"),
                    "2026-01-01T00:01:00.000Z",
                    Some("Todo"),
                    Some("Done"),
                ),
            ],
            false,
            None,
        ))
        .mount(server)
        .await;
    activity_call(ActivityKind::Comments, &id)
        .respond_with(activity_page(
            ActivityKind::Comments,
            &[comment_node(
                &format!("{id}-c1"),
                "2026-01-01T00:00:10.000Z",
                "lgtm",
                None,
            )],
            false,
            None,
        ))
        .mount(server)
        .await;
}

fn args(history: bool, comments: bool) -> LinearSyncArgs {
    LinearSyncArgs {
        team: Some("ENG".into()),
        history,
        comments,
        ..Default::default()
    }
}

/// Requests whose body reads an issue's `history` or `comments`.
async fn activity_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            let body = String::from_utf8_lossy(&r.body);
            body.contains("history(first: $first") || body.contains("comments(first: $first")
        })
        .count()
}

/// #190 step 6: every issue's transitions and comments are stored, and the
/// outcome reports what was due, read and written.
#[tokio::test]
async fn history_and_comments_land_for_every_issue() {
    let server = MockServer::start().await;
    mount_issues(&server).await;
    for i in ISSUES {
        mount_activity(&server, i).await;
    }
    let mut db = Database::open_in_memory().expect("db");
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args(true, true),
    )
    .await
    .expect("sync");

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_transitions"),
        4
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        2
    );
    let activity = &outcomes[0].activity;
    assert_eq!(activity.len(), 2);
    assert_eq!(
        (
            activity[0].kind,
            activity[0].due,
            activity[0].read,
            activity[0].rows
        ),
        (ActivityKind::History, 2, 2, 4)
    );
    assert_eq!(
        (
            activity[1].kind,
            activity[1].due,
            activity[1].read,
            activity[1].rows
        ),
        (ActivityKind::Comments, 2, 2, 2)
    );
    assert!(activity[0].summary_line(false).contains("wrote 4"));
}

/// #190 step 6 (Fail-Open check, error arm): a history read that fails on
/// one issue fails the whole sync, naming the issue; the issue cursor still
/// advances, but the failed issue stays due, so the next run — whose
/// incremental issue walk no longer needs to return it — reads it.
#[tokio::test]
async fn a_failed_history_read_fails_the_sync_and_stays_due() {
    let server = MockServer::start().await;
    mount_issues(&server).await;
    mount_activity(&server, "ENG-1").await;
    activity_call(ActivityKind::History, "uuid-ENG-2")
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream down"))
        .mount(&server)
        .await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");

    let err = run_sync_with(&client, &Config::default(), &mut db, &args(true, false))
        .await
        .expect_err("a failed history read must fail the sync");
    let text = format!("{err:#}");
    assert!(
        text.contains("ENG-2") && text.contains("history") && text.contains("500"),
        "{text}"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 2);
    assert!(get_linear_cursor(db.connection(), "ENG")
        .expect("read")
        .is_some());
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_linear_transitions WHERE identifier = 'ENG-2'"
        ),
        0
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM linear_issue_activity_state \
             WHERE issue_id = 'uuid-ENG-2' AND history_for IS NOT NULL"
        ),
        0,
        "the failed issue's marker must not move"
    );

    // Linear recovers; nothing about the issues changed.
    server.reset().await;
    mount_issues(&server).await;
    mount_activity(&server, "ENG-2").await;
    let outcomes = run_sync_with(&client, &Config::default(), &mut db, &args(true, false))
        .await
        .expect("second sync");
    let history = &outcomes[0].activity[0];
    assert_eq!((history.due, history.read), (1, 1));
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_linear_transitions WHERE identifier = 'ENG-2'"
        ),
        2
    );
}

/// #190 step 6: a re-run with no issue change sends no activity request;
/// `--backfill` reads every issue again and leaves the row counts stable.
#[tokio::test]
async fn unchanged_issues_are_not_read_again() {
    let server = MockServer::start().await;
    mount_issues(&server).await;
    for i in ISSUES {
        mount_activity(&server, i).await;
    }
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    run_sync_with(&client, &Config::default(), &mut db, &args(true, true))
        .await
        .expect("first");
    let first = activity_requests(&server).await;
    assert_eq!(first, 4);

    let outcomes = run_sync_with(&client, &Config::default(), &mut db, &args(true, true))
        .await
        .expect("second");
    assert_eq!(activity_requests(&server).await, first);
    assert!(outcomes[0]
        .activity
        .iter()
        .all(|a| a.due == 0 && a.read == 0));

    let backfill = LinearSyncArgs {
        backfill: true,
        ..args(true, true)
    };
    let outcomes = run_sync_with(&client, &Config::default(), &mut db, &backfill)
        .await
        .expect("backfill");
    assert!(outcomes[0]
        .activity
        .iter()
        .all(|a| a.due == 2 && a.read == 2));
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_transitions"),
        4
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        2
    );
}

/// #190 step 6: a dry run counts the due issues from the fetched issues,
/// sends no activity request, and leaves the database file unchanged.
#[tokio::test]
async fn dry_run_counts_due_issues_and_sends_no_activity_request() {
    let server = MockServer::start().await;
    mount_issues(&server).await;
    for i in ISSUES {
        mount_activity(&server, i).await;
    }
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tga.db");
    drop(Database::open(&path).expect("create and migrate"));
    let before = std::fs::read(&path).expect("read db");

    let mut db = Database::open_read_only(&path).expect("read-only open");
    let dry = LinearSyncArgs {
        dry_run: true,
        ..args(true, true)
    };
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &dry,
    )
    .await
    .expect("dry run");
    drop(db);

    assert!(outcomes[0]
        .activity
        .iter()
        .all(|a| a.due == 2 && a.read == 0));
    assert!(outcomes[0].activity[0]
        .summary_line(true)
        .contains("would read 2"));
    assert_eq!(activity_requests(&server).await, 0);
    assert_eq!(std::fs::read(&path).expect("read db"), before);
}

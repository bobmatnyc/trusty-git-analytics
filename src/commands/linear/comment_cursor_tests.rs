//! `tga linear sync --comments` incremental runs against a mock Linear
//! (#190): the comments pass reads every comment whose own `updatedAt` moved,
//! whether or not its issue's `updatedAt` did. Live Linear data showed
//! `Issue.updatedAt` does not reliably move on a comment create or edit, so a
//! pass keyed on the issue marker missed comments. Hand-built GraphQL pages;
//! no network.

use super::*;
use serde_json::{json, Value as Json};
use tga::collect::linear::activity::tests::{activity_call, activity_page};
use tga::collect::linear::bulk::tests::{mock_client, node, page_response};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Marks the workspace `comments` query (the per-issue one reads
/// `issue(id:) { comments(first: $first, after: $after, includeArchived: ...`).
const WORKSPACE_COMMENTS: &str = "comments(first: $first, after: $after, filter: $filter";

/// The issue `updatedAt` every run reports: no issue ever changes here.
const ISSUE_UPDATED: &str = "2026-01-01T00:01:00.000Z";

/// One comment as both the per-issue and the workspace walk return it.
#[derive(Clone)]
struct C {
    id: &'static str,
    issue: &'static str,
    created: &'static str,
    updated: &'static str,
    body: &'static str,
}

impl C {
    /// The node, with its issue, as the workspace `comments` query returns it.
    fn node(&self) -> Json {
        json!({"id": self.id, "createdAt": self.created, "updatedAt": self.updated,
               "body": self.body, "user": {"id": "user-2", "name": "Grace Hopper"},
               "parent": null,
               "issue": {"id": format!("uuid-{}", self.issue), "identifier": self.issue,
                         "team": {"key": "ENG"}}})
    }
}

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

fn args(backfill: bool) -> LinearSyncArgs {
    LinearSyncArgs {
        team: Some("ENG".into()),
        comments: true,
        backfill,
        ..Default::default()
    }
}

/// One page of the workspace `comments` connection.
fn workspace_page(comments: &[C], has_next: bool, cursor: Option<&str>) -> ResponseTemplate {
    let nodes: Vec<Json> = comments.iter().map(C::node).collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "data": {"comments": {
            "nodes": nodes,
            "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
        }}
    }))
}

/// Answer ENG's issue walk with ENG-1 and ENG-2 (never changing), each
/// issue's own comments connection, and the workspace `comments` query, all
/// with `comments`.
async fn mount(server: &MockServer, comments: &[C]) {
    let issues = ["ENG-1", "ENG-2"]
        .iter()
        .map(|i| node(i, ISSUE_UPDATED, None))
        .collect();
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .and(body_string_contains("issues(first: $first"))
        .respond_with(page_response(issues, false, None))
        .mount(server)
        .await;
    for issue in ["ENG-1", "ENG-2"] {
        let own: Vec<Json> = comments
            .iter()
            .filter(|c| c.issue == issue)
            .map(|c| {
                let mut n = c.node();
                n.as_object_mut().map(|o| o.remove("issue"));
                n
            })
            .collect();
        activity_call(ActivityKind::Comments, &format!("uuid-{issue}"))
            .respond_with(activity_page(ActivityKind::Comments, &own, false, None))
            .mount(server)
            .await;
    }
    Mock::given(method("POST"))
        .and(body_string_contains(WORKSPACE_COMMENTS))
        .respond_with(workspace_page(comments, false, None))
        .mount(server)
        .await;
}

/// Bodies of the workspace `comments` requests the server received.
async fn workspace_requests(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| String::from_utf8_lossy(&r.body).into_owned())
        .filter(|b| b.contains(WORKSPACE_COMMENTS))
        .collect()
}

/// Requests that read one issue's own `comments` connection.
async fn per_issue_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| {
            String::from_utf8_lossy(&r.body)
                .contains("comments(first: $first, after: $after, includeArchived")
        })
        .count()
}

const C1: C = C {
    id: "c1",
    issue: "ENG-1",
    created: "2026-01-02T00:00:00.000Z",
    updated: "2026-01-02T00:00:00.000Z",
    body: "lgtm",
};

/// Run one incremental (or `--backfill`) comments sync.
async fn sync(server: &MockServer, db: &mut Database, backfill: bool) -> anyhow::Result<()> {
    run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        db,
        &args(backfill),
    )
    .await
    .map(|_| ())
}

/// The stored `(updated_at, body_len)` of comment `id`.
fn stored(db: &Database, id: &str) -> Option<(String, i64)> {
    db.connection()
        .query_row(
            "SELECT updated_at, body_len FROM fact_linear_comment_detail WHERE comment_id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

/// #190 (a): a comment edited in Linear, whose issue's `updatedAt` did not
/// move, is read by the next incremental run and its row updated.
#[tokio::test]
async fn an_edited_comment_is_read_without_an_issue_change() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");
    assert_eq!(stored(&db, "c1"), Some((C1.updated.to_string(), 4)));

    server.reset().await;
    let edited = C {
        updated: "2026-01-03T00:00:00.000Z",
        body: "lgtm, merged",
        ..C1
    };
    mount(&server, &[edited]).await;
    sync(&server, &mut db, false).await.expect("second run");
    assert_eq!(
        stored(&db, "c1"),
        Some(("2026-01-03T00:00:00.000Z".to_string(), 12)),
        "the edit must reach the stored row"
    );
}

/// #190 (b): a new comment on an issue whose `updatedAt` did not move is
/// stored by the next incremental run.
#[tokio::test]
async fn a_new_comment_on_an_unchanged_issue_is_stored() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");

    server.reset().await;
    let new = C {
        id: "c2",
        issue: "ENG-2",
        created: "2026-01-03T00:00:00.000Z",
        updated: "2026-01-03T00:00:00.000Z",
        body: "why?",
    };
    mount(&server, &[C1, new]).await;
    sync(&server, &mut db, false).await.expect("second run");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_linear_comment_detail \
             WHERE comment_id = 'c2' AND identifier = 'ENG-2'"
        ),
        1,
        "the new comment must be stored"
    );
}

/// #190 (c): the next run asks for comments updated after the stored cursor
/// minus the 10-minute overlap, and re-reading a comment inside the overlap
/// writes no second row.
#[tokio::test]
async fn an_overlap_re_read_writes_no_duplicate() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");
    for run in ["second", "third"] {
        sync(&server, &mut db, false).await.expect(run);
    }
    let bodies = workspace_requests(&server).await;
    // C1.updated (2026-01-02T00:00:00Z) minus 10 minutes.
    let bounded = bodies
        .iter()
        .filter(|b| b.contains("\"updatedAt\":{\"gt\":\"2026-01-01T23:50:00.000Z\"}"))
        .count();
    assert_eq!(
        bounded, 2,
        "runs two and three must read from cursor minus the overlap: {bodies:?}"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        1
    );
}

/// #190 (d) (Fail-Open check, error arm): a failed page mid-walk fails the
/// run, writes none of the walk's comments, and leaves the cursor where it
/// was, so the next run reads the same window again.
#[tokio::test]
async fn a_failed_comments_page_fails_the_run_and_keeps_the_cursor() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");

    server.reset().await;
    let c2 = C {
        id: "c2",
        issue: "ENG-2",
        created: "2026-01-03T00:00:00.000Z",
        updated: "2026-01-03T00:00:00.000Z",
        body: "why?",
    };
    mount(&server, &[C1, c2.clone()]).await;
    // Priority 1 beats the default 5, so these answer the workspace query.
    Mock::given(method("POST"))
        .and(body_string_contains(WORKSPACE_COMMENTS))
        .and(body_string_contains("\"after\":null"))
        .respond_with(workspace_page(&[c2], true, Some("p-1")))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(WORKSPACE_COMMENTS))
        .and(body_string_contains("\"after\":\"p-1\""))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream down"))
        .with_priority(1)
        .mount(&server)
        .await;
    let err = sync(&server, &mut db, false)
        .await
        .expect_err("a failed comments page must fail the run");
    let text = format!("{err:#}");
    assert!(text.contains("comments") && text.contains("500"), "{text}");
    assert_eq!(stored(&db, "c2"), None, "page 1 must not be written");
    let cursor: String = db
        .connection()
        .query_row(
            "SELECT cursor_updated_at FROM linear_comment_cursor WHERE team_key = 'ENG'",
            [],
            |r| r.get(0),
        )
        .expect("cursor row");
    assert_eq!(cursor, C1.updated, "the cursor must not move");
}

/// #190: with no stored cursor, the comments pass reads the team's whole
/// comment set through the workspace query (no `updatedAt` bound) instead of
/// one walk per issue.
#[tokio::test]
async fn first_run_sweeps_the_team_without_a_bound() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");
    let bodies = workspace_requests(&server).await;
    assert_eq!(bodies.len(), 1, "{bodies:?}");
    assert!(
        bodies[0].contains("\"team\":{\"key\":{\"eq\":\"ENG\"}}")
            && !bodies[0].contains("\"updatedAt\":{"),
        "{}",
        bodies[0]
    );
    assert_eq!(per_issue_requests(&server).await, 0);
    assert_eq!(stored(&db, "c1"), Some((C1.updated.to_string(), 4)));
}

/// #190 (e): `--backfill --comments` keeps the per-issue walk, which also
/// removes a comment Linear no longer has, and sends no workspace query.
#[tokio::test]
async fn backfill_keeps_the_per_issue_walk() {
    let server = MockServer::start().await;
    let c2 = C {
        id: "c2",
        issue: "ENG-1",
        created: "2026-01-03T00:00:00.000Z",
        updated: "2026-01-03T00:00:00.000Z",
        body: "gone soon",
    };
    mount(&server, &[C1, c2]).await;
    let mut db = Database::open_in_memory().expect("db");
    sync(&server, &mut db, false).await.expect("first run");
    assert!(stored(&db, "c2").is_some());

    server.reset().await;
    mount(&server, &[C1]).await;
    sync(&server, &mut db, true).await.expect("backfill");
    assert_eq!(per_issue_requests(&server).await, 2);
    assert!(workspace_requests(&server).await.is_empty());
    assert_eq!(
        stored(&db, "c2"),
        None,
        "a deleted comment's row is removed"
    );
    assert!(stored(&db, "c1").is_some());
}

/// #190: a comment whose issue is not in `linear_issues` (created after the
/// issue pass, or never synced) is stored and counted in the summary line,
/// never dropped silently.
#[tokio::test]
async fn unlinked_comments_are_stored_and_counted() {
    let server = MockServer::start().await;
    let elsewhere = C {
        id: "c9",
        issue: "ENG-9",
        ..C1
    };
    mount(&server, &[C1, elsewhere]).await;
    let mut db = Database::open_in_memory().expect("db");
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args(false),
    )
    .await
    .expect("sync");
    let comments = outcomes[0].comments.as_ref().expect("comments pass");
    assert_eq!((comments.written, comments.unlinked), (2, 1));
    let line = comments.summary_line(false);
    assert!(
        line.contains("1 on issue(s) not yet in linear_issues"),
        "{line}"
    );
    assert!(stored(&db, "c9").is_some());
}

/// #190: a dry run with a stored cursor reports the bound it would read from
/// and sends no comments request.
#[tokio::test]
async fn dry_run_sends_no_comments_request() {
    let server = MockServer::start().await;
    mount(&server, &[C1]).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tga.db");
    {
        let mut db = Database::open(&path).expect("create");
        sync(&server, &mut db, false).await.expect("first run");
    }
    let before = workspace_requests(&server).await.len();
    let mut db = Database::open_read_only(&path).expect("read-only open");
    let dry = LinearSyncArgs {
        dry_run: true,
        ..args(false)
    };
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &dry,
    )
    .await
    .expect("dry run");
    assert_eq!(workspace_requests(&server).await.len(), before);
    let line = outcomes[0]
        .comments
        .as_ref()
        .expect("comments pass")
        .summary_line(true);
    assert!(
        line.contains("would read comments updated after 2026-01-01T23:50:00.000Z"),
        "{line}"
    );
}

/// Marks the due-issue walk: the workspace `comments` query filtered by the
/// ids of issues that moved into the team.
const BY_ISSUE_ID: &str = "\"issue\":{\"id\":{\"in\":[";

/// The `updatedAt` the moved and new issues carry in the run that sees them.
const MOVED_UPDATED: &str = "2026-01-03T00:00:00.000Z";

/// Written on ENG-5 while it sat in OPS; older than ENG's cursor minus the
/// overlap, so the team walk never returns it.
const C5: C = C {
    id: "c5",
    issue: "ENG-5",
    created: "2026-01-01T06:00:00.000Z",
    updated: "2026-01-01T06:00:00.000Z",
    body: "moved",
};

/// Written on ENG-6, an issue new to ENG but created before ENG's window.
const C6: C = C {
    id: "c6",
    issue: "ENG-6",
    created: "2026-01-01T07:00:00.000Z",
    updated: "2026-01-01T07:00:00.000Z",
    body: "old",
};

/// Store issue `uuid-ENG-5` as OPS-3 through an OPS issue sync (no comments).
async fn store_in_ops(server: &MockServer, db: &mut Database) {
    let mut n = node("ENG-5", "2026-01-01T05:00:00.000Z", None);
    n["identifier"] = json!("OPS-3");
    n["team"]["key"] = json!("OPS");
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"OPS\""))
        .and(body_string_contains("issues(first: $first"))
        .respond_with(page_response(vec![n], false, None))
        .mount(server)
        .await;
    let ops = LinearSyncArgs {
        team: Some("OPS".into()),
        ..Default::default()
    };
    run_sync_with(&mock_client(&server.uri()), &Config::default(), db, &ops)
        .await
        .expect("OPS issue sync");
}

/// Answer ENG's issue walk with ENG-1 and ENG-2 (unchanged), ENG-5 (moved
/// in from OPS), ENG-6 (new, created before ENG's comment window) and ENG-7
/// (new, created inside it). The team comments walk returns what Linear
/// would for the window — C1 only. `due` answers the walk by issue id.
async fn mount_moved(server: &MockServer, due: ResponseTemplate) {
    let mut issues: Vec<Json> = ["ENG-1", "ENG-2"]
        .iter()
        .map(|i| node(i, ISSUE_UPDATED, None))
        .collect();
    issues.push(node("ENG-5", MOVED_UPDATED, None));
    issues.push(node("ENG-6", MOVED_UPDATED, None));
    let mut fresh = node("ENG-7", MOVED_UPDATED, None);
    fresh["createdAt"] = json!("2026-01-02T12:00:00.000Z");
    issues.push(fresh);
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .and(body_string_contains("issues(first: $first"))
        .respond_with(page_response(issues, false, None))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(WORKSPACE_COMMENTS))
        .respond_with(workspace_page(&[C1], false, None))
        .mount(server)
        .await;
    // Priority 1 beats the team walk's default 5.
    Mock::given(method("POST"))
        .and(body_string_contains(BY_ISSUE_ID))
        .respond_with(due)
        .with_priority(1)
        .mount(server)
        .await;
}

/// Bodies of the due-issue walk requests the server received.
async fn due_requests(server: &MockServer) -> Vec<String> {
    workspace_requests(server)
        .await
        .into_iter()
        .filter(|b| b.contains(BY_ISSUE_ID))
        .collect()
}

/// ENG holds a comment cursor (C1) and OPS holds `uuid-ENG-5` as OPS-3.
async fn moved_setup(server: &MockServer, db: &mut Database) {
    mount(server, &[C1]).await;
    sync(server, db, false).await.expect("first ENG run");
    store_in_ops(server, db).await;
    server.reset().await;
}

/// #190: an issue that moves into the team, and an issue new to the team but
/// created before its comment window, have every comment read — including
/// one whose `updatedAt` is older than the cursor minus the overlap, which
/// the team walk never returns. An issue created inside the window needs no
/// extra read. A later run reads none of them again.
#[tokio::test]
async fn a_moved_issues_old_comments_are_read_in_full() {
    let server = MockServer::start().await;
    let mut db = Database::open_in_memory().expect("db");
    moved_setup(&server, &mut db).await;

    mount_moved(&server, workspace_page(&[C5, C6], false, None)).await;
    sync(&server, &mut db, false).await.expect("second ENG run");
    assert_eq!(
        stored(&db, "c5"),
        Some((C5.updated.to_string(), 5)),
        "the moved issue's old comment must be stored"
    );
    assert_eq!(stored(&db, "c6"), Some((C6.updated.to_string(), 3)));
    let due = due_requests(&server).await;
    assert_eq!(due.len(), 1, "{due:?}");
    assert!(
        due[0].contains("uuid-ENG-5")
            && due[0].contains("uuid-ENG-6")
            && !due[0].contains("uuid-ENG-7")
            && !due[0].contains("\"updatedAt\":{"),
        "{}",
        due[0]
    );

    sync(&server, &mut db, false).await.expect("third ENG run");
    assert_eq!(due_requests(&server).await.len(), 1, "read once, not again");
}

/// #190: a move seen by an issue-only run (no `--comments`) is still read by
/// the next `--comments` run, whose issue pass sees the issue unchanged.
#[tokio::test]
async fn a_move_seen_by_an_issue_only_run_is_read_by_the_next_comments_run() {
    let server = MockServer::start().await;
    let mut db = Database::open_in_memory().expect("db");
    moved_setup(&server, &mut db).await;

    mount_moved(&server, workspace_page(&[C5], false, None)).await;
    let issues_only = LinearSyncArgs {
        comments: false,
        ..args(false)
    };
    run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &issues_only,
    )
    .await
    .expect("issue-only ENG run");
    assert_eq!(stored(&db, "c5"), None);

    sync(&server, &mut db, false).await.expect("comments run");
    assert_eq!(
        stored(&db, "c5"),
        Some((C5.updated.to_string(), 5)),
        "the move recorded by the issue-only run must reach the comments pass"
    );
}

/// #190 (Fail-Open check): a failed due-issue page fails the run, writes no
/// comment, keeps the cursor, and leaves the moved issue due for the next run.
#[tokio::test]
async fn a_failed_moved_issue_walk_fails_the_run_and_stays_due() {
    let server = MockServer::start().await;
    let mut db = Database::open_in_memory().expect("db");
    moved_setup(&server, &mut db).await;

    mount_moved(
        &server,
        ResponseTemplate::new(500).set_body_string("upstream down"),
    )
    .await;
    let err = sync(&server, &mut db, false)
        .await
        .expect_err("a failed due-issue page must fail the run");
    assert!(format!("{err:#}").contains("500"), "{err:#}");
    assert_eq!(stored(&db, "c5"), None);
    let cursor: String = db
        .connection()
        .query_row(
            "SELECT cursor_updated_at FROM linear_comment_cursor WHERE team_key = 'ENG'",
            [],
            |r| r.get(0),
        )
        .expect("cursor row");
    assert_eq!(cursor, C1.updated, "the cursor must not move");

    server.reset().await;
    mount_moved(&server, workspace_page(&[C5], false, None)).await;
    sync(&server, &mut db, false).await.expect("retry run");
    assert_eq!(stored(&db, "c5"), Some((C5.updated.to_string(), 5)));
}

//! `tga linear sync` end to end against a mock Linear (#190): all teams,
//! the cap, dry-run, incremental re-sync, the team move, and `raw_json`.
//! Hand-built GraphQL pages; no network.

use super::*;
use tga::collect::linear::bulk::tests::{mock_client, node, page_response};
use tga::collect::linear::issue::tests::full_node;
use tga::core::db::get_linear_cursor;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn teams_response(keys: &[&str]) -> ResponseTemplate {
    let nodes: Vec<serde_json::Value> = keys
        .iter()
        .map(|k| serde_json::json!({"id": format!("team-{k}"), "key": k, "name": k}))
        .collect();
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "data": {"teams": {"nodes": nodes,
            "pageInfo": {"hasNextPage": false, "endCursor": null}}}
    }))
}

/// Answer the `teams` query with `keys`.
async fn mount_teams(server: &MockServer, keys: &[&str]) {
    Mock::given(method("POST"))
        .and(body_string_contains("teams(first"))
        .respond_with(teams_response(keys))
        .mount(server)
        .await;
}

/// Answer `team`'s issue walk with one page of `nodes`.
async fn mount_issues(server: &MockServer, team: &str, nodes: Vec<serde_json::Value>) {
    Mock::given(method("POST"))
        .and(body_string_contains(format!("\"eq\":\"{team}\"")))
        .respond_with(page_response(nodes, false, None))
        .mount(server)
        .await;
}

fn team_node(id: &str, identifier: &str, team: &str, updated: &str) -> serde_json::Value {
    let mut n = full_node(id, identifier, team);
    n["updatedAt"] = serde_json::json!(updated);
    n
}

fn args(team: Option<&str>) -> LinearSyncArgs {
    LinearSyncArgs {
        team: team.map(String::from),
        all_teams: team.is_none(),
        ..Default::default()
    }
}

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

/// #190 acceptance 2 and 8: `--all-teams` lists the teams, syncs each under
/// its own cursor, and includes archived issues.
#[tokio::test]
async fn all_teams_syncs_each_team_under_its_own_cursor() {
    let server = MockServer::start().await;
    mount_teams(&server, &["ENG", "OPS"]).await;
    mount_issues(
        &server,
        "ENG",
        vec![
            node("ENG-1", "2026-01-01T00:01:00.000Z", None),
            node(
                "ENG-2",
                "2026-01-01T00:02:00.000Z",
                Some("2026-01-03T00:00:00.000Z"),
            ),
        ],
    )
    .await;
    mount_issues(
        &server,
        "OPS",
        vec![team_node("uuid-o1", "OPS-1", "OPS", "2026-02-01T00:00:00Z")],
    )
    .await;

    let mut db = Database::open_in_memory().expect("db");
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args(None),
    )
    .await
    .expect("sync");

    let keys: Vec<&str> = outcomes.iter().map(|o| o.team_key.as_str()).collect();
    assert_eq!(keys, vec!["ENG", "OPS"]);
    assert_eq!(outcomes[0].archived, 1);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 3);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM linear_issues WHERE archived = 1"),
        1
    );
    let eng = get_linear_cursor(db.connection(), "ENG")
        .expect("read")
        .expect("ENG cursor");
    let ops = get_linear_cursor(db.connection(), "OPS")
        .expect("read")
        .expect("OPS cursor");
    assert!(eng.last_synced_at.starts_with("2026-01-01T00:02"));
    assert!(ops.last_synced_at.starts_with("2026-02-01"));
}

/// #190 acceptance 2 (Fail-Open check): a team over `--max-issues` fails the
/// sync with its key and the cap, writes no row and records no cursor.
#[tokio::test]
async fn cap_exceeded_fails_and_writes_nothing() {
    let server = MockServer::start().await;
    mount_issues(
        &server,
        "ENG",
        vec![
            node("ENG-1", "2026-01-01T00:01:00.000Z", None),
            node("ENG-2", "2026-01-01T00:02:00.000Z", None),
        ],
    )
    .await;
    let mut db = Database::open_in_memory().expect("db");
    let sync_args = LinearSyncArgs {
        max_issues: Some(1),
        ..args(Some("ENG"))
    };
    let err = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &sync_args,
    )
    .await
    .expect_err("over the cap must fail");
    let text = format!("{err:#}");
    assert!(
        text.contains("ENG") && text.contains("--max-issues 1"),
        "{text}"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 0);
    assert!(get_linear_cursor(db.connection(), "ENG")
        .expect("read")
        .is_none());
}

/// #190 acceptance 6: `--dry-run` runs against a read-only handle (#189),
/// reports what it would write, and writes nothing.
#[tokio::test]
async fn dry_run_reports_and_writes_nothing() {
    let server = MockServer::start().await;
    mount_issues(
        &server,
        "ENG",
        vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
    )
    .await;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("tga.db");
    drop(Database::open(&path).expect("create and migrate"));
    let before = std::fs::read(&path).expect("read db");

    let mut db = Database::open_read_only(&path).expect("read-only open");
    let sync_args = LinearSyncArgs {
        dry_run: true,
        ..args(Some("ENG"))
    };
    let outcomes = run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &sync_args,
    )
    .await
    .expect("dry run");
    drop(db);

    assert_eq!(outcomes[0].counts.new, 1);
    assert!(outcomes[0].summary_line(true).contains("would write 1"));
    assert_eq!(std::fs::read(&path).expect("read db"), before);
}

/// #190 acceptance 7: a second sync with no remote change writes no issue
/// row, and its request carries the team's cursor as the `updatedAt` bound.
#[tokio::test]
async fn second_sync_with_no_remote_change_writes_nothing() {
    let server = MockServer::start().await;
    mount_issues(
        &server,
        "ENG",
        vec![
            node("ENG-1", "2026-01-01T00:01:00.000Z", None),
            node(
                "ENG-2",
                "2026-01-01T00:02:00.000Z",
                Some("2026-01-03T00:00:00.000Z"),
            ),
        ],
    )
    .await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    let first = run_sync_with(&client, &Config::default(), &mut db, &args(Some("ENG")))
        .await
        .expect("first");
    assert_eq!(first[0].counts.new, 2);

    let second = run_sync_with(&client, &Config::default(), &mut db, &args(Some("ENG")))
        .await
        .expect("second");
    assert_eq!(second[0].counts.written(), 0, "{:?}", second[0].counts);
    assert_eq!(second[0].counts.unchanged, 2);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 2);

    let requests = server.received_requests().await.expect("recorded");
    let last = String::from_utf8_lossy(&requests.last().expect("a request").body).to_string();
    assert!(
        last.contains("\"gte\":\"2026-01-01T00:02:00+00:00\""),
        "second request must resume from the cursor: {last}"
    );
}

/// #190 acceptance 4: an issue that moves from ENG to OPS between runs keeps
/// one `linear_issues` row, now under its new identifier and team.
#[tokio::test]
async fn moved_issue_keeps_one_row_across_runs() {
    let server = MockServer::start().await;
    mount_teams(&server, &["ENG", "OPS"]).await;
    mount_issues(
        &server,
        "ENG",
        vec![team_node("uuid-m", "ENG-5", "ENG", "2026-01-01T00:00:00Z")],
    )
    .await;
    mount_issues(&server, "OPS", vec![]).await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    run_sync_with(&client, &Config::default(), &mut db, &args(None))
        .await
        .expect("first");

    server.reset().await;
    mount_teams(&server, &["ENG", "OPS"]).await;
    mount_issues(&server, "ENG", vec![]).await;
    mount_issues(
        &server,
        "OPS",
        vec![team_node("uuid-m", "OPS-12", "OPS", "2026-01-02T00:00:00Z")],
    )
    .await;
    let second = run_sync_with(&client, &Config::default(), &mut db, &args(None))
        .await
        .expect("second");

    assert_eq!(second[1].counts.moved, 1);
    let (n, identifier, team_key): (i64, String, String) = db
        .connection()
        .query_row(
            "SELECT COUNT(*), MAX(identifier), MAX(team_key) FROM linear_issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(
        (n, identifier.as_str(), team_key.as_str()),
        (1, "OPS-12", "OPS")
    );
}

/// #190 acceptance 3: `work_items.raw_json` holds Linear's node, so the
/// effort extractor reads its `estimate` and `parent`. With tga's flat
/// struct stored there, both came back `None`.
#[tokio::test]
async fn raw_json_estimate_reaches_the_effort_extractor() {
    let server = MockServer::start().await;
    mount_issues(
        &server,
        "ENG",
        vec![node("ENG-7", "2026-01-01T00:01:00.000Z", None)],
    )
    .await;
    let mut db = Database::open_in_memory().expect("db");
    run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args(Some("ENG")),
    )
    .await
    .expect("sync");

    let raw: String = db
        .connection()
        .query_row(
            "SELECT raw_json FROM work_items WHERE id = 'ENG-7' AND source = 'linear'",
            [],
            |r| r.get(0),
        )
        .expect("work item");
    let fields = tga::core::pm_effort::extract::extract_fields(Some(&raw));
    assert_eq!(fields.story_points, Some(3.0));
    assert_eq!(fields.parent_key.as_deref(), Some("ENG-1"));
}

/// #190 (Fail-Open): a `work_items` write that fails after the issue rows are
/// staged must not leave issues that the next sync classifies `Unchanged` and
/// never projects. With two transactions the issue rows committed first, so
/// the re-sync reported "2 unchanged" and `work_items` stayed empty.
#[tokio::test]
async fn failed_work_items_write_is_repaired_by_the_next_sync() {
    let server = MockServer::start().await;
    mount_issues(
        &server,
        "ENG",
        vec![
            node("ENG-1", "2026-01-01T00:01:00.000Z", None),
            node("ENG-2", "2026-01-01T00:02:00.000Z", None),
        ],
    )
    .await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    db.connection()
        .execute_batch(
            "CREATE TEMP TRIGGER fail_work_items BEFORE INSERT ON work_items \
             BEGIN SELECT RAISE(ABORT, 'injected work_items failure'); END;",
        )
        .expect("install failing trigger");

    let err = run_sync_with(&client, &Config::default(), &mut db, &args(Some("ENG")))
        .await
        .expect_err("a failed work_items write must fail the sync");
    assert!(format!("{err:#}").contains("injected"), "{err:#}");
    let issues_after_failure = count(&db, "SELECT COUNT(*) FROM linear_issues");

    db.connection()
        .execute_batch("DROP TRIGGER fail_work_items;")
        .expect("drop trigger");
    let second = run_sync_with(&client, &Config::default(), &mut db, &args(Some("ENG")))
        .await
        .expect("re-sync");

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM work_items WHERE source = 'linear'"
        ),
        2,
        "the re-sync must write the work_items rows the failed sync lost"
    );
    assert_eq!(second[0].counts.new, 2, "{:?}", second[0].counts);
    assert_eq!(
        issues_after_failure, 0,
        "the failed sync must roll its linear_issues rows back"
    );
}

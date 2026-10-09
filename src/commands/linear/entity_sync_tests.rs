//! `tga linear sync --entities` against a mock Linear (#190): row counts,
//! project dates, the empty cycle, idempotence, and the mid-walk failure.
//! Hand-built GraphQL pages from `entities::tests::workspace`; no network.

use super::entity_sync::sync_entities;
use super::*;
use tga::collect::linear::bulk::tests::{mock_client, page_response};
use tga::collect::linear::entities::store::get_entity_sync_state;
use tga::collect::linear::entities::tests::{
    entity_page, mount_workspace, project, root_call, workspace,
};
use tga::collect::linear::EntityKind;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

async fn synced_db() -> Database {
    let server = MockServer::start().await;
    mount_workspace(&server).await;
    let mut db = Database::open_in_memory().expect("db");
    sync_entities(&mock_client(&server.uri()), &mut db, false)
        .await
        .expect("entity sync");
    db
}

/// #190: every table holds exactly the fixture's rows, archived ones
/// included, and every set records its sync state.
#[tokio::test]
async fn entity_sync_row_counts_match_the_fixture() {
    let db = synced_db().await;
    for (kind, nodes) in workspace() {
        let table = kind.table();
        assert_eq!(
            count(&db, &format!("SELECT COUNT(*) FROM {table}")),
            nodes.len() as i64,
            "{table}"
        );
        assert!(
            count(
                &db,
                &format!("SELECT COUNT(*) FROM {table} WHERE archived = 1")
            ) >= 1,
            "{table}: archived rows stored"
        );
        let state = get_entity_sync_state(db.connection(), kind)
            .expect("read")
            .expect("state row");
        assert_eq!(state.rows_synced, nodes.len() as i64, "{table}");
    }
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM linear_labels WHERE team_id IS NULL"
        ),
        1,
        "one workspace-level label"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM linear_users WHERE active = 0"),
        1
    );
    let estimation: String = db
        .connection()
        .query_row(
            "SELECT issue_estimation_type FROM linear_teams WHERE key = 'ENG'",
            [],
            |r| r.get(0),
        )
        .expect("ENG");
    assert_eq!(estimation, "fibonacci");
}

/// #190: a project's dates, lead and team ids round-trip.
#[tokio::test]
async fn project_dates_round_trip() {
    let db = synced_db().await;
    let row: (String, String, String, String, String, String) = db
        .connection()
        .query_row(
            "SELECT state, start_date, target_date, completed_at, lead_id, team_ids \
             FROM linear_projects WHERE id = 'proj-alpha'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .expect("proj-alpha");
    assert_eq!(
        row,
        (
            "completed".into(),
            "2026-01-05".into(),
            "2026-02-27".into(),
            "2026-02-26T17:30:00.000Z".into(),
            "user-ada".into(),
            r#"["team-eng","team-ops"]"#.into(),
        )
    );
}

/// #190: a cycle with zero scope is stored and flagged empty; a cycle with
/// no history yet is unknown, not empty.
#[tokio::test]
async fn zero_scope_cycle_is_stored_and_flagged_empty() {
    let db = synced_db().await;
    let cycle = |id: &str| -> (Option<i64>, Option<i64>, Option<i64>) {
        db.connection()
            .query_row(
                "SELECT scope_count, completed_count, is_empty FROM linear_cycles WHERE id = ?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect(id)
    };
    assert_eq!(cycle("cyc-ops-1"), (Some(0), Some(0), Some(1)));
    assert_eq!(cycle("cyc-eng-1"), (Some(4), Some(4), Some(0)));
    assert_eq!(cycle("cyc-eng-2"), (None, None, None));
}

/// #190: a second sync of the same workspace leaves every row count as is.
#[tokio::test]
async fn rerun_is_idempotent() {
    let server = MockServer::start().await;
    mount_workspace(&server).await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    sync_entities(&client, &mut db, false).await.expect("first");
    sync_entities(&client, &mut db, false)
        .await
        .expect("second");
    for (kind, nodes) in workspace() {
        assert_eq!(
            count(&db, &format!("SELECT COUNT(*) FROM {}", kind.table())),
            nodes.len() as i64,
            "{}",
            kind.table()
        );
    }
}

/// #190 error arm: page 1 of a second projects walk carries a new project,
/// page 2 fails. Nothing from page 1 is written and the projects sync state
/// is untouched. A writer that commits per page would store `proj-new`.
#[tokio::test]
async fn mid_pagination_failure_writes_nothing_and_keeps_the_state() {
    let mut db = synced_db().await;
    let before = get_entity_sync_state(db.connection(), EntityKind::Projects)
        .expect("read")
        .expect("state after the first sync");

    let server = MockServer::start().await;
    let projects = root_call(EntityKind::Projects);
    let mut fresh = project("proj-new", &["team-eng"], false);
    fresh["updatedAt"] = serde_json::json!("2026-09-01T00:00:00.000Z");
    Mock::given(method("POST"))
        .and(body_string_contains(projects.clone()))
        .and(body_string_contains("\"after\":null"))
        .respond_with(entity_page(
            EntityKind::Projects,
            &[fresh],
            true,
            Some("c1"),
        ))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains(projects))
        .and(body_string_contains("\"after\":\"c1\""))
        .respond_with(ResponseTemplate::new(500).set_body_string("upstream failure"))
        .with_priority(1)
        .mount(&server)
        .await;
    mount_workspace(&server).await;

    let err = sync_entities(&mock_client(&server.uri()), &mut db, false)
        .await
        .expect_err("a failed page must fail the pass");
    let text = format!("{err:#}");
    assert!(text.contains("syncing Linear projects"), "{text}");
    assert!(text.contains("HTTP 500"), "{text}");

    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM linear_projects WHERE id = 'proj-new'"
        ),
        0,
        "page 1 must not be written"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_projects"), 2);
    let after = get_entity_sync_state(db.connection(), EntityKind::Projects)
        .expect("read")
        .expect("state");
    assert_eq!(after, before, "the projects state must not move");
}

/// #190: `--entities` runs after the issues through `tga linear sync`;
/// `--dry-run` fetches and writes nothing.
#[tokio::test]
async fn dry_run_then_real_run_through_the_sync_command() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .respond_with(page_response(vec![], false, None))
        .mount(&server)
        .await;
    mount_workspace(&server).await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");
    let mut args = LinearSyncArgs {
        team: Some("ENG".into()),
        entities: true,
        dry_run: true,
        ..Default::default()
    };
    run_sync_with(&client, &Config::default(), &mut db, &args)
        .await
        .expect("dry run");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_projects"), 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM linear_entity_sync_state"),
        0
    );

    args.dry_run = false;
    run_sync_with(&client, &Config::default(), &mut db, &args)
        .await
        .expect("real run");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_projects"), 2);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_cycles"), 4);
}

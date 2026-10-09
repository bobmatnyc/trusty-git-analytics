//! Tests for `collect::linear::activity` (#190 step 6): the per-issue walk,
//! the due-issue selection, the row writer and migration v36. Every server is
//! a `wiremock::MockServer` answering hand-built GraphQL pages; no network.

use super::store::{activity_candidates, commit_issue_activity};
use super::*;
use crate::collect::linear::bulk::tests::{mock_client, node};
use crate::collect::linear::issue::parse_issue_node;
use crate::collect::linear::upsert_linear_issues;
use crate::core::db::Database;
use serde_json::{json, Value as Json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockBuilder, MockServer, ResponseTemplate};

/// A history entry; `to` = `None` is an entry that did not change the state.
pub(crate) fn history_node(id: &str, at: &str, from: Option<&str>, to: Option<&str>) -> Json {
    let state = |s: Option<&str>| {
        s.map_or(
            Json::Null,
            |n| json!({"name": n, "type": if n == "Done" { "completed" } else { "started" }}),
        )
    };
    json!({"id": id, "createdAt": at, "actor": {"id": "user-1", "name": "Ada Lovelace"},
           "fromState": state(from), "toState": state(to)})
}

/// A comment with `body`.
pub(crate) fn comment_node(id: &str, at: &str, body: &str, parent: Option<&str>) -> Json {
    json!({"id": id, "createdAt": at, "updatedAt": at, "body": body,
           "user": {"id": "user-2", "name": "Grace Hopper"},
           "parent": parent.map(|p| json!({"id": p}))})
}

/// One page of `kind` for an issue.
pub(crate) fn activity_page(
    kind: ActivityKind,
    nodes: &[Json],
    has_next: bool,
    cursor: Option<&str>,
) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": {"issue": {kind.connection(): {
            "nodes": nodes,
            "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
        }}}
    }))
}

/// Matches `kind`'s request for the issue with id `issue_id`.
pub(crate) fn activity_call(kind: ActivityKind, issue_id: &str) -> MockBuilder {
    Mock::given(method("POST"))
        .and(body_string_contains(format!(
            "{}(first: $first",
            kind.connection()
        )))
        .and(body_string_contains(format!("\"id\":\"{issue_id}\"")))
}

fn target(n: u32) -> ActivityTarget {
    ActivityTarget::new(
        &format!("uuid-ENG-{n}"),
        &format!("ENG-{n}"),
        "ENG",
        Some("2026-01-01T00:00:00+00:00".into()),
    )
}

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

/// #190 step 6: a two-page history walk returns both pages, asks for
/// archived entries, and queries the issue by its id.
#[tokio::test]
async fn history_walk_follows_every_page() {
    let server = MockServer::start().await;
    activity_call(ActivityKind::History, "uuid-ENG-1")
        .and(body_string_contains("\"after\":null"))
        .and(body_string_contains("\"includeArchived\":true"))
        .respond_with(activity_page(
            ActivityKind::History,
            &[history_node(
                "h1",
                "2026-01-01T00:00:00.000Z",
                None,
                Some("Todo"),
            )],
            true,
            Some("c-1"),
        ))
        .mount(&server)
        .await;
    activity_call(ActivityKind::History, "uuid-ENG-1")
        .and(body_string_contains("\"after\":\"c-1\""))
        .respond_with(activity_page(
            ActivityKind::History,
            &[history_node(
                "h2",
                "2026-01-02T00:00:00.000Z",
                Some("Todo"),
                Some("Done"),
            )],
            false,
            None,
        ))
        .mount(&server)
        .await;
    let nodes = mock_client(&server.uri())
        .fetch_issue_activity(&target(1), ActivityKind::History)
        .await
        .expect("walk");
    let ids: Vec<&str> = nodes.iter().filter_map(|n| n["id"].as_str()).collect();
    assert_eq!(ids, vec!["h1", "h2"]);
}

/// #190 step 6: the comments walk pages the same way.
#[tokio::test]
async fn comments_walk_follows_every_page() {
    let server = MockServer::start().await;
    activity_call(ActivityKind::Comments, "uuid-ENG-1")
        .and(body_string_contains("\"after\":null"))
        .respond_with(activity_page(
            ActivityKind::Comments,
            &[comment_node("c1", "2026-01-01T00:00:00.000Z", "one", None)],
            true,
            Some("p-1"),
        ))
        .mount(&server)
        .await;
    activity_call(ActivityKind::Comments, "uuid-ENG-1")
        .and(body_string_contains("\"after\":\"p-1\""))
        .respond_with(activity_page(
            ActivityKind::Comments,
            &[comment_node(
                "c2",
                "2026-01-02T00:00:00.000Z",
                "two",
                Some("c1"),
            )],
            false,
            None,
        ))
        .mount(&server)
        .await;
    let nodes = mock_client(&server.uri())
        .fetch_issue_activity(&target(1), ActivityKind::Comments)
        .await
        .expect("walk");
    assert_eq!(nodes.len(), 2);
}

/// #190 step 6 (Fail-Open check): a server error on page 2 fails the walk
/// with the issue, the connection and the page; page 1 is not returned.
#[tokio::test]
async fn a_page_error_fails_the_walk() {
    let server = MockServer::start().await;
    activity_call(ActivityKind::History, "uuid-ENG-1")
        .and(body_string_contains("\"after\":null"))
        .respond_with(activity_page(
            ActivityKind::History,
            &[history_node(
                "h1",
                "2026-01-01T00:00:00.000Z",
                None,
                Some("Todo"),
            )],
            true,
            Some("c-1"),
        ))
        .mount(&server)
        .await;
    activity_call(ActivityKind::History, "uuid-ENG-1")
        .and(body_string_contains("\"after\":\"c-1\""))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_issue_activity(&target(1), ActivityKind::History)
        .await
        .expect_err("page 2 failed");
    match err {
        CollectError::LinearActivityApi {
            status,
            identifier,
            connection,
            page,
            ..
        } => {
            assert_eq!(
                (status, identifier.as_str(), connection, page),
                (500, "ENG-1", "history", 2)
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

/// #190 step 6: `issue: null` (a deleted issue, or no access) is an error,
/// never an empty history.
#[tokio::test]
async fn a_missing_issue_fails_the_walk() {
    let server = MockServer::start().await;
    activity_call(ActivityKind::Comments, "uuid-ENG-1")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"issue": null}})))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_issue_activity(&target(1), ActivityKind::Comments)
        .await
        .expect_err("no issue");
    assert!(
        matches!(
            err,
            CollectError::LinearActivityApi {
                connection: "comments",
                ..
            }
        ),
        "{err:?}"
    );
}

/// #190 step 6: a page with no boolean `hasNextPage` fails the walk rather
/// than reading as the last page.
#[tokio::test]
async fn a_page_without_page_info_fails_the_walk() {
    let server = MockServer::start().await;
    activity_call(ActivityKind::History, "uuid-ENG-1")
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": {"issue": {"history": {"nodes": [
                history_node("h1", "2026-01-01T00:00:00.000Z", None, Some("Todo"))
            ]}}}
        })))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_issue_activity(&target(1), ActivityKind::History)
        .await
        .expect_err("no pageInfo");
    assert!(
        matches!(err, CollectError::LinearPageInfoInvalid { .. }),
        "{err:?}"
    );
}

/// Store `ENG-1..=n` (and one OPS issue) and return the database.
fn stored(n: u32) -> Database {
    let db = Database::open_in_memory().expect("db");
    let mut issues: Vec<_> = (1..=n)
        .map(|i| {
            parse_issue_node(
                "",
                &node(&format!("ENG-{i}"), "2026-01-01T00:00:00.000Z", None),
            )
        })
        .collect();
    let mut ops = node("OPS-1", "2026-01-01T00:00:00.000Z", None);
    ops["team"]["key"] = json!("OPS");
    issues.push(parse_issue_node("", &ops));
    upsert_linear_issues(&db, &issues).expect("store");
    db
}

fn ids(targets: &[ActivityTarget]) -> Vec<&str> {
    targets.iter().map(|t| t.identifier.as_str()).collect()
}

/// #190 step 6: the due issues are the team's rows whose marker differs from
/// their stored `updated_at`; a write makes an issue current per kind, a
/// changed issue is due again, and `force` returns every row.
#[test]
fn candidates_are_the_rows_whose_marker_lags() {
    let mut db = stored(2);
    let due = activity_candidates(db.connection(), "ENG", ActivityKind::History, false, &[])
        .expect("due");
    assert_eq!(ids(&due), vec!["ENG-1", "ENG-2"]);

    commit_issue_activity(&mut db, &due[0], ActivityKind::History, &[]).expect("write");
    let due = activity_candidates(db.connection(), "ENG", ActivityKind::History, false, &[])
        .expect("due");
    assert_eq!(ids(&due), vec!["ENG-2"]);
    // The comments marker is separate.
    let due = activity_candidates(db.connection(), "ENG", ActivityKind::Comments, false, &[])
        .expect("due");
    assert_eq!(ids(&due), vec!["ENG-1", "ENG-2"]);

    db.connection()
        .execute(
            "UPDATE linear_issues SET updated_at = '2026-02-01T00:00:00+00:00' \
             WHERE identifier = 'ENG-1'",
            [],
        )
        .expect("touch");
    let due = activity_candidates(db.connection(), "ENG", ActivityKind::History, false, &[])
        .expect("due");
    assert_eq!(ids(&due), vec!["ENG-1", "ENG-2"]);
    assert_eq!(
        due[0].updated_at.as_deref(),
        Some("2026-02-01T00:00:00+00:00")
    );

    let all = activity_candidates(db.connection(), "ENG", ActivityKind::History, true, &[])
        .expect("forced");
    assert_eq!(ids(&all), vec!["ENG-1", "ENG-2"]);
}

/// #190 step 6: the dry run's fetched issues count as due when no stored
/// row is current for them.
#[test]
fn overlay_counts_fetched_but_unstored_issues() {
    let db = stored(1);
    let fresh = parse_issue_node("", &node("ENG-9", "2026-01-03T00:00:00.000Z", None));
    let due = activity_candidates(
        db.connection(),
        "ENG",
        ActivityKind::Comments,
        false,
        &[fresh],
    )
    .expect("due");
    assert_eq!(ids(&due), vec!["ENG-1", "ENG-9"]);
}

/// #190 step 6: only entries that changed the workflow state become
/// transition rows, with both states, their types, the actor and the time.
#[test]
fn history_rows_hold_state_transitions_only() {
    let mut db = stored(1);
    let nodes = [
        history_node("h1", "2026-01-01T00:00:00.000Z", None, Some("Todo")),
        history_node("h2", "2026-01-01T01:00:00.000Z", None, None),
        history_node("h3", "2026-01-02T00:00:00.000Z", Some("Todo"), Some("Done")),
    ];
    let rows =
        commit_issue_activity(&mut db, &target(1), ActivityKind::History, &nodes).expect("write");
    assert_eq!(rows, 2);
    let row: (
        String,
        String,
        String,
        String,
        String,
        String,
        String,
        String,
    ) = db
        .connection()
        .query_row(
            "SELECT issue_id, identifier, team_key, from_state, to_state, to_state_type, \
             actor_name, transitioned_at FROM fact_linear_transitions WHERE history_id = 'h3'",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .expect("row");
    assert_eq!(
        row,
        (
            "uuid-ENG-1".into(),
            "ENG-1".into(),
            "ENG".into(),
            "Todo".into(),
            "Done".into(),
            "completed".into(),
            "Ada Lovelace".into(),
            "2026-01-02T00:00:00.000Z".into()
        )
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_linear_transitions WHERE from_state IS NULL"
        ),
        1
    );
}

/// #190 step 6: a comment row holds its author, thread parent and body
/// length; the body text is stored nowhere.
#[test]
fn comment_rows_hold_metadata_not_body() {
    let mut db = stored(1);
    let body = "a secret sentence nobody should store";
    let nodes = [
        comment_node("c1", "2026-01-01T00:00:00.000Z", body, None),
        comment_node("c2", "2026-01-02T00:00:00.000Z", "héllo", Some("c1")),
    ];
    let rows =
        commit_issue_activity(&mut db, &target(1), ActivityKind::Comments, &nodes).expect("write");
    assert_eq!(rows, 2);
    let (len, parent, author): (i64, Option<String>, String) = db
        .connection()
        .query_row(
            "SELECT body_len, parent_id, author_name FROM fact_linear_comment_detail \
             WHERE comment_id = 'c2'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(
        (len, parent.as_deref(), author.as_str()),
        (5, Some("c1"), "Grace Hopper")
    );
    let pattern = format!("%{}%", &body[2..8]);
    let hits: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_linear_comment_detail WHERE \
             comment_id LIKE ?1 OR COALESCE(author_name, '') LIKE ?1",
            [pattern],
            |r| r.get(0),
        )
        .expect("scan");
    assert_eq!(hits, 0);
    let columns: Vec<String> = db
        .connection()
        .prepare("SELECT name FROM pragma_table_info('fact_linear_comment_detail')")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("columns");
    assert!(!columns.iter().any(|c| c == "body"), "{columns:?}");
}

/// #190 step 6: a re-read replaces the issue's rows: an identical list keeps
/// the count, and a comment Linear no longer returns is gone.
#[test]
fn rewrite_replaces_rows_in_place() {
    let mut db = stored(1);
    let two = [
        comment_node("c1", "2026-01-01T00:00:00.000Z", "a", None),
        comment_node("c2", "2026-01-02T00:00:00.000Z", "b", None),
    ];
    commit_issue_activity(&mut db, &target(1), ActivityKind::Comments, &two).expect("first");
    commit_issue_activity(&mut db, &target(1), ActivityKind::Comments, &two).expect("again");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        2
    );
    commit_issue_activity(&mut db, &target(1), ActivityKind::Comments, &two[..1])
        .expect("deleted one");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        1
    );
}

/// #190 step 6: a node without an `id` fails the write, and the issue's
/// marker does not move.
#[test]
fn a_node_without_an_id_writes_nothing() {
    let mut db = stored(1);
    let bad = [json!({"createdAt": "2026-01-01T00:00:00.000Z", "body": "x"})];
    commit_issue_activity(&mut db, &target(1), ActivityKind::Comments, &bad).expect_err("no id");
    let due = activity_candidates(db.connection(), "ENG", ActivityKind::Comments, false, &[])
        .expect("due");
    assert_eq!(ids(&due), vec!["ENG-1"]);
}

/// #190 step 6: migration v36 is additive — a v35 database keeps its Linear
/// and JIRA fact rows, gains the three new tables, and stays in WAL mode.
#[test]
fn migration_v36_keeps_v35_rows() {
    use crate::core::db::migrations::{run, run_through};
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("v35.db");
    let mut conn = rusqlite::Connection::open(&path).expect("conn");
    let mode: String = conn
        .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
        .expect("wal");
    assert_eq!(mode, "wal");
    run_through(&mut conn, 35).expect("v35");
    conn.execute_batch(
        "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
         linear_id, updated_at) VALUES ('ENG-1', 'kept', 'Done', 'Eng', 'ENG', 'f', 'uuid-1', 'u'); \
         INSERT INTO fact_ticket_transitions (ticket_key, project_key, to_status, \
         transitioned_at, synced_at) VALUES ('J-1', 'J', 'Done', 't', 1);",
    )
    .expect("seed v35");
    run(&mut conn).expect("migrate");
    let kept: (String, String) = conn
        .query_row(
            "SELECT identifier, title FROM linear_issues WHERE linear_id = 'uuid-1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("linear row");
    assert_eq!(kept, ("ENG-1".into(), "kept".into()));
    let jira: i64 = conn
        .query_row("SELECT COUNT(*) FROM fact_ticket_transitions", [], |r| {
            r.get(0)
        })
        .expect("jira");
    assert_eq!(jira, 1);
    for table in [
        "fact_linear_transitions",
        "fact_linear_comment_detail",
        "linear_issue_activity_state",
    ] {
        let n: i64 = conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .expect(table);
        assert_eq!(n, 0, "{table}");
    }
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("mode");
    assert_eq!(mode, "wal");
}

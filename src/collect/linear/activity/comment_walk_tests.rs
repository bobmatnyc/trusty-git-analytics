//! Tests for the incremental comments walk and its writer (#190): the
//! request shape, paging, the malformed-page arms, and the cursor rules.
//! Every server is a `wiremock::MockServer`; no network.

use chrono::{DateTime, Utc};
use serde_json::{json, Value as Json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::comments::{comments_filter, COMMENTS_PAGE_SIZE};
use super::store::{comment_cursor, commit_team_comments};
use crate::collect::errors::CollectError;
use crate::collect::linear::bulk::tests::{mock_client, node};
use crate::collect::linear::issue::parse_issue_node;
use crate::collect::linear::upsert_linear_issues;
use crate::core::db::Database;

fn at(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("fixture instant")
        .with_timezone(&Utc)
}

/// A workspace comment on `issue` (`None`: a comment on no issue).
fn comment(id: &str, updated: &str, issue: Option<&str>) -> Json {
    json!({"id": id, "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": updated,
           "body": "secret body text", "user": {"id": "u1", "name": "Ada"}, "parent": null,
           "issue": issue.map(|i| json!({"id": format!("uuid-{i}"), "identifier": i,
                                          "team": {"key": "ENG"}}))})
}

fn page(nodes: &[Json], has_next: bool, cursor: Option<&str>) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"data": {"comments": {
        "nodes": nodes, "pageInfo": {"hasNextPage": has_next, "endCursor": cursor}}}}))
}

/// A database holding issue ENG-1 only.
fn with_eng1() -> Database {
    let db = Database::open_in_memory().expect("db");
    let issue = parse_issue_node("", &node("ENG-1", "2026-01-01T00:00:00.000Z", None));
    upsert_linear_issues(&db, &[issue]).expect("store");
    db
}

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

/// #190: the team filter is always sent; the `updatedAt` bound only with a
/// cursor, as a strict `gt` in RFC 3339 milliseconds.
#[test]
fn the_filter_scopes_to_the_team_and_bound() {
    assert_eq!(
        comments_filter("ENG", None),
        json!({"issue": {"team": {"key": {"eq": "ENG"}}}})
    );
    let bound = at("2026-01-01T23:50:00Z");
    assert_eq!(
        comments_filter("ENG", Some(bound)),
        json!({"issue": {"team": {"key": {"eq": "ENG"}}},
               "updatedAt": {"gt": "2026-01-01T23:50:00.000Z"}})
    );
}

/// #190: a two-page walk returns both pages and sends the page size,
/// `includeArchived: true` and `orderBy: createdAt` on every page.
#[tokio::test]
async fn the_walk_follows_every_page() {
    let server = MockServer::start().await;
    let common = |m: wiremock::MockBuilder| {
        m.and(body_string_contains(format!(
            "\"first\":{COMMENTS_PAGE_SIZE}"
        )))
        .and(body_string_contains("\"includeArchived\":true"))
        .and(body_string_contains("\"orderBy\":\"createdAt\""))
    };
    common(Mock::given(method("POST")).and(body_string_contains("\"after\":null")))
        .respond_with(page(
            &[comment("c1", "2026-01-02T00:00:00.000Z", Some("ENG-1"))],
            true,
            Some("p-1"),
        ))
        .mount(&server)
        .await;
    common(Mock::given(method("POST")).and(body_string_contains("\"after\":\"p-1\"")))
        .respond_with(page(
            &[comment("c2", "2026-01-03T00:00:00.000Z", Some("ENG-1"))],
            false,
            None,
        ))
        .mount(&server)
        .await;
    let nodes = mock_client(&server.uri())
        .fetch_team_comments("ENG", None)
        .await
        .expect("walk");
    let ids: Vec<&str> = nodes.iter().filter_map(|n| n["id"].as_str()).collect();
    assert_eq!(ids, vec!["c1", "c2"]);
}

/// #190 (Fail-Open check): a page with no `nodes` array, or no boolean
/// `hasNextPage`, fails the walk; neither reads as an empty last page.
#[tokio::test]
async fn a_malformed_page_fails_the_walk() {
    let cases = [
        (
            "no nodes",
            json!({"data": {"comments": {"pageInfo": {"hasNextPage": false}}}}),
        ),
        ("no pageInfo", json!({"data": {"comments": {"nodes": []}}})),
    ];
    for (name, body) in cases {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let err = mock_client(&server.uri())
            .fetch_team_comments("ENG", None)
            .await
            .expect_err(name);
        assert!(
            matches!(
                err,
                CollectError::LinearBulkApi { .. } | CollectError::LinearPageInfoInvalid { .. }
            ),
            "{name}: {err:?}"
        );
    }
}

/// #190: a comment on an issue not in `linear_issues` is stored and counted;
/// one on no issue is counted and not stored. No body is stored.
#[test]
fn the_write_counts_unlinked_and_issueless_comments() {
    let mut db = with_eng1();
    let nodes = [
        comment("c1", "2026-01-02T00:00:00.000Z", Some("ENG-1")),
        comment("c2", "2026-01-03T00:00:00.000Z", Some("ENG-9")),
        comment("c3", "2026-01-04T00:00:00.000Z", None),
    ];
    let write = commit_team_comments(&mut db, "ENG", &nodes, Utc::now()).expect("write");
    assert_eq!((write.written, write.unlinked, write.no_issue), (2, 1, 1));
    assert_eq!(write.cursor, Some(at("2026-01-03T00:00:00.000Z")));
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        2
    );
    assert_eq!(
        count(
            &db,
            "SELECT body_len FROM fact_linear_comment_detail WHERE comment_id = 'c1'"
        ),
        16
    );
}

/// #190: the cursor is the newest stored `updatedAt`, capped at the walk's
/// start, and never moves backward.
#[test]
fn the_cursor_is_capped_at_the_walk_start_and_never_moves_back() {
    let mut db = with_eng1();
    let started = at("2026-01-02T12:00:00.000Z");
    let late = [comment("c1", "2026-01-03T00:00:00.000Z", Some("ENG-1"))];
    let write = commit_team_comments(&mut db, "ENG", &late, started).expect("write");
    assert_eq!(write.cursor, Some(started));

    let older = [comment("c0", "2026-01-01T00:00:00.000Z", Some("ENG-1"))];
    commit_team_comments(&mut db, "ENG", &older, Utc::now()).expect("write");
    assert_eq!(
        comment_cursor(db.connection(), "ENG").expect("read"),
        Some(started)
    );
}

/// #190: a node without `updatedAt` fails the write; nothing from the walk
/// is stored and the cursor stays unset.
#[test]
fn a_malformed_node_writes_nothing() {
    let mut db = with_eng1();
    let mut bad = comment("c2", "2026-01-03T00:00:00.000Z", Some("ENG-1"));
    bad["updatedAt"] = Json::Null;
    let nodes = [
        comment("c1", "2026-01-02T00:00:00.000Z", Some("ENG-1")),
        bad,
    ];
    commit_team_comments(&mut db, "ENG", &nodes, Utc::now()).expect_err("no updatedAt");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM fact_linear_comment_detail"),
        0
    );
    assert_eq!(comment_cursor(db.connection(), "ENG").expect("read"), None);
}

/// #190: a moved issue's comment rows take its current identifier and team.
#[test]
fn a_moved_issue_s_comment_rows_follow_it() {
    let mut db = with_eng1();
    let nodes = [comment("c1", "2026-01-02T00:00:00.000Z", Some("ENG-1"))];
    commit_team_comments(&mut db, "ENG", &nodes, Utc::now()).expect("write");
    db.connection()
        .execute(
            "UPDATE linear_issues SET identifier = 'OPS-7', team_key = 'OPS' \
             WHERE identifier = 'ENG-1'",
            [],
        )
        .expect("move");
    commit_team_comments(&mut db, "OPS", &[], Utc::now()).expect("refresh");
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM fact_linear_comment_detail \
             WHERE identifier = 'OPS-7' AND team_key = 'OPS'"
        ),
        1
    );
}

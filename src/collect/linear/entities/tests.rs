//! Tests for `collect::linear::entities` (#190), and the multi-team
//! workspace fixture the command tests share. Every server is a
//! `wiremock::MockServer` answering hand-built GraphQL pages; no network.

use super::*;
use crate::collect::linear::bulk::tests::mock_client;
use serde_json::{json, Value as Json};
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The text that opens `kind`'s root call in [`queries::query_for`]. Nested
/// connections (`teams(first: 100`) never match it.
pub(crate) fn root_call(kind: EntityKind) -> String {
    format!(
        "{}(first: $first, after: $after, includeArchived: $includeArchived",
        kind.root_field()
    )
}

/// One page of `kind`'s connection.
pub(crate) fn entity_page(
    kind: EntityKind,
    nodes: &[Json],
    has_next: bool,
    cursor: Option<&str>,
) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "data": { kind.root_field(): {
            "nodes": nodes,
            "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
        }}
    }))
}

const ARCHIVED_AT: &str = "2026-03-01T00:00:00.000Z";

fn base(id: &str, updated: &str, archived: bool) -> Json {
    json!({
        "id": id,
        "createdAt": "2026-01-01T00:00:00.000Z",
        "updatedAt": updated,
        "archivedAt": if archived { json!(ARCHIVED_AT) } else { Json::Null },
    })
}

fn with(mut node: Json, fields: Json) -> Json {
    if let (Some(n), Some(f)) = (node.as_object_mut(), fields.as_object()) {
        for (k, v) in f {
            n.insert(k.clone(), v.clone());
        }
    }
    node
}

fn team(id: &str, key: &str, archived: bool) -> Json {
    with(
        base(id, "2026-02-01T00:00:00.000Z", archived),
        json!({"key": key, "name": format!("{key} team"), "private": false,
               "timezone": "UTC", "issueEstimationType": "fibonacci",
               "issueEstimationAllowZero": true, "issueEstimationExtended": false,
               "defaultIssueEstimate": 1.0, "cyclesEnabled": true, "cycleDuration": 2.0,
               "cycleCooldownTime": 0.0, "cycleStartDay": 1.0, "upcomingCycleCount": 2.0}),
    )
}

fn user(id: &str, name: &str, active: bool, archived: bool) -> Json {
    with(
        base(id, "2026-02-02T00:00:00.000Z", archived),
        json!({"name": name, "displayName": name.to_lowercase(),
               "email": format!("{}@acme.example", name.to_lowercase()),
               "active": active, "admin": false, "guest": false}),
    )
}

fn label(id: &str, team: Option<&str>, parent: Option<&str>, group: bool, archived: bool) -> Json {
    with(
        base(id, "2026-02-03T00:00:00.000Z", archived),
        json!({"name": id, "color": "#5e6ad2", "isGroup": group,
               "parent": parent.map(|p| json!({"id": p})),
               "team": team.map(|t| json!({"id": t}))}),
    )
}

/// A project with `teams`, target date and completion time.
pub(crate) fn project(id: &str, teams: &[&str], archived: bool) -> Json {
    let nodes: Vec<Json> = teams.iter().map(|t| json!({"id": t})).collect();
    with(
        base(id, "2026-02-04T00:00:00.000Z", archived),
        json!({"name": id, "slugId": format!("{id}-slug"),
               "url": format!("https://linear.app/acme/project/{id}"),
               "state": "completed", "status": {"name": "Completed", "type": "completed"},
               "progress": 1.0, "startDate": "2026-01-05", "targetDate": "2026-02-27",
               "startedAt": "2026-01-05T09:00:00.000Z",
               "completedAt": "2026-02-26T17:30:00.000Z", "canceledAt": null,
               "lead": {"id": "user-ada"},
               "teams": {"nodes": nodes, "pageInfo": {"hasNextPage": false}}}),
    )
}

fn milestone(id: &str, project: &str, archived: bool) -> Json {
    with(
        base(id, "2026-02-05T00:00:00.000Z", archived),
        json!({"name": id, "targetDate": "2026-02-15", "sortOrder": 1.0,
               "project": {"id": project}}),
    )
}

fn cycle(id: &str, team: &str, number: f64, scope: &[f64], done: &[f64], archived: bool) -> Json {
    with(
        base(id, "2026-02-06T00:00:00.000Z", archived),
        json!({"number": number, "name": null,
               "startsAt": "2026-01-05T00:00:00.000Z", "endsAt": "2026-01-19T00:00:00.000Z",
               "completedAt": "2026-01-19T00:00:00.000Z", "progress": 0.5,
               "team": {"id": team},
               "issueCountHistory": scope, "completedIssueCountHistory": done,
               "scopeHistory": scope, "completedScopeHistory": done}),
    )
}

/// A small two-team workspace (`ENG`, `OPS`, plus an archived `ARC`). Every
/// set holds at least one archived node.
pub(crate) fn workspace() -> Vec<(EntityKind, Vec<Json>)> {
    vec![
        (
            EntityKind::Teams,
            vec![
                team("team-eng", "ENG", false),
                team("team-ops", "OPS", false),
                team("team-arc", "ARC", true),
            ],
        ),
        (
            EntityKind::Users,
            vec![
                user("user-ada", "Ada", true, false),
                user("user-bo", "Bo", true, false),
                user("user-cy", "Cy", false, true),
            ],
        ),
        (
            EntityKind::Labels,
            vec![
                label("label-bug", None, None, false, false),
                label("label-area", Some("team-eng"), None, true, false),
                label(
                    "label-area-api",
                    Some("team-eng"),
                    Some("label-area"),
                    false,
                    false,
                ),
                label("label-ops-old", Some("team-ops"), None, false, true),
            ],
        ),
        (
            EntityKind::Projects,
            vec![
                project("proj-alpha", &["team-eng", "team-ops"], false),
                project("proj-old", &["team-ops"], true),
            ],
        ),
        (
            EntityKind::Milestones,
            vec![
                milestone("ms-beta", "proj-alpha", false),
                milestone("ms-old", "proj-old", true),
            ],
        ),
        (
            EntityKind::Cycles,
            vec![
                cycle(
                    "cyc-eng-1",
                    "team-eng",
                    1.0,
                    &[3.0, 4.0, 4.0],
                    &[0.0, 2.0, 4.0],
                    false,
                ),
                cycle(
                    "cyc-ops-1",
                    "team-ops",
                    1.0,
                    &[0.0, 0.0, 0.0],
                    &[0.0, 0.0, 0.0],
                    false,
                ),
                cycle("cyc-eng-2", "team-eng", 2.0, &[], &[], false),
                cycle("cyc-eng-0", "team-eng", 0.0, &[2.0], &[2.0], true),
            ],
        ),
    ]
}

/// Answer every entity query with its [`workspace`] set, on one page.
pub(crate) async fn mount_workspace(server: &MockServer) {
    for (kind, nodes) in workspace() {
        Mock::given(method("POST"))
            .and(body_string_contains(root_call(kind)))
            .respond_with(entity_page(kind, &nodes, false, None))
            .mount(server)
            .await;
    }
}

/// #190 negative control: each mock answers like Linear — archived nodes
/// only when the root call carries `includeArchived` and the variable is
/// `true`. Dropping it from any one query cuts that set's archived nodes,
/// and this test fails.
#[tokio::test]
async fn every_entity_query_sends_include_archived() {
    let server = MockServer::start().await;
    for (kind, nodes) in workspace() {
        let active: Vec<Json> = nodes
            .iter()
            .filter(|n| !is_archived_node(n))
            .cloned()
            .collect();
        Mock::given(method("POST"))
            .and(body_string_contains(root_call(kind)))
            .and(body_string_contains("\"includeArchived\":true"))
            .respond_with(entity_page(kind, &nodes, false, None))
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains(format!(
                "{}(first: $first",
                kind.root_field()
            )))
            .respond_with(entity_page(kind, &active, false, None))
            .with_priority(2)
            .mount(&server)
            .await;
    }
    let client = mock_client(&server.uri());
    for (kind, expected) in workspace() {
        let nodes = client.fetch_entity_nodes(kind).await.expect("walk");
        assert_eq!(nodes.len(), expected.len(), "{kind:?}: every node");
        assert!(
            nodes.iter().any(is_archived_node),
            "{kind:?}: archived nodes must arrive"
        );
    }
}

/// A two-page walk returns both pages, in order.
#[tokio::test]
async fn fetch_walks_every_page() {
    let server = MockServer::start().await;
    let kind = EntityKind::Projects;
    Mock::given(method("POST"))
        .and(body_string_contains("\"after\":null"))
        .respond_with(entity_page(
            kind,
            &[project("p1", &["team-eng"], false)],
            true,
            Some("c1"),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"after\":\"c1\""))
        .respond_with(entity_page(kind, &[project("p2", &[], true)], false, None))
        .mount(&server)
        .await;
    let nodes = mock_client(&server.uri())
        .fetch_entity_nodes(kind)
        .await
        .expect("walk");
    let ids: Vec<&str> = nodes.iter().filter_map(|n| n["id"].as_str()).collect();
    assert_eq!(ids, vec!["p1", "p2"]);
}

/// A project whose nested `teams` page says more follow fails the walk;
/// storing the first page as the whole list would be a silent cut.
#[tokio::test]
async fn a_cut_nested_team_list_fails_the_walk() {
    let server = MockServer::start().await;
    let mut p = project("p1", &["team-eng"], false);
    p["teams"]["pageInfo"]["hasNextPage"] = json!(true);
    Mock::given(method("POST"))
        .respond_with(entity_page(EntityKind::Projects, &[p], false, None))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_entity_nodes(EntityKind::Projects)
        .await
        .expect_err("a cut nested list must fail");
    assert!(
        matches!(err, CollectError::LinearNestedConnectionTruncated { ref id, connection: "teams", .. } if id == "p1"),
        "{err:?}"
    );
}

/// A 200 with no `nodes` array is an error, not an empty set.
#[tokio::test]
async fn a_response_without_nodes_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"cycles": null}})))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_entity_nodes(EntityKind::Cycles)
        .await
        .expect_err("no nodes must not read as an empty set");
    assert!(
        matches!(
            err,
            CollectError::LinearEntityApi {
                entity: "cycles",
                page: 1,
                ..
            }
        ),
        "{err:?}"
    );
}

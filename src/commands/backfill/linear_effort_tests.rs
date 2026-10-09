//! #190 step 3 acceptance: a Linear issue synced by `tga linear sync` reaches
//! `fact_pm_effort` with its estimate as story points, and its parent's row
//! counts it as a child. Mock Linear, no network.

use chrono::{TimeZone, Utc};
use tga::collect::linear::bulk::tests::{mock_client, page_response};
use tga::collect::linear::issue::tests::full_node;
use tga::core::config::Config;
use tga::core::db::Database;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer};

use super::pm_effort::backfill_pm_effort_at;
use super::pm_work::backfill_pm_work;
use crate::commands::linear::{run_sync_with, LinearSyncArgs};

/// An ENG issue node; `description` is what Linear returns when asked.
fn issue_node(id: &str, identifier: &str, parent: Option<&str>) -> serde_json::Value {
    let mut node = full_node(id, identifier, "ENG");
    node["description"] = serde_json::json!("Ship the importer behind a flag.");
    node["parent"] = match parent {
        Some(p) => serde_json::json!({"id": format!("uuid-{p}"), "identifier": p}),
        None => serde_json::Value::Null,
    };
    node
}

/// The same node without `description`, which is what Linear returns to a
/// query that does not select the field.
fn without_description(mut node: serde_json::Value) -> serde_json::Value {
    if let Some(map) = node.as_object_mut() {
        map.remove("description");
    }
    node
}

/// `(story_points, epic_children_count)` of one `fact_pm_effort` row.
fn effort(db: &Database, id: &str) -> Option<(Option<f64>, i64)> {
    db.connection()
        .query_row(
            "SELECT story_points, epic_children_count FROM fact_pm_effort \
             WHERE work_item_id = ?1 AND work_item_source = 'linear'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
}

/// #190 step 3 acceptance: titles of three words are terse, so only the
/// description makes these issues meaningful. With the description missing
/// from the query, `fact_pm_work` excluded both and `fact_pm_effort` stayed
/// empty.
#[tokio::test]
async fn synced_linear_issues_reach_fact_pm_effort() {
    let server = MockServer::start().await;
    let nodes = vec![
        issue_node("uuid-ENG-1", "ENG-1", None),
        issue_node("uuid-ENG-7", "ENG-7", Some("ENG-1")),
    ];
    Mock::given(method("POST"))
        .and(body_string_contains(" description "))
        .respond_with(page_response(nodes.clone(), false, None))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(page_response(
            nodes.into_iter().map(without_description).collect(),
            false,
            None,
        ))
        .with_priority(2)
        .mount(&server)
        .await;

    let mut db = Database::open_in_memory().expect("db");
    let args = LinearSyncArgs {
        team: Some("ENG".to_string()),
        ..Default::default()
    };
    run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args,
    )
    .await
    .expect("sync");
    backfill_pm_work(&mut db, false).expect("pm-work");
    let scored_at = Utc
        .with_ymd_and_hms(2026, 6, 1, 0, 0, 0)
        .single()
        .expect("instant");
    backfill_pm_effort_at(&mut db, false, scored_at).expect("pm-effort");

    assert_eq!(
        effort(&db, "ENG-7"),
        Some((Some(3.0), 0)),
        "the child is scored with its estimate as story points"
    );
    assert_eq!(
        effort(&db, "ENG-1"),
        Some((Some(3.0), 1)),
        "the parent counts the child"
    );
}

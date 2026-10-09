//! The bulk Linear sync inside `tga collect` (#190 step 5) against a mock
//! Linear: off makes no call; on syncs the configured team and the
//! entities; a failure lands on the run's faults under the #146 (D28)
//! contract. No network.

use std::time::Duration;

use tga::collect::jira::retry::RetryPolicy;
use tga::collect::linear::bulk::tests::{mock_client, node, page_response};
use tga::collect::linear::entities::tests::mount_workspace;
use tga::collect::FaultSeverity;
use tga::core::config::LinearConfig;
use wiremock::matchers::{any, body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

fn config(linear_yaml: &str) -> Config {
    let linear: LinearConfig = serde_yaml::from_str(linear_yaml).expect("linear block parses");
    Config {
        linear: Some(linear),
        ..Default::default()
    }
}

/// `team_keys: [ENG]` with `sync_on_collect: true`.
fn enabled() -> Config {
    config("api_key: test-key\nteam_keys: [ENG]\nsync_on_collect: true\n")
}

/// Retries a 503 once, without the default policy's seconds of backoff.
fn fast_client(server: &MockServer) -> LinearClient {
    mock_client(&server.uri()).with_retry_policy(RetryPolicy {
        max_attempts: 2,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(1),
        max_total_delay: Duration::from_millis(10),
    })
}

fn count(db: &Database, sql: &str) -> i64 {
    db.connection()
        .query_row(sql, [], |r| r.get(0))
        .expect("count")
}

/// D8f: the sync is opt-in. With the flag off — or absent — collect makes no
/// Linear request at all, even with a key and a team configured.
#[tokio::test]
async fn sync_on_collect_off_makes_no_linear_call() {
    for yaml in [
        "api_key: test-key\nteam_keys: [ENG]\n",
        "api_key: test-key\nteam_keys: [ENG]\nsync_on_collect: false\n",
    ] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let mut db = Database::open_in_memory().expect("db");
        let mut stats = CollectionStats::default();
        sync_with(
            &fast_client(&server),
            &config(yaml),
            &mut db,
            false,
            &mut stats,
        )
        .await;
        assert!(stats.errors.is_empty(), "{yaml}: {:?}", stats.errors);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 0);
        server.verify().await;
    }
}

/// On: the configured team's issues and the reference entities land, and
/// the run records no fault.
#[tokio::test]
async fn sync_on_collect_syncs_the_configured_team_and_the_entities() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .respond_with(page_response(
            vec![
                node("ENG-1", "2026-01-01T00:01:00.000Z", None),
                node("ENG-2", "2026-01-01T00:02:00.000Z", None),
            ],
            false,
            None,
        ))
        .mount(&server)
        .await;
    mount_workspace(&server).await;

    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    sync_with(
        &fast_client(&server),
        &enabled(),
        &mut db,
        false,
        &mut stats,
    )
    .await;

    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 2);
    assert!(count(&db, "SELECT COUNT(*) FROM linear_teams") > 0);
    assert!(count(&db, "SELECT COUNT(*) FROM linear_cycles") > 0);
}

/// #146 (D28): 401, 403 and a 5xx (503 after its retry) are one stage
/// failure each, which makes `tga collect` exit non-zero. Nothing is written.
#[tokio::test]
async fn a_linear_sync_auth_or_server_error_is_a_stage_failure() {
    for status in [401_u16, 403, 500, 503] {
        let server = MockServer::start().await;
        Mock::given(any())
            .respond_with(ResponseTemplate::new(status).set_body_string("{}"))
            .mount(&server)
            .await;
        let mut db = Database::open_in_memory().expect("db");
        let mut stats = CollectionStats::default();
        sync_with(
            &fast_client(&server),
            &enabled(),
            &mut db,
            false,
            &mut stats,
        )
        .await;

        let failures = stats.stage_failures();
        assert_eq!(failures.len(), 1, "HTTP {status}: {:?}", stats.errors);
        let msg = &failures[0].message;
        assert!(
            msg.contains("linear sync") && msg.contains(&format!("HTTP {status}")),
            "HTTP {status}: {msg}"
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM linear_issues"), 0);
    }
}

/// #146 (D28): a 404 is a counted warning, not a stage failure — the run
/// still exits 0 — and it names each scope: the team and the entity pass.
#[tokio::test]
async fn a_linear_sync_404_is_a_counted_warning() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(404).set_body_string("{}"))
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    sync_with(
        &fast_client(&server),
        &enabled(),
        &mut db,
        false,
        &mut stats,
    )
    .await;

    assert!(stats.stage_failures().is_empty(), "{:?}", stats.errors);
    assert_eq!(stats.errors.len(), 1, "{:?}", stats.errors);
    let fault = &stats.errors[0];
    assert_eq!(fault.severity, FaultSeverity::ItemSkipped);
    assert!(
        fault.message.contains("2 of 2")
            && fault.message.contains("HTTP 404")
            && fault.message.contains("ENG, reference entities"),
        "{}",
        fault.message
    );
}

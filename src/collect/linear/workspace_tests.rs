//! Tests for the #272 Linear workspace guard and read-only check.

use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::collect::errors::CollectError;
use crate::collect::linear::LinearClient;
use crate::core::config::LinearConfig;

/// A fake key: never a real credential.
const FAKE_KEY: &str = "lin_api_fake_workspace_guard_key";

/// Mount an `organization { urlKey }` answer of `url_key`.
async fn mount_org(server: &MockServer, url_key: &str) {
    Mock::given(method("POST"))
        .and(body_string_contains("organization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            serde_json::json!({ "data": { "organization": { "urlKey": url_key } } }),
        ))
        .mount(server)
        .await;
}

/// Mount an issue answer for any `issue(` query.
async fn mount_issue(server: &MockServer) {
    Mock::given(method("POST"))
        .and(body_string_contains("issue(id:"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "issue": {
                "id": "uuid-1", "identifier": "PLA-12", "title": "t",
                "url": "https://linear.app/pace/issue/PLA-12",
                "state": { "name": "Done" }
            } }
        })))
        .mount(server)
        .await;
}

/// Count requests whose body names `organization`.
async fn org_queries(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("organization"))
        .count()
}

/// A client for `endpoint`, guarded when `workspace` is `Some`.
fn client(endpoint: &str, workspace: Option<&str>) -> LinearClient {
    let cfg = LinearConfig {
        api_key: Some(FAKE_KEY.into()),
        workspace: workspace.map(str::to_string),
        ..Default::default()
    };
    LinearClient::with_endpoint(&cfg, endpoint).expect("client builds")
}

#[tokio::test]
async fn guard_passes_on_matching_url_key() {
    let server = MockServer::start().await;
    mount_org(&server, "pace").await;
    let guard = WorkspaceGuard::new("pace");
    guard
        .verify(&reqwest::Client::new(), &server.uri(), FAKE_KEY)
        .await
        .expect("matching urlKey passes");
}

#[tokio::test]
async fn guard_fails_closed_on_other_url_key() {
    let server = MockServer::start().await;
    mount_org(&server, "other").await;
    let err = WorkspaceGuard::new("pace")
        .verify(&reqwest::Client::new(), &server.uri(), FAKE_KEY)
        .await
        .expect_err("a different workspace must fail");
    assert_eq!(
        err,
        WorkspaceGuardError::Mismatch {
            expected: "pace".into(),
            actual: "other".into()
        }
    );
    let text = err.to_string();
    assert!(
        text.contains("`pace`") && text.contains("`other`"),
        "{text}"
    );
}

/// No fail-open arm: an org query that errors must refuse, not skip the check.
#[tokio::test]
async fn guard_fails_closed_on_query_failure() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string(FAKE_KEY))
        .mount(&server)
        .await;
    let err = WorkspaceGuard::new("pace")
        .verify(&reqwest::Client::new(), &server.uri(), FAKE_KEY)
        .await
        .expect_err("a failed org query must fail");
    assert!(matches!(err, WorkspaceGuardError::Unverified { .. }));
    assert!(!err.to_string().contains(FAKE_KEY), "key must never print");
}

#[tokio::test]
async fn guard_queries_once() {
    let server = MockServer::start().await;
    mount_org(&server, "pace").await;
    mount_issue(&server).await;
    let c = client(&server.uri(), Some("pace"));
    for _ in 0..3 {
        c.fetch_issue("PLA-12").await.expect("guarded fetch passes");
    }
    assert_eq!(org_queries(&server).await, 1, "once per run, not per call");
}

/// The per-commit path: a mismatched workspace fails `fetch_issue` and no
/// issue query is sent.
#[tokio::test]
async fn fetch_issue_is_guarded_and_fails_closed_on_mismatch() {
    let server = MockServer::start().await;
    mount_org(&server, "other").await;
    mount_issue(&server).await;
    let err = client(&server.uri(), Some("pace"))
        .fetch_issue("PLA-12")
        .await
        .expect_err("mismatched workspace must fail the fetch");
    assert!(
        matches!(
            err,
            CollectError::LinearWorkspace(WorkspaceGuardError::Mismatch { .. })
        ),
        "{err:?}"
    );
    let issue_queries = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| String::from_utf8_lossy(&r.body).contains("issue(id:"))
        .count();
    assert_eq!(
        issue_queries, 0,
        "no issue may be read from another workspace"
    );
}

/// Unset config: behaviour unchanged, no org query is sent.
#[tokio::test]
async fn unset_workspace_sends_no_org_query() {
    let server = MockServer::start().await;
    mount_org(&server, "other").await;
    mount_issue(&server).await;
    let issue = client(&server.uri(), None)
        .fetch_issue("PLA-12")
        .await
        .expect("unguarded fetch");
    assert!(issue.is_some());
    assert_eq!(org_queries(&server).await, 0);
    assert!(WorkspaceGuard::from_config(None).is_none());
    assert!(WorkspaceGuard::from_config(Some("  ")).is_none());
}

/// Every GraphQL document the Linear clients send is a query. The runtime
/// check rejects anything else, and every request body captured from the
/// guard, the per-issue fetch and a bulk page starts with `query` or `{`.
#[tokio::test]
async fn read_only_check_rejects_mutations() {
    assert_eq!(
        ensure_read_only("mutation { issueCreate(input: {}) { success } }"),
        Err(WorkspaceGuardError::NotReadOnly)
    );
    assert_eq!(
        ensure_read_only("subscription { x }"),
        Err(WorkspaceGuardError::NotReadOnly)
    );
    assert_eq!(ensure_read_only(""), Err(WorkspaceGuardError::NotReadOnly));
    assert!(ensure_read_only("  query { viewer { id } }").is_ok());
    assert!(ensure_read_only("{ viewer { id } }").is_ok());

    let server = MockServer::start().await;
    mount_org(&server, "pace").await;
    mount_issue(&server).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": { "issues": { "nodes": [], "pageInfo": { "hasNextPage": false, "endCursor": null } } }
        })))
        .mount(&server)
        .await;
    let c = client(&server.uri(), Some("pace"));
    c.fetch_issue("PLA-12").await.expect("fetch");
    let _ = c
        .post_bulk(
            &serde_json::json!({ "query": "query { issues { nodes { id } } }" }),
            "PLA",
            0,
        )
        .await
        .expect("bulk page");
    let bodies = server.received_requests().await.unwrap_or_default();
    assert!(bodies.len() >= 3, "guard, issue and bulk requests captured");
    for r in &bodies {
        let v: serde_json::Value = serde_json::from_slice(&r.body).expect("json body");
        let q = v["query"].as_str().expect("query string");
        let t = q.trim_start();
        assert!(t.starts_with("query") || t.starts_with('{'), "{q}");
        assert!(!t.starts_with("mutation"), "{q}");
    }

    // A bulk body carrying a mutation never leaves the process.
    let err = c
        .post_bulk(&serde_json::json!({ "query": "mutation { x }" }), "PLA", 0)
        .await
        .expect_err("mutation refused");
    assert!(matches!(
        err,
        CollectError::LinearWorkspace(WorkspaceGuardError::NotReadOnly)
    ));
}

/// Source-level proof: no Linear GraphQL string in this crate opens with
/// `mutation`.
#[test]
fn no_linear_source_contains_a_graphql_mutation() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut stack = vec![
        root.join("collect/linear"),
        root.join("commands/linear"),
        root.join("classify/sources/linear.rs"),
        root.join("collect/linear_pipeline.rs"),
    ];
    let re = regex::Regex::new(r#""\s*mutation\b"#).expect("regex");
    let mut scanned = 0;
    while let Some(p) = stack.pop() {
        if p.is_dir() {
            for e in std::fs::read_dir(&p).expect("read dir").flatten() {
                stack.push(e.path());
            }
        } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("workspace_tests.rs") {
            let text = std::fs::read_to_string(&p).expect("read source");
            scanned += 1;
            assert!(!re.is_match(&text), "GraphQL mutation in {}", p.display());
        }
    }
    assert!(scanned > 10, "scan reached the Linear sources");
}

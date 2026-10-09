//! Tests for `collect::linear::bulk` (#7139, #190). Every server is a
//! `wiremock::MockServer` answering hand-built GraphQL pages; no network.

use super::*;
use crate::collect::jira::retry::RetryPolicy;
use crate::collect::linear::issue::tests::full_node;
use crate::core::config::LinearConfig;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAKE_API_KEY: &str = "lin_api_averyrealisticlookingkey0123456789";

pub(crate) fn mock_client(endpoint: &str) -> LinearClient {
    let cfg = LinearConfig {
        api_key: Some(FAKE_API_KEY.into()),
        ..Default::default()
    };
    LinearClient::with_endpoint(&cfg, endpoint).expect("client builds")
}

/// An ENG issue node with `updatedAt` and, when given, `archivedAt` set.
pub(crate) fn node(identifier: &str, updated: &str, archived: Option<&str>) -> serde_json::Value {
    let mut n = full_node(&format!("uuid-{identifier}"), identifier, "ENG");
    n["updatedAt"] = serde_json::json!(updated);
    n["archivedAt"] = serde_json::json!(archived);
    n
}

pub(crate) fn page_response(
    nodes: Vec<serde_json::Value>,
    has_next: bool,
    cursor: Option<&str>,
) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "data": {"issues": {
            "nodes": nodes,
            "pageInfo": {"hasNextPage": has_next, "endCursor": cursor},
        }}
    }))
}

fn eng(include_archived: bool) -> IssueQuery {
    IssueQuery::new("ENG", None, include_archived)
}

/// #190 acceptance 8 (pagination): a two-page walk assembles both pages via
/// `after`/`endCursor`.
#[tokio::test]
async fn fetch_team_issues_walks_every_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"after\":null"))
        .respond_with(page_response(
            vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
            true,
            Some("cursor-1"),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("cursor-1"))
        .respond_with(page_response(
            vec![node("ENG-2", "2026-01-01T00:02:00.000Z", None)],
            false,
            None,
        ))
        .mount(&server)
        .await;

    let issues = mock_client(&server.uri())
        .fetch_team_issues(&eng(true), None)
        .await
        .expect("walk succeeds");
    let ids: Vec<&str> = issues.iter().map(|i| i.identifier.as_str()).collect();
    assert_eq!(ids, vec!["ENG-1", "ENG-2"]);
}

/// #190 acceptance 1: Linear returns an archived issue only when the request
/// carries `includeArchived: true`. The mock answers each flag value with
/// what Linear would, so dropping the variable makes this walk fail.
#[tokio::test]
async fn archived_issues_arrive_only_with_include_archived() {
    let server = MockServer::start().await;
    let active = node("ENG-1", "2026-01-01T00:01:00.000Z", None);
    let archived = node(
        "ENG-2",
        "2026-01-01T00:02:00.000Z",
        Some("2026-01-03T00:00:00.000Z"),
    );
    Mock::given(method("POST"))
        .and(body_string_contains("\"includeArchived\":true"))
        .respond_with(page_response(vec![active.clone(), archived], false, None))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"includeArchived\":false"))
        .respond_with(page_response(vec![active], false, None))
        .mount(&server)
        .await;

    let client = mock_client(&server.uri());
    let with = client
        .fetch_team_issues(&eng(true), None)
        .await
        .expect("archived walk");
    assert_eq!(with.len(), 2);
    assert!(with[1].is_archived());
    assert_eq!(
        with[1].archived_at.map(|d| d.to_rfc3339()).as_deref(),
        Some("2026-01-03T00:00:00+00:00")
    );

    let without = client
        .fetch_team_issues(&eng(false), None)
        .await
        .expect("active walk");
    assert_eq!(without.len(), 1);
}

/// #190 acceptance 2 (cap): reaching `--max-issues` with issues left is an
/// error naming the team and the cap. Before #190 the walk truncated and
/// returned `Ok` with a flag the command printed under a success line.
#[tokio::test]
async fn fetch_team_issues_fails_when_the_cap_is_exceeded() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(page_response(
            vec![
                node("ENG-1", "2026-01-01T00:01:00.000Z", None),
                node("ENG-2", "2026-01-01T00:02:00.000Z", None),
                node("ENG-3", "2026-01-01T00:03:00.000Z", None),
            ],
            true,
            Some("cursor-1"),
        ))
        .mount(&server)
        .await;

    let err = mock_client(&server.uri())
        .fetch_team_issues(&eng(true), Some(2))
        .await
        .expect_err("a cap must not truncate silently");
    match err {
        CollectError::LinearIssueCapExceeded { team_key, cap } => {
            assert_eq!((team_key.as_str(), cap), ("ENG", 2));
        }
        other => panic!("expected LinearIssueCapExceeded, got {other:?}"),
    }
}

/// A team that holds exactly `cap` issues and no more completes.
#[tokio::test]
async fn fetch_team_issues_accepts_a_team_at_exactly_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(page_response(
            vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
            false,
            None,
        ))
        .mount(&server)
        .await;
    let issues = mock_client(&server.uri())
        .fetch_team_issues(&eng(true), Some(1))
        .await
        .expect("at the cap is not over it");
    assert_eq!(issues.len(), 1);
}

/// #7139 security finding, kept with no cap: a server that answers
/// `hasNextPage: true` with no rows must error, not loop.
#[tokio::test]
async fn fetch_team_issues_errors_on_a_runaway_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(page_response(vec![], true, Some("always-more")))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_team_issues(&eng(true), None)
        .await
        .expect_err("an endless hasNextPage:true must not loop forever");
    assert!(
        matches!(err, CollectError::PagingBudgetExceeded { endpoint: "linear/issues", ref key, .. } if key == "ENG"),
        "{err:?}"
    );
}

/// Every #190 field parses, and the node is kept whole as `raw`.
#[tokio::test]
async fn fetch_team_issues_page_maps_every_field() {
    let server = MockServer::start().await;
    let n = node("ENG-1", "2026-01-01T00:01:00.000Z", None);
    Mock::given(method("POST"))
        .respond_with(page_response(vec![n.clone()], false, None))
        .mount(&server)
        .await;
    let page = mock_client(&server.uri())
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect("page");
    let issue = &page.issues[0];
    assert_eq!(issue.linear_id.as_deref(), Some("uuid-ENG-1"));
    assert_eq!(issue.state_type.as_deref(), Some("completed"));
    assert_eq!(issue.estimate, Some(3.0));
    assert_eq!(issue.raw.as_ref(), Some(&n));
    assert!(!page.has_next_page);
}

/// A non-2xx on a bulk page is a `LinearBulkApi` error, not an empty team.
#[tokio::test]
async fn fetch_team_issues_page_errors_on_non_2xx() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            r#"{"errors":[{"message":"Authentication required"}]}"#,
            "application/json",
        ))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect_err("a 401 must not read as an empty team");
    assert!(
        matches!(err, CollectError::LinearBulkApi { status: 401, ref team_key, page: 1, .. } if team_key == "ENG"),
        "{err:?}"
    );
}

/// A 429 on one page backs off and resumes (#7139 critic HIGH).
#[tokio::test]
async fn fetch_team_issues_page_retries_a_429_then_succeeds() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use wiremock::{Request, Respond};

    struct OnceThrottled {
        calls: Arc<AtomicUsize>,
    }
    impl Respond for OnceThrottled {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("Retry-After", "0")
            } else {
                page_response(
                    vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
                    false,
                    None,
                )
            }
        }
    }

    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    Mock::given(method("POST"))
        .respond_with(OnceThrottled {
            calls: Arc::clone(&calls),
        })
        .mount(&server)
        .await;
    let policy = RetryPolicy {
        max_attempts: 3,
        base_delay: std::time::Duration::from_millis(1),
        max_delay: std::time::Duration::from_millis(1),
        max_total_delay: std::time::Duration::from_millis(100),
    };
    let page = mock_client(&server.uri())
        .with_retry_policy(policy)
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect("the 429 is retried, not surfaced");
    assert_eq!(page.issues.len(), 1);
    assert_eq!(calls.load(Ordering::SeqCst), 2, "429 then 200");
}

/// The all-teams mode reads every page of `teams`.
#[tokio::test]
async fn fetch_teams_pages_every_team() {
    let server = MockServer::start().await;
    let teams_page = |nodes: serde_json::Value, next: bool, cursor: Option<&str>| {
        ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"teams": {"nodes": nodes,
                "pageInfo": {"hasNextPage": next, "endCursor": cursor}}}
        }))
    };
    Mock::given(method("POST"))
        .and(body_string_contains("\"after\":null"))
        .respond_with(teams_page(
            serde_json::json!([{"id": "t1", "key": "ENG", "name": "Engineering"}]),
            true,
            Some("teams-1"),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("teams-1"))
        .respond_with(teams_page(
            serde_json::json!([{"id": "t2", "key": "OPS", "name": "Operations"}]),
            false,
            None,
        ))
        .mount(&server)
        .await;
    let teams = mock_client(&server.uri())
        .fetch_teams(true)
        .await
        .expect("teams");
    let keys: Vec<&str> = teams.iter().map(|t| t.key.as_str()).collect();
    assert_eq!(keys, vec!["ENG", "OPS"]);
}

/// #190 finding 4: `hasNextPage: true` with a null `endCursor` is an error
/// for the issue walk too (the shared `PageGuard`), never the last page.
#[tokio::test]
async fn fetch_team_issues_errors_on_more_pages_without_a_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(page_response(
            vec![node("ENG-1", "2026-01-01T00:00:00.000Z", None)],
            true,
            None,
        ))
        .mount(&server)
        .await;
    let err = mock_client(&server.uri())
        .fetch_team_issues(&eng(true), None)
        .await
        .expect_err("more pages with no cursor must not read as the end");
    let text = err.to_string();
    assert!(text.contains("endCursor"), "{text}");
    assert!(text.contains("ENG"), "{text}");
}

/// A Linear GraphQL error response: HTTP 400 with `extensions.code`.
pub(crate) fn graphql_400(code: &str) -> ResponseTemplate {
    ResponseTemplate::new(400).set_body_json(serde_json::json!({
        "errors": [{"message": "error from Linear", "extensions": {"code": code}}]
    }))
}

/// Answers the first `failures` requests with `failure`, then one valid page.
pub(crate) struct FailThenPage {
    pub(crate) calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    pub(crate) failures: usize,
    pub(crate) failure: ResponseTemplate,
}

impl wiremock::Respond for FailThenPage {
    fn respond(&self, _request: &wiremock::Request) -> ResponseTemplate {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n < self.failures {
            self.failure.clone()
        } else {
            page_response(
                vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
                false,
                None,
            )
        }
    }
}

/// A retry policy fast enough for tests: 3 attempts, 1 ms apart.
pub(crate) fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_attempts: 3,
        base_delay: std::time::Duration::from_millis(1),
        max_delay: std::time::Duration::from_millis(1),
        max_total_delay: std::time::Duration::from_millis(100),
    }
}

/// Mount `responder` for every POST and return its call counter.
async fn mount_failing(
    server: &MockServer,
    failures: usize,
    failure: ResponseTemplate,
) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    Mock::given(method("POST"))
        .respond_with(FailThenPage {
            calls: std::sync::Arc::clone(&calls),
            failures,
            failure,
        })
        .mount(server)
        .await;
    calls
}

/// #190 step 3: Linear signals a rate limit as HTTP 400 with
/// `extensions.code = RATELIMITED`. It is retried like a 429, under the same
/// policy and budget, and the walk resumes.
#[tokio::test]
async fn ratelimited_400_is_retried_like_a_429() {
    let server = MockServer::start().await;
    let calls = mount_failing(&server, 1, graphql_400("RATELIMITED")).await;
    let page = mock_client(&server.uri())
        .with_retry_policy(fast_retry())
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect("RATELIMITED is retried, not surfaced");
    assert_eq!(page.issues.len(), 1);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// #190 step 3 (Fail-Open): RATELIMITED past the retry policy is still an
/// error — the throttle error after every attempt is spent — never a page.
#[tokio::test]
async fn ratelimited_past_the_retry_policy_fails() {
    let server = MockServer::start().await;
    let calls = mount_failing(&server, usize::MAX, graphql_400("RATELIMITED")).await;
    let err = mock_client(&server.uri())
        .with_retry_policy(fast_retry())
        .fetch_team_issues(&eng(true), None)
        .await
        .expect_err("an endless rate limit must fail the walk");
    assert!(
        matches!(err, CollectError::Throttled { status: 400, .. }),
        "{err:?}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
}

/// Any other HTTP 400 is a request error, not a throttle: one attempt, and
/// the error names the status.
#[tokio::test]
async fn other_400_is_not_retried() {
    let server = MockServer::start().await;
    let calls = mount_failing(&server, usize::MAX, graphql_400("INVALID_INPUT")).await;
    let err = mock_client(&server.uri())
        .with_retry_policy(fast_retry())
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect_err("a bad request fails");
    assert!(
        matches!(err, CollectError::LinearBulkApi { status: 400, .. }),
        "{err:?}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// #190 step 3: the wait before the retry honours Linear's documented
/// `X-RateLimit-*-Reset` header (UTC epoch milliseconds) for the window
/// whose `Remaining` is 0, not the 1 ms policy backoff. A window with points
/// left does not set the wait.
#[tokio::test]
async fn ratelimited_retry_waits_for_the_exhausted_window_reset() {
    let server = MockServer::start().await;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let throttled = graphql_400("RATELIMITED")
        .insert_header("X-RateLimit-Requests-Remaining", "0")
        .insert_header("X-RateLimit-Requests-Reset", (now_ms + 400).to_string())
        .insert_header("X-RateLimit-Complexity-Remaining", "900")
        .insert_header(
            "X-RateLimit-Complexity-Reset",
            (now_ms + 60_000).to_string(),
        );
    let calls = mount_failing(&server, 1, throttled).await;
    let policy = RetryPolicy {
        max_attempts: 2,
        max_total_delay: std::time::Duration::from_secs(5),
        ..fast_retry()
    };
    let started = std::time::Instant::now();
    mock_client(&server.uri())
        .with_retry_policy(policy)
        .fetch_team_issues_page(&eng(true), None, ISSUES_PAGE_SIZE, 1)
        .await
        .expect("retried after the reset");
    let waited = started.elapsed();
    assert!(
        waited >= std::time::Duration::from_millis(300),
        "waited {waited:?}"
    );
    assert!(
        waited < std::time::Duration::from_secs(5),
        "waited {waited:?}"
    );
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
}

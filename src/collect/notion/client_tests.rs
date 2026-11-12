//! Client tests against a local wiremock server (#252). No live Notion call.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::client::NotionClient;
use super::error::NotionError;
use super::retry::RetryPolicy;
use crate::core::config::NotionConfig;

/// A token value no test asserts on except to prove it is sent.
const TOKEN: &str = "secret_test_token_252";

pub(super) fn config() -> NotionConfig {
    NotionConfig {
        token: Some(TOKEN.to_string()),
        ..NotionConfig::default()
    }
}

pub(super) fn client(server: &MockServer) -> NotionClient {
    NotionClient::with_base_url(&config(), &server.uri())
        .expect("client builds")
        .with_retry(3, Duration::ZERO)
}

pub(super) fn page(id: &str, created: &str, edited: &str, by: &str) -> Value {
    json!({
        "object": "page",
        "id": id,
        "created_time": created,
        "last_edited_time": edited,
        "created_by": {"object": "user", "id": by},
        "last_edited_by": {"object": "user", "id": by},
    })
}

pub(super) fn list(results: Vec<Value>, next: Option<&str>) -> Value {
    json!({
        "object": "list",
        "results": results,
        "next_cursor": next,
        "has_more": next.is_some(),
    })
}

#[tokio::test]
async fn search_paginates_and_stops_at_the_watermark() {
    let server = MockServer::start().await;
    // Page 2 of the search, requested with the cursor page 1 returned.
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(body_partial_json(json!({"start_cursor": "c2"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                page(
                    "p3",
                    "2026-11-01T09:00:00.000Z",
                    "2026-11-09T10:00:00.000Z",
                    "u1",
                ),
                page(
                    "p4",
                    "2026-10-01T09:00:00.000Z",
                    "2026-11-01T10:00:00.000Z",
                    "u1",
                ),
                page(
                    "p5",
                    "2026-09-01T09:00:00.000Z",
                    "2026-10-01T10:00:00.000Z",
                    "u1",
                ),
            ],
            Some("c3"),
        )))
        .expect(1)
        .mount(&server)
        .await;
    // Page 1: newest first, sorted by last_edited_time descending.
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(header("Notion-Version", "2022-06-28"))
        .and(header("Authorization", format!("Bearer {TOKEN}").as_str()))
        .and(body_partial_json(json!({
            "sort": {"direction": "descending", "timestamp": "last_edited_time"},
            "filter": {"property": "object", "value": "page"},
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                page(
                    "p1",
                    "2026-11-10T09:00:00.000Z",
                    "2026-11-11T10:00:00.000Z",
                    "u1",
                ),
                json!({"object": "database", "id": "db1",
                       "created_time": "2026-11-10T09:00:00.000Z",
                       "last_edited_time": "2026-11-10T10:00:00.000Z"}),
                page(
                    "p2",
                    "2026-11-02T09:00:00.000Z",
                    "2026-11-10T10:00:00.000Z",
                    "u1",
                ),
            ],
            Some("c2"),
        )))
        .mount(&server)
        .await;

    let all_since = Utc.with_ymd_and_hms(2026, 11, 1, 10, 0, 0).single();
    let pages = client(&server)
        .search_pages_since(all_since)
        .await
        .expect("search");
    let ids: Vec<&str> = pages.iter().map(|p| p.id.as_str()).collect();
    // p4 sits exactly on the watermark and is read again; p5 is older, so the
    // walk stops there and never requests cursor c3.
    assert_eq!(ids, ["p1", "p2", "p3", "p4"]);
}

/// Answers 429 with `Retry-After: 0` on the first call, then 200.
struct ThrottleOnce {
    calls: Arc<AtomicUsize>,
    status: u16,
}

impl Respond for ThrottleOnce {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            return ResponseTemplate::new(self.status).insert_header("Retry-After", "0");
        }
        ResponseTemplate::new(200).set_body_json(list(Vec::new(), None))
    }
}

#[tokio::test]
async fn a_429_is_retried_after_retry_after() {
    for status in [429, 529] {
        let server = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        Mock::given(method("GET"))
            .and(path("/v1/users"))
            .respond_with(ThrottleOnce {
                calls: Arc::clone(&calls),
                status,
            })
            .mount(&server)
            .await;
        let users = client(&server).list_users().await.expect("retried");
        assert!(users.is_empty());
        assert_eq!(calls.load(Ordering::SeqCst), 2, "HTTP {status} is retried");
    }
}

#[tokio::test]
async fn throttling_is_bounded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "0"))
        .expect(3)
        .mount(&server)
        .await;
    let err = client(&server).list_users().await.expect_err("bounded");
    assert!(
        matches!(err, NotionError::Throttled { attempts: 3, .. }),
        "{err}"
    );
}

/// #252 (c): a non-transient 4xx is sent once, and the #254 skip statuses
/// (403, 404) are not retried either; every 5xx is retried to the bound.
#[tokio::test]
async fn only_transient_statuses_are_retried() {
    for (status, calls) in [
        (400, 1),
        (401, 1),
        (403, 1),
        (404, 1),
        (409, 1),
        (500, 3),
        (502, 3),
        (503, 3),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/comments"))
            .respond_with(ResponseTemplate::new(status))
            .expect(calls)
            .mount(&server)
            .await;
        let err = client(&server)
            .list_comments("p1")
            .await
            .expect_err("fails");
        assert!(
            matches!(err, NotionError::Status { status: s, .. } if s == status),
            "HTTP {status}: {err}"
        );
        server.verify().await;
    }
}

/// A policy whose `Retry-After` second lasts 1 ms.
fn fast_policy(max_attempts: u32, max_total_ms: u64) -> RetryPolicy {
    RetryPolicy {
        max_attempts,
        base_delay: Duration::ZERO,
        max_delay: Duration::from_secs(30),
        max_total_delay: Duration::from_millis(max_total_ms),
        retry_after_unit: Duration::from_millis(1),
    }
}

/// #252 (b): a 429 on `/v1/comments` is retried after the server's
/// `Retry-After` (here 80 units of 1 ms), and the retry succeeds.
#[tokio::test]
async fn a_429_honors_retry_after() {
    let server = MockServer::start().await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(move |_: &Request| {
            if seen.fetch_add(1, Ordering::SeqCst) == 0 {
                ResponseTemplate::new(429).insert_header("Retry-After", "80")
            } else {
                ResponseTemplate::new(200).set_body_json(list(Vec::new(), None))
            }
        })
        .mount(&server)
        .await;
    let c = NotionClient::with_base_url(&config(), &server.uri())
        .expect("client")
        .with_retry_policy(fast_policy(3, 10_000));
    let started = std::time::Instant::now();
    c.list_comments("p1").await.expect("retried");
    assert_eq!(calls.load(Ordering::SeqCst), 2, "one retry");
    assert!(
        started.elapsed() >= Duration::from_millis(80),
        "waited the Retry-After: {:?}",
        started.elapsed()
    );
}

/// #252: the summed wait is bounded. A second 100-unit `Retry-After` would
/// cross the 150 ms budget, so the request stops after two attempts.
#[tokio::test]
async fn the_total_retry_delay_is_bounded() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "100"))
        .expect(2)
        .mount(&server)
        .await;
    let err = NotionClient::with_base_url(&config(), &server.uri())
        .expect("client")
        .with_retry_policy(fast_policy(10, 150))
        .list_comments("p1")
        .await
        .expect_err("budget spent");
    assert!(
        matches!(err, NotionError::Throttled { attempts: 2, .. }),
        "{err}"
    );
}

/// #252: the live-check failure was one transport error on `/v1/comments`.
/// A raw listener drops the first two connections after reading the request,
/// then answers; the client must retry through both drops.
#[tokio::test]
async fn a_transport_error_is_retried() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let n = counter.fetch_add(1, Ordering::SeqCst);
            let mut buf = Vec::new();
            let mut chunk = [0u8; 1024];
            while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                match sock.read(&mut chunk).await {
                    Ok(0) | Err(_) => break,
                    Ok(k) => buf.extend_from_slice(&chunk[..k]),
                }
            }
            if n < 2 {
                drop(sock);
                continue;
            }
            let body = list(Vec::new(), None).to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
                 Connection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = sock.write_all(resp.as_bytes()).await;
        }
    });
    let c = NotionClient::with_base_url(&config(), &format!("http://{addr}"))
        .expect("client")
        .with_retry(4, Duration::ZERO);
    let comments = c.list_comments("p1").await.expect("retried past drops");
    assert!(comments.is_empty());
    assert_eq!(accepted.load(Ordering::SeqCst), 3, "two drops, one answer");
}

#[tokio::test]
async fn comments_and_users_paginate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .and(query_param("start_cursor", "k2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![json!({"object": "comment", "id": "c2",
                        "created_time": "2026-11-11T11:00:00.000Z",
                        "created_by": {"object": "user", "id": "u2"},
                        "rich_text": [{"plain_text": "héllo"}]})],
            None,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![json!({"object": "comment", "id": "c1",
                        "created_time": "2026-11-11T10:00:00.000Z",
                        "created_by": {"object": "user", "id": "u1"},
                        "rich_text": [{"plain_text": "abc"}, {"plain_text": "de"}]})],
            Some("k2"),
        )))
        .mount(&server)
        .await;

    let comments = client(&server).list_comments("p1").await.expect("comments");
    let got: Vec<(&str, usize)> = comments
        .iter()
        .map(|c| (c.id.as_str(), c.body_len))
        .collect();
    assert_eq!(got, [("c1", 5), ("c2", 5)], "body length in characters");
    assert!(comments.iter().all(|c| c.page_id == "p1"));
}

/// #252: a server that answers `has_more` with the cursor just requested
/// must not loop the walk forever.
#[tokio::test]
async fn a_repeated_cursor_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), Some("k1"))))
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), Some("s1"))))
        .expect(2)
        .mount(&server)
        .await;
    let c = client(&server);
    let err = c.list_users().await.expect_err("users stop");
    assert!(matches!(err, NotionError::CursorRepeated { .. }), "{err}");
    let err = c.search_pages_since(None).await.expect_err("search stops");
    assert!(matches!(err, NotionError::CursorRepeated { .. }), "{err}");
}

/// #252: an error names the status, the endpoint, and Notion's own error
/// `code` and `message` — and never the token.
#[tokio::test]
async fn an_error_carries_the_notion_code_and_message() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "object": "error", "status": 404, "code": "object_not_found",
            "message": "Could not find block with ID: p1."
        })))
        .mount(&server)
        .await;
    let err = client(&server).list_comments("p1").await.expect_err("404");
    let text = err.to_string();
    for part in [
        "HTTP 404",
        "/v1/comments",
        "object_not_found",
        "Could not find block",
    ] {
        assert!(text.contains(part), "missing {part:?} in {text}");
    }
    assert!(!text.contains(TOKEN), "the token must never appear: {text}");
}

#[tokio::test]
async fn has_more_without_a_cursor_is_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"results": [], "has_more": true, "next_cursor": null})),
        )
        .mount(&server)
        .await;
    let err = client(&server).list_users().await.expect_err("no loop");
    assert!(matches!(err, NotionError::Pagination(_)), "{err}");
}

#[test]
fn an_unset_token_is_a_config_error() {
    for token in [None, Some(""), Some("${TGA_TEST_252_UNSET_VAR}")] {
        let cfg = NotionConfig {
            token: token.map(str::to_string),
            ..NotionConfig::default()
        };
        let Err(err) = NotionClient::new(&cfg) else {
            panic!("token {token:?} must be rejected");
        };
        assert!(matches!(err, NotionError::Config(_)), "{err}");
    }
}

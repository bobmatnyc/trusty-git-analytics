//! HTTP client for the three Notion endpoints the v1 signal reads (#252).
//!
//! Why: the activity signal needs pages edited since the last sync, the
//! comments on those pages, and the workspace's users for identity. Nothing
//! else is requested.
//! What: [`NotionClient`] — `POST /v1/search` sorted by `last_edited_time`
//! descending and stopped at a watermark, `GET /v1/comments?block_id=` per
//! page, `GET /v1/users`. Every list follows `next_cursor`. A transient
//! failure (transport, 429/529, 5xx) is retried under a bounded
//! [`RetryPolicy`]; see `retry`.
//! Test: `client_tests` — `search_paginates_and_stops_at_the_watermark`,
//! `a_429_is_retried_after_retry_after`, `throttling_is_bounded`,
//! `only_transient_statuses_are_retried`, `a_transport_error_is_retried`,
//! `comments_and_users_paginate`, `an_unset_token_is_a_config_error`,
//! `a_repeated_cursor_is_an_error`; the lookback window:
//! `pipeline_tests::a_late_indexed_page_below_the_watermark_is_collected`.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::{Client, RequestBuilder};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tracing::{debug, info, warn};

use super::error::NotionError;
use super::retry::{
    is_throttled, is_transient_status, is_transient_transport, retry_after_secs, RetryPolicy,
};
use super::wire::{CommentMeta, CommentWire, ListPage, NotionUser, PageMeta};
use crate::collect::env_expand::expand_env_var;
use crate::core::config::NotionConfig;

/// Production API root.
const NOTION_API_BASE: &str = "https://api.notion.com";
/// The API version the response shapes in `wire` are written against.
const NOTION_VERSION: &str = "2022-06-28";
/// Largest page size every list endpoint accepts.
const PAGE_SIZE: u32 = 100;
/// Pages between two search progress lines (#252).
const PROGRESS_EVERY: usize = 100;

/// Authenticated client for the Notion public API.
///
/// `Debug` is not implemented: the struct holds the expanded token.
pub struct NotionClient {
    http: Client,
    base_url: String,
    token: String,
    // #252: one policy for every transient failure, not only throttling.
    retry: RetryPolicy,
    lookback: chrono::Duration,
}

/// One attempt's result: final, or a transient failure worth retrying.
enum Attempt<T> {
    Done(Result<T, NotionError>),
    Retry {
        err: NotionError,
        retry_after_secs: Option<u64>,
    },
}

impl NotionClient {
    /// Build a client from the `notion:` block.
    ///
    /// # Errors
    ///
    /// [`NotionError::Config`] when `token` is missing or expands to nothing
    /// (`"${NOTION_TOKEN}"` with the variable unset). The message never
    /// carries the token value. [`NotionError::Transport`] when the HTTP
    /// client cannot be built.
    pub fn new(cfg: &NotionConfig) -> Result<Self, NotionError> {
        Self::with_base_url(cfg, NOTION_API_BASE)
    }

    /// [`Self::new`] against another API root (a local mock in tests).
    ///
    /// # Errors
    ///
    /// As [`Self::new`].
    pub fn with_base_url(cfg: &NotionConfig, base_url: &str) -> Result<Self, NotionError> {
        let token = cfg.token.as_deref().map(expand_env_var).unwrap_or_default();
        if token.trim().is_empty() {
            return Err(NotionError::Config(
                "notion.token is unset or empty after expansion; set NOTION_TOKEN \
                 and use `token: \"${NOTION_TOKEN}\"`"
                    .to_string(),
            ));
        }
        let http = Client::builder()
            .user_agent(concat!("tga/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            token,
            retry: RetryPolicy::default(),
            // #252: overlap window below the watermark; see `search_pages_since`.
            lookback: chrono::Duration::minutes(i64::from(cfg.lookback_minutes())),
        })
    }

    /// Override the attempt bound and the base backoff (tests).
    pub fn with_retry(mut self, max_attempts: u32, base_delay: Duration) -> Self {
        self.retry.max_attempts = max_attempts.max(1);
        self.retry.base_delay = base_delay;
        self
    }

    /// Replace the whole retry policy (#252: tests shrink every wait).
    pub fn with_retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry = RetryPolicy {
            max_attempts: policy.max_attempts.max(1),
            ..policy
        };
        self
    }

    /// Pages edited at or after `watermark` minus the lookback, newest first.
    ///
    /// Walks `/v1/search` (pages only, `last_edited_time` descending) and stops
    /// at the first page edited strictly before `watermark - lookback`
    /// (`notion.lookback_minutes`, default 60). Notion indexes search
    /// asynchronously, so a page edited just before the stored watermark can
    /// first appear after the sync that set it; the overlap window re-reads
    /// those pages. Notion also rounds `last_edited_time` to the minute. The
    /// event upsert makes every re-read harmless. `None`: every page the
    /// token can see.
    ///
    /// # Errors
    ///
    /// Any [`NotionError`] from a request, a body, or a page timestamp.
    pub async fn search_pages_since(
        &self,
        watermark: Option<DateTime<Utc>>,
    ) -> Result<Vec<PageMeta>, NotionError> {
        let endpoint = "/v1/search";
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        // #252: re-read an overlap window below the watermark.
        let stop_before = watermark.map(|w| w - self.lookback);
        loop {
            let mut body = json!({
                "filter": {"property": "object", "value": "page"},
                "sort": {"direction": "descending", "timestamp": "last_edited_time"},
                "page_size": PAGE_SIZE,
            });
            if let Some(c) = &cursor {
                body["start_cursor"] = Value::String(c.clone());
            }
            let url = format!("{}{endpoint}", self.base_url);
            let page: ListPage<PageMeta> = self
                .send(endpoint, || self.http.post(&url).json(&body))
                .await?;
            for p in page.results {
                if p.object != "page" {
                    continue;
                }
                if let Some(mark) = stop_before {
                    if parse_ts(&p.last_edited_time)? < mark {
                        return Ok(out);
                    }
                }
                out.push(p);
                // #252: a first sync walks the whole workspace; show progress.
                if out.len() % PROGRESS_EVERY == 0 {
                    info!("notion: search {} pages so far", out.len());
                }
            }
            match next_cursor(endpoint, page.has_more, page.next_cursor, cursor.as_deref())? {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }

    /// Every comment the API lists for `page_id`, bodies reduced to lengths.
    ///
    /// # Errors
    ///
    /// Any [`NotionError`] from a request or a body.
    pub async fn list_comments(&self, page_id: &str) -> Result<Vec<CommentMeta>, NotionError> {
        let endpoint = "/v1/comments";
        let url = format!("{}{endpoint}", self.base_url);
        let pages: Vec<CommentWire> = self
            .paginate_get(endpoint, &url, &[("block_id", page_id)])
            .await?;
        Ok(pages.into_iter().map(|c| c.into_meta(page_id)).collect())
    }

    /// Every user in the workspace (people and bots).
    ///
    /// # Errors
    ///
    /// Any [`NotionError`] from a request or a body.
    pub async fn list_users(&self) -> Result<Vec<NotionUser>, NotionError> {
        let endpoint = "/v1/users";
        let url = format!("{}{endpoint}", self.base_url);
        self.paginate_get(endpoint, &url, &[]).await
    }

    /// Follow `next_cursor` on a `GET` list endpoint until the last page.
    async fn paginate_get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        url: &str,
        params: &[(&str, &str)],
    ) -> Result<Vec<T>, NotionError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        let page_size = PAGE_SIZE.to_string();
        loop {
            let mut query: Vec<(&str, &str)> = params.to_vec();
            query.push(("page_size", page_size.as_str()));
            if let Some(c) = &cursor {
                query.push(("start_cursor", c.as_str()));
            }
            let page: ListPage<T> = self
                .send(endpoint, || self.http.get(url).query(&query))
                .await?;
            out.extend(page.results);
            match next_cursor(endpoint, page.has_more, page.next_cursor, cursor.as_deref())? {
                Some(c) => cursor = Some(c),
                None => return Ok(out),
            }
        }
    }

    /// Send one request under the retry policy, then decode.
    ///
    /// `build` is called once per attempt because a `RequestBuilder` is
    /// single-use. #252: a transport error, 429/529 and any 5xx are retried
    /// until `max_attempts` or `max_total_delay` runs out, then the last
    /// error is returned; any other non-success status fails at once.
    async fn send<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        build: impl Fn() -> RequestBuilder,
    ) -> Result<T, NotionError> {
        let mut attempt = 0;
        let mut waited = Duration::ZERO;
        loop {
            attempt += 1;
            let (err, retry_after_secs) = match self.attempt(endpoint, &build, attempt).await {
                Attempt::Done(result) => return result,
                Attempt::Retry {
                    err,
                    retry_after_secs,
                } => (err, retry_after_secs),
            };
            if attempt >= self.retry.max_attempts {
                return Err(err);
            }
            let wait = self.retry.wait(attempt, retry_after_secs);
            if waited + wait > self.retry.max_total_delay {
                warn!(endpoint, attempt, error = %err, "Notion retry budget spent; giving up");
                return Err(err);
            }
            waited += wait;
            warn!(
                endpoint,
                attempt,
                wait_ms = wait.as_millis() as u64,
                error = %err,
                "Notion request failed transiently; retrying"
            );
            tokio::time::sleep(wait).await;
        }
    }

    /// One attempt of [`Self::send`], classified as final or retryable.
    async fn attempt<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        build: &impl Fn() -> RequestBuilder,
        attempt: u32,
    ) -> Attempt<T> {
        let transport = |e: reqwest::Error| {
            if is_transient_transport(&e) {
                Attempt::Retry {
                    err: NotionError::Transport(e),
                    retry_after_secs: None,
                }
            } else {
                Attempt::Done(Err(NotionError::Transport(e)))
            }
        };
        let resp = match build()
            .bearer_auth(&self.token)
            .header("Notion-Version", NOTION_VERSION)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return transport(e),
        };
        let status = resp.status();
        if !status.is_success() {
            let retry_after_secs = retry_after_secs(&resp);
            // #252: carry Notion's error code and message, never headers.
            let (code, message) = error_body(resp).await;
            let endpoint = endpoint.to_string();
            let err = if is_throttled(status) {
                NotionError::Throttled {
                    status: status.as_u16(),
                    endpoint,
                    attempts: attempt,
                    code,
                    message,
                }
            } else {
                NotionError::Status {
                    status: status.as_u16(),
                    endpoint,
                    code,
                    message,
                }
            };
            return if is_transient_status(status) {
                Attempt::Retry {
                    err,
                    retry_after_secs,
                }
            } else {
                Attempt::Done(Err(err))
            };
        }
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => return transport(e),
        };
        debug!(endpoint, bytes = bytes.len(), "Notion response");
        Attempt::Done(
            serde_json::from_slice(&bytes).map_err(|e| NotionError::Decode {
                endpoint: endpoint.to_string(),
                message: e.to_string(),
            }),
        )
    }
}

/// Longest Notion error `message` carried into an error, in characters.
const MAX_ERROR_MESSAGE_CHARS: usize = 300;

/// Notion's error body: `{"object": "error", "status", "code", "message"}`.
#[derive(serde::Deserialize)]
struct ErrorBody {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

/// Read `(code, message)` from an error response body (#252).
///
/// A body that is not Notion's error shape yields `(None, None)`; the status
/// and endpoint still name the failure. The message is truncated.
async fn error_body(resp: reqwest::Response) -> (Option<String>, Option<String>) {
    let Ok(bytes) = resp.bytes().await else {
        return (None, None);
    };
    match serde_json::from_slice::<ErrorBody>(&bytes) {
        Ok(b) => (
            b.code.filter(|c| !c.is_empty()),
            b.message
                .filter(|m| !m.is_empty())
                .map(|m| m.chars().take(MAX_ERROR_MESSAGE_CHARS).collect()),
        ),
        Err(_) => (None, None),
    }
}

/// The cursor to request next, `None` after the last page.
///
/// #252: a `next_cursor` equal to the cursor just requested is an error, so
/// a server that repeats itself cannot loop the walk forever.
fn next_cursor(
    endpoint: &str,
    has_more: bool,
    next: Option<String>,
    previous: Option<&str>,
) -> Result<Option<String>, NotionError> {
    if !has_more {
        return Ok(None);
    }
    match next {
        Some(c) if previous == Some(c.as_str()) => Err(NotionError::CursorRepeated {
            endpoint: endpoint.to_string(),
        }),
        Some(c) if !c.is_empty() => Ok(Some(c)),
        _ => Err(NotionError::Pagination(endpoint.to_string())),
    }
}

/// Parse a Notion RFC 3339 timestamp to UTC.
///
/// # Errors
///
/// [`NotionError::Timestamp`] naming the value.
pub(crate) fn parse_ts(raw: &str) -> Result<DateTime<Utc>, NotionError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| NotionError::Timestamp(raw.to_string()))
}

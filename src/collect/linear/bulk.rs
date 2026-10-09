//! Bulk Linear reads for `tga linear sync`: the paginated `issues` walk and
//! the `teams` list (#7139, #190).
//!
//! Why: #190 — the walk omitted `includeArchived`, so archived issues (most
//! of an older workspace) never arrived; it read one team per run; and its
//! `--max-issues` cap truncated the walk and still reported success. Lifted
//! out of `client.rs` so that file does not grow.
//! What: [`IssueQuery`] (team, `updatedAt` lower bound, archived flag),
//! [`LinearClient::fetch_team_issues_page`] and
//! [`LinearClient::fetch_team_issues`] over it, and
//! [`LinearClient::fetch_teams`] for the all-teams mode. Every request goes
//! through one sender that maps 429/503 to [`CollectError::Throttled`] for
//! [`crate::collect::jira::retry::with_retry`].
//! Test: `bulk_tests.rs`.

use chrono::{DateTime, Utc};

use super::client::{redacted_body_excerpt, LinearClient};
use super::issue::{parse_issue_node, LinearIssue, ISSUE_FIELDS};
use super::sync;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::retry::with_retry;

/// Issues requested per page. Fifty keeps one page of the full
/// [`ISSUE_FIELDS`] selection, with its nested `labels` connection, well
/// inside Linear's per-query complexity limit.
pub const ISSUES_PAGE_SIZE: usize = 50;

/// Teams requested per page of [`LinearClient::fetch_teams`].
const TEAMS_PAGE_SIZE: usize = 100;

/// The scope of one team's issue walk.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct IssueQuery {
    /// Linear team key, e.g. `"ENG"`.
    pub team_key: String,
    /// Lower bound on `updatedAt`; `None` walks the team's whole history.
    pub since: Option<DateTime<Utc>>,
    /// Send `includeArchived: true`. Linear omits archived issues otherwise.
    pub include_archived: bool,
}

impl IssueQuery {
    /// Build a query for `team_key`.
    #[must_use]
    pub fn new(team_key: &str, since: Option<DateTime<Utc>>, include_archived: bool) -> Self {
        Self {
            team_key: team_key.to_string(),
            since,
            include_archived,
        }
    }
}

/// One page of [`LinearClient::fetch_team_issues_page`].
#[derive(Debug, Clone, PartialEq)]
pub struct LinearIssuesPage {
    /// Issues on this page.
    pub issues: Vec<LinearIssue>,
    /// Whether Linear reports another page after this one.
    pub has_next_page: bool,
    /// Opaque cursor for the next page's `after` argument, when
    /// `has_next_page` is `true`.
    pub end_cursor: Option<String>,
}

/// A Linear team as `teams` lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct LinearTeam {
    /// Team id (UUID).
    pub id: String,
    /// Team key, e.g. `"ENG"`.
    pub key: String,
    /// Team name.
    pub name: String,
}

/// Paging state shared by every walk: stops a server that keeps answering
/// `hasNextPage: true` without advancing (#7139 security finding).
pub(super) struct PageGuard {
    endpoint: &'static str,
    key: String,
    last_cursor: Option<String>,
}

impl PageGuard {
    /// A guard for a fresh walk of `endpoint`, naming `key` in its error.
    // #190: the reference-entity walks (`entities.rs`) share this guard.
    pub(super) fn new(endpoint: &'static str, key: &str) -> Self {
        Self {
            endpoint,
            key: key.to_string(),
            last_cursor: None,
        }
    }

    /// Return the next `after` cursor, or `None` when the walk is done.
    ///
    /// # Errors
    ///
    /// - [`CollectError::PagingBudgetExceeded`] when a page says more follow
    ///   but returned no rows, or repeats the previous cursor.
    /// - [`CollectError::LinearPageInfoInvalid`] when a page says more
    ///   follow but has no `endCursor`.
    pub(super) fn advance(
        &mut self,
        has_next: bool,
        end_cursor: Option<String>,
        rows_on_page: usize,
        page: usize,
    ) -> Result<Option<String>> {
        if !has_next {
            return Ok(None);
        }
        // #190: more pages with no cursor used to read as the end of the
        // walk, a silent cut. It fails closed for every walk.
        let Some(cursor) = end_cursor else {
            return Err(CollectError::LinearPageInfoInvalid {
                endpoint: self.endpoint,
                key: self.key.clone(),
                page,
                problem: "hasNextPage is true but endCursor is null",
            });
        };
        if rows_on_page == 0 || self.last_cursor.as_deref() == Some(cursor.as_str()) {
            return Err(CollectError::PagingBudgetExceeded {
                endpoint: self.endpoint,
                key: self.key.clone(),
                pages: page,
            });
        }
        self.last_cursor = Some(cursor.clone());
        Ok(Some(cursor))
    }
}

impl LinearClient {
    /// POST one bulk GraphQL request and return its `data` object.
    ///
    /// Why: one place maps 429/503 — and, since #190 step 3, Linear's rate
    /// limit, an HTTP 400 whose `errors[].extensions.code` is `RATELIMITED` —
    /// to [`CollectError::Throttled`] (retried by `with_retry` under the
    /// client's policy and budget), any other non-2xx or a GraphQL `errors`
    /// array to [`CollectError::LinearBulkApi`], with the body scrubbed of
    /// the key.
    /// What: one un-retried attempt; callers wrap it in `with_retry`, because
    /// a `reqwest::RequestBuilder` is single-use. A RATELIMITED wait is the
    /// `Retry-After` header when sent, else [`ratelimit_reset_delay`], else
    /// the policy's own backoff.
    /// Test: `ratelimited_400_is_retried_like_a_429`,
    /// `ratelimited_past_the_retry_policy_fails`, `other_400_is_not_retried`,
    /// `ratelimited_retry_waits_for_the_exhausted_window_reset`.
    pub(super) async fn post_bulk(
        &self,
        body: &serde_json::Value,
        scope: &str,
        page: usize,
    ) -> Result<serde_json::Value> {
        let resp = self
            .client
            .post(&self.endpoint)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(CollectError::Http)?;
        let status = resp.status();
        let retry_after = resp
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            || status == reqwest::StatusCode::SERVICE_UNAVAILABLE
        {
            return Err(CollectError::Throttled {
                status: status.as_u16(),
                retry_after,
            });
        }
        // #190: Linear reports a rate limit as HTTP 400 with a RATELIMITED
        // GraphQL error; it is a throttle, so `with_retry` waits and retries.
        let reset_wait = ratelimit_reset_delay(resp.headers(), Utc::now().timestamp_millis());
        let not_found = |entity: String| CollectError::LinearNotFound {
            status: status.as_u16(),
            scope: scope.to_string(),
            page,
            entity,
        };
        let bulk_error = |message: String| CollectError::LinearBulkApi {
            status: status.as_u16(),
            team_key: scope.to_string(),
            page,
            message,
        };
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status == reqwest::StatusCode::BAD_REQUEST && is_ratelimited(&text) {
                return Err(CollectError::Throttled {
                    status: status.as_u16(),
                    retry_after: retry_after.or(reset_wait),
                });
            }
            // #190 step 6: Linear's not-found answer, should it arrive as a 400.
            if status == reqwest::StatusCode::BAD_REQUEST {
                let errors = serde_json::from_str::<serde_json::Value>(&text)
                    .map(|mut v| v["errors"].take())
                    .unwrap_or_default();
                if let Some(entity) = not_found_entity(&errors) {
                    return Err(not_found(entity));
                }
            }
            return Err(bulk_error(redacted_body_excerpt(&text, &self.api_key)));
        }
        let mut json: serde_json::Value = resp.json().await.map_err(CollectError::Http)?;
        if let Some(errors) = json.get("errors") {
            // #190 step 6: "Entity not found" is Linear's answer, not a fault;
            // the caller decides whether an absent entity is an error.
            if let Some(entity) = not_found_entity(errors) {
                return Err(not_found(entity));
            }
            if errors.as_array().is_some_and(|a| !a.is_empty()) {
                return Err(bulk_error(redacted_body_excerpt(
                    &errors.to_string(),
                    &self.api_key,
                )));
            }
        }
        Ok(json["data"].take())
    }

    /// Fetch one page of a team's issues, ordered by `updatedAt`, retrying a
    /// 429/503 with backoff.
    ///
    /// Why: the primitive [`Self::fetch_team_issues`] pages over; a single
    /// rate-limit response on page 150 must not discard the walk (#7139).
    /// What: sends [`ISSUE_FIELDS`] with the team filter, the optional
    /// `updatedAt` bound, and `includeArchived` from `query` (#190).
    /// Test: `fetch_team_issues_page_maps_every_field`,
    /// `archived_issues_arrive_only_with_include_archived`,
    /// `fetch_team_issues_page_errors_on_non_2xx`,
    /// `fetch_team_issues_page_retries_a_429_then_succeeds`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearBulkApi`] on a non-2xx (other than 429/503 once
    ///   retries are spent) or a GraphQL `errors` array.
    /// - [`CollectError::Http`] on transport failure or a non-JSON body.
    pub async fn fetch_team_issues_page(
        &self,
        query: &IssueQuery,
        after: Option<&str>,
        page_size: usize,
        page_number: usize,
    ) -> Result<LinearIssuesPage> {
        let gql = format!(
            "query($first: Int!, $after: String, $filter: IssueFilter, \
             $orderBy: PaginationOrderBy, $includeArchived: Boolean) {{ \
             issues(first: $first, after: $after, filter: $filter, orderBy: $orderBy, \
             includeArchived: $includeArchived) {{ \
             nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }}"
        );
        let body = serde_json::json!({
            "query": gql,
            "variables": {
                "first": page_size,
                "after": after,
                "filter": sync::build_issues_filter(&query.team_key, query.since),
                "orderBy": "updatedAt",
                // #190: Linear returns archived issues only when asked.
                "includeArchived": query.include_archived,
            },
        });
        let data = with_retry("linear issues page", &self.retry, &self.budget, || {
            self.post_bulk(&body, &query.team_key, page_number)
        })
        .await?;
        let issues = data["issues"]["nodes"]
            .as_array()
            .map(|nodes| nodes.iter().map(|n| parse_issue_node("", n)).collect())
            .unwrap_or_default();
        Ok(LinearIssuesPage {
            issues,
            has_next_page: data["issues"]["pageInfo"]["hasNextPage"]
                .as_bool()
                .unwrap_or(false),
            end_cursor: data["issues"]["pageInfo"]["endCursor"]
                .as_str()
                .map(String::from),
        })
    }

    /// Walk every page of one team's issues.
    ///
    /// Why: #190 — the walk used to stop at `max_issues` and return a
    /// truncation flag the command printed as a note under a success line.
    /// What: follows `endCursor` until `hasNextPage` is false. `max_issues =
    /// None` walks without a cap. With `Some(cap)`, holding more than `cap`
    /// issues, or exactly `cap` with another page to come, is an error and
    /// no partial result is returned.
    /// Test: `fetch_team_issues_walks_every_page`,
    /// `fetch_team_issues_fails_when_the_cap_is_exceeded`,
    /// `fetch_team_issues_errors_on_a_runaway_cursor`,
    /// `fetch_team_issues_errors_on_more_pages_without_a_cursor`.
    ///
    /// # Errors
    ///
    /// - The first page error from [`Self::fetch_team_issues_page`].
    /// - [`CollectError::LinearIssueCapExceeded`] when the cap is reached
    ///   with issues left.
    /// - [`CollectError::PagingBudgetExceeded`] when the server stops
    ///   advancing its cursor.
    /// - [`CollectError::LinearPageInfoInvalid`] when a page says more
    ///   follow with a null `endCursor`.
    pub async fn fetch_team_issues(
        &self,
        query: &IssueQuery,
        max_issues: Option<usize>,
    ) -> Result<Vec<LinearIssue>> {
        let mut guard = PageGuard::new("linear/issues", &query.team_key);
        let mut issues = Vec::new();
        let mut after: Option<String> = None;
        for page_number in 1usize.. {
            let page = self
                .fetch_team_issues_page(query, after.as_deref(), ISSUES_PAGE_SIZE, page_number)
                .await?;
            let rows = page.issues.len();
            issues.extend(page.issues);
            let next = guard.advance(page.has_next_page, page.end_cursor, rows, page_number)?;
            if let Some(cap) = max_issues {
                // #190: a cap is a stop with an error, never a silent cut.
                if issues.len() > cap || (issues.len() == cap && next.is_some()) {
                    return Err(CollectError::LinearIssueCapExceeded {
                        team_key: query.team_key.clone(),
                        cap,
                    });
                }
            }
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(issues)
    }

    /// List every team the API key can see.
    ///
    /// Why: #190 — the all-teams mode syncs each team under its own cursor,
    /// so it needs the team keys before the first issue walk.
    /// What: pages `teams(first: 100)`; `include_archived` is sent as the
    /// query's `includeArchived`.
    /// Test: `fetch_teams_pages_every_team`.
    ///
    /// # Errors
    ///
    /// As [`Self::fetch_team_issues`], with `*` as the team in error text.
    pub async fn fetch_teams(&self, include_archived: bool) -> Result<Vec<LinearTeam>> {
        const QUERY: &str = "query($first: Int!, $after: String, $includeArchived: Boolean) { \
             teams(first: $first, after: $after, includeArchived: $includeArchived) { \
             nodes { id key name } pageInfo { hasNextPage endCursor } } }";
        let mut guard = PageGuard::new("linear/teams", "*");
        let mut teams = Vec::new();
        let mut after: Option<String> = None;
        for page_number in 1usize.. {
            let body = serde_json::json!({
                "query": QUERY,
                "variables": {
                    "first": TEAMS_PAGE_SIZE,
                    "after": after,
                    "includeArchived": include_archived,
                },
            });
            let data = with_retry("linear teams page", &self.retry, &self.budget, || {
                self.post_bulk(&body, "*", page_number)
            })
            .await?;
            let nodes = data["teams"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            for node in &nodes {
                let field = |k: &str| node[k].as_str().unwrap_or("").to_string();
                teams.push(LinearTeam {
                    id: field("id"),
                    key: field("key"),
                    name: field("name"),
                });
            }
            let info = &data["teams"]["pageInfo"];
            let next = guard.advance(
                info["hasNextPage"].as_bool().unwrap_or(false),
                info["endCursor"].as_str().map(String::from),
                nodes.len(),
                page_number,
            )?;
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(teams)
    }
}

/// The entity type a GraphQL `errors` array says does not exist, when every
/// error in it is Linear's `Entity not found: <type>` for that one type.
///
/// Why: #190 step 6 — a stored issue Linear no longer has must be told apart
/// from a failed request. Narrow on purpose: an empty array, any other
/// message, or two entity types is `None`, and the caller fails as before.
/// What: `Some("Issue")` for `[{"message": "Entity not found: Issue"}]`.
/// Test: `not_found_entity_is_narrow`.
fn not_found_entity(errors: &serde_json::Value) -> Option<String> {
    let mut entity: Option<&str> = None;
    for error in errors.as_array()? {
        let this = error["message"]
            .as_str()?
            .strip_prefix("Entity not found: ")?
            .trim();
        if this.is_empty() || entity.is_some_and(|e| e != this) {
            return None;
        }
        entity = Some(this);
    }
    entity.map(String::from)
}

/// Whether a Linear error body carries a `RATELIMITED` GraphQL error code
/// (#190 step 3). Linear's rate-limit documentation: HTTP 400 with
/// `errors[].extensions.code = "RATELIMITED"`.
fn is_ratelimited(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v["errors"].as_array().cloned())
        .is_some_and(|errors| {
            errors
                .iter()
                .any(|e| e["extensions"]["code"].as_str() == Some("RATELIMITED"))
        })
}

/// The wait until the exhausted Linear rate-limit window resets.
///
/// Why: #190 step 3 — retrying a RATELIMITED request before its window
/// resets only spends the retry budget.
/// What: for each window Linear documents (`X-RateLimit-Requests-*`,
/// `X-RateLimit-Complexity-*`, `X-RateLimit-Endpoint-Requests-*`) whose
/// `Remaining` header is 0 or less, the time from `now_ms` to its `Reset`
/// header (UTC epoch milliseconds); the longest such wait, never negative.
/// `None` when no window reports itself exhausted, so the policy's own
/// backoff applies. The retry budget still caps the sleep.
/// Test: `ratelimited_retry_waits_for_the_exhausted_window_reset`.
fn ratelimit_reset_delay(
    headers: &reqwest::header::HeaderMap,
    now_ms: i64,
) -> Option<std::time::Duration> {
    let number = |name: String| -> Option<i64> {
        headers
            .get(name.as_str())
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
    };
    ["requests", "complexity", "endpoint-requests"]
        .into_iter()
        .filter(|w| number(format!("x-ratelimit-{w}-remaining")).is_some_and(|r| r <= 0))
        .filter_map(|w| number(format!("x-ratelimit-{w}-reset")))
        .map(|reset| u64::try_from(reset.saturating_sub(now_ms)).unwrap_or(0))
        .max()
        .map(std::time::Duration::from_millis)
}

#[cfg(test)]
#[path = "bulk_tests.rs"]
pub(crate) mod tests;

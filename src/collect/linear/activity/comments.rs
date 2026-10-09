//! The incremental comments walk of `tga linear sync --comments` (#190).
//!
//! Why: live Linear data shows `Issue.updatedAt` does not reliably move when
//! a comment is created or edited (1,212 of 60,055 comments on one workspace
//! were newer than their issue), so the v36 pass, which re-read an issue's
//! comments only when the issue changed, missed them.
//! What: [`comments_filter`] and [`LinearClient::fetch_team_comments`] read
//! Linear's top-level `comments` connection for one team, filtered by the
//! comment's own `updatedAt`. The store side is
//! [`super::store::commit_team_comments`].
//! Test: `activity::comment_walk_tests`; `commands::linear::comment_cursor_tests`.

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

use super::super::bulk::PageGuard;
use super::super::client::LinearClient;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::retry::with_retry;

/// Comments requested per page: the largest page Linear accepted live.
pub const COMMENTS_PAGE_SIZE: usize = 250;

/// How far behind the stored cursor an incremental walk starts.
pub const COMMENT_OVERLAP: TimeDelta = TimeDelta::minutes(10);

/// The paginated workspace `comments` query: each comment with its issue.
/// `body` is read only to measure its length; it is never stored or logged.
pub const TEAM_COMMENTS_QUERY: &str = "query($first: Int!, $after: String, \
     $filter: CommentFilter, $orderBy: PaginationOrderBy, $includeArchived: Boolean) { \
     comments(first: $first, after: $after, filter: $filter, orderBy: $orderBy, \
     includeArchived: $includeArchived) { nodes { id createdAt updatedAt body \
     user { id name } parent { id } issue { id identifier team { key } } } \
     pageInfo { hasNextPage endCursor } } }";

/// The `filter` variable: comments on `team_key`'s issues, updated after
/// `updated_after` when given.
///
/// Why: #190 — the team filter keeps the walk inside the run's `--team` /
/// `--all-teams` scope; the `updatedAt` bound is the incremental part.
/// What: `{"issue": {"team": {"key": {"eq": team_key}}}}`, plus
/// `"updatedAt": {"gt": <RFC 3339, milliseconds, Z>}` when `updated_after` is
/// `Some`. `None` omits the bound: a full sweep of the team.
/// Test: `activity::comment_walk_tests::the_filter_scopes_to_the_team_and_bound`.
#[must_use]
pub fn comments_filter(team_key: &str, updated_after: Option<DateTime<Utc>>) -> serde_json::Value {
    let mut filter = serde_json::json!({ "issue": { "team": { "key": { "eq": team_key } } } });
    if let Some(after) = updated_after {
        filter["updatedAt"] =
            serde_json::json!({ "gt": after.to_rfc3339_opts(SecondsFormat::Millis, true) });
    }
    filter
}

impl LinearClient {
    /// Walk every page of the comments on `team_key`'s issues updated after
    /// `updated_after` (all of them when `None`).
    ///
    /// Why: #190 — the store commits the walk and moves the team's cursor in
    /// one step, so a cut walk would move the cursor past comments never
    /// read; the walk finishes or fails (Fail-Open check).
    /// What: sends [`TEAM_COMMENTS_QUERY`] with [`comments_filter`],
    /// `orderBy: createdAt` (a key an edit does not move) and
    /// `includeArchived: true`, [`COMMENTS_PAGE_SIZE`] per page, following
    /// `endCursor` until `hasNextPage` is false. Returns the nodes as Linear
    /// sent them. A 429/503 or RATELIMITED 400 is retried with backoff.
    /// Test: `activity::comment_walk_tests::the_walk_follows_every_page`,
    /// `activity::comment_walk_tests::a_malformed_page_fails_the_walk`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearBulkApi`] on a non-2xx, a GraphQL `errors`
    ///   array, or a page with no `comments.nodes` array.
    /// - [`CollectError::LinearPageInfoInvalid`] when a page has no boolean
    ///   `hasNextPage`, or says more follow with a null `endCursor`.
    /// - [`CollectError::PagingBudgetExceeded`] when the cursor stops moving.
    /// - [`CollectError::Http`] / [`CollectError::Throttled`] on transport
    ///   failure or spent retries.
    pub async fn fetch_team_comments(
        &self,
        team_key: &str,
        updated_after: Option<DateTime<Utc>>,
    ) -> Result<Vec<serde_json::Value>> {
        const ENDPOINT: &str = "linear/comments";
        let filter = comments_filter(team_key, updated_after);
        let mut guard = PageGuard::new(ENDPOINT, team_key);
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        for page_number in 1usize.. {
            let body = serde_json::json!({
                "query": TEAM_COMMENTS_QUERY,
                "variables": {
                    "first": COMMENTS_PAGE_SIZE,
                    "after": after,
                    "filter": filter,
                    "orderBy": "createdAt",
                    "includeArchived": true,
                },
            });
            let mut data = with_retry("linear comments page", &self.retry, &self.budget, || {
                self.post_bulk(&body, team_key, page_number)
            })
            .await?;
            let page = data["comments"].take();
            // #190: a page with no `nodes` is an error; read as empty, the
            // cursor would move past comments never read.
            let Some(rows) = page["nodes"].as_array() else {
                return Err(CollectError::LinearBulkApi {
                    status: 200,
                    team_key: team_key.to_string(),
                    page: page_number,
                    message: "response has no comments.nodes array".to_string(),
                });
            };
            let count = rows.len();
            nodes.extend(rows.iter().cloned());
            let info = &page["pageInfo"];
            let Some(has_next) = info["hasNextPage"].as_bool() else {
                return Err(CollectError::LinearPageInfoInvalid {
                    endpoint: ENDPOINT,
                    key: team_key.to_string(),
                    page: page_number,
                    problem: "response has no boolean pageInfo.hasNextPage",
                });
            };
            match guard.advance(
                has_next,
                info["endCursor"].as_str().map(String::from),
                count,
                page_number,
            )? {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(nodes)
    }
}

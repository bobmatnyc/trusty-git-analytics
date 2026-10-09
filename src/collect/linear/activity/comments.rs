//! The incremental comments walk of `tga linear sync --comments` (#190).
//!
//! Why: live Linear data shows `Issue.updatedAt` does not reliably move when
//! a comment is created or edited (1,212 of 60,055 comments on one workspace
//! were newer than their issue), so the v36 pass, which re-read an issue's
//! comments only when the issue changed, missed them.
//! What: [`comments_filter`] and [`LinearClient::fetch_team_comments`] read
//! Linear's top-level `comments` connection for one team, filtered by the
//! comment's own `updatedAt`. #190: [`LinearClient::fetch_issue_comments`]
//! reads, with no bound, every comment of the issues that moved into the team
//! ([`super::due`]). The store side is
//! [`super::store::commit_team_comments_with_due`].
//! Test: `activity::comment_walk_tests`; `commands::linear::comment_cursor_tests`.

use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};

use super::super::bulk::PageGuard;
use super::super::client::LinearClient;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::retry::with_retry;

/// Comments requested per page: the largest page Linear accepted live.
pub const COMMENTS_PAGE_SIZE: usize = 250;

/// How far behind the stored cursor an incremental walk starts.
///
/// #190: the cursor is a Linear server time (a comment `updatedAt`) capped at
/// the local clock's walk start. The overlap also absorbs skew between the
/// two clocks: a local clock up to 10 minutes behind Linear's still re-reads
/// every comment updated during the previous walk. More skew than that can
/// skip such comments until `--backfill`.
pub const COMMENT_OVERLAP: TimeDelta = TimeDelta::minutes(10);

/// Issue ids per request of [`LinearClient::fetch_issue_comments`]. Each
/// batch is one paged walk at [`COMMENTS_PAGE_SIZE`] comments per page.
pub const DUE_ISSUES_PER_REQUEST: usize = 100;

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

/// The `filter` variable of [`LinearClient::fetch_issue_comments`]: every
/// comment on the issues with these ids, with no `updatedAt` bound.
#[must_use]
pub fn issue_ids_filter(issue_ids: &[String]) -> serde_json::Value {
    serde_json::json!({ "issue": { "id": { "in": issue_ids } } })
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
        self.walk_comments(team_key, &comments_filter(team_key, updated_after))
            .await
    }

    /// Walk every page of every comment on the issues `issue_ids`, with no
    /// `updatedAt` bound; `team_key` labels errors.
    ///
    /// Why: #190 — a comment on an issue that moved into the team can be
    /// older than the team walk's bound; the comments pass reads these issues
    /// in full.
    /// What: one [`TEAM_COMMENTS_QUERY`] walk per batch of
    /// [`DUE_ISSUES_PER_REQUEST`] ids, filtered by [`issue_ids_filter`], same
    /// paging, ordering, archived handling and retries as
    /// [`LinearClient::fetch_team_comments`]. Cost: one request per batch
    /// plus one per further 250 comments. No ids sends no request.
    /// Test: `commands::linear::comment_cursor_tests::a_moved_issues_old_comments_are_read_in_full`,
    /// `commands::linear::comment_cursor_tests::a_failed_moved_issue_walk_fails_the_run_and_stays_due`.
    ///
    /// # Errors
    ///
    /// As [`LinearClient::fetch_team_comments`]; any failed batch fails the
    /// whole read.
    pub async fn fetch_issue_comments(
        &self,
        team_key: &str,
        issue_ids: &[String],
    ) -> Result<Vec<serde_json::Value>> {
        let mut nodes = Vec::new();
        for batch in issue_ids.chunks(DUE_ISSUES_PER_REQUEST) {
            nodes.extend(
                self.walk_comments(team_key, &issue_ids_filter(batch))
                    .await?,
            );
        }
        Ok(nodes)
    }

    /// Walk every page of the workspace `comments` query under `filter`.
    async fn walk_comments(
        &self,
        team_key: &str,
        filter: &serde_json::Value,
    ) -> Result<Vec<serde_json::Value>> {
        const ENDPOINT: &str = "linear/comments";
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

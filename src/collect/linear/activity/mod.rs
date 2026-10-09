//! Linear issue history and comments for `tga linear sync --history
//! --comments` (#190 step 6).
//!
//! Why: JIRA parity — `tga jira sync` stores every status transition
//! (`fact_ticket_transitions`) and comment metadata
//! (`fact_jira_comment_detail`); a Linear issue row holds only its current
//! state and three lifecycle stamps.
//! What: [`ActivityKind`] names the two per-issue connections;
//! [`LinearClient::fetch_issue_activity`] walks one issue's `history` or
//! `comments` to the end and returns the raw GraphQL nodes; [`store`] picks
//! the issues whose activity is out of date and writes their rows. Linear has
//! no workspace-wide history list, so the walk is per issue.
//! Test: `tests` below; `commands::linear::activity_sync_tests`.

pub mod store;

use super::bulk::PageGuard;
use super::client::LinearClient;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::retry::with_retry;

/// Nodes requested per page of one issue's history or comments.
pub const ACTIVITY_PAGE_SIZE: usize = 50;

/// `IssueHistory`: when, who, and the workflow state on each side. An entry
/// that did not change the state has a null `toState`.
pub const HISTORY_FIELDS: &str =
    "id createdAt actor { id name } fromState { name type } toState { name type }";

/// `Comment`: author, thread parent and timestamps. `body` is read only to
/// measure its length; it is never stored.
pub const COMMENT_FIELDS: &str = "id createdAt updatedAt body user { id name } parent { id }";

/// One per-issue Linear connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ActivityKind {
    /// `Issue.history` → `fact_linear_transitions`.
    History,
    /// `Issue.comments` → `fact_linear_comment_detail`.
    Comments,
}

impl ActivityKind {
    /// The `Issue` field that lists this connection.
    #[must_use]
    pub fn connection(self) -> &'static str {
        match self {
            ActivityKind::History => "history",
            ActivityKind::Comments => "comments",
        }
    }

    /// The table its rows are stored in.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            ActivityKind::History => "fact_linear_transitions",
            ActivityKind::Comments => "fact_linear_comment_detail",
        }
    }

    /// The [`PageGuard`] endpoint label.
    fn endpoint(self) -> &'static str {
        match self {
            ActivityKind::History => "linear/issue.history",
            ActivityKind::Comments => "linear/issue.comments",
        }
    }

    /// The selection set for one node.
    fn fields(self) -> &'static str {
        match self {
            ActivityKind::History => HISTORY_FIELDS,
            ActivityKind::Comments => COMMENT_FIELDS,
        }
    }
}

/// The paginated per-issue query for `kind`.
///
/// `includeArchived` is sent so an archived comment or history entry is not
/// dropped from an otherwise complete walk.
#[must_use]
pub fn query_for(kind: ActivityKind) -> String {
    format!(
        "query($id: String!, $first: Int!, $after: String, $includeArchived: Boolean) {{ \
         issue(id: $id) {{ {conn}(first: $first, after: $after, \
         includeArchived: $includeArchived) {{ \
         nodes {{ {fields} }} pageInfo {{ hasNextPage endCursor }} }} }} }}",
        conn = kind.connection(),
        fields = kind.fields(),
    )
}

/// An issue whose history or comments a sync reads.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ActivityTarget {
    /// Linear's issue id (UUID); the walk queries `issue(id:)` with it.
    pub issue_id: String,
    /// The identifier the issue carries now, e.g. `ENG-123`.
    pub identifier: String,
    /// The issue's current team key.
    pub team_key: String,
    /// The issue's `updatedAt` as stored in `linear_issues` (RFC 3339). A
    /// successful write records it, so the issue is not read again until it
    /// changes.
    pub updated_at: Option<String>,
}

impl ActivityTarget {
    /// Build a target.
    #[must_use]
    pub fn new(
        issue_id: &str,
        identifier: &str,
        team_key: &str,
        updated_at: Option<String>,
    ) -> Self {
        Self {
            issue_id: issue_id.to_string(),
            identifier: identifier.to_string(),
            team_key: team_key.to_string(),
            updated_at,
        }
    }
}

/// Re-label a bulk-page error with the issue and connection.
fn activity_error(err: CollectError, kind: ActivityKind, identifier: &str) -> CollectError {
    match err {
        CollectError::LinearBulkApi {
            status,
            page,
            message,
            ..
        } => CollectError::LinearActivityApi {
            status,
            identifier: identifier.to_string(),
            connection: kind.connection(),
            page,
            message,
        },
        other => other,
    }
}

impl LinearClient {
    /// Walk every page of one issue's history or comments; `None` when
    /// Linear says the issue does not exist.
    ///
    /// Why: #190 step 6 — the store replaces an issue's rows with what this
    /// returns, so a cut list would delete rows; the walk finishes or fails.
    /// An issue Linear no longer has is an answer, not a failure (owner
    /// ruling D28): the caller counts and tombstones it.
    /// What: sends [`query_for`] with `issue(id:)` = `target.issue_id`,
    /// follows `endCursor` until `hasNextPage` is false, and returns every
    /// node as Linear sent it. A 429/503 or RATELIMITED 400 is retried with
    /// backoff. Returns `Ok(None)` only for Linear's explicit not-found
    /// answer on any page: `data.issue` present and null, or a GraphQL
    /// `errors` array that is all `Entity not found: Issue`. Any other page
    /// with no `nodes` array, or no boolean `hasNextPage`, is an error, never
    /// the last page. No cap.
    /// Test: `activity::tests::history_walk_follows_every_page`,
    /// `activity::tests::comments_walk_follows_every_page`,
    /// `activity::tests::a_page_error_fails_the_walk`,
    /// `activity::tests::only_linear_not_found_reads_as_a_missing_issue`,
    /// `activity::tests::a_page_without_page_info_fails_the_walk`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearActivityApi`] on a non-2xx, a GraphQL
    ///   `errors` array other than the not-found one, or a response with no
    ///   `nodes` array.
    /// - [`CollectError::PagingBudgetExceeded`] when the cursor stops moving.
    /// - [`CollectError::LinearPageInfoInvalid`] when a page has no boolean
    ///   `hasNextPage`, or says more follow with a null `endCursor`.
    /// - [`CollectError::Http`] / [`CollectError::Throttled`] on transport
    ///   failure or spent retries.
    pub async fn fetch_issue_activity(
        &self,
        target: &ActivityTarget,
        kind: ActivityKind,
    ) -> Result<Option<Vec<serde_json::Value>>> {
        let query = query_for(kind);
        let connection = kind.connection();
        let mut guard = PageGuard::new(kind.endpoint(), &target.identifier);
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        for page_number in 1usize.. {
            let body = serde_json::json!({
                "query": query,
                "variables": {
                    "id": target.issue_id,
                    "first": ACTIVITY_PAGE_SIZE,
                    "after": after,
                    "includeArchived": true,
                },
            });
            let fetched = with_retry(
                "linear issue activity page",
                &self.retry,
                &self.budget,
                || self.post_bulk(&body, &target.identifier, page_number),
            )
            .await;
            let mut data = match fetched {
                // #190 step 6 (D28): Linear's not-found answer for this issue.
                Err(CollectError::LinearNotFound { entity, .. }) if entity == "Issue" => {
                    return Ok(None);
                }
                other => other.map_err(|e| activity_error(e, kind, &target.identifier))?,
            };
            // #190 step 6 (D28): `issue: null` is the same answer. A missing
            // `issue` key, or an issue with no connection, is not.
            if data.get("issue").is_some_and(serde_json::Value::is_null) {
                return Ok(None);
            }
            let page = data["issue"][connection].take();
            // #190 step 6: a missing `nodes` array is an error; read as
            // empty, the store would delete the issue's rows.
            let Some(rows) = page["nodes"].as_array() else {
                return Err(CollectError::LinearActivityApi {
                    status: 200,
                    identifier: target.identifier.clone(),
                    connection,
                    page: page_number,
                    message: format!("response has no issue.{connection}.nodes array"),
                });
            };
            let count = rows.len();
            nodes.extend(rows.iter().cloned());
            let info = &page["pageInfo"];
            let Some(has_next) = info["hasNextPage"].as_bool() else {
                return Err(CollectError::LinearPageInfoInvalid {
                    endpoint: kind.endpoint(),
                    key: target.identifier.clone(),
                    page: page_number,
                    problem: "response has no boolean pageInfo.hasNextPage",
                });
            };
            let next = guard.advance(
                has_next,
                info["endCursor"].as_str().map(String::from),
                count,
                page_number,
            )?;
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(Some(nodes))
    }
}

#[cfg(test)]
pub(crate) mod tests;

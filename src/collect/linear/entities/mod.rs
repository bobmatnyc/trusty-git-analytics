//! Linear reference entities for `tga linear sync --entities` (#190).
//!
//! Why: an issue row holds only the ids of its project, cycle, labels,
//! assignee and team. Reports need the objects behind those ids — a
//! project's target date, a cycle's scope, a team's estimation scale — and
//! archived ones too, because archived issues point at them.
//! What: [`EntityKind`] names the six entity sets;
//! [`LinearClient::fetch_entity_nodes`] walks one set to the end, archived
//! nodes included, and returns the raw GraphQL nodes; [`store`] writes them.
//! Every set is a full refresh: the sets are small, and a cycle's scope
//! history changes without a reliable `updatedAt` bump.
//! Test: `tests` below; `commands::linear::entity_sync_tests`.

pub mod queries;
pub mod store;

use super::bulk::PageGuard;
use super::client::LinearClient;
use crate::collect::errors::{CollectError, Result};
use crate::collect::jira::retry::with_retry;

/// Nodes requested per page. Fifty keeps a page of projects, each with its
/// nested `teams` connection, inside Linear's query complexity limit.
pub const ENTITY_PAGE_SIZE: usize = 50;

/// One Linear reference-entity set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EntityKind {
    /// `teams` → `linear_teams`.
    Teams,
    /// `users` → `linear_users`.
    Users,
    /// `issueLabels` → `linear_labels`.
    Labels,
    /// `projects` → `linear_projects`.
    Projects,
    /// `projectMilestones` → `linear_milestones`.
    Milestones,
    /// `cycles` → `linear_cycles`.
    Cycles,
}

impl EntityKind {
    /// Every set, in sync order: the sets other sets point at come first.
    pub const ALL: [EntityKind; 6] = [
        EntityKind::Teams,
        EntityKind::Users,
        EntityKind::Labels,
        EntityKind::Projects,
        EntityKind::Milestones,
        EntityKind::Cycles,
    ];

    /// The GraphQL root field that lists this set.
    #[must_use]
    pub fn root_field(self) -> &'static str {
        match self {
            EntityKind::Teams => "teams",
            EntityKind::Users => "users",
            EntityKind::Labels => "issueLabels",
            EntityKind::Projects => "projects",
            EntityKind::Milestones => "projectMilestones",
            EntityKind::Cycles => "cycles",
        }
    }

    /// The table this set is stored in.
    #[must_use]
    pub fn table(self) -> &'static str {
        match self {
            EntityKind::Teams => "linear_teams",
            EntityKind::Users => "linear_users",
            EntityKind::Labels => "linear_labels",
            EntityKind::Projects => "linear_projects",
            EntityKind::Milestones => "linear_milestones",
            EntityKind::Cycles => "linear_cycles",
        }
    }

    /// Short name for reports and `linear_entity_sync_state.entity`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            EntityKind::Teams => "teams",
            EntityKind::Users => "users",
            EntityKind::Labels => "labels",
            EntityKind::Projects => "projects",
            EntityKind::Milestones => "milestones",
            EntityKind::Cycles => "cycles",
        }
    }

    /// The [`PageGuard`] endpoint label.
    fn endpoint(self) -> &'static str {
        match self {
            EntityKind::Teams => "linear/teams",
            EntityKind::Users => "linear/users",
            EntityKind::Labels => "linear/issueLabels",
            EntityKind::Projects => "linear/projects",
            EntityKind::Milestones => "linear/projectMilestones",
            EntityKind::Cycles => "linear/cycles",
        }
    }
}

/// Whether a GraphQL node carries an `archivedAt`.
#[must_use]
pub fn is_archived_node(node: &serde_json::Value) -> bool {
    node["archivedAt"].is_string()
}

/// Re-label a bulk-page error with the entity, so the message does not
/// name a team.
fn entity_error(err: CollectError, kind: EntityKind) -> CollectError {
    match err {
        CollectError::LinearBulkApi {
            status,
            page,
            message,
            ..
        } => CollectError::LinearEntityApi {
            status,
            entity: kind.root_field(),
            page,
            message,
        },
        other => other,
    }
}

/// Fail when a project's nested `teams` list was cut at one page.
fn check_nested(kind: EntityKind, node: &serde_json::Value) -> Result<()> {
    if kind == EntityKind::Projects
        && node["teams"]["pageInfo"]["hasNextPage"].as_bool() == Some(true)
    {
        return Err(CollectError::LinearNestedConnectionTruncated {
            entity: kind.root_field(),
            id: node["id"].as_str().unwrap_or("").to_string(),
            connection: "teams",
            limit: queries::PROJECT_TEAMS_LIMIT,
        });
    }
    Ok(())
}

impl LinearClient {
    /// Walk every page of one entity set, archived nodes included.
    ///
    /// Why: #190 — the store writes a set in one transaction, so the walk
    /// must finish before anything is written; a partial set is never
    /// returned.
    /// What: sends [`queries::query_for`] with `includeArchived: true`,
    /// `orderBy: createdAt` and (for users) `includeDisabled: true`, follows
    /// `endCursor` until `hasNextPage` is false, and returns every node as
    /// Linear sent it. A 429/503 is retried with backoff. A page that does
    /// not say whether more follow is an error, never the last page.
    /// Test: `entities::tests::every_entity_query_sends_include_archived`,
    /// `entities::tests::fetch_walks_every_page`,
    /// `entities::tests::a_cut_nested_team_list_fails_the_walk`,
    /// `entities::tests::a_response_without_nodes_is_an_error`,
    /// `entities::tests::a_page_without_page_info_fails_the_walk`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::LinearEntityApi`] on a non-2xx, a GraphQL `errors`
    ///   array, or a response with no `nodes` array.
    /// - [`CollectError::LinearNestedConnectionTruncated`] when a project has
    ///   more teams than the nested page holds.
    /// - [`CollectError::PagingBudgetExceeded`] when the cursor stops moving.
    /// - [`CollectError::LinearPageInfoInvalid`] when a page has no boolean
    ///   `hasNextPage`, or says more follow with a null `endCursor`.
    /// - [`CollectError::Http`] / [`CollectError::Throttled`] on transport
    ///   failure or spent retries.
    pub async fn fetch_entity_nodes(&self, kind: EntityKind) -> Result<Vec<serde_json::Value>> {
        let query = queries::query_for(kind);
        let root = kind.root_field();
        let mut guard = PageGuard::new(kind.endpoint(), "*");
        let mut nodes = Vec::new();
        let mut after: Option<String> = None;
        for page_number in 1usize.. {
            let mut variables = serde_json::json!({
                "first": ENTITY_PAGE_SIZE,
                "after": after,
                // #190: archived entities are part of the reference set.
                "includeArchived": true,
                "orderBy": "createdAt",
            });
            if kind == EntityKind::Users {
                variables["includeDisabled"] = serde_json::Value::Bool(true);
            }
            let body = serde_json::json!({ "query": query, "variables": variables });
            let mut data = with_retry("linear entity page", &self.retry, &self.budget, || {
                self.post_bulk(&body, root, page_number)
            })
            .await
            .map_err(|e| entity_error(e, kind))?;
            let connection = data[root].take();
            // #190: a page with no `nodes` array is an error, never an empty set.
            let Some(page) = connection["nodes"].as_array() else {
                return Err(CollectError::LinearEntityApi {
                    status: 200,
                    entity: root,
                    page: page_number,
                    message: format!("response has no {root}.nodes array"),
                });
            };
            for node in page {
                check_nested(kind, node)?;
            }
            let rows = page.len();
            nodes.extend(page.iter().cloned());
            let info = &connection["pageInfo"];
            // #190: a page that does not say whether more follow fails the
            // set. Read as the last page, it would let the store mark every
            // unseen row removed.
            let Some(has_next) = info["hasNextPage"].as_bool() else {
                return Err(CollectError::LinearPageInfoInvalid {
                    endpoint: kind.endpoint(),
                    key: "*".to_string(),
                    page: page_number,
                    problem: "response has no boolean pageInfo.hasNextPage",
                });
            };
            let next = guard.advance(
                has_next,
                info["endCursor"].as_str().map(String::from),
                rows,
                page_number,
            )?;
            match next {
                Some(cursor) => after = Some(cursor),
                None => break,
            }
        }
        Ok(nodes)
    }
}

#[cfg(test)]
pub(crate) mod tests;

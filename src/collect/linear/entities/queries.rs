//! GraphQL field selections for the Linear reference-entity walks (#190).
//!
//! Why: one place to check every selection against Linear's public schema
//! (`Team`, `User`, `IssueLabel`, `Project`, `ProjectMilestone`, `Cycle`).
//! A field the schema does not have fails the whole query, so these lists
//! hold only the fields `entities::store` writes to a column.
//! What: one `*_FIELDS` selection per entity and [`query_for`], which wraps a
//! selection in the paginated root query.
//! Test: `entities::tests::every_entity_query_sends_include_archived`.

use super::EntityKind;

/// `Team`: key, name, estimation settings and cycle settings.
pub const TEAM_FIELDS: &str = "id key name private timezone createdAt updatedAt archivedAt \
     issueEstimationType issueEstimationAllowZero issueEstimationExtended defaultIssueEstimate \
     cyclesEnabled cycleDuration cycleCooldownTime cycleStartDay upcomingCycleCount";

/// `User`: identity and account state.
pub const USER_FIELDS: &str =
    "id name displayName email active admin guest createdAt updatedAt archivedAt";

/// `IssueLabel`: a `null` team is a workspace-level label.
pub const LABEL_FIELDS: &str =
    "id name color isGroup parent { id } team { id } createdAt updatedAt archivedAt";

/// `first:` sent for the nested `Project.teams` connection. A project with
/// more teams than this fails the walk instead of storing a cut list.
pub const PROJECT_TEAMS_LIMIT: usize = 100;

/// `Project`: lifecycle dates, lead and team ids. `state` is the legacy
/// state string; `status` is the custom status that replaces it.
pub const PROJECT_FIELDS: &str = "id name slugId url state status { name type } progress \
     startDate targetDate startedAt completedAt canceledAt createdAt updatedAt archivedAt \
     lead { id } \
     teams(first: 100, includeArchived: true) { nodes { id } pageInfo { hasNextPage } }";

/// `ProjectMilestone`: the project it belongs to and its target date.
pub const MILESTONE_FIELDS: &str =
    "id name targetDate sortOrder createdAt updatedAt archivedAt project { id }";

/// `Cycle`: team, number, dates and the daily scope/completion histories.
pub const CYCLE_FIELDS: &str = "id number name startsAt endsAt completedAt progress \
     createdAt updatedAt archivedAt team { id } \
     issueCountHistory completedIssueCountHistory scopeHistory completedScopeHistory";

/// The full paginated query for `kind`.
///
/// Every root query sends `includeArchived` and orders by `createdAt`, which
/// never changes, so an entity edited during the walk keeps its page
/// position. `users` also sends `includeDisabled`: Linear omits deactivated
/// users without it.
#[must_use]
pub fn query_for(kind: EntityKind) -> String {
    let (fields, extra_var, extra_arg) = match kind {
        EntityKind::Teams => (TEAM_FIELDS, "", ""),
        EntityKind::Users => (
            USER_FIELDS,
            ", $includeDisabled: Boolean",
            ", includeDisabled: $includeDisabled",
        ),
        EntityKind::Labels => (LABEL_FIELDS, "", ""),
        EntityKind::Projects => (PROJECT_FIELDS, "", ""),
        EntityKind::Milestones => (MILESTONE_FIELDS, "", ""),
        EntityKind::Cycles => (CYCLE_FIELDS, "", ""),
    };
    let root = kind.root_field();
    // #190: `includeArchived` on every root; Linear omits archived nodes
    // otherwise.
    format!(
        "query($first: Int!, $after: String, $includeArchived: Boolean, \
         $orderBy: PaginationOrderBy{extra_var}) {{ \
         {root}(first: $first, after: $after, includeArchived: $includeArchived, \
         orderBy: $orderBy{extra_arg}) {{ \
         nodes {{ {fields} }} pageInfo {{ hasNextPage endCursor }} }} }}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The nested `first:` in the selection is the limit the walk enforces.
    #[test]
    fn project_teams_limit_matches_the_selection() {
        assert!(PROJECT_FIELDS.contains(&format!("teams(first: {PROJECT_TEAMS_LIMIT},")));
    }
}

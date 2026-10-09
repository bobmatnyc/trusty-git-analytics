//! Linear project management integration.
//!
//! Provides a GraphQL client for the Linear API to enrich commits that
//! reference Linear issue identifiers (e.g. `ENG-123`, `FE-456`) with the
//! corresponding issue title, status, team, assignee, and priority, and the
//! bulk walk behind `tga linear sync`, and (#190) the reference entities
//! (teams, users, labels, projects, milestones, cycles) it can also store,
//! and (#190 step 6) each issue's state history and comment metadata.

// #190 step 6: per-issue history and comments.
pub mod activity;
pub mod bulk;
pub mod client;
pub mod entities;
pub mod issue;
pub(crate) mod projection;
pub mod store;
pub mod sync;

pub use activity::{ActivityKind, ActivityTarget};
pub use bulk::{IssueQuery, LinearTeam};
pub use client::{store_linear_issues, LinearClient, LinearIssue, LinearIssuesPage};
pub use entities::EntityKind;
pub use store::{
    plan_linear_issues, upsert_linear_issues, upsert_linear_issues_in, ChangeCounts, IssueChange,
};

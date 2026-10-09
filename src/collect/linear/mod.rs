//! Linear project management integration.
//!
//! Provides a GraphQL client for the Linear API to enrich commits that
//! reference Linear issue identifiers (e.g. `ENG-123`, `FE-456`) with the
//! corresponding issue title, status, team, assignee, and priority, and the
//! bulk walk behind `tga linear sync`.

pub mod bulk;
pub mod client;
pub mod issue;
pub mod store;
pub mod sync;

pub use bulk::{IssueQuery, LinearTeam};
pub use client::{store_linear_issues, LinearClient, LinearIssue, LinearIssuesPage};
pub use store::{plan_linear_issues, upsert_linear_issues, ChangeCounts, IssueChange};

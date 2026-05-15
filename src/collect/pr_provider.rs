//! Provider-agnostic interface for pull-request collection.
//!
//! The pipeline supports more than one source of pull-request data (GitHub
//! today, Bitbucket Cloud next). Concrete clients implement [`PrProvider`] so
//! the orchestrator in [`crate::collect::collector`] can iterate over a
//! homogeneous list of providers without caring which backend is which.
//!
//! The returned [`PullRequest`] rows are already mapped into the project's
//! internal shape — backend-specific JSON never escapes the client module.

use async_trait::async_trait;

use crate::collect::errors::Result;
use crate::core::db::Database;
use crate::core::models::PullRequest;

/// A source of pull-request metadata (GitHub, Bitbucket, …).
///
/// Implementors are expected to be cheap to construct and `Send + Sync` so
/// the pipeline can run multiple providers concurrently via
/// `tokio::task::JoinSet`. `store_pull_requests` runs on the main task — it
/// is not `async` because it talks to a synchronous `rusqlite::Connection`.
#[async_trait]
pub trait PrProvider: Send + Sync {
    /// Stable, lowercase short name for logs and error messages
    /// (e.g. `"github"`, `"bitbucket"`).
    fn name(&self) -> &str;

    /// Fetch every pull request the provider can see for the configured
    /// repository.
    ///
    /// # Errors
    ///
    /// Implementors should return [`crate::collect::CollectError::Http`] on
    /// transport failures and [`crate::collect::CollectError::Json`] on
    /// payload parse failures.
    async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>>;

    /// Persist a batch of pull-request rows to the database.
    ///
    /// Returns the number of rows written.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
    fn store_pull_requests(&self, db: &Database, prs: &[PullRequest])
        -> crate::core::Result<usize>;
}

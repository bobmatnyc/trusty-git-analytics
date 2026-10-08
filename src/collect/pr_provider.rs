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
/// Why: the collector needs to drive multiple PR sources concurrently
/// without caring which backend each one is; a single trait makes the
/// per-provider client interchangeable.
/// What: defines `name`, async `fetch_pull_requests`, and synchronous
/// `store_pull_requests` (sync because rusqlite is not async).
/// Test: covered by the per-provider client tests
/// (`collect::github::client`, `collect::bitbucket::client`,
/// `collect::azdo::client`) that implement and exercise this trait.
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

    /// Bounds this provider hit while fetching, in operator-facing wording.
    ///
    /// Why (#6084): a provider that stops at a page or budget cap returns rows
    /// that read exactly like a complete fetch. Reporting the stop is what
    /// keeps a trimmed sweep from being presented as the whole repository.
    /// What: empty by default — a provider with no caps has nothing to say.
    /// Overridden by [`crate::collect::github::GitHubClient`].
    /// Test: `crate::collect::github::client_tests::a_listing_that_never_ends_stops_at_the_page_cap_and_says_so`.
    fn fetch_notices(&self) -> Vec<String> {
        Vec::new()
    }

    /// Per-repository fetch failures from the last `fetch_pull_requests`, with
    /// the severity the run must record them at.
    ///
    /// Why (#146): a provider that skips a failed repository and returns the
    /// rest answers `Ok`, so without this the skip is invisible to the run's
    /// fault list and exit code.
    /// What: empty by default. Overridden by
    /// [`crate::collect::github::GitHubClient`]: a 404 is an
    /// [`crate::collect::FaultSeverity::ItemSkipped`] warning, any other
    /// failure a [`crate::collect::FaultSeverity::StageFailed`].
    /// Test: `crate::collect::pr_pipeline::tests::a_pr_list_404_is_a_counted_warning_not_a_stage_failure`,
    /// `crate::collect::pr_pipeline::tests::a_pr_list_403_fails_the_stage_once`.
    fn fetch_faults(&self) -> Vec<crate::collect::CollectionFault> {
        Vec::new()
    }
}

//! End-to-end Stage 1 collection pipeline.
//!
//! Orchestrates git extraction, identity resolution, and optional GitHub
//! and JIRA fetches against a configured [`tga_core::config::Config`].

use tga_core::config::Config;
use tga_core::db::Database;
use tracing::{info, warn};

use crate::errors::Result;
use crate::git::GitCollector;
use crate::github::GitHubClient;
use crate::identity::IdentityResolver;

/// Aggregate statistics for a single pipeline run.
#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    /// Number of new commit rows written across all repositories.
    pub commits_collected: usize,
    /// Number of distinct authors observed and upserted.
    pub authors_resolved: usize,
    /// Number of PR rows written (zero if GitHub fetch disabled).
    pub prs_fetched: usize,
    /// Per-repo error messages encountered (non-fatal).
    pub errors: Vec<String>,
}

/// Top-level Stage 1 orchestrator.
pub struct CollectionPipeline {
    config: Config,
}

impl CollectionPipeline {
    /// Construct a new pipeline from a validated [`Config`].
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Borrow the underlying configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Run the full collection sequence against `db`.
    ///
    /// Each repository is processed sequentially; per-repo failures are
    /// recorded in [`CollectionStats::errors`] but do not abort the run.
    ///
    /// # Errors
    ///
    /// Returns a non-recoverable [`crate::CollectError`] only for failures
    /// outside the per-repo loop (e.g. GitHub auth setup failure when PR
    /// fetch is enabled and a single repo is configured).
    pub async fn run(&self, db: &mut Database) -> Result<CollectionStats> {
        let mut stats = CollectionStats::default();

        let resolver = IdentityResolver::new(self.config.team.as_ref());

        for repo_cfg in &self.config.repositories {
            let collector = match GitCollector::new(repo_cfg) {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("failed to open repo {}: {e}", repo_cfg.path.display());
                    warn!("{msg}");
                    stats.errors.push(msg);
                    continue;
                }
            };
            let repo_name = collector.name().to_string();
            match collector.collect(db) {
                Ok(n) => {
                    info!(repo = %repo_name, commits = n, "extracted");
                    stats.commits_collected += n;
                }
                Err(e) => {
                    let msg = format!("collection failed for {repo_name}: {e}");
                    warn!("{msg}");
                    stats.errors.push(msg);
                }
            }
        }

        // Backfill authors from observed commits.
        stats.authors_resolved = self.upsert_observed_authors(db, &resolver)?;

        // Optional: GitHub PR fetch.
        if let Some(gh_cfg) = &self.config.github {
            if gh_cfg.fetch_prs {
                match GitHubClient::new(gh_cfg) {
                    Ok(gh) => match gh.fetch_pull_requests().await {
                        Ok(prs) => match gh.store_pull_requests(db, &prs) {
                            Ok(n) => {
                                info!(prs = n, "stored pull requests");
                                stats.prs_fetched += n;
                            }
                            Err(e) => {
                                stats.errors.push(format!("PR store failed: {e}"));
                            }
                        },
                        Err(e) => {
                            stats.errors.push(format!("PR fetch failed: {e}"));
                        }
                    },
                    Err(e) => {
                        stats.errors.push(format!("GitHub client init failed: {e}"));
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Read distinct `(author_name, author_email)` pairs from `commits`
    /// and upsert them via the resolver, then link `commits.author_id`.
    fn upsert_observed_authors(
        &self,
        db: &mut Database,
        resolver: &IdentityResolver,
    ) -> Result<usize> {
        // Collect distinct pairs first to avoid holding a Statement across
        // mutating calls.
        let pairs: Vec<(String, String)> = {
            let conn = db.connection();
            let mut stmt = conn.prepare(
                "SELECT DISTINCT author_name, author_email FROM commits WHERE author_id IS NULL",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut out = Vec::new();
            for r in rows {
                out.push(r?);
            }
            out
        };

        let mut count = 0usize;
        for (name, email) in pairs {
            let author_id = resolver.upsert_author(db, &name, &email)?;
            db.connection().execute(
                "UPDATE commits SET author_id = ?1 \
                 WHERE author_id IS NULL AND author_name = ?2 AND author_email = ?3",
                rusqlite::params![author_id, name, email],
            )?;
            count += 1;
        }
        Ok(count)
    }
}

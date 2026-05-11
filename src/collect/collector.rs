//! End-to-end Stage 1 collection pipeline.
//!
//! Orchestrates git extraction, identity resolution, and optional GitHub
//! and JIRA fetches against a configured [`crate::core::config::Config`].

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use tracing::{info, warn};

use crate::collect::azdo::AzureDevOpsClient;
use crate::collect::errors::Result;
use crate::collect::git::GitCollector;
use crate::collect::github::GitHubClient;
use crate::collect::identity::IdentityResolver;
use crate::collect::linear::LinearClient;
use crate::collect::weeks::{clamp_week_to_range, weeks_in_range};
use crate::core::config::Config;
use crate::core::db::{self, Database};

/// Aggregate statistics for a single pipeline run.
#[derive(Debug, Clone, Default)]
pub struct CollectionStats {
    /// Number of new commit rows written across all repositories.
    pub commits_collected: usize,
    /// Number of distinct authors observed and upserted.
    pub authors_resolved: usize,
    /// Number of PR rows written (zero if GitHub fetch disabled).
    pub prs_fetched: usize,
    /// Number of Linear issues fetched (0 if Linear not configured).
    pub linear_issues_fetched: usize,
    /// Number of `(repo, week)` pairs that were collected this run.
    pub weeks_collected: usize,
    /// Number of `(repo, week)` pairs skipped because already present in
    /// `collection_runs` (and `force` was false).
    pub weeks_skipped: usize,
    /// Per-repo error messages encountered (non-fatal).
    pub errors: Vec<String>,
}

/// Top-level Stage 1 orchestrator.
pub struct CollectionPipeline {
    config: Config,
    force: bool,
}

impl CollectionPipeline {
    /// Construct a new pipeline from a validated [`Config`].
    pub fn new(config: Config) -> Self {
        Self {
            config,
            force: false,
        }
    }

    /// Enable forced re-collection: every `(repo, ISO-week)` pair is
    /// collected regardless of whether `collection_runs` already has a row
    /// for it.
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
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
    /// Returns a non-recoverable [`crate::collect::CollectError`] only for
    /// failures outside the per-repo loop.
    pub async fn run(&self, db: &mut Database) -> Result<CollectionStats> {
        let mut stats = CollectionStats::default();

        let resolver = IdentityResolver::from_config(&self.config);

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
            self.collect_repo_by_week(db, &collector, &mut stats);
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

        // Optional: Azure DevOps connection probe (non-fatal).
        // Phase 2 only verifies the PAT; work-item fetch lands in Phase 6.
        if let Some(azdo_cfg) = self.config.azure_devops_config() {
            let client = AzureDevOpsClient::new(azdo_cfg.clone());
            match client.test_connection().await {
                Ok(info) => info!(
                    user = info.user_name.as_deref().unwrap_or("?"),
                    org = %info.organization_url,
                    "Azure DevOps connection verified",
                ),
                Err(e) => warn!("Azure DevOps connection failed (non-fatal): {e}"),
            }
        }

        // Optional: Linear issue enrichment.
        if let Some(linear_cfg) = &self.config.linear {
            if linear_cfg.fetch_on_reference {
                match LinearClient::new(linear_cfg) {
                    Ok(client) => {
                        // Collect commit messages from DB.
                        let messages: Vec<String> = {
                            let conn = db.connection();
                            let mut stmt = match conn.prepare("SELECT message FROM commits") {
                                Ok(s) => s,
                                Err(e) => {
                                    stats
                                        .errors
                                        .push(format!("Linear: query commits failed: {e}"));
                                    return Ok(stats);
                                }
                            };
                            let rows = match stmt.query_map([], |row| row.get::<_, String>(0)) {
                                Ok(r) => r,
                                Err(e) => {
                                    stats
                                        .errors
                                        .push(format!("Linear: read commits failed: {e}"));
                                    return Ok(stats);
                                }
                            };
                            let mut out = Vec::new();
                            for r in rows.flatten() {
                                out.push(r);
                            }
                            out
                        };

                        let msg_refs: Vec<&str> = messages.iter().map(String::as_str).collect();
                        let issues = client
                            .fetch_referenced_issues(&msg_refs, &linear_cfg.team_keys)
                            .await;
                        for issue in &issues {
                            info!(
                                id = %issue.identifier,
                                state = %issue.state,
                                team = %issue.team,
                                "Linear issue fetched"
                            );
                        }
                        match client.store_issues(db, &issues) {
                            Ok(n) => {
                                info!(stored = n, "persisted linear_issues rows");
                                stats.linear_issues_fetched += n;
                            }
                            Err(e) => {
                                stats
                                    .errors
                                    .push(format!("Linear: store issues failed: {e}"));
                            }
                        }
                    }
                    Err(e) => {
                        stats.errors.push(format!("Linear client init failed: {e}"));
                    }
                }
            }
        }

        Ok(stats)
    }

    /// Collect a single repository week-by-week, skipping `(repo, ISO-week)`
    /// pairs that already have a row in `collection_runs` unless `force` is
    /// set. All non-fatal errors are pushed into `stats.errors` so that one
    /// bad week (or bad repo) does not abort the entire run.
    fn collect_repo_by_week(
        &self,
        db: &mut Database,
        collector: &GitCollector,
        stats: &mut CollectionStats,
    ) {
        let repo_name = collector.name().to_string();

        // Derive the [from, to] NaiveDate window from the collector's
        // configured since/until. If no bounds are set, fall back to
        // collecting the entire history in one shot.
        let (from, to) = match (collector.since(), collector.until()) {
            (Some(s), Some(u)) => (s.date_naive(), u.date_naive()),
            _ => {
                // No date bounds → bypass week-level bookkeeping. This
                // preserves prior behaviour for repos configured without
                // since/until dates.
                match collector.collect(db) {
                    Ok(n) => {
                        info!(repo = %repo_name, commits = n, "extracted (unbounded)");
                        stats.commits_collected += n;
                    }
                    Err(e) => {
                        let msg = format!("collection failed for {repo_name}: {e}");
                        warn!("{msg}");
                        stats.errors.push(msg);
                    }
                }
                return;
            }
        };

        for week in weeks_in_range(from, to) {
            let (year, week_no, _, _) = week;
            // Skip-if-collected check.
            if !self.force {
                match db::is_week_collected(db, &repo_name, year, week_no) {
                    Ok(true) => {
                        info!("Skipping {repo_name} W{week_no} {year} — already collected");
                        println!(
                            "Skipped   W{week_no:02} {year}: already collected \
                             (use --force to re-collect) [{repo_name}]"
                        );
                        stats.weeks_skipped += 1;
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        let msg = format!(
                            "collection_runs lookup failed for {repo_name} W{week_no} {year}: {e}"
                        );
                        warn!("{msg}");
                        stats.errors.push(msg);
                        continue;
                    }
                }
            }

            // Clamp the week to the user-requested range so we don't pull
            // commits outside [from, to] on partial-week boundaries.
            let (win_start, win_end) = clamp_week_to_range(week, from, to);
            let since_ts = naive_date_start_utc(win_start);
            let until_ts = naive_date_end_utc(win_end);

            match collector.collect_window(db, Some(since_ts), Some(until_ts)) {
                Ok(n) => {
                    info!(
                        repo = %repo_name,
                        year,
                        week = week_no,
                        commits = n,
                        "extracted week"
                    );
                    println!("Collected W{week_no:02} {year}: {n} commits [{repo_name}]");
                    stats.commits_collected += n;
                    stats.weeks_collected += 1;
                    if let Err(e) = db::record_collection_run(db, &repo_name, year, week_no, n) {
                        let msg = format!(
                            "failed to record collection_run for {repo_name} W{week_no} {year}: {e}"
                        );
                        warn!("{msg}");
                        stats.errors.push(msg);
                    }
                }
                Err(e) => {
                    let msg = format!("collection failed for {repo_name} W{week_no} {year}: {e}");
                    warn!("{msg}");
                    stats.errors.push(msg);
                }
            }
        }
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

/// Convert a calendar date to the UTC instant at 00:00:00 on that day.
fn naive_date_start_utc(d: NaiveDate) -> DateTime<Utc> {
    let ndt = d
        .and_hms_opt(0, 0, 0)
        .expect("00:00:00 is always a valid time");
    Utc.from_utc_datetime(&ndt)
}

/// Convert a calendar date to the UTC instant at 23:59:59 on that day.
fn naive_date_end_utc(d: NaiveDate) -> DateTime<Utc> {
    let ndt = d
        .and_hms_opt(23, 59, 59)
        .expect("23:59:59 is always a valid time");
    Utc.from_utc_datetime(&ndt)
}

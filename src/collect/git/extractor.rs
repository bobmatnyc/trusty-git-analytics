//! Commit extraction via `git2`.
//!
//! Walks a repository's revision history, applies date filters, computes
//! diff statistics for each commit, and persists the result into the
//! SQLite store via `core::db::Database`.

use std::path::PathBuf;

use chrono::{DateTime, TimeZone, Utc};
use git2::{Repository, Sort};
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tracing::{debug, info, warn};

use crate::collect::errors::{CollectError, Result};
use crate::collect::git::diff::{compute_commit_diff, CommitDiff};
use crate::collect::git::fetch::fetch_remote;
use crate::collect::ticket::is_ticketed;
use crate::core::config::{expand_path, RepositoryConfig};
use crate::core::db::Database;

/// Extracts commits from a single configured repository.
#[derive(Debug)]
pub struct GitCollector {
    /// Resolved on-disk path of the repository.
    path: PathBuf,
    /// Display name used in the `repository` column.
    name: String,
    /// Branch override (None = HEAD).
    branch: Option<String>,
    /// Optional inclusive since date (ISO 8601, parsed to UTC).
    since: Option<DateTime<Utc>>,
    /// Optional inclusive until date (ISO 8601, parsed to UTC).
    until: Option<DateTime<Utc>>,
    /// If true, merge commits are not written to the DB.
    skip_merges: bool,
    /// If true, skip the pre-walk `git fetch` step.
    no_fetch: bool,
    /// Remote name to fetch from prior to the walk (default "origin").
    remote_name: String,
}

impl GitCollector {
    /// Construct a new collector from a [`RepositoryConfig`].
    ///
    /// Validates that the path exists and refers to a real git repository.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Git`] if the path is not a git repository.
    /// - [`CollectError::Config`] if date strings cannot be parsed.
    pub fn new(config: &RepositoryConfig) -> Result<Self> {
        let path = expand_path(&config.path);
        if !path.exists() {
            return Err(CollectError::Config(format!(
                "repository path does not exist: {}",
                path.display()
            )));
        }
        // Verify it's actually a repository up-front.
        let _ = Repository::open(&path)?;

        let name = config
            .name
            .clone()
            .or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| path.display().to_string());

        let since = parse_iso_date(config.since_date.as_deref())?;
        let until = parse_iso_date(config.until_date.as_deref())?;

        Ok(Self {
            path,
            name,
            branch: config.branch.clone(),
            since,
            until,
            skip_merges: false,
            no_fetch: false,
            remote_name: "origin".to_string(),
        })
    }

    /// Set whether to skip merge commits during extraction.
    pub fn skip_merges(mut self, skip: bool) -> Self {
        self.skip_merges = skip;
        self
    }

    /// Disable the pre-walk `git fetch` (useful for offline / CI scenarios
    /// or when the caller has already fetched out-of-band).
    pub fn no_fetch(mut self, no_fetch: bool) -> Self {
        self.no_fetch = no_fetch;
        self
    }

    /// Override the remote name used for the pre-walk fetch (default `"origin"`).
    pub fn with_remote(mut self, remote: impl Into<String>) -> Self {
        self.remote_name = remote.into();
        self
    }

    /// Walk the repository and insert commits into the database.
    ///
    /// Returns the number of commits written.
    ///
    /// # Errors
    ///
    /// Any underlying git or database failure is propagated.
    pub fn collect(&self, db: &mut Database) -> Result<usize> {
        self.collect_window(db, self.since, self.until)
    }

    /// Walk the repository and insert commits whose timestamp falls within
    /// `[since, until]`. The supplied bounds override the collector's
    /// configured `since`/`until` for this call only.
    ///
    /// Either bound may be `None` to leave that side open.
    ///
    /// # Errors
    ///
    /// Any underlying git or database failure is propagated.
    pub fn collect_window(
        &self,
        db: &mut Database,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<usize> {
        let repo = Repository::open(&self.path)?;
        info!(
            repo = %self.name,
            path = %self.path.display(),
            ?since,
            ?until,
            "starting commit extraction"
        );

        // Optional pre-walk remote fetch. Soft-fails on auth/transport so a
        // misconfigured remote doesn't break collection on local history.
        if !self.no_fetch {
            if let Err(e) = fetch_remote(&repo, &self.remote_name) {
                warn!(
                    repo = %self.name,
                    remote = %self.remote_name,
                    error = %e,
                    "pre-walk fetch returned an error; continuing with local refs"
                );
            }
        } else {
            debug!(repo = %self.name, "skipping pre-walk fetch (--no-fetch)");
        }

        let mut revwalk = repo.revwalk()?;
        revwalk.set_sorting(Sort::TIME)?;
        match &self.branch {
            Some(name) => {
                let refname = format!("refs/heads/{name}");
                if revwalk.push_ref(&refname).is_err() {
                    // Try as a generic revision (could be a tag or remote ref).
                    revwalk.push_ref(name)?;
                }
            }
            None => revwalk.push_head()?,
        }

        // Spinner-style progress bar — we stream the revwalk so we don't
        // know the total in advance. This is intentional: materialising
        // every OID up-front on a 58K-commit monolith eats memory AND
        // forces a full-history walk before the time filter can take
        // effect. With Sort::TIME the walk yields newest-first, so we can
        // safely break the moment we cross the `since` boundary.
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner} {pos} commits walked {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));

        let mut written = 0usize;
        let mut walked = 0usize;
        let tx = db.connection_mut().transaction()?;
        for oid_res in revwalk {
            let oid = match oid_res {
                Ok(o) => o,
                Err(e) => {
                    warn!(error = %e, "revwalk yielded error; stopping traversal");
                    break;
                }
            };
            walked += 1;
            pb.set_position(walked as u64);
            if walked % 1000 == 0 {
                info!(repo = %self.name, walked, written, "extraction progress");
            }

            let commit = repo.find_commit(oid)?;
            let ts = match commit_time_utc(&commit) {
                Some(t) => t,
                None => {
                    warn!(sha = %oid, "skipping commit with invalid timestamp");
                    continue;
                }
            };

            // Since commits are ordered newest-first by Sort::TIME, once we
            // cross below `since` we can stop walking entirely. This is the
            // primary fix for full-history traversal when --weeks/--from is
            // set: prior code walked every commit then filtered post-hoc.
            if let Some(s) = since {
                if ts < s {
                    debug!(sha = %oid, ts = %ts, since = %s, "reached since bound; stopping revwalk");
                    break;
                }
            }
            if let Some(u) = until {
                if ts > u {
                    // Newer than upper bound — still need to keep walking
                    // because we're going backwards and earlier commits
                    // may still fall in range.
                    continue;
                }
            }

            let is_merge = commit.parent_count() > 1;
            if self.skip_merges && is_merge {
                continue;
            }

            let diff = match compute_commit_diff(&repo, &commit) {
                Ok(d) => d,
                Err(e) => {
                    warn!(sha = %oid, error = %e, "failed to compute diff; recording commit with zero stats");
                    CommitDiff::default()
                }
            };

            let author = commit.author();
            let author_name = author.name().unwrap_or("").to_string();
            let author_email = author.email().unwrap_or("").to_string();
            let message = commit.message().unwrap_or("").to_string();
            let sha_str = oid.to_string();

            let ticketed = is_ticketed(&message);

            let inserted = tx.execute(
                "INSERT OR IGNORE INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  files_changed, insertions, deletions, is_merge, ticketed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    sha_str,
                    author_name,
                    author_email,
                    ts.to_rfc3339(),
                    message,
                    self.name,
                    diff.files_changed as i64,
                    diff.insertions as i64,
                    diff.deletions as i64,
                    is_merge as i64,
                    ticketed as i64,
                ],
            )?;

            if inserted == 1 {
                let commit_id = tx.last_insert_rowid();
                for f in &diff.files {
                    tx.execute(
                        "INSERT INTO files (commit_id, path, change_type, insertions, deletions) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            commit_id,
                            f.path,
                            f.change_type.as_str(),
                            f.insertions as i64,
                            f.deletions as i64,
                        ],
                    )?;
                }
                written += 1;
            }
        }
        tx.commit()?;
        pb.finish_with_message(format!("done ({walked} walked, {written} new)"));
        debug!(repo = %self.name, written, "commit extraction complete");
        Ok(written)
    }

    /// Borrow the resolved repository name (display).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Configured inclusive lower bound on commit timestamps, if any.
    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.since
    }

    /// Configured inclusive upper bound on commit timestamps, if any.
    pub fn until(&self) -> Option<DateTime<Utc>> {
        self.until
    }
}

/// Parse an ISO-8601 date or datetime into a UTC timestamp.
fn parse_iso_date(s: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(s) = s else { return Ok(None) };
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(dt.with_timezone(&Utc)));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| CollectError::Config(format!("invalid date: {s}")))?;
        return Ok(Some(Utc.from_utc_datetime(&ndt)));
    }
    Err(CollectError::Config(format!(
        "could not parse date '{s}' (expected YYYY-MM-DD or RFC3339)"
    )))
}

/// Convert a git commit author time to UTC `DateTime`.
fn commit_time_utc(commit: &git2::Commit<'_>) -> Option<DateTime<Utc>> {
    let t = commit.time();
    Utc.timestamp_opt(t.seconds(), 0).single()
}

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
        })
    }

    /// Set whether to skip merge commits during extraction.
    pub fn skip_merges(mut self, skip: bool) -> Self {
        self.skip_merges = skip;
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
        let repo = Repository::open(&self.path)?;
        info!(repo = %self.name, path = %self.path.display(), "starting commit extraction");

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

        // Collect candidate OIDs first so we have a progress total.
        let oids: Vec<git2::Oid> = revwalk.filter_map(|r| r.ok()).collect();
        let pb = ProgressBar::new(oids.len() as u64);
        pb.set_style(
            ProgressStyle::with_template(
                "{spinner} [{bar:40.cyan/blue}] {pos}/{len} commits {msg}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );

        let mut written = 0usize;
        let tx = db.connection_mut().transaction()?;
        for oid in &oids {
            let commit = repo.find_commit(*oid)?;
            let ts = match commit_time_utc(&commit) {
                Some(t) => t,
                None => {
                    warn!(sha = %oid, "skipping commit with invalid timestamp");
                    pb.inc(1);
                    continue;
                }
            };

            if let Some(s) = self.since {
                if ts < s {
                    pb.inc(1);
                    continue;
                }
            }
            if let Some(u) = self.until {
                if ts > u {
                    pb.inc(1);
                    continue;
                }
            }

            let is_merge = commit.parent_count() > 1;
            if self.skip_merges && is_merge {
                pb.inc(1);
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

            let inserted = tx.execute(
                "INSERT OR IGNORE INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  files_changed, insertions, deletions, is_merge) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            pb.inc(1);
        }
        tx.commit()?;
        pb.finish_with_message(format!("done ({written} new)"));
        debug!(repo = %self.name, written, "commit extraction complete");
        Ok(written)
    }

    /// Borrow the resolved repository name (display).
    pub fn name(&self) -> &str {
        &self.name
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

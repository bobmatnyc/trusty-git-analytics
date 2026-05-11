//! Database aggregation: turn raw rows into [`ReportData`].
//!
//! The aggregator runs a single scan of the `commits` table (left-joined
//! against `classifications`) and groups the results in-memory. For the
//! data sizes typical of `trusty-git-analytics` this is simpler and
//! faster than emitting multiple grouped SQL queries.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Datelike, Utc};
use tracing::debug;

use crate::core::config::Config;
use crate::core::db::Database;
use crate::report::errors::Result;
use crate::report::models::{AuthorSummary, ReportData, RepositorySummary, WeeklyActivity};

/// Helper that walks the database and assembles [`ReportData`].
pub struct Aggregator;

/// Internal row pulled from the commit/classification join.
struct CommitRow {
    author_name: String,
    author_email: String,
    timestamp: DateTime<Utc>,
    repository: String,
    insertions: i64,
    deletions: i64,
    files_changed: i64,
    category: Option<String>,
}

impl Aggregator {
    /// Build a full [`ReportData`] from the given database.
    ///
    /// The optional `_config` argument is currently unused but kept on the
    /// signature so future filtering (date ranges from `RepositoryConfig`,
    /// include/exclude merges, etc.) can be added without breaking callers.
    ///
    /// # Errors
    ///
    /// Returns [`crate::report::ReportError::Core`] if the underlying queries fail.
    pub fn build(db: &Database, _config: &Config) -> Result<ReportData> {
        let rows = Self::load_rows(db)?;
        Ok(Self::aggregate(rows))
    }

    fn load_rows(db: &Database) -> Result<Vec<CommitRow>> {
        let conn = db.connection();
        let mut stmt = conn
            .prepare(
                "SELECT c.author_name, c.author_email, c.timestamp, c.repository, \
                        c.insertions, c.deletions, c.files_changed, cl.category \
                 FROM commits c \
                 LEFT JOIN classifications cl ON cl.id = c.classification_id",
            )
            .map_err(crate::core::TgaError::from)?;

        let rows = stmt
            .query_map([], |row| {
                let ts_str: String = row.get(2)?;
                let timestamp = DateTime::parse_from_rfc3339(&ts_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok(CommitRow {
                    author_name: row.get(0)?,
                    author_email: row.get(1)?,
                    timestamp,
                    repository: row.get(3)?,
                    insertions: row.get(4)?,
                    deletions: row.get(5)?,
                    files_changed: row.get(6)?,
                    category: row.get(7)?,
                })
            })
            .map_err(crate::core::TgaError::from)?;

        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(crate::core::TgaError::from)?);
        }
        debug!(count = out.len(), "loaded commit rows for aggregation");
        Ok(out)
    }

    fn aggregate(rows: Vec<CommitRow>) -> ReportData {
        let generated_at = Utc::now().to_rfc3339();
        let mut data = ReportData::empty(generated_at);

        if rows.is_empty() {
            return data;
        }

        // Period bounds.
        let mut min_ts = rows[0].timestamp;
        let mut max_ts = rows[0].timestamp;

        // Per-author state.
        struct AuthorAcc {
            name: String,
            email: String,
            commits: usize,
            insertions: i64,
            deletions: i64,
            files_changed: i64,
            categories: HashMap<String, usize>,
            first: DateTime<Utc>,
            last: DateTime<Utc>,
        }
        // Keyed by author_email only — the same person committing with the same email
        // but slightly different display names (e.g. "Bob Smith" vs "bobsmith") should
        // be aggregated into a single author row. We retain the longest display name
        // seen as the canonical name for that email.
        let mut authors: HashMap<String, AuthorAcc> = HashMap::new();

        // Per-repo state.
        struct RepoAcc {
            commits: usize,
            authors: HashSet<String>,
            insertions: i64,
            deletions: i64,
            categories: HashMap<String, usize>,
        }
        let mut repos: HashMap<String, RepoAcc> = HashMap::new();

        // Weekly buckets keyed by (week, author, repository).
        struct WeekAcc {
            commits: usize,
            insertions: i64,
            deletions: i64,
            categories: HashMap<String, usize>,
        }
        let mut weekly: BTreeMap<(String, String, String), WeekAcc> = BTreeMap::new();

        let mut category_total: HashMap<String, usize> = HashMap::new();

        for row in &rows {
            if row.timestamp < min_ts {
                min_ts = row.timestamp;
            }
            if row.timestamp > max_ts {
                max_ts = row.timestamp;
            }

            // Authors. Group by email only; pick the longest display name seen
            // as the canonical name (heuristic: longer names tend to be the full
            // "Firstname Lastname" form rather than a short login handle).
            let key = row.author_email.clone();
            let a = authors.entry(key).or_insert_with(|| AuthorAcc {
                name: row.author_name.clone(),
                email: row.author_email.clone(),
                commits: 0,
                insertions: 0,
                deletions: 0,
                files_changed: 0,
                categories: HashMap::new(),
                first: row.timestamp,
                last: row.timestamp,
            });
            if row.author_name.len() > a.name.len() {
                a.name = row.author_name.clone();
            }
            a.commits += 1;
            a.insertions += row.insertions;
            a.deletions += row.deletions;
            a.files_changed += row.files_changed;
            if row.timestamp < a.first {
                a.first = row.timestamp;
            }
            if row.timestamp > a.last {
                a.last = row.timestamp;
            }
            if let Some(cat) = &row.category {
                *a.categories.entry(cat.clone()).or_insert(0) += 1;
            }

            // Repositories.
            let r = repos
                .entry(row.repository.clone())
                .or_insert_with(|| RepoAcc {
                    commits: 0,
                    authors: HashSet::new(),
                    insertions: 0,
                    deletions: 0,
                    categories: HashMap::new(),
                });
            r.commits += 1;
            r.authors.insert(row.author_email.clone());
            r.insertions += row.insertions;
            r.deletions += row.deletions;
            if let Some(cat) = &row.category {
                *r.categories.entry(cat.clone()).or_insert(0) += 1;
            }

            // Weekly. Keyed by email (not display name) so that the same identity
            // committing under multiple names lands in a single weekly bucket.
            let week = iso_week_label(&row.timestamp);
            let wkey = (week, row.author_email.clone(), row.repository.clone());
            let w = weekly.entry(wkey).or_insert_with(|| WeekAcc {
                commits: 0,
                insertions: 0,
                deletions: 0,
                categories: HashMap::new(),
            });
            w.commits += 1;
            w.insertions += row.insertions;
            w.deletions += row.deletions;
            if let Some(cat) = &row.category {
                *w.categories.entry(cat.clone()).or_insert(0) += 1;
            }

            // Category totals.
            if let Some(cat) = &row.category {
                *category_total.entry(cat.clone()).or_insert(0) += 1;
            }
        }

        // Materialize authors.
        let mut author_summaries: Vec<AuthorSummary> = authors
            .into_values()
            .map(|a| AuthorSummary {
                name: a.name,
                email: a.email,
                commit_count: a.commits,
                insertions: a.insertions,
                deletions: a.deletions,
                files_changed: a.files_changed,
                categories: a.categories,
                first_commit: a.first.to_rfc3339(),
                last_commit: a.last.to_rfc3339(),
            })
            .collect();
        author_summaries.sort_by(|x, y| y.commit_count.cmp(&x.commit_count));

        // Materialize repositories.
        let mut repo_summaries: Vec<RepositorySummary> = repos
            .into_iter()
            .map(|(name, r)| {
                let mut top: Vec<(String, usize)> = r.categories.into_iter().collect();
                top.sort_by(|a, b| b.1.cmp(&a.1));
                RepositorySummary {
                    name,
                    commit_count: r.commits,
                    author_count: r.authors.len(),
                    insertions: r.insertions,
                    deletions: r.deletions,
                    top_categories: top,
                }
            })
            .collect();
        repo_summaries.sort_by(|x, y| y.commit_count.cmp(&x.commit_count));

        // Build email → canonical display name map from the author summaries
        // so that the weekly activity rows display the same canonical name as
        // the authors table (avoids "Bob" in one report and "bobmatnyc" in
        // another for the same underlying identity).
        let email_to_name: HashMap<String, String> = author_summaries
            .iter()
            .map(|a| (a.email.clone(), a.name.clone()))
            .collect();

        // Materialize weekly activity. The bucket key uses email; resolve to
        // canonical display name for the output row.
        let weekly_activity: Vec<WeeklyActivity> = weekly
            .into_iter()
            .map(|((week, email, repository), w)| WeeklyActivity {
                week,
                author: email_to_name.get(&email).cloned().unwrap_or(email),
                repository,
                commit_count: w.commits,
                insertions: w.insertions,
                deletions: w.deletions,
                categories: w.categories,
            })
            .collect();

        data.total_commits = rows.len();
        data.total_authors = author_summaries.len();
        data.period_start = Some(min_ts.to_rfc3339());
        data.period_end = Some(max_ts.to_rfc3339());
        data.authors = author_summaries;
        data.repositories = repo_summaries;
        data.weekly_activity = weekly_activity;
        data.category_breakdown = category_total;
        data
    }
}

/// Format an ISO week label such as `"2024-W03"` from a UTC timestamp.
fn iso_week_label(ts: &DateTime<Utc>) -> String {
    let iso = ts.iso_week();
    format!("{}-W{:02}", iso.year(), iso.week())
}

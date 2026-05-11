//! Report-specific aggregated data structures.
//!
//! These structs are populated by [`crate::aggregator::Aggregator`] from
//! database queries and then consumed by the formatters in
//! [`crate::formatters`]. They are `serde`-friendly so that the JSON
//! formatter can emit [`ReportData`] directly.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Aggregated per-author commit summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthorSummary {
    /// Canonical author display name.
    pub name: String,
    /// Canonical author email.
    pub email: String,
    /// Number of commits attributed to this author.
    pub commit_count: usize,
    /// Total insertions across all of this author's commits.
    pub insertions: i64,
    /// Total deletions across all of this author's commits.
    pub deletions: i64,
    /// Total files changed across all of this author's commits.
    pub files_changed: i64,
    /// Per-category commit counts for this author.
    pub categories: HashMap<String, usize>,
    /// ISO 8601 timestamp of the author's earliest observed commit.
    pub first_commit: String,
    /// ISO 8601 timestamp of the author's latest observed commit.
    pub last_commit: String,
}

/// Aggregated per-repository summary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepositorySummary {
    /// Repository name (matches `commits.repository`).
    pub name: String,
    /// Total commits in this repository.
    pub commit_count: usize,
    /// Distinct author count in this repository.
    pub author_count: usize,
    /// Total insertions across all commits in this repository.
    pub insertions: i64,
    /// Total deletions across all commits in this repository.
    pub deletions: i64,
    /// Top categories by commit count, sorted descending.
    pub top_categories: Vec<(String, usize)>,
}

/// Per-week-per-author-per-repository activity row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeeklyActivity {
    /// ISO week label, e.g. `"2024-W03"`.
    pub week: String,
    /// Author name.
    pub author: String,
    /// Repository name.
    pub repository: String,
    /// Number of commits in this week/author/repo bucket.
    pub commit_count: usize,
    /// Insertions in this bucket.
    pub insertions: i64,
    /// Deletions in this bucket.
    pub deletions: i64,
    /// Per-category counts within this bucket.
    pub categories: HashMap<String, usize>,
}

/// Full report payload passed to every formatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportData {
    /// ISO 8601 timestamp at which the report was generated.
    pub generated_at: String,
    /// Earliest observed commit timestamp (ISO 8601), or `None` if empty.
    pub period_start: Option<String>,
    /// Latest observed commit timestamp (ISO 8601), or `None` if empty.
    pub period_end: Option<String>,
    /// Per-author summaries.
    pub authors: Vec<AuthorSummary>,
    /// Per-repository summaries.
    pub repositories: Vec<RepositorySummary>,
    /// Weekly activity rows.
    pub weekly_activity: Vec<WeeklyActivity>,
    /// Total commit count across the dataset.
    pub total_commits: usize,
    /// Total distinct author count across the dataset.
    pub total_authors: usize,
    /// Cross-cutting category → count tally.
    pub category_breakdown: HashMap<String, usize>,
}

impl ReportData {
    /// Construct an empty `ReportData` with the given generation timestamp.
    pub fn empty(generated_at: String) -> Self {
        Self {
            generated_at,
            period_start: None,
            period_end: None,
            authors: Vec::new(),
            repositories: Vec::new(),
            weekly_activity: Vec::new(),
            total_commits: 0,
            total_authors: 0,
            category_breakdown: HashMap::new(),
        }
    }
}

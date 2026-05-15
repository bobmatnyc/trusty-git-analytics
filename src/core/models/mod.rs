//! Domain models corresponding to the v1 database schema.
//!
//! These structs are the in-memory representation of rows in the core
//! tables. They are intentionally serialization-friendly via `serde` so
//! that they can be emitted as JSON in reports without an intermediate
//! DTO layer.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single commit observed in a repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commit {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Full git OID (hex).
    pub sha: String,

    /// Foreign key into [`Author`].
    pub author_id: Option<i64>,

    /// Author display name as recorded in the commit.
    pub author_name: String,

    /// Author email as recorded in the commit.
    pub author_email: String,

    /// Author timestamp (UTC).
    pub timestamp: DateTime<Utc>,

    /// Commit message body (raw).
    pub message: String,

    /// Repository identifier (path or canonical name).
    pub repository: String,

    /// Number of files changed.
    pub files_changed: u32,

    /// Lines added.
    pub insertions: u32,

    /// Lines deleted.
    pub deletions: u32,

    /// Foreign key into [`Classification`], if classified.
    pub classification_id: Option<i64>,

    /// Confidence assigned by the classifier (0.0–1.0).
    pub confidence: Option<f64>,

    /// True for merge commits (parents > 1).
    pub is_merge: bool,

    /// True if the commit message references a known ticket system
    /// (JIRA/Linear-style `PROJ-123`, GitHub `fixes #123`, or bare `#123`).
    ///
    /// Computed at extraction time by [`crate::collect::ticket::is_ticketed`]
    /// and persisted on the `commits` row.
    pub ticketed: bool,
}

/// A canonical author / developer identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Author {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Canonical display name.
    pub canonical_name: String,

    /// Canonical email address.
    pub canonical_email: String,

    /// JSON-encoded array of alias strings (names or emails).
    pub aliases: String,
}

/// A classification verdict produced by the cascade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Classification {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Top-level category (e.g. `feature`, `bugfix`, `chore`).
    pub category: String,

    /// Optional finer-grained label.
    pub subcategory: Option<String>,

    /// Associated ticket identifier (e.g. `API-123`), if any.
    pub ticket_id: Option<String>,

    /// Confidence in this verdict (0.0–1.0).
    pub confidence: f64,

    /// Which tier of the cascade produced this verdict.
    pub method: ClassificationMethod,
}

/// File-level change record attached to a commit.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChange {
    /// Primary key (database-assigned).
    pub id: i64,

    /// Foreign key into [`Commit`].
    pub commit_id: i64,

    /// Relative path within the repository.
    pub path: String,

    /// Type of change.
    pub change_type: ChangeType,

    /// Lines added in this file.
    pub insertions: u32,

    /// Lines deleted in this file.
    pub deletions: u32,
}

/// A pull request record (typically GitHub).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullRequest {
    /// Primary key (database-assigned).
    pub id: i64,

    /// PR number within its repository.
    pub pr_number: u64,

    /// Repository this PR belongs to. Together with `provider` and
    /// `pr_number` this forms the persistence-level unique identity of a
    /// PR. GitHub assigns `pr_number` per-repository (so #1 in repo A is
    /// not the same PR as #1 in repo B); without this field the
    /// `(provider, pr_number)` unique index from migration v10 silently
    /// dropped cross-repo collisions during multi-repo collection (#88).
    ///
    /// Format is provider-specific:
    /// - GitHub: `"owner/repo"` (e.g. `"acme/widgets"`)
    /// - Bitbucket: `"workspace/repo_slug"`
    /// - Azure DevOps: `"project"` (PRs are project-scoped)
    pub repository: String,

    /// PR title.
    pub title: String,

    /// Author login.
    pub author: String,

    /// Lifecycle state.
    pub state: PrState,

    /// PR creation timestamp (UTC).
    pub created_at: DateTime<Utc>,

    /// Merge timestamp, if merged.
    pub merged_at: Option<DateTime<Utc>>,

    /// JSON-encoded array of commit SHAs in the PR.
    pub commit_shas: String,
}

/// Cascade tier that produced a classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClassificationMethod {
    /// Matched a deterministic exact rule.
    ExactRule,
    /// Matched a regex rule.
    RegexRule,
    /// Matched via fuzzy similarity.
    FuzzyMatch,
    /// Assigned by an LLM fallback.
    LlmFallback,
    /// Set manually by a user override.
    Manual,
}

impl ClassificationMethod {
    /// Stable string representation used for DB storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            ClassificationMethod::ExactRule => "exact_rule",
            ClassificationMethod::RegexRule => "regex_rule",
            ClassificationMethod::FuzzyMatch => "fuzzy_match",
            ClassificationMethod::LlmFallback => "llm_fallback",
            ClassificationMethod::Manual => "manual",
        }
    }
}

/// File change kind for [`FileChange`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeType {
    /// File was added.
    Added,
    /// File contents were modified.
    Modified,
    /// File was deleted.
    Deleted,
    /// File was renamed (and possibly modified).
    Renamed,
}

impl ChangeType {
    /// Stable string representation used for DB storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            ChangeType::Added => "added",
            ChangeType::Modified => "modified",
            ChangeType::Deleted => "deleted",
            ChangeType::Renamed => "renamed",
        }
    }
}

/// Lifecycle state of a [`PullRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrState {
    /// PR is open.
    Open,
    /// PR was closed without merging.
    Closed,
    /// PR was merged.
    Merged,
}

impl PrState {
    /// Stable string representation used for DB storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
    }
}

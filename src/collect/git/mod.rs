//! Git extraction module.
//!
//! Wraps `git2` to walk repositories, compute per-commit diff statistics,
//! and persist commit + file rows into the SQLite store.

pub mod diff;
pub mod extractor;
pub mod fetch;

pub use diff::{compute_commit_diff, CommitDiff, FileDiff};
pub use extractor::GitCollector;
pub use fetch::fetch_remote;

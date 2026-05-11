//! Ticketed / unticketed commit statistics.
//!
//! Provides a small aggregate over a slice of [`Commit`] records that
//! summarizes how many commits reference a ticket. Used by reporters to
//! surface "process hygiene" metrics alongside the existing category
//! breakdown.

use serde::{Deserialize, Serialize};

use crate::core::models::Commit;

/// Aggregate ticketed / unticketed commit counts and percentages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketedStats {
    /// Total number of commits considered.
    pub total: usize,
    /// Number of commits whose message references a ticket.
    pub ticketed: usize,
    /// Number of commits whose message does not reference a ticket.
    pub unticketed: usize,
    /// `ticketed / total * 100`. Zero when `total == 0`.
    pub ticketed_pct: f64,
    /// `unticketed / total * 100`. Zero when `total == 0`.
    pub unticketed_pct: f64,
}

impl TicketedStats {
    /// Construct an empty (zeroed) `TicketedStats`.
    pub fn empty() -> Self {
        Self {
            total: 0,
            ticketed: 0,
            unticketed: 0,
            ticketed_pct: 0.0,
            unticketed_pct: 0.0,
        }
    }
}

/// Compute [`TicketedStats`] from a slice of [`Commit`] records.
///
/// Percentages are computed against the actual slice length, so partial
/// datasets (e.g. one repository at a time) yield sensible numbers. An
/// empty slice yields the [`TicketedStats::empty`] value.
pub fn compute_ticketed_stats(commits: &[Commit]) -> TicketedStats {
    let total = commits.len();
    if total == 0 {
        return TicketedStats::empty();
    }
    let ticketed = commits.iter().filter(|c| c.ticketed).count();
    let unticketed = total - ticketed;
    let total_f = total as f64;
    TicketedStats {
        total,
        ticketed,
        unticketed,
        ticketed_pct: (ticketed as f64) * 100.0 / total_f,
        unticketed_pct: (unticketed as f64) * 100.0 / total_f,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn make_commit(sha: &str, ticketed: bool) -> Commit {
        Commit {
            id: 0,
            sha: sha.into(),
            author_id: None,
            author_name: "Alice".into(),
            author_email: "alice@example.com".into(),
            timestamp: Utc::now(),
            message: if ticketed {
                "ENG-1 x".into()
            } else {
                "x".into()
            },
            repository: "repo".into(),
            files_changed: 0,
            insertions: 0,
            deletions: 0,
            classification_id: None,
            confidence: None,
            is_merge: false,
            ticketed,
        }
    }

    #[test]
    fn empty_slice_produces_zeroed_stats() {
        let stats = compute_ticketed_stats(&[]);
        assert_eq!(stats, TicketedStats::empty());
    }

    #[test]
    fn mixed_slice_produces_correct_counts_and_percentages() {
        let commits = vec![
            make_commit("a", true),
            make_commit("b", true),
            make_commit("c", true),
            make_commit("d", false),
        ];
        let stats = compute_ticketed_stats(&commits);
        assert_eq!(stats.total, 4);
        assert_eq!(stats.ticketed, 3);
        assert_eq!(stats.unticketed, 1);
        assert!((stats.ticketed_pct - 75.0).abs() < 1e-9);
        assert!((stats.unticketed_pct - 25.0).abs() < 1e-9);
    }

    #[test]
    fn all_ticketed_is_one_hundred_percent() {
        let commits = vec![make_commit("a", true), make_commit("b", true)];
        let stats = compute_ticketed_stats(&commits);
        assert_eq!(stats.ticketed, 2);
        assert_eq!(stats.unticketed, 0);
        assert!((stats.ticketed_pct - 100.0).abs() < 1e-9);
        assert!((stats.unticketed_pct - 0.0).abs() < 1e-9);
    }

    #[test]
    fn all_unticketed_is_zero_percent() {
        let commits = vec![make_commit("a", false), make_commit("b", false)];
        let stats = compute_ticketed_stats(&commits);
        assert_eq!(stats.ticketed, 0);
        assert_eq!(stats.unticketed, 2);
        assert!((stats.ticketed_pct - 0.0).abs() < 1e-9);
        assert!((stats.unticketed_pct - 100.0).abs() < 1e-9);
    }
}

//! Tier 3: heuristic / fuzzy classification.
//!
//! Catches commits the rule tiers missed using cheap structural signals:
//! - explicit merge commits (`is_merge` flag from git)
//! - "Merge pull request" / "Merge branch" style messages
//! - "Revert" prefix
//! - bare ticket-prefixed messages (e.g. `PROJ-123: update auth`)
//! - very short / very long messages (low-confidence default)

use crate::classify::taxonomy::TopLevelCategory;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// Tier-3 heuristic classifier. Stateless.
pub struct FuzzyClassifier;

impl FuzzyClassifier {
    /// Classify `message` using heuristics.
    ///
    /// `is_merge` is the upstream git-level merge flag (parents > 1) and
    /// takes precedence over text-based detection.
    ///
    /// Returns `None` when no heuristic matched — the caller will then
    /// either invoke the LLM tier or fall back to "uncategorized".
    pub fn classify(&self, message: &str, is_merge: bool) -> Option<ClassificationResult> {
        let trimmed = message.trim();
        let lower = trimmed.to_lowercase();

        // 1. Merge commits — strongest structural signal.
        if is_merge
            || lower.starts_with("merge pull request")
            || lower.starts_with("merge branch")
            || lower.starts_with("merge remote-tracking")
        {
            return Some(ClassificationResult {
                category: "merge".to_string(),
                subcategory: None,
                top_level: Some(TopLevelCategory::Maintenance),
                confidence: 0.95,
                method: ClassificationMethod::FuzzyMatch,
                ticket_id: None,
                complexity: None,
            });
        }

        // 2. Revert commits.
        if lower.starts_with("revert ") || lower.starts_with("revert:") {
            return Some(ClassificationResult {
                category: "revert".to_string(),
                subcategory: None,
                top_level: Some(TopLevelCategory::Maintenance),
                confidence: 0.9,
                method: ClassificationMethod::FuzzyMatch,
                ticket_id: None,
                complexity: None,
            });
        }

        // 3. Bare ticket-prefixed message — likely a feature/chore.
        if let Some(ticket) = bare_ticket_prefix(trimmed) {
            return Some(ClassificationResult {
                category: "feature".to_string(),
                subcategory: Some("ticketed".to_string()),
                top_level: Some(TopLevelCategory::Feature),
                confidence: 0.6,
                method: ClassificationMethod::FuzzyMatch,
                ticket_id: Some(ticket),
                complexity: None,
            });
        }

        // 4. Very short messages — chore by convention.
        if trimmed.len() < 12 && !trimmed.is_empty() {
            return Some(ClassificationResult {
                category: "chore".to_string(),
                subcategory: None,
                top_level: Some(TopLevelCategory::Maintenance),
                confidence: 0.4,
                method: ClassificationMethod::FuzzyMatch,
                ticket_id: None,
                complexity: None,
            });
        }

        None
    }
}

/// If `message` starts with a JIRA-style ticket identifier, return it.
fn bare_ticket_prefix(message: &str) -> Option<String> {
    let first = message.split_whitespace().next()?;
    // Strip a trailing colon or hyphen separator.
    let candidate = first.trim_end_matches([':', '-', ',']);
    let mut parts = candidate.split('-');
    let project = parts.next()?;
    let number = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    if project.is_empty() || number.is_empty() {
        return None;
    }
    if !project
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
    {
        return None;
    }
    if !project.chars().next()?.is_ascii_uppercase() {
        return None;
    }
    if !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{project}-{number}"))
}

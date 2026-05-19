//! Tier 3: classify by JIRA project key mapping.
//!
//! Why: Some organizations dedicate JIRA projects to a particular kind of
//! work (e.g. `INFRA` for platform work, `DATA` for data-pipeline features).
//! When a commit references a ticket from such a project, the project key
//! itself is a strong classification signal that no amount of message
//! parsing can reliably reproduce.
//!
//! What: Extracts the JIRA project key from a commit message (the prefix
//! of the first `PROJ-123`-style identifier) and looks it up in a
//! user-configured `HashMap<String, String>`. On hit, returns a verdict
//! with confidence 0.95.
//!
//! Test: Configure mappings `{"INFRA": "platform"}`, classify
//! `"INFRA-42 fix nginx"`, and assert the verdict is `"platform"` with
//! confidence 0.95; assert that an unmapped key (`"FOO-1"`) yields `None`.

use std::collections::HashMap;

use std::sync::OnceLock;

use regex::Regex;

use crate::classify::taxonomy::{TaxonomyRegistry, TopLevelCategory};
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// JIRA-style key pattern: uppercase project prefix + numeric suffix.
///
/// Compiled once on first access via [`OnceLock`]. If the (fixed) pattern
/// fails to compile, lookups simply return `None` rather than panicking.
fn jira_key_re() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Z0-9]+)-\d+\b").ok())
        .as_ref()
}

/// Confidence assigned to every verdict from this tier.
const PROJECT_MAPPING_CONFIDENCE: f64 = 0.95;

/// Tier-3 JIRA project-key classifier.
pub struct JiraProjectTier {
    mappings: HashMap<String, String>,
    taxonomy: TaxonomyRegistry,
}

impl JiraProjectTier {
    /// Construct a new tier with the given `project_key → work_type` map.
    ///
    /// Keys are normalized to uppercase on insert so callers don't have to
    /// pre-uppercase their config.
    pub fn new(mappings: HashMap<String, String>) -> Self {
        Self::with_taxonomy(mappings, TaxonomyRegistry::with_builtins())
    }

    /// Construct with a custom taxonomy registry (lets user-defined
    /// subcategories resolve to a top-level parent).
    pub fn with_taxonomy(mappings: HashMap<String, String>, taxonomy: TaxonomyRegistry) -> Self {
        let normalized = mappings
            .into_iter()
            .map(|(k, v)| (k.to_uppercase(), v))
            .collect();
        Self {
            mappings: normalized,
            taxonomy,
        }
    }

    /// Borrow the underlying mappings (primarily for tests / diagnostics).
    pub fn mappings(&self) -> &HashMap<String, String> {
        &self.mappings
    }

    /// Returns `true` when no mappings are configured (cheap check used by
    /// the engine to skip this tier entirely).
    pub fn is_empty(&self) -> bool {
        self.mappings.is_empty()
    }

    /// Extract the first JIRA project key from `commit_message` and look it
    /// up in the configured mappings.
    ///
    /// Returns a verdict with confidence 0.95 on hit, or `None` if no
    /// ticket reference is present or its project key isn't mapped.
    pub fn classify(&self, commit_message: &str) -> Option<ClassificationResult> {
        if self.mappings.is_empty() {
            return None;
        }
        let re = jira_key_re()?;
        let caps = re.captures(commit_message)?;
        let project = caps.get(1)?.as_str().to_uppercase();
        let category = self.mappings.get(&project)?.clone();

        let ticket_id = caps.get(0).map(|m| m.as_str().to_string());
        let top_level = self
            .taxonomy
            .resolve(&category)
            .unwrap_or(TopLevelCategory::Unknown);

        Some(ClassificationResult {
            category,
            subcategory: None,
            top_level: Some(top_level),
            confidence: PROJECT_MAPPING_CONFIDENCE,
            method: ClassificationMethod::RegexRule,
            ticket_id,
            complexity: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> JiraProjectTier {
        let mut m = HashMap::new();
        m.insert("INFRA".to_string(), "platform".to_string());
        m.insert("DATA".to_string(), "feature".to_string());
        JiraProjectTier::new(m)
    }

    #[test]
    fn hit_returns_mapped_category() {
        let t = fixture();
        let r = t.classify("INFRA-42 fix nginx config").expect("hit");
        assert_eq!(r.category, "platform");
        assert!((r.confidence - 0.95).abs() < 1e-9);
        assert_eq!(r.ticket_id.as_deref(), Some("INFRA-42"));
    }

    #[test]
    fn unmapped_project_returns_none() {
        let t = fixture();
        assert!(t.classify("FOO-1 some change").is_none());
    }

    #[test]
    fn no_ticket_returns_none() {
        let t = fixture();
        assert!(t.classify("fix: something without a ticket").is_none());
    }

    #[test]
    fn empty_mappings_short_circuits() {
        let t = JiraProjectTier::new(HashMap::new());
        assert!(t.is_empty());
        assert!(t.classify("INFRA-1 anything").is_none());
    }

    #[test]
    fn mappings_normalized_to_uppercase() {
        let mut m = HashMap::new();
        m.insert("infra".to_string(), "platform".to_string());
        let t = JiraProjectTier::new(m);
        let r = t.classify("INFRA-7 patch").expect("hit");
        assert_eq!(r.category, "platform");
    }
}

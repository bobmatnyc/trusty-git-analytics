//! Implementations of the four classification tiers.
//!
//! The cascade runs them in order:
//! 1. [`exact`] — fast multi-keyword matching via Aho-Corasick.
//! 2. [`regex_tier`] — regex pattern matching.
//! 3. [`fuzzy`] — heuristics (merge/revert detection, etc.).
//! 4. [`llm`] — optional async LLM fallback.

pub mod bedrock;
pub mod exact;
pub mod fuzzy;
pub mod issue_type_tier;
pub mod jira_project_tier;
pub mod llm;
pub mod override_tier;
pub mod regex_tier;

use serde::{Deserialize, Serialize};

use crate::classify::taxonomy::TopLevelCategory;
use crate::core::models::ClassificationMethod;

/// Output of any tier: a category verdict plus provenance.
///
/// The hierarchy is:
/// - `top_level` — one of the canonical [`TopLevelCategory`] variants
///   (resolved from `category` via the [`crate::classify::taxonomy::TaxonomyRegistry`]).
/// - `category` — the **subcategory name** (e.g. `"feature"`, `"security"`).
///   Kept as a free-form string for backward compatibility with the DB schema.
/// - `subcategory` — an even-more-specific leaf label (e.g. `"sql-injection"`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResult {
    /// Subcategory name (e.g. `"feature"`, `"bugfix"`, `"security"`).
    ///
    /// Despite the field name, this is the **subcategory** in the two-level
    /// taxonomy — the registered `TopLevelCategory` parent is reported in
    /// `top_level`. The field name is preserved for DB-schema compatibility.
    pub category: String,
    /// Optional leaf label (e.g. `"sql-injection"`, `"cleanup"`).
    pub subcategory: Option<String>,
    /// Resolved top-level category (`None` if `category` is unregistered).
    #[serde(default)]
    pub top_level: Option<TopLevelCategory>,
    /// Confidence in this verdict (0.0–1.0).
    pub confidence: f64,
    /// Which tier produced this verdict.
    pub method: ClassificationMethod,
    /// Optional extracted ticket id (e.g. `"PROJ-123"`).
    pub ticket_id: Option<String>,
}

impl ClassificationResult {
    /// Construct an "unclassified" result used as a default when no tier matches.
    pub fn unclassified() -> Self {
        Self {
            category: "uncategorized".to_string(),
            subcategory: None,
            top_level: Some(TopLevelCategory::Unknown),
            confidence: 0.0,
            method: ClassificationMethod::FuzzyMatch,
            ticket_id: None,
        }
    }
}

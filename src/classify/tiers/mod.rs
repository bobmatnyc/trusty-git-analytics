//! Implementations of the four classification tiers.
//!
//! The cascade runs them in order:
//! 1. [`exact`] — fast multi-keyword matching via Aho-Corasick.
//! 2. [`regex_tier`] — regex pattern matching.
//! 3. [`fuzzy`] — heuristics (merge/revert detection, etc.).
//! 4. [`llm`] — optional async LLM fallback.

pub mod exact;
pub mod fuzzy;
pub mod llm;
pub mod regex_tier;

use serde::{Deserialize, Serialize};

use crate::core::models::ClassificationMethod;

/// Output of any tier: a category verdict plus provenance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClassificationResult {
    /// Top-level category (e.g. `"feature"`).
    pub category: String,
    /// Optional subcategory.
    pub subcategory: Option<String>,
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
            confidence: 0.0,
            method: ClassificationMethod::FuzzyMatch,
            ticket_id: None,
        }
    }
}

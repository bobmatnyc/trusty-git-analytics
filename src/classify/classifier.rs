//! Cascade orchestrator combining the four classification tiers.

use rayon::prelude::*;

use crate::classify::errors::Result;
use crate::classify::rules::RuleSet;
use crate::classify::taxonomy::{SubcategoryDef, TaxonomyRegistry};
use crate::classify::tiers::exact::ExactMatcher;
use crate::classify::tiers::fuzzy::FuzzyClassifier;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::regex_tier::RegexMatcher;
use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// Runtime configuration for the [`ClassificationEngine`].
#[derive(Debug, Clone)]
pub struct ClassificationEngineConfig {
    /// Whether to engage the LLM tier when tiers 1–3 fail.
    pub use_llm: bool,
    /// LLM model identifier (provider-specific).
    pub llm_model: String,
    /// Minimum confidence required to accept a verdict.
    ///
    /// Verdicts below this threshold are returned as-is (so the caller
    /// can still inspect them), but their `confidence` informs filtering
    /// in downstream reports.
    pub confidence_threshold: f64,
}

impl Default for ClassificationEngineConfig {
    fn default() -> Self {
        Self {
            use_llm: false,
            llm_model: "gpt-4o-mini".to_string(),
            confidence_threshold: 0.7,
        }
    }
}

/// Combined four-tier cascade.
pub struct ClassificationEngine {
    exact: ExactMatcher,
    regex: RegexMatcher,
    fuzzy: FuzzyClassifier,
    llm: Option<LlmClassifier>,
    taxonomy: TaxonomyRegistry,
    config: ClassificationEngineConfig,
}

impl ClassificationEngine {
    /// Build a new engine from a [`RuleSet`] and configuration.
    ///
    /// The LLM tier is constructed (but only invoked) if `config.use_llm`
    /// is true. The API key is read from the `OPENAI_API_KEY` environment
    /// variable; if unset, the LLM tier silently returns `None`.
    ///
    /// Uses the built-in taxonomy registry only. To extend it with
    /// user-defined subcategories, use [`Self::with_taxonomy`].
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile (e.g. invalid regex).
    pub fn new(ruleset: RuleSet, config: ClassificationEngineConfig) -> Result<Self> {
        Self::with_taxonomy(ruleset, config, Vec::new())
    }

    /// Build an engine with user-defined subcategory definitions merged into
    /// the built-in taxonomy registry.
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile.
    pub fn with_taxonomy(
        ruleset: RuleSet,
        config: ClassificationEngineConfig,
        custom_taxonomy: Vec<SubcategoryDef>,
    ) -> Result<Self> {
        let exact = ExactMatcher::new(&ruleset.rules)?;
        let regex = RegexMatcher::new(&ruleset.rules)?;
        let fuzzy = FuzzyClassifier;
        let llm = if config.use_llm {
            let api_key = std::env::var("OPENAI_API_KEY").ok();
            Some(LlmClassifier::new(&config.llm_model, api_key))
        } else {
            None
        };
        let taxonomy = TaxonomyRegistry::new(custom_taxonomy);
        Ok(Self {
            exact,
            regex,
            fuzzy,
            llm,
            taxonomy,
            config,
        })
    }

    /// Borrow the engine's taxonomy registry.
    pub fn taxonomy(&self) -> &TaxonomyRegistry {
        &self.taxonomy
    }

    /// Borrow the engine's effective configuration.
    pub fn config(&self) -> &ClassificationEngineConfig {
        &self.config
    }

    /// Run tiers 1–3 (synchronous) for a single message.
    ///
    /// Returns `None` if no tier matched; callers may then invoke the
    /// async [`ClassificationEngine::classify`] for the LLM fallback.
    pub fn classify_sync(&self, message: &str, is_merge: bool) -> Option<ClassificationResult> {
        // Tier 1: exact keywords
        if let Some(rule) = self.exact.classify(message) {
            return Some(ClassificationResult {
                top_level: self.taxonomy.resolve(&rule.category),
                category: rule.category.clone(),
                subcategory: rule.subcategory.clone(),
                confidence: rule.confidence,
                method: ClassificationMethod::ExactRule,
                ticket_id: RegexMatcher::extract_ticket_id(message),
            });
        }

        // Tier 2: regex
        if let Some(rule) = self.regex.classify(message) {
            return Some(ClassificationResult {
                top_level: self.taxonomy.resolve(&rule.category),
                category: rule.category.clone(),
                subcategory: rule.subcategory.clone(),
                confidence: rule.confidence,
                method: ClassificationMethod::RegexRule,
                ticket_id: RegexMatcher::extract_ticket_id(message),
            });
        }

        // Tier 3: fuzzy heuristics
        if let Some(mut result) = self.fuzzy.classify(message, is_merge) {
            if result.ticket_id.is_none() {
                result.ticket_id = RegexMatcher::extract_ticket_id(message);
            }
            // Re-resolve top_level via the engine's registry in case user
            // overrides changed the parent for the fuzzy verdict's category.
            if let Some(top) = self.taxonomy.resolve(&result.category) {
                result.top_level = Some(top);
            }
            return Some(result);
        }

        None
    }

    /// Run the full four-tier cascade including the optional LLM fallback.
    pub async fn classify(&self, message: &str, is_merge: bool) -> ClassificationResult {
        if let Some(r) = self.classify_sync(message, is_merge) {
            return r;
        }

        if let Some(llm) = &self.llm {
            if let Some(mut r) = llm.classify(message).await {
                r.top_level = self.taxonomy.resolve(&r.category);
                return r;
            }
        }

        let mut fallback = ClassificationResult::unclassified();
        fallback.ticket_id = RegexMatcher::extract_ticket_id(message);
        fallback
    }

    /// Classify a batch of `(message, is_merge)` pairs in parallel using
    /// Rayon (tiers 1–3 only). Entries where no tier matched are returned
    /// as [`ClassificationResult::unclassified`].
    pub fn classify_batch(&self, messages: &[(&str, bool)]) -> Vec<ClassificationResult> {
        messages
            .par_iter()
            .map(|(msg, is_merge)| {
                self.classify_sync(msg, *is_merge)
                    .unwrap_or_else(ClassificationResult::unclassified)
            })
            .collect()
    }
}

//! Cascade orchestrator combining the classification tiers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use rayon::prelude::*;
use rusqlite::Connection;

use crate::classify::errors::Result;
use crate::classify::rules::RuleSet;
use crate::classify::taxonomy::{SubcategoryDef, TaxonomyRegistry};
use crate::classify::tiers::exact::ExactMatcher;
use crate::classify::tiers::fuzzy::FuzzyClassifier;
use crate::classify::tiers::issue_type_tier::IssueTypeTier;
use crate::classify::tiers::jira_project_tier::JiraProjectTier;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::override_tier::OverrideTier;
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
    /// LLM provider: `"openrouter"`, `"openai"`, or `"auto"`.
    pub llm_provider: String,
    /// Optional OpenRouter API key. If `None`, the env var
    /// `OPENROUTER_API_KEY` is consulted at engine-build time.
    pub openrouter_api_key: Option<String>,
    /// Minimum confidence required to accept a verdict.
    ///
    /// Verdicts below this threshold are returned as-is (so the caller
    /// can still inspect them), but their `confidence` informs filtering
    /// in downstream reports.
    pub confidence_threshold: f64,
    /// Confidence at or below which a sync-tier verdict is escalated to
    /// the LLM tier (when `use_llm` is true).
    ///
    /// Default `0.0` preserves the historical behavior of only firing the
    /// LLM on commits with no rule match. The LLM verdict only replaces
    /// the existing verdict when its confidence is strictly higher, so a
    /// failed LLM call preserves the rule verdict.
    pub llm_fallback_threshold: f64,
}

impl Default for ClassificationEngineConfig {
    fn default() -> Self {
        Self {
            use_llm: false,
            llm_model: "gpt-4o-mini".to_string(),
            llm_provider: "auto".to_string(),
            openrouter_api_key: None,
            confidence_threshold: 0.7,
            llm_fallback_threshold: 0.0,
        }
    }
}

/// Combined classification cascade.
pub struct ClassificationEngine {
    override_tier: Option<OverrideTier>,
    exact: ExactMatcher,
    issue_type: IssueTypeTier,
    regex: RegexMatcher,
    jira_project: JiraProjectTier,
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
        Self::with_taxonomy_and_mappings(ruleset, config, custom_taxonomy, HashMap::new(), None)
    }

    /// Full builder allowing JIRA project-key mappings and an optional DB
    /// connection for the manual-override tier.
    ///
    /// # Errors
    ///
    /// Returns an error if the rules fail to compile.
    pub fn with_taxonomy_and_mappings(
        ruleset: RuleSet,
        config: ClassificationEngineConfig,
        custom_taxonomy: Vec<SubcategoryDef>,
        jira_project_mappings: HashMap<String, String>,
        override_conn: Option<Arc<Mutex<Connection>>>,
    ) -> Result<Self> {
        let exact = ExactMatcher::new(&ruleset.rules)?;
        let regex = RegexMatcher::new(&ruleset.rules)?;
        let fuzzy = FuzzyClassifier;
        let llm = if config.use_llm {
            match LlmClassifier::from_provider(
                &config.llm_provider,
                &config.llm_model,
                config.openrouter_api_key.clone(),
            ) {
                Ok(c) => Some(c),
                Err(e) => {
                    return Err(crate::classify::errors::ClassifyError::Config(format!(
                        "LLM provider init failed: {e}"
                    )))
                }
            }
        } else {
            None
        };
        let taxonomy = TaxonomyRegistry::new(custom_taxonomy);
        let issue_type = IssueTypeTier::with_taxonomy(taxonomy.clone());
        let jira_project = JiraProjectTier::with_taxonomy(jira_project_mappings, taxonomy.clone());
        let override_tier = override_conn.map(|c| OverrideTier::with_taxonomy(c, taxonomy.clone()));
        Ok(Self {
            override_tier,
            exact,
            issue_type,
            regex,
            jira_project,
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

    /// Run the synchronous tiers (0, 1, 1.5, 2, 3, 3.5) for a single message.
    ///
    /// Returns `None` if no tier matched; callers may then invoke the
    /// async [`ClassificationEngine::classify`] for the LLM fallback.
    ///
    /// `commit_sha` and `repo_path` are optional; when supplied, the
    /// manual-override tier (Tier 0) is consulted first. When `issue_type`
    /// is supplied, the issue-type tier (Tier 1.5) is consulted between
    /// the exact and regex tiers.
    pub fn classify_sync(&self, message: &str, is_merge: bool) -> Option<ClassificationResult> {
        self.classify_sync_with_context(message, is_merge, None, None, None)
    }

    /// Context-aware variant of [`Self::classify_sync`] that supplies
    /// optional commit identity (for Tier 0) and PM-system issue type
    /// (for Tier 1.5).
    pub fn classify_sync_with_context(
        &self,
        message: &str,
        is_merge: bool,
        commit_sha: Option<&str>,
        repo_path: Option<&str>,
        issue_type: Option<&str>,
    ) -> Option<ClassificationResult> {
        // Tier 0: manual override (DB lookup, short-circuits everything).
        if let (Some(tier), Some(sha), Some(repo)) =
            (self.override_tier.as_ref(), commit_sha, repo_path)
        {
            if let Some(r) = tier.lookup(sha, repo) {
                return Some(r);
            }
        }

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

        // Tier 1.5: PM issue-type mapping.
        if let Some(it) = issue_type {
            if let Some(mut r) = self.issue_type.classify(it) {
                r.ticket_id = RegexMatcher::extract_ticket_id(message);
                return Some(r);
            }
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

        // Tier 3: JIRA project-key mapping (if configured).
        if !self.jira_project.is_empty() {
            if let Some(r) = self.jira_project.classify(message) {
                return Some(r);
            }
        }

        // Tier 3.5: fuzzy heuristics
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
    ///
    /// The LLM tier is consulted when the synchronous cascade returns no
    /// verdict, or when its verdict's confidence is at or below
    /// `config.llm_fallback_threshold`. Even then, the LLM verdict only
    /// replaces the existing one when its own confidence is strictly
    /// higher — a failed LLM call (network / auth / JSON-parse error)
    /// thus preserves the rule verdict rather than silently regressing it
    /// to `uncategorized`.
    pub async fn classify(&self, message: &str, is_merge: bool) -> ClassificationResult {
        let sync_result = self.classify_sync(message, is_merge);
        let should_try_llm = match &sync_result {
            Some(r) => r.confidence <= self.config.llm_fallback_threshold,
            None => true,
        };

        if should_try_llm {
            if let Some(llm) = &self.llm {
                if let Some(mut r) = llm.classify(message).await {
                    r.top_level = self.taxonomy.resolve(&r.category);
                    // Overwrite-guard: only replace the existing rule
                    // verdict when the LLM is strictly more confident.
                    let existing_conf = sync_result.as_ref().map_or(0.0, |s| s.confidence);
                    if r.confidence > existing_conf {
                        return r;
                    }
                }
            }
        }

        if let Some(r) = sync_result {
            return r;
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

    /// Test-only: replace the LLM tier's endpoint URL and inject an API
    /// key so the LLM path can be exercised against a mock server.
    #[cfg(test)]
    pub(crate) fn set_llm_endpoint_for_tests(&mut self, endpoint: &str, api_key: &str) {
        self.llm = Some(
            crate::classify::tiers::llm::LlmClassifier::new("test-model", Some(api_key.into()))
                .with_endpoint(endpoint),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::rules;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Build an engine with default rules and `use_llm: true`. The LLM
    /// endpoint is left pointing at the production default; tests rebind
    /// it via [`ClassificationEngine::set_llm_endpoint_for_tests`].
    fn engine_with_threshold(threshold: f64) -> ClassificationEngine {
        ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: true,
                llm_model: "test-model".to_string(),
                llm_provider: "openai".to_string(),
                openrouter_api_key: None,
                confidence_threshold: 0.5,
                llm_fallback_threshold: threshold,
            },
        )
        .expect("engine builds")
    }

    /// Threshold 0.3 escalates a catch-all 0.3 verdict to the LLM, and
    /// the LLM's strictly-higher-confidence verdict wins.
    #[tokio::test]
    async fn llm_fallback_threshold_upgrades_catchall_verdict() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"category\":\"feature\",\"subcategory\":\"security\",\"confidence\":0.9}"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let mut engine = engine_with_threshold(0.3);
        engine.set_llm_endpoint_for_tests(&server.uri(), "test-key");

        // A message that hits only the catch-all rule (no domain keywords).
        let r = engine.classify("xyzzy plugh frobnicate quux", false).await;
        assert_eq!(r.category, "feature");
        assert_eq!(r.subcategory.as_deref(), Some("security"));
        assert!(
            (r.confidence - 0.9).abs() < 1e-9,
            "confidence={}",
            r.confidence
        );
        assert_eq!(r.method, ClassificationMethod::LlmFallback);
    }

    /// Threshold 0.3 still triggers an LLM call, but on failure (non-2xx)
    /// the catch-all rule verdict at 0.3 is preserved — the overwrite-guard
    /// must not regress it to `uncategorized`.
    #[tokio::test]
    async fn llm_failure_preserves_catchall_verdict() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let mut engine = engine_with_threshold(0.3);
        engine.set_llm_endpoint_for_tests(&server.uri(), "test-key");

        let r = engine.classify("xyzzy plugh frobnicate quux", false).await;
        // Catch-all preserved.
        assert_eq!(r.category, "maintenance");
        assert_eq!(r.subcategory.as_deref(), Some("uncategorized"));
        assert!(
            (r.confidence - 0.3).abs() < 1e-9,
            "confidence={}",
            r.confidence
        );
        assert_ne!(r.method, ClassificationMethod::LlmFallback);
    }

    /// With threshold 0.0 (the default), the catch-all verdict is *not*
    /// escalated — historical behavior preserved.
    #[tokio::test]
    async fn default_threshold_does_not_escalate_catchall() {
        // Mount a mock that would *fail the test* if it's hit, by returning
        // a high-confidence verdict we'd otherwise expect to win.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "choices": [{
                    "message": {
                        "content": "{\"category\":\"feature\",\"subcategory\":\"security\",\"confidence\":0.9}"
                    }
                }]
            })))
            .mount(&server)
            .await;

        let mut engine = engine_with_threshold(0.0);
        engine.set_llm_endpoint_for_tests(&server.uri(), "test-key");

        let r = engine.classify("xyzzy plugh frobnicate quux", false).await;
        // Sync catch-all wins because 0.3 > 0.0 threshold.
        assert_eq!(r.category, "maintenance");
        assert_eq!(r.subcategory.as_deref(), Some("uncategorized"));
        assert!(
            (r.confidence - 0.3).abs() < 1e-9,
            "confidence={}",
            r.confidence
        );
    }
}

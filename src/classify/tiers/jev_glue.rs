//! The Jev-specific [`LlmClassifier`] methods (#111).
//!
//! Why: kept out of `llm.rs` and `jev.rs`, which both sit near the size cap.
//! What: `source: jev` construction, the category-set attachment, the run's
//! name preparation, and the test endpoint seam.
//! Test: `classify::tiers::jev_tests`.

use tracing::info;

use crate::classify::rules::CategoryDef;
use crate::classify::tiers::jev::{JevClassifier, JEV_MODEL};
use crate::classify::tiers::jev_error::JevError;
use crate::classify::tiers::jev_obfuscate::{KnownNames, RunNames};
use crate::classify::tiers::llm::LlmClassifier;
use crate::core::config::LlmConfig;
use crate::core::creds::CredentialSource;

impl LlmClassifier {
    /// `source: jev` → a classifier that routes every call through Jev.
    ///
    /// What: in payload-dump mode reads no key at all; otherwise reads the
    /// key from `cfg.effective_api_key_env()` (default `TYPESAFE_API_KEY`).
    /// The model is pinned to [`JEV_MODEL`]: the OpenRouter fallback
    /// `gpt-4o-mini` maps to it, any other `llm.model` is an error.
    /// Test: `jev_tests::from_llm_config_reads_the_typesafe_key`,
    /// `jev_tests::unpinned_model_is_refused`.
    ///
    /// # Errors
    ///
    /// No key outside payload-dump mode (the message names the variable),
    /// or a model other than [`JEV_MODEL`].
    pub(super) fn build_jev(
        cfg: &LlmConfig,
        model: &str,
        creds: &CredentialSource,
    ) -> Result<Self, String> {
        if model != JEV_MODEL && model != "gpt-4o-mini" {
            return Err(JevError::UnpinnedModel(model.to_string()).to_string());
        }
        let key_env = cfg.effective_api_key_env();
        // #111: a dump run sends nothing, so it never touches the key.
        let key = if cfg.jev.payload_dump_dir.is_some() {
            None
        } else {
            creds.get(key_env)
        };
        if key.is_none() && cfg.jev.payload_dump_dir.is_none() {
            return Err(format!(
                "LLM source 'jev' requires an API key but the environment variable \
                 '{key_env}' (set via llm.api_key_env) is not set or empty. Export \
                 your TypeSafe API key in it before running tga, or set \
                 llm.jev.payload_dump_dir to write the request bodies without sending."
            ));
        }
        info!(model = JEV_MODEL, api_key_env = %key_env, "LLM provider: jev (TypeSafe decision model)");
        let jev = JevClassifier::from_options(key, &cfg.jev).map_err(|e| e.to_string())?;
        let mut llm = Self::base(JEV_MODEL);
        llm.endpoint = String::new();
        llm.jev = Some(jev);
        Ok(llm)
    }

    /// Give the Jev backend its category set and the config names to
    /// pseudonymize; a no-op for every other provider.
    ///
    /// # Errors
    ///
    /// The category set is empty, too large, or uses a reserved code, or
    /// the name matcher cannot be built.
    pub(crate) fn with_jev_context(
        mut self,
        categories: Vec<CategoryDef>,
        names: KnownNames,
    ) -> Result<Self, JevError> {
        if let Some(jev) = self.jev.take() {
            self.jev = Some(jev.with_context(categories, names)?);
        }
        Ok(self)
    }

    /// Test seam: point the Jev backend at a mock server.
    #[cfg(test)]
    pub(crate) fn with_test_jev_endpoint(mut self, endpoint: &str) -> Self {
        self.jev = self.jev.take().map(|j| j.with_test_endpoint(endpoint));
        self
    }

    /// Whether this classifier routes through Jev.
    pub(crate) fn is_jev(&self) -> bool {
        self.jev.is_some()
    }

    /// Whether this classifier routes through Jev with `llm.jev.obfuscate`
    /// on, so the run's names must be learned first (#111).
    pub(crate) fn jev_obfuscates(&self) -> bool {
        self.jev.as_ref().is_some_and(JevClassifier::obfuscates)
    }

    /// Give the Jev pseudonymizer the database's names and the run's
    /// messages before the first request (see [`JevClassifier::prepare_run`]);
    /// a no-op otherwise.
    ///
    /// # Errors
    ///
    /// As [`JevClassifier::prepare_run`].
    pub(crate) fn prepare_batch(
        &self,
        messages: &[&str],
        names: &RunNames,
    ) -> Result<(), JevError> {
        match &self.jev {
            Some(jev) => jev.prepare_run(messages, names),
            None => Ok(()),
        }
    }
}

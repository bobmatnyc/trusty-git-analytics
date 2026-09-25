//! The `llm:` config section and the LLM-tier routing knobs.
//!
//! Why: moved out of `config/mod.rs` (#131) so that file stops growing past
//! the size cap while the LLM tier gains its fallback scope and effort keys.
//! What: [`LlmSource`], [`LlmConfig`], [`LlmEffort`] and [`LlmFallbackScope`].
//! Test: `core::config::tests::llm_config_*`,
//! `core::config::llm::tests::fallback_scope_and_effort_parse`.

use serde::{Deserialize, Serialize};

/// LLM provider selection for the classification LLM tier.
///
/// Why: operators need to switch between OpenRouter, AWS Bedrock, and the
/// direct Anthropic API without changing binary flags. An enum keeps the set
/// of valid values closed and type-safe.
/// What: three variants — `Openrouter`, `Bedrock`, and `AnthropicApi`.
/// Serde renames map to lowercase kebab-case strings matching the YAML schema.
/// Test: deserialization is covered by `llm_config_*` unit tests in this
/// module. Provider-specific behaviour is covered by `classify::tiers::llm`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LlmSource {
    /// Route through the OpenRouter API (OpenAI-compatible schema).
    ///
    /// Requires a key stored in the environment variable named by
    /// [`LlmConfig::api_key_env`] (default: `OPENROUTER_API_KEY`).
    #[default]
    Openrouter,
    /// Route through AWS Bedrock (IAM credential-chain auth, no API key).
    ///
    /// Only available when the binary is compiled with `--features bedrock`.
    /// Requires valid AWS credentials in the default chain (env vars, profile,
    /// SSO, IMDS, etc.). No secret is stored in the config; region and model
    /// are the only Bedrock-specific fields.
    Bedrock,
    /// Route through the Anthropic Messages API directly
    /// (`POST https://api.anthropic.com/v1/messages`).
    ///
    /// Requires a key in the environment variable named by
    /// [`LlmConfig::api_key_env`] (set it to e.g. `ANTHROPIC_API_KEY`).
    #[serde(rename = "anthropic-api")]
    AnthropicApi,
}

/// Anthropic `output_config.effort` level for the LLM tier (#131).
///
/// Why: Claude Sonnet 5 runs adaptive thinking by default at `high` effort;
/// a one-line commit classification rarely needs that, and effort is the
/// lever that sets its token spend.
/// What: the five documented levels. Sent only for `source: anthropic-api`
/// and only when set; Claude Haiku 4.5 rejects the parameter, so leave it
/// unset for Haiku.
/// Test: `core::config::llm::tests::fallback_scope_and_effort_parse`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LlmEffort {
    /// Least thinking; cheapest.
    Low,
    /// Between `low` and `high`.
    Medium,
    /// The API default.
    High,
    /// Between `high` and `max`.
    Xhigh,
    /// Most thinking; most expensive.
    Max,
}

impl LlmEffort {
    /// The wire value sent in `output_config.effort`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Which rule verdicts the LLM fallback tier is allowed to revisit (#111).
///
/// Why: `llm_fallback_threshold` (default 0.65) also routes low-confidence
/// rule HITS to the LLM, so the tier can overwrite an answer the rules gave.
/// The accuracy plan sends the LLM only the commits the rules left
/// unanswered.
/// What: `low_confidence` (default) keeps the threshold rule; `unanswered`
/// sends only verdicts with no category — the `uncategorized` placeholder
/// or the built-in `catch-all` rule — and ignores the threshold. Under
/// either scope a merge commit is never sent.
/// Test: `classify::pipeline_llm_tests::unanswered_scope_sends_only_abstentions`,
/// `classify::pipeline_llm_tests::merge_commits_never_reach_the_llm`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LlmFallbackScope {
    /// Every verdict with `confidence <= llm_fallback_threshold`.
    #[default]
    LowConfidence,
    /// Only verdicts where the rules gave no category.
    Unanswered,
}

/// Top-level LLM configuration section (`llm:` in YAML).
///
/// Why: the previous design placed LLM credentials inside
/// `classification.openrouter_api_key` and `classification.llm_provider`,
/// mixing transport concerns with classification tuning. The `llm:` section
/// separates *how to reach an LLM* from *when to use it*, and enables
/// first-class AWS Bedrock support (region + model, no stored secret).
/// What: groups provider selection, the environment-variable name holding
/// any required API key (never the key itself), an optional AWS region
/// override (Bedrock only), the model id, and the Anthropic effort level.
/// The section is optional; when absent the pipeline falls back to legacy
/// `classification.*` fields.
/// Test: `llm_config_parses_from_yaml` and `llm_source_defaults_to_openrouter`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    /// LLM provider to use.
    ///
    /// Valid values (YAML): `openrouter`, `bedrock`, `anthropic-api`.
    /// Defaults to `openrouter`.
    #[serde(default)]
    pub source: LlmSource,

    /// Name of the environment variable holding the API key.
    ///
    /// For `openrouter` and `anthropic-api` this is required. The value
    /// stored here is the **variable name** (e.g. `OPENROUTER_API_KEY`),
    /// never the secret itself. At use time the pipeline reads the env var.
    /// If the variable is unset or empty when a key-based source is in use,
    /// the LLM tier fails loudly with an actionable error — no silent no-ops.
    ///
    /// For `bedrock` this field is ignored; AWS credentials are resolved via
    /// the SDK's default credential chain.
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,

    /// AWS region for Bedrock invocations (Bedrock only).
    ///
    /// Ignored for `openrouter` and `anthropic-api`. When absent, the AWS SDK
    /// reads the region from the environment (`AWS_DEFAULT_REGION`,
    /// `AWS_REGION`, or the active profile) as usual.
    #[serde(default)]
    pub region: Option<String>,

    /// Model identifier (provider-specific), passed through unchanged.
    ///
    /// Examples:
    /// - OpenRouter: `"gpt-4o-mini"`, `"anthropic/claude-3-5-sonnet"`
    /// - Bedrock: an inference-profile id,
    ///   `"us.anthropic.claude-haiku-4-5-20251001-v1:0"` (a bare `anthropic.`
    ///   id fails for current Claude models)
    /// - Anthropic API: `"claude-haiku-4-5-20251001"`, `"claude-sonnet-5"`
    ///
    /// When absent, a provider-appropriate default is used.
    #[serde(default)]
    pub model: Option<String>,

    /// Anthropic `output_config.effort` (#131); `anthropic-api` only.
    ///
    /// Unset (the default) sends no effort parameter, which the API treats as
    /// `high`. Ignored by the other sources.
    #[serde(default)]
    pub effort: Option<LlmEffort>,
}

fn default_api_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            source: LlmSource::default(),
            api_key_env: default_api_key_env(),
            region: None,
            model: None,
            effort: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why (#131): a typo in `effort` or `llm_fallback_scope` must fail the
    /// config load, not reach the API as a value it rejects on every call.
    /// What: parses both keys, then asserts an unknown value is an error.
    /// Test: this test.
    #[test]
    fn fallback_scope_and_effort_parse() {
        let cfg: LlmConfig =
            serde_yaml::from_str("source: anthropic-api\neffort: low\n").expect("parse");
        assert_eq!(cfg.effort, Some(LlmEffort::Low));
        assert_eq!(cfg.effort.map(LlmEffort::as_str), Some("low"));
        assert!(serde_yaml::from_str::<LlmConfig>("effort: lowest\n").is_err());

        let scope: LlmFallbackScope = serde_yaml::from_str("unanswered").expect("scope");
        assert_eq!(scope, LlmFallbackScope::Unanswered);
        assert_eq!(LlmFallbackScope::default(), LlmFallbackScope::LowConfidence);
        assert!(serde_yaml::from_str::<LlmFallbackScope>("abstentions").is_err());
    }
}

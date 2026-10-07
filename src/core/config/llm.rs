//! The `llm:` config section and the LLM-tier routing knobs.
//!
//! Why: moved out of `config/mod.rs` (#131) so that file stops growing past
//! the size cap while the LLM tier gains its fallback scope and effort keys.
//! What: [`LlmSource`], [`LlmConfig`], [`LlmEffort`], [`LlmFallbackScope`]
//! and [`LlmContextItem`].
//! Test: `core::config::tests::llm_config_*`,
//! `core::config::llm::tests::fallback_scope_and_effort_parse`.

use serde::{Deserialize, Serialize};

use crate::core::errors::{Result, TgaError};

/// LLM provider selection for the classification LLM tier.
///
/// Why: operators need to switch between OpenRouter, AWS Bedrock, the
/// direct Anthropic API and TypeSafe Jev without changing binary flags. An
/// enum keeps the set of valid values closed and type-safe.
/// What: `Openrouter`, `Bedrock`, `AnthropicApi` and `Jev` (#111).
/// Serde renames map to lowercase kebab-case strings matching the YAML schema.
/// Test: deserialization is covered by `llm_config_*` unit tests in this
/// module. Provider-specific behaviour is covered by `classify::tiers::llm`.
// #137: `#[non_exhaustive]` so a new provider variant is additive; a
// downstream `match` needs a wildcard arm.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum LlmSource {
    /// Route through the OpenRouter API (OpenAI-compatible schema).
    ///
    /// Requires a key stored in the environment variable named by
    /// [`LlmConfig::api_key_env`] (default: `OPENROUTER_API_KEY`).
    #[default]
    Openrouter,
    /// Route through AWS Bedrock (IAM credential-chain auth, no API key).
    ///
    /// Included in the default build; a `--no-default-features` build rejects it.
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
    /// Route through TypeSafe's Jev decision model
    /// (`POST https://api.typesafe.ai/v1/systemone`, #111).
    ///
    /// Requires a key in the environment variable named by
    /// [`LlmConfig::api_key_env`]; left at its default, that is
    /// [`JEV_API_KEY_ENV`]. Every commit message is pseudonymized before it
    /// is sent; see [`JevOptions`].
    Jev,
}

/// Environment variable read for `source: jev` when `api_key_env` is left at
/// its default (#111).
pub const JEV_API_KEY_ENV: &str = "TYPESAFE_API_KEY";

/// Default per-run spend cap for `source: jev`, in US dollars (#111).
pub const JEV_DEFAULT_BUDGET_USD: f64 = 0.25;

/// Default heap-byte cap of the Jev name matcher (#111).
pub const JEV_DEFAULT_NAME_MATCHER_BYTES: usize = 64 << 20;

/// Settings that apply only to `source: jev` (`llm.jev:` in YAML, #111).
///
/// Why: Jev is a third-party hosted model, so what leaves the host and what
/// a run may spend both need an operator-owned knob.
/// What: the per-run spend cap, the opt-in pseudonymizer (`obfuscate`) and
/// the extra terms it must replace, and an optional directory that turns the run into a payload dump: each
/// outbound request body is written there and nothing is sent.
/// Test: `core::config::llm::tests::jev_source_and_options_parse`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct JevOptions {
    /// Per-run spend cap in US dollars (default [`JEV_DEFAULT_BUDGET_USD`]).
    /// No call starts once it could push the run's spend past this cap.
    #[serde(default = "default_jev_budget_usd")]
    pub budget_usd: f64,
    /// Extra operator terms (e.g. internal service names) replaced by
    /// `TERM_n` before a message is sent; matched case-insensitively as
    /// whole words. Empty by default.
    #[serde(default)]
    pub sensitive_terms: Vec<String>,
    /// When set, write each outbound request body as a JSON file in this
    /// directory and send nothing; no API key is needed. Calls are recorded
    /// as `skipped`. Point `tga classify` at a scratch database copy.
    #[serde(default)]
    pub payload_dump_dir: Option<std::path::PathBuf>,
    /// Extra operator regexes for internal record ids; each match is
    /// replaced by `ID_n` before any other rule runs (#111). An invalid
    /// regex fails the run before anything is sent. Empty by default.
    #[serde(default)]
    pub id_patterns: Vec<String>,
    /// Heap bytes the compiled name matcher may hold (default
    /// [`JEV_DEFAULT_NAME_MATCHER_BYTES`], 64 MiB; about 66 bytes per name,
    /// so roughly a million names). A run whose names do not fit fails
    /// before anything is sent; raise it for a larger name set (#111).
    #[serde(default = "default_jev_name_matcher_bytes")]
    pub name_matcher_bytes: usize,
    /// Pseudonymize each message before it is sent (default `false`).
    ///
    /// #111 (owner ruling 2026-10-06): Jev receives the commit message as
    /// stored unless this is `true`. `false` skips the pseudonymizer, the
    /// name, trailer and repository learning (with its database scans) and
    /// the name-matcher build; `sensitive_terms`, `id_patterns` and
    /// `name_matcher_bytes` then have no effect. `true` replaces names
    /// before sending and fails the run closed when the matcher cannot be
    /// built.
    #[serde(default)]
    pub obfuscate: bool,
}

fn default_jev_budget_usd() -> f64 {
    JEV_DEFAULT_BUDGET_USD
}

fn default_jev_name_matcher_bytes() -> usize {
    JEV_DEFAULT_NAME_MATCHER_BYTES
}

impl Default for JevOptions {
    fn default() -> Self {
        Self {
            budget_usd: JEV_DEFAULT_BUDGET_USD,
            sensitive_terms: Vec::new(),
            payload_dump_dir: None,
            id_patterns: Vec::new(),
            name_matcher_bytes: JEV_DEFAULT_NAME_MATCHER_BYTES,
            obfuscate: false,
        }
    }
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
#[non_exhaustive]
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
#[non_exhaustive]
pub enum LlmFallbackScope {
    /// Every verdict with `confidence <= llm_fallback_threshold`.
    #[default]
    LowConfidence,
    /// Only verdicts where the rules gave no category.
    Unanswered,
}

/// One commit fact `llm.context` can append to the LLM user prompt (#111).
///
/// Why: the message-only LLM scored well below a rater who also saw the
/// changed paths, the PR title and the issue type.
/// What: `paths` (changed file paths, capped by
/// [`LlmConfig::context_max_paths`] and [`LlmConfig::context_max_path_bytes`]),
/// `pr_title` (the lowest-numbered stored PR containing the commit) and
/// `issue_type` (the first linked work item's type). Any other value fails
/// the config load.
/// Test: `tests::llm_context_items_parse_and_unknown_items_fail_the_load`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LlmContextItem {
    /// Changed file paths from the `files` table.
    Paths,
    /// The linked pull request's title from `pull_requests`.
    PrTitle,
    /// The linked work item's `item_type` (e.g. `Bug`, `Story`).
    IssueType,
}

/// Default for [`LlmConfig::context_max_paths`] (#111).
pub const LLM_CONTEXT_DEFAULT_MAX_PATHS: usize = 30;

/// Default for [`LlmConfig::context_max_path_bytes`] (#111).
pub const LLM_CONTEXT_DEFAULT_MAX_PATH_BYTES: usize = 2048;

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
///
/// `#[non_exhaustive]` (#137): outside this crate, start from
/// [`LlmConfig::default`] and assign fields.
///
/// #111 (critic HIGH 3): an unknown key is a load error, so a `jev:`
/// option written one level too high (`llm.payload_dump_dir`) can never be
/// ignored and turn a payload dump into live requests.
/// Test: `tests::misplaced_llm_keys_fail_the_config_load`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct LlmConfig {
    /// LLM provider to use.
    ///
    /// Valid values (YAML): `openrouter`, `bedrock`, `anthropic-api`, `jev`.
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

    /// `source: jev` settings (#111); ignored by the other sources.
    #[serde(default)]
    pub jev: JevOptions,

    /// Commit facts appended to the user prompt after the message (#111).
    ///
    /// Empty (the default) sends the message alone, exactly as before. Each
    /// item adds a line to one delimited block when the database stores
    /// that fact for the commit. Every source honours it; for `jev` the
    /// block is part of the message text, and with `jev.obfuscate: true`
    /// it is pseudonymized too.
    #[serde(default)]
    pub context: Vec<LlmContextItem>,

    /// Most changed paths `context: [paths]` lists per commit (default
    /// [`LLM_CONTEXT_DEFAULT_MAX_PATHS`]).
    #[serde(default = "default_context_max_paths")]
    pub context_max_paths: usize,

    /// Most bytes of path text `context: [paths]` lists per commit (default
    /// [`LLM_CONTEXT_DEFAULT_MAX_PATH_BYTES`]); the list stops before the
    /// first path that would pass it.
    #[serde(default = "default_context_max_path_bytes")]
    pub context_max_path_bytes: usize,
}

fn default_context_max_paths() -> usize {
    LLM_CONTEXT_DEFAULT_MAX_PATHS
}

fn default_context_max_path_bytes() -> usize {
    LLM_CONTEXT_DEFAULT_MAX_PATH_BYTES
}

fn default_api_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}

/// [`JevOptions`] field names; a top-level key with one of these names is a
/// misplaced `llm.jev:` option (#111).
const JEV_OPTION_KEYS: [&str; 6] = [
    "obfuscate",
    "payload_dump_dir",
    "sensitive_terms",
    "id_patterns",
    "name_matcher_bytes",
    "budget_usd",
];

/// Reject a top-level config key that belongs under `llm:` or `llm.jev:`.
///
/// Why (#111, delta review HIGH): `Config` ignores unknown top-level keys
/// for Python compatibility, so a misplaced Jev block loaded with
/// `obfuscate` left off and real text was sent.
/// What: for a YAML mapping, fails on the key `jev`, any key starting with
/// `llm.`, or a [`JevOptions`] field name, naming the key and where it
/// belongs. Keys are read as text, as the typed parse reads them, so a
/// tagged or non-string scalar key is checked by its text. Every other key
/// passes. Text passes unchecked only when it is not a mapping with
/// scalar keys (a sequence, a map or sequence key, or a syntax error);
/// the typed parse fails on all of those, so nothing loads unchecked.
/// Test: `tests::misplaced_top_level_jev_keys_fail_the_config_load`,
/// `tests::misplaced_top_level_jev_key_fails_beside_a_duplicate_key`,
/// `tests::misplaced_top_level_jev_key_fails_beside_a_tagged_key`.
pub(super) fn reject_misplaced_top_level_keys(text: &str) -> Result<()> {
    // #111: a `Value` parse rejects a duplicate key at any depth that the
    // typed parse accepts; read only top-level keys, last one winning.
    // String keys: a `Value` key failed on `!!int abc`, which loads.
    type TopLevel = std::collections::HashMap<String, serde::de::IgnoredAny>;
    let Ok(map) = serde_yaml::from_str::<TopLevel>(text) else {
        return Ok(());
    };
    for key in map.keys().map(String::as_str) {
        let belongs = if key == "jev" || JEV_OPTION_KEYS.contains(&key) {
            "it belongs under `llm.jev:`"
        } else if key.starts_with("llm.") {
            "YAML does not split a dotted key; write it nested under `llm:`"
        } else {
            continue;
        };
        return Err(TgaError::ConfigError(format!(
            "top-level key `{key}` is not read; {belongs}"
        )));
    }
    Ok(())
}

impl LlmConfig {
    /// The environment variable the configured source reads its key from.
    ///
    /// Why (#111): `api_key_env` defaults to `OPENROUTER_API_KEY` for every
    /// source, and a Jev run must not send an OpenRouter key to TypeSafe.
    /// What: for `source: jev` with `api_key_env` left at that default,
    /// returns [`JEV_API_KEY_ENV`]; otherwise `api_key_env` as written.
    /// Test: `core::config::llm::tests::jev_source_and_options_parse`.
    pub fn effective_api_key_env(&self) -> &str {
        if self.source == LlmSource::Jev && self.api_key_env == default_api_key_env() {
            JEV_API_KEY_ENV
        } else {
            &self.api_key_env
        }
    }
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            source: LlmSource::default(),
            api_key_env: default_api_key_env(),
            region: None,
            model: None,
            effort: None,
            jev: JevOptions::default(),
            context: Vec::new(),
            context_max_paths: LLM_CONTEXT_DEFAULT_MAX_PATHS,
            context_max_path_bytes: LLM_CONTEXT_DEFAULT_MAX_PATH_BYTES,
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

    /// Why (#111): `source: jev` must parse, read `TYPESAFE_API_KEY` unless
    /// the operator names another variable, and default its budget, term
    /// list, dump mode and `obfuscate` (off) so an existing config needs no
    /// new keys.
    /// What: parses a minimal and a full `jev` section, checks the defaults,
    /// the key variable, and that a typo under `jev:` is rejected.
    /// Test: this test.
    #[test]
    fn jev_source_and_options_parse() {
        let min: LlmConfig = serde_yaml::from_str("source: jev\n").expect("parse");
        assert_eq!(min.source, LlmSource::Jev);
        assert_eq!(min.effective_api_key_env(), JEV_API_KEY_ENV);
        assert_eq!(min.jev, JevOptions::default());
        assert_eq!(min.jev.budget_usd, JEV_DEFAULT_BUDGET_USD);
        assert!(min.jev.sensitive_terms.is_empty());
        assert!(min.jev.payload_dump_dir.is_none());
        assert!(min.jev.id_patterns.is_empty());
        assert_eq!(min.jev.name_matcher_bytes, JEV_DEFAULT_NAME_MATCHER_BYTES);
        // #111 (owner ruling 2026-10-06): real commit text unless opted in.
        assert!(!min.jev.obfuscate);

        let full: LlmConfig = serde_yaml::from_str(
            "source: jev\napi_key_env: MY_JEV_KEY\njev:\n  budget_usd: 0.1\n  \
             sensitive_terms: [ledgerd, paygate]\n  payload_dump_dir: /tmp/jev\n  \
             obfuscate: true\n",
        )
        .expect("parse");
        assert!(full.jev.obfuscate);
        assert_eq!(full.effective_api_key_env(), "MY_JEV_KEY");
        assert_eq!(full.jev.budget_usd, 0.1);
        assert_eq!(full.jev.sensitive_terms, ["ledgerd", "paygate"]);
        assert_eq!(
            full.jev.payload_dump_dir.as_deref(),
            Some(std::path::Path::new("/tmp/jev"))
        );
        assert!(serde_yaml::from_str::<LlmConfig>("source: jev\njev:\n  budget: 1\n").is_err());

        // Other sources keep reading `api_key_env` as written.
        let or = LlmConfig::default();
        assert_eq!(or.effective_api_key_env(), "OPENROUTER_API_KEY");
    }

    /// Why (#111, critic HIGH 3): a `jev:` option written one level too high
    /// was ignored, so `llm: {source: jev, payload_dump_dir: ...}` sent live
    /// requests instead of writing a dump.
    /// What: each `jev:` option, and a typo, placed directly under `llm:` is
    /// a parse error naming the key, both for the section alone and through
    /// [`crate::core::config::Config::load`]; the documented shape loads.
    /// Test: this test.
    #[test]
    fn misplaced_llm_keys_fail_the_config_load() {
        for key in [
            "payload_dump_dir: ./dump",
            "budget_usd: 0.1",
            "sensitive_terms: [ledgerd]",
            "id_patterns: ['Q\\d+']",
            "name_matcher_bytes: 1024",
            "obfuscate: true",
            "modle: jev-1.13.0",
        ] {
            let e = serde_yaml::from_str::<LlmConfig>(&format!("source: jev\n{key}\n"))
                .expect_err(key)
                .to_string();
            let name = key.split(':').next().expect("key");
            assert!(e.contains(name), "{key}: {e}");
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        std::fs::write(&path, "llm:\n  source: jev\n  payload_dump_dir: ./dump\n").expect("write");
        assert!(
            crate::core::config::Config::load(&path).is_err(),
            "a misplaced payload_dump_dir loaded"
        );
        std::fs::write(
            &path,
            "llm:\n  source: jev\n  jev:\n    payload_dump_dir: ./dump\n",
        )
        .expect("write");
        let cfg = crate::core::config::Config::load(&path).expect("documented shape");
        assert!(cfg.llm.expect("llm").jev.payload_dump_dir.is_some());
    }

    /// Why (#111, delta review HIGH): `Config` tolerates unknown top-level
    /// keys, so a top-level `jev:` block, a dotted `llm.jev.obfuscate:` key
    /// or a bare `obfuscate:` loaded with `obfuscate` left off, and real
    /// text reached TypeSafe.
    /// What: each placement fails [`crate::core::config::Config::load`]
    /// with an error naming the key and where it belongs; another unknown
    /// top-level key still loads.
    /// Test: this test.
    #[test]
    fn misplaced_top_level_jev_keys_fail_the_config_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        let load = |yaml: &str| {
            std::fs::write(&path, yaml).expect("write");
            crate::core::config::Config::load(&path)
        };
        let jev_keys = [
            "obfuscate: true",
            "payload_dump_dir: ./dump",
            "sensitive_terms: [ledgerd]",
            "id_patterns: ['Q\\d+']",
            "name_matcher_bytes: 1024",
            "budget_usd: 0.1",
        ];
        let mut cases: Vec<(String, &str, &str)> = vec![
            (
                "llm:\n  source: jev\njev:\n  obfuscate: true\n".into(),
                "jev",
                "llm.jev",
            ),
            (
                "llm:\n  source: jev\nllm.jev.obfuscate: true\n".into(),
                "llm.jev.obfuscate",
                "nested",
            ),
            ("llm.source: jev\n".into(), "llm.source", "nested"),
        ];
        for key in jev_keys {
            let name = key.split(':').next().expect("key");
            cases.push((format!("llm:\n  source: jev\n{key}\n"), name, "llm.jev"));
        }
        let mut loaded = Vec::new();
        for (yaml, key, belongs) in &cases {
            match load(yaml) {
                Ok(_) => loaded.push(*key),
                Err(e) => {
                    let e = e.to_string();
                    assert!(e.contains(&format!("`{key}`")), "{key}: {e}");
                    assert!(e.contains(belongs), "{key}: {e}");
                }
            }
        }
        assert!(loaded.is_empty(), "misplaced keys loaded: {loaded:?}");

        // Documented Python sections `Config` does not model keep loading.
        let cfg = load(
            "llm:\n  source: jev\n  jev:\n    obfuscate: true\nvelocity: {}\nquality_report: {}\n",
        )
        .expect("unknown top-level key tolerated");
        assert!(cfg.llm.expect("llm").jev.obfuscate);
    }

    /// Why (#111): `llm.context` is a real field under `deny_unknown_fields`,
    /// and a typo in one of its items must fail the load, not silently send
    /// less context than the operator asked for.
    /// What: the three items and both caps load through
    /// [`crate::core::config::Config::load`]; the caps default to 30 paths
    /// and 2048 bytes; an unknown item, or a bare scalar instead of a list,
    /// fails the load and the error names the item.
    /// Test: this test.
    #[test]
    fn llm_context_items_parse_and_unknown_items_fail_the_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        let load = |yaml: &str| {
            std::fs::write(&path, yaml).expect("write");
            crate::core::config::Config::load(&path)
        };
        let cfg = load(
            "llm:\n  source: openrouter\n  context: [paths, pr_title, issue_type]\n  \
             context_max_paths: 5\n  context_max_path_bytes: 100\n",
        )
        .unwrap_or_else(|e| panic!("documented shape: {e}"));
        let llm = serde_yaml::to_value(cfg.llm.expect("llm")).expect("serialize");
        assert_eq!(
            llm["context"],
            serde_yaml::from_str::<serde_yaml::Value>("[paths, pr_title, issue_type]")
                .expect("yaml")
        );
        assert_eq!(llm["context_max_paths"], serde_yaml::Value::from(5));
        assert_eq!(llm["context_max_path_bytes"], serde_yaml::Value::from(100));

        let default = serde_yaml::to_value(LlmConfig::default()).expect("serialize");
        assert_eq!(default["context"], serde_yaml::Value::Sequence(Vec::new()));
        assert_eq!(default["context_max_paths"], serde_yaml::Value::from(30));
        assert_eq!(
            default["context_max_path_bytes"],
            serde_yaml::Value::from(2048)
        );

        for (yaml, named) in [
            ("llm:\n  context: [paths, diff]\n", "diff"),
            ("llm:\n  context: [PR_TITLE]\n", "PR_TITLE"),
            ("llm:\n  context: paths\n", "paths"),
        ] {
            let e = load(yaml).expect_err(yaml).to_string();
            assert!(e.contains(named), "{yaml:?}: {e}");
        }
    }

    /// #111: a duplicate key the typed parse accepts must not skip the check.
    #[test]
    fn misplaced_top_level_jev_key_fails_beside_a_duplicate_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        let load = |yaml: &str| {
            std::fs::write(&path, yaml).expect("write");
            crate::core::config::Config::load(&path)
        };
        let misplaced = "jev:\n  obfuscate: true\n";
        let cases = [
            ("top level", "velocity: {}\nvelocity: {}\n"),
            (
                "developer_aliases",
                "developer_aliases:\n  Ann: [a@x.io]\n  Ann: [b@x.io]\n",
            ),
            ("ignored section", "velocity:\n  window: 1\n  window: 2\n"),
        ];
        let mut loaded = Vec::new();
        for (name, dup) in cases {
            match load(&format!("llm:\n  source: jev\n{dup}{misplaced}")) {
                Ok(_) => loaded.push(name),
                Err(e) => assert!(e.to_string().contains("`jev`"), "{name}: {e}"),
            }
        }
        assert!(
            loaded.is_empty(),
            "loaded despite misplaced `jev`: {loaded:?}"
        );

        // Text the key-only parse rejects still fails the typed parse.
        for yaml in [
            "- jev:\n    obfuscate: true\n",
            "jev: {obfuscate: true}\nllm: [\n",
        ] {
            assert!(load(yaml).is_err(), "loaded: {yaml:?}");
        }
    }

    /// Why (#111): a key whose tag its text does not fit (`!!int abc`)
    /// failed the key-only parse, which then checked nothing, while the
    /// typed parse read the key as text and loaded.
    /// What: a misplaced `jev:` beside such a key, or itself tagged, fails
    /// the load; non-string scalar keys alone still load.
    /// Test: this test.
    #[test]
    fn misplaced_top_level_jev_key_fails_beside_a_tagged_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("config.yaml");
        let load = |yaml: &str| {
            std::fs::write(&path, yaml).expect("write");
            crate::core::config::Config::load(&path)
        };
        let mut loaded = Vec::new();
        for (name, extra) in [
            ("int tag", "!!int abc: 1\njev:\n  obfuscate: true\n"),
            ("bool tag", "!!bool xyz: 1\njev:\n  obfuscate: true\n"),
            ("tagged jev", "!foo jev: {obfuscate: true}\n"),
            (
                "scalar keys",
                "1: a\n~: b\ntrue: c\njev:\n  obfuscate: true\n",
            ),
        ] {
            match load(&format!("llm:\n  source: jev\n{extra}")) {
                Ok(_) => loaded.push(name),
                Err(e) => assert!(e.to_string().contains("`jev`"), "{name}: {e}"),
            }
        }
        assert!(
            loaded.is_empty(),
            "loaded despite misplaced `jev`: {loaded:?}"
        );

        let cfg = load("llm:\n  source: jev\n  jev:\n    obfuscate: true\n1: a\n~: b\ntrue: c\n")
            .expect("non-string scalar keys are not refused");
        assert!(cfg.llm.expect("llm").jev.obfuscate);

        // A map or sequence key fails the key-only parse and the typed one.
        for key in ["? {a: 1}\n: x\n", "? [a]\n: x\n"] {
            let yaml = format!("{key}jev: {{obfuscate: true}}\n");
            assert!(load(&yaml).is_err(), "loaded: {yaml:?}");
        }
    }
}

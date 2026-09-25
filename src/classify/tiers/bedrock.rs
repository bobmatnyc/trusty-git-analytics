//! AWS Bedrock LLM provider for tier-4 classification.
//!
//! Feature-gated behind `bedrock`. When the feature is disabled the module
//! still compiles and exposes [`BedrockClassifier`] as a stub that returns
//! a clear error explaining the build configuration.
//!
//! Why: organizations on AWS often prefer Bedrock (private VPC, IAM-based
//! auth, no per-request data egress to a third-party SaaS) over OpenRouter
//! or OpenAI for LLM access. Making it an optional feature keeps the
//! default binary lean for users who don't need it. As of #2411 the Bedrock
//! wire integration (region resolution, the AWS credential chain, and the
//! Converse request/response conversion) is NO LONGER a private aws-sdk
//! `InvokeModel` port here — it bridges onto the shared
//! `trusty_common::inference::bedrock` Converse adapter (#2407), the same one
//! trusty-code and the trusty-mpm SM provider consume, so the wire mechanics
//! live in exactly one place. This module keeps the tga-specific policy:
//! sequential best-effort batch classification (never crashes on a bad
//! payload) and the shared `SYSTEM_PROMPT`/[`LlmVerdict`] parsing contract
//! from `llm.rs`.

// #131: the reply is parsed by `llm_prompt::resolve`, shared with the HTTP path.
#[cfg(all(test, not(feature = "bedrock")))]
use crate::classify::tiers::llm::SYSTEM_PROMPT;
use crate::classify::tiers::llm_prompt::LlmUsage;
use trusty_common::inference::{ChatMessage, ChatRequest};

/// AWS Bedrock-backed LLM classifier targeting Anthropic Claude on Bedrock.
///
/// Uses the AWS default credential provider chain (env vars, profile,
/// SSO, IMDS, etc.), resolved lazily by the shared
/// `trusty_common::inference::bedrock::BedrockAdapter` on the first call.
pub struct BedrockClassifier {
    /// Bedrock model id (e.g. `us.anthropic.claude-haiku-4-5-20251001-v1:0`).
    #[allow(dead_code)] // only read under the `bedrock` feature.
    pub(crate) model: String,
    /// Shared Converse adapter (owns region + lazily-built AWS client).
    #[cfg(feature = "bedrock")]
    inner: trusty_common::inference::BedrockAdapter,
}

/// Default Bedrock model id when the caller doesn't override it.
///
/// Why (#111): current Claude models on Bedrock are invoked on demand only
/// through a cross-region inference profile (`us.` prefix); the bare
/// `anthropic.` id fails. The previous default, Claude 3 Haiku, was not
/// invocable in us-east-1.
/// Test: `request_tests::default_model_is_a_us_inference_profile`.
pub const DEFAULT_BEDROCK_MODEL: &str = "us.anthropic.claude-haiku-4-5-20251001-v1:0";

/// Build the Converse request for one commit message.
///
/// Why (#111): Claude Sonnet 5 on Bedrock rejects any request that sets
/// `temperature`, so no Bedrock request carries one, whatever the model.
/// What: system prompt, the shared user message, `max_tokens` 256, and
/// `temperature` left unset. The shared adapter maps an unset temperature to
/// an omitted `inferenceConfig.temperature`.
/// Test: `request_tests::sonnet5_request_has_no_temperature`,
/// `request_tests::default_model_request_has_no_temperature`.
#[cfg_attr(not(feature = "bedrock"), allow(dead_code))]
pub(crate) fn converse_request(model: &str, system: &str, message: &str) -> ChatRequest {
    let mut req = ChatRequest::new(
        model.to_string(),
        vec![
            ChatMessage::system(system),
            ChatMessage::user(format!("Classify this commit message:\n\n{message}")),
        ],
    );
    req.max_tokens = Some(256);
    req
}

impl BedrockClassifier {
    /// Construct a new Bedrock classifier.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a clear message when the binary was built without
    /// `--features bedrock`. With the feature enabled, this always returns
    /// `Ok` — the shared adapter defers AWS credential/client construction to
    /// the first `classify_one` call (#2245 lazy-construction guarantee).
    ///
    /// Why: surfacing the missing-feature condition as an error (rather
    /// than silently no-oping) helps operators diagnose deployments.
    /// What: builds a [`trusty_common::inference::BedrockAdapter`] pinned to
    /// the default region resolution (`TRUSTY_AWS_REGION` > `AWS_REGION` >
    /// `us-east-1`).
    /// Test: building with and without `--features bedrock` verifies both
    /// arms compile and behave correctly at startup.
    #[cfg(feature = "bedrock")]
    pub async fn new(model: &str) -> Result<Self, String> {
        Ok(Self {
            model: model.to_string(),
            inner: trusty_common::inference::BedrockAdapter::new(None),
        })
    }

    /// Construct a new Bedrock classifier with an explicit AWS region.
    ///
    /// Why: operators who specify a `region:` in the `llm:` config section
    /// need a way to override the SDK's default region selection without
    /// mutating environment variables.
    /// What: builds a [`trusty_common::inference::BedrockAdapter`] pinned to
    /// `region` (falling back to the shared adapter's own resolution when
    /// `None`, matching `new`'s semantics).
    /// Test: indirectly tested via config-driven construction when `region:`
    /// is set in the `llm:` YAML block.
    #[cfg(feature = "bedrock")]
    pub async fn with_region(model: &str, region: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            model: model.to_string(),
            inner: trusty_common::inference::BedrockAdapter::new(region),
        })
    }

    /// Stub constructor returned when the `bedrock` feature is disabled.
    ///
    /// Always errors so the caller can surface a build-time guidance
    /// message to the operator.
    ///
    /// Why: the SDK is heavy (~10MB of generated code) — gating it behind
    /// a feature avoids paying that cost for users who don't need Bedrock.
    /// What: returns a clear `Err` with rebuild instructions.
    /// Test: confirmed by `bedrock_stub_returns_error_without_feature`.
    #[cfg(not(feature = "bedrock"))]
    pub async fn new(_model: &str) -> Result<Self, String> {
        Err("bedrock feature not compiled in — rebuild with --features bedrock".to_string())
    }

    /// Stub `with_region` when the `bedrock` feature is disabled.
    #[cfg(not(feature = "bedrock"))]
    pub async fn with_region(_model: &str, _region: Option<&str>) -> Result<Self, String> {
        Err("bedrock feature not compiled in — rebuild with --features bedrock".to_string())
    }

    /// Send one commit message to Bedrock and return the raw reply text and
    /// token usage.
    ///
    /// Why (#131): verdict parsing, category validation and token accounting
    /// live in `llm_prompt` so the HTTP and Bedrock paths cannot drift.
    /// What: Converse call built by [`converse_request`] (no `temperature`);
    /// a transport error yields `(None, None)` (best-effort, never crashes
    /// the batch).
    /// Test: the request shape is `request_tests::sonnet5_request_has_no_temperature`;
    /// the call itself needs live AWS credentials. The stub path is
    /// `bedrock_stub_returns_error_without_feature`.
    #[cfg(feature = "bedrock")]
    pub async fn complete(
        &self,
        system: &str,
        message: &str,
    ) -> (Option<String>, Option<LlmUsage>) {
        use tracing::warn;
        use trusty_common::inference::InferenceAdapter;

        let req = converse_request(&self.model, system, message);
        match self.inner.chat(&req).await {
            Ok(resp) => {
                let usage = LlmUsage {
                    input_tokens: u64::from(resp.usage.prompt_tokens),
                    output_tokens: u64::from(resp.usage.completion_tokens),
                };
                (resp.first_text(), Some(usage))
            }
            Err(e) => {
                warn!(error = %e, "bedrock converse call failed");
                (None, None)
            }
        }
    }

    /// Stub when the feature is disabled: no reply, no usage.
    #[cfg(not(feature = "bedrock"))]
    pub async fn complete(
        &self,
        _system: &str,
        _message: &str,
    ) -> (Option<String>, Option<LlmUsage>) {
        (None, None)
    }
}

#[cfg(all(test, not(feature = "bedrock")))]
mod tests {
    use super::*;

    /// Without the `bedrock` feature, [`BedrockClassifier::new`] must
    /// error with the build-instruction message.
    ///
    /// Why: the message is the public-facing handle for operators to
    /// understand why `--provider bedrock` failed — if it ever drifts,
    /// docs / runbooks become wrong.
    /// What: calls `BedrockClassifier::new` and asserts the error string.
    /// Test: assert the string starts with "bedrock feature not compiled".
    #[tokio::test]
    async fn bedrock_stub_returns_error_without_feature() {
        let result = BedrockClassifier::new(DEFAULT_BEDROCK_MODEL).await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("must error without feature"),
        };
        assert!(err.contains("bedrock feature not compiled in"));
    }

    /// Why: `SYSTEM_PROMPT` is shared from llm.rs; if the import breaks the
    /// stub path would fail to compile — this test ensures both features of
    /// that sharing (accessible constant, mentions "complexity") hold without
    /// the bedrock feature.
    /// What: asserts the shared constant is visible and mentions complexity.
    /// Test: pure compile + substring check.
    #[test]
    fn shared_system_prompt_contains_complexity_instruction() {
        assert!(
            SYSTEM_PROMPT.contains("complexity"),
            "shared SYSTEM_PROMPT must instruct the model to return a complexity score"
        );
    }
}

/// #111: the Converse request shape, built without any AWS call.
#[cfg(test)]
mod request_tests {
    use super::*;

    /// A Claude Sonnet 5 inference-profile id, as a user would set it in
    /// `llm.model`. The builder does not branch on the id.
    const SONNET_5: &str = "us.anthropic.claude-sonnet-5";

    /// Serialize the request `model` would send and return it as JSON.
    fn request_json(model: &str) -> serde_json::Value {
        let req = converse_request(model, "sys", "fix: null check");
        serde_json::to_value(&req).expect("serialize ChatRequest")
    }

    /// Why (#111): Sonnet 5 on Bedrock rejects any request that sets
    /// `temperature`.
    /// What: serializes the Sonnet 5 request and asserts no `temperature` key.
    /// Test: this test.
    #[test]
    fn sonnet5_request_has_no_temperature() {
        let json = request_json(SONNET_5);
        assert!(json.get("temperature").is_none(), "{json}");
        assert_eq!(json["model"], SONNET_5);
        assert_eq!(json["max_tokens"], 256);
    }

    /// Why (#111): no temperature is sent for any model, so a model-name
    /// allowlist that keeps it for Haiku fails here.
    /// What: serializes the default-model request and asserts no
    /// `temperature` key.
    /// Test: this test.
    #[test]
    fn default_model_request_has_no_temperature() {
        let json = request_json(DEFAULT_BEDROCK_MODEL);
        assert!(json.get("temperature").is_none(), "{json}");
        assert_eq!(json["model"], DEFAULT_BEDROCK_MODEL);
    }

    /// Why (#111): a bare `anthropic.` id of a current Claude model fails
    /// on-demand invocation; the default must be a `us.` inference profile.
    /// What: checks the prefix and that the default is Haiku 4.5.
    /// Test: this test.
    #[test]
    fn default_model_is_a_us_inference_profile() {
        assert!(
            DEFAULT_BEDROCK_MODEL.starts_with("us.anthropic."),
            "{DEFAULT_BEDROCK_MODEL}"
        );
        assert!(
            DEFAULT_BEDROCK_MODEL.contains("claude-haiku-4-5"),
            "{DEFAULT_BEDROCK_MODEL}"
        );
    }
}

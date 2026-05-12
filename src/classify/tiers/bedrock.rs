//! AWS Bedrock LLM provider for tier-4 classification.
//!
//! Feature-gated behind `bedrock`. When the feature is disabled the module
//! still compiles and exposes [`BedrockClassifier`] as a stub that returns
//! a clear error explaining the build configuration.
//!
//! Why: organizations on AWS often prefer Bedrock (private VPC, IAM-based
//! auth, no per-request data egress to a third-party SaaS) over OpenRouter
//! or OpenAI for LLM access. Making it an optional feature keeps the
//! default binary lean for users who don't need it.

use crate::classify::tiers::ClassificationResult;

/// AWS Bedrock-backed LLM classifier targeting Anthropic Claude on Bedrock.
///
/// Uses the AWS default credential provider chain (env vars, profile,
/// SSO, IMDS, etc.). Requests are formatted as the Anthropic Messages API
/// with `anthropic_version: "bedrock-2023-05-31"` per Bedrock's contract.
pub struct BedrockClassifier {
    /// Bedrock model id (e.g. `anthropic.claude-3-haiku-20240307-v1:0`).
    #[allow(dead_code)] // only read under the `bedrock` feature.
    pub(crate) model: String,
    #[cfg(feature = "bedrock")]
    client: aws_sdk_bedrockruntime::Client,
}

/// Default Bedrock model id when the caller doesn't override it.
pub const DEFAULT_BEDROCK_MODEL: &str = "anthropic.claude-3-haiku-20240307-v1:0";

impl BedrockClassifier {
    /// Construct a new Bedrock classifier.
    ///
    /// # Errors
    ///
    /// Returns `Err` with a clear message when the binary was built without
    /// `--features bedrock`. With the feature enabled, this returns `Ok`
    /// after the AWS credential chain initializes.
    ///
    /// Why: surfacing the missing-feature condition as an error (rather
    /// than silently no-oping) helps operators diagnose deployments.
    /// What: loads default AWS config and constructs the SDK client.
    /// Test: building with and without `--features bedrock` verifies both
    /// arms compile and behave correctly at startup.
    #[cfg(feature = "bedrock")]
    pub async fn new(model: &str) -> Result<Self, String> {
        // Use the explicit BehaviorVersion (latest()) form to avoid the
        // deprecation on `load_from_env` without taking the cross-version
        // feature flag.
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
            .load()
            .await;
        let client = aws_sdk_bedrockruntime::Client::new(&config);
        Ok(Self {
            model: model.to_string(),
            client,
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

    /// Classify a batch of commit messages via Bedrock, returning one
    /// [`ClassificationResult`] per input message.
    ///
    /// Matches the OpenRouter path's contract: failures yield `None` in
    /// place of a verdict so the pipeline can fall back to uncategorized
    /// without crashing.
    ///
    /// Why: the LLM tier is best-effort; a single bad payload must not
    /// poison an entire batch.
    /// What: sequentially invokes `InvokeModel` for each message.
    /// Test: integration-tested when AWS credentials are present; stubbed
    /// path tested in `bedrock_stub_returns_error_without_feature`.
    #[cfg(feature = "bedrock")]
    pub async fn classify_batch_bedrock(
        &self,
        messages: &[&str],
    ) -> Vec<Option<ClassificationResult>> {
        let mut out = Vec::with_capacity(messages.len());
        for msg in messages {
            out.push(self.classify_one(msg).await);
        }
        out
    }

    /// Stub batch classifier when the feature is disabled. Always returns
    /// `None`s — the pipeline treats this as "uncategorized".
    #[cfg(not(feature = "bedrock"))]
    pub async fn classify_batch_bedrock(
        &self,
        messages: &[&str],
    ) -> Vec<Option<ClassificationResult>> {
        vec![None; messages.len()]
    }

    /// Classify a single commit message via Bedrock InvokeModel.
    #[cfg(feature = "bedrock")]
    async fn classify_one(&self, message: &str) -> Option<ClassificationResult> {
        use crate::core::models::ClassificationMethod;
        use aws_sdk_bedrockruntime::primitives::Blob;
        use serde::Deserialize;
        use tracing::warn;

        let body = serde_json::json!({
            "anthropic_version": "bedrock-2023-05-31",
            "max_tokens": 256,
            "temperature": 0.0,
            "system": "You are a git commit classifier. Respond with ONLY a JSON \
                object: {\"category\": \"feature|bugfix|chore|documentation|refactor|test|ci|performance|style|build|revert|merge|breaking|uncategorized\", \
                \"subcategory\": \"optional string or null\", \"confidence\": 0.0-1.0}. \
                No prose, no markdown.",
            "messages": [
                {"role": "user", "content": format!("Classify this commit message:\n\n{message}")}
            ]
        });
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "bedrock body serialize failed");
                return None;
            }
        };

        let resp = match self
            .client
            .invoke_model()
            .model_id(&self.model)
            .content_type("application/json")
            .accept("application/json")
            .body(Blob::new(body_bytes))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "bedrock invoke_model failed");
                return None;
            }
        };

        let raw = resp.body.into_inner();

        #[derive(Deserialize)]
        struct BedrockResponse {
            content: Vec<ContentBlock>,
        }
        #[derive(Deserialize)]
        struct ContentBlock {
            #[serde(default)]
            text: Option<String>,
        }
        let parsed: BedrockResponse = match serde_json::from_slice(&raw) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "bedrock response decode failed");
                return None;
            }
        };
        let text = parsed
            .content
            .into_iter()
            .find_map(|b| b.text)
            .unwrap_or_default();

        #[derive(Deserialize)]
        struct Verdict {
            category: String,
            #[serde(default)]
            subcategory: Option<String>,
            #[serde(default = "default_confidence")]
            confidence: f64,
        }
        fn default_confidence() -> f64 {
            0.5
        }
        let verdict: Verdict = match serde_json::from_str(text.trim()) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, raw = %text, "bedrock verdict parse failed");
                return None;
            }
        };

        Some(ClassificationResult {
            category: verdict.category,
            subcategory: verdict.subcategory,
            top_level: None,
            confidence: verdict.confidence.clamp(0.0, 1.0),
            method: ClassificationMethod::LlmFallback,
            ticket_id: None,
        })
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
        let result = BedrockClassifier::new("anthropic.claude-3-haiku-20240307-v1:0").await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("must error without feature"),
        };
        assert!(err.contains("bedrock feature not compiled in"));
    }
}

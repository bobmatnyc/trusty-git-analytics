//! Tier 4: optional LLM fallback.
//!
//! Sends an OpenAI-compatible chat completion request asking the model to
//! emit a JSON object with `category`, `subcategory`, and `confidence`. The
//! LLM is consulted only when tiers 1–3 all failed and the engine has been
//! configured with `use_llm = true`.
//!
//! All failures are **non-fatal**: a network error, parse error, or missing
//! API key results in `None` so the pipeline can fall back to
//! "uncategorized" rather than crashing.

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// OpenAI-compatible chat completion endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// System prompt instructing the model to return strict JSON.
const SYSTEM_PROMPT: &str = "You are a git commit classifier. Respond with ONLY a JSON \
object: {\"category\": \"feature|bugfix|chore|documentation|refactor|test|ci|performance|style|build|revert|merge|breaking|uncategorized\", \
\"subcategory\": \"optional string or null\", \"confidence\": 0.0-1.0}. No prose, no markdown.";

/// Tier-4 LLM-fallback classifier.
pub struct LlmClassifier {
    client: Client,
    model: String,
    api_key: Option<String>,
    endpoint: String,
}

impl LlmClassifier {
    /// Construct a new LLM classifier.
    ///
    /// `model` is provider-specific (e.g. `"gpt-4o-mini"`). If `api_key` is
    /// `None`, classification calls will return `None` immediately.
    pub fn new(model: &str, api_key: Option<String>) -> Self {
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key,
            endpoint: DEFAULT_ENDPOINT.to_string(),
        }
    }

    /// Override the chat-completions endpoint URL (e.g. for Azure / local proxies).
    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Classify `message` by calling the LLM.
    ///
    /// Returns `None` if the LLM is disabled (no API key), the request
    /// fails, or the response cannot be parsed.
    pub async fn classify(&self, message: &str) -> Option<ClassificationResult> {
        let api_key = self.api_key.as_deref()?;

        let body = ChatRequest {
            model: &self.model,
            messages: vec![
                ChatMessage {
                    role: "system",
                    content: SYSTEM_PROMPT.to_string(),
                },
                ChatMessage {
                    role: "user",
                    content: format!("Classify this commit message:\n\n{message}"),
                },
            ],
            temperature: 0.0,
            response_format: Some(ResponseFormat {
                kind: "json_object".to_string(),
            }),
        };

        let response = match self
            .client
            .post(&self.endpoint)
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, "LLM request failed");
                return None;
            }
        };

        if !response.status().is_success() {
            warn!(status = %response.status(), "LLM returned non-success status");
            return None;
        }

        let parsed: ChatResponse = match response.json().await {
            Ok(j) => j,
            Err(e) => {
                warn!(error = %e, "LLM response JSON decode failed");
                return None;
            }
        };

        let content = parsed.choices.first()?.message.content.clone();
        debug!(content = %content, "LLM raw response");

        let verdict: LlmVerdict = serde_json::from_str(&content)
            .map_err(|e| warn!(error = %e, "LLM JSON parse failed"))
            .ok()?;

        Some(ClassificationResult {
            category: verdict.category,
            subcategory: verdict.subcategory,
            top_level: None, // resolved by ClassificationEngine via the taxonomy registry
            confidence: verdict.confidence.clamp(0.0, 1.0),
            method: ClassificationMethod::LlmFallback,
            ticket_id: None,
        })
    }
}

// ---- request / response DTOs (private) ----

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<ChatMessage>,
    temperature: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    content: String,
}

#[derive(Serialize)]
struct ResponseFormat {
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    content: String,
}

#[derive(Deserialize)]
struct LlmVerdict {
    category: String,
    #[serde(default)]
    subcategory: Option<String>,
    #[serde(default = "default_confidence")]
    confidence: f64,
}

fn default_confidence() -> f64 {
    0.5
}

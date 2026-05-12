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

use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::classify::tiers::ClassificationResult;
use crate::core::models::ClassificationMethod;

/// OpenAI-compatible chat completion endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.openai.com/v1/chat/completions";

/// OpenRouter chat completion endpoint (OpenAI-compatible schema).
const OPENROUTER_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Project identity sent to OpenRouter as `HTTP-Referer` (for usage analytics).
const OPENROUTER_REFERER: &str = "https://github.com/bobmatnyc/trusty-git-analytics";

/// Project identity sent to OpenRouter as `X-Title`.
const OPENROUTER_TITLE: &str = "trusty-git-analytics";

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
    /// Provider-specific extra headers (e.g. OpenRouter attribution).
    extra_headers: HeaderMap,
}

impl LlmClassifier {
    /// Construct a new LLM classifier targeting the OpenAI chat-completions
    /// endpoint.
    ///
    /// `model` is provider-specific (e.g. `"gpt-4o-mini"`). If `api_key` is
    /// `None`, classification calls will return `None` immediately.
    pub fn new(model: &str, api_key: Option<String>) -> Self {
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key,
            endpoint: DEFAULT_ENDPOINT.to_string(),
            extra_headers: HeaderMap::new(),
        }
    }

    /// Construct an LLM classifier configured for a specific provider.
    ///
    /// `provider` accepts:
    /// - `"openrouter"` — uses the OpenRouter endpoint. API key comes from
    ///   `openrouter_api_key` if `Some`, else the `OPENROUTER_API_KEY`
    ///   environment variable. Adds the `HTTP-Referer` / `X-Title` headers
    ///   that OpenRouter uses for attribution.
    /// - `"openai"` — uses the OpenAI endpoint. API key comes from
    ///   `OPENAI_API_KEY`.
    /// - `"auto"` (default) — tries OpenRouter first, falls back to OpenAI.
    ///
    /// If no API key can be resolved, the classifier is still constructed
    /// but every `classify` call will short-circuit to `None`.
    pub fn from_provider(provider: &str, model: &str, openrouter_api_key: Option<String>) -> Self {
        let normalized = provider.trim().to_ascii_lowercase();
        match normalized.as_str() {
            "openrouter" => Self::build_openrouter(model, openrouter_api_key),
            "openai" => Self::new(model, std::env::var("OPENAI_API_KEY").ok()),
            "auto" | "" => {
                let or_key =
                    openrouter_api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
                if or_key.is_some() {
                    info!("LLM provider auto-selected: openrouter");
                    Self::build_openrouter(model, or_key)
                } else {
                    info!("LLM provider auto-selected: openai");
                    Self::new(model, std::env::var("OPENAI_API_KEY").ok())
                }
            }
            other => {
                warn!(
                    provider = %other,
                    "unknown LLM provider; falling back to OpenAI endpoint"
                );
                Self::new(model, std::env::var("OPENAI_API_KEY").ok())
            }
        }
    }

    /// Internal helper: build an OpenRouter-configured classifier with
    /// attribution headers set.
    fn build_openrouter(model: &str, api_key: Option<String>) -> Self {
        let key = api_key.or_else(|| std::env::var("OPENROUTER_API_KEY").ok());
        let mut headers = HeaderMap::new();
        // These are static, valid ASCII strings — `from_static` cannot panic
        // on them at runtime.
        headers.insert("HTTP-Referer", HeaderValue::from_static(OPENROUTER_REFERER));
        headers.insert("X-Title", HeaderValue::from_static(OPENROUTER_TITLE));
        Self {
            client: Client::new(),
            model: model.to_string(),
            api_key: key,
            endpoint: OPENROUTER_ENDPOINT.to_string(),
            extra_headers: headers,
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
            .headers(self.extra_headers.clone())
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

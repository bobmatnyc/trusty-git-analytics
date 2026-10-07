//! #178: every prompt a classify path sends stays inside the LLM input
//! budget, whatever the size of the commit message or its context block.
//! Mock providers; no network. Every message, path and title is synthetic.

use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::classify::tiers::llm::{LlmClassifier, SYSTEM_PROMPT};
use crate::classify::tiers::llm_context::{CommitContext, CLOSE, OPEN};
use crate::core::config::LlmSource;

/// The OpenAI-compatible default input budget, in estimated tokens (one
/// byte = one token).
const BUDGET_TOKENS: usize = 100_000;
/// #178: the Bedrock and Anthropic API default (200k-token Claude windows).
const CLAUDE_BUDGET_TOKENS: usize = 190_000;
/// The fixed text every provider puts before the message.
const PREFIX: &str = "Classify this commit message:\n\n";
/// The marker that opens the truncation note.
const MARKER: &str = "\n[truncated ";

/// About 1 MiB of message text. The unit mixes 1-, 2- and 3-byte characters,
/// so a byte-count cut lands inside a character at most positions.
fn huge(unit: &str, bytes: usize) -> String {
    unit.repeat(bytes / unit.len() + 1)
}

fn huge_message() -> String {
    huge(
        "fix: résumé parser drops €42 rows — see diff below\n",
        1 << 20,
    )
}

fn openai_reply() -> ResponseTemplate {
    let content =
        "{\"category\":\"bugfix\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9}
    }))
}

fn anthropic_reply() -> ResponseTemplate {
    let text = "{\"category\":\"bugfix\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 120, "output_tokens": 9}
    }))
}

async fn server(route: &str, template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(route))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

/// The one request the mock received, as `(system, user)` text.
async fn sent(server: &MockServer) -> (String, String) {
    let requests = server.received_requests().await.expect("recording on");
    assert_eq!(requests.len(), 1, "one call per commit");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("json body");
    let messages = body["messages"].as_array().expect("messages");
    let text = |role: &str| {
        messages
            .iter()
            .find(|m| m["role"] == role)
            .and_then(|m| m["content"].as_str())
            .map(str::to_string)
    };
    // OpenAI-compatible: a `system` message; Anthropic: a top-level field.
    let system = text("system")
        .or_else(|| body["system"].as_str().map(str::to_string))
        .expect("system prompt");
    (system, text("user").expect("user message"))
}

/// Assert the prompt fits `budget` and that `sent_text` is `original` cut
/// at a character boundary and followed by an exact `[truncated N bytes]`
/// note, where N is the byte count left out.
fn assert_truncated(budget: usize, system: &str, user: &str, sent_text: &str, original: &str) {
    let total = system.len() + user.len();
    assert!(
        total <= budget,
        "prompt is {total} bytes, over the {budget}-token budget"
    );
    let at = sent_text
        .find(MARKER)
        .unwrap_or_else(|| panic!("no truncation marker in {} bytes", sent_text.len()));
    let kept = &sent_text[..at];
    let note = &sent_text[at + MARKER.len()..];
    let dropped: usize = note
        .split_once(" bytes]")
        .and_then(|(n, _)| n.parse().ok())
        .unwrap_or_else(|| panic!("marker is not `[truncated N bytes]`: {note:.40}"));
    assert!(original.starts_with(kept), "the kept text is a prefix");
    assert_eq!(
        kept.len() + dropped,
        original.len(),
        "N counts the cut bytes"
    );
    assert!(!kept.is_empty(), "the message head is kept");
}

/// Why (#178): a commit message of hundreds of kilobytes made Bedrock reject
/// every classify call as over the model's context limit.
/// What: a 1 MiB message through the OpenAI-compatible path is sent cut to
/// the budget, with the marker and the exact count of bytes left out.
/// Test: this test.
#[tokio::test]
async fn oversized_message_fits_the_budget_on_openai_compat() {
    let server = server("/v1/chat/completions", openai_reply()).await;
    let llm = LlmClassifier::new("test-model", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let message = huge_message();
    let call = llm.classify_detailed(&message).await;
    assert!(call.verdict.is_some(), "the cut prompt still classifies");
    let (system, user) = sent(&server).await;
    let body = user.strip_prefix(PREFIX).expect("fixed prefix");
    assert_truncated(BUDGET_TOKENS, &system, &user, body, &message);
}

/// Why (#178): the cap must hold for the Anthropic Messages API too, and a
/// cut message must not cost the commit its `llm.context` block.
/// What: a 1 MiB message with paths, a PR title and an issue type is cut
/// before the block; the block is sent whole after the marker.
/// Test: this test.
#[tokio::test]
async fn oversized_message_keeps_the_context_block_on_anthropic() {
    let server = server("/v1/messages", anthropic_reply()).await;
    let llm = LlmClassifier::build_anthropic("claude-test", Some("sk-ant-test".to_string())) // pragma: allowlist secret
        .with_endpoint(format!("{}/v1/messages", server.uri()));
    let paths = vec!["src/ledger/retry.rs".to_string(), "docs/a.md".to_string()];
    let ctx = CommitContext::new(&paths, 30, 2048, Some("Retry ledger writes"), Some("Bug"));
    let message = huge_message();
    llm.classify_detailed_with_context(&message, Some(&ctx))
        .await;
    let (system, user) = sent(&server).await;
    let block = format!(
        "{OPEN}Changed paths:\n- src/ledger/retry.rs\n- docs/a.md\n\
         PR title: Retry ledger writes\nIssue type: Bug\n{CLOSE}"
    );
    let body = user.strip_prefix(PREFIX).expect("fixed prefix");
    let head = body.strip_suffix(block.as_str()).expect("block sent whole");
    // #178: the Anthropic API default is the 190k Claude budget.
    assert_truncated(CLAUDE_BUDGET_TOKENS, &system, &user, head, &message);
}

/// Why (#178): the context block is part of the prompt, so an oversized PR
/// title must be cut as well.
/// What: a short message with a 600 KB PR title is sent under the budget,
/// with the message whole and the marker in the block.
/// Test: this test.
#[tokio::test]
async fn oversized_context_block_fits_the_budget() {
    let server = server("/v1/chat/completions", openai_reply()).await;
    let llm = LlmClassifier::new("test-model", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let title = huge("Generated ⚙ title ", 600_000);
    let ctx = CommitContext::new(&[], 30, 2048, Some(&title), None);
    llm.classify_detailed_with_context("fix: a", Some(&ctx))
        .await;
    let (system, user) = sent(&server).await;
    let block = user
        .strip_prefix(&format!("{PREFIX}fix: a"))
        .expect("message sent whole");
    let full = ctx.render_plain();
    assert_truncated(BUDGET_TOKENS, &system, &user, block, &full);
}

/// Why (#178): a 150 KB prompt fits a 200k-token Claude window, and 10.3.2
/// sent it whole on Bedrock and the Anthropic API; only a 128k-class
/// OpenAI-compatible model needs it cut by default.
/// What: with no `max_input_tokens`, a 150 KB message is sent byte for byte
/// by the Anthropic API path and in the Bedrock Converse request, and cut,
/// with the marker, by the OpenAI-compatible path.
/// Test: this test.
#[tokio::test]
async fn claude_sources_send_150_kb_whole_and_openai_compat_cuts_it() {
    let message = huge("feat: generated ledger fixture ", 150_000);
    let expected = format!("{PREFIX}{message}");

    let anthropic = server("/v1/messages", anthropic_reply()).await;
    let llm = LlmClassifier::build_anthropic("claude-test", Some("sk-ant-test".to_string())) // pragma: allowlist secret
        .with_endpoint(format!("{}/v1/messages", anthropic.uri()));
    llm.classify_detailed(&message).await;
    assert_eq!(sent(&anthropic).await.1, expected, "anthropic-api");

    let bedrock_budget =
        crate::classify::tiers::llm_budget::PromptBudget::for_source(&LlmSource::Bedrock);
    let text = bedrock_budget
        .fit(SYSTEM_PROMPT, &message, None)
        .expect("room");
    let req = crate::classify::tiers::bedrock::converse_request("m", SYSTEM_PROMPT, &text);
    let json = serde_json::to_value(&req).expect("serialize");
    let user = json["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .find(|m| m["role"] == "user")
        .expect("user message");
    assert_eq!(user["content"], Value::from(expected), "bedrock");

    let openai = server("/v1/chat/completions", openai_reply()).await;
    let llm = LlmClassifier::new("test-model", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", openai.uri()));
    llm.classify_detailed(&message).await;
    let (system, user) = sent(&openai).await;
    let body = user.strip_prefix(PREFIX).expect("fixed prefix");
    assert_truncated(BUDGET_TOKENS, &system, &user, body, &message);
}

/// Why (#178): a prompt inside the budget must be sent byte for byte as
/// 10.3.2 sent it.
/// What: a 60 KB message with a context block is sent unchanged.
/// Test: this test.
#[tokio::test]
async fn prompt_inside_the_budget_is_byte_identical() {
    let server = server("/v1/chat/completions", openai_reply()).await;
    let llm = LlmClassifier::new("test-model", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let message = huge("refactor: split the ledger ", 60_000);
    let ctx = CommitContext::new(&["src/a.rs".to_string()], 30, 2048, None, Some("Story"));
    llm.classify_detailed_with_context(&message, Some(&ctx))
        .await;
    let (_, user) = sent(&server).await;
    assert_eq!(
        user,
        format!("{PREFIX}{message}{OPEN}Changed paths:\n- src/a.rs\nIssue type: Story\n{CLOSE}")
    );
}

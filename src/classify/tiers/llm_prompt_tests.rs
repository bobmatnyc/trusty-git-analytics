//! #131 / #111: prompt restriction, fail-closed reply validation, and token
//! usage recorded from mocked provider replies. No network.

use crate::classify::rules::CategoryDef;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::llm_prompt::{
    resolve, restricted_system_prompt, LlmOutcome, LlmUsage, ABSTAIN_LABEL,
};
use crate::core::config::LlmEffort;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn allowed() -> Vec<CategoryDef> {
    vec![
        CategoryDef {
            name: "bug_fix".into(),
            description: Some("Corrects wrong\n  behaviour.".into()),
        },
        CategoryDef {
            name: "enablement".into(),
            description: None,
        },
    ]
}

fn verdict_json(category: &str) -> String {
    format!(
        "{{\"category\":\"{category}\",\"subcategory\":\"x\",\"confidence\":0.8,\"complexity\":2}}"
    )
}

const USAGE: LlmUsage = LlmUsage {
    input_tokens: 120,
    output_tokens: 9,
};

/// Why (#131): the LLM must be offered only the configured categories plus
/// the abstain label, never the built-in list.
/// What: builds the restricted prompt and checks each line.
/// Test: this test.
#[test]
fn restricted_prompt_lists_only_configured_categories() {
    let p = restricted_system_prompt(&allowed());
    assert!(p.contains("- bug_fix: Corrects wrong behaviour.\n"), "{p}");
    assert!(p.contains("- enablement\n"), "{p}");
    assert!(p.contains(&format!("- {ABSTAIN_LABEL}: ")), "{p}");
    assert!(p.contains("<one of: bug_fix|enablement|unclear>"), "{p}");
    let offered: Vec<&str> = p.lines().filter(|l| l.starts_with("- ")).collect();
    assert_eq!(offered.len(), 3, "exactly two categories plus abstain: {p}");
    for builtin in ["feature|", "bugfix|", "chore", "documentation", "refactor|"] {
        assert!(!p.contains(builtin), "built-in `{builtin}` leaked: {p}");
    }
}

/// Why (#131): fail-closed — a category outside the configured set is an
/// abstention and must never be stored as given.
/// What: resolves a `chore` reply against the configured set.
/// Test: this test.
#[test]
fn out_of_set_reply_is_an_abstention() {
    let cats = allowed();
    let call = resolve(Some(&verdict_json("chore")), Some(&cats), Some(USAGE));
    assert_eq!(call.outcome, LlmOutcome::OutOfSet);
    assert!(call.verdict.is_none());
    assert_eq!(call.usage, Some(USAGE), "tokens are billed either way");
}

/// Why (#131): the abstain label is an abstention, whatever its case; a
/// configured name is kept under its configured spelling, without the
/// model's invented subcategory.
/// What: resolves `UNCLEAR` and `Bug_Fix` replies.
/// Test: this test.
#[test]
fn abstain_label_is_an_abstention() {
    let cats = allowed();
    let call = resolve(Some(&verdict_json("UNCLEAR")), Some(&cats), None);
    assert_eq!(call.outcome, LlmOutcome::Abstained);
    assert!(call.verdict.is_none());

    let call = resolve(Some(&verdict_json("Bug_Fix")), Some(&cats), None);
    assert_eq!(call.outcome, LlmOutcome::Answered);
    let v = call.verdict.expect("verdict");
    assert_eq!(v.category, "bug_fix");
    assert_eq!(v.subcategory, None);
}

/// Why: configs without `extend_defaults: false` keep the pre-#131
/// behaviour, and a fenced reply still parses.
/// What: resolves an unrestricted fenced reply and an unparseable one.
/// Test: this test.
#[test]
fn unrestricted_reply_is_kept() {
    let fenced = format!("```json\n{}\n```", verdict_json("chore"));
    let call = resolve(Some(&fenced), None, None);
    assert_eq!(call.outcome, LlmOutcome::Answered);
    let v = call.verdict.expect("verdict");
    assert_eq!(v.category, "chore");
    assert_eq!(v.subcategory.as_deref(), Some("x"));

    let call = resolve(Some("not json"), None, Some(USAGE));
    assert_eq!(call.outcome, LlmOutcome::Failed);
    assert_eq!(call.usage, Some(USAGE));
}

async fn anthropic_server(category: &str) -> MockServer {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": verdict_json(category)}
        ],
        "stop_reason": "end_turn",
        "usage": {"input_tokens": 120, "output_tokens": 9}
    });
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

fn anthropic_llm(server: &MockServer) -> LlmClassifier {
    LlmClassifier::build_anthropic("claude-sonnet-5", Some("sk-ant-test".to_string())) // pragma: allowlist secret
        .with_endpoint(format!("{}/v1/messages", server.uri()))
}

/// Request body the mock server received first.
async fn sent_body(server: &MockServer) -> serde_json::Value {
    let reqs = server.received_requests().await.expect("recording on");
    serde_json::from_slice(&reqs[0].body).expect("json body")
}

/// Why (#131): the restricted prompt must actually be sent, and an
/// out-of-set reply dropped end to end.
/// What: a classifier restricted to two categories calls a mock Anthropic
/// endpoint that answers `chore`.
/// Test: this test.
#[tokio::test]
async fn classifier_sends_restricted_prompt_and_drops_out_of_set() {
    let server = anthropic_server("chore").await;
    let llm = anthropic_llm(&server).with_allowed_categories(allowed());
    let call = llm.classify_detailed("tidy up").await;
    assert_eq!(call.outcome, LlmOutcome::OutOfSet);
    assert!(call.verdict.is_none());

    let body = sent_body(&server).await;
    let system = body["system"].as_str().expect("system");
    assert!(system.contains("- enablement"), "{system}");
    assert!(!system.contains("chore"), "built-in list sent: {system}");
    assert!(body.get("output_config").is_none(), "no effort unless set");
}

/// Why (#111): every call's tokens come from the provider reply's `usage`.
/// What: mock Anthropic reply with `usage`; also checks `effort` is sent.
/// Test: this test.
#[tokio::test]
async fn anthropic_usage_is_recorded() {
    let server = anthropic_server("feature").await;
    let llm = anthropic_llm(&server).with_effort(Some(LlmEffort::Low));
    let call = llm.classify_detailed("feat: add login").await;
    assert_eq!(call.outcome, LlmOutcome::Answered);
    assert_eq!(call.usage, Some(USAGE));
    assert_eq!(sent_body(&server).await["output_config"]["effort"], "low");
    assert_eq!(llm.provider_label(), "anthropic-api");
    assert_eq!(llm.model(), "claude-sonnet-5");
}

/// Why (#111): the OpenAI-compatible path reports `prompt_tokens` /
/// `completion_tokens`.
/// What: mock chat-completions reply with `usage`.
/// Test: this test.
#[tokio::test]
async fn openai_usage_is_recorded() {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "choices": [{"message": {"content": verdict_json("feature")}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9, "total_tokens": 129}
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let llm = LlmClassifier::new("gpt-4o-mini", Some("sk-test".to_string()))
        .with_endpoint(format!("{}/v1/chat/completions", server.uri()));
    let call = llm.classify_detailed("feat: add login").await;
    assert_eq!(call.outcome, LlmOutcome::Answered);
    assert_eq!(call.usage, Some(USAGE));
}

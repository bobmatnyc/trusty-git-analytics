//! #111: `llm.context` appends changed paths, the PR title and the issue
//! type to the LLM user prompt. In-memory DB and mock providers; no network.
//! Every path, title and name here is synthetic.

use std::io::Write;

use rusqlite::params;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::core::config::{ClassificationConfig, Config, LlmConfig};
use crate::core::db::Database;

const RULES: &str = "extend_defaults: false
rules:
  - id: infra-weak
    category: platform
    keywords: [\"infra:\"]
    confidence: 0.5
categories:
  - name: enablement
    description: Tooling that makes other teams faster.
";

/// A message no rule answers, so it reaches the LLM.
const MESSAGE: &str = "zzz qqq vvv www yyy uuu";
/// The user prompt every provider sent before #111 added `llm.context`.
const BARE_PROMPT: &str = "Classify this commit message:\n\nzzz qqq vvv www yyy uuu";
/// The changed paths stored for the commit, in path order.
const PATHS: [&str; 3] = [
    "docs/guide/setup.md",
    "src/ledger/retry.rs",
    "tests/retry_tests.rs",
];
const OPEN: &str = "\n\n--- commit context (from the repository, not the commit message) ---\n";
const CLOSE: &str = "--- end commit context ---";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

/// The `llm:` section parsed from YAML, as `Config::load` reads it.
fn llm_section(yaml: &str) -> LlmConfig {
    serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("llm section {yaml:?}: {e}"))
}

fn config(rules: &std::path::Path, llm: LlmConfig) -> Config {
    Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            weighted_sum: WeightedSumConfig {
                enabled: false,
                ..WeightedSumConfig::default()
            },
            ..ClassificationConfig::default()
        }),
        llm: Some(llm),
        ..Config::default()
    }
}

/// One unanswered commit with three changed paths, a PR and a Bug issue.
fn seed(db: &Database) {
    let conn = db.connection();
    conn.execute(
        "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
         VALUES ('sha-ctx', 'a', 'a@x', '2024-01-01T00:00:00Z', ?1, 'acme/widgets')",
        params![MESSAGE],
    )
    .expect("insert commit");
    let id: i64 = conn
        .query_row("SELECT id FROM commits WHERE sha = 'sha-ctx'", [], |r| {
            r.get(0)
        })
        .expect("commit id");
    // Inserted out of order: the prompt lists them in path order.
    for p in [PATHS[2], PATHS[0], PATHS[1]] {
        conn.execute(
            "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'M')",
            params![id, p],
        )
        .expect("insert file");
    }
    conn.execute(
        "INSERT INTO pull_requests (pr_number, title, author, state, created_at, commit_shas) \
         VALUES (7, 'Retry ledger writes', 'a', 'merged', '2024-01-02', '[\"sha-ctx\"]')",
        [],
    )
    .expect("insert pr");
    conn.execute(
        "INSERT INTO work_items (id, source, title, status, item_type) \
         VALUES ('W-1', 'jira', 'Writes drop', 'Done', 'Bug')",
        [],
    )
    .expect("insert work item");
    conn.execute(
        "INSERT INTO commit_work_items (commit_sha, work_item_id, work_item_source) \
         VALUES ('sha-ctx', 'W-1', 'jira')",
        [],
    )
    .expect("link work item");
}

fn openai_reply() -> ResponseTemplate {
    let content =
        "{\"category\":\"enablement\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9}
    }))
}

fn anthropic_reply() -> ResponseTemplate {
    let text =
        "{\"category\":\"enablement\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
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

/// The `user` message text of every request the mock received.
async fn user_prompts(server: &MockServer) -> Vec<String> {
    let requests = server.received_requests().await.expect("recording on");
    requests
        .iter()
        .map(|r| {
            let body: Value = serde_json::from_slice(&r.body).expect("json body");
            let messages = body["messages"].as_array().expect("messages");
            let user = messages
                .iter()
                .find(|m| m["role"] == "user")
                .expect("user message");
            user["content"].as_str().expect("text content").to_string()
        })
        .collect()
}

/// Run the pipeline over [`seed`] with `llm` as the `llm:` section and the
/// provider `make` builds at `server`; return the user prompts it sent.
async fn prompts_for(
    llm: LlmConfig,
    server: &MockServer,
    make: impl FnOnce(&MockServer) -> LlmClassifier,
) -> Vec<String> {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), llm));
    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    let cats = pipeline
        .llm_categories()
        .expect("categories")
        .expect("custom-only");
    engine.attach_llm(make(server).with_allowed_categories(cats));
    let mut db = Database::open_in_memory().expect("db");
    seed(&db);
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");
    user_prompts(server).await
}

async fn openai_prompts(llm_yaml: &str) -> Vec<String> {
    let server = server("/v1/chat/completions", openai_reply()).await;
    prompts_for(llm_section(llm_yaml), &server, |s| {
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", s.uri()))
    })
    .await
}

fn block(body: &str) -> String {
    format!("{BARE_PROMPT}{OPEN}{body}{CLOSE}")
}

/// Why (#111): the second rater, who saw the changed paths, scored well
/// above the message-only LLM; `context: [paths]` gives the LLM the same.
/// What: an OpenAI-compatible run with `context: [paths]` sends the message
/// followed by the delimited block listing every stored path in path
/// order, and nothing it did not ask for (no PR title, no issue type).
/// Test: this test.
#[tokio::test]
async fn context_paths_appends_a_paths_block() {
    let prompts = openai_prompts("source: openrouter\ncontext: [paths]\n").await;
    let expected = block(
        "Changed paths:\n- docs/guide/setup.md\n- src/ledger/retry.rs\n- tests/retry_tests.rs\n",
    );
    assert_eq!(prompts, [expected]);
}

/// Why (#111): `llm.context` defaults to empty so an existing config sends
/// exactly what it sent before, even when the database holds paths, a PR
/// and an issue for the commit.
/// What: the default section and an explicit `context: []` both send the
/// pre-#111 prompt byte for byte.
/// Test: this test.
#[tokio::test]
async fn default_context_prompt_is_byte_identical() {
    for yaml in ["source: openrouter\n", "source: openrouter\ncontext: []\n"] {
        let prompts = openai_prompts(yaml).await;
        assert_eq!(prompts, [BARE_PROMPT], "{yaml:?}");
    }
}

/// Why (#111): a commit can touch thousands of files; the paths block must
/// stay small, and the cut must be visible to the model.
/// What: `context_max_paths: 2` keeps the first two of three paths;
/// `context_max_path_bytes: 25` keeps only the first (19 bytes; a second
/// would make 38). Each header reports `kept of total`.
/// Test: this test.
#[tokio::test]
async fn context_caps_truncate_the_paths() {
    let by_count =
        openai_prompts("source: openrouter\ncontext: [paths]\ncontext_max_paths: 2\n").await;
    assert_eq!(
        by_count,
        [block(
            "Changed paths (2 of 3 shown):\n- docs/guide/setup.md\n- src/ledger/retry.rs\n"
        )]
    );
    let by_bytes =
        openai_prompts("source: openrouter\ncontext: [paths]\ncontext_max_path_bytes: 25\n").await;
    assert_eq!(
        by_bytes,
        [block(
            "Changed paths (1 of 3 shown):\n- docs/guide/setup.md\n"
        )]
    );
}

/// Why (#111): every provider must honour `llm.context`, not only the
/// OpenAI-compatible path.
/// What: an Anthropic Messages API run with all three items sends the
/// paths, the stored PR title and the linked issue type.
/// Test: this test.
#[tokio::test]
async fn anthropic_prompt_carries_every_context_item() {
    let server = server("/v1/messages", anthropic_reply()).await;
    let llm = llm_section("source: anthropic-api\ncontext: [paths, pr_title, issue_type]\n");
    let prompts = prompts_for(llm, &server, |s| {
        LlmClassifier::build_anthropic("claude-test", Some("sk-ant-test".to_string())) // pragma: allowlist secret
            .with_endpoint(format!("{}/v1/messages", s.uri()))
    })
    .await;
    let expected = block(
        "Changed paths:\n- docs/guide/setup.md\n- src/ledger/retry.rs\n- tests/retry_tests.rs\n\
         PR title: Retry ledger writes\nIssue type: Bug\n",
    );
    assert_eq!(prompts, [expected]);
}

/// Why (#111): a commit with no stored PR or issue must not get an empty
/// heading; the block lists only facts the database holds.
/// What: `context: [pr_title, issue_type]` over a commit with neither
/// stored sends the pre-#111 prompt.
/// Test: this test.
#[tokio::test]
async fn missing_context_facts_add_nothing() {
    let server = server("/v1/chat/completions", openai_reply()).await;
    let rules = rules_file();
    let llm = llm_section("source: openrouter\ncontext: [pr_title, issue_type]\n");
    let pipeline = ClassificationPipeline::new(config(rules.path(), llm));
    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    let cats = pipeline.llm_categories().expect("cats").expect("custom");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()))
            .with_allowed_categories(cats),
    );
    let mut db = Database::open_in_memory().expect("db");
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES ('sha-bare', 'a', 'a@x', '2024-01-01T00:00:00Z', ?1, 'acme/widgets')",
            params![MESSAGE],
        )
        .expect("insert");
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");
    assert_eq!(user_prompts(&server).await, [BARE_PROMPT]);
}

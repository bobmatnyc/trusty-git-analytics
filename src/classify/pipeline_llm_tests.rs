//! #111 / #131: LLM routing scope, token accounting, category set, and the
//! `--shas` subset, against an in-memory DB and a mock provider. No network.

use std::io::Write;

use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::core::config::{ClassificationConfig, Config, LlmFallbackScope};
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

/// A rules file on disk; the handle keeps it alive.
fn rules_file(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(yaml.as_bytes()).expect("write rules");
    f
}

fn config(rules: &std::path::Path, scope: LlmFallbackScope) -> Config {
    Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            llm_fallback_scope: scope,
            // Keep the signal-voting tier out so an unmatched message stays
            // unanswered.
            weighted_sum: WeightedSumConfig {
                enabled: false,
                ..WeightedSumConfig::default()
            },
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    }
}

fn insert_commit(db: &Database, sha: &str, message: &str) {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'acme/widgets')",
            params![sha, message],
        )
        .expect("insert commit");
}

fn category_of(db: &Database, sha: &str) -> Option<String> {
    db.connection()
        .query_row(
            "SELECT cl.category FROM commits c \
             LEFT JOIN classifications cl ON cl.id = c.classification_id WHERE c.sha = ?1",
            [sha],
            |r| r.get(0),
        )
        .expect("query category")
}

/// Mock OpenAI-compatible endpoint answering `enablement` with usage.
async fn mock_llm() -> MockServer {
    let server = MockServer::start().await;
    let body = serde_json::json!({
        "choices": [{"message": {"content":
            "{\"category\":\"enablement\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}"}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9}
    });
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// Engine exactly as `build_engine` wires it, with the LLM at `server`.
fn engine(pipeline: &ClassificationPipeline, server: &MockServer) -> ClassificationEngine {
    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    let cats = pipeline
        .llm_categories()
        .expect("categories")
        .expect("custom-only");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()))
            .with_allowed_categories(cats),
    );
    engine
}

/// Seed one weak rule hit and one commit no rule answers; run the pipeline.
async fn run_scope(scope: LlmFallbackScope) -> (Database, MockServer, ClassificationStats) {
    let rules = rules_file(RULES);
    let pipeline = ClassificationPipeline::new(config(rules.path(), scope));
    let server = mock_llm().await;
    let mut db = Database::open_in_memory().expect("db");
    insert_commit(&db, "sha-weak", "infra: bump the cluster size");
    insert_commit(&db, "sha-none", "zzz qqq vvv www yyy uuu");
    let stats = pipeline
        .run_with_engine(&mut db, engine(&pipeline, &server))
        .await
        .expect("run");
    (db, server, stats)
}

/// Why (#111): with `llm_fallback_scope: unanswered` only the commits the
/// rules left uncategorized reach the LLM; a weak rule hit keeps its answer.
/// Each call's tokens land in `llm_usage` and the run totals.
/// What: one weak hit (0.5, below the 0.65 threshold) and one miss; the mock
/// must see exactly one request, for the miss.
/// Test: this test.
#[tokio::test]
async fn unanswered_scope_sends_only_abstentions() {
    let (db, server, stats) = run_scope(LlmFallbackScope::Unanswered).await;
    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(category_of(&db, "sha-weak").as_deref(), Some("platform"));
    assert_eq!(category_of(&db, "sha-none").as_deref(), Some("enablement"));

    assert_eq!(stats.llm_usage.calls, 1);
    assert_eq!(stats.llm_usage.answered, 1);
    assert_eq!(stats.llm_usage.input_tokens, 120);
    assert_eq!(stats.llm_usage.output_tokens, 9);
    let row: (String, String, String, i64, i64) = db
        .connection()
        .query_row(
            "SELECT commit_sha, provider, outcome, input_tokens, output_tokens FROM llm_usage",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("exactly one usage row");
    assert_eq!(
        row,
        (
            "sha-none".into(),
            "openai-compatible".into(),
            "answered".into(),
            120,
            9
        )
    );
}

/// Why: documents why the scope key exists — the default threshold routing
/// also sends a weak rule hit, and the LLM overwrites it.
/// What: same fixture under the default scope; two requests.
/// Test: this test.
#[tokio::test]
async fn low_confidence_scope_also_sends_weak_rule_hits() {
    let (db, server, stats) = run_scope(LlmFallbackScope::LowConfidence).await;
    assert_eq!(server.received_requests().await.expect("rec").len(), 2);
    assert_eq!(category_of(&db, "sha-weak").as_deref(), Some("enablement"));
    assert_eq!(stats.llm_usage.calls, 2);
}

/// Why (#131): only a custom-only rules file restricts the LLM's categories;
/// descriptions come from the `categories:` section.
/// What: `extend_defaults: false` → rule categories then declared ones;
/// `extend_defaults: true` or no rules file → `None`.
/// Test: this test.
#[test]
fn llm_categories_follow_extend_defaults() {
    let rules = rules_file(RULES);
    let p = ClassificationPipeline::new(config(rules.path(), LlmFallbackScope::default()));
    let cats = p.llm_categories().expect("load").expect("restricted");
    let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["platform", "enablement"]);
    assert_eq!(
        cats[1].description.as_deref(),
        Some("Tooling that makes other teams faster.")
    );
    assert_eq!(
        p.rule_categories().expect("rule cats"),
        ["platform", "enablement"]
    );

    let extending = rules_file(&RULES.replace("extend_defaults: false", "extend_defaults: true"));
    let p = ClassificationPipeline::new(config(extending.path(), LlmFallbackScope::default()));
    assert!(p.llm_categories().expect("load").is_none());
    assert!(ClassificationPipeline::new(Config::default())
        .llm_categories()
        .expect("builtin")
        .is_none());
}

fn subset_pipeline(rules: &std::path::Path, shas: &[&str]) -> ClassificationPipeline {
    ClassificationPipeline::new(config(rules, LlmFallbackScope::default()))
        .with_force(true)
        .with_shas(Some(shas.iter().map(|s| s.to_string()).collect()))
}

/// Why (#111): the eval classifies its sample only; every other commit keeps
/// its state.
/// What: two commits, `--shas` names one.
/// Test: this test.
#[tokio::test]
async fn shas_subset_classifies_only_listed_commits() {
    let rules = rules_file(RULES);
    let mut db = Database::open_in_memory().expect("db");
    insert_commit(&db, "sha-a", "infra: bump the cluster size");
    insert_commit(&db, "sha-b", "infra: rotate certificates");
    let p = subset_pipeline(rules.path(), &["sha-a"]);
    let stats = p
        .run_with_engine(&mut db, p.build_rule_engine().expect("engine"))
        .await
        .expect("run");
    assert_eq!(stats.total_commits, 1);
    assert_eq!(category_of(&db, "sha-a").as_deref(), Some("platform"));
    assert_eq!(category_of(&db, "sha-b"), None);
}

/// Why (#111): fail-closed — an unknown SHA or an empty list stops the run
/// before any write, rather than classifying fewer (or all) commits.
/// What: `--shas` naming a missing SHA, then an empty list; no
/// `classifications` row may exist afterwards.
/// Test: this test.
#[tokio::test]
async fn unknown_sha_fails_before_any_write() {
    let rules = rules_file(RULES);
    let mut db = Database::open_in_memory().expect("db");
    insert_commit(&db, "sha-a", "infra: bump the cluster size");
    for shas in [&["sha-a", "sha-missing"][..], &[][..]] {
        let p = subset_pipeline(rules.path(), shas);
        let err = p
            .run_with_engine(&mut db, p.build_rule_engine().expect("engine"))
            .await
            .expect_err("must refuse");
        let msg = err.to_string();
        assert!(
            msg.contains("sha-missing") || msg.contains("empty"),
            "{msg}"
        );
    }
    let written: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
        .expect("count");
    assert_eq!(written, 0);
}

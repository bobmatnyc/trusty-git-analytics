//! #158: `classification.repo_categories`, the repo → category hard override,
//! end to end against an in-memory DB and a mock LLM. No network.

use std::collections::HashMap;
use std::io::Write;

use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::core::config::{ClassificationConfig, Config, LlmFallbackScope};
use crate::core::db::Database;

/// A security rule and a bug-fix rule that both match `fix: security ...`,
/// plus the two categories the repo map names.
const RULES: &str = "extend_defaults: false
rules:
  - id: sec
    category: security
    keywords: [\"security\"]
    priority: 120
    confidence: 0.95
  - id: fix
    category: bug_fix
    keywords: [\"fix:\"]
    priority: 100
    confidence: 0.9
categories:
  - name: internal_tooling
  - name: qa
";

const MAPPED: &str = "mcp-services";
const UNMAPPED: &str = "billing-api";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

fn config(rules: &std::path::Path, map: &[(&str, &str)]) -> Config {
    Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            repo_categories: map
                .iter()
                .map(|(r, c)| (r.to_string(), c.to_string()))
                .collect::<HashMap<_, _>>(),
            llm_fallback_scope: LlmFallbackScope::LowConfidence,
            // Keep the signal-voting tier out so an unmatched message stays
            // unanswered and is LLM-eligible.
            weighted_sum: WeightedSumConfig {
                enabled: false,
                ..WeightedSumConfig::default()
            },
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    }
}

fn owner_map() -> Vec<(&'static str, &'static str)> {
    vec![
        (MAPPED, "internal_tooling"),
        ("duetto-playwright-e2e", "qa"),
    ]
}

fn insert(db: &Database, sha: &str, repo: &str, message: &str, is_merge: bool) {
    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, is_merge) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, ?3, ?4)",
            params![sha, message, repo, i64::from(is_merge)],
        )
        .expect("insert commit");
}

/// `(category, method, confidence)` stored for `sha`.
fn verdict(db: &Database, sha: &str) -> (String, String, f64) {
    db.connection()
        .query_row(
            "SELECT cl.category, cl.method, cl.confidence FROM commits c \
             JOIN classifications cl ON cl.id = c.classification_id WHERE c.sha = ?1",
            [sha],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("classified")
}

async fn run(pipeline: &ClassificationPipeline, db: &mut Database) -> Result<ClassificationStats> {
    let engine = pipeline.build_rule_engine()?;
    pipeline.run_with_engine(db, engine).await
}

/// A mock chat-completions endpoint answering every call.
async fn mock_llm() -> MockServer {
    let content =
        "{\"category\":\"bug_fix\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": content}}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 9}
        })))
        .mount(&server)
        .await;
    server
}

/// Why (#158 criteria 3 and 7): a commit in a mapped repo gets the mapped
/// category, and its method records that the repo map decided it, so
/// reports and `tga eval` can tell it apart. The repository name matches
/// whole and case-sensitively.
/// What: one unanswered commit per repo — the mapped repo, the same name in
/// another case, a longer name sharing the prefix, and an unmapped repo.
/// Only the first is mapped; `by_method` counts it under `repo_map`.
/// Test: this test.
#[tokio::test]
async fn mapped_repo_gets_the_mapped_category_and_the_repo_map_method() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &owner_map()));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-mapped", MAPPED, "zzz qqq vvv", false);
    insert(&db, "sha-case", "MCP-Services", "zzz qqq vvv", false);
    insert(&db, "sha-prefix", "mcp-services-v2", "zzz qqq vvv", false);
    insert(&db, "sha-unmapped", UNMAPPED, "zzz qqq vvv", false);

    let stats = run(&pipeline, &mut db).await.expect("run");

    let (category, method, confidence) = verdict(&db, "sha-mapped");
    assert_eq!(
        (category.as_str(), method.as_str()),
        ("internal_tooling", "repo_map")
    );
    assert!((confidence - 1.0).abs() < 1e-9, "{confidence}");
    for sha in ["sha-case", "sha-prefix", "sha-unmapped"] {
        let (category, method, _) = verdict(&db, sha);
        assert_ne!(method, "repo_map", "{sha} must not match the map");
        assert_eq!(category, "uncategorized", "{sha}");
    }
    assert_eq!(stats.by_method.get("repo_map"), Some(&1));
}

/// Why (#158 criterion 4): a mapped commit never reaches the LLM — no
/// request and no token spend.
/// What: the LLM is on and `low_confidence` sends every unanswered commit.
/// Two unanswered commits, one mapped; the mock must see one request and
/// `llm_usage` must name only the unmapped commit.
/// Test: this test.
#[tokio::test]
async fn mapped_repo_commits_never_reach_the_llm() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &owner_map()));
    let server = mock_llm().await;
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-mapped", MAPPED, "zzz qqq vvv", false);
    insert(&db, "sha-unmapped", UNMAPPED, "zzz qqq vvv", false);

    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri())),
    );
    let stats = pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");

    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(stats.llm_usage.calls, 1);
    let used: Vec<String> = db
        .connection()
        .prepare("SELECT commit_sha FROM llm_usage")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows");
    assert_eq!(used, ["sha-unmapped"]);
    assert_eq!(verdict(&db, "sha-mapped").1, "repo_map");
}

/// Why (#158 criteria 5 and 6): the map outranks a rule that picks another
/// category, a security fix included; an unmapped repo keeps the rule.
/// What: the same `fix: security ...` message in a mapped and an unmapped
/// repo. The unmapped one gets the security rule; the mapped one gets `qa`.
/// Test: this test.
#[tokio::test]
async fn repo_map_outranks_a_security_or_bug_fix_rule() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &owner_map()));
    let mut db = Database::open_in_memory().expect("db");
    let msg = "fix: close the security hole in token refresh";
    insert(&db, "sha-mapped", "duetto-playwright-e2e", msg, false);
    insert(&db, "sha-unmapped", UNMAPPED, msg, false);

    run(&pipeline, &mut db).await.expect("run");

    let (category, method, _) = verdict(&db, "sha-unmapped");
    assert_eq!(
        (category.as_str(), method.as_str()),
        ("security", "exact_rule")
    );
    let (category, method, _) = verdict(&db, "sha-mapped");
    assert_eq!((category.as_str(), method.as_str()), ("qa", "repo_map"));
}

/// Why (#158 criterion 8): a merge keeps its existing handling; the map
/// decides only non-merge commits.
/// What: the same merge message in a mapped and an unmapped repo gets the
/// same verdict, and neither is `repo_map`.
/// Test: this test.
#[tokio::test]
async fn a_merge_in_a_mapped_repo_keeps_its_existing_handling() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &owner_map()));
    let mut db = Database::open_in_memory().expect("db");
    let msg = "Merge pull request #42 from acme/feature";
    insert(&db, "sha-mapped", MAPPED, msg, true);
    insert(&db, "sha-unmapped", UNMAPPED, msg, true);

    run(&pipeline, &mut db).await.expect("run");

    let mapped = verdict(&db, "sha-mapped");
    assert_ne!(mapped.1, "repo_map");
    assert_eq!(mapped, verdict(&db, "sha-unmapped"));
}

/// Why (#158 criterion 2): a mapped category the config does not know is
/// an error naming the repo and the category, before any write.
/// What: map a repo to `quality_assurance`, which no rule or taxonomy entry
/// names; the run fails and no `classifications` row exists.
/// Test: this test.
#[tokio::test]
async fn an_unknown_mapped_category_is_rejected_before_any_write() {
    let rules = rules_file();
    let map = [("duetto-playwright-e2e", "quality_assurance")];
    let pipeline = ClassificationPipeline::new(config(rules.path(), &map));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", UNMAPPED, "zzz qqq vvv", false);

    let err = run(&pipeline, &mut db).await.expect_err("must refuse");
    let msg = err.to_string();
    assert!(msg.contains("repo_categories"), "{msg}");
    assert!(msg.contains("duetto-playwright-e2e"), "{msg}");
    assert!(msg.contains("quality_assurance"), "{msg}");
    let written: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM classifications", [], |r| r.get(0))
        .expect("count");
    assert_eq!(written, 0);
}

/// Why (#158 criterion 7): a glob key never matched anything as an exact
/// name, so it is refused at load rather than ignored.
/// What: a `*` key fails the run with an error naming the key.
/// Test: this test.
#[tokio::test]
async fn a_glob_key_is_rejected() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &[("mcp-*", "qa")]));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", MAPPED, "zzz qqq vvv", false);
    let msg = run(&pipeline, &mut db).await.expect_err("glob").to_string();
    assert!(msg.contains("mcp-*"), "{msg}");
}

/// Why (#158 criterion 4): the complexity backfill is an LLM call too, so a
/// `repo_map` row is never sent to it.
/// What: one `repo_map` row and one `regex_rule` row, both without a
/// complexity score; the mock sees one request and only the rule row is
/// scored.
/// Test: this test.
#[tokio::test]
async fn backfill_complexity_never_sends_a_repo_map_row_to_the_llm() {
    let server = mock_llm().await;
    let mut db = Database::open_in_memory().expect("db");
    let seed = |sha: &str, method: &str| -> i64 {
        db.connection()
            .execute(
                "INSERT INTO classifications (category, confidence, method, complexity) \
                 VALUES ('qa', 1.0, ?1, NULL)",
                [method],
            )
            .expect("insert classification");
        let cl = db.connection().last_insert_rowid();
        insert(&db, sha, MAPPED, "add the widget", false);
        db.connection()
            .execute(
                "UPDATE commits SET classification_id = ?1 WHERE sha = ?2",
                params![cl, sha],
            )
            .expect("link");
        cl
    };
    let mapped_cl = seed("sha-mapped", "repo_map");
    let rule_cl = seed("sha-rule", "regex_rule");

    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path(), &owner_map()));
    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri())),
    );
    let updated = ClassificationPipeline::backfill_complexity_with_engine(&mut db, &engine)
        .await
        .expect("backfill");

    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(updated, 1);
    let complexity = |id: i64| -> Option<i64> {
        db.connection()
            .query_row(
                "SELECT complexity FROM classifications WHERE id = ?1",
                [id],
                |r| r.get(0),
            )
            .expect("complexity")
    };
    assert_eq!(complexity(rule_cl), Some(2));
    assert_eq!(complexity(mapped_cl), None);
}

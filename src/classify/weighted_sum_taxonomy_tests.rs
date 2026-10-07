//! #165: the weighted-sum tier answers only inside the active category set.
//! In-memory DB and a mock LLM; no network.

use std::io::Write;

use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::tiers::llm::LlmClassifier;
use crate::core::config::{ClassificationConfig, Config, LlmFallbackScope};
use crate::core::db::Database;
use crate::core::models::ClassificationMethod;

/// The weighted-sum tier answers `chore` for this message.
const DEP_BUMP: &str = "chore(deps): bump openssl for CVE-2026-1234";
/// The weighted-sum tier answers `feature` for this message.
const FEATURE: &str = "implement new dashboard feature";

/// A custom taxonomy without `chore`: one rule that never matches, and a
/// `Feature` category spelled differently from the tier's `feature`.
const RULES: &str = "extend_defaults: false
rules:
  - id: never
    category: defect
    keywords: [\"zzz-never-matches-zzz\"]
categories:
  - name: Feature
  - name: upkeep
";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

/// The weighted-sum tier is on (the default).
fn pipeline(rules: &std::path::Path) -> ClassificationPipeline {
    ClassificationPipeline::new(Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            llm_fallback_scope: LlmFallbackScope::Unanswered,
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    })
}

fn insert(db: &Database, sha: &str, message: &str) {
    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, is_merge) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'repo', 0)",
            params![sha, message],
        )
        .expect("insert commit");
}

/// `(category, method)` stored for `sha`.
fn stored(db: &Database, sha: &str) -> (String, String) {
    db.connection()
        .query_row(
            "SELECT cl.category, cl.method FROM commits c \
             JOIN classifications cl ON cl.id = c.classification_id WHERE c.sha = ?1",
            [sha],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("classified")
}

/// Why (#165 defect 2): with `extend_defaults: false` the rules define the
/// whole category set, but the weighted-sum tier wrote its built-in `chore`.
/// What: through `tga classify`'s engine and write path, the dependency bump
/// is stored as `uncategorized`, never `chore`; the next tier (fuzzy) is off
/// for a custom taxonomy and no LLM is configured. A weighted-sum verdict
/// inside the set is kept, in the spelling the rules give it.
/// Test: this test.
#[tokio::test]
async fn weighted_sum_never_stores_a_category_outside_a_custom_taxonomy() {
    let rules = rules_file();
    let pipeline = pipeline(rules.path());
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-dep", DEP_BUMP);
    insert(&db, "sha-feat", FEATURE);

    let engine = pipeline.build_rule_engine().expect("rule engine");
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");

    let (category, method) = stored(&db, "sha-dep");
    assert_eq!(category, "uncategorized", "method {method}");
    assert_ne!(method, "weighted_sum");
    assert_eq!(
        stored(&db, "sha-feat"),
        ("Feature".to_string(), "weighted_sum".to_string())
    );
}

/// Why (#165): a dropped weighted-sum verdict must fall through to the next
/// tier, not vanish.
/// What: the LLM is on and sees only unanswered commits. The dependency bump
/// reaches it once and is stored with the LLM's in-set answer `upkeep`.
/// Test: this test.
#[tokio::test]
async fn a_dropped_weighted_sum_verdict_falls_through_to_the_llm() {
    let rules = rules_file();
    let pipeline = pipeline(rules.path());
    let server = MockServer::start().await;
    let content =
        "{\"category\":\"upkeep\",\"subcategory\":null,\"confidence\":0.9,\"complexity\":2}";
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": content}}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 9}
        })))
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-dep", DEP_BUMP);

    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    let categories = pipeline
        .llm_categories()
        .expect("categories")
        .expect("custom set");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri()))
            .with_allowed_categories(categories),
    );
    pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");

    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(
        stored(&db, "sha-dep"),
        ("upkeep".to_string(), "llm_fallback".to_string())
    );
}

/// Why (#165): with the built-in taxonomy in force the tier is unchanged.
/// What: `extend_defaults: true` and a rule that never matches; the
/// dependency bump still gets today's verdict: `weighted_sum` / `chore`,
/// top level `Maintenance`, confidence 0.60.
/// Test: this test.
#[test]
fn weighted_sum_is_unchanged_when_the_defaults_are_in_force() {
    let ruleset = RuleSet {
        extend_defaults: true,
        rules: vec![Rule {
            id: "never".to_string(),
            category: "defect".to_string(),
            subcategory: None,
            keywords: vec!["zzz-never-matches-zzz".to_string()],
            patterns: vec![],
            priority: 110,
            confidence: 0.9,
        }],
        ..RuleSet::default()
    };
    let engine =
        ClassificationEngine::new(ruleset, ClassificationEngineConfig::default()).expect("engine");

    let v = engine.classify_sync(DEP_BUMP, false).expect("verdict");
    assert_eq!(v.method, ClassificationMethod::WeightedSum);
    assert_eq!(v.category, "chore");
    assert_eq!(v.top_level, Some(TopLevelCategory::Maintenance));
    assert!((v.confidence - 0.60).abs() < 1e-6, "{}", v.confidence);
}

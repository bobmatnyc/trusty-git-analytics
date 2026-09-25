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

/// A 200 chat-completions reply naming `category` at `confidence`, with usage.
fn reply(category: &str, confidence: f64) -> ResponseTemplate {
    let content = format!(
        "{{\"category\":\"{category}\",\"subcategory\":null,\"confidence\":{confidence},\"complexity\":2}}"
    );
    ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "choices": [{"message": {"content": content}}],
        "usage": {"prompt_tokens": 120, "completion_tokens": 9}
    }))
}

/// Mock OpenAI-compatible endpoint answering every call with `template`.
async fn mock_llm(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

/// `(commit_sha, outcome, input_tokens)` of every `llm_usage` row, by sha.
fn usage_rows(db: &Database) -> Vec<(String, String, Option<i64>)> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT commit_sha, outcome, input_tokens FROM llm_usage ORDER BY commit_sha")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows")
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

/// Seed one weak rule hit and one commit no rule answers; run the pipeline
/// against a mock LLM answering with `template`. `break_writes` drops the
/// `classifications` table first so the write-back fails.
async fn run_with(
    scope: LlmFallbackScope,
    template: ResponseTemplate,
    break_writes: bool,
) -> (Database, MockServer, Result<ClassificationStats>) {
    let rules = rules_file(RULES);
    let pipeline = ClassificationPipeline::new(config(rules.path(), scope));
    let server = mock_llm(template).await;
    let mut db = Database::open_in_memory().expect("db");
    insert_commit(&db, "sha-weak", "infra: bump the cluster size");
    insert_commit(&db, "sha-none", "zzz qqq vvv www yyy uuu");
    if break_writes {
        db.connection()
            .execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE classifications;")
            .expect("drop");
    }
    let stats = pipeline
        .run_with_engine(&mut db, engine(&pipeline, &server))
        .await;
    (db, server, stats)
}

async fn run_scope(scope: LlmFallbackScope) -> (Database, MockServer, ClassificationStats) {
    let (db, server, stats) = run_with(scope, reply("enablement", 0.9), false).await;
    (db, server, stats.expect("run"))
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
    assert_eq!(stats.llm_usage.adopted, 1);
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
            "adopted".into(),
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

/// Why (#111 review): a failed call (HTTP 500) keeps the rule verdict and is
/// still counted, with no tokens since none were reported.
/// What: unanswered scope, mock answers 500.
/// Test: this test.
#[tokio::test]
async fn failed_call_keeps_the_rule_verdict() {
    let template = ResponseTemplate::new(500).set_body_string("upstream exploded");
    let (db, _server, stats) = run_with(LlmFallbackScope::Unanswered, template, false).await;
    let stats = stats.expect("run");
    assert_eq!(
        category_of(&db, "sha-none").as_deref(),
        Some("uncategorized")
    );
    assert_eq!((stats.llm_usage.calls, stats.llm_usage.failed), (1, 1));
    assert_eq!(stats.llm_usage.calls_with_usage, 0);
    assert_eq!(
        usage_rows(&db),
        [("sha-none".into(), "failed".into(), None)]
    );
}

/// Why (#131 review): an out-of-set answer is dropped end to end — the rule
/// verdict is stored, and the call is recorded as `out_of_set` with tokens.
/// What: unanswered scope, mock answers the built-in `chore`.
/// Test: this test.
#[tokio::test]
async fn out_of_set_answer_keeps_the_rule_verdict() {
    let (db, _server, stats) =
        run_with(LlmFallbackScope::Unanswered, reply("chore", 0.95), false).await;
    let stats = stats.expect("run");
    assert_eq!(
        category_of(&db, "sha-none").as_deref(),
        Some("uncategorized")
    );
    assert_eq!((stats.llm_usage.calls, stats.llm_usage.out_of_set), (1, 1));
    assert_eq!(stats.llm_usage.input_tokens, 120);
    assert_eq!(
        usage_rows(&db),
        [("sha-none".into(), "out_of_set".into(), Some(120))]
    );
}

/// Why (#111 review): an in-set answer at or below the rule verdict's
/// confidence loses the overwrite guard and is counted as `not_adopted`.
/// What: default scope; the weak hit (0.5) gets a 0.4 answer, the miss (0.0)
/// adopts it.
/// Test: this test.
#[tokio::test]
async fn answer_below_rule_confidence_is_not_adopted() {
    let (db, _server, stats) = run_with(
        LlmFallbackScope::LowConfidence,
        reply("enablement", 0.4),
        false,
    )
    .await;
    let stats = stats.expect("run");
    assert_eq!(category_of(&db, "sha-weak").as_deref(), Some("platform"));
    assert_eq!(category_of(&db, "sha-none").as_deref(), Some("enablement"));
    assert_eq!(
        (stats.llm_usage.adopted, stats.llm_usage.not_adopted),
        (1, 1)
    );
    assert_eq!(
        usage_rows(&db),
        [
            ("sha-none".into(), "adopted".into(), Some(120)),
            ("sha-weak".into(), "not_adopted".into(), Some(120)),
        ]
    );
}

/// Why (#111 review): billed calls are recorded before the classification
/// write-back, so a failed write never loses them.
/// What: the `classifications` table is dropped; the run fails, and the
/// call's `llm_usage` row is still there.
/// Test: this test.
#[tokio::test]
async fn usage_survives_a_failed_write_back() {
    let (db, _server, stats) =
        run_with(LlmFallbackScope::Unanswered, reply("enablement", 0.9), true).await;
    assert!(stats.is_err(), "write-back must fail without the table");
    assert_eq!(
        usage_rows(&db),
        [("sha-none".into(), "adopted".into(), Some(120))]
    );
}

/// Why (#111 review): with `--force --shas` a `--repos` filter that
/// excludes a listed SHA must fail, not classify fewer commits.
/// What: the listed SHA lives in `acme/widgets`; the filter names another
/// repository.
/// Test: this test.
#[tokio::test]
async fn shas_outside_the_filter_fail_before_any_write() {
    let rules = rules_file(RULES);
    let mut db = Database::open_in_memory().expect("db");
    insert_commit(&db, "sha-a", "infra: bump the cluster size");
    let p = subset_pipeline(rules.path(), &["sha-a"]).with_repos(vec!["other/repo".into()]);
    let err = p
        .run_with_engine(&mut db, p.build_rule_engine().expect("engine"))
        .await
        .expect_err("must refuse");
    assert!(err.to_string().contains("sha-a"), "{err}");
    assert_eq!(category_of(&db, "sha-a"), None);
}

/// Why (#111): merges are excluded from metrics and the eval, so an LLM call
/// on one is wasted spend (20 merges in a 100-row sample drew 48 calls).
/// What: two merge commits (`is_merge = 1`) — one no rule answers, one weak
/// rule hit — beside the usual fixture; under both scopes neither merge
/// reaches the mock, each keeps its rule verdict and has no `llm_usage` row.
/// The skip keys on the flag, not the text: one merge's message lacks
/// "merge", and a non-merge whose message says "merge" does reach the mock.
/// Test: this test.
#[tokio::test]
async fn merge_commits_never_reach_the_llm() {
    for (scope, sent) in [
        (LlmFallbackScope::Unanswered, 2),
        (LlmFallbackScope::LowConfidence, 3),
    ] {
        let rules = rules_file(RULES);
        let pipeline = ClassificationPipeline::new(config(rules.path(), scope));
        let server = mock_llm(reply("enablement", 0.9)).await;
        let mut db = Database::open_in_memory().expect("db");
        insert_commit(&db, "sha-weak", "infra: bump the cluster size");
        insert_commit(&db, "sha-none", "zzz qqq vvv www yyy uuu");
        // #111: a merge whose text never says "merge", and a regular commit
        // whose text does, so a message-text skip fails this test.
        insert_commit(&db, "sha-merge-none", "zzz qqq vvv combined");
        insert_commit(&db, "sha-merge-weak", "infra: merge release branch");
        insert_commit(&db, "sha-text-merge", "zzz qqq merge sort rewrite");
        db.connection()
            .execute(
                "UPDATE commits SET is_merge = 1 WHERE sha LIKE 'sha-merge-%'",
                [],
            )
            .expect("mark merges");
        let stats = pipeline
            .run_with_engine(&mut db, engine(&pipeline, &server))
            .await
            .expect("run");

        let requests = server.received_requests().await.expect("rec");
        assert_eq!(requests.len(), sent, "{scope:?}");
        let bodies: Vec<String> = requests
            .iter()
            .map(|r| String::from_utf8_lossy(&r.body).into_owned())
            .collect();
        assert!(
            bodies
                .iter()
                .all(|b| !b.contains("vvv combined") && !b.contains("merge release branch")),
            "{scope:?}: a merge commit reached the LLM"
        );
        assert!(
            bodies.iter().any(|b| b.contains("merge sort rewrite")),
            "{scope:?}: a non-merge that mentions \"merge\" must reach the LLM"
        );
        assert_eq!(
            category_of(&db, "sha-text-merge").as_deref(),
            Some("enablement")
        );
        assert_eq!(stats.llm_usage.calls, sent, "{scope:?}");
        assert_eq!(
            category_of(&db, "sha-merge-none").as_deref(),
            Some("uncategorized")
        );
        assert_eq!(
            category_of(&db, "sha-merge-weak").as_deref(),
            Some("platform")
        );
        assert!(usage_rows(&db)
            .iter()
            .all(|(sha, _, _)| !sha.starts_with("sha-merge")));
    }
}

/// Why (#111): `ClassificationEngine::classify` is the other LLM entry point;
/// it must not send a merge either.
/// What: a message no rule answers, classified once as a merge (no request)
/// and once as a normal commit (one request).
/// Test: this test.
#[tokio::test]
async fn engine_classify_skips_the_llm_for_a_merge() {
    let rules = rules_file(RULES);
    let pipeline = ClassificationPipeline::new(config(rules.path(), LlmFallbackScope::default()));
    let server = mock_llm(reply("enablement", 0.9)).await;
    let engine = engine(&pipeline, &server);

    let merge = engine.classify("zzz qqq vvv merged", true).await;
    assert_eq!(server.received_requests().await.expect("rec").len(), 0);
    assert_eq!(merge.category, "uncategorized");

    let normal = engine.classify("zzz qqq vvv merged", false).await;
    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(normal.category, "enablement");
}

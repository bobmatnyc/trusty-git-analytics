//! #111: the Jev provider against a mock server. No network, no real key:
//! every credential here is a fixed test string, every name synthetic.

use std::io::Write;

use futures::stream::StreamExt;
use rusqlite::params;
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::jev::{JevClassifier, ABSTAIN_CODES, JEV_INPUT_PRICE_PER_MTOK_USD, JEV_MODEL};
use super::jev_obfuscate::KnownNames;
use super::llm::LlmClassifier;
use super::llm_prompt::{LlmCall, LlmOutcome};
use crate::classify::pipeline::ClassificationPipeline;
use crate::classify::rules::CategoryDef;
use crate::core::config::{
    ClassificationConfig, Config, JevOptions, LlmConfig, LlmSource, RepositoryConfig,
};
use crate::core::creds::CredentialSource;
use crate::core::db::Database;
use crate::core::models::ClassificationMethod;

pub(super) const TEST_KEY: &str = "jev-test-key-not-real"; // pragma: allowlist secret
const PRICE: f64 = JEV_INPUT_PRICE_PER_MTOK_USD;

// ---- API seams: the only lines that differ from the 7eb6a3a API ----

/// Let a classifier send: it has seen the run's (here: no) names.
pub(super) fn prep(jev: &JevClassifier) {
    jev.prepare(&[], &[]).expect("prepare");
}

/// Tokens counted against the cap so far.
pub(super) fn spent_tokens(jev: &JevClassifier) -> u64 {
    jev.budget().spent_tokens()
}

/// Reported cost so far, in US dollars.
fn cost_usd(jev: &JevClassifier) -> f64 {
    jev.budget().cost_usd()
}

// ---- helpers ----

/// Default options with `obfuscate: true` (#111: obfuscation is opt-in).
pub(super) fn obfuscating() -> JevOptions {
    JevOptions {
        obfuscate: true,
        ..JevOptions::default()
    }
}

pub(super) fn categories() -> Vec<CategoryDef> {
    let mut feature = CategoryDef::new("feature");
    feature.description = Some("New user-facing   behaviour.".into());
    let mut bugfix = CategoryDef::new("bugfix");
    bugfix.description = Some("Corrects a defect.".into());
    vec![feature, bugfix, CategoryDef::new("chore")]
}

/// A 200 reply choosing `choice`, with usage and a model id.
pub(super) fn reply_with(choice: &str, probs: Value, input: u64, output: u64) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "model": JEV_MODEL,
        "answers": {"category": {
            "type": "choice", "choice": choice, "confidence": 0.99, "probabilities": probs
        }},
        "usage": {"input_tokens": input, "output_tokens": output}
    }))
}

pub(super) fn reply(choice: &str, probs: Value, input_tokens: u64) -> ResponseTemplate {
    reply_with(choice, probs, input_tokens, 12)
}

/// Probabilities over the pseudonym codes; `feature` = CAT_1, `bugfix` =
/// CAT_2, `chore` = CAT_3.
pub(super) fn probs(choice: &str) -> Value {
    let mut p = json!({"CAT_1": 0.0, "CAT_2": 0.0, "CAT_3": 0.1,
                       "NO_MATCH": 0.0, "INSUFFICIENT_INFORMATION": 0.0});
    p[choice] = json!(0.9);
    p
}

pub(super) async fn server_with(template: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(template)
        .mount(&server)
        .await;
    server
}

fn unprepared(server: &MockServer, budget_usd: f64) -> JevClassifier {
    let opts = JevOptions {
        budget_usd,
        // #111: obfuscation is opt-in; these tests cover it.
        obfuscate: true,
        ..JevOptions::default()
    };
    JevClassifier::from_options(Some(TEST_KEY.into()), &opts)
        .expect("keyed")
        .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
        .with_context(categories(), KnownNames::default())
        .expect("context")
}

pub(super) fn classifier(server: &MockServer, budget_usd: f64) -> JevClassifier {
    let jev = unprepared(server, budget_usd);
    prep(&jev);
    jev
}

pub(super) async fn bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("recording on")
        .iter()
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .collect()
}

/// Why: Jev must receive exactly one `choice` question whose criteria are
/// the configured categories under pseudonym codes plus the abstain codes,
/// over the pseudonymized message, with bearer auth.
/// What: one call; checks header, model, message, question set and criteria.
/// Test: this test.
#[tokio::test]
async fn request_body_shape() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = classifier(&server, 1.0);
    jev.classify("feat: add login page for ACME-42").await;

    let reqs = server.received_requests().await.expect("recording on");
    assert_eq!(reqs.len(), 1);
    assert_eq!(
        reqs[0]
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok()),
        Some(format!("Bearer {TEST_KEY}").as_str())
    );
    let body: Value = serde_json::from_slice(&reqs[0].body).expect("json");
    assert_eq!(body["model"], JEV_MODEL);
    assert_eq!(JEV_MODEL, "jev-1.13.0");
    // #111 (owner ruling): v1 never sends the second "mixed" question.
    let raw = String::from_utf8_lossy(&reqs[0].body).to_lowercase();
    assert!(!raw.contains("mixed") && !raw.contains("noul"), "{raw}");
    assert_eq!(
        body["state"]["commit"]["message"],
        "feat: add login page for TICKET_1"
    );
    let questions = body["questions"].as_object().expect("questions");
    assert_eq!(questions.keys().collect::<Vec<_>>(), ["category"]);
    let q = &questions["category"];
    assert_eq!(q["type"], "choice");
    let criteria = q["criteria"].as_object().expect("criteria");
    assert_eq!(criteria["CAT_1"], "New user-facing behaviour.");
    assert_eq!(criteria["CAT_3"], "chore");
}

/// Why (#111 item 5): a category name is operator text; only pseudonym
/// codes leave the host, and the answer maps back to the real category.
#[tokio::test]
async fn category_codes_are_pseudonyms() {
    let server = server_with(reply("CAT_2", probs("CAT_2"), 150)).await;
    let call = classifier(&server, 1.0).classify("zzz").await;
    let body = &bodies(&server).await[0];
    let mut codes: Vec<&str> = body["questions"]["category"]["criteria"]
        .as_object()
        .expect("criteria")
        .keys()
        .map(String::as_str)
        .collect();
    codes.sort_unstable();
    assert_eq!(
        codes,
        [
            "CAT_1",
            "CAT_2",
            "CAT_3",
            "INSUFFICIENT_INFORMATION",
            "NO_MATCH"
        ]
    );
    assert_eq!(call.outcome, LlmOutcome::Answered);
    assert_eq!(call.verdict.expect("answered").category, "bugfix");
}

/// Why: both reserved codes mean "no answer"; neither may become a verdict.
#[tokio::test]
async fn abstain_codes_are_abstentions() {
    for code in ABSTAIN_CODES {
        let server = server_with(reply(code, probs(code), 150)).await;
        let call = classifier(&server, 1.0).classify("zzz").await;
        assert_eq!(call.outcome, LlmOutcome::Abstained, "{code}");
        assert!(call.verdict.is_none());
        assert_eq!(call.usage.map(|u| u.input_tokens), Some(150));
    }
}

/// Why: a code the config does not define must never be stored, and a
/// real category name is not a code.
#[tokio::test]
async fn unknown_code_is_out_of_set() {
    for code in ["CAT_9", "feature"] {
        let mut p = json!({"CAT_1": 0.05, "CAT_2": 0.05, "CAT_3": 0.0,
                           "NO_MATCH": 0.0, "INSUFFICIENT_INFORMATION": 0.0});
        p[code] = json!(0.9);
        let server = server_with(reply(code, p, 150)).await;
        let call = classifier(&server, 1.0).classify("zzz").await;
        assert_eq!(call.outcome, LlmOutcome::OutOfSet, "{code}");
        assert!(call.verdict.is_none());
    }
}

/// Why: the verdict's confidence is the chosen code's probability, not the
/// reply's own `confidence` field.
#[tokio::test]
async fn answer_confidence_is_the_chosen_probability() {
    let p = json!({"CAT_1": 0.7, "CAT_2": 0.2, "CAT_3": 0.1,
                   "NO_MATCH": 0.0, "INSUFFICIENT_INFORMATION": 0.0});
    let server = server_with(reply("CAT_1", p, 150)).await;
    let call = classifier(&server, 1.0).classify("zzz").await;
    assert_eq!(call.outcome, LlmOutcome::Answered);
    let v = call.verdict.expect("answered");
    assert_eq!(v.category, "feature");
    assert_eq!(v.confidence, 0.7);
    assert_eq!(v.method, ClassificationMethod::LlmFallback);
}

/// Why: fail closed — a malformed distribution, or a choice the
/// probabilities do not support, is a failure and never an answer.
#[tokio::test]
async fn invalid_probabilities_fail_closed() {
    let cases = [
        json!({"CAT_1": 0.9, "CAT_2": 0.9, "CAT_3": 0.0,
               "NO_MATCH": 0.0, "INSUFFICIENT_INFORMATION": 0.0}),
        json!({"CAT_1": 0.1, "CAT_2": 0.9, "CAT_3": 0.0,
               "NO_MATCH": 0.0, "INSUFFICIENT_INFORMATION": 0.0}),
        json!({"CAT_1": 1.5, "CAT_2": -0.5}),
    ];
    for p in cases {
        let server = server_with(reply("CAT_1", p.clone(), 150)).await;
        let call = classifier(&server, 1.0).classify("zzz").await;
        assert_eq!(call.outcome, LlmOutcome::Failed, "{p}");
        assert!(call.verdict.is_none());
    }
}

/// Why (#111, tm-jev): a reply that names no model is unverifiable; it is
/// `failed`, never an answer.
#[tokio::test]
async fn reply_without_model_fails() {
    let template = ResponseTemplate::new(200).set_body_json(json!({
        "answers": {"category": {
            "type": "choice", "choice": "CAT_1", "probabilities": probs("CAT_1")
        }},
        "usage": {"input_tokens": 150, "output_tokens": 12}
    }));
    let server = server_with(template).await;
    let call = classifier(&server, 1.0).classify("zzz").await;
    assert_eq!(call.outcome, LlmOutcome::Failed);
    assert!(call.verdict.is_none());
}

/// Why: 429 is back-pressure; the call must retry and then succeed.
#[tokio::test]
async fn http_429_is_retried_then_answers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(reply("CAT_2", probs("CAT_2"), 150))
        .with_priority(2)
        .mount(&server)
        .await;
    let call = classifier(&server, 1.0).classify("zzz").await;
    assert_eq!(call.outcome, LlmOutcome::Answered);
    assert_eq!(server.received_requests().await.expect("rec").len(), 2);
}

/// Why: a server that keeps failing must end as `failed`, not an answer.
#[tokio::test]
async fn http_500_exhausts_to_failed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .expect(3)
        .mount(&server)
        .await;
    let call: LlmCall = classifier(&server, 1.0).classify("zzz").await;
    assert_eq!(call.outcome, LlmOutcome::Failed);
    assert!(call.verdict.is_none() && call.usage.is_none());
}

/// Why: the owner caps each live run; no call may start once the next one
/// could pass the cap.
/// What: six sequential calls against a cap a few calls wide: the outcomes
/// are answers then only skips, one request per answer, spend within cap.
#[tokio::test]
async fn budget_guard_stops_calls_at_the_cap() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 2000)).await;
    let cap_usd = 10_000.5 * PRICE / 1_000_000.0;
    let jev = classifier(&server, cap_usd);
    let mut outcomes = Vec::new();
    for _ in 0..6 {
        outcomes.push(jev.classify("zzz").await.outcome);
    }
    let answered = outcomes
        .iter()
        .take_while(|o| **o == LlmOutcome::Answered)
        .count();
    assert!((1..6).contains(&answered), "{outcomes:?}");
    assert!(outcomes[answered..]
        .iter()
        .all(|o| *o == LlmOutcome::Skipped));
    assert_eq!(
        server.received_requests().await.expect("rec").len(),
        answered
    );
    assert!(spent_tokens(&jev) <= 10_000);
}

/// Why (#111 item 8): concurrent calls must not overrun the cap when a
/// reply bills more than one attempt's estimate.
/// What: twelve calls through `buffer_unordered(8)`; each reply bills 3 000
/// input tokens, above one attempt's estimate (body bytes + 1 024) and
/// below the three-attempt reservation. Spend stays within a 12 000-token
/// cap, in tokens and in dollars.
#[tokio::test]
async fn concurrent_calls_never_pass_the_cap() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 3000)).await;
    let jev = classifier(&server, 12_000.5 * PRICE / 1_000_000.0);
    let jev_ref = &jev;
    let outcomes: Vec<LlmOutcome> = futures::stream::iter(0..12)
        .map(|_| async move { jev_ref.classify("zzz").await.outcome })
        .buffer_unordered(8)
        .collect()
        .await;
    assert!(outcomes.contains(&LlmOutcome::Answered), "{outcomes:?}");
    let spent = spent_tokens(&jev);
    assert!(spent <= 12_000, "spent {spent} tokens against a 12 000 cap");
    assert!(cost_usd(&jev) <= jev.budget().cap_usd() + 1e-15);
}

/// Why (#111 item 8): usage above the reservation means the bound was
/// wrong; the rest of the run must send nothing.
#[tokio::test]
async fn usage_above_the_reservation_exhausts_the_budget() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 1_000_000)).await;
    let jev = classifier(&server, 10.0);
    let mut outcomes = Vec::new();
    for _ in 0..3 {
        outcomes.push(jev.classify("zzz").await.outcome);
    }
    assert_eq!(
        outcomes,
        [
            LlmOutcome::Answered,
            LlmOutcome::Skipped,
            LlmOutcome::Skipped
        ]
    );
}

/// Why (#111 item 11): Jev output tokens are free; the reported cost
/// charges input only.
#[tokio::test]
async fn reported_cost_charges_output_at_zero() {
    let server = server_with(reply_with("CAT_1", probs("CAT_1"), 1000, 2000)).await;
    let jev = classifier(&server, 1.0);
    jev.classify("zzz").await;
    let want = 1000.0 * PRICE / 1_000_000.0;
    assert!((cost_usd(&jev) - want).abs() < 1e-12, "{}", cost_usd(&jev));
}

/// Why (#111 item 2): until the run's names are known, nothing is sent.
#[tokio::test]
async fn unprepared_classifier_sends_nothing() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let call = unprepared(&server, 1.0).classify("zzz").await;
    assert_eq!(call.outcome, LlmOutcome::Failed);
    assert!(server.received_requests().await.expect("rec").is_empty());
}

/// Why: gate B inspects real payloads on the host without sending them.
/// What: dump mode needs no key, writes the exact body, and records skipped.
#[tokio::test]
async fn dump_mode_writes_body_and_sends_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let opts = JevOptions {
        payload_dump_dir: Some(dir.path().join("out")),
        obfuscate: true,
        ..JevOptions::default()
    };
    let jev = JevClassifier::from_options(None, &opts)
        .expect("dump mode needs no key")
        .with_context(categories(), KnownNames::default())
        .expect("context");
    prep(&jev);
    let call = jev.classify("fix PROJ-9 for bob@example.com").await;
    assert_eq!(call.outcome, LlmOutcome::Skipped);
    let files: Vec<_> = std::fs::read_dir(dir.path().join("out"))
        .expect("dir")
        .map(|e| e.expect("entry").path())
        // #111: the on-host token map sits next to the body.
        .filter(|p| !p.to_string_lossy().ends_with(".tokens.json"))
        .collect();
    assert_eq!(files.len(), 1);
    let body: Value =
        serde_json::from_slice(&std::fs::read(&files[0]).expect("read")).expect("json");
    assert_eq!(
        body["state"]["commit"]["message"],
        "fix TICKET_1 for EMAIL_1"
    );
}

/// Why: `source: jev` reads `TYPESAFE_API_KEY` by default and fails loudly
/// without it; its usage rows are labelled `jev`.
#[tokio::test]
async fn from_llm_config_reads_the_typesafe_key() {
    let cfg = LlmConfig {
        source: LlmSource::Jev,
        ..LlmConfig::default()
    };
    let creds = CredentialSource::fixed([("TYPESAFE_API_KEY", TEST_KEY)]);
    let llm = LlmClassifier::from_llm_config_with_creds(&cfg, "gpt-4o-mini", &creds)
        .await
        .expect("keyed");
    assert!(llm.has_api_key() && llm.is_jev());
    assert_eq!(llm.provider_label(), "jev");
    assert_eq!(llm.model(), JEV_MODEL);

    let err =
        LlmClassifier::from_llm_config_with_creds(&cfg, JEV_MODEL, &CredentialSource::empty())
            .await
            .err()
            .expect("no key");
    assert!(err.contains("TYPESAFE_API_KEY"), "{err}");
}

/// Why (#111 item 14): a legacy `classification.llm_provider: jev` fell
/// through to the OpenAI endpoint with raw text when `OPENAI_API_KEY` was
/// set; it must be an error pointing at `llm.source: jev`.
#[test]
fn legacy_jev_provider_is_an_error() {
    let creds = CredentialSource::fixed([("OPENAI_API_KEY", TEST_KEY)]);
    let err = LlmClassifier::from_provider_with_creds("jev", "m", None, &creds)
        .err()
        .expect("jev via the legacy path is refused");
    assert!(err.contains("llm.source: jev"), "{err}");
}

pub(super) const RULES: &str = "extend_defaults: false
rules:
  - id: infra
    category: platform
    keywords: [\"infra:\"]
categories:
  - name: feature
    description: New behaviour for acme-fin users.
";

/// Sensitive commit messages and every original string that must not leave.
const MESSAGES: [&str; 3] = [
    "zzz update for jane.roe@acme-fin.com, see https://git.acme-fin.internal/ledger-core/pull/9 (FIN-4411)",
    "qqq tweak /srv/code/ledger-core/src/settle.rs on db7.acme-fin.internal via paygate\n\n\
     Co-authored-by: Jane Roe <jane.roe@acme-fin.com>\n\
     Signed-off-by: Omar Baker <omar@acme-fin.com>",
    "vvv ledger-core cleanup asked by @oroe for Jane Roe",
];
const SENSITIVE: [&str; 13] = [
    "jane.roe@acme-fin.com",
    "https://git.acme-fin.internal",
    "FIN-4411",
    "/srv/code",
    "settle",
    "db7.acme-fin.internal",
    "paygate",
    "Jane Roe",
    "Omar Baker",
    "omar@acme-fin.com",
    "ledger-core",
    "acme-fin",
    "oroe",
];

/// A pipeline over `messages` (`(message, author, email, is_merge)`) with a
/// mock Jev at `server`; returns the stats after the run.
async fn run_pipeline(
    server: &MockServer,
    rows: &[(&str, &str, &str, bool)],
) -> (crate::classify::ClassificationStats, Database) {
    run_pipeline_with(server, rows, |_| {}).await
}

/// [`run_pipeline`] with `setup` run on the database before the rows are
/// inserted (earlier runs' commits, the `authors` table).
pub(super) async fn run_pipeline_with(
    server: &MockServer,
    rows: &[(&str, &str, &str, bool)],
    setup: impl FnOnce(&Database),
) -> (crate::classify::ClassificationStats, Database) {
    run_pipeline_cfg(server, rows, RULES, setup, |_| {}).await
}

/// [`run_pipeline_with`] over the rules file `rules_yaml`, with `tweak`
/// applied to the config before the pipeline is built (#111).
pub(super) async fn run_pipeline_cfg(
    server: &MockServer,
    rows: &[(&str, &str, &str, bool)],
    rules_yaml: &str,
    setup: impl FnOnce(&Database),
    tweak: impl FnOnce(&mut Config),
) -> (crate::classify::ClassificationStats, Database) {
    let mut rules = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    rules.write_all(rules_yaml.as_bytes()).expect("write rules");
    let llm_cfg = LlmConfig {
        source: LlmSource::Jev,
        // #111 (owner ruling 2026-10-06): obfuscation is opt-in; these
        // pipeline tests cover it, so they turn it on. `tweak` may undo it.
        jev: JevOptions {
            sensitive_terms: vec!["paygate".into()],
            obfuscate: true,
            ..JevOptions::default()
        },
        ..LlmConfig::default()
    };
    let repo = RepositoryConfig {
        path: "/srv/code/ledger-core".into(),
        org: Some("acme-fin".into()),
        ..RepositoryConfig::default()
    };
    let mut config = Config {
        repositories: vec![repo],
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.path().to_path_buf()],
            weighted_sum: crate::classify::tiers::weighted_sum::WeightedSumConfig {
                enabled: false,
                ..Default::default()
            },
            ..ClassificationConfig::default()
        }),
        llm: Some(llm_cfg),
        ..Config::default()
    };
    tweak(&mut config);
    let llm_cfg = config.llm.clone().expect("llm section");
    let pipeline = ClassificationPipeline::new(config);
    let creds = CredentialSource::fixed([("TYPESAFE_API_KEY", TEST_KEY)]);
    let llm = LlmClassifier::from_llm_config_with_creds(&llm_cfg, JEV_MODEL, &creds)
        .await
        .expect("keyed")
        .with_test_jev_endpoint(&format!("{}/v1/systemone", server.uri()));
    let mut engine = pipeline.build_rule_engine().expect("rules");
    engine.attach_llm(pipeline.attach_jev_context(llm).expect("context"));

    let mut db = Database::open_in_memory().expect("db");
    setup(&db);
    for (i, (m, author, email, merge)) in rows.iter().enumerate() {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, is_merge) VALUES (?1, ?2, ?3, '2024-01-01T00:00:00Z', ?4, \
                 'ledger-core', ?5)",
                params![format!("sha-{i}"), author, email, m, i64::from(*merge)],
            )
            .expect("insert");
    }
    let stats = pipeline
        .run_with_engine(&mut db, engine)
        .await
        .expect("run");
    (stats, db)
}

/// Why: the leak guarantee end to end — pipeline, engine, provider, HTTP.
/// What: runs the classify pipeline over three sensitive commits against a
/// mock Jev, then asserts no captured body carries any original string, and
/// that each call's usage is in `llm_usage` under provider `jev` with the
/// model the reply named (#111 item 10).
/// Test: this test.
#[tokio::test]
async fn outbound_body_carries_no_sensitive_string() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let rows: Vec<_> = MESSAGES.iter().map(|m| (*m, "a", "a@x", false)).collect();
    let (stats, db) = run_pipeline(&server, &rows).await;
    assert_eq!(stats.llm_usage.calls, 3);

    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 3);
    for body in &sent {
        // #111 (gate B): the criteria are operator config sent verbatim
        // (see `jev_gateb_tests::category_text_is_never_tokenised`); every
        // other part of the body must be clean.
        let mut rest = body.clone();
        rest["questions"]["category"]["criteria"] = Value::Null;
        let text = rest.to_string().to_lowercase();
        for s in SENSITIVE {
            assert!(!text.contains(&s.to_lowercase()), "{s:?} leaked in {body}");
        }
    }
    let rows: Vec<(String, String, Option<i64>)> = {
        let conn = db.connection();
        let mut stmt = conn
            .prepare("SELECT provider, model, input_tokens FROM llm_usage")
            .expect("prepare");
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("rows")
    };
    assert_eq!(rows.len(), 3);
    for (provider, model, input) in rows {
        assert_eq!(
            (provider.as_str(), model.as_str(), input),
            ("jev", JEV_MODEL, Some(150))
        );
    }
}

/// Why (#111, owner ruling 2026-10-06): the consumer owns the bucket map. A
/// map it supplies widens Jev's one question so each arm can land in every
/// bucket; tga's built-in fallback map must not add categories the rules
/// and Bedrock never see. The bucket pair is derived, never asked for.
/// What: the rules emit `platform` and define `feature`. With no consumer
/// map the question offers those two only; with a map in the config, or in
/// the rules file, it also offers that map's `bug_fix` and
/// `content_design`, each exactly once, and no second question.
/// Test: this function.
#[tokio::test]
async fn jev_choices_cover_every_bucket_category() {
    const MAP: &str =
        "Maintenance: [bug_fix, platform]\nValue Creation: [feature, content_design]\n";
    async fn criteria(rules: &str, buckets: Option<&str>) -> Vec<String> {
        let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
        let buckets = buckets.map(|b| serde_yaml::from_str(b).expect("map"));
        let (stats, _db) = run_pipeline_cfg(
            &server,
            &[("zzz", "a", "a@x", false)],
            rules,
            |_| {},
            |c| c.classification.as_mut().expect("section").buckets = buckets,
        )
        .await;
        assert_eq!(stats.llm_usage.calls, 1);
        let sent = bodies(&server).await;
        let questions = sent[0]["questions"].as_object().expect("questions");
        assert_eq!(questions.len(), 1, "{questions:?}");
        questions["category"]["criteria"]
            .as_object()
            .expect("criteria")
            .values()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    }
    let offered = |texts: &[String], name: &str| {
        texts
            .iter()
            .filter(|t| t.eq_ignore_ascii_case(name))
            .count()
    };

    let fallback = criteria(RULES, None).await;
    assert_eq!(offered(&fallback, "platform"), 1, "{fallback:?}");
    for fine in crate::core::config::BucketMap::default().fine_categories() {
        assert_eq!(
            offered(&fallback, fine),
            0,
            "fallback added {fine}: {fallback:?}"
        );
    }
    // 2 rule categories + 2 abstain codes.
    assert_eq!(fallback.len(), 4, "{fallback:?}");

    let in_rules = format!(
        "{RULES}buckets:\n  Maintenance: [bug_fix, platform]\n  \
         Value Creation: [feature, content_design]\n"
    );
    for texts in [
        criteria(RULES, Some(MAP)).await,
        criteria(&in_rules, None).await,
    ] {
        for fine in ["platform", "bug_fix", "content_design"] {
            assert_eq!(offered(&texts, fine), 1, "{fine}: {texts:?}");
        }
        // 2 rule categories + 2 map-only categories + 2 abstain codes.
        assert_eq!(texts.len(), 6, "{texts:?}");
    }
}

/// Why (#111 item 2): an author known only from commit metadata, or named
/// only in a trailer of a commit the LLM never sees (a merge), must be
/// hidden in every request, including the first.
/// What: commit 0 is authored by `Quenby Vantrell <qvantrell@corp.test>`;
/// commit 1 is a merge whose trailer names `Zed Quillon`; commit 2 names
/// both in prose. No body may carry either name or the login.
#[tokio::test]
async fn db_author_names_are_known_before_the_first_request() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let rows = [
        ("zzz tidy", "Quenby Vantrell", "qvantrell@corp.test", false),
        (
            "Merge branch 'x'\n\nCo-authored-by: Zed Quillon <zq@corp.test>",
            "a",
            "a@x",
            true,
        ),
        (
            "zzz thanks Quenby, qvantrell and Zed Quillon",
            "b",
            "b@x",
            false,
        ),
    ];
    let (_, _db) = run_pipeline(&server, &rows).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    for body in &sent {
        let text = body.to_string().to_lowercase();
        for s in ["quenby", "vantrell", "quillon"] {
            assert!(!text.contains(s), "{s:?} leaked in {body}");
        }
    }
}

/// Why (#111, fail closed): a message the pseudonymizer cannot process must
/// never be sent, and neither may any later message once a rebuild failed.
/// What: a live classifier whose matcher cap fits no 300-name trailer; the
/// trailer message and a later message naming one of those users are both
/// `failed`, with no request on the wire.
#[tokio::test]
async fn obfuscation_error_sends_nothing() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let limit = super::jev_obfuscate_tests::small_limit();
    let jev = JevClassifier::from_options(Some(TEST_KEY.into()), &obfuscating())
        .expect("keyed")
        .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
        .with_context_and_limit(categories(), KnownNames::default(), limit)
        .expect("context");
    prep(&jev);
    let first = jev
        .classify(&super::jev_obfuscate_tests::big_trailer())
        .await;
    let second = jev
        .classify("thanks qzed0001 for the fix\n\nSubscribers: qzed0001")
        .await;
    assert_eq!(first.outcome, LlmOutcome::Failed);
    assert_eq!(second.outcome, LlmOutcome::Failed);
    let sent = server.received_requests().await.expect("rec");
    assert!(sent.is_empty(), "{} request(s) sent", sent.len());
}

/// Why (#111): tga pins `jev-1.13.0`; a reply from any other model is a
/// recorded failure, never an answer, and `llm_usage` names the served
/// model.
#[tokio::test]
async fn reply_from_another_model_is_a_recorded_failure() {
    let template = ResponseTemplate::new(200).set_body_json(json!({
        "model": "jev-1.14.0",
        "answers": {"category": {
            "type": "choice", "choice": "CAT_1", "probabilities": probs("CAT_1")
        }},
        "usage": {"input_tokens": 150, "output_tokens": 12}
    }));
    let server = server_with(template).await;
    let (stats, db) = run_pipeline(&server, &[("zzz tidy", "a", "a@x", false)]).await;
    assert_eq!(stats.llm_usage.failed, 1);
    let row: (String, String) = db
        .connection()
        .query_row("SELECT model, outcome FROM llm_usage", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("row");
    assert_eq!(row, ("jev-1.14.0".to_string(), "failed".to_string()));
}

/// Why (#111): no test may reach TypeSafe. A transport left at its default
/// in a test build points at a closed loopback port.
/// What: a classifier never given a test endpoint fails its call.
#[tokio::test]
async fn unredirected_test_client_never_reaches_typesafe() {
    assert_eq!(
        super::jev::JEV_ENDPOINT,
        "https://api.typesafe.ai/v1/systemone"
    );
    assert!(super::jev::DEFAULT_ENDPOINT.starts_with("http://127.0.0.1:"));
    let jev = JevClassifier::from_options(Some(TEST_KEY.into()), &JevOptions::default())
        .expect("keyed")
        .with_context(categories(), KnownNames::default())
        .expect("context");
    prep(&jev);
    assert_eq!(jev.classify("zzz").await.outcome, LlmOutcome::Failed);
}

/// Why (#111): a configured model other than the pinned one is refused.
#[tokio::test]
async fn unpinned_model_is_refused() {
    let cfg = LlmConfig {
        source: LlmSource::Jev,
        ..LlmConfig::default()
    };
    let creds = CredentialSource::fixed([("TYPESAFE_API_KEY", TEST_KEY)]);
    let err = LlmClassifier::from_llm_config_with_creds(&cfg, "jev-2.0.0", &creds)
        .await
        .err()
        .expect("unpinned model");
    assert!(
        err.contains("jev-1.13.0") && err.contains("jev-2.0.0"),
        "{err}"
    );
}

/// Why (#111): output tokens cost $0, so they never count against the
/// spend cap; only input does.
/// What: five calls that each report 100 input and 5 000 000 output tokens
/// under the default $0.25 cap all answer, and the run costs 500 input
/// tokens.
#[tokio::test]
async fn output_tokens_never_count_against_the_cap() {
    let server = server_with(reply_with("CAT_1", probs("CAT_1"), 100, 5_000_000)).await;
    let jev = classifier(&server, JevOptions::default().budget_usd);
    for _ in 0..5 {
        assert_eq!(jev.classify("zzz").await.outcome, LlmOutcome::Answered);
    }
    assert_eq!(spent_tokens(&jev), 500);
    let want = 500.0 * PRICE / 1_000_000.0;
    assert!((cost_usd(&jev) - want).abs() < 1e-15, "{}", cost_usd(&jev));
}

/// Why (#111): the owner's prices and default cap.
#[test]
fn pricing_and_default_cap() {
    assert_eq!(JEV_INPUT_PRICE_PER_MTOK_USD, 0.042);
    assert_eq!(super::jev::JEV_OUTPUT_PRICE_PER_MTOK_USD, 0.0);
    assert_eq!(crate::core::config::JEV_DEFAULT_BUDGET_USD, 0.25);
    let cap = super::jev_budget::JevBudget::new(JevOptions::default().budget_usd).cap_usd();
    assert!((cap - 0.25).abs() < 1e-6, "{cap}");
}

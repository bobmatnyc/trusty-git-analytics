//! #111 (owner ruling 2026-10-06): Jev receives the commit text as stored
//! unless `llm.jev.obfuscate` is on. No network: a mock server or a dump
//! directory. Every name here is synthetic.

use serde_json::Value;

use super::jev::JevClassifier;
use super::jev_obfuscate::KnownNames;
use super::jev_tests::{bodies, categories, prep, probs, reply, server_with, TEST_KEY};
use super::llm_prompt::{LlmOutcome, TEXT_MODE_OBFUSCATED, TEXT_MODE_REAL};
use crate::classify::pipeline_jev::db_people;
use crate::core::config::JevOptions;

/// A message naming a configured person, a trailer and an e-mail address.
const MESSAGE: &str = "fix: retry for Qirin Vossberg\n\nCo-authored-by: Zed Quillon <zq@corp.test>";

fn roster() -> KnownNames {
    KnownNames {
        people: vec!["Qirin Vossberg".into(), "jroe-acme".into()],
        ..KnownNames::default()
    }
}

fn live(server: &wiremock::MockServer, opts: &JevOptions) -> JevClassifier {
    JevClassifier::from_options(Some(TEST_KEY.into()), opts)
        .expect("keyed")
        .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
        .with_context(categories(), roster())
        .expect("context")
}

/// Why: the default config sends Jev the commit message exactly as stored,
/// and each call records the mode it ran in.
/// What: default options, no `prepare`; one call; the body's
/// `state.commit.message` equals the input byte for byte, the call is
/// answered, and its `text_mode` is `real`.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn default_config_sends_the_message_verbatim() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = live(&server, &JevOptions::default());
    let call = jev.classify(MESSAGE).await;
    assert_eq!(call.outcome, LlmOutcome::Answered);
    assert_eq!(call.text_mode, Some(TEXT_MODE_REAL));
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["state"]["commit"]["message"], Value::from(MESSAGE));
    assert!(logs_contain("Jev: sending real commit text"));
}

/// Why: `obfuscate: true` keeps today's behaviour: names are replaced.
/// What: the same message and roster with the key on; the body carries
/// `PERSON_` tokens and none of the names, and the call's `text_mode` is
/// `obfuscated`.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn obfuscate_true_replaces_names() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let opts = JevOptions {
        obfuscate: true,
        ..JevOptions::default()
    };
    let jev = live(&server, &opts);
    prep(&jev);
    let call = jev.classify(MESSAGE).await;
    assert_eq!(call.text_mode, Some(TEXT_MODE_OBFUSCATED));
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent[0]["state"]["commit"]["message"]
        .as_str()
        .expect("message");
    assert!(message.contains("PERSON_"), "{message}");
    for name in ["Qirin", "Vossberg", "Quillon", "zq@corp.test"] {
        assert!(!message.contains(name), "{name} leaked: {message}");
    }
    assert!(logs_contain("Jev: sending obfuscated text"));
}

/// Why: the commit-hash rule (gate B 2) is part of the pseudonymizer, so it
/// runs only with `obfuscate: true`; by default a hash is sent as stored.
/// What: one message holding a synthetic 12-character commit hash; by
/// default the body carries it verbatim and no matcher is built; with the
/// key on, the hash becomes an `ID_` token and the matcher is built.
/// Test: this test.
#[tokio::test]
async fn commit_hash_is_redacted_only_with_obfuscation() {
    const HASHED: &str = "revert a1b2c3d4e5f6 for Qirin Vossberg";
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let real = live(&server, &JevOptions::default());
    assert!(!real.has_matcher(), "real-text mode built a matcher");
    real.classify(HASHED).await;
    let opts = JevOptions {
        obfuscate: true,
        ..JevOptions::default()
    };
    let obf = live(&server, &opts);
    assert!(obf.has_matcher(), "obfuscation built no matcher");
    prep(&obf);
    obf.classify(HASHED).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0]["state"]["commit"]["message"], Value::from(HASHED));
    let message = sent[1]["state"]["commit"]["message"]
        .as_str()
        .expect("message");
    assert!(message.contains("ID_"), "{message}");
    assert!(!message.contains("a1b2c3d4e5f6"), "hash leaked: {message}");
}

/// Why: the default config must build no name matcher, so a run starts with
/// no matcher cost and a matcher limit cannot fail it.
/// What: a 16-byte `name_matcher_bytes`, which fails the matcher build when
/// obfuscation is on (the control), attaches cleanly by default, builds no
/// matcher, and the call sends the message verbatim.
/// Test: this test.
#[tokio::test]
async fn default_config_builds_no_matcher() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let tiny = JevOptions {
        name_matcher_bytes: 16,
        ..JevOptions::default()
    };
    let control = JevOptions {
        obfuscate: true,
        ..tiny.clone()
    };
    let refused = JevClassifier::from_options(Some(TEST_KEY.into()), &control)
        .expect("keyed")
        .with_context(categories(), roster());
    assert!(refused.is_err(), "a 16-byte matcher must not build");

    let jev = live(&server, &tiny);
    assert!(!jev.has_matcher(), "real-text mode built a matcher");
    jev.classify(MESSAGE).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["state"]["commit"]["message"], Value::from(MESSAGE));
}

/// Why: payload-dump mode works in real-text mode and writes no token map,
/// since no token replaced anything.
/// What: default options with a dump directory; one call is `skipped`, the
/// directory holds one body file and no `.tokens.json`, and the body's
/// message is the input verbatim.
/// Test: this test.
#[tokio::test]
async fn real_text_dump_writes_no_token_map() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("dump");
    let opts = JevOptions {
        payload_dump_dir: Some(out.clone()),
        ..JevOptions::default()
    };
    let jev = JevClassifier::from_options(None, &opts)
        .expect("dump needs no key")
        .with_context(categories(), roster())
        .expect("context");
    let call = jev.classify(MESSAGE).await;
    assert_eq!(call.outcome, LlmOutcome::Skipped);
    assert_eq!(call.text_mode, Some(TEXT_MODE_REAL));
    let files: Vec<_> = std::fs::read_dir(&out)
        .expect("dump dir")
        .map(|e| e.expect("entry").path())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().and_then(|n| n.to_str()).expect("name");
    assert!(!name.ends_with(".tokens.json"), "{name}");
    let body: Value =
        serde_json::from_slice(&std::fs::read(&files[0]).expect("read")).expect("json");
    assert_eq!(body["state"]["commit"]["message"], Value::from(MESSAGE));
}

/// Why: the default config skips the database name scans, and each
/// `llm_usage` row records the text mode.
/// What: an `authors` row whose `aliases` is not JSON makes [`db_people`]
/// fail (the control); a default-config pipeline run over the same
/// database still succeeds, sends the message verbatim, and writes
/// `llm_usage.text_mode = 'real'`.
/// Test: this test.
#[tokio::test]
async fn default_config_scans_no_names() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let bad_aliases = |db: &crate::core::db::Database| {
        db.connection()
            .execute(
                "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES \
                 ('Tamsin Quell', 'tamsin@corp.test', 'not json')",
                [],
            )
            .expect("author");
        assert!(db_people(db).is_err(), "a scan must fail on bad aliases");
    };
    let real_text = |c: &mut crate::core::config::Config| {
        c.llm.as_mut().expect("llm").jev.obfuscate = false;
    };
    let rows = [("zzz thanks Tamsin Quell", "a", "a@x", false)];
    let (stats, db) = super::jev_tests::run_pipeline_cfg(
        &server,
        &rows,
        super::jev_tests::RULES,
        bad_aliases,
        real_text,
    )
    .await;
    assert_eq!(stats.llm_usage.calls, 1);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0]["state"]["commit"]["message"],
        Value::from("zzz thanks Tamsin Quell")
    );
    let mode: Option<String> = db
        .connection()
        .query_row("SELECT text_mode FROM llm_usage", [], |r| r.get(0))
        .expect("usage row");
    assert_eq!(mode.as_deref(), Some(TEXT_MODE_REAL));
}

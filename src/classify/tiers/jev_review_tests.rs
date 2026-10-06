//! #111: regression tests for the code-critic's review of d248b08, one per
//! finding. Each fails against d248b08. No network; every name is synthetic.

use std::time::Duration;

use rusqlite::params;
use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::jev::JevClassifier;
use super::jev_obfuscate::{KnownNames, Obfuscator};
use super::jev_patterns::{classify_dotted, Dotted};
use super::jev_tests::{
    bodies, categories, classifier, prep, probs, reply, run_pipeline_with, server_with,
    spent_tokens, TEST_KEY,
};
use super::llm_prompt::LlmOutcome;
use crate::core::config::JevOptions;
use crate::core::db::Database;

fn run(names: &KnownNames, text: &str) -> String {
    let mut o = Obfuscator::new(names).expect("builds");
    o.obfuscate(text).expect("obfuscates").as_str().to_string()
}

fn plain(text: &str) -> String {
    run(&KnownNames::default(), text)
}

fn assert_absent(out: &str, secrets: &[&str]) {
    let lower = out.to_lowercase();
    for s in secrets {
        assert!(
            !lower.contains(&s.to_lowercase()),
            "{s:?} survived in {out:?}"
        );
    }
}

/// Why (review item 1, critical): a recovery run sends a few commits whose
/// text names people whose own commits were classified in earlier runs,
/// or who are known only from the `authors` table and its aliases.
/// What: the database holds an earlier, classified commit by `Qirin
/// Vossberg <qvossberg@corp.test>` and an `authors` row for `Tamsin Quell`
/// with aliases `tquell` and `tq.alt@corp.test`; the run's only commit is by
/// someone else and names them all. No sent body may carry any of them.
#[tokio::test]
async fn names_from_earlier_runs_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        let c = db.connection();
        c.execute(
            "INSERT INTO classifications (category, confidence, method) \
             VALUES ('bugfix', 0.9, 'exact')",
            [],
        )
        .expect("classification");
        let cid = c.last_insert_rowid();
        c.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, classification_id) VALUES ('sha-old', 'Qirin Vossberg', \
             'qvossberg@corp.test', '2023-01-01T00:00:00Z', 'fix: cache', 'ledger-core', ?1)",
            params![cid],
        )
        .expect("old commit");
        c.execute(
            "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES \
             ('Tamsin Quell', 'tamsin@corp.test', '[\"tquell\", \"tq.alt@corp.test\"]')",
            [],
        )
        .expect("author");
    };
    let rows = [(
        "Revert qvossberg's cache change; Vossberg, Tamsin, tquell and tq.alt agree",
        "b",
        "b@x",
        false,
    )];
    let (_, _db) = run_pipeline_with(&server, &rows, setup).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let text = sent[0].to_string();
    assert_absent(
        &text,
        &["qvossberg", "vossberg", "tamsin", "tquell", "tq.alt"],
    );
}

/// Why (review item 2): identity trailers come in any case and spacing,
/// with or without an address: `Approved by:`, `-with`/`-to` forms, and
/// role words singular or plural.
#[test]
fn trailer_tokens_any_case_and_spacing() {
    let out = plain(
        "fix: cache\n\n\
         Approved by: qvoss\n\
         requested BY: qarn\n\
         Paired-with: qbel\n\
         Thanks-to: qcor\n\
         Reviewer: qdun\n\
         Owners: qeli qfen\n\
         assignee: qgor\n\
         APPROVERS: qhal, qjin\n\
         Author: qkap",
    );
    assert_absent(
        &out,
        &[
            "qvoss", "qarn", "qbel", "qcor", "qdun", "qeli", "qfen", "qgor", "qhal", "qjin", "qkap",
        ],
    );
    assert!(out.contains("Approved by: PERSON_1"), "{out}");
}

/// Why (review item 3): a key glued to `_fix` or inside a branch path is
/// still a ticket.
#[test]
fn ticket_keys_in_branch_names() {
    let out = plain("merge PROJ-1234_fix and feature/qproj-4321 into main");
    assert_absent(&out, &["proj-1234", "qproj-4321"]);
    assert!(out.contains("TICKET_1_fix"), "{out}");
}

/// Why (review item 4): host case does not matter when the last label is a
/// known suffix; camelCase code stays.
#[test]
fn hosts_in_any_case() {
    let out = plain("connect SQLPROD01.CORP.QACME.COM and Qacme.com");
    assert_absent(&out, &["sqlprod01", "qacme"]);
    // #111 round 3: internal and ambiguous suffixes in any case too.
    let out = plain(
        "join SQL01.QACME.LOCAL in realm QACME.LOCAL; mount Qfileserver.local, \
         Api.Qacme.Dev and QLEDGER.PROD",
    );
    assert_absent(&out, &["sql01", "qacme", "qfileserver", "qledger"]);
    let keep = "read fooBar.baz and obj.fooBar";
    assert_eq!(plain(keep), keep);
}

/// Why (review item 5): two-label internal names are hosts; code
/// collisions and bare `.dev`/`.app` names (accepted residual risk) stay.
#[test]
fn internal_two_label_hosts() {
    let out = plain("mount fileserver.local, ledger.prod, paygate.staging and vpn.qa");
    assert_absent(&out, &["fileserver", "ledger", "paygate", "vpn.qa"]);
    let keep = "read config.prod and window.app";
    assert_eq!(plain(keep), keep);
    // #111 (gate B re-run): the `.dev`/`.app` exemption is gone.
    assert_eq!(classify_dotted("qacme.dev", None), Dotted::Host);
    assert_eq!(classify_dotted("qacme.app", None), Dotted::Host);
}

/// Why (review item 6): a wrapped space-separated username list is one
/// trailer.
#[test]
fn folded_username_list() {
    let out = plain("Summary: sync\n\nSubscribers: qharp qmeyer\n    qtan qosei\n\nTest Plan: ran");
    assert_absent(&out, &["qharp", "qmeyer", "qtan", "qosei"]);
    assert!(out.ends_with("Test Plan: ran"), "{out}");
}

/// Why (review item 7): names with apostrophes and hyphens are names, with
/// their hyphen parts; a known person's stop-listed given name is hidden in
/// Titlecase but ordinary lowercase prose stays.
#[test]
fn name_tokens_apostrophes_hyphens_titlecase() {
    let names = KnownNames {
        people: vec![
            "Sean O'Qbrien".into(),
            "Mary-Qjane Quillfeather".into(),
            "Frank Qolt".into(),
        ],
        ..KnownNames::default()
    };
    let out = run(
        &names,
        "thanks O\u{2019}Qbrien, O'Qbrien, Mary-Qjane and Qjane; ask Frank for a frank review",
    );
    assert_absent(&out, &["qbrien", "mary-qjane", "qjane"]);
    assert!(!out.contains("Frank"), "{out}");
    assert!(out.contains("a frank review"), "{out}");
}

/// Why (review item 8): every attempt sent may be billed, a timed-out one
/// included; the cap must count them all.
/// What: attempt 1 times out (then, in a second run, gets a 500), attempt 2
/// answers reporting 150 input tokens; the spend is one attempt's bound
/// (body bytes + 1 024) plus 150.
#[tokio::test]
async fn billed_retries_count_against_the_cap() {
    for timeout in [true, false] {
        let server = MockServer::start().await;
        let first = if timeout {
            ResponseTemplate::new(200).set_delay(Duration::from_secs(3))
        } else {
            ResponseTemplate::new(500)
        };
        Mock::given(method("POST"))
            .respond_with(first)
            .up_to_n_times(1)
            .with_priority(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(reply("CAT_1", probs("CAT_1"), 150))
            .with_priority(2)
            .mount(&server)
            .await;
        let jev = JevClassifier::from_options(Some(TEST_KEY.into()), &JevOptions::default())
            .expect("keyed")
            .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
            .with_test_timeout(Duration::from_millis(300))
            .with_context(categories(), KnownNames::default())
            .expect("context");
        prep(&jev);
        let call = jev.classify("zzz").await;
        assert_eq!(call.outcome, LlmOutcome::Answered, "timeout={timeout}");
        let reqs = server.received_requests().await.expect("rec");
        assert_eq!(reqs.len(), 2, "timeout={timeout}");
        let per_attempt = reqs[0].body.len() as u64 + 1024;
        assert_eq!(spent_tokens(&jev), per_attempt + 150, "timeout={timeout}");
    }
}

/// Why (review item 8): under retries no run passes its cap.
/// What: a cap of exactly one call's reservation; the server always fails,
/// so all three attempts are charged; the spend equals the cap and the next
/// call is skipped without a request.
#[tokio::test]
async fn retries_never_pass_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let probe = classifier(&server, 1.0);
    probe.classify("zzz").await;
    let body_len = server.received_requests().await.expect("rec")[0].body.len() as u64;
    let reservation = (body_len + 1024) * 3;
    let cap_usd = (reservation as f64 + 0.5) * super::jev::JEV_INPUT_PRICE_PER_MTOK_USD / 1e6;
    server.reset().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let jev = classifier(&server, cap_usd);
    assert_eq!(jev.classify("zzz").await.outcome, LlmOutcome::Failed);
    assert_eq!(jev.classify("zzz").await.outcome, LlmOutcome::Skipped);
    assert_eq!(server.received_requests().await.expect("rec").len(), 3);
    assert_eq!(spent_tokens(&jev), reservation);
}

/// Why (review item 9): `llm_usage.model` records the model the reply
/// named on every path, a reply without usage included.
#[tokio::test]
async fn reply_without_usage_records_its_model() {
    let server = server_with(ResponseTemplate::new(200).set_body_json(json!({
        "model": "jev-9.9.9",
        "answers": {"category": {
            "type": "choice", "choice": "CAT_1", "probabilities": probs("CAT_1")
        }}
    })))
    .await;
    let (stats, db) = run_pipeline_with(&server, &[("zzz tidy", "a", "a@x", false)], |_| {}).await;
    assert_eq!(stats.llm_usage.failed, 1);
    let model: String = db
        .connection()
        .query_row("SELECT model FROM llm_usage", [], |r| r.get(0))
        .expect("row");
    assert_eq!(model, "jev-9.9.9");
}

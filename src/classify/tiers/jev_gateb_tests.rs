//! #111: regression tests for the gate-B findings on c5578a9 (leaks and
//! over-redaction over real payloads). Each fails against c5578a9. No
//! network; every name, path and key here is synthetic.

use serde_json::Value;

use super::jev::JevClassifier;
use super::jev_obfuscate::{KnownNames, Obfuscator};
use super::jev_tests::{bodies, prep, probs, reply, run_pipeline_with, server_with, TEST_KEY};
use crate::classify::rules::CategoryDef;
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

/// Why (gate B item 1): two-segment directories, branch names and build
/// files leaked; prose joined by a slash must stay.
#[test]
fn slash_paths_branches_and_files() {
    let out = plain(
        "move qbilling/qinvoices and qsvc/ledger, rename qinvoice_sync.py and \
         Dockerfile.prod, branch feature/qfoo-bar and release/2026-09",
    );
    assert_absent(
        &out,
        &[
            "qbilling",
            "qinvoices",
            "qsvc",
            "qinvoice_sync",
            "dockerfile.prod",
            "qfoo-bar",
            "release/2026-09",
        ],
    );
    assert!(!out.contains("HOST_"), "a file name became a host: {out}");
    assert!(out.contains("PATH_"), "{out}");
    let prose = "use and/or, read/write, client/server, 1/2, 2026/09/25 and w/o";
    assert_eq!(plain(prose), prose);
}

/// Why (gate B item 2): lowercase keys with one digit leaked; every form
/// the uppercase rule handles must work in lowercase too.
#[test]
fn lowercase_ticket_keys_any_digits() {
    let out = plain("fix qab-1, [qproj-7], qabc-3_fix and build_qxy-4 today");
    assert_absent(&out, &["qab-1", "qproj-7", "qabc-3", "qxy-4"]);
    assert!(
        out.contains("_fix") && out.contains("build_TICKET_"),
        "{out}"
    );
    let keep = "utf-8 python-3 step-2 e2e-1 fix-1 v1.2.3";
    assert_eq!(plain(keep), keep);
}

/// Why (gate B item 3): single-word identities that are generic or
/// technical words (`test`, `admin`, `ci`, `deploy`, bots) were learned
/// and erased classification vocabulary from ordinary prose; the real
/// person behind a generic login must still be hidden by their full name
/// and address.
#[test]
fn generic_logins_are_not_learned() {
    let names = KnownNames {
        people: [
            "test",
            "admin",
            "ci",
            "build",
            "deploy",
            "action",
            "code",
            "role",
            "dev",
            "renovate[bot]",
            "deploy-bot",
            "Dev Qarlsen",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        ..KnownNames::default()
    };
    let out = run(
        &names,
        "Fix the test build; admin role for ci deploy action code on the dev server\n\n\
         thanks Dev Qarlsen (dev@qcorp.test) and Qarlsen",
    );
    assert!(
        out.starts_with(
            "Fix the test build; admin role for ci deploy action code on the dev server"
        ),
        "{out}"
    );
    assert_absent(&out, &["qarlsen", "dev@qcorp.test", "qcorp"]);
}

/// Why (gate B item 3): a name part never matches a word of the rules or
/// categories, including a pattern word behind regex escapes; a stop-list
/// given name is matched only in Titlecase and never at a sentence start.
#[test]
fn rule_vocabulary_is_never_a_name() {
    let names = KnownNames {
        people: vec!["Qlara Hotfix".into(), "Mark Qoven".into()],
        vocab: vec![r"(?i)\bhotfix(es)?\b".into(), "invoice".into()],
        ..KnownNames::default()
    };
    let out = run(
        &names,
        "Mark the hotfix done; apply the hotfix per Qlara Hotfix, ask Mark and Qoven",
    );
    assert!(
        out.starts_with("Mark the hotfix done; apply the hotfix per "),
        "{out}"
    );
    assert_absent(&out, &["qlara", "qoven", "ask mark"]);
}

/// Why (gate B item 1): the category text sent by `build_criteria` is
/// never tokenised, even when it contains a configured term or a known
/// name.
#[tokio::test]
async fn category_text_is_never_tokenised() {
    let mut a = CategoryDef::new("billing_fix");
    a.description = Some("Fixes in qpaygate billing/invoices for Qlara's team.".into());
    let opts = JevOptions {
        sensitive_terms: vec!["qpaygate".into()],
        ..JevOptions::default()
    };
    let names = KnownNames {
        people: vec!["Qlara Qoven".into()],
        ..KnownNames::default()
    };
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = JevClassifier::from_options(Some(TEST_KEY.into()), &opts)
        .expect("keyed")
        .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
        .with_context(vec![a, CategoryDef::new("chore")], names)
        .expect("context");
    prep(&jev);
    jev.classify("zzz").await;
    let sent = bodies(&server).await;
    let criteria: &Value = &sent[0]["questions"]["category"]["criteria"];
    assert_eq!(
        criteria["CAT_1"],
        "Fixes in qpaygate billing/invoices for Qlara's team."
    );
    assert_eq!(criteria["CAT_2"], "chore");
}

/// Why (gate B item 4): Jira comment authors and reporters, and the
/// weekly fact tables' engineers, were never learned.
#[tokio::test]
async fn jira_comment_and_reporter_names_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        let c = db.connection();
        c.execute(
            "INSERT INTO fact_jira_comment_detail (ticket_key, comment_id, project_key, \
             author, created_at, body_len, synced_at) VALUES ('QJ-1', 'c1', 'QJ', \
             'Quorra Vantablack', '2024-01-01T00:00:00Z', 10, 0)",
            [],
        )
        .expect("jira comment");
        c.execute(
            "INSERT INTO work_items (id, source, title, status, item_type) \
             VALUES ('QJ-2', 'jira', 't', 'Done', 'Task')",
            [],
        )
        .expect("work item");
        c.execute(
            "INSERT INTO fact_pm_effort (work_item_id, work_item_source, pm_name, \
             score_status, epic_children_count, description_word_count, comment_count, \
             transition_count, inputs_present, computed_at) VALUES ('QJ-2', 'jira', \
             'Pellam Qordova', 'SCORED', 0, 0, 0, 0, 'NONE', 0)",
            [],
        )
        .expect("pm effort");
    };
    let rows = [(
        "zzz per Vantablack and Qordova, cc Quorra",
        "b",
        "b@x",
        false,
    )];
    let (_, _db) = run_pipeline_with(&server, &rows, setup).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent[0]["state"]["commit"]["message"].to_string();
    assert_absent(&message, &["vantablack", "qordova", "quorra"]);
}

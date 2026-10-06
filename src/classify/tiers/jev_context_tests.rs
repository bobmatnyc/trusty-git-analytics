//! #111: `llm.context` on the Jev path. The context rides in the message
//! text; with `llm.jev.obfuscate: true` every part of it goes through the
//! pseudonymizer. Mock server or dump directory; no network. Every name and
//! path is synthetic.

use rusqlite::params;
use serde_json::Value;

use super::jev_tests::{bodies, probs, reply, run_pipeline_cfg, server_with, RULES};
use crate::core::config::{Config, LlmConfig};
use crate::core::db::Database;

const MESSAGE: &str = "zzz qqq vvv";
/// Changed paths; `Dockerfile` and `build` have no slash and no extension,
/// so only a whole-path pseudonym hides them.
const PATHS: [&str; 4] = [
    "Dockerfile",
    "build",
    "infra/acme-fin/deploy.yaml",
    "services/paygate-bridge/src/settle_core.rs",
];
/// Every path part a raw path would show; extensions are kept on purpose.
const PATH_PARTS: [&str; 9] = [
    "Dockerfile",
    "build",
    "infra",
    "acme-fin",
    "deploy",
    "services",
    "paygate-bridge",
    "src/",
    "settle_core",
];
const PR_TITLE: &str = "Settle fix for Qirin Vossberg on ledger-core";

/// One unanswered commit by Qirin Vossberg with [`PATHS`], a PR titled
/// [`PR_TITLE`] and a linked Bug.
fn seed(db: &Database) {
    let conn = db.connection();
    conn.execute(
        "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
         VALUES ('sha-ctx', 'Qirin Vossberg', 'qv@corp.test', '2024-01-01T00:00:00Z', ?1, \
         'ledger-core')",
        params![MESSAGE],
    )
    .expect("insert commit");
    let id: i64 = conn
        .query_row("SELECT id FROM commits WHERE sha = 'sha-ctx'", [], |r| {
            r.get(0)
        })
        .expect("id");
    for p in PATHS {
        conn.execute(
            "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'M')",
            params![id, p],
        )
        .expect("insert file");
    }
    conn.execute(
        "INSERT INTO pull_requests (pr_number, title, author, state, created_at, commit_shas) \
         VALUES (3, ?1, 'qv', 'merged', '2024-01-02', '[\"sha-ctx\"]')",
        params![PR_TITLE],
    )
    .expect("insert pr");
    conn.execute(
        "INSERT INTO work_items (id, source, title, status, item_type) \
         VALUES ('W-9', 'jira', 'Settle drops', 'Done', 'Bug')",
        [],
    )
    .expect("insert work item");
    conn.execute(
        "INSERT INTO commit_work_items (commit_sha, work_item_id, work_item_source) \
         VALUES ('sha-ctx', 'W-9', 'jira')",
        [],
    )
    .expect("link");
}

/// Replace the `llm:` section with `yaml`, parsed as `Config::load` reads it.
fn llm_from(yaml: &str) -> impl FnOnce(&mut Config) + '_ {
    move |c: &mut Config| {
        let llm: LlmConfig =
            serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("llm section {yaml:?}: {e}"));
        c.llm = Some(llm);
    }
}

fn sent_message(body: &Value) -> String {
    body["state"]["commit"]["message"]
        .as_str()
        .expect("message")
        .to_string()
}

/// Why (#111): Jev is a third-party host. With `obfuscate: true` no part of
/// the context may leave raw — paths are a redacted category, and a PR
/// title can name a person or a repository.
/// What: a pipeline run with all three context items and obfuscation on;
/// the one body's message carries the block (headers, `PATH_` tokens, the
/// issue type) and none of the path parts, the author's name or the
/// repository name.
/// Test: this test.
#[tokio::test]
async fn obfuscated_jev_context_sends_no_raw_path() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let yaml = "source: jev\ncontext: [paths, pr_title, issue_type]\n\
                jev:\n  obfuscate: true\n";
    let (stats, _db) = run_pipeline_cfg(&server, &[], RULES, seed, llm_from(yaml)).await;
    assert_eq!(stats.llm_usage.calls, 1);
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent_message(&sent[0]);
    assert!(message.starts_with(MESSAGE), "{message}");
    assert!(message.contains("--- commit context"), "{message}");
    assert!(message.contains("Changed paths:\n- PATH_"), "{message}");
    assert!(message.contains("PR title: "), "{message}");
    assert!(message.contains("Issue type: Bug"), "{message}");
    assert_eq!(
        message.matches("\n- PATH_").count(),
        PATHS.len(),
        "{message}"
    );
    for raw in PATH_PARTS
        .iter()
        .chain(&["Qirin", "Vossberg", "ledger-core"])
    {
        assert!(!message.contains(raw), "{raw} leaked: {message}");
    }
}

/// Why (#111): without obfuscation Jev gets the commit text as stored, and
/// the context is part of that text.
/// What: the same run with `obfuscate` left off sends the message followed
/// by the plain block, byte for byte.
/// Test: this test.
#[tokio::test]
async fn real_text_jev_context_is_sent_as_stored() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let yaml = "source: jev\ncontext: [paths, pr_title, issue_type]\n";
    run_pipeline_cfg(&server, &[], RULES, seed, llm_from(yaml)).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let expected = format!(
        "{MESSAGE}\n\n--- commit context (from the repository, not the commit message) ---\n\
         Changed paths:\n- Dockerfile\n- build\n- infra/acme-fin/deploy.yaml\n\
         - services/paygate-bridge/src/settle_core.rs\nPR title: {PR_TITLE}\nIssue type: Bug\n\
         --- end commit context ---"
    );
    assert_eq!(sent_message(&sent[0]), expected);
}

/// Why (#111): the payload dump names each file by a hash of the body,
/// the one prompt-keyed artifact tga writes; a run with context must never
/// collide with, and so reuse, a no-context body.
/// What: two dump runs over the same commit, without and with
/// `context: [paths]`; each writes one body, the names differ, and only
/// the context body carries the block.
/// Test: this test.
#[tokio::test]
async fn context_changes_the_jev_payload_key() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut names = Vec::new();
    for (sub, context) in [("bare", "[]"), ("ctx", "[paths]")] {
        let out = dir.path().join(sub);
        let yaml = format!(
            "source: jev\ncontext: {context}\njev:\n  payload_dump_dir: {}\n",
            out.display()
        );
        run_pipeline_cfg(&server, &[], RULES, seed, llm_from(&yaml)).await;
        let bodies: Vec<_> = std::fs::read_dir(&out)
            .expect("dump dir")
            .map(|e| e.expect("entry").path())
            .filter(|p| !p.to_string_lossy().ends_with(".tokens.json"))
            .collect();
        assert_eq!(bodies.len(), 1, "{sub}: {bodies:?}");
        let body: Value =
            serde_json::from_slice(&std::fs::read(&bodies[0]).expect("read")).expect("json");
        let has_block = sent_message(&body).contains("--- commit context");
        assert_eq!(has_block, sub == "ctx", "{sub}");
        names.push(bodies[0].file_name().expect("name").to_owned());
    }
    assert_ne!(names[0], names[1], "context and bare bodies share a key");
    assert!(
        bodies(&server).await.is_empty(),
        "a dump run sent a request"
    );
}

//! #111: regression tests for the gate-B re-run on 7930a6e (host and path
//! leaks) and the dump token map. Each fails against 7930a6e. No network;
//! every name, path and host here is synthetic.

use rusqlite::params;
use serde_json::Value;

use super::jev::JevClassifier;
use super::jev_obfuscate::{KnownNames, Obfuscator};
use super::jev_patterns::{classify_dotted, Dotted};
use super::jev_tests::{bodies, categories, prep, probs, reply, run_pipeline_with, server_with};
use super::llm_prompt::LlmOutcome;
use crate::core::config::JevOptions;
use crate::core::db::Database;

fn plain(text: &str) -> String {
    let mut o = Obfuscator::new(&KnownNames::default()).expect("builds");
    o.obfuscate(text).expect("obfuscates").as_str().to_string()
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

/// Why (gate B re-run, item 1a): bare repository file names in prose
/// leaked, including well-known ones that were exempt.
#[test]
fn bare_file_names_are_paths() {
    let out = plain(
        "edit qinvoice_sync.py, QPriceTable.tsx, docker-compose.override.yml, Makefile, \
         Dockerfile, README.md, package.json and .github/workflows/qrelease.yml",
    );
    assert_absent(
        &out,
        &[
            "qinvoice_sync",
            "qpricetable",
            "docker-compose",
            "makefile",
            "dockerfile",
            "readme",
            "package.json",
            "workflows",
            "qrelease",
        ],
    );
    assert!(out.contains("PATH_") && !out.contains("HOST_"), "{out}");
}

/// Why (gate B re-run, item 1b): files with no or an unusual extension are
/// caught only by learning the file names the database records.
/// What: `files.path` of an earlier commit holds `services/qledgerd/Qbuildspec`
/// and `tools/qprice_calc.qcfg`; the run's commit names both basenames and
/// the stem. None may be sent. `services/build`, an extension-less
/// vocabulary basename, teaches nothing.
#[tokio::test]
async fn db_file_names_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        let c = db.connection();
        c.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, classification_id) VALUES ('sha-files', 'a', 'a@x', \
             '2023-01-01T00:00:00Z', 'x', 'r', NULL)",
            [],
        )
        .expect("commit");
        let id = c.last_insert_rowid();
        for path in [
            "services/qledgerd/Qbuildspec",
            "tools/qprice_calc.qcfg",
            "services/build",
        ] {
            c.execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'modified')",
                params![id, path],
            )
            .expect("file");
        }
    };
    let rows = [(
        "zzz update Qbuildspec and qprice_calc.qcfg; rerun qprice_calc after the build",
        "b",
        "b@x",
        false,
    )];
    let (_, _db) = run_pipeline_with(&server, &rows, setup).await;
    let sent = bodies(&server).await;
    let message = sent
        .iter()
        .map(|b| b["state"]["commit"]["message"].to_string())
        .find(|m| m.contains("rerun"))
        .expect("the run's commit was sent");
    assert_absent(&message, &["qbuildspec", "qprice_calc", "qcfg"]);
    assert!(message.contains("after the build"), "{message}");
}

/// Why (gate B re-run, item 2): two-segment slash tokens leaked; only a
/// fixed list of prose pairs, all-digit forms and a one-character side stay.
/// All-caps pairs are no longer exempt.
#[test]
fn only_fixed_prose_pairs_escape_the_slash_rule() {
    let out = plain("ship CI/CD, I/O and QFOO/QBAR plus qfoo/qbar");
    assert_absent(&out, &["ci/cd", "i/o", "qfoo", "qbar"]);
    let keep = "and/or read/write client/server input/output true/false yes/no on/off \
                pass/fail Or/And 1/2 2026/09/25 w/o";
    assert_eq!(plain(keep), keep);
}

/// Why (gate B re-run, item 3): bare two-label names on the formerly exempt
/// suffixes leaked; they are hosts unless the head is a code or member
/// word.
#[test]
fn exempt_suffix_domains_are_hosts() {
    let suffixes = [
        "dev", "app", "in", "it", "at", "be", "me", "us", "info", "tech", "site", "online",
        "global", "test", "no", "so", "to",
    ];
    for s in suffixes {
        let name = format!("qacme.{s}");
        assert_eq!(classify_dotted(&name, None), Dotted::Host, "{name}");
        assert_absent(&plain(&format!("see {name} today")), &["qacme"]);
    }
    let keep = "read config.dev, set window.app and process.env.dev";
    assert_eq!(plain(keep), keep);
}

/// Why (gate B re-run, item 4): redaction loss is reported per payload, so
/// the dump must say which original each token replaced, on the host only.
/// What: dump mode writes `jev-request-<hash>.tokens.json` next to the body,
/// mapping each pseudonym in the message to its original.
#[tokio::test]
async fn dump_writes_the_token_map() {
    let dir = tempfile::tempdir().expect("tempdir");
    let out = dir.path().join("out");
    let opts = JevOptions {
        payload_dump_dir: Some(out.clone()),
        obfuscate: true,
        ..JevOptions::default()
    };
    let jev = JevClassifier::from_options(None, &opts)
        .expect("dump mode needs no key")
        .with_context(categories(), KnownNames::default())
        .expect("context");
    prep(&jev);
    let call = jev.classify("fix QPROJ-9 for qbob@example.com").await;
    assert_eq!(call.outcome, LlmOutcome::Skipped);
    let maps: Vec<_> = std::fs::read_dir(&out)
        .expect("dir")
        .map(|e| e.expect("entry").path())
        .filter(|p| p.to_string_lossy().ends_with(".tokens.json"))
        .collect();
    assert_eq!(maps.len(), 1, "no token map written");
    let map: Value = serde_json::from_slice(&std::fs::read(&maps[0]).expect("read")).expect("json");
    assert_eq!(map["tokens"]["TICKET_1"], "QPROJ-9");
    assert_eq!(map["tokens"]["EMAIL_1"], "qbob@example.com");
}

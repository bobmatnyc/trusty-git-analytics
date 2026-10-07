//! #175: `classification.use_llm: false` is a hard off switch.
//!
//! With a populated `llm:` section, `tga classify` and the complexity
//! backfill must build no provider client and send nothing. Configs are
//! parsed from YAML, as `Config::load` reads them. No network: the counted
//! provider is Jev in payload-dump mode, which writes one file per call and
//! sends nothing. Bedrock, OpenRouter and the Anthropic API only ever run
//! against an empty database, so even a regression that builds their client
//! has no commit to send.

use std::io::Write;
use std::path::Path;

use rusqlite::params;

use super::ClassificationPipeline;
use crate::core::config::Config;
use crate::core::db::Database;

/// A custom-only rules set whose one rule never matches [`MESSAGE_PREFIX`],
/// so every commit stays unanswered and LLM-eligible.
const RULES: &str = "extend_defaults: false
rules:
  - id: infra
    category: platform
    keywords: [\"infra:\"]
categories:
  - name: feature
";

/// The start of a message no rule answers; each commit appends its SHA.
const MESSAGE_PREFIX: &str = "zzz qqq";

/// An `api_key_env` no environment sets.
const UNSET_KEY_ENV: &str = "TGA_TEST_175_NEVER_SET_KEY";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

/// A full config: `classification:` with `use_llm_line` (empty for an
/// absent key) and the `llm:` section body `llm` (each line indented two
/// spaces), parsed as YAML.
fn config(rules: &Path, use_llm_line: &str, llm: &str) -> Config {
    let yaml = format!(
        "classification:\n  rules_files: [\"{}\"]\n{use_llm_line}  weighted_sum:\n    \
         enabled: false\nllm:\n{llm}",
        rules.display()
    );
    serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("config {yaml:?}: {e}"))
}

/// The Jev `llm:` section in payload-dump mode, writing into `dump`.
fn jev_section(dump: &Path) -> String {
    format!(
        "  source: jev\n  jev:\n    payload_dump_dir: \"{}\"\n",
        dump.display()
    )
}

/// One `llm:` section per provider source. Bedrock, OpenRouter and the
/// Anthropic API name a model or key variable as an operator would.
fn every_source(dump: &Path) -> Vec<(&'static str, String)> {
    vec![
        (
            "bedrock",
            "  source: bedrock\n  region: us-east-1\n  model: anthropic.claude-test\n".into(),
        ),
        (
            "openrouter",
            format!("  source: openrouter\n  api_key_env: {UNSET_KEY_ENV}\n"),
        ),
        (
            "anthropic-api",
            format!("  source: anthropic-api\n  api_key_env: {UNSET_KEY_ENV}\n"),
        ),
        ("jev", jev_section(dump)),
    ]
}

/// Provider calls the Jev dump made. The dump names each file by a hash of
/// the request body, so every commit carries a distinct message and each
/// call writes its own file.
fn dump_files(dump: &Path) -> usize {
    match std::fs::read_dir(dump) {
        Ok(entries) => entries.count(),
        Err(_) => 0,
    }
}

fn insert_commit(db: &Database, sha: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, 'widgets')",
            params![sha, format!("{MESSAGE_PREFIX} {sha}")],
        )
        .expect("insert commit");
    db.connection().last_insert_rowid()
}

/// A classified commit whose `complexity` is NULL: a backfill candidate.
fn insert_backfill_candidate(db: &Database, sha: &str) {
    db.connection()
        .execute(
            "INSERT INTO classifications (category, confidence, method, complexity) \
             VALUES ('feature', 0.5, 'regex_rule', NULL)",
            [],
        )
        .expect("insert classification");
    let cl = db.connection().last_insert_rowid();
    let c = insert_commit(db, sha);
    db.connection()
        .execute(
            "UPDATE commits SET classification_id = ?1 WHERE id = ?2",
            params![cl, c],
        )
        .expect("link");
}

fn llm_usage_rows(db: &Database) -> i64 {
    db.connection()
        .query_row("SELECT COUNT(*) FROM llm_usage", [], |r| r.get(0))
        .expect("count llm_usage")
}

/// Why (#175): `use_llm: false` with a populated `llm:` section sent every
/// commit to the configured provider (67,867 Bedrock calls in one run).
/// What: three unanswered commits, `use_llm: false`, a Jev `llm:` section
/// in dump mode; `tga classify`'s pipeline must write no payload, record no
/// call, and log no "LLM provider" line, and must say the section is
/// ignored.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn use_llm_false_classify_sends_nothing_to_the_configured_provider() {
    let rules = rules_file();
    let dir = tempfile::tempdir().expect("tempdir");
    let dump = dir.path().join("out");
    let cfg = config(rules.path(), "  use_llm: false\n", &jev_section(&dump));
    let mut db = Database::open_in_memory().expect("db");
    for i in 0..3 {
        insert_commit(&db, &format!("sha-{i}"));
    }

    let stats = ClassificationPipeline::new(cfg)
        .run(&mut db)
        .await
        .expect("classify runs with the LLM off");

    let calls = dump_files(&dump);
    assert_eq!(
        calls, 0,
        "use_llm: false made {calls} provider call(s) (jev payload files)"
    );
    assert_eq!(stats.llm_usage.calls, 0, "LLM calls counted in the stats");
    assert_eq!(llm_usage_rows(&db), 0, "llm_usage rows written");
    logs_assert(no_provider_line);
    assert!(
        logs_contain("ignored because classification.use_llm is false"),
        "no log line says the llm: section is ignored"
    );
}

/// Why (#175 criterion 3): the complexity backfill builds the same LLM tier
/// and calls it once per candidate; gating only the classify fallback
/// leaves it spending.
/// What: two backfill candidates, `use_llm: false`, a Jev `llm:` section in
/// dump mode; the backfill must write no payload and score nothing.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn use_llm_false_backfill_sends_nothing_to_the_configured_provider() {
    let rules = rules_file();
    let dir = tempfile::tempdir().expect("tempdir");
    let dump = dir.path().join("out");
    let cfg = config(rules.path(), "  use_llm: false\n", &jev_section(&dump));
    let mut db = Database::open_in_memory().expect("db");
    insert_backfill_candidate(&db, "sha-a");
    insert_backfill_candidate(&db, "sha-b");

    let updated = ClassificationPipeline::new(cfg)
        .backfill_complexity(&mut db)
        .await
        .expect("backfill runs with the LLM off");

    let calls = dump_files(&dump);
    assert_eq!(
        calls, 0,
        "use_llm: false made {calls} backfill provider call(s) (jev payload files)"
    );
    assert_eq!(updated, 0, "the backfill scored rows with the LLM off");
    logs_assert(no_provider_line);
}

/// Why (#175 criterion 2): building the client is the step that leads to
/// spend; a fix that builds it and skips the calls is one regression away
/// from the same bill, and a key-based provider with no key turned a run
/// that never needs the LLM into an error.
/// What: for each provider source and for both entry points (classify and
/// the complexity backfill), an empty database and `use_llm: false`; every
/// run succeeds and no "LLM provider" line is logged; every `llm:`-section
/// constructor logs one.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn use_llm_false_builds_no_client_for_any_source() {
    let rules = rules_file();
    let dir = tempfile::tempdir().expect("tempdir");
    let dump = dir.path().join("out");
    let mut failures = Vec::new();
    for (name, section) in every_source(&dump) {
        let cfg = config(rules.path(), "  use_llm: false\n", &section);
        let pipeline = ClassificationPipeline::new(cfg);
        let mut db = Database::open_in_memory().expect("db");
        if let Err(e) = pipeline.run(&mut db).await {
            failures.push(format!("{name} classify: {e}"));
        }
        if let Err(e) = pipeline.backfill_complexity(&mut db).await {
            failures.push(format!("{name} backfill: {e}"));
        }
    }
    assert!(
        failures.is_empty(),
        "use_llm: false still built a provider client:\n{}",
        failures.join("\n")
    );
    logs_assert(no_provider_line);
    assert_eq!(dump_files(&dump), 0, "jev wrote a payload");
}

/// Why (#175): `tga classify`, the backfill and `tga rules test` all read
/// one predicate, so it carries the rule. Absent `use_llm` keeps the
/// self-enabling `llm:` section; `use_llm: true` keeps the legacy path.
/// What: the predicate over every combination of the key and the section.
/// Test: this test.
#[test]
fn llm_enabled_honours_an_explicit_false() {
    let rules = rules_file();
    let dir = tempfile::tempdir().expect("tempdir");
    let bedrock = "  source: bedrock\n  region: us-east-1\n";
    let cases = [
        (
            "use_llm: false + bedrock",
            "  use_llm: false\n",
            bedrock.to_string(),
            false,
        ),
        (
            "use_llm: false + jev",
            "  use_llm: false\n",
            jev_section(dir.path()),
            false,
        ),
        ("use_llm absent + bedrock", "", bedrock.to_string(), true),
        (
            "use_llm: true + bedrock",
            "  use_llm: true\n",
            bedrock.to_string(),
            true,
        ),
    ];
    for (label, line, section, want) in cases {
        let got = ClassificationPipeline::new(config(rules.path(), line, &section)).llm_enabled();
        assert_eq!(got, want, "{label}");
    }
    // No `llm:` section: only `use_llm: true` turns the tier on.
    let no_section = |line: &str| -> Config {
        let yaml = format!(
            "classification:\n  rules_files: [\"{}\"]\n{line}",
            rules.path().display()
        );
        serde_yaml::from_str(&yaml).expect("config")
    };
    assert!(!ClassificationPipeline::new(no_section("")).llm_enabled());
    assert!(!ClassificationPipeline::new(no_section("  use_llm: false\n")).llm_enabled());
    assert!(ClassificationPipeline::new(no_section("  use_llm: true\n")).llm_enabled());
}

/// `logs_assert` check: no captured line names a provider; every
/// `llm:`-section constructor logs one.
fn no_provider_line(lines: &[&str]) -> Result<(), String> {
    let hits: Vec<&&str> = lines
        .iter()
        .filter(|l| l.contains("LLM provider"))
        .collect();
    if hits.is_empty() {
        Ok(())
    } else {
        Err(format!("a provider client was built: {hits:#?}"))
    }
}

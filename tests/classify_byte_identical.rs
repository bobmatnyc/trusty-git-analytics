//! `tga classify` writes byte-identical `classifications` rows (#111).
//!
//! Why: #111 adds rule tracing to the classification cascade so the eval
//! harness can name the rule behind each verdict. A downstream consumer reads
//! `tga.db` after `tga classify --force`, so the trace must never reach the
//! database or change a single verdict. This test pins every row the pipeline
//! writes for a fixed corpus against a golden file captured from the code
//! BEFORE rule tracing landed.
//! What: seeds a fresh database with the recorded trusty-tools commits
//! (`corpus_fixture`, all 3,970 or every fourth) plus a handful of rows that reach the tiers the corpus
//! does not (a user rules file, a JIRA project mapping, a manual override, a
//! merge flag), runs the public `ClassificationPipeline` exactly as
//! `tga classify` does — a default run and then a `--force` re-run — and
//! compares a text dump of every commit's stored verdict to
//! `tests/fixtures/classify_golden.tsv`.
//! Regenerate the golden (only when a verdict change is intended) with
//! `TGA_BLESS_CLASSIFY_GOLDEN=1 cargo test --test classify_byte_identical`.
//! Test: this file.

mod corpus_fixture;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rusqlite::params;
use tga::classify::{ClassificationPipeline, ClassificationStats};
use tga::core::config::Config;
use tga::core::db::Database;

/// Rules layered over the built-ins, so the dump covers a user rules file.
const LAYERED_RULES: &str = r#"
extend_defaults: true
rules:
  - id: fixture-release
    category: release
    keywords: ["ship it:"]
    priority: 120
    confidence: 0.93
  - id: fixture-payments
    category: payments
    patterns: ["(?i)\\bledger\\b"]
    priority: 95
    confidence: 0.81
"#;

/// A custom-only rule set. With the built-in catch-all gone, the weighted-sum
/// tier — which the catch-all otherwise pre-empts — carries most of the corpus.
const CUSTOM_ONLY_RULES: &str = r#"
extend_defaults: false
rules:
  - id: fixture-release
    category: release
    keywords: ["ship it:"]
    priority: 120
    confidence: 0.93
"#;

/// Messages that reach tiers the recorded corpus never does.
const SUPPLEMENTAL: &[(&str, bool)] = &[
    ("ship it: v1.2.3", false),
    ("rebalance the ledger nightly", false),
    ("TQL-1234 null pointer on login", false),
    ("Merge branch 'main' into topic", true),
    ("wip", false),
    ("", false),
    ("   ", false),
    ("manual override target", false),
];

fn golden_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("classify_golden.tsv")
}

/// Write the rules file and a config pointing at it; load it the way `tga` does.
fn fixture_config(dir: &Path, rules_yaml: &str) -> Config {
    let rules = dir.join("rules.yaml");
    std::fs::write(&rules, rules_yaml).expect("write rules");
    let cfg = dir.join("config.yaml");
    let yaml = format!(
        "classification:\n  rules_files:\n    - {}\njira:\n  jira_project_mappings:\n    TQL: bugfix\n",
        rules.display()
    );
    std::fs::write(&cfg, yaml).expect("write config");
    Config::load(&cfg).expect("load config")
}

/// Seed every `stride`-th corpus commit plus the supplemental rows.
fn seed(db: &Database, stride: usize) {
    let conn = db.connection();
    let repos = ["alpha", "beta", "gamma"];
    let mut messages: Vec<(String, bool)> = corpus_fixture::history()
        .into_iter()
        .step_by(stride)
        .map(|c| {
            let merge = c.message.starts_with("Merge ");
            (c.message, merge)
        })
        .collect();
    messages.extend(SUPPLEMENTAL.iter().map(|(m, b)| ((*m).to_string(), *b)));
    for (i, (message, is_merge)) in messages.iter().enumerate() {
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, is_merge) VALUES (?1, 'a', 'a@example.invalid', ?2, ?3, ?4, ?5)",
            params![
                format!("{i:08x}"),
                format!("2024-01-01T00:{:02}:{:02}Z", (i / 60) % 60, i % 60),
                message,
                repos[i % repos.len()],
                i64::from(*is_merge),
            ],
        )
        .expect("insert commit");
    }
    let target = format!("{:08x}", messages.len() - 1);
    let repo = repos[(messages.len() - 1) % repos.len()];
    conn.execute(
        "INSERT INTO classification_overrides (commit_sha, repo_path, work_type, change_type) \
         VALUES (?1, ?2, 'maintenance', 'cleanup')",
        params![target, repo],
    )
    .expect("insert override");
}

/// Every stored verdict, one line per commit in insertion order.
fn dump(db: &Database, stats: &ClassificationStats) -> String {
    let conn = db.connection();
    let mut out = String::new();
    let orphans: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM classifications WHERE id NOT IN \
             (SELECT classification_id FROM commits WHERE classification_id IS NOT NULL)",
            [],
            |r| r.get(0),
        )
        .expect("count orphans");
    let by_method: BTreeMap<_, _> = stats.by_method.iter().collect();
    let by_category: BTreeMap<_, _> = stats.by_category.iter().collect();
    writeln!(
        out,
        "# total={} classified={} orphans={orphans} by_method={by_method:?} by_category={by_category:?}",
        stats.total_commits, stats.classified
    )
    .expect("write");
    let mut stmt = conn
        .prepare(
            "SELECT c.sha, c.repository, c.confidence, c.is_revert, cl.category, \
             cl.subcategory, cl.ticket_id, cl.confidence, cl.method, cl.complexity, \
             cl.top_level_category \
             FROM commits c LEFT JOIN classifications cl ON cl.id = c.classification_id \
             ORDER BY c.id",
        )
        .expect("prepare dump");
    let rows = stmt
        .query_map([], |r| {
            Ok(format!(
                "{}\t{}\t{:?}\t{}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}\t{:?}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<f64>>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<f64>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<i64>>(9)?,
                r.get::<_, Option<String>>(10)?,
            ))
        })
        .expect("query dump");
    for row in rows {
        writeln!(out, "{}", row.expect("row")).expect("write");
    }
    out
}

/// Run `tga classify` then `tga classify --force` over a fresh database and
/// return the dump, asserting the two runs agree.
async fn classify_scenario(name: &str, rules_yaml: &str, stride: usize) -> String {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fixture_config(dir.path(), rules_yaml);
    let mut db = Database::open(&dir.path().join("tga.db")).expect("open db");
    seed(&db, stride);

    let stats = ClassificationPipeline::new(config.clone())
        .run(&mut db)
        .await
        .expect("default classify run");
    let first = dump(&db, &stats);

    let forced_stats = ClassificationPipeline::new(config)
        .with_force(true)
        .run(&mut db)
        .await
        .expect("forced classify run");
    let forced = dump(&db, &forced_stats);
    assert_eq!(
        first, forced,
        "{name}: a --force re-run must reproduce the same rows"
    );
    format!("## scenario {name}\n{first}")
}

/// Why: the #111 rule trace must stay in memory — `tga classify` and
/// `tga classify --force` must write exactly the rows they wrote before it.
/// What: runs two scenarios — built-ins plus a layered rules file over the
/// full corpus, and a custom-only rules file over every fourth commit (the
/// configuration where the weighted-sum tier fires) — and asserts the combined
/// dump equals the golden captured before rule tracing existed.
/// Test: this function.
#[tokio::test]
async fn classify_rows_are_byte_identical_to_golden() {
    let mut actual = classify_scenario("layered", LAYERED_RULES, 1).await;
    actual.push_str(&classify_scenario("custom_only", CUSTOM_ONLY_RULES, 4).await);

    let golden = golden_path();
    if std::env::var_os("TGA_BLESS_CLASSIFY_GOLDEN").is_some() {
        std::fs::write(&golden, &actual).expect("write golden");
    }
    let expected = std::fs::read_to_string(&golden)
        .unwrap_or_else(|e| panic!("read golden {}: {e}", golden.display()));
    if actual != expected {
        let diverged = actual
            .lines()
            .zip(expected.lines())
            .find(|(a, b)| a != b)
            .map(|(a, b)| format!("got:      {a}\nexpected: {b}"))
            .unwrap_or_else(|| "line counts differ".to_string());
        panic!("classifications rows diverged from the golden:\n{diverged}");
    }
}

//! #182: `commits.is_revert` after `tga classify` is the verdict OR the
//! commit-message revert match, whatever tier answered and with or without
//! `--force`. In-memory DB, custom rules, no LLM, no network.

use std::collections::HashMap;
use std::io::Write;

use rusqlite::params;

use super::*;
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::core::config::{ClassificationConfig, Config};
use crate::core::db::Database;

/// A custom-only rules set with no revert rule for `Revert "..."` messages:
/// `fix:` answers `bug_fix`. The one revert-category rule fires only on
/// `undo:`, a prefix `core::revert::is_revert` does not recognize.
const RULES: &str = "extend_defaults: false
rules:
  - id: fix
    category: bug_fix
    keywords: [\"fix:\"]
    priority: 120
    confidence: 0.9
  - id: undo
    category: revert
    keywords: [\"undo:\"]
    priority: 120
    confidence: 0.9
categories:
  - name: internal_tooling
";

/// The repository the repo map sends to `internal_tooling`.
const MAPPED: &str = "mcp-services";
/// A repository the repo map does not name.
const UNMAPPED: &str = "billing-api";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

fn config(rules: &std::path::Path) -> Config {
    Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            repo_categories: [(MAPPED.to_string(), "internal_tooling".to_string())]
                .into_iter()
                .collect::<HashMap<_, _>>(),
            weighted_sum: WeightedSumConfig {
                enabled: false,
                ..WeightedSumConfig::default()
            },
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    }
}

/// Insert an unclassified commit with `is_revert` preset to `is_revert`.
fn insert(db: &Database, sha: &str, repo: &str, message: &str, is_revert: bool) {
    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, is_revert) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, ?3, ?4)",
            params![sha, message, repo, i64::from(is_revert)],
        )
        .expect("insert commit");
}

/// Attach an existing classification row to `sha`, as a prior run would.
fn preclassify(db: &Database, sha: &str, category: &str) {
    db.connection()
        .execute(
            "INSERT INTO classifications (category, subcategory, confidence, method) \
             VALUES (?1, NULL, 0.9, 'exact_rule')",
            [category],
        )
        .expect("insert classification");
    let id = db.connection().last_insert_rowid();
    db.connection()
        .execute(
            "UPDATE commits SET classification_id = ?1 WHERE sha = ?2",
            params![id, sha],
        )
        .expect("link classification");
}

/// `(category, method, is_revert)` stored for `sha`.
fn stored(db: &Database, sha: &str) -> (String, String, i64) {
    db.connection()
        .query_row(
            "SELECT cl.category, cl.method, c.is_revert FROM commits c \
             JOIN classifications cl ON cl.id = c.classification_id WHERE c.sha = ?1",
            [sha],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("classified")
}

async fn run(pipeline: &ClassificationPipeline, db: &mut Database) {
    let engine = pipeline.build_rule_engine().expect("engine");
    pipeline.run_with_engine(db, engine).await.expect("run");
}

/// Why (#182): a repo-map verdict is never a revert category, so before the
/// fix a `Revert "..."` commit in a mapped repo was written `is_revert = 0`.
/// Catches a "verdict only" fix.
/// What: first-time classify of a revert message in a mapped repo; the
/// verdict is `internal_tooling` by `repo_map`, and `is_revert` is 1.
/// Test: this test.
#[tokio::test]
async fn revert_message_sets_is_revert_under_a_repo_map_verdict() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path()));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-revert", MAPPED, "Revert \"x\"", false);

    run(&pipeline, &mut db).await;

    let (category, method, is_revert) = stored(&db, "sha-revert");
    assert_eq!(
        (category.as_str(), method.as_str()),
        ("internal_tooling", "repo_map")
    );
    assert_eq!(is_revert, 1, "a revert message must set is_revert=1");
}

/// Why (#182): the UAT run of `tga classify --force` with no revert rule
/// rewrote 751 rows from `is_revert = 1` to 0. Catches a "verdict only" fix.
/// What: a `Revert "fix: ..."` commit already classified and flagged; a
/// forced re-run answers `bug_fix` from the `fix:` rule, and the flag stays 1.
/// Test: this test.
#[tokio::test]
async fn force_reclassify_keeps_is_revert_without_a_revert_rule() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path())).with_force(true);
    let mut db = Database::open_in_memory().expect("db");
    insert(
        &db,
        "sha-revert",
        UNMAPPED,
        "Revert \"fix: null user\"",
        true,
    );
    preclassify(&db, "sha-revert", "revert");

    run(&pipeline, &mut db).await;

    let (category, _, is_revert) = stored(&db, "sha-revert");
    assert_eq!(category, "bug_fix", "the forced verdict is not a revert");
    assert_eq!(
        is_revert, 1,
        "--force must not clear a revert message's flag"
    );
}

/// Why (#182): the message match must not flag ordinary commits. Catches a
/// fix that sets the flag unconditionally.
/// What: a forced re-run of a `fix:` commit answers `bug_fix`; `is_revert`
/// stays 0.
/// Test: this test.
#[tokio::test]
async fn non_revert_message_with_non_revert_verdict_stays_zero() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path())).with_force(true);
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-fix", UNMAPPED, "fix: null user", false);
    preclassify(&db, "sha-fix", "bug_fix");

    run(&pipeline, &mut db).await;

    let (category, _, is_revert) = stored(&db, "sha-fix");
    assert_eq!(category, "bug_fix");
    assert_eq!(is_revert, 0, "a non-revert commit must stay is_revert=0");
}

/// Why (#182): the verdict path still sets the flag when the message does
/// not match `core::revert::is_revert`. Catches a "message match only" fix.
/// What: an `undo:` commit gets the `revert` category from the `undo` rule,
/// and `is_revert` is 1.
/// Test: this test.
#[tokio::test]
async fn revert_verdict_sets_is_revert_without_a_revert_message() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(rules.path()));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-undo", UNMAPPED, "undo: login form", false);
    assert!(!crate::core::revert::is_revert("undo: login form"));

    run(&pipeline, &mut db).await;

    let (category, _, is_revert) = stored(&db, "sha-undo");
    assert_eq!(category, "revert");
    assert_eq!(is_revert, 1, "a revert verdict must set is_revert=1");
}

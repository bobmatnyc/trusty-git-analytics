//! #167: repo map v2 — floor mode, `<repo>:<prefix>` keys and the
//! unmatched-key warning, end to end against an in-memory DB and a mock LLM.
//! No network.

use std::collections::HashMap;
use std::io::Write;

use rusqlite::params;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::core::config::{
    ClassificationConfig, Config, LlmFallbackScope, RepoMapConfig, RepoMapMode,
};
use crate::core::db::Database;

/// Rules whose confidences sit on both sides of the 0.8 default floor.
const RULES: &str = "extend_defaults: false
rules:
  - id: sec
    category: security
    keywords: [\"security:\"]
    priority: 120
    confidence: 0.8
  - id: flaky
    category: qa
    keywords: [\"flaky:\"]
    priority: 110
    confidence: 0.79
  - id: feat
    category: new_feature
    keywords: [\"feat:\"]
    priority: 100
    confidence: 0.95
categories:
  - name: internal_tooling
  - name: platform_infrastructure
  - name: bug_fix
";

const MAPPED: &str = "acme-tools";
const UNMAPPED: &str = "acme-billing";
const MONO: &str = "acme-mono";
/// A message no rule matches, so the cascade leaves it unanswered.
const NOTHING: &str = "zzz qqq vvv";

fn rules_file() -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(RULES.as_bytes()).expect("write rules");
    f
}

fn config(rules: &std::path::Path, map: &[(&str, &str)], repo_map: RepoMapConfig) -> Config {
    Config {
        classification: Some(ClassificationConfig {
            rules_files: vec![rules.to_path_buf()],
            repo_categories: map
                .iter()
                .map(|(r, c)| (r.to_string(), c.to_string()))
                .collect::<HashMap<_, _>>(),
            repo_map,
            llm_fallback_scope: LlmFallbackScope::LowConfidence,
            // Keep the signal-voting tier out so an unmatched message stays
            // unanswered and is LLM-eligible.
            weighted_sum: WeightedSumConfig {
                enabled: false,
                ..WeightedSumConfig::default()
            },
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    }
}

/// Floor mode with the default exceptions and threshold.
fn floor() -> RepoMapConfig {
    RepoMapConfig {
        mode: RepoMapMode::Floor,
        ..RepoMapConfig::default()
    }
}

fn insert(db: &Database, sha: &str, repo: &str, message: &str) {
    db.connection()
        .execute(
            "INSERT INTO commits \
             (sha, author_name, author_email, timestamp, message, repository, is_merge) \
             VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', ?2, ?3, 0)",
            params![sha, message, repo],
        )
        .expect("insert commit");
}

/// Insert a commit and one `files` row per changed path.
fn insert_with_paths(db: &Database, sha: &str, repo: &str, paths: &[&str]) {
    insert(db, sha, repo, NOTHING);
    let id = db.connection().last_insert_rowid();
    for p in paths {
        db.connection()
            .execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'M')",
                params![id, p],
            )
            .expect("insert file");
    }
}

/// `(category, method, confidence)` stored for `sha`.
fn verdict(db: &Database, sha: &str) -> (String, String, f64) {
    db.connection()
        .query_row(
            "SELECT cl.category, cl.method, cl.confidence FROM commits c \
             JOIN classifications cl ON cl.id = c.classification_id WHERE c.sha = ?1",
            [sha],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("classified")
}

/// `(category, method)` stored for `sha`.
fn cat_method(db: &Database, sha: &str) -> (String, String) {
    let (c, m, _) = verdict(db, sha);
    (c, m)
}

fn pair(category: &str, method: &str) -> (String, String) {
    (category.to_string(), method.to_string())
}

async fn run(pipeline: &ClassificationPipeline, db: &mut Database) -> Result<ClassificationStats> {
    let engine = pipeline.build_rule_engine()?;
    pipeline.run_with_engine(db, engine).await
}

/// A mock chat-completions endpoint answering every call with `category`
/// at `confidence`.
async fn mock_llm(category: &str, confidence: f64) -> MockServer {
    let content = serde_json::json!({
        "category": category,
        "subcategory": null,
        "confidence": confidence,
        "complexity": 2
    })
    .to_string();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"content": content}}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 9}
        })))
        .mount(&server)
        .await;
    server
}

async fn run_with_llm(
    pipeline: &ClassificationPipeline,
    db: &mut Database,
    server: &MockServer,
) -> ClassificationStats {
    let mut engine = pipeline.build_rule_engine().expect("rule engine");
    engine.attach_llm(
        LlmClassifier::new("test-model", Some("sk-test".to_string()))
            .with_endpoint(format!("{}/v1/chat/completions", server.uri())),
    );
    pipeline.run_with_engine(db, engine).await.expect("run")
}

/// Why (#167 floor): an exception verdict is kept at the threshold and
/// floored just below it.
/// What: in a floor-mode mapped repo, a `security` rule hit at 0.8 keeps its
/// verdict and a `qa` rule hit at 0.79 takes the mapped category. The same
/// messages in an unmapped repo keep their rule verdicts.
/// Test: this test.
#[tokio::test]
async fn floor_keeps_an_exception_at_the_threshold_and_floors_it_below() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        floor(),
    ));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-sec", MAPPED, "security: rotate the signing keys");
    insert(&db, "sha-qa", MAPPED, "flaky: retry the login test");
    insert(
        &db,
        "sha-qa-unmapped",
        UNMAPPED,
        "flaky: retry the login test",
    );

    run(&pipeline, &mut db).await.expect("run");

    let (category, method, confidence) = verdict(&db, "sha-sec");
    assert_eq!(pair(&category, &method), pair("security", "exact_rule"));
    assert!((confidence - 0.8).abs() < 1e-9, "{confidence}");
    let (category, method, confidence) = verdict(&db, "sha-qa");
    assert_eq!(
        pair(&category, &method),
        pair("internal_tooling", "repo_map")
    );
    assert!((confidence - 1.0).abs() < 1e-9, "{confidence}");
    assert_eq!(cat_method(&db, "sha-qa-unmapped"), pair("qa", "exact_rule"));
}

/// Why (#167 floor): a non-exception verdict never overrides the floor, and
/// a mapped commit is LLM-eligible again in floor mode.
/// What: the LLM answers `new_feature` at 0.95. A `new_feature` rule hit and
/// an unanswered commit, both in a floor-mode mapped repo, end as the mapped
/// category; the unanswered one reached the LLM (one request).
/// Test: this test.
#[tokio::test]
async fn floor_overrides_a_non_exception_rule_and_llm_verdict() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        floor(),
    ));
    let server = mock_llm("new_feature", 0.95).await;
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-feat", MAPPED, "feat: add the widget");
    insert(&db, "sha-none", MAPPED, NOTHING);

    let stats = run_with_llm(&pipeline, &mut db, &server).await;

    assert_eq!(server.received_requests().await.expect("rec").len(), 1);
    assert_eq!(stats.llm_usage.calls, 1);
    for sha in ["sha-feat", "sha-none"] {
        assert_eq!(
            cat_method(&db, sha),
            pair("internal_tooling", "repo_map"),
            "{sha}"
        );
    }
}

/// Why (#167 floor): an LLM verdict in an exception category at or above
/// the threshold is kept ("qa and bugfix etc inferred").
/// What: the LLM answers `bug_fix` at 0.9 for an unanswered commit in a
/// floor-mode mapped repo; the stored verdict is the LLM's.
/// Test: this test.
#[tokio::test]
async fn floor_keeps_a_high_confidence_llm_exception() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        floor(),
    ));
    let server = mock_llm("bug_fix", 0.9).await;
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-none", MAPPED, NOTHING);

    run_with_llm(&pipeline, &mut db, &server).await;

    assert_eq!(cat_method(&db, "sha-none"), pair("bug_fix", "llm_fallback"));
}

/// Why (#167): the exceptions and the threshold are config values, not
/// constants.
/// What: `exceptions: [new_feature, security]`, `min_confidence: 0.85`. The
/// `new_feature` hit at 0.95 is kept; the `security` hit at 0.8, kept under
/// the defaults, is floored; the `qa` hit is floored (not listed).
/// Test: this test.
#[tokio::test]
async fn exceptions_and_threshold_are_config_values() {
    let rules = rules_file();
    let repo_map = RepoMapConfig {
        mode: RepoMapMode::Floor,
        exceptions: Some(vec!["new_feature".into(), "security".into()]),
        min_confidence: 0.85,
    };
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        repo_map,
    ));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-feat", MAPPED, "feat: add the widget");
    insert(&db, "sha-sec", MAPPED, "security: rotate the signing keys");
    insert(&db, "sha-qa", MAPPED, "flaky: retry the login test");

    run(&pipeline, &mut db).await.expect("run");

    assert_eq!(
        cat_method(&db, "sha-feat"),
        pair("new_feature", "exact_rule")
    );
    for sha in ["sha-sec", "sha-qa"] {
        assert_eq!(
            cat_method(&db, sha),
            pair("internal_tooling", "repo_map"),
            "{sha}"
        );
    }
}

/// Why (#167 criterion 5): with no `repo_map` block the #158 hard override
/// applies exactly as in 10.2.0.
/// What: the same commits under no block and under an explicit
/// `mode: override` store identical rows; a mapped `security` hit at 0.8,
/// which floor mode would keep, is `repo_map`, and an unanswered mapped
/// commit is `repo_map` too.
/// Test: this test.
#[tokio::test]
async fn an_absent_repo_map_block_leaves_output_unchanged() {
    let rules = rules_file();
    let seed = |db: &Database| {
        insert(db, "sha-sec", MAPPED, "security: rotate the signing keys");
        insert(db, "sha-none", MAPPED, NOTHING);
        insert(
            db,
            "sha-unmapped",
            UNMAPPED,
            "security: rotate the signing keys",
        );
    };
    let rows = |db: &Database| -> Vec<(String, String, f64)> {
        ["sha-sec", "sha-none", "sha-unmapped"]
            .iter()
            .map(|s| verdict(db, s))
            .collect()
    };
    let map = [(MAPPED, "internal_tooling")];

    let mut absent_cfg = config(rules.path(), &map, RepoMapConfig::default());
    // No block at all: the section as YAML without `repo_map:` would load.
    if let Some(c) = absent_cfg.classification.as_mut() {
        c.repo_map = serde_yaml::from_str::<ClassificationConfig>("{}")
            .expect("empty section")
            .repo_map;
    }
    let mut absent = Database::open_in_memory().expect("db");
    seed(&absent);
    run(&ClassificationPipeline::new(absent_cfg), &mut absent)
        .await
        .expect("run");

    let explicit_cfg = config(
        rules.path(),
        &map,
        RepoMapConfig {
            mode: RepoMapMode::Override,
            ..RepoMapConfig::default()
        },
    );
    let mut explicit = Database::open_in_memory().expect("db");
    seed(&explicit);
    run(&ClassificationPipeline::new(explicit_cfg), &mut explicit)
        .await
        .expect("run");

    assert_eq!(rows(&absent), rows(&explicit));
    assert_eq!(
        cat_method(&absent, "sha-sec"),
        pair("internal_tooling", "repo_map")
    );
    assert_eq!(
        cat_method(&absent, "sha-none"),
        pair("internal_tooling", "repo_map")
    );
    assert_eq!(
        cat_method(&absent, "sha-unmapped"),
        pair("security", "exact_rule")
    );
}

/// Why (#167 path-prefix keys): a monorepo maps by subdirectory, and the
/// most specific key wins.
/// What: keys `acme-mono`, `acme-mono:services` and
/// `acme-mono:services/qa-harness/`. A commit under
/// `services/qa-harness` gets the longest key; one under `services/api` the
/// shorter one. `services-legacy/` is not under `services` (prefixes match
/// whole path segments), so it falls back to the bare key.
/// Test: this test.
#[tokio::test]
async fn the_longest_matching_prefix_wins() {
    let rules = rules_file();
    let map = [
        (MONO, "internal_tooling"),
        ("acme-mono:services", "platform_infrastructure"),
        ("acme-mono:services/qa-harness/", "qa"),
    ];
    let pipeline =
        ClassificationPipeline::new(config(rules.path(), &map, RepoMapConfig::default()));
    let mut db = Database::open_in_memory().expect("db");
    insert_with_paths(&db, "sha-qa", MONO, &["services/qa-harness/run.rs"]);
    insert_with_paths(&db, "sha-api", MONO, &["services/api/main.rs"]);
    insert_with_paths(&db, "sha-legacy", MONO, &["services-legacy/x.rs"]);

    run(&pipeline, &mut db).await.expect("run");

    assert_eq!(cat_method(&db, "sha-qa"), pair("qa", "repo_map"));
    assert_eq!(
        cat_method(&db, "sha-api"),
        pair("platform_infrastructure", "repo_map")
    );
    assert_eq!(
        cat_method(&db, "sha-legacy"),
        pair("internal_tooling", "repo_map")
    );
}

/// Why (#167 path-prefix keys): the bare `<repo>` key covers every path no
/// prefix key holds, and a repo with only prefix keys maps nothing outside
/// them.
/// What: `acme-mono` has a bare key and a `services` key; a commit under
/// `docs/` and a commit with no stored paths take the bare category.
/// `acme-platform` has only a `services` key; its `docs/` commit stays
/// unmapped.
/// Test: this test.
#[tokio::test]
async fn a_bare_repo_key_is_the_fallback() {
    let rules = rules_file();
    let map = [
        (MONO, "internal_tooling"),
        ("acme-mono:services", "qa"),
        ("acme-platform:services", "qa"),
    ];
    let pipeline =
        ClassificationPipeline::new(config(rules.path(), &map, RepoMapConfig::default()));
    let mut db = Database::open_in_memory().expect("db");
    insert_with_paths(&db, "sha-docs", MONO, &["docs/readme.md"]);
    insert_with_paths(&db, "sha-nopaths", MONO, &[]);
    insert_with_paths(&db, "sha-svc", MONO, &["services/a.rs"]);
    insert_with_paths(&db, "sha-other-docs", "acme-platform", &["docs/readme.md"]);

    run(&pipeline, &mut db).await.expect("run");

    for sha in ["sha-docs", "sha-nopaths"] {
        assert_eq!(
            cat_method(&db, sha),
            pair("internal_tooling", "repo_map"),
            "{sha}"
        );
    }
    assert_eq!(cat_method(&db, "sha-svc"), pair("qa", "repo_map"));
    let (category, method) = cat_method(&db, "sha-other-docs");
    assert_ne!(method, "repo_map");
    assert_eq!(category, "uncategorized");
}

/// Why (#167 criterion 4): a commit whose paths span several prefixes must
/// resolve deterministically. The rule: each path votes for the category of
/// its most specific key (else the bare key, else "unmapped"); the category
/// with the most votes wins; a tie goes to the category holding the most
/// specific (longest) key, then to the alphabetically first category;
/// "unmapped" loses every tie.
/// What: one repo per case so the keys stay independent:
/// majority, two keys of one category pooling votes, a length tie-break,
/// an alphabetical tie-break, an unmapped majority, and an unmapped tie.
/// Test: this test.
#[tokio::test]
async fn a_commit_spanning_prefixes_takes_the_category_most_paths_resolve_to() {
    let rules = rules_file();
    let map = [
        // majority: 2 services vs 1 tests.
        ("r-major:services", "platform_infrastructure"),
        ("r-major:tests", "qa"),
        // pooling: a + b (qa) outvote c (platform).
        ("r-pool:a", "qa"),
        ("r-pool:b", "qa"),
        ("r-pool:c", "platform_infrastructure"),
        // length tie-break: 1 vs 1, `services` is longer than `tests`, and
        // its category sorts later, so a missing length step picks platform.
        ("r-len:services", "qa"),
        ("r-len:tests", "platform_infrastructure"),
        // alphabetical tie-break: equal lengths, `platform_...` < `qa`.
        ("r-alpha:bb", "qa"),
        ("r-alpha:aa", "platform_infrastructure"),
        // unmapped majority: no bare key, 2 docs paths vs 1 services path.
        ("r-unmapped:services", "qa"),
        // unmapped tie: 1 vs 1, the key wins.
        ("r-tie:services", "qa"),
    ];
    let pipeline =
        ClassificationPipeline::new(config(rules.path(), &map, RepoMapConfig::default()));
    let mut db = Database::open_in_memory().expect("db");
    insert_with_paths(
        &db,
        "sha-major",
        "r-major",
        &["services/a.rs", "services/b.rs", "tests/c.rs"],
    );
    insert_with_paths(&db, "sha-pool", "r-pool", &["a/1", "b/1", "c/1"]);
    insert_with_paths(&db, "sha-len", "r-len", &["services/a.rs", "tests/c.rs"]);
    insert_with_paths(&db, "sha-alpha", "r-alpha", &["aa/1", "bb/1"]);
    insert_with_paths(
        &db,
        "sha-unmapped",
        "r-unmapped",
        &["docs/x.md", "docs/y.md", "services/a.rs"],
    );
    insert_with_paths(&db, "sha-tie", "r-tie", &["docs/x.md", "services/a.rs"]);

    run(&pipeline, &mut db).await.expect("run");

    let platform = pair("platform_infrastructure", "repo_map");
    let qa = pair("qa", "repo_map");
    assert_eq!(cat_method(&db, "sha-major"), platform);
    assert_eq!(cat_method(&db, "sha-pool"), qa);
    assert_eq!(cat_method(&db, "sha-len"), qa);
    assert_eq!(cat_method(&db, "sha-alpha"), platform);
    let (category, method) = cat_method(&db, "sha-unmapped");
    assert_ne!(method, "repo_map", "{category}");
    assert_eq!(cat_method(&db, "sha-tie"), qa);
}

/// Why (#167 criterion: unmatched keys): a key that names no stored
/// repository, or a prefix no stored path falls under, is a silent no-op
/// today; tga warns, once per key per run.
/// What: keys for a stored repo, an unstored repo and a stored repo with a
/// prefix no path matches. One classify run warns once for each of the last
/// two and never for the first.
/// Test: this test.
#[tokio::test]
#[tracing_test::traced_test]
async fn an_unmatched_key_warns_once_per_run() {
    let rules = rules_file();
    let map = [
        (MAPPED, "internal_tooling"),
        ("ghost-repo", "qa"),
        ("acme-mono:nowhere", "qa"),
        ("acme-mono:services", "qa"),
    ];
    let pipeline =
        ClassificationPipeline::new(config(rules.path(), &map, RepoMapConfig::default()));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", MAPPED, NOTHING);
    insert(&db, "sha-b", MAPPED, NOTHING);
    insert_with_paths(&db, "sha-c", MONO, &["services/a.rs"]);

    run(&pipeline, &mut db).await.expect("run");

    logs_assert(|lines: &[&str]| {
        let warned = |key: &str| {
            lines
                .iter()
                .filter(|l| l.contains("WARN") && l.contains(&format!("'{key}'")))
                .count()
        };
        for (key, want) in [
            ("ghost-repo", 1),
            ("acme-mono:nowhere", 1),
            (MAPPED, 0),
            ("acme-mono:services", 0),
        ] {
            if warned(key) != want {
                return Err(format!("{key}: {} warnings, want {want}", warned(key)));
            }
        }
        Ok(())
    });
}

/// Why (#167): a key with a `/` where `<repo>:<prefix>` is expected, or an
/// empty side of the `:`, can never match; it fails the run before any
/// write instead of being ignored.
/// What: each malformed key fails the run with an error naming it.
/// Test: this test.
#[tokio::test]
async fn a_malformed_key_is_rejected() {
    let rules = rules_file();
    for key in ["acme-mono/api", "acme-mono:", ":api", "acme-mono:/"] {
        let pipeline = ClassificationPipeline::new(config(
            rules.path(),
            &[(key, "qa")],
            RepoMapConfig::default(),
        ));
        let mut db = Database::open_in_memory().expect("db");
        insert(&db, "sha-a", MONO, NOTHING);
        let msg = run(&pipeline, &mut db)
            .await
            .expect_err("malformed key")
            .to_string();
        assert!(msg.contains(key), "{key}: {msg}");
        assert!(msg.contains("<repo>:<prefix>"), "{key}: {msg}");
    }
}

/// Why (#167): a threshold outside `[0, 1]` would keep every exception or
/// none, silently.
/// What: `min_confidence: 1.5` fails the run with an error naming the key.
/// Test: this test.
#[tokio::test]
async fn an_out_of_range_min_confidence_is_rejected() {
    let rules = rules_file();
    let repo_map = RepoMapConfig {
        min_confidence: 1.5,
        ..floor()
    };
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        repo_map,
    ));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", MAPPED, NOTHING);
    let msg = run(&pipeline, &mut db)
        .await
        .expect_err("out of range")
        .to_string();
    assert!(msg.contains("min_confidence"), "{msg}");
}

/// Why (#167): Jev learns repository names from the map keys to
/// pseudonymize them. A prefix is a path; learning its segments would
/// redact ordinary words such as `api` in every commit message.
/// What: the key `qmono-repo:services/api` gives Jev `qmono-repo` and no
/// part of the prefix.
/// Test: this test.
#[test]
fn a_prefix_key_gives_jev_only_its_repository_name() {
    let rules = rules_file();
    let cfg = config(
        rules.path(),
        &[("qmono-repo:services/api", "qa")],
        RepoMapConfig::default(),
    );
    let names = super::pipeline_jev::known_names(&cfg);
    assert!(
        names.repos.iter().any(|n| n == "qmono-repo"),
        "{:?}",
        names.repos
    );
    for part in ["api", "services", "services/api", "qmono-repo:services/api"] {
        assert!(
            !names.repos.iter().any(|n| n == part),
            "{part}: {:?}",
            names.repos
        );
    }
}

/// Why (#167 review HIGH): the default exception `bug_fix` must match the
/// built-in ruleset, whose `cc-fix` rule emits `bugfix`; the taxonomy
/// names `bug_fix` as its alias.
/// What: no rules file (built-in rules), floor mode with the default
/// exceptions. A `fix: ...` commit in a mapped repo keeps `bugfix`; a
/// `feat: ...` commit takes the mapped category.
/// Test: this test.
#[tokio::test]
async fn floor_keeps_a_default_ruleset_bug_fix() {
    let config = Config {
        classification: Some(ClassificationConfig {
            repo_categories: [(MAPPED.to_string(), "tooling".to_string())].into(),
            repo_map: floor(),
            ..ClassificationConfig::default()
        }),
        ..Config::default()
    };
    let pipeline = ClassificationPipeline::new(config);
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-fix", MAPPED, "fix: handle the empty token");
    insert(&db, "sha-feat", MAPPED, "feat: add the export button");

    run(&pipeline, &mut db).await.expect("run");

    assert_eq!(cat_method(&db, "sha-fix").0, "bugfix");
    assert_eq!(cat_method(&db, "sha-feat"), pair("tooling", "repo_map"));
}

/// Why (#167 review): an exception the operator wrote that no category
/// matches would silently never keep anything (fail-open).
/// What: `exceptions: [qa, bugfixes]` under a ruleset without `bugfixes`
/// fails the run with an error naming it.
/// Test: this test.
#[tokio::test]
async fn a_user_written_unknown_exception_is_rejected() {
    let rules = rules_file();
    let repo_map: RepoMapConfig =
        serde_yaml::from_str("mode: floor\nexceptions: [qa, bugfixes]\n").expect("block");
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        repo_map,
    ));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", MAPPED, NOTHING);
    let msg = run(&pipeline, &mut db)
        .await
        .expect_err("unknown exception")
        .to_string();
    assert!(msg.contains("bugfixes"), "{msg}");
    assert!(msg.contains("exceptions"), "{msg}");
}

/// Why (#167 review): `repositories[].name` may hold a `/` (`org/repo`); a
/// bare key equal to such a name maps that repository. A `/` key matching
/// no configured name is still refused, and the error says so.
/// What: the configured name `acme-org/widget` maps; `acme-org/gadget`
/// fails naming `repositories[].name`.
/// Test: this test.
#[tokio::test]
async fn a_bare_key_naming_a_configured_slash_repository_maps_it() {
    let rules = rules_file();
    let with_repo = |key: &str| {
        let mut cfg = config(rules.path(), &[(key, "qa")], RepoMapConfig::default());
        cfg.repositories = vec![crate::core::config::RepositoryConfig {
            path: "/nonexistent/widget".into(),
            name: Some("acme-org/widget".into()),
            ..Default::default()
        }];
        ClassificationPipeline::new(cfg)
    };
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", "acme-org/widget", NOTHING);
    run(&with_repo("acme-org/widget"), &mut db)
        .await
        .expect("configured name");
    assert_eq!(cat_method(&db, "sha-a"), pair("qa", "repo_map"));

    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", "acme-org/widget", NOTHING);
    let msg = run(&with_repo("acme-org/gadget"), &mut db)
        .await
        .expect_err("unconfigured slash key")
        .to_string();
    assert!(msg.contains("acme-org/gadget"), "{msg}");
    assert!(msg.contains("repositories[].name"), "{msg}");
}

/// Why (#167 review): floor mode and prefix keys combine: the floor is the
/// category the commit's paths resolve to.
/// What: floor mode, keys `acme-mono` and `acme-mono:services`. Under
/// `services/`, a `security` hit at 0.8 is kept and a `new_feature` hit
/// takes the prefix category; under `docs/`, a `qa` hit at 0.79 takes the
/// bare category.
/// Test: this test.
#[tokio::test]
async fn floor_mode_applies_to_a_prefix_key() {
    let rules = rules_file();
    let map = [
        (MONO, "internal_tooling"),
        ("acme-mono:services", "platform_infrastructure"),
    ];
    let pipeline = ClassificationPipeline::new(config(rules.path(), &map, floor()));
    let mut db = Database::open_in_memory().expect("db");
    let with = |sha: &str, msg: &str, path: &str| {
        insert(&db, sha, MONO, msg);
        let id = db.connection().last_insert_rowid();
        db.connection()
            .execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'M')",
                params![id, path],
            )
            .expect("insert file");
    };
    with(
        "sha-sec",
        "security: rotate the signing keys",
        "services/a.rs",
    );
    with("sha-feat", "feat: add the widget", "services/b.rs");
    with("sha-qa", "flaky: retry the login test", "docs/x.md");

    run(&pipeline, &mut db).await.expect("run");

    assert_eq!(cat_method(&db, "sha-sec"), pair("security", "exact_rule"));
    assert_eq!(
        cat_method(&db, "sha-feat"),
        pair("platform_infrastructure", "repo_map")
    );
    assert_eq!(
        cat_method(&db, "sha-qa"),
        pair("internal_tooling", "repo_map")
    );
}

/// Why (#167): `r:api` and `r:api/` normalize to one key; two categories
/// for it would make the map depend on key order.
/// What: the pair fails the run with an error naming both keys.
/// Test: this test.
#[tokio::test]
async fn duplicate_keys_naming_one_prefix_are_rejected() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[("r:api", "qa"), ("r:api/", "internal_tooling")],
        RepoMapConfig::default(),
    ));
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-a", "r", NOTHING);
    let msg = run(&pipeline, &mut db)
        .await
        .expect_err("duplicate")
        .to_string();
    assert!(msg.contains("'r:api'") && msg.contains("'r:api/'"), "{msg}");
}

/// Why (#167 review): the complexity backfill never revisits a `repo_map`
/// row, so a floored LLM verdict must keep the LLM's complexity score.
/// What: the LLM answers `new_feature` at 0.95 with complexity 2 for an
/// unanswered commit in a floor-mode mapped repo; the stored `repo_map`
/// row carries complexity 2.
/// Test: this test.
#[tokio::test]
async fn floor_keeps_the_llm_complexity_when_it_replaces_an_llm_verdict() {
    let rules = rules_file();
    let pipeline = ClassificationPipeline::new(config(
        rules.path(),
        &[(MAPPED, "internal_tooling")],
        floor(),
    ));
    let server = mock_llm("new_feature", 0.95).await;
    let mut db = Database::open_in_memory().expect("db");
    insert(&db, "sha-none", MAPPED, NOTHING);

    run_with_llm(&pipeline, &mut db, &server).await;

    assert_eq!(
        cat_method(&db, "sha-none"),
        pair("internal_tooling", "repo_map")
    );
    let complexity: Option<i64> = db
        .connection()
        .query_row(
            "SELECT cl.complexity FROM commits c JOIN classifications cl \
             ON cl.id = c.classification_id WHERE c.sha = 'sha-none'",
            [],
            |r| r.get(0),
        )
        .expect("complexity");
    assert_eq!(complexity, Some(2));
}

//! #111: regression tests for the code-critic's third review round (on
//! 7297e87), one or more per finding. Each fails against 7297e87. No
//! network; every name is synthetic.

use std::time::{Duration, Instant};

use rusqlite::params;
use serde_json::Value;
use wiremock::MockServer;

use super::jev::JevClassifier;
use super::jev_budget::JevBudget;
use super::jev_obfuscate::{KnownNames, Obfuscator, NAME_MATCHER_SIZE_LIMIT};
use super::jev_tests::{bodies, prep, probs, reply, run_pipeline_with, server_with, TEST_KEY};
use crate::classify::rules::CategoryDef;
use crate::core::config::JevOptions;
use crate::core::db::Database;

fn assert_absent(out: &str, secrets: &[&str]) {
    let lower = out.to_lowercase();
    for s in secrets {
        assert!(
            !lower.contains(&s.to_lowercase()),
            "{s:?} survived in {out:?}"
        );
    }
}

/// Why (round 3, item 2): pull-request authors and reviewers and issue
/// assignees can be people who never authored a commit, under a login that
/// differs from every git address.
/// What: `pull_requests.author`, `pr_reviewers.reviewer_id` and
/// `display_name`, `linear_issues.assignee` and
/// `fact_ticket_transitions.author` each name someone the run's one commit
/// mentions in prose; none may be sent.
#[tokio::test]
async fn pr_and_issue_people_are_redacted() {
    let server = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let setup = |db: &Database| {
        let c = db.connection();
        c.execute(
            "INSERT INTO pull_requests (pr_number, title, author, state, created_at) \
             VALUES (7, 'x', 'jroe-acme', 'merged', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("pull request");
        let pr = c.last_insert_rowid();
        c.execute(
            "INSERT INTO pr_reviewers (pr_id, provider, reviewer_id, display_name) \
             VALUES (?1, 'github', 'qrev-7', 'Quinta Revelle')",
            params![pr],
        )
        .expect("reviewer");
        c.execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, assignee, \
             fetched_at) VALUES ('QX-1', 'x', 'done', 't', 'QX', 'qassignee', \
             '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("linear issue");
        c.execute(
            "INSERT INTO fact_ticket_transitions (ticket_key, project_key, to_status, \
             transitioned_at, author, synced_at) VALUES ('QJ-1', 'QJ', 'Done', \
             '2024-01-01T00:00:00Z', 'Jorvik Tallow', 0)",
            [],
        )
        .expect("jira transition");
    };
    let rows = [(
        "zzz thanks jroe-acme, qrev-7, Revelle, qassignee and Tallow",
        "b",
        "b@x",
        false,
    )];
    let (_, _db) = run_pipeline_with(&server, &rows, setup).await;
    let sent = bodies(&server).await;
    assert_eq!(sent.len(), 1);
    let message = sent[0]["state"]["commit"]["message"].to_string();
    assert_absent(
        &message,
        &["jroe-acme", "qrev-7", "revelle", "qassignee", "tallow"],
    );
}

fn vocab_categories() -> Vec<CategoryDef> {
    let mut a = CategoryDef::new("bugfix");
    a.description = Some("A bug fix; tested with chrome and firefox.".into());
    let mut b = CategoryDef::new("refactor");
    b.description = Some("Moved to a new API without behaviour change.".into());
    let mut c = CategoryDef::new("ci");
    c.description = Some("Release and deploy tooling.".into());
    vec![a, b, c]
}

/// The `category` criteria of the one request `server` received.
async fn criteria(server: &MockServer) -> Value {
    let sent = bodies(server).await;
    assert_eq!(sent.len(), 1);
    sent[0]["questions"]["category"]["criteria"].clone()
}

/// Why (round 3, item 3): learned names must never erase classification
/// vocabulary: weak trailers (`Tested with:`, `Related to:`, `Moved to:`)
/// whose values are ordinary words, and single-token bot logins
/// (`semantic-release-bot`, `deploy-bot`), must not rewrite the category
/// text or ordinary prose.
/// What: one classifier learns nothing; another learns from those trailers
/// and logins. Both send the same criteria, and the second keeps `deploy`
/// and `chrome` in prose while a login-shaped `Paired-with:` value is still
/// learned.
#[tokio::test]
async fn category_text_survives_learning() {
    let build = |server: &MockServer| {
        JevClassifier::from_options(Some(TEST_KEY.into()), &super::jev_tests::obfuscating())
            .expect("keyed")
            .with_test_endpoint(&format!("{}/v1/systemone", server.uri()))
            .with_context(vocab_categories(), KnownNames::default())
            .expect("context")
    };
    let plain = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = build(&plain);
    prep(&jev);
    jev.classify("zzz").await;
    let want = criteria(&plain).await;

    let learned = server_with(reply("CAT_1", probs("CAT_1"), 150)).await;
    let jev = build(&learned);
    let messages = [
        "fix: flaky test\n\nTested with: chrome and firefox",
        "chore: tidy\n\nRelated to: bug fix",
        "refactor: client\n\nMoved to: new API",
        "feat: sync\n\nPaired-with: jroe-acme",
    ];
    let people = ["semantic-release-bot".to_string(), "deploy-bot".to_string()];
    jev.prepare(&messages, &people).expect("prepare");
    jev.classify("switch the deploy step; tested in chrome; ask jroe-acme")
        .await;
    assert_eq!(criteria(&learned).await, want);
    let message = bodies(&learned).await[0]["state"]["commit"]["message"].to_string();
    assert!(
        message.contains("switch the deploy step; tested in chrome"),
        "{message}"
    );
    assert_absent(&message, &["jroe-acme"]);
}

/// Fifty thousand synthetic `First Last` names, all distinct.
fn fifty_thousand_names() -> Vec<String> {
    const SYL: [&str; 24] = [
        "qa", "ze", "vo", "xu", "ky", "jo", "wi", "pe", "ru", "ta", "no", "mi", "lu", "fe", "ga",
        "ho", "bi", "de", "si", "cu", "ra", "ve", "yo", "zi",
    ];
    let word = |n: usize| {
        let mut w = String::from("Q");
        let mut n = n;
        for _ in 0..4 {
            w.push_str(SYL[n % SYL.len()]);
            n /= SYL.len();
        }
        w
    };
    (0..50_000)
        .map(|i| format!("{} {}", word(i), word(i * 7 + 13)))
        .collect()
}

/// Why (round 3, item 4): a large organisation's history names tens of
/// thousands of people; the matcher must build in bounded time under a
/// documented cap, or the run sends nothing.
/// What: 50,000 two-word names (140,254 entries with parts) fit the
/// default 64 MiB cap (gate B 2: the regex matcher fitted about 18,600 and
/// needed 256 MiB for these), building and redacting one message in under
/// 120 s in the unoptimised test build.
#[test]
fn fifty_thousand_names_build_under_the_default_limit() {
    let names = KnownNames {
        people: fifty_thousand_names(),
        ..KnownNames::default()
    };
    let start = Instant::now();
    let mut o = Obfuscator::with_size_limit(&names, NAME_MATCHER_SIZE_LIMIT)
        .expect("50k names fit the default cap");
    let built = start.elapsed();
    let who = names.people[31_337].clone();
    let out = o
        .obfuscate(&format!("thanks {who} for the fix"))
        .expect("obfuscates");
    let elapsed = start.elapsed();
    eprintln!("50k-name matcher: built in {built:?}, first message in {elapsed:?}");
    assert!(elapsed < Duration::from_secs(120), "took {elapsed:?}");
    assert_absent(out.as_str(), &[who.as_str()]);
}

/// Why (round 3, item 4): the matcher cap is an operator knob under
/// `llm.jev.name_matcher_bytes`, and the classifier honours it.
/// What: the key parses; a 16-byte cap makes attaching a roster fail.
#[test]
fn name_matcher_bytes_is_configurable() {
    let opts: JevOptions =
        serde_yaml::from_str("name_matcher_bytes: 16\nobfuscate: true\n").expect("key parses");
    let names = KnownNames {
        people: vec!["Qirin Vossberg".into(), "jroe-acme".into()],
        ..KnownNames::default()
    };
    let attached = JevClassifier::from_options(Some(TEST_KEY.into()), &opts)
        .expect("keyed")
        .with_context(vocab_categories(), names);
    assert!(attached.is_err(), "a 16-byte cap must not fit a roster");
}

/// Why (round 3, item 5): the spend line must show what the cap counted,
/// not only reported usage, or a retried run looks cheaper than it was.
#[test]
#[tracing_test::traced_test]
fn spend_log_shows_what_the_cap_counted() {
    let budget = JevBudget::new(1e-9);
    assert!(!budget.reserve(1_000));
    assert!(logs_contain("spent_usd"), "no counted spend in the cap log");
}

/// Why (round 3, item 6): a `Test:` pointer that names a missing test
/// misleads a reader and survives a green suite.
/// What: every `jev_*tests::name` pointer in the Jev sources names a
/// function that exists in that test file.
#[test]
fn jev_test_pointers_resolve() {
    let sources = [
        ("jev.rs", include_str!("jev.rs")),
        ("jev_budget.rs", include_str!("jev_budget.rs")),
        ("jev_error.rs", include_str!("jev_error.rs")),
        ("jev_glue.rs", include_str!("jev_glue.rs")),
        ("jev_matcher.rs", include_str!("jev_matcher.rs")),
        ("jev_names.rs", include_str!("jev_names.rs")),
        ("jev_obfuscate.rs", include_str!("jev_obfuscate.rs")),
        ("jev_patterns.rs", include_str!("jev_patterns.rs")),
        ("jev_response.rs", include_str!("jev_response.rs")),
        ("jev_trailers.rs", include_str!("jev_trailers.rs")),
        ("jev_tickets.rs", include_str!("jev_tickets.rs")),
        ("jev_vocab.rs", include_str!("jev_vocab.rs")),
        ("pipeline_jev.rs", include_str!("../pipeline_jev.rs")),
    ];
    let tests = [
        ("jev_tests", include_str!("jev_tests.rs")),
        (
            "jev_obfuscate_tests",
            include_str!("jev_obfuscate_tests.rs"),
        ),
        (
            "jev_redaction_tests",
            include_str!("jev_redaction_tests.rs"),
        ),
        ("jev_review_tests", include_str!("jev_review_tests.rs")),
        ("jev_round3_tests", include_str!("jev_round3_tests.rs")),
        ("jev_round4_tests", include_str!("jev_round4_tests.rs")),
        ("jev_round5_tests", include_str!("jev_round5_tests.rs")),
        ("jev_gateb_tests", include_str!("jev_gateb_tests.rs")),
        ("jev_gateb2_tests", include_str!("jev_gateb2_tests.rs")),
        (
            "jev_text_mode_tests",
            include_str!("jev_text_mode_tests.rs"),
        ),
    ];
    let pointer = regex::Regex::new(r"(jev_[a-z0-9_]*tests)::([a-z0-9_]+)").expect("regex");
    let mut stale = Vec::new();
    for (file, text) in sources {
        for c in pointer.captures_iter(text) {
            let body = tests.iter().find(|(m, _)| *m == &c[1]).map(|(_, b)| *b);
            let found = body.is_some_and(|b| b.contains(&format!("fn {}(", &c[2])));
            if !found {
                stale.push(format!("{file}: {}::{}", &c[1], &c[2]));
            }
        }
    }
    assert!(stale.is_empty(), "stale Test: pointers: {stale:#?}");
}

/// Why (round 3, item 7; gate B re-run): the residual-risk note is gone —
/// no two-label suffix is exempt any more — and the host rule that
/// replaced it names every former exemption.
#[test]
fn residual_risk_note_lists_every_exempt_suffix() {
    let doc = include_str!("../../../docs/requirements/configuration.md");
    assert!(
        !doc.contains("**Residual risk:**"),
        "stale residual-risk note"
    );
    let rule = doc
        .split("**Hosts on word suffixes:**")
        .nth(1)
        .and_then(|rest| rest.split("\n- **").next())
        .expect("host rule");
    for s in [
        "dev", "app", "in", "it", "at", "be", "me", "us", "no", "so", "to", "info", "tech", "site",
        "online", "global", "test",
    ] {
        assert!(
            rule.contains(&format!("`{s}`")),
            "`{s}` missing from: {rule}"
        );
    }
}

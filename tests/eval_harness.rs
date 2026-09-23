//! End-to-end tests for the classifier precision harness (#111).

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use rusqlite::params;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::eval::{self, SampleParams, SampleRecord, ScoreParams};

const MESSAGES: [&str; 8] = [
    "feat: add export endpoint",
    "fix: handle empty payload",
    "docs: update readme",
    "Merge branch 'main' into topic",
    "tweaked the thing a bit",
    "wip",
    "",
    "PROJ-12 adjust pricing screen",
];

/// A database with 240 commits over 6 repos and 9 authors, spread over 20 weeks.
fn seed_db(path: &Path) {
    let db = Database::open(path).expect("open db");
    let conn = db.connection();
    for i in 0..240usize {
        let day = 1 + (i % 140);
        let ts = (chrono::NaiveDate::from_ymd_opt(2025, 1, 1).expect("date")
            + chrono::Duration::days(day as i64))
        .format("%Y-%m-%dT10:00:00Z")
        .to_string();
        let msg = format!("{} {}", MESSAGES[i % MESSAGES.len()], i)
            .trim()
            .to_string();
        let msg = if i % MESSAGES.len() == 6 {
            String::new()
        } else {
            msg
        };
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
             files_changed, insertions, deletions, is_merge) \
             VALUES (?1, 'n', ?2, ?3, ?4, ?5, 1, 3, 1, ?6)",
            params![
                format!("{:040x}", i * 104_729 + 1),
                format!("dev{}@example.com", i % 9),
                ts,
                msg,
                format!("org/repo{}", i % 6),
                i64::from(i % MESSAGES.len() == 3),
            ],
        )
        .expect("insert commit");
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'modified')",
            params![id, format!("src/mod{}.rs", i % 4)],
        )
        .expect("insert file");
    }
}

fn read_sample(path: &Path) -> Vec<SampleRecord> {
    fs::read_to_string(path)
        .expect("read sample")
        .lines()
        .map(|l| serde_json::from_str(l).expect("parse record"))
        .collect()
}

fn sample_params(db: &Path, out: &Path, seed: u64) -> SampleParams {
    SampleParams {
        db: db.to_path_buf(),
        config: Config::default(),
        weeks: 26,
        size: 60,
        seed,
        cap: 3,
        salt: Some("fixed-salt".into()),
        out: out.to_path_buf(),
    }
}

/// Why: the harness must never be able to alter the operator's database.
/// What: a write through the handle `open_eval_db` returns fails, while a
/// read succeeds.
/// Test: this function.
#[test]
fn eval_handle_rejects_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    seed_db(&db);
    let conn = eval::open_eval_db(&db).expect("open read-only");
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0))
        .expect("read");
    assert_eq!(n, 240);
    let write = conn.execute("DELETE FROM commits", []);
    assert!(write.is_err(), "write through the eval handle succeeded");
    let still: i64 = conn
        .query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0))
        .expect("read");
    assert_eq!(still, 240);
}

/// Why: sample → label → score is the harness's whole workflow; a rerun with
/// the same seed must reproduce the sample exactly.
/// What: draws twice with one seed (byte-identical sample.jsonl) and once
/// with another (different); checks caps, hidden predictions in labels.csv,
/// strata totals; then labels every row with its prediction and scores it.
/// Test: this function.
#[test]
fn sample_then_score_end_to_end() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    seed_db(&db);
    let (a, b, c) = (
        dir.path().join("a"),
        dir.path().join("b"),
        dir.path().join("c"),
    );
    let summary = eval::run_sample(&sample_params(&db, &a, 11)).expect("sample a");
    eval::run_sample(&sample_params(&db, &b, 11)).expect("sample b");
    eval::run_sample(&sample_params(&db, &c, 12)).expect("sample c");
    let bytes = |d: &Path| fs::read(d.join("sample.jsonl")).expect("read");
    assert_eq!(bytes(&a), bytes(&b), "same seed drew a different sample");
    assert_ne!(bytes(&a), bytes(&c), "different seed drew the same sample");

    let records = read_sample(&a.join("sample.jsonl"));
    assert!(!records.is_empty() && records.len() <= 60);
    let strata = &summary.strata;
    let pop: u64 = strata.strata.values().map(|c| c.population).sum();
    assert_eq!(pop, strata.population);
    let sampled: u64 = strata.strata.values().map(|c| c.sampled).sum();
    assert_eq!(sampled as usize, records.len());
    let mut per_repo: HashMap<(String, String), u64> = HashMap::new();
    let mut per_author: HashMap<(String, String), u64> = HashMap::new();
    for r in &records {
        let s = r.stratum.as_str().to_string();
        *per_repo.entry((s.clone(), r.repo.clone())).or_default() += 1;
        *per_author.entry((s, r.author_hash.clone())).or_default() += 1;
        assert!(!r.author_hash.contains('@'));
        assert!(!r.rule_id.is_empty());
        assert_eq!(r.paths.len(), 1);
    }
    assert!(per_repo.values().all(|&n| n <= 3), "repo cap exceeded");
    assert!(per_author.values().all(|&n| n <= 3), "author cap exceeded");

    let labels_text = fs::read_to_string(a.join("labels.csv")).expect("labels");
    let header = labels_text.lines().next().unwrap_or_default();
    assert_eq!(
        header,
        "sha,repo,subject,body_excerpt,paths_excerpt,pr_title,issue_type,label,note"
    );
    for r in &records {
        assert!(labels_text.contains(&r.sha));
    }

    // Label every sampled commit with its own prediction.
    let mut w = csv::Writer::from_path(a.join("rater.csv")).expect("csv");
    w.write_record(["sha", "label", "note"]).expect("hdr");
    for r in &records {
        w.write_record([r.sha.as_str(), r.predicted_category.as_str(), ""])
            .expect("row");
    }
    w.flush().expect("flush");
    let report = eval::run_score(&ScoreParams {
        sample: a.join("sample.jsonl"),
        strata: None,
        labels: vec![a.join("rater.csv")],
        adjudicated: None,
        categories: None,
        out: a.join("report"),
    })
    .expect("score");
    assert_eq!(report.scored as usize, records.len());
    assert!(report.per_rule.iter().all(|r| r.precision == Some(1.0)));
    let acc = report.weighted_accuracy.expect("accuracy");
    assert!((acc.estimate - 1.0).abs() < 1e-9);
    assert!(a.join("report/report.md").exists());
}

fn record(sha: &str, stratum: &str, rule: &str, cat: &str, conf: f64, weight: f64) -> String {
    let method = match stratum {
        "catch_all" => "catch_all",
        "unknown" => "unclassified",
        _ => "exact",
    };
    serde_json::json!({
        "sha": sha, "repo": "r", "date": "2025-01-01T00:00:00Z", "author_hash": "h",
        "subject": "s", "body": "", "paths": [],
        "diffstat": {"files": 1, "insertions": 1, "deletions": 0},
        "pr_title": null, "ticket_id": null, "issue_type": null,
        "stratum": stratum, "method": method, "rule_id": rule,
        "predicted_category": cat, "confidence": conf, "weight": weight
    })
    .to_string()
}

fn write_labels(path: &Path, rows: &[(&str, &str)]) {
    let mut text = String::from("sha,label,note\n");
    for (sha, label) in rows {
        text.push_str(&format!("{sha},{label},\n"));
    }
    fs::write(path, text).expect("write labels");
}

/// Why: the scorer's numbers are the harness's product.
/// What: a hand-built 10-commit sample over three strata (population 40 /
/// 50 / 10), two raters and one adjudication; asserts precision per rule,
/// Wilson bounds, exclusion of unclear/mixed, the unresolved disagreement,
/// stratum-weighted accuracy 0.55, abstention 0.6, kappa 0.7297, the
/// coverage curve and the confusion matrix. An unknown label is rejected.
/// Test: this function.
#[test]
fn score_computes_expected_metrics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let mut lines = Vec::new();
    for i in 1..=4 {
        lines.push(record(
            &format!("s{i}"),
            "exact",
            "builtin#a",
            "feature",
            0.95,
            10.0,
        ));
    }
    for i in 5..=8 {
        lines.push(record(
            &format!("s{i}"),
            "catch_all",
            "catch_all",
            "maintenance",
            0.3,
            12.5,
        ));
    }
    for i in 9..=10 {
        lines.push(record(
            &format!("s{i}"),
            "unknown",
            "unclassified",
            "uncategorized",
            0.0,
            5.0,
        ));
    }
    fs::write(d.join("sample.jsonl"), lines.join("\n") + "\n").expect("sample");
    let strata = serde_json::json!({
        "seed": 1, "weeks": 26, "window_start": "a", "window_end": "b",
        "requested_size": 10, "cap": 5, "population": 100,
        "strata": {
            "exact": {"population": 40, "sampled": 4},
            "catch_all": {"population": 50, "sampled": 4},
            "unknown": {"population": 10, "sampled": 2}
        },
        "categories": ["feature", "bugfix", "maintenance"]
    });
    fs::write(d.join("strata.json"), strata.to_string()).expect("strata");
    let a = [
        ("s1", "feature"),
        ("s2", "feature"),
        ("s3", "feature"),
        ("s4", "bugfix"),
        ("s5", "maintenance"),
        ("s6", "bugfix"),
        ("s7", "unclear"),
        ("s8", "maintenance"),
        ("s9", "feature"),
        ("s10", "mixed"),
    ];
    let b = [
        ("s1", "feature"),
        ("s2", "feature"),
        ("s3", "bugfix"),
        ("s4", "bugfix"),
        ("s5", "maintenance"),
        ("s6", "bugfix"),
        ("s7", "unclear"),
        ("s8", "feature"),
        ("s9", "feature"),
        ("s10", "mixed"),
    ];
    write_labels(&d.join("a.csv"), &a);
    write_labels(&d.join("b.csv"), &b);
    write_labels(&d.join("adj.csv"), &[("s3", "feature")]);

    let params = ScoreParams {
        sample: d.join("sample.jsonl"),
        strata: None,
        labels: vec![d.join("a.csv"), d.join("b.csv")],
        adjudicated: Some(d.join("adj.csv")),
        categories: None,
        out: d.join("out"),
    };
    let r = eval::run_score(&params).expect("score");

    assert_eq!((r.sample_size, r.labelled, r.scored), (10, 9, 7));
    assert_eq!((r.unclear, r.mixed, r.unresolved_disagreements), (1, 1, 1));
    let rule = |k: &str| r.per_rule.iter().find(|x| x.key == k).expect("rule row");
    let exact = rule("builtin#a");
    assert_eq!((exact.n, exact.correct), (4, 3));
    assert!((exact.ci_low.unwrap_or(0.0) - 0.3006).abs() < 1e-3);
    assert!((exact.ci_high.unwrap_or(0.0) - 0.9544).abs() < 1e-3);
    let catch_all = rule("catch_all");
    assert_eq!(
        (catch_all.n, catch_all.correct, catch_all.excluded),
        (2, 1, 1)
    );
    assert_eq!(rule("unclassified").precision, Some(0.0));
    let method = r
        .per_method
        .iter()
        .find(|x| x.key == "exact")
        .expect("method");
    assert_eq!(method.correct, 3);

    let acc = r.weighted_accuracy.as_ref().expect("accuracy");
    assert!((acc.estimate - 0.55).abs() < 1e-9, "{}", acc.estimate);
    assert!((r.abstention.share - 0.6).abs() < 1e-9);
    let kappa = r.kappa.as_ref().and_then(|k| k.kappa).expect("kappa");
    assert!((kappa - 0.72973).abs() < 1e-4, "{kappa}");

    let point = |t: f64| {
        r.coverage_curve
            .iter()
            .find(|p| (p.threshold - t).abs() < 1e-9)
            .expect("point")
    };
    assert!((point(0.95).coverage - 0.4).abs() < 1e-9);
    assert_eq!(point(0.95).precision, Some(0.75));
    assert!((point(0.3).coverage - 0.9).abs() < 1e-9);
    assert!((point(0.3).precision.unwrap_or(0.0) - 42.5 / 65.0).abs() < 1e-9);
    assert_eq!(point(0.0).n, 7);

    assert_eq!(r.confusion["feature"]["feature"], 3);
    assert_eq!(r.confusion["feature"]["bugfix"], 1);
    assert_eq!(r.confusion["maintenance"]["unclear"], 1);
    assert_eq!(r.confusion["uncategorized"]["mixed"], 1);
    let md = fs::read_to_string(d.join("out/report.md")).expect("md");
    assert!(md.contains("Stratum-weighted accuracy: **55.0%**"));
    assert!(d.join("out/report.json").exists());

    write_labels(&d.join("bad.csv"), &[("s1", "featur")]);
    let err = eval::run_score(&ScoreParams {
        labels: vec![d.join("bad.csv")],
        adjudicated: None,
        ..params
    })
    .expect_err("typo label accepted");
    assert!(err.to_string().contains("featur"));
}

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
        db: None,
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
        "predicted_category": cat, "confidence": conf, "weight": weight,
        "is_merge": false
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
/// Wilson bounds, exclusion of unclear/mixed, the unresolved disagreement
/// (counted, but s8 is still scored with the first rater's label, #111),
/// stratum-weighted accuracy 0.6333, abstention 0.6, kappa 0.7297, the
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
        db: None,
        out: d.join("out"),
    };
    let r = eval::run_score(&params).expect("score");

    assert_eq!((r.sample_size, r.labelled, r.scored), (10, 10, 8));
    assert_eq!((r.unclear, r.mixed, r.unresolved_disagreements), (1, 1, 1));
    let rule = |k: &str| r.per_rule.iter().find(|x| x.key == k).expect("rule row");
    let exact = rule("builtin#a");
    assert_eq!((exact.n, exact.correct), (4, 3));
    assert!((exact.ci_low.unwrap_or(0.0) - 0.3006).abs() < 1e-3);
    assert!((exact.ci_high.unwrap_or(0.0) - 0.9544).abs() < 1e-3);
    let catch_all = rule("catch_all");
    assert_eq!(
        (catch_all.n, catch_all.correct, catch_all.excluded),
        (3, 2, 1)
    );
    assert_eq!(rule("unclassified").precision, Some(0.0));
    let method = r
        .per_method
        .iter()
        .find(|x| x.key == "exact")
        .expect("method");
    assert_eq!(method.correct, 3);

    let acc = r.weighted_accuracy.as_ref().expect("accuracy");
    // 0.4 · 3/4 + 0.5 · 2/3 + 0.1 · 0
    let expected = 0.4 * 0.75 + 0.5 * 2.0 / 3.0;
    assert!((acc.estimate - expected).abs() < 1e-9, "{}", acc.estimate);
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
    assert!((point(0.3).precision.unwrap_or(0.0) - 55.0 / 77.5).abs() < 1e-9);
    assert_eq!(point(0.0).n, 8);

    assert_eq!(r.confusion["feature"]["feature"], 3);
    assert_eq!(r.confusion["feature"]["bugfix"], 1);
    assert_eq!(r.confusion["maintenance"]["unclear"], 1);
    assert_eq!(r.confusion["uncategorized"]["mixed"], 1);
    let md = fs::read_to_string(d.join("out/report.md")).expect("md");
    assert!(md.contains("Stratum-weighted accuracy: **63.3%**"));
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

/// Why: the rater sheet must name nobody, while the private sample keeps the
/// full body for the scorer.
/// What: commits whose bodies carry identity trailers and an address; the
/// sheet has neither, sample.jsonl has both.
/// Test: this function.
#[test]
fn label_sheet_hides_trailers_and_emails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    {
        let db = Database::open(&db).expect("open db");
        for i in 0..12 {
            db.connection()
                .execute(
                    "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                     repository) VALUES (?1, 'n', 'a@example.com', '2025-03-01T00:00:00Z', ?2, ?3)",
                    params![
                        format!("{i:040x}"),
                        format!(
                            "fix: bug {i} for ops@example.com\n\nBody text.\n\
                             Co-Authored-By: Jane Doe <jane@example.com>\n\
                             Signed-off-by: Joe <joe@example.org>"
                        ),
                        format!("org/r{i}"),
                    ],
                )
                .expect("insert");
        }
    }
    let out = dir.path().join("s");
    eval::run_sample(&sample_params(&db, &out, 3)).expect("sample");
    let sheet = fs::read_to_string(out.join("labels.csv")).expect("labels");
    assert!(!sheet.contains('@'), "address on the sheet:\n{sheet}");
    assert!(!sheet.to_lowercase().contains("co-authored-by"));
    assert!(sheet.contains("<email>") && sheet.contains("Body text."));
    let sample = fs::read_to_string(out.join("sample.jsonl")).expect("sample");
    assert!(sample.contains("Co-Authored-By: Jane Doe <jane@example.com>"));
}

/// SHAs drawn by seed 11 on `seed_db`, recorded when merge commits left the
/// population (#111); the redaction change must not move them.
const GOLDEN_SEED_11: &[&str] = &[
    "0000000000000000000000000000000000e61e11",
    "0000000000000000000000000000000000365553",
    "0000000000000000000000000000000000000001",
    "00000000000000000000000000000000017457c2",
    "0000000000000000000000000000000000d95549",
    "000000000000000000000000000000000172bea9",
    "0000000000000000000000000000000000265a59",
    "0000000000000000000000000000000000e7b72a",
    "0000000000000000000000000000000000a7cb42",
    "000000000000000000000000000000000034bc3a",
    "0000000000000000000000000000000000332321",
    "00000000000000000000000000000000013f9b89",
    "0000000000000000000000000000000000e95043",
    "0000000000000000000000000000000000ce259a",
    "000000000000000000000000000000000001991a",
    "0000000000000000000000000000000000c15cd2",
    "0000000000000000000000000000000000418502",
    "00000000000000000000000000000000014dfd6a",
    "0000000000000000000000000000000000949e16",
    "00000000000000000000000000000000012470e0",
    "00000000000000000000000000000000011fa595",
    "0000000000000000000000000000000000318a08",
    "00000000000000000000000000000000009304fd",
    "0000000000000000000000000000000000531915",
    "00000000000000000000000000000000002cbebd",
    "0000000000000000000000000000000000caf368",
    "0000000000000000000000000000000000d2f0e5",
    "00000000000000000000000000000000001ff5f5",
    "00000000000000000000000000000000003b209e",
    "0000000000000000000000000000000000be2aa0",
    "00000000000000000000000000000000015461ce",
    "00000000000000000000000000000000013ad03e",
    "0000000000000000000000000000000001579400",
    "0000000000000000000000000000000000863c35",
    "000000000000000000000000000000000079736d",
    "000000000000000000000000000000000046504d",
    "0000000000000000000000000000000000896e67",
    "000000000000000000000000000000000063140f",
    "000000000000000000000000000000000096372f",
    "00000000000000000000000000000000002ff0ef",
    "00000000000000000000000000000000007ca59f",
    "0000000000000000000000000000000001160eff",
    "000000000000000000000000000000000049827f",
    "00000000000000000000000000000000016f8c77",
    "0000000000000000000000000000000000232827",
];

/// Why: redacting the label sheet must not change which commits are sampled.
/// What: draws seed 11 on the fixture database and compares the SHAs, in
/// sample.jsonl order, with the list recorded before the change.
/// Test: this function.
#[test]
fn redaction_leaves_the_sample_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    seed_db(&db);
    let out = dir.path().join("s");
    eval::run_sample(&sample_params(&db, &out, 11)).expect("sample");
    let shas: Vec<String> = read_sample(&out.join("sample.jsonl"))
        .into_iter()
        .map(|r| r.sha)
        .collect();
    assert_eq!(shas, GOLDEN_SEED_11);
}

/// One source stratum: label, sampled rows, population, predicted category,
/// confidence.
type Spec<'a> = (&'a str, usize, u64, &'a str, f64);

/// Write a source `sample.jsonl` + `strata.json` whose rows carry the source
/// allocation's weight (population ÷ rows); returns (sha, stratum, predicted).
fn write_source(dir: &Path, spec: &[Spec]) -> Vec<(String, String, String)> {
    let (mut lines, mut rows, mut strata) = (Vec::new(), Vec::new(), serde_json::Map::new());
    for (s, n, pop, cat, conf) in spec {
        for i in 0..*n {
            let sha = format!("{s}-{i:04}");
            let mut rec: serde_json::Value =
                serde_json::from_str(&record(&sha, s, &format!("rule_{s}"), cat, *conf, 0.0))
                    .expect("record");
            rec["weight"] = serde_json::json!(*pop as f64 / *n as f64);
            rec["subject"] = serde_json::json!(format!("change {i} for ops@example.com"));
            rec["body"] = serde_json::json!("Body.\nCo-authored-by: Jane <jane@example.com>");
            lines.push(rec.to_string());
            rows.push((sha, s.to_string(), cat.to_string()));
        }
        strata.insert(
            s.to_string(),
            serde_json::json!({"population": pop, "sampled": n}),
        );
    }
    fs::write(dir.join("sample.jsonl"), lines.join("\n") + "\n").expect("sample");
    let population: u64 = spec.iter().map(|s| s.2).sum();
    let summary = serde_json::json!({
        "seed": 1, "weeks": 26, "window_start": "a", "window_end": "b",
        "requested_size": rows.len(), "cap": 5, "population": population,
        "strata": strata, "categories": ["feature", "bugfix", "maintenance"]
    });
    fs::write(dir.join("strata.json"), summary.to_string()).expect("strata");
    rows
}

fn subsample(from: &Path, size: usize, seed: u64, out: &Path) -> eval::Result<()> {
    eval::run_subsample(&eval::SubsampleParams {
        from: from.join("sample.jsonl"),
        strata: None,
        size,
        seed,
        db: None,
        out: out.to_path_buf(),
    })
    .map(|_| ())
}

fn score(
    sample: &Path,
    labels: &[&Path],
    adjudicated: Option<&Path>,
) -> eval::Result<eval::ScoreReport> {
    eval::run_score(&ScoreParams {
        sample: sample.join("sample.jsonl"),
        strata: None,
        labels: labels.iter().map(|p| p.to_path_buf()).collect(),
        adjudicated: adjudicated.map(Path::to_path_buf),
        categories: None,
        db: None,
        out: sample.join("report"),
    })
}

const FOUR_STRATA: [Spec<'static>; 4] = [
    ("regex_high", 212, 5000, "feature", 0.95),
    ("regex_other", 85, 900, "bugfix", 0.8),
    ("weighted_sum", 37, 120, "maintenance", 0.5),
    ("unknown", 66, 2000, "uncategorized", 0.0),
];

/// Why: the subset a second rater labels must keep the sample's stratum mix
/// and be redrawable from its seed alone (#111).
/// What: 212/85/37/66 subsampled to 100 gives the largest-remainder counts
/// 53/21/9/17 in sample.jsonl and strata.json, with weight = population ÷
/// subset rows; the same seed gives byte-identical files, another seed a
/// different subset. The sheet keeps the columns and redaction of
/// `tga eval sample`, and a rerun into the same directory is refused.
/// Test: this function.
#[test]
fn subsample_is_proportional_and_seeded() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    write_source(d, &FOUR_STRATA);
    let (a, b, c) = (d.join("a"), d.join("b"), d.join("c"));
    subsample(d, 100, 7, &a).expect("subsample a");
    subsample(d, 100, 7, &b).expect("subsample b");
    subsample(d, 100, 8, &c).expect("subsample c");

    let rows = read_sample(&a.join("sample.jsonl"));
    assert_eq!(rows.len(), 100);
    let strata: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(a.join("strata.json")).expect("strata"))
            .expect("json");
    for ((s, _, pop, _, _), want) in FOUR_STRATA.iter().zip([53u64, 21, 9, 17]) {
        let got: Vec<_> = rows.iter().filter(|r| r.stratum.as_str() == *s).collect();
        assert_eq!(got.len() as u64, want, "{s}");
        assert_eq!(strata["strata"][s]["sampled"], want, "{s}");
        assert_eq!(strata["strata"][s]["population"], *pop, "{s}");
        assert!(got
            .iter()
            .all(|r| (r.weight - *pop as f64 / want as f64).abs() < 1e-9));
    }
    assert_eq!(strata["subsample"]["seed"], 7);
    assert_eq!(strata["subsample"]["source_size"], 400);

    for f in ["sample.jsonl", "labels.csv", "strata.json"] {
        let read = |p: &Path| fs::read(p.join(f)).expect("read");
        assert_eq!(read(&a), read(&b), "same seed, different {f}");
    }
    assert_ne!(
        fs::read(a.join("sample.jsonl")).expect("a"),
        fs::read(c.join("sample.jsonl")).expect("c"),
        "different seed drew the same subset"
    );

    let sheet = fs::read_to_string(a.join("labels.csv")).expect("labels");
    assert_eq!(
        sheet.lines().next().unwrap_or_default(),
        "sha,repo,subject,body_excerpt,paths_excerpt,pr_title,issue_type,label,note"
    );
    assert_eq!(sheet.lines().count(), 101);
    assert!(rows.iter().all(|r| sheet.contains(&r.sha)));
    assert!(!sheet.contains('@') && !sheet.to_lowercase().contains("co-authored-by"));
    let sheet_order: Vec<&str> = sheet
        .lines()
        .skip(1)
        .map(|l| &l[..l.find(',').unwrap_or(0)])
        .collect();
    let sample_order: Vec<&str> = rows.iter().map(|r| r.sha.as_str()).collect();
    assert_ne!(
        sheet_order, sample_order,
        "sheet rows follow the stratum order"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode(&a), 0o700);
        for f in ["sample.jsonl", "labels.csv", "strata.json"] {
            assert_eq!(mode(&a.join(f)), 0o600, "{f}");
        }
    }
    let err = subsample(d, 100, 9, &a).expect_err("overwrote a subset");
    assert!(err.to_string().contains("already exists"));
    assert!(subsample(d, 401, 7, &d.join("too-big")).is_err());
}

/// Why: a stratum's weight is its population over the rows labelled in it,
/// not over the source allocation, or a subset or partly filled sheet scores
/// wrong (#111).
/// What: the source allocates 300 rows to a stratum of 600 (weight 2) and 100
/// to one of 400 (weight 4). The rater labels 50 of each, all correct in the
/// first and all wrong in the second, and leaves the other 300 rows blank.
/// Weighted accuracy and the coverage curve's precision over every row must
/// be 600/1000 = 0.6; the stored weights would give 100/300. Scoring a
/// subsample of the same source labelled the same way also gives 0.6.
/// Test: this function.
#[test]
fn score_weights_by_labelled_rows() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let rows = write_source(
        d,
        &[
            ("exact", 300, 600, "feature", 0.9),
            ("catch_all", 100, 400, "maintenance", 0.3),
        ],
    );
    let label = |stratum: &str| {
        if stratum == "exact" {
            "feature"
        } else {
            "bugfix"
        }
    };
    let mut text = String::from("sha,label,note\n");
    for (i, (sha, s, _)) in rows.iter().enumerate() {
        let filled = if s == "exact" { i < 50 } else { i >= 350 };
        text.push_str(&format!("{sha},{},\n", if filled { label(s) } else { "" }));
    }
    fs::write(d.join("rater.csv"), text).expect("labels");
    let r = score(d, &[&d.join("rater.csv")], None).expect("score");
    assert_eq!((r.labelled, r.scored), (100, 100));
    let acc = r.weighted_accuracy.expect("accuracy").estimate;
    assert!((acc - 0.6).abs() < 1e-9, "{acc}");
    let low = r.coverage_curve.first().expect("curve");
    assert!((low.threshold - 0.3).abs() < 1e-9 && (low.coverage - 1.0).abs() < 1e-9);
    let p = low.precision.expect("precision");
    assert!((p - 0.6).abs() < 1e-9, "coverage precision {p}, want 0.6");
    let high = r.coverage_curve.last().expect("curve");
    assert!((high.coverage - 0.6).abs() < 1e-9, "{}", high.coverage);

    let sub = d.join("sub");
    subsample(d, 100, 3, &sub).expect("subsample");
    let mut text = String::from("sha,label,note\n");
    for rec in read_sample(&sub.join("sample.jsonl")) {
        text.push_str(&format!("{},{},\n", rec.sha, label(rec.stratum.as_str())));
    }
    fs::write(sub.join("rater.csv"), text).expect("labels");
    let r = score(&sub, &[&sub.join("rater.csv")], None).expect("score subset");
    assert_eq!(r.scored, 100);
    let acc = r.weighted_accuracy.expect("accuracy").estimate;
    assert!((acc - 0.6).abs() < 1e-9, "{acc}");
    let p = r
        .coverage_curve
        .first()
        .and_then(|c| c.precision)
        .expect("precision");
    assert!((p - 0.6).abs() < 1e-9, "{p}");
}

/// Why: the owner labels 100 rows of a sample a second rater labelled in
/// full; agreement is measured on the overlap and precision on the owner's
/// rows alone (#111).
/// What: rater 1's sheet lists 120 rows, 100 labelled (all correct) and 20
/// blank; rater 2 labels all 400, agreeing on 80 of rater 1's rows and wrong
/// everywhere else. Kappa reports n = 100 (blank rows are unlabelled, not
/// errors); precision uses rater 1 only (100 scored, accuracy 1.0); the 20
/// disagreements are counted. An adjudicated SHA rater 1 left blank is
/// refused.
/// Test: this function.
#[test]
fn score_pairs_a_subset_rater_with_a_full_rater() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let rows = write_source(
        d,
        &[
            ("exact", 200, 800, "feature", 0.9),
            ("catch_all", 200, 200, "maintenance", 0.3),
        ],
    );
    // Rater 1: every other row of the first 240 — 120 rows over both strata.
    let mine: Vec<&(String, String, String)> = rows.iter().step_by(2).take(120).collect();
    let mut one = String::from("sha,label,note\n");
    for (i, (sha, _, pred)) in mine.iter().enumerate() {
        one.push_str(&format!(
            "{sha},{},\n",
            if i < 100 { pred.as_str() } else { "" }
        ));
    }
    fs::write(d.join("rater1.csv"), one).expect("rater1");
    let mut two = String::from("sha,label,note\n");
    for (sha, _, pred) in &rows {
        let pos = mine.iter().position(|m| &m.0 == sha);
        let agree = pos.is_some_and(|p| p < 80);
        two.push_str(&format!(
            "{sha},{},\n",
            if agree { pred.as_str() } else { "bugfix" }
        ));
    }
    fs::write(d.join("rater2.csv"), two).expect("rater2");

    let (r1, r2) = (d.join("rater1.csv"), d.join("rater2.csv"));
    let r = score(d, &[&r1, &r2], None).expect("score");
    assert_eq!(r.kappa.as_ref().map(|k| k.n), Some(100));
    assert_eq!((r.sample_size, r.labelled, r.scored), (400, 100, 100));
    assert_eq!(r.unresolved_disagreements, 20);
    assert_eq!(r.scored_rater, "rater1.csv");
    let acc = r.weighted_accuracy.expect("accuracy").estimate;
    assert!((acc - 1.0).abs() < 1e-9, "{acc}");

    let blank = &mine[110].0;
    write_labels(&d.join("adj.csv"), &[(blank.as_str(), "feature")]);
    let err = score(d, &[&r1, &r2], Some(&d.join("adj.csv"))).expect_err("adjudicated a blank row");
    assert!(err.to_string().contains("leaves blank"));
}

/// Why: #111 — a commit with 2+ parents is a merge and never enters the eval.
/// What: `seed_db` marks every eighth commit (30 of 240) as a merge; a sample
/// sized to take the whole window draws none of them, flags every row
/// `is_merge: false`, and records the 30 in `merges_excluded`.
/// Test: this function.
#[test]
fn sample_never_draws_a_merge() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    seed_db(&db);
    let out = dir.path().join("s");
    let params = SampleParams {
        size: 400,
        cap: 400,
        ..sample_params(&db, &out, 5)
    };
    let summary = eval::run_sample(&params).expect("sample");
    assert_eq!(summary.strata.merges_excluded, 30);
    assert_eq!(summary.strata.population, 210);
    let merges: Vec<String> = (0..240usize)
        .filter(|i| i % MESSAGES.len() == 3)
        .map(|i| format!("{:040x}", i * 104_729 + 1))
        .collect();
    let rows = read_sample(&out.join("sample.jsonl"));
    assert_eq!(rows.len(), 210);
    assert!(rows.iter().all(|r| r.is_merge == Some(false)));
    assert!(rows.iter().all(|r| !merges.contains(&r.sha)));
}

/// Why: #111 scheme v2 — `release_merge` is a valid label that scores as no
/// answer, like `unclear`, and the report counts it on its own.
/// What: three rows labelled correct, `release_merge` and `unclear`: one is
/// scored, both others are counted per label and as the rule's no-answer
/// rows, and report.md shows `release_merge 1`.
/// Test: this function.
#[test]
fn score_counts_release_merge_as_no_answer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let rows = write_source(d, &[("exact", 3, 30, "feature", 0.9)]);
    let labels = [
        (rows[0].0.as_str(), "feature"),
        (rows[1].0.as_str(), "release_merge"),
        (rows[2].0.as_str(), "unclear"),
    ];
    write_labels(&d.join("rater.csv"), &labels);
    let r = score(d, &[&d.join("rater.csv")], None).expect("score");
    assert_eq!((r.labelled, r.scored), (3, 1));
    assert_eq!((r.release_merge, r.unclear, r.mixed), (1, 1, 0));
    let rule = &r.per_rule[0];
    assert_eq!((rule.n, rule.correct, rule.excluded), (1, 1, 2));
    let md = fs::read_to_string(d.join("report/report.md")).expect("md");
    assert!(md.contains("release_merge 1"), "{md}");
}

/// Write a sample in the pre-#111 format (no `is_merge` on any row) whose
/// SHAs are `sha`, and a database holding `in_db` with their merge flags.
fn legacy_sample_and_db(dir: &Path, shas: &[&str], in_db: &[(&str, bool)]) {
    let lines: Vec<String> = shas
        .iter()
        .map(|sha| {
            let mut rec: serde_json::Value =
                serde_json::from_str(&record(sha, "exact", "rule_a", "feature", 0.9, 10.0))
                    .expect("record");
            rec.as_object_mut().expect("object").remove("is_merge");
            rec.to_string()
        })
        .collect();
    fs::write(dir.join("sample.jsonl"), lines.join("\n") + "\n").expect("sample");
    let strata = serde_json::json!({
        "seed": 1, "weeks": 26, "window_start": "2025-01-01T00:00:00+00:00",
        "window_end": "2025-06-01T00:00:00+00:00",
        "requested_size": shas.len(), "cap": 5, "population": 30,
        "strata": {"exact": {"population": 30, "sampled": shas.len()}},
        "categories": ["feature", "bugfix"]
    });
    fs::write(dir.join("strata.json"), strata.to_string()).expect("strata");
    let db = Database::open(&dir.join("tga.db")).expect("open db");
    for (sha, is_merge) in in_db {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, is_merge) VALUES (?1, 'n', 'a@example.com', \
                 '2025-03-01T00:00:00Z', 'm', 'r', ?2)",
                params![sha, i64::from(*is_merge)],
            )
            .expect("insert");
    }
}

/// Why: #111 — the 400-row sample drawn before the merge rule has no merge
/// flag; scoring it must still exclude its merges, resolved by SHA.
/// What: a legacy sample of three rows, of which `m1` is a merge in the
/// database and `q1` a squash commit (one parent). The merge's wrong label is
/// dropped with it: 1 row excluded, 2 scored, precision 1.0, and report.md
/// says so. A row whose SHA is missing from the database is an error.
/// Test: this function.
#[test]
fn score_excludes_merges_resolved_from_the_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let flags = [("m1", true), ("q1", false), ("n1", false)];
    legacy_sample_and_db(d, &["m1", "q1", "n1"], &flags);
    write_labels(
        &d.join("rater.csv"),
        &[("m1", "bugfix"), ("q1", "feature"), ("n1", "feature")],
    );
    let params = ScoreParams {
        sample: d.join("sample.jsonl"),
        strata: None,
        labels: vec![d.join("rater.csv")],
        adjudicated: None,
        categories: None,
        db: Some(d.join("tga.db")),
        out: d.join("report"),
    };
    let r = eval::run_score(&params).expect("score");
    assert_eq!(r.merges_excluded, 1);
    assert_eq!((r.sample_size, r.labelled, r.scored), (3, 2, 2));
    assert_eq!(r.per_rule[0].precision, Some(1.0));
    let md = fs::read_to_string(d.join("report/report.md")).expect("md");
    assert!(md.contains("1 rows excluded as merges"), "{md}");

    let gap = d.join("gap");
    fs::create_dir_all(&gap).expect("mkdir");
    legacy_sample_and_db(&gap, &["m1", "x9"], &[("m1", true)]);
    write_labels(&gap.join("rater.csv"), &[("x9", "feature")]);
    let err = eval::run_score(&ScoreParams {
        sample: gap.join("sample.jsonl"),
        labels: vec![gap.join("rater.csv")],
        db: Some(gap.join("tga.db")),
        out: gap.join("report"),
        ..params
    })
    .expect_err("a row with unknown merge status was scored");
    assert!(err.to_string().contains("merge status unknown"), "{err}");
}

/// Why: #111 fail-closed rule — a row whose merge status cannot be
/// determined must never be scored as a non-merge. Before the rule, `tga eval
/// score` scored a flagless sample with its merges in it and exited 0.
/// What: runs the binary on a legacy sample (no `is_merge`) without `--db`;
/// it must exit non-zero, name `--db`, and write no report.
/// Test: this function.
#[test]
fn score_refuses_rows_with_unknown_merge_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    legacy_sample_and_db(d, &["m1", "q1"], &[]);
    write_labels(
        &d.join("rater.csv"),
        &[("m1", "feature"), ("q1", "feature")],
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_tga"))
        .current_dir(d)
        .args(["eval", "score", "--sample"])
        .arg(d.join("sample.jsonl"))
        .arg("--labels")
        .arg(d.join("rater.csv"))
        .arg("--out")
        .arg(d.join("report"))
        .output()
        .expect("run tga");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "scored rows of unknown merge status");
    assert!(stderr.contains("--db"), "{stderr}");
    assert!(!d.join("report/report.json").exists());
}

/// Why: #111 — v2 labels are accepted whenever the config's rules file names
/// the categories; tga hardcodes no category list.
/// What: a rules file (`extend_defaults: false`) with `internal_tooling` and
/// `data_science` rules; a label of `data_science`, which neither the sample
/// nor the built-in taxonomy holds, is accepted once the config's categories
/// are passed, and rejected without them.
/// Test: this function.
#[test]
fn score_accepts_labels_named_by_the_rules_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let rules = d.join("rules.yaml");
    fs::write(
        &rules,
        "extend_defaults: false\nrules:\n  - id: tooling\n    category: internal_tooling\n    \
         keywords: [\"tooling:\"]\n  - id: ds\n    category: data_science\n    keywords: [\"model:\"]\n",
    )
    .expect("rules");
    let cfg = d.join("config.yaml");
    fs::write(
        &cfg,
        format!(
            "classification:\n  rules_files:\n    - {}\n",
            rules.display()
        ),
    )
    .expect("config");
    let config = Config::load(&cfg).expect("load config");
    let rows = write_source(d, &[("exact", 2, 20, "internal_tooling", 0.9)]);
    write_labels(
        &d.join("rater.csv"),
        &[
            (rows[0].0.as_str(), "internal_tooling"),
            (rows[1].0.as_str(), "data_science"),
        ],
    );
    let params = ScoreParams {
        sample: d.join("sample.jsonl"),
        strata: None,
        labels: vec![d.join("rater.csv")],
        adjudicated: None,
        categories: Some(eval::config_categories(&config).expect("categories")),
        db: None,
        out: d.join("report"),
    };
    let r = eval::run_score(&params).expect("v2 label rejected");
    assert_eq!((r.scored, r.per_rule[0].correct), (2, 1));
    let err = eval::run_score(&ScoreParams {
        categories: None,
        ..params
    })
    .expect_err("an unknown label was accepted");
    assert!(err.to_string().contains("data_science"));
}

/// Why: #111 — a `strata.json` written before merges were excluded counts
/// them in its stratum populations; weighting by those would still weigh
/// merges.
/// What: stratum `exact` (population 600) has 4 correct rows; `catch_all`
/// (population 400) has 4 rows, 2 of them merges and 2 wrong. Its population
/// scales to 400 · 2/4 = 200, so weighted accuracy is 600/800 = 0.75, not
/// the unscaled 600/1000 = 0.6, and still covers the whole population.
/// Test: this function.
#[test]
fn score_weights_strata_without_their_merges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let rows = write_source(
        d,
        &[
            ("exact", 4, 600, "feature", 0.9),
            ("catch_all", 4, 400, "maintenance", 0.3),
        ],
    );
    let merges = ["catch_all-0000", "catch_all-0001"];
    let text = fs::read_to_string(d.join("sample.jsonl")).expect("sample");
    let lines: Vec<String> = text
        .lines()
        .map(|l| {
            let mut rec: serde_json::Value = serde_json::from_str(l).expect("record");
            let is_merge = merges.contains(&rec["sha"].as_str().unwrap_or(""));
            rec["is_merge"] = serde_json::json!(is_merge);
            rec.to_string()
        })
        .collect();
    fs::write(d.join("sample.jsonl"), lines.join("\n") + "\n").expect("sample");
    let labels: Vec<(&str, &str)> = rows
        .iter()
        .map(|(sha, s, _)| {
            let label = match s.as_str() {
                "exact" => "feature",
                _ if merges.contains(&sha.as_str()) => "maintenance",
                _ => "bugfix",
            };
            (sha.as_str(), label)
        })
        .collect();
    write_labels(&d.join("rater.csv"), &labels);

    let r = score(d, &[&d.join("rater.csv")], None).expect("score");
    assert_eq!((r.merges_excluded, r.scored), (2, 6));
    let acc = r.weighted_accuracy.expect("accuracy");
    assert!((acc.estimate - 0.75).abs() < 1e-9, "{}", acc.estimate);
    assert!((acc.population_covered - 1.0).abs() < 1e-9);
}

/// Why: #111 — a subset drawn from a sample written before merges were
/// excluded must not hand a rater merge rows, and must refuse rows whose
/// merge status it cannot resolve, as `score` does.
/// What: a legacy four-row sample where `m1` is a merge in the database.
/// With `--db`, a 3-row subset holds exactly the three non-merge rows, all
/// written `is_merge: false`, and the stratum population drops from 30 to 22
/// (30 − round(30 · 1/4)); a 4-row subset is refused. Without `--db` the
/// draw is refused.
/// Test: this function.
#[test]
fn subsample_drops_merges_resolved_from_the_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let flags = [("m1", true), ("q1", false), ("n1", false), ("p1", false)];
    legacy_sample_and_db(d, &["m1", "q1", "n1", "p1"], &flags);
    let params = |size: usize, db: Option<&Path>, out: &str| eval::SubsampleParams {
        from: d.join("sample.jsonl"),
        strata: None,
        size,
        seed: 4,
        db: db.map(Path::to_path_buf),
        out: d.join(out),
    };
    let db = d.join("tga.db");
    let summary = eval::run_subsample(&params(3, Some(&db), "sub")).expect("subsample");
    let rows = read_sample(&d.join("sub/sample.jsonl"));
    let mut shas: Vec<&str> = rows.iter().map(|r| r.sha.as_str()).collect();
    shas.sort_unstable();
    assert_eq!(shas, ["n1", "p1", "q1"]);
    assert!(rows.iter().all(|r| r.is_merge == Some(false)));
    assert_eq!(summary.strata.population_of(eval::Stratum::Exact), 22);

    let err = eval::run_subsample(&params(4, Some(&db), "big")).expect_err("drew a merge");
    assert!(err.to_string().contains("non-merge rows"), "{err}");
    let err = eval::run_subsample(&params(3, None, "nodb")).expect_err("unknown status drawn");
    assert!(err.to_string().contains("--db"), "{err}");
}

/// Why: #111 — the per-stratum merge estimate is biased by the draw's caps,
/// so with `--db` the report gives the window's exact merge count beside it.
/// What: a legacy four-row sample with one merge (`m1`) scales the stratum
/// by 30 · 1/4 → 8 estimated merges. The database holds `m1`, a second merge
/// in the window and one before it: the exact count is 2, and report.md
/// labels the population estimated and prints both numbers.
/// Test: this function.
#[test]
fn score_reports_exact_window_merges() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let flags = [
        ("m1", true),
        ("q1", false),
        ("n1", false),
        ("p1", false),
        ("m2", true),
    ];
    legacy_sample_and_db(d, &["m1", "q1", "n1", "p1"], &flags);
    Database::open(&d.join("tga.db"))
        .expect("open db")
        .connection()
        .execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
             repository, is_merge) VALUES ('m0', 'n', 'a@example.com', \
             '2024-06-01T00:00:00Z', 'm', 'r', 1)",
            [],
        )
        .expect("insert out-of-window merge");
    write_labels(&d.join("rater.csv"), &[("q1", "feature")]);
    let r = eval::run_score(&ScoreParams {
        sample: d.join("sample.jsonl"),
        strata: None,
        labels: vec![d.join("rater.csv")],
        adjudicated: None,
        categories: None,
        db: Some(d.join("tga.db")),
        out: d.join("report"),
    })
    .expect("score");
    assert_eq!(r.window_merges_estimated, 8);
    assert_eq!(r.window_merges_exact, Some(2));
    let md = fs::read_to_string(d.join("report/report.md")).expect("md");
    assert!(
        md.contains("population 22 (estimated, merges removed)"),
        "{md}"
    );
    assert!(md.contains("exact from the database: 2"), "{md}");
}

/// Write a scheme-v2-style `rules.yaml` (`extend_defaults: false`, one rule
/// sending `fix:` to `defect`) into `dir`, and a `config.yaml` beside it that
/// names the rules file by a RELATIVE path and turns the weighted-sum tier off.
fn v2_config(dir: &Path) -> std::path::PathBuf {
    fs::write(
        dir.join("rules.yaml"),
        "extend_defaults: false\nrules:\n  - id: v2-defect\n    category: defect\n    \
         keywords: [\"fix:\"]\n",
    )
    .expect("rules");
    let cfg = dir.join("config.yaml");
    fs::write(
        &cfg,
        "classification:\n  rules_file: rules.yaml\n  weighted_sum:\n    enabled: false\n",
    )
    .expect("config");
    cfg
}

fn blake3_of(path: &Path) -> String {
    blake3::hash(&fs::read(path).expect("read"))
        .to_hex()
        .to_string()
}

/// Why: #111 — a sample labelled under old rules must be scorable under new
/// rules without redrawing it.
/// What: draws a sample under the built-in rules, re-predicts it under a v2
/// config. Every row keeps its SHA, order, stratum, weight and text; `fix:`
/// rows now predict `defect` by the exact tier, the rest abstain
/// (`uncategorized`/`unclassified`) and are counted as abstentions. The
/// database file is byte-identical afterwards. The provenance names the tga
/// version and the config and rules-file BLAKE3 hashes. Scoring the result
/// treats an abstaining row as `tga eval score` always has: a wrong
/// prediction. A second run into the same --out is refused.
/// Test: this function.
#[test]
fn repredict_keeps_rows_and_follows_the_new_rules() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let db = d.join("tga.db");
    seed_db(&db);
    let a = d.join("a");
    eval::run_sample(&sample_params(&db, &a, 11)).expect("sample");
    let before = read_sample(&a.join("sample.jsonl"));
    let cfg_path = v2_config(d);
    let config = Config::load(&cfg_path).expect("load config");
    let db_bytes = fs::read(&db).expect("db bytes");
    let out = a.join("sample.v2.jsonl");
    let params = eval::RepredictParams {
        sample: a.join("sample.jsonl"),
        db: db.clone(),
        config: config.clone(),
        config_path: cfg_path.clone(),
        out: out.clone(),
    };
    let summary = eval::run_repredict(&params).expect("repredict");
    assert_eq!(fs::read(&db).expect("db bytes"), db_bytes, "db was written");

    let after = read_sample(&out);
    assert_eq!(after.len(), before.len());
    let mut fixes = 0;
    for (b, r) in before.iter().zip(&after) {
        assert_eq!(
            (&r.sha, r.stratum, r.weight, &r.subject, &r.paths),
            (&b.sha, b.stratum, b.weight, &b.subject, &b.paths)
        );
        let got = (r.predicted_category.as_str(), r.method.as_str());
        if r.subject.starts_with("fix:") {
            fixes += 1;
            assert_eq!(got, ("defect", "exact"), "{}", r.subject);
        } else {
            assert_eq!(got, ("uncategorized", "unclassified"), "{}", r.subject);
        }
    }
    assert!(fixes > 0 && fixes < after.len(), "sample lacks a fix row");
    let p = &summary.provenance;
    let changed = before
        .iter()
        .zip(&after)
        .filter(|(b, r)| b.predicted_category != r.predicted_category)
        .count();
    assert_eq!(p.changed as usize, changed);
    assert!(changed > 0);
    assert_eq!(p.abstentions as usize, after.len() - fixes);

    let prov: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(a.join("sample.v2.provenance.json")).expect("provenance"),
    )
    .expect("provenance json");
    assert_eq!(prov["tga_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(prov["config"]["blake3"], blake3_of(&cfg_path));
    assert_eq!(
        prov["rules_files"][0]["blake3"],
        blake3_of(&d.join("rules.yaml"))
    );
    assert_eq!(prov["rows"], after.len());

    let rater = a.join("v2.csv");
    let labels: Vec<(&str, &str)> = after.iter().map(|r| (r.sha.as_str(), "defect")).collect();
    write_labels(&rater, &labels);
    let report = eval::run_score(&ScoreParams {
        sample: out.clone(),
        strata: None,
        labels: vec![rater],
        adjudicated: None,
        categories: Some(eval::config_categories(&config).expect("categories")),
        db: None,
        out: a.join("report-v2"),
    })
    .expect("score the re-predicted sample");
    assert_eq!(report.scored as usize, after.len());
    let method = |m: &str| report.per_method.iter().find(|row| row.key == m).cloned();
    let exact = method("exact").expect("exact row");
    assert_eq!((exact.n, exact.correct), (fixes as u64, fixes as u64));
    let abstain = method("unclassified").expect("unclassified row");
    assert_eq!(abstain.correct, 0);

    let again = eval::run_repredict(&params).expect_err("overwrote --out");
    assert!(again.to_string().contains("already exists"), "{again}");
}

/// Why: #111 fail-closed rule — a row that cannot be re-derived must stop the
/// run, never be skipped or keep its old prediction.
/// What: a sample of `m1` (in the database) and `x9` (not) is refused with an
/// error naming `x9`, and neither output file is written.
/// Test: this function.
#[test]
fn repredict_refuses_a_sha_missing_from_the_db() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    legacy_sample_and_db(d, &["m1", "x9"], &[("m1", false)]);
    let cfg_path = v2_config(d);
    let out = d.join("v2").join("sample.jsonl");
    let err = eval::run_repredict(&eval::RepredictParams {
        sample: d.join("sample.jsonl"),
        db: d.join("tga.db"),
        config: Config::load(&cfg_path).expect("load config"),
        config_path: cfg_path,
        out: out.clone(),
    })
    .expect_err("a missing SHA was skipped");
    let msg = err.to_string();
    assert!(
        msg.contains("1 sample rows are not in") && msg.contains("x9"),
        "{msg}"
    );
    assert!(!out.exists());
    assert!(!eval::repredict::provenance_path(&out).exists());
}

/// Insert commits `(sha, message)` into a fresh database in repo `r`.
fn db_with(path: &Path, rows: &[(&str, &str)]) {
    let db = Database::open(path).expect("open db");
    for (sha, message) in rows {
        db.connection()
            .execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, is_merge) VALUES (?1, 'n', 'a@example.com', \
                 '2025-03-01T00:00:00Z', ?2, 'r', 0)",
                params![sha, message],
            )
            .expect("insert");
    }
}

/// Why: #111 — `classification.rules_file` resolved against the process CWD
/// while `database:` resolved against the config's directory, so
/// `--config /abs/config.yaml` from another directory failed with an I/O
/// error. The `eval score` half fails on a96727c.
/// What: runs the binary from an unrelated CWD with a config naming
/// `rules.yaml` relatively. `eval score --config` accepts the label `defect`,
/// which only that rules file names; `eval repredict` re-predicts the
/// `fix:` row as `defect`, and without an explicit --config is refused.
/// Test: this function.
#[test]
fn rules_file_resolves_from_another_cwd() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let cfg_dir = d.join("cfg");
    fs::create_dir_all(&cfg_dir).expect("mkdir");
    let cfg = v2_config(&cfg_dir);
    let rows = write_source(d, &[("exact", 2, 20, "feature", 0.9)]);
    write_labels(
        &d.join("rater.csv"),
        &[
            (rows[0].0.as_str(), "defect"),
            (rows[1].0.as_str(), "feature"),
        ],
    );
    let elsewhere = d.join("elsewhere");
    fs::create_dir_all(&elsewhere).expect("mkdir");
    let tga = || {
        let mut c = std::process::Command::new(env!("CARGO_BIN_EXE_tga"));
        c.current_dir(&elsewhere);
        c
    };

    let out = tga()
        .arg("--config")
        .arg(&cfg)
        .args(["eval", "score", "--sample"])
        .arg(d.join("sample.jsonl"))
        .arg("--labels")
        .arg(d.join("rater.csv"))
        .arg("--out")
        .arg(d.join("report"))
        .output()
        .expect("run tga");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "score failed: {stderr}");

    db_with(
        &d.join("tga.db"),
        &[(rows[0].0.as_str(), "fix: a"), (rows[1].0.as_str(), "b")],
    );
    let repredict = |with_config: bool, out_path: &Path| {
        let mut c = tga();
        if with_config {
            c.arg("--config").arg(&cfg);
        }
        c.args(["eval", "repredict", "--sample"])
            .arg(d.join("sample.jsonl"))
            .arg("--db")
            .arg(d.join("tga.db"))
            .arg("--out")
            .arg(out_path)
            .output()
            .expect("run tga")
    };
    let v2 = d.join("v2").join("sample.jsonl");
    let out = repredict(true, &v2);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "repredict failed: {stderr}");
    let after = read_sample(&v2);
    assert_eq!(after[0].predicted_category, "defect");
    assert_eq!(after[1].predicted_category, "uncategorized");

    let out = repredict(false, &d.join("v3").join("sample.jsonl"));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "ran without --config");
    assert!(stderr.contains("--config"), "{stderr}");
}

//! #111 (owner ruling 2026-10-06): the consumer's rules file, named on the
//! command line, carries its categories and its bucket map. Runs the `tga`
//! binary; no network.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::params;
use tga::core::db::Database;

/// A rules file sending `fix:` to `defect` and `chore:` to `upkeep`, with
/// `extra` appended.
fn rules(dir: &Path, name: &str, extra: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(
        &path,
        format!(
            "extend_defaults: false\nrules:\n  - id: d\n    category: defect\n    \
             keywords: [\"fix:\"]\n  - id: u\n    category: upkeep\n    \
             keywords: [\"chore:\"]\n{extra}"
        ),
    )
    .expect("rules");
    path
}

/// A config with the weighted-sum tier off and `extra` under `classification:`.
fn config(dir: &Path, extra: &str) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    fs::write(
        &path,
        format!("classification:\n  weighted_sum:\n    enabled: false\n{extra}"),
    )
    .expect("config");
    path
}

fn tga(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tga"))
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("run tga")
}

fn ok(out: &Output) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "tga failed: {stderr}");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn p(path: &Path) -> &str {
    path.to_str().expect("utf-8 path")
}

/// Two sample rows, `s0` (`fix: a`) and `s1` (`b`), both predicted
/// `feature`, plus strata.json and a rater labelling them `defect` and
/// `feature`; and a database holding the two commits.
fn sample(dir: &Path) {
    let rows: Vec<String> = ["s0", "s1"]
        .iter()
        .map(|sha| {
            serde_json::json!({
                "sha": sha, "repo": "r", "date": "2025-01-01T00:00:00Z", "author_hash": "h",
                "subject": "s", "body": "", "paths": [],
                "diffstat": {"files": 1, "insertions": 1, "deletions": 0},
                "pr_title": null, "ticket_id": null, "issue_type": null,
                "stratum": "exact", "method": "exact", "rule_id": "rule",
                "predicted_category": "feature", "confidence": 0.9, "weight": 1.0,
                "is_merge": false
            })
            .to_string()
        })
        .collect();
    fs::write(dir.join("sample.jsonl"), rows.join("\n") + "\n").expect("sample");
    let strata = serde_json::json!({
        "seed": 1, "weeks": 26, "window_start": "a", "window_end": "b",
        "requested_size": 2, "cap": 5, "population": 2,
        "strata": {"exact": {"population": 2, "sampled": 2}},
        "categories": ["feature", "bugfix", "maintenance"]
    });
    fs::write(dir.join("strata.json"), strata.to_string()).expect("strata");
    fs::write(
        dir.join("rater.csv"),
        "sha,label,note\ns0,defect,\ns1,feature,\n",
    )
    .expect("labels");
    let db = Database::open(&dir.join("tga.db")).expect("open db");
    for (sha, message) in [("s0", "fix: a"), ("s1", "b")] {
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

/// Why (#111): `tga eval score` and `repredict` took the rules file only
/// from the config, so a consumer could not point one run at its own file.
/// What: the config names no rules file. `eval score --rules` accepts the
/// label `defect`, which only that file names (without `--rules` it is
/// refused), and `eval repredict --rules` re-predicts `fix: a` as `defect`
/// and records the file in its provenance.
/// Test: this function.
#[test]
fn eval_score_and_repredict_take_rules_from_the_cli() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    sample(d);
    let cfg = config(d, "");
    let rules = rules(d, "rules.yaml", "");
    let score = |extra: &[&str], out: &str| {
        let mut args = vec!["--config", p(&cfg), "eval", "score"];
        args.extend_from_slice(extra);
        let labels = d.join("rater.csv");
        let sample = d.join("sample.jsonl");
        let out = d.join(out);
        args.extend([
            "--sample",
            p(&sample),
            "--labels",
            p(&labels),
            "--out",
            p(&out),
        ]);
        tga(d, &args)
    };
    let refused = score(&[], "report-a");
    assert!(!refused.status.success(), "defect accepted without --rules");
    ok(&score(&["--rules", p(&rules)], "report-b"));

    let out = d.join("v2.jsonl");
    let (sample, db) = (d.join("sample.jsonl"), d.join("tga.db"));
    ok(&tga(
        d,
        &[
            "--config",
            p(&cfg),
            "eval",
            "repredict",
            "--rules",
            p(&rules),
            "--sample",
            p(&sample),
            "--db",
            p(&db),
            "--out",
            p(&out),
        ],
    ));
    let first: serde_json::Value = serde_json::from_str(
        fs::read_to_string(&out)
            .expect("repredicted")
            .lines()
            .next()
            .expect("row"),
    )
    .expect("json");
    assert_eq!(first["predicted_category"], "defect");
    let prov: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(d.join("v2.provenance.json")).expect("provenance"),
    )
    .expect("json");
    assert!(
        prov["rules_files"][0]["path"]
            .as_str()
            .is_some_and(|s| s.ends_with("rules.yaml")),
        "{prov}"
    );
}

/// Why (#111): a consumer must be able to see which bucket map tga applies
/// and where it came from: `classification.buckets`, its rules file, or
/// tga's built-in fallback.
/// What: `tga rules list --format json` prints `bucket_map.source` and
/// `bucket_map.buckets` (in map order) for each of the three levels, and
/// keeps the taxonomy rollup.
/// Test: this function.
#[test]
fn rules_list_json_reports_the_bucket_map_and_its_source() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    let mapped = rules(
        d,
        "mapped.yaml",
        "buckets:\n  Keep: [upkeep]\n  Fix: [defect]\n",
    );
    // `global` flags first, then the `rules list` flags.
    let list = |global: &[&str], local: &[&str]| {
        let mut args = global.to_vec();
        args.extend(["rules", "list", "--format", "json"]);
        args.extend_from_slice(local);
        let text = ok(&tga(d, &args));
        let json: serde_json::Value = serde_json::from_str(&text).expect("json");
        assert!(json["subcategory_to_top_level"].is_object(), "{text}");
        (json, text)
    };

    let (json, _) = list(&[], &[]);
    assert_eq!(json["bucket_map"]["source"], "fallback");
    assert_eq!(json["bucket_map"]["buckets"]["Maintenance"][4], "upkeep");

    let (json, text) = list(&[], &["--rules", p(&mapped)]);
    assert_eq!(json["bucket_map"]["source"], "rules_file", "{text}");
    assert_eq!(json["bucket_map"]["buckets"]["Fix"][0], "defect");
    let (keep, fix) = (text.find("\"Keep\""), text.find("\"Fix\""));
    assert!(keep.is_some() && keep < fix, "map order lost: {text}");

    // Written last: the binary loads `./config.yaml` when it exists.
    let cfg = config(
        d,
        &format!(
            "  rules_file: {}\n  buckets:\n    Everything: [upkeep, defect]\n",
            p(&mapped)
        ),
    );
    let (json, _) = list(&["--config", p(&cfg)], &[]);
    assert_eq!(json["bucket_map"]["source"], "config");
    assert_eq!(json["bucket_map"]["buckets"]["Everything"][1], "defect");
}

/// Why (#111): `tga classify --rules` must read that file's bucket map,
/// as it reads its rules.
/// What: the config names no rules file; `classify --rules` with a mapped
/// rules file prints the derived bucket view under that file's bucket
/// names.
/// Test: this function.
#[test]
fn classify_rules_flag_reads_the_rules_file_buckets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let d = dir.path();
    sample(d);
    let cfg = config(d, "");
    let mapped = rules(
        d,
        "mapped.yaml",
        "buckets:\n  Keep: [upkeep]\n  Fix: [defect]\n",
    );
    let db = d.join("tga.db");
    let stdout = ok(&tga(
        d,
        &[
            "--config",
            p(&cfg),
            "--database",
            p(&db),
            "classify",
            "--rules",
            p(&mapped),
        ],
    ));
    assert!(stdout.contains("  Fix: 1 (defect 1)"), "{stdout}");
    assert!(stdout.contains("rules file"), "{stdout}");
}

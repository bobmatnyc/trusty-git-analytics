//! Criterion benchmarks for the `tga` hot path.
//!
//! Five benchmarks cover the components that previous profiling runs flagged
//! as load-bearing (see `docs/adr/0002-performance-hotspots.md`):
//!
//! 1. End-to-end commit classification throughput (Tier-1 + Tier-2 cascade)
//! 2. Tier-1 alone — the Aho-Corasick exact matcher
//! 3. CSV generation for `weekly_metrics.csv`
//! 4. Identity resolution (`IdentityResolver::resolve`)
//! 5. ISO-week range iteration (`weeks_in_range`)
//!
//! Run with `cargo bench`. Run a single bench with
//! `cargo bench --bench tga_bench -- classify_throughput`.

use std::collections::HashMap;

use chrono::NaiveDate;
use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};

use tga::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use tga::classify::rules::{default_rules, Rule};
use tga::classify::tiers::exact::ExactMatcher;
use tga::collect::identity::IdentityResolver;
use tga::collect::weeks::weeks_in_range;
use tga::report::formatters::csv::write_weekly_metrics_csv;
use tga::report::models::{ReportData, WeeklyMetrics};

/// Build a deterministic batch of `n` synthetic commit messages spanning
/// the common rule categories so the benchmark exercises every tier.
fn synthetic_commit_messages(n: usize) -> Vec<String> {
    let templates = [
        "feat: add user authentication endpoint",
        "fix: handle null pointer in cache loader",
        "docs: update README installation steps",
        "refactor: extract validation helpers",
        "test: add unit tests for parser",
        "chore: bump dependencies",
        "perf: optimize hot loop in scheduler",
        "ci: tighten clippy thresholds",
        "style: rustfmt pass",
        "build: switch to cargo workspaces",
        "merge: branch 'feature/x' into main",
        "WIP scratch experiment do not merge",
        "revert: undo broken change",
        "security: bump openssl for CVE",
        "Initial commit",
    ];
    (0..n)
        .map(|i| format!("{} (#{})", templates[i % templates.len()], i))
        .collect()
}

fn bench_classify_throughput(c: &mut Criterion) {
    let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
        .expect("default rules build");
    let msgs = synthetic_commit_messages(1_000);

    let mut group = c.benchmark_group("classify_throughput");
    group.throughput(Throughput::Elements(msgs.len() as u64));
    group.bench_function("default_rules_1000", |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for m in &msgs {
                if engine.classify_sync(black_box(m), false).is_some() {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    });
    group.finish();
}

fn bench_exact_matcher(c: &mut Criterion) {
    // Build a focused ruleset with a few hundred keywords so the automaton
    // is non-trivial but reproducible across runs.
    let mut rules: Vec<Rule> = Vec::new();
    let categories = [
        (
            "feature",
            vec!["feat", "feature", "add", "implement", "introduce"],
        ),
        ("bugfix", vec!["fix", "bug", "patch", "resolve", "hotfix"]),
        ("docs", vec!["doc", "docs", "readme", "comment"]),
        (
            "refactor",
            vec!["refactor", "cleanup", "rework", "simplify"],
        ),
        ("test", vec!["test", "tests", "unit", "integration"]),
        ("perf", vec!["perf", "optimize", "speedup", "fast"]),
        ("security", vec!["cve", "security", "openssl", "vuln"]),
    ];
    for (i, (cat, kws)) in categories.iter().enumerate() {
        rules.push(Rule {
            id: format!("r{i}"),
            category: (*cat).to_string(),
            subcategory: None,
            keywords: kws.iter().map(|s| (*s).to_string()).collect(),
            patterns: vec![],
            priority: i as i32,
            confidence: 0.9,
        });
    }
    let matcher = ExactMatcher::new(&rules).expect("exact matcher builds");
    let msgs = synthetic_commit_messages(1_000);

    let mut group = c.benchmark_group("exact_matcher");
    group.throughput(Throughput::Elements(msgs.len() as u64));
    group.bench_function("aho_corasick_1000", |b| {
        b.iter(|| {
            let mut hits = 0usize;
            for m in &msgs {
                if matcher.classify(black_box(m)).is_some() {
                    hits += 1;
                }
            }
            black_box(hits);
        });
    });
    group.finish();
}

/// Build a synthetic [`ReportData`] with `n` weekly rows.
fn synthetic_report_data(n: usize) -> ReportData {
    let mut data = ReportData::empty("2025-01-01T00:00:00Z".to_string());
    data.weekly_metrics = (0..n)
        .map(|i| WeeklyMetrics {
            week: format!("2025-W{:02}", (i % 52) + 1),
            total_commits: 50 + i,
            feature_commits: 20,
            bugfix_commits: 10,
            maintenance_commits: 5,
            refactor_commits: 5,
            test_commits: 5,
            doc_commits: 5,
            active_developers: 8,
            story_points: (i as f64) * 1.5,
        })
        .collect();
    data
}

fn bench_csv_generation(c: &mut Criterion) {
    let data = synthetic_report_data(520); // 10 years of weeks
    let tmp = std::env::temp_dir().join("tga_bench_csv");
    std::fs::create_dir_all(&tmp).expect("temp dir");

    let mut group = c.benchmark_group("csv_generation");
    group.throughput(Throughput::Elements(data.weekly_metrics.len() as u64));
    group.bench_function("weekly_metrics_520_rows", |b| {
        b.iter(|| {
            let path =
                write_weekly_metrics_csv(black_box(&data), &tmp).expect("csv write should succeed");
            black_box(path);
        });
    });
    group.finish();
}

fn bench_identity_resolution(c: &mut Criterion) {
    // Build a resolver with a moderately sized alias map.
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for i in 0..100 {
        let canon = format!("Developer {i}");
        map.insert(
            canon.clone(),
            vec![
                format!("dev{i}@example.com"),
                format!("dev{i}@old-corp.com"),
                format!("dev.{i}"),
            ],
        );
    }
    let resolver = IdentityResolver::from_alias_map(&map);

    // A mix of hits (in the alias map) and misses (forcing fuzzy fallback).
    let pairs: Vec<(String, String)> = (0..500)
        .map(|i| {
            if i % 3 == 0 {
                // Direct alias hit.
                (
                    format!("Developer {}", i % 100),
                    format!("dev{}@example.com", i % 100),
                )
            } else if i % 3 == 1 {
                // Email-only hit (alias map lookup).
                (
                    "Unknown".to_string(),
                    format!("dev{}@old-corp.com", i % 100),
                )
            } else {
                // Miss — exercises fuzzy path.
                (
                    format!("Devloper {}", i % 100),
                    format!("typo{i}@example.com"),
                )
            }
        })
        .collect();

    let mut group = c.benchmark_group("identity_resolution");
    group.throughput(Throughput::Elements(pairs.len() as u64));
    group.bench_function("resolve_500_mixed", |b| {
        b.iter(|| {
            for (n, e) in &pairs {
                let r = resolver.resolve(black_box(n), black_box(e));
                black_box(r);
            }
        });
    });
    group.finish();
}

fn bench_iso_weeks(c: &mut Criterion) {
    let from = NaiveDate::from_ymd_opt(2024, 1, 1).expect("valid date");
    let to = NaiveDate::from_ymd_opt(2025, 12, 31).expect("valid date");

    let mut group = c.benchmark_group("iso_weeks");
    group.bench_function("weeks_in_range_2y", |b| {
        b.iter(|| {
            let v: Vec<_> = weeks_in_range(black_box(from), black_box(to)).collect();
            black_box(v);
        });
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_classify_throughput,
    bench_exact_matcher,
    bench_csv_generation,
    bench_identity_resolution,
    bench_iso_weeks,
);
criterion_main!(benches);

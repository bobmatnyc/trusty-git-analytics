//! `tga report` and the Linear delivery section (#190 step 5).
//!
//! The figures are pinned in `report::linear_stats::tests`. These tests pin
//! what the pipeline adds: the section reaches `report.md` only when
//! `linear.stats.report` is on and the database holds synced Linear issues;
//! otherwise `report.md` is byte-identical to the plain Markdown formatter's
//! output; a stats failure fails the report before any file is written; and
//! the run leaves every Linear table as it found it.

use std::path::Path;

use chrono::{DateTime, Utc};
use tempfile::TempDir;

use crate::core::config::{Config, LinearConfig, OutputConfig};
use crate::core::db::Database;
use crate::report::aggregator::Aggregator;
use crate::report::formatters::markdown as md_fmt;
use crate::report::linear_stats::test_fixture::seeded;
use crate::report::linear_stats::{compute_linear_stats, render_markdown, StatsOptions};
use crate::report::ReportPipeline;

const SECTION_HEADING: &str = "## Linear delivery";
const AUTHOR_NOTE: &str = "The Linear delivery section is left out under an author filter \
    because it covers the whole workspace.";

/// A `linear:` block parsed from YAML, as `config.yaml` would give it.
fn linear(yaml: &str) -> LinearConfig {
    serde_yaml::from_str(yaml).expect("linear block parses")
}

/// A Markdown-only report config writing into `dir`.
fn config(dir: &Path, linear: Option<LinearConfig>) -> Config {
    Config {
        output: Some(OutputConfig {
            directory: Some(dir.to_path_buf()),
            formats: vec!["markdown".into()],
            ..Default::default()
        }),
        linear,
        ..Default::default()
    }
}

/// `linear.stats.report: true`.
fn enabled() -> Option<LinearConfig> {
    Some(linear("stats:\n  report: true\n"))
}

/// The one line that differs between two renders of the same data.
fn without_generated_line(md: &str) -> String {
    md.lines()
        .filter(|l| !l.starts_with("Generated: "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn generated_at(md: &str) -> DateTime<Utc> {
    let raw = md
        .lines()
        .find_map(|l| l.strip_prefix("Generated: "))
        .expect("report.md has a Generated line");
    DateTime::parse_from_rfc3339(raw)
        .expect("Generated is RFC 3339")
        .with_timezone(&Utc)
}

/// `report.md` from the pipeline under `cfg`.
fn pipeline_md(db: &Database, cfg: &Config, author: Option<&str>) -> String {
    let dir = cfg
        .output
        .as_ref()
        .and_then(|o| o.directory.clone())
        .expect("dir");
    ReportPipeline::new(cfg.clone())
        .run_with_filter(db, author)
        .expect("report runs");
    std::fs::read_to_string(dir.join(md_fmt::REPORT_MD)).expect("report.md")
}

/// `report.md` from the plain Markdown formatter: the output before #190.
fn plain_md(db: &Database, cfg: &Config) -> String {
    let dir = TempDir::new().expect("tmp");
    let data = Aggregator::build_filtered(db, cfg, None).expect("aggregate");
    let path = md_fmt::write_markdown(&data, dir.path()).expect("write");
    std::fs::read_to_string(path).expect("read")
}

/// Every row of every `linear*` table, in a stable order.
fn linear_tables(db: &Database) -> Vec<String> {
    let conn = db.connection();
    let mut tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' AND name LIKE 'linear%'")
        .expect("prepare")
        .query_map([], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("names");
    tables.sort();
    assert!(tables.len() >= 8, "linear tables: {tables:?}");
    let mut out = Vec::new();
    for t in tables {
        let mut stmt = conn
            .prepare(&format!("SELECT * FROM {t} ORDER BY rowid"))
            .expect("prepare");
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                let cells: Vec<String> = (0..n)
                    .map(|i| {
                        r.get::<_, rusqlite::types::Value>(i)
                            .map(|v| format!("{v:?}"))
                    })
                    .collect::<Result<_, _>>()?;
                Ok(format!("{t}: {}", cells.join(" | ")))
            })
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("rows");
        out.extend(rows);
    }
    out
}

#[test]
fn report_md_carries_the_linear_section_when_enabled_and_synced() {
    let db = seeded();
    let dir = TempDir::new().expect("tmp");
    let cfg = config(dir.path(), enabled());
    let md = pipeline_md(&db, &cfg, None);

    let lin = cfg.linear.as_ref().expect("linear");
    let opts = StatsOptions::from_config(generated_at(&md), &lin.stats);
    let block = render_markdown(&compute_linear_stats(db.connection(), &opts).expect("stats"));
    assert!(
        md.ends_with(&block),
        "report.md does not end with the block:\n{md}"
    );
    assert_eq!(md.matches(SECTION_HEADING).count(), 1, "{md}");
    // Everything before the block is the plain report.
    let head = md.strip_suffix(&block).expect("suffix");
    assert_eq!(
        without_generated_line(head).trim_end(),
        without_generated_line(&plain_md(&db, &cfg)).trim_end()
    );
}

#[test]
fn report_md_is_byte_identical_when_linear_is_off_or_empty() {
    // (case, database, linear block)
    let cases: Vec<(&str, Database, Option<LinearConfig>)> = vec![
        ("no linear block", seeded(), None),
        (
            "linear block without stats.report",
            seeded(),
            Some(linear("team_keys: [ENG]\n")),
        ),
        (
            "stats.report false",
            seeded(),
            Some(linear("stats:\n  report: false\n")),
        ),
        (
            "enabled, no Linear rows",
            Database::open_in_memory().expect("db"),
            enabled(),
        ),
    ];
    for (case, db, lin) in cases {
        let dir = TempDir::new().expect("tmp");
        let cfg = config(dir.path(), lin);
        let md = pipeline_md(&db, &cfg, None);
        assert!(
            !md.contains(SECTION_HEADING),
            "{case}: section leaked:\n{md}"
        );
        assert_eq!(
            without_generated_line(&md),
            without_generated_line(&plain_md(&db, &cfg)),
            "{case}"
        );
    }
}

#[test]
fn report_fails_naming_linear_stats_when_they_cannot_be_computed() {
    let db = seeded();
    db.connection()
        .execute("UPDATE linear_issues SET completed_at = 'not-a-time'", [])
        .expect("damage a row");
    let dir = TempDir::new().expect("tmp");
    let mut cfg = config(dir.path(), enabled());
    // Every format, so a partial write would show.
    cfg.output.as_mut().expect("output").formats = Vec::new();

    let err = ReportPipeline::new(cfg)
        .run(&db)
        .expect_err("a stats failure must fail the report");
    let msg = err.to_string();
    assert!(
        msg.contains("Linear delivery section") && msg.contains("not-a-time"),
        "{msg}"
    );
    let written: Vec<_> = std::fs::read_dir(dir.path())
        .map(|d| d.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    assert!(
        written.is_empty(),
        "files written before the failure: {written:?}"
    );
}

#[test]
fn report_md_omits_the_linear_section_under_an_author_filter() {
    let db = seeded();
    db.connection()
        .execute(
            "INSERT INTO authors (id, canonical_name, canonical_email, aliases) \
             VALUES (1, 'Ann', 'ann@example.com', '[]')",
            [],
        )
        .expect("insert author");
    let dir = TempDir::new().expect("tmp");
    let cfg = config(dir.path(), enabled());
    let md = pipeline_md(&db, &cfg, Some("ann@example.com"));
    assert!(!md.contains(SECTION_HEADING), "{md}");
    // The reader is told why, in one plain line at the end of the report.
    assert!(md.ends_with(&format!("\n\n{AUTHOR_NOTE}\n")), "{md}");
    assert_eq!(md.matches(AUTHOR_NOTE).count(), 1, "{md}");
}

#[test]
fn report_leaves_every_linear_table_unchanged() {
    let db = seeded();
    let before = linear_tables(&db);
    assert!(!before.is_empty());
    let dir = TempDir::new().expect("tmp");
    let cfg = config(dir.path(), enabled());
    let md = pipeline_md(&db, &cfg, None);
    assert!(
        md.contains(SECTION_HEADING),
        "the section must have been computed"
    );
    assert_eq!(linear_tables(&db), before);
}

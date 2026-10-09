//! Tests for `tga linear stats` (#190 step 4).
//!
//! The figures themselves are pinned in `report::linear_stats::tests`; these
//! tests cover what the command adds: the arguments, the output shape, and
//! that `--as-of` and `linear.stats` reach the computation.

use chrono::{NaiveDate, TimeZone, Utc};
use clap::Parser;
use serde_json::Value;

use super::*;
use crate::commands::args::LinearSubcommand;
use tga::core::config::LinearConfig;
use tga::report::linear_stats::test_fixture::seeded;

/// A stand-in for `tga linear`, so the parse goes through the real variant.
#[derive(Parser, Debug)]
struct LinearCli {
    #[command(subcommand)]
    sub: LinearSubcommand,
}

fn parse(argv: &[&str]) -> Result<LinearStatsArgs, clap::Error> {
    let cli = LinearCli::try_parse_from(std::iter::once("linear").chain(argv.iter().copied()))?;
    match cli.sub {
        LinearSubcommand::Stats(a) => Ok(a),
        other => panic!("parsed as {other:?}, not stats"),
    }
}

fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date")
}

fn json(config: &Config, as_of: &str) -> Value {
    let args = LinearStatsArgs {
        json: true,
        as_of: Some(date(as_of)),
    };
    let out = render_stats(config, &seeded(), &args, Utc::now()).expect("renders");
    serde_json::from_str(&out).expect("valid JSON")
}

#[test]
fn stats_parses_with_no_flags() {
    assert_eq!(
        parse(&["stats"]).expect("parses"),
        LinearStatsArgs::default()
    );
}

#[test]
fn stats_parses_json_and_as_of() {
    let a = parse(&["stats", "--json", "--as-of", "2026-01-31"]).expect("parses");
    assert!(a.json);
    assert_eq!(a.as_of, Some(date("2026-01-31")));
}

#[test]
fn stats_rejects_an_as_of_that_is_not_a_date() {
    let err = parse(&["stats", "--as-of", "last week"]).expect_err("rejected");
    assert!(err.to_string().contains("--as-of"), "{err}");
}

#[test]
fn as_of_is_midnight_utc_at_the_start_of_the_date() {
    let now = Utc.with_ymd_and_hms(2030, 1, 1, 5, 0, 0).unwrap();
    assert_eq!(as_of_instant(None, now), now);
    assert_eq!(
        as_of_instant(Some(date("2026-10-09")), now),
        Utc.with_ymd_and_hms(2026, 10, 9, 0, 0, 0).unwrap()
    );
}

#[test]
fn json_output_has_the_documented_shape() {
    let v = json(&Config::default(), "2026-10-09");
    let keys: Vec<&str> = v
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    for k in [
        "schema_version",
        "as_of",
        "field_use_created_since",
        "concentration_completed_since",
        "population",
        "completions",
        "lead_cycle",
        "cycles",
        "title_tags",
        "field_use",
        "projects",
        "cancel_rate",
        "people",
        "tolerances",
    ] {
        assert!(keys.contains(&k), "missing {k} in {keys:?}");
    }
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["as_of"], "2026-10-09T00:00:00+00:00");
    assert_eq!(v["population"]["issues"], 31);
    assert_eq!(v["population"]["issues_by_team"]["ENG"], 21);
    assert!(v["completions"]["per_quarter"]["all"].is_object());
    assert!(v["completions"]["per_quarter"]["teams"]["ENG"].is_object());
}

#[test]
fn as_of_moves_windows_and_overdue_projects() {
    let now = json(&Config::default(), "2026-10-09");
    let before = json(&Config::default(), "2026-06-01");
    assert_eq!(
        now["completions"]["recent_window"],
        serde_json::json!(["2025-Q4", "2026-Q1", "2026-Q2", "2026-Q3"])
    );
    assert_eq!(
        before["completions"]["recent_window"],
        serde_json::json!(["2025-Q2", "2025-Q3", "2025-Q4", "2026-Q1"])
    );
    // prj-4 is started with a 2026-09-01 target: overdue in October only.
    assert_eq!(now["projects"]["started_past_target"], 1);
    assert_eq!(before["projects"]["started_past_target"], 0);
}

#[test]
fn config_bounds_reach_the_output() {
    let mut linear = LinearConfig::default();
    linear.stats = serde_yaml::from_str("field_use_created_since: 2026-01-01\n").expect("stats");
    let config = Config {
        linear: Some(linear),
        ..Default::default()
    };
    let bounded = json(&config, "2026-10-09");
    let open = json(&Config::default(), "2026-10-09");
    assert_eq!(bounded["field_use_created_since"], "2026-01-01");
    assert!(open["field_use_created_since"].is_null());
    assert!(
        bounded["field_use"]["shares"]["all"]["n"].as_u64()
            < open["field_use"]["shares"]["all"]["n"].as_u64()
    );
}

#[test]
fn default_output_is_the_markdown_block() {
    let args = LinearStatsArgs {
        json: false,
        as_of: Some(date("2026-10-09")),
    };
    let out = render_stats(&Config::default(), &seeded(), &args, Utc::now()).expect("renders");
    assert!(out.starts_with("## Linear delivery"), "{out}");
    assert!(out.contains("As of 2026-10-09T00:00:00+00:00"), "{out}");
}

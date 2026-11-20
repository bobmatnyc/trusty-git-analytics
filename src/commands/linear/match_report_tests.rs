//! Tests for `tga linear match-report` (#272). The counts are pinned in
//! `report::linear_match::tests`; these cover the arguments and the output.

use clap::Parser;

use super::*;
use crate::commands::args::LinearSubcommand;
use tga::report::linear_match::tests::seeded;

#[derive(Parser, Debug)]
struct LinearCli {
    #[command(subcommand)]
    sub: LinearSubcommand,
}

fn parse(argv: &[&str]) -> Result<MatchReportArgs, clap::Error> {
    let cli = LinearCli::try_parse_from(std::iter::once("linear").chain(argv.iter().copied()))?;
    match cli.sub {
        LinearSubcommand::MatchReport(a) => Ok(a),
        other => panic!("parsed as {other:?}, not match-report"),
    }
}

fn config() -> Config {
    serde_yaml::from_str(
        "repositories:\n  - path: /src/rates\n    github_repo: pacerevenue/rates\n\
         linear:\n  repos: [pacerevenue]\n",
    )
    .expect("yaml")
}

#[test]
fn parses_window_and_format() {
    let a = parse(&[
        "match-report",
        "--since",
        "2026-01-01",
        "--until",
        "2026-04-01",
        "--format",
        "json",
    ])
    .expect("parses");
    assert_eq!(a.format, MatchReportFormat::Json);
    assert_eq!(
        a.since.map(|d| d.to_string()).as_deref(),
        Some("2026-01-01")
    );
    assert!(parse(&["match-report", "--format", "xml"]).is_err());
}

/// #272: the text report on the shared fixture; printed with
/// `--nocapture` for the PR evidence.
#[test]
fn text_report_on_the_fixture() {
    let args = parse(&[
        "match-report",
        "--since",
        "2026-01-01",
        "--until",
        "2026-04-01",
    ])
    .expect("parses");
    let out = render_match_report(&config(), &seeded(), &args).expect("renders");
    println!("{out}");
    assert!(out.contains("PRs in Linear-bound repos: 3 total, 2 with a ticket, 1 without"));
    assert!(out.contains("attachment 1, branch 1, title 1, body 0, commit 0"));
    assert!(out.contains("Linear assignees with no tga developer (1): Zed Stranger"));
    assert!(!out.contains('@'), "{out}");
}

#[test]
fn json_report_has_the_documented_keys() {
    let args = MatchReportArgs {
        format: MatchReportFormat::Json,
        ..Default::default()
    };
    let out = render_match_report(&config(), &seeded(), &args).expect("renders");
    let v: serde_json::Value = serde_json::from_str(&out).expect("json");
    for key in [
        "prs",
        "engineers",
        "unassigned_closed",
        "unmatched_linear_assignees",
        "unmatched_tga_authors",
    ] {
        assert!(v.get(key).is_some(), "missing {key}: {out}");
    }
}

#[test]
fn since_after_until_is_refused() {
    let args = parse(&[
        "match-report",
        "--since",
        "2026-04-01",
        "--until",
        "2026-01-01",
    ])
    .expect("parses");
    let err = render_match_report(&config(), &seeded(), &args).expect_err("refused");
    assert!(err.to_string().contains("--since"), "{err}");
}

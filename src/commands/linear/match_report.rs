//! `tga linear match-report` (#272): PR/ticket coverage and identity gaps.
//!
//! Why: before cto-reports reads per-engineer Linear figures, an operator
//! checks how many PRs in Linear-bound repos link a ticket and whose
//! identities did not match.
//! What: opens the database read-only, builds a
//! [`tga::report::linear_match::MatchReport`] and prints it as text or JSON.
//! No network call; names only, never an email or a key.
//! Test: `match_report_tests`.

use std::path::Path;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::{Args, ValueEnum};

use tga::collect::identity::IdentityResolver;
use tga::collect::linear::developer::DeveloperIndex;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::report::linear_match::{compute_match_report, render_text};

/// Output format of `tga linear match-report`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum MatchReportFormat {
    /// Plain text.
    #[default]
    Text,
    /// Pretty JSON.
    Json,
}

/// Arguments for `tga linear match-report`.
#[derive(Args, Debug, Default, Clone, PartialEq, Eq)]
#[command(
    about = "Report PRs with and without a Linear ticket, per-engineer counts and unmatched \
             identities (no network).",
    long_about = "Read the stored PRs, PR-to-ticket links, Linear issues and authors, and print:\n\
- PRs in Linear-bound repos (linear.repos) with at least one ticket link against\n\
  PRs with none, and the PRs linked per source;\n\
- per engineer: PRs, PRs with a ticket, and closed Linear issues in the window;\n\
- Linear assignees with no tga developer, and tga authors in bound repos with no\n\
  Linear user.\n\n\
Names only; no email and no key is printed. Reads the database read-only.",
    after_help = "EXAMPLES:\n\
  tga linear match-report --since 2026-01-01 --until 2026-04-01\n\
  tga linear match-report --format json"
)]
pub struct MatchReportArgs {
    /// Window start (inclusive), YYYY-MM-DD, midnight UTC.
    #[arg(long, value_name = "DATE")]
    pub since: Option<NaiveDate>,
    /// Window end (exclusive), YYYY-MM-DD, midnight UTC.
    #[arg(long, value_name = "DATE")]
    pub until: Option<NaiveDate>,
    /// Output format.
    #[arg(long, value_enum, default_value_t = MatchReportFormat::Text)]
    pub format: MatchReportFormat,
}

fn midnight(d: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(&d.and_time(chrono::NaiveTime::MIN))
}

/// Build the report and render it as the command prints it.
///
/// # Errors
///
/// `--since` after `--until`; a malformed stored row (named in the error);
/// a database failure.
pub fn render_match_report(
    config: &Config,
    db: &Database,
    args: &MatchReportArgs,
) -> anyhow::Result<String> {
    if let (Some(s), Some(u)) = (args.since, args.until) {
        anyhow::ensure!(s < u, "--since ({s}) must be before --until ({u})");
    }
    let index = DeveloperIndex::load(db.connection(), IdentityResolver::from_config(config))?;
    let report = compute_match_report(
        db.connection(),
        config,
        &index,
        args.since.map(midnight),
        args.until.map(midnight),
    )?;
    Ok(match args.format {
        MatchReportFormat::Text => render_text(&report),
        MatchReportFormat::Json => serde_json::to_string_pretty(&report)?,
    })
}

/// Print the report for a database the caller already opened.
///
/// # Errors
///
/// See [`render_match_report`].
pub fn run_match_report(
    config: &Config,
    db: &Database,
    args: &MatchReportArgs,
) -> anyhow::Result<()> {
    println!("{}", render_match_report(config, db, args)?);
    Ok(())
}

/// What a pending-migrations error tells the user to do.
const MIGRATE_FIRST: &str = "tga linear match-report reads the database read-only and never \
    migrates it. Run a command that migrates it first, such as `tga linear sync` \
    (back up the file first)";

/// Dispatch entry point, as `main` calls it: a read-only open, so a wrong
/// path fails instead of minting an empty database.
///
/// # Errors
///
/// A missing path, pending migrations, or the errors of
/// [`render_match_report`].
pub fn run_match_report_at(
    config: &Config,
    db_path: &Path,
    args: &MatchReportArgs,
) -> anyhow::Result<()> {
    let db = Database::open_read_only_with_remedy(db_path, MIGRATE_FIRST)?;
    run_match_report(config, &db, args)
}

#[cfg(test)]
#[path = "match_report_tests.rs"]
mod tests;

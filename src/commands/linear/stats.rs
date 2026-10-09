//! `tga linear stats` — print the Linear delivery metrics (#190 step 4).
//!
//! Why: `report::linear_stats` computes completions, lead and cycle time,
//! cycles, field use, projects, cancel rate and people from the tables
//! `tga linear sync` fills, but no command printed them.
//! What: [`run_stats`] reads the Linear tables already in the database,
//! makes no network call, and prints [`render_stats`]: the Markdown block by
//! default, the full JSON with `--json`. The lower bounds come from
//! `linear.stats` in config.yaml.
//! Test: `stats_tests` (argument parsing, JSON shape on the fixture
//! database, `--as-of` moving the result).

use std::path::Path;

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use clap::Args;

use tga::core::config::Config;
use tga::core::db::Database;
use tga::report::linear_stats::{compute_linear_stats, render_markdown, StatsOptions};

/// Arguments for `tga linear stats`.
#[derive(Args, Debug, Default, Clone, PartialEq, Eq)]
#[command(
    about = "Print Linear delivery metrics from the synced Linear tables (no network).",
    long_about = "Compute the Linear delivery metrics from the tables `tga linear sync`\n\
(with --entities) already wrote: population, completions per quarter and the\n\
four-quarter trend, lead and cycle time, cycle completion, title tags, field\n\
use, projects, cancel rate and people. Reads the database only; makes no\n\
network call.\n\n\
Prints a Markdown block by default, or the full JSON with --json.\n\
`linear.stats` in config.yaml sets the lower bounds for field use and\n\
concentration.",
    after_help = "EXAMPLES:\n\
  # Markdown summary as of now\n\
  tga linear stats\n\n\
  # Full JSON, measured as of the start of 2026-10-01 (UTC)\n\
  tga linear stats --json --as-of 2026-10-01"
)]
pub struct LinearStatsArgs {
    /// Print the full metrics as JSON instead of the Markdown summary.
    #[arg(long, default_value_t = false)]
    pub json: bool,
    /// Measure windows, ages and overdue figures at midnight UTC at the
    /// start of this date (YYYY-MM-DD). Default: now.
    #[arg(long, value_name = "DATE")]
    pub as_of: Option<NaiveDate>,
}

/// The instant `--as-of` names: midnight UTC at the start of the date, or
/// `now` when the flag is absent.
fn as_of_instant(as_of: Option<NaiveDate>, now: DateTime<Utc>) -> DateTime<Utc> {
    as_of.map_or(now, |d| {
        Utc.from_utc_datetime(&d.and_time(chrono::NaiveTime::MIN))
    })
}

/// Compute the metrics and render them as the command prints them.
///
/// Why: the command's output is the contract; a test needs it as a string.
/// What: builds [`StatsOptions`] from `--as-of` (or `now`) and
/// `linear.stats`, runs [`compute_linear_stats`] on `db`, and returns the
/// Markdown block, or pretty JSON with `--json`. Writes nothing.
/// Test: `stats_tests::json_output_has_the_documented_shape`,
/// `stats_tests::as_of_moves_windows_and_overdue_projects`,
/// `stats_tests::config_bounds_reach_the_output`,
/// `stats_tests::default_output_is_the_markdown_block`.
///
/// # Errors
///
/// A failed read, or a stored value that does not parse (the error names
/// the row).
pub fn render_stats(
    config: &Config,
    db: &Database,
    args: &LinearStatsArgs,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    // #190: an absent `linear:` block or `linear.stats` means no bounds.
    let bounds = config
        .linear
        .as_ref()
        .map(|l| l.stats.clone())
        .unwrap_or_default();
    let opts = StatsOptions::from_config(as_of_instant(args.as_of, now), &bounds);
    let stats = compute_linear_stats(db.connection(), &opts)?;
    if args.json {
        Ok(serde_json::to_string_pretty(&stats)?)
    } else {
        Ok(render_markdown(&stats))
    }
}

/// Print `tga linear stats` for a database the caller already opened.
///
/// # Errors
///
/// See [`render_stats`].
pub fn run_stats(config: &Config, db: &Database, args: &LinearStatsArgs) -> anyhow::Result<()> {
    println!("{}", render_stats(config, db, args, Utc::now())?);
    Ok(())
}

/// What a pending-migrations error tells the `tga linear stats` user to do.
const MIGRATE_FIRST: &str = "tga linear stats reads the database read-only and never \
    migrates it. Run a command that migrates it first, such as `tga linear sync` \
    (back up the file first)";

/// Dispatch entry point for `tga linear stats`, as `main` calls it.
///
/// Why: #190 — the shared `Database::open` creates a missing file and runs
/// every migration, so a wrong `--database` path printed a zeroed report and
/// exited 0. A report must read the database as it stands.
/// What: opens `db_path` with [`Database::open_read_only_with_remedy`],
/// which refuses a missing or non-SQLite file and a schema with pending
/// migrations, then runs [`run_stats`].
/// Test: `tests/linear_stats_read_only.rs`.
///
/// # Errors
///
/// A missing path (named in the error), pending migrations, or the errors
/// of [`render_stats`].
pub fn run_stats_at(config: &Config, db_path: &Path, args: &LinearStatsArgs) -> anyhow::Result<()> {
    let db = Database::open_read_only_with_remedy(db_path, MIGRATE_FIRST)?;
    run_stats(config, &db, args)
}

#[cfg(test)]
#[path = "stats_tests.rs"]
mod tests;

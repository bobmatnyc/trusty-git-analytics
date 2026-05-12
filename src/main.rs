//! `tga` — the trusty-git-analytics command-line binary.
//!
//! Wires together the library modules (`core`, `collect`, `classify`,
//! `report`) behind a clap subcommand interface.

#![warn(missing_docs)]

mod commands;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use tga::core::config::Config;
use tga::core::db::Database;

use crate::commands::aliases::AliasesArgs;
use crate::commands::backfill::BackfillArgs;
use crate::commands::install::InstallArgs;
use crate::commands::pr_metrics::PrMetricsArgs;

/// Top-level CLI parser.
#[derive(Parser, Debug)]
#[command(
    name = "tga",
    about = "trusty-git-analytics — developer productivity analytics",
    version,
    propagate_version = true
)]
struct Cli {
    /// Path to config file (default: ./config.yaml).
    #[arg(short, long, default_value = "config.yaml", global = true)]
    config: PathBuf,

    /// Path to SQLite database (default: ./tga.db).
    #[arg(short, long, default_value = "tga.db", global = true)]
    database: PathBuf,

    /// Verbosity level (-v, -vv, -vvv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the full pipeline: collect → classify → report.
    Analyze(AnalyzeArgs),
    /// Stage 1: collect commits from git repositories.
    Collect(CollectArgs),
    /// Stage 2: classify collected commits.
    Classify(ClassifyArgs),
    /// Stage 3: generate reports from classified commits.
    Report(ReportArgs),
    /// Aggregate pull-request metrics per engineer.
    PrMetrics(PrMetricsArgs),
    /// Interactive configuration wizard.
    Install(InstallArgs),
    /// List or merge developer identities (aliases).
    Aliases(AliasesArgs),
    /// Retroactive maintenance operations on existing commit rows.
    Backfill(BackfillArgs),
}

/// Arguments for `tga analyze`.
#[derive(Args, Debug)]
pub struct AnalyzeArgs {
    /// Skip collection (use existing DB data).
    #[arg(long)]
    pub skip_collect: bool,
    /// Skip classification.
    #[arg(long)]
    pub skip_classify: bool,
    /// Output directory override.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Re-collect all weeks even if already present in the database.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Limit collection to the last N weeks of commits (overrides config start_date).
    #[arg(long, value_name = "N", conflicts_with_all = ["from", "to"])]
    pub weeks: Option<u32>,
    /// Start date for collection (ISO8601: YYYY-MM-DD). Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub from: Option<String>,
    /// End date for collection (ISO8601: YYYY-MM-DD). Defaults to today.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub to: Option<String>,
    /// Skip the pre-walk `git fetch` step (use only local refs).
    #[arg(long, default_value_t = false)]
    pub no_fetch: bool,
    /// Perform all steps except writing to the database (log intent only).
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

/// Arguments for `tga collect`.
#[derive(Args, Debug)]
pub struct CollectArgs {
    /// Only collect from these repository names (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub repos: Vec<String>,
    /// Collect since date (ISO 8601, overrides config). Legacy alias for --from.
    #[arg(long)]
    pub since: Option<String>,
    /// Collect until date (ISO 8601, overrides config). Legacy alias for --to.
    #[arg(long)]
    pub until: Option<String>,
    /// Start date for collection (ISO8601: YYYY-MM-DD). Mutually exclusive with --weeks.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub from: Option<String>,
    /// End date for collection (ISO8601: YYYY-MM-DD). Defaults to today.
    #[arg(long, value_name = "DATE", conflicts_with = "weeks")]
    pub to: Option<String>,
    /// Re-collect all weeks even if already present in the database.
    #[arg(long, short = 'f', default_value_t = false)]
    pub force: bool,
    /// Limit collection to the last N weeks of commits (overrides config start_date).
    #[arg(long, value_name = "N", conflicts_with_all = ["from", "to"])]
    pub weeks: Option<u32>,
    /// Skip the pre-walk `git fetch` step (use only local refs).
    #[arg(long, default_value_t = false)]
    pub no_fetch: bool,
    /// Perform all steps except writing to the database (log intent only).
    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}

/// Arguments for `tga classify`.
#[derive(Args, Debug)]
pub struct ClassifyArgs {
    /// Rules file override.
    #[arg(long)]
    pub rules: Option<PathBuf>,
    /// Enable LLM fallback (overrides config).
    #[arg(long)]
    pub use_llm: bool,
}

/// Arguments for `tga report`.
#[derive(Args, Debug)]
pub struct ReportArgs {
    /// Output directory override.
    #[arg(short, long)]
    pub output: Option<PathBuf>,
    /// Output formats (csv, json, markdown — comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub formats: Vec<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Initialize tracing based on verbosity flag count.
    let level = match cli.verbose {
        0 => tracing::Level::WARN,
        1 => tracing::Level::INFO,
        2 => tracing::Level::DEBUG,
        _ => tracing::Level::TRACE,
    };
    tracing_subscriber::fmt().with_max_level(level).init();

    // Load configuration (fall back to default if file is missing).
    let config = if cli.config.exists() {
        tracing::info!(path = %cli.config.display(), "loading config");
        Config::load(&cli.config)?
    } else {
        tracing::warn!(
            "config file {} not found, using defaults",
            cli.config.display()
        );
        Config::default()
    };

    // `tga install` does not require an open database — it just writes a
    // config file. Handle it before the DB open call so a missing/locked
    // `tga.db` cannot block bootstrapping a fresh project.
    if let Commands::Install(args) = cli.command {
        return commands::install::run(config, args);
    }

    // Open SQLite database (runs migrations on open).
    tracing::info!(path = %cli.database.display(), "opening database");
    let mut db = Database::open(&cli.database)?;

    match cli.command {
        Commands::Analyze(args) => commands::analyze::run(config, &mut db, args).await?,
        Commands::Collect(args) => commands::collect::run(config, &mut db, args).await?,
        Commands::Classify(args) => commands::classify::run(config, &mut db, args).await?,
        Commands::Report(args) => commands::report::run(config, &db, args)?,
        Commands::PrMetrics(args) => commands::pr_metrics::run(config, &db, args)?,
        Commands::Aliases(args) => commands::aliases::run(config, &mut db, args)?,
        Commands::Backfill(args) => commands::backfill::run(config, &mut db, args)?,
        // Handled above — match is exhaustive.
        Commands::Install(_) => unreachable!("install dispatched above"),
    }

    Ok(())
}

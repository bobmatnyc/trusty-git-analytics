//! `tga` — the trusty-git-analytics command-line binary.
//!
//! Wires together the four library crates (`tga-core`, `tga-collect`,
//! `tga-classify`, `tga-report`) behind a clap subcommand interface.

#![warn(missing_docs)]

mod commands;

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use tga_core::config::Config;
use tga_core::db::Database;

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
}

/// Arguments for `tga collect`.
#[derive(Args, Debug)]
pub struct CollectArgs {
    /// Only collect from these repository names (comma-separated).
    #[arg(long, value_delimiter = ',')]
    pub repos: Vec<String>,
    /// Collect since date (ISO 8601, overrides config).
    #[arg(long)]
    pub since: Option<String>,
    /// Collect until date (ISO 8601, overrides config).
    #[arg(long)]
    pub until: Option<String>,
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

    // Open SQLite database (runs migrations on open).
    tracing::info!(path = %cli.database.display(), "opening database");
    let mut db = Database::open(&cli.database)?;

    match cli.command {
        Commands::Analyze(args) => commands::analyze::run(config, &mut db, args).await?,
        Commands::Collect(args) => commands::collect::run(config, &mut db, args).await?,
        Commands::Classify(args) => commands::classify::run(config, &mut db, args).await?,
        Commands::Report(args) => commands::report::run(config, &db, args)?,
    }

    Ok(())
}

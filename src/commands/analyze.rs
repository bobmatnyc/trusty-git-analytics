//! `tga analyze` — run the full pipeline (collect → classify → report).

use chrono::{Duration, Utc};
use tga::classify::ClassificationPipeline;
use tga::collect::CollectionPipeline;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::report::ReportPipeline;

use crate::AnalyzeArgs;

/// Run all three pipeline stages in sequence, honoring `--skip-collect`
/// and `--skip-classify` flags to allow partial re-runs.
///
/// When `args.dry_run` is set, the entire pipeline executes against a
/// transient in-memory SQLite database. The git walk, API calls, and
/// classification still run so the user sees what *would* have happened,
/// but the on-disk database is untouched.
pub async fn run(config: Config, db: &mut Database, args: AnalyzeArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Redirect writes to an in-memory database in dry-run mode. Note that
    // `--dry-run` implies starting from an empty state, so `--skip-collect`
    // becomes effectively a no-op (the shadow DB has no prior data).
    let mut shadow_db;
    let db: &mut Database = if args.dry_run {
        tracing::info!("Dry run — no database writes will occur");
        shadow_db = Database::open_in_memory()?;
        &mut shadow_db
    } else {
        db
    };

    // Apply output override up front so the final report stage sees it.
    if let Some(output) = args.output {
        let mut out = cfg.output.unwrap_or_default();
        out.directory = Some(output);
        cfg.output = Some(out);
    }

    // `--weeks N` overrides any `start_date` configured per-repository.
    if let Some(weeks) = args.weeks {
        let since = weeks_to_since(weeks);
        tracing::info!(weeks, since = %since, "applying --weeks collection window");
        for repo in &mut cfg.repositories {
            repo.since_date = Some(since.clone());
        }
    }

    if !args.skip_collect {
        tracing::info!("stage 1: collect");
        let collect_stats = CollectionPipeline::new(cfg.clone())
            .with_force(args.force)
            .run(db)
            .await?;
        println!(
            "Collected {} commits from {} authors ({} weeks collected, {} weeks skipped)",
            collect_stats.commits_collected,
            collect_stats.authors_resolved,
            collect_stats.weeks_collected,
            collect_stats.weeks_skipped,
        );
        if !collect_stats.errors.is_empty() {
            for e in &collect_stats.errors {
                eprintln!("  warning: {e}");
            }
        }
    } else {
        tracing::info!("stage 1: collect (skipped)");
    }

    if !args.skip_classify {
        tracing::info!("stage 2: classify");
        let classify_stats = ClassificationPipeline::new(cfg.clone()).run(db).await?;
        println!(
            "Classified {}/{} commits",
            classify_stats.classified, classify_stats.total_commits
        );
    } else {
        tracing::info!("stage 2: classify (skipped)");
    }

    tracing::info!("stage 3: report");
    let report_stats = ReportPipeline::new(cfg).run(db)?;
    println!(
        "Generated {} report file(s) ({} commits, {} authors)",
        report_stats.files_written.len(),
        report_stats.total_commits,
        report_stats.total_authors
    );
    for f in &report_stats.files_written {
        println!("  {}", f.display());
    }

    if args.dry_run {
        println!("Dry run complete. No changes persisted to the on-disk database.");
    }

    Ok(())
}

/// Convert a `--weeks N` value to an RFC3339 `since` timestamp.
///
/// Returns `(now - N weeks)` formatted as an RFC3339 string, which the
/// git extractor accepts as a lower bound on commit author time.
fn weeks_to_since(weeks: u32) -> String {
    let cutoff = Utc::now() - Duration::weeks(i64::from(weeks));
    cutoff.to_rfc3339()
}

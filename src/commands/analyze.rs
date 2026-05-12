//! `tga analyze` — run the full pipeline (collect → classify → report).

use tga::classify::ClassificationPipeline;
use tga::collect::CollectionPipeline;
use tga::core::config::Config;
use tga::core::db::Database;
use tga::report::ReportPipeline;

use crate::commands::date_range::resolve_date_range;
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

    // Resolve --weeks / --from / --to into a (since, until) pair.
    let (resolved_since, resolved_until) =
        resolve_date_range(args.weeks, args.from.as_deref(), args.to.as_deref(), None)?;
    if let Some(since) = resolved_since.as_ref() {
        tracing::info!(since = %since, "applying collection lower bound");
        for repo in &mut cfg.repositories {
            repo.since_date = Some(since.clone());
        }
    }
    if let Some(until) = resolved_until.as_ref() {
        tracing::info!(until = %until, "applying collection upper bound");
        for repo in &mut cfg.repositories {
            repo.until_date = Some(until.clone());
        }
    }

    if !args.skip_collect {
        tracing::info!("stage 1: collect");
        let collect_stats = CollectionPipeline::new(cfg.clone())
            .with_force(args.force)
            .with_no_fetch(args.no_fetch)
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

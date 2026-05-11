//! `tga analyze` — run the full pipeline (collect → classify → report).

use tga_classify::ClassificationPipeline;
use tga_collect::CollectionPipeline;
use tga_core::config::Config;
use tga_core::db::Database;
use tga_report::ReportPipeline;

use crate::AnalyzeArgs;

/// Run all three pipeline stages in sequence, honoring `--skip-collect`
/// and `--skip-classify` flags to allow partial re-runs.
pub async fn run(config: Config, db: &mut Database, args: AnalyzeArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Apply output override up front so the final report stage sees it.
    if let Some(output) = args.output {
        let mut out = cfg.output.unwrap_or_default();
        out.directory = Some(output);
        cfg.output = Some(out);
    }

    if !args.skip_collect {
        tracing::info!("stage 1: collect");
        let collect_stats = CollectionPipeline::new(cfg.clone()).run(db).await?;
        println!(
            "Collected {} commits from {} authors",
            collect_stats.commits_collected, collect_stats.authors_resolved
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

    Ok(())
}

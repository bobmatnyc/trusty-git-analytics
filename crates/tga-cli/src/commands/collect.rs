//! `tga collect` — stage 1 (git extraction) entry point.

use tga_collect::CollectionPipeline;
use tga_core::config::Config;
use tga_core::db::Database;

use crate::CollectArgs;

/// Run the collection stage against the provided database.
///
/// Applies CLI overrides (repository filter, since/until dates) on top of
/// the loaded YAML configuration before invoking
/// [`CollectionPipeline::run`].
pub async fn run(config: Config, db: &mut Database, args: CollectArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Filter repositories by name when --repos is supplied.
    if !args.repos.is_empty() {
        cfg.repositories.retain(|r| {
            let name = r.name.clone().unwrap_or_default();
            args.repos.contains(&name)
        });
        if cfg.repositories.is_empty() {
            tracing::warn!(
                "no repositories matched --repos filter ({:?}); nothing to do",
                args.repos
            );
        }
    }

    // Apply date overrides to every selected repository.
    if let Some(since) = args.since.as_ref() {
        for repo in &mut cfg.repositories {
            repo.since_date = Some(since.clone());
        }
    }
    if let Some(until) = args.until.as_ref() {
        for repo in &mut cfg.repositories {
            repo.until_date = Some(until.clone());
        }
    }

    let pipeline = CollectionPipeline::new(cfg);
    let stats = pipeline.run(db).await?;

    println!(
        "Collected {} commits from {} authors ({} PRs fetched)",
        stats.commits_collected, stats.authors_resolved, stats.prs_fetched
    );
    if !stats.errors.is_empty() {
        eprintln!(
            "Encountered {} warnings during collection:",
            stats.errors.len()
        );
        for e in &stats.errors {
            eprintln!("  warning: {e}");
        }
    }
    Ok(())
}

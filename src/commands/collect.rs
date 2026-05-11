//! `tga collect` — stage 1 (git extraction) entry point.

use chrono::{Duration, Utc};
use tga::collect::CollectionPipeline;
use tga::core::config::Config;
use tga::core::db::Database;

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

    // `--weeks N` is computed first and may be superseded by an explicit `--since`.
    let weeks_since = args.weeks.map(weeks_to_since);
    let effective_since = args.since.clone().or(weeks_since);

    // Apply date overrides to every selected repository.
    if let Some(since) = effective_since.as_ref() {
        tracing::info!(since = %since, "applying collection lower bound");
        for repo in &mut cfg.repositories {
            repo.since_date = Some(since.clone());
        }
    }
    if let Some(until) = args.until.as_ref() {
        for repo in &mut cfg.repositories {
            repo.until_date = Some(until.clone());
        }
    }

    let pipeline = CollectionPipeline::new(cfg).with_force(args.force);
    let stats = pipeline.run(db).await?;

    println!(
        "Collected {} commits from {} authors ({} PRs fetched, \
         {} weeks collected, {} weeks skipped)",
        stats.commits_collected,
        stats.authors_resolved,
        stats.prs_fetched,
        stats.weeks_collected,
        stats.weeks_skipped,
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

/// Convert a `--weeks N` value to an RFC3339 `since` timestamp.
///
/// Returns `(now - N weeks)` formatted as an RFC3339 string, which the
/// git extractor accepts as a lower bound on commit author time.
fn weeks_to_since(weeks: u32) -> String {
    let cutoff = Utc::now() - Duration::weeks(i64::from(weeks));
    cutoff.to_rfc3339()
}

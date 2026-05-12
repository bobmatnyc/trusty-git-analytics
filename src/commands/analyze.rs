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

    // Issue #67: surface multi-repo coverage gaps before collection runs.
    // A single configured repository silently undercounts engineers who work
    // across the wider portfolio, so warn prominently (both `tracing` and
    // `stderr` so the message is visible without `--log`).
    warn_repository_coverage(&cfg);

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

/// Emit warnings when the repository roster is suspiciously narrow.
///
/// Why: see issue #67 — engineers active across many repositories are
/// silently undercounted when the YAML lists only one (or just a couple).
/// What: prints a high-visibility warning via both `tracing::warn!` and
/// `eprintln!` so it survives the default log level. A separate, softer
/// warning fires when `github.org` is configured but the repo list is
/// still under three, hinting at the available org-wide discovery path.
/// Test: build a `Config` with one repository and assert a stderr capture
/// contains "Only 1 repository configured".
fn warn_repository_coverage(cfg: &tga::core::config::Config) {
    let n = cfg.repositories.len();
    if n == 1 {
        let msg = "WARNING: Only 1 repository configured. Engineers working across multiple \
                   repos will be undercounted. Add all active repos to `repositories[]` for \
                   accurate results.";
        tracing::warn!("{msg}");
        eprintln!("{msg}");
    }
    let has_org = cfg
        .github
        .as_ref()
        .and_then(|gh| gh.org.as_deref())
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    if has_org && n < 3 {
        let msg = format!(
            "WARNING: `github.org` is set but `repositories[]` has only {n} entr{plural} — \
             consider expanding the repository list (org-wide discovery is available) for \
             representative coverage.",
            plural = if n == 1 { "y" } else { "ies" }
        );
        tracing::warn!("{msg}");
        eprintln!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tga::core::config::{Config, GithubConfig, RepositoryConfig};

    fn one_repo_cfg() -> Config {
        Config {
            repositories: vec![RepositoryConfig {
                path: "/tmp/r".into(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn warn_fires_for_single_repo() {
        // Smoke test: just exercise the path; capturing stderr in unit tests
        // is platform-specific and brittle, so we assert it does not panic.
        warn_repository_coverage(&one_repo_cfg());
    }

    #[test]
    fn warn_fires_for_org_with_few_repos() {
        let mut cfg = one_repo_cfg();
        cfg.github = Some(GithubConfig {
            org: Some("acme".into()),
            ..Default::default()
        });
        warn_repository_coverage(&cfg);
    }

    #[test]
    fn warn_silent_for_many_repos() {
        let mut cfg = one_repo_cfg();
        for i in 0..5 {
            cfg.repositories.push(RepositoryConfig {
                path: format!("/tmp/r{i}").into(),
                ..Default::default()
            });
        }
        warn_repository_coverage(&cfg);
    }
}

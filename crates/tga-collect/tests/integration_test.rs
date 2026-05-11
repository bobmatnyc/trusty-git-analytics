//! Integration test: run collection against real Duetto repos.
//!
//! Skipped automatically if the repos don't exist locally — safe to run in CI.

use std::path::Path;
use tga_collect::CollectionPipeline;
use tga_core::config::Config;
use tga_core::db::Database;

#[tokio::test]
async fn collect_duetto_frontend() {
    let repo_path = Path::new("/Users/masa/Duetto/repos/duetto-frontend");
    if !repo_path.exists() {
        eprintln!("SKIP: Duetto repos not available");
        return;
    }

    // Load the config (same YAML as the Python tool).
    let config = Config::load(Path::new(
        "/Users/masa/Projects/trusty-git-analytics/configs/duetto-contractors.yaml",
    ))
    .expect("config load");

    // Only collect the first repo (duetto-frontend) for speed.
    // Clear `branch` so the collector falls back to HEAD — local clones may
    // be on `develop` instead of the configured `main`.
    let mut single_repo_config = config.clone();
    single_repo_config.repositories = config
        .repositories
        .into_iter()
        .filter(|r| r.name.as_deref() == Some("duetto-frontend"))
        .map(|mut r| {
            r.branch = None;
            r
        })
        .collect();

    assert!(
        !single_repo_config.repositories.is_empty(),
        "duetto-frontend not in config"
    );

    // Open an in-memory DB for this test.
    let mut db = Database::open_in_memory().expect("db open");

    let pipeline = CollectionPipeline::new(single_repo_config);
    let stats = pipeline.run(&mut db).await.expect("collection run");

    println!(
        "Collected {} commits, {} authors",
        stats.commits_collected, stats.authors_resolved
    );
    assert!(
        stats.commits_collected > 0,
        "expected commits to be collected"
    );
    assert!(
        stats.authors_resolved > 0,
        "expected authors to be resolved"
    );
}

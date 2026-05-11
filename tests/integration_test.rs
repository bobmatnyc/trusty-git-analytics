//! Integration test: run collection against a real local git repository.
//!
//! Set the `INTEGRATION_REPO_PATH` environment variable to the path of a local
//! git repository to enable this test. Skipped automatically when the variable
//! is unset — safe to run in CI without any local repos present.

use std::path::PathBuf;
use tga::collect::CollectionPipeline;
use tga::core::config::{Config, RepositoryConfig};
use tga::core::db::Database;

#[tokio::test]
async fn collect_integration_repo() {
    // Set INTEGRATION_REPO_PATH env var to a local git repo to run this test.
    let repo_path = match std::env::var("INTEGRATION_REPO_PATH") {
        Ok(p) => PathBuf::from(p),
        Err(_) => {
            eprintln!("SKIP: set INTEGRATION_REPO_PATH to run integration test");
            return;
        }
    };

    if !repo_path.exists() {
        eprintln!(
            "SKIP: INTEGRATION_REPO_PATH does not exist: {}",
            repo_path.display()
        );
        return;
    }

    // Build a minimal config pointing at the provided repo.
    let repo_name = repo_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("integration-repo")
        .to_string();
    let config = Config {
        repositories: vec![RepositoryConfig {
            path: repo_path.clone(),
            name: Some(repo_name),
            branch: None,
            ..Default::default()
        }],
        ..Default::default()
    };

    // Open an in-memory DB for this test — nothing is written to disk.
    let mut db = Database::open_in_memory().expect("db open");

    let pipeline = CollectionPipeline::new(config);
    let stats = pipeline.run(&mut db).await.expect("collection run");

    println!(
        "Collected {} commits, {} authors",
        stats.commits_collected, stats.authors_resolved
    );
    assert!(
        stats.commits_collected > 0,
        "expected commits to be collected from {}",
        repo_path.display()
    );
    assert!(
        stats.authors_resolved > 0,
        "expected authors to be resolved"
    );
}

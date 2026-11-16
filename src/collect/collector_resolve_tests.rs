//! #266: GitHub repo resolution errors in `build_pr_providers` fail the
//! stage the run reports.
//!
//! Loaded via `#[cfg(test)] #[path]` in `collector.rs`.

use super::{CollectionPipeline, CollectionStats};
use crate::collect::bitbucket::WorkspaceDiscovery;
use crate::core::config::{Config, GithubConfig, RepositoryConfig};

/// Why (#266): a malformed `github_repo` used to drop the repo from PR
/// collection with only a debug line. It must fail the GitHub stage.
/// What: a hand-built config (no load check) with `fetch_prs: true` and
/// `github_repo: bad`; build the PR providers.
/// Test: this test.
#[test]
fn malformed_github_repo_fails_the_pr_stage() {
    let mut repo = RepositoryConfig::new("/no-such-clone-266");
    repo.github_repo = Some("bad".into());
    let config = Config {
        github: Some(GithubConfig {
            fetch_prs: true,
            token: Some("t".into()),
            ..GithubConfig::default()
        }),
        repositories: vec![repo],
        ..Config::default()
    };
    let pipeline = CollectionPipeline::new(config);
    let mut stats = CollectionStats::default();
    let providers = pipeline.build_pr_providers(&mut stats, &[], &WorkspaceDiscovery::default());
    assert!(providers.is_empty());
    assert!(
        stats
            .errors
            .iter()
            .any(|f| f.message.contains("github_repo")
                || f.message.contains("repo resolution failed")),
        "no stage fault recorded: {:?}",
        stats.errors
    );
}

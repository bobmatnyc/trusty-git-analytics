//! Live integration test: hit the real Bitbucket Cloud API.
//!
//! Set `BITBUCKET_TEST_TOKEN`, `BITBUCKET_TEST_EMAIL`,
//! `BITBUCKET_TEST_WORKSPACE`, and `BITBUCKET_TEST_REPO` to enable this test.
//! Skipped automatically when any are unset, so CI never hits the live API.
//!
//! The token slot accepts an Atlassian account-level API token (created at
//! <https://id.atlassian.com/manage-profile/security/api-tokens>), used via
//! HTTP Basic auth with the email as username — i.e. it is passed through the
//! `app_password` field on `BitbucketConfig`, not the `token` field (which is
//! reserved for workspace/repo Bearer tokens).
//!
//! Run with:
//!
//! ```sh
//! cargo test --test bitbucket_live -- --nocapture
//! ```

use std::collections::{HashMap, HashSet};

use tga::collect::bitbucket::BitbucketClient;
use tga::core::config::BitbucketConfig;
use tga::core::models::PrState;

#[tokio::test]
async fn fetch_pull_requests_from_live_bitbucket() {
    let Some((token, email, workspace, repo_slug)) = read_env() else {
        eprintln!(
            "SKIP: set BITBUCKET_TEST_TOKEN, BITBUCKET_TEST_EMAIL, \
             BITBUCKET_TEST_WORKSPACE, BITBUCKET_TEST_REPO to run live test"
        );
        return;
    };

    let client = BitbucketClient::new(&BitbucketConfig {
        username: Some(email),
        app_password: Some(token),
        workspace: Some(workspace.clone()),
        repo_slug: Some(repo_slug.clone()),
        fetch_prs: true,
        ..Default::default()
    })
    .expect("client builds");

    let prs = client
        .fetch_pull_requests()
        .await
        .expect("live fetch succeeded");

    // ---- Sanity asserts ----
    let mut seen_ids: HashSet<u64> = HashSet::new();
    for pr in &prs {
        assert!(
            !pr.title.is_empty(),
            "PR #{} has empty title — schema drift?",
            pr.pr_number
        );
        assert!(
            seen_ids.insert(pr.pr_number),
            "duplicate PR id {} in fetch result — pagination bug",
            pr.pr_number
        );
        // Merged PRs should carry a merge time (we map updated_on into merged_at
        // for state == Merged in the mapper).
        if matches!(pr.state, PrState::Merged) {
            assert!(
                pr.merged_at.is_some(),
                "merged PR #{} has no merged_at timestamp",
                pr.pr_number
            );
        }
    }

    // ---- Summary ----
    let mut by_state: HashMap<&'static str, usize> = HashMap::new();
    for pr in &prs {
        *by_state.entry(pr.state.as_str()).or_insert(0) += 1;
    }
    let merged_with_sha = prs
        .iter()
        .filter(|p| matches!(p.state, PrState::Merged))
        .filter(|p| p.commit_shas.contains(':') || p.commit_shas.contains('"'))
        .count();
    let oldest = prs.iter().map(|p| p.created_at).min();
    let newest = prs.iter().map(|p| p.created_at).max();
    let top_authors = top_n_authors(&prs, 5);

    eprintln!("---- Bitbucket live fetch ----");
    eprintln!("workspace/repo : {workspace}/{repo_slug}");
    eprintln!("total PRs      : {}", prs.len());
    eprintln!("by state       : {by_state:?}");
    eprintln!("merged w/ SHA  : {merged_with_sha}");
    eprintln!("oldest created : {oldest:?}");
    eprintln!("newest created : {newest:?}");
    eprintln!("top authors    :");
    for (name, n) in top_authors {
        eprintln!("  {n:>4}  {name}");
    }
}

fn read_env() -> Option<(String, String, String, String)> {
    let token = std::env::var("BITBUCKET_TEST_TOKEN").ok()?;
    let email = std::env::var("BITBUCKET_TEST_EMAIL").ok()?;
    let workspace = std::env::var("BITBUCKET_TEST_WORKSPACE").ok()?;
    let repo_slug = std::env::var("BITBUCKET_TEST_REPO").ok()?;
    if token.trim().is_empty()
        || email.trim().is_empty()
        || workspace.trim().is_empty()
        || repo_slug.trim().is_empty()
    {
        return None;
    }
    Some((token, email, workspace, repo_slug))
}

fn top_n_authors(prs: &[tga::core::models::PullRequest], n: usize) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for pr in prs {
        let key = if pr.author.is_empty() {
            "<unknown>".to_string()
        } else {
            pr.author.clone()
        };
        *counts.entry(key).or_insert(0) += 1;
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

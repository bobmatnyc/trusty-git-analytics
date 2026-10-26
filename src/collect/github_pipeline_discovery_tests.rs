//! #188: org-wide repository discovery is opt-in.
//!
//! Why: with `github.org` set, `collect` discovered every repo in the org and
//! fetched PRs for all of them, whatever `repositories:` listed. A 3-repo
//! config scanned about 590 repos. `github.discover_orgs` now defaults to
//! `false`; these tests assert on the requests a mock server receives.

use std::path::PathBuf;

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::run_github_org_discovery;
use crate::collect::github::budget::RunBudget;
use crate::collect::github::org_discovery::resolve_github_repos_with_discovered;
use crate::core::config::{GithubConfig, RepositoryConfig};

/// A mock GitHub whose `acme` org lists `repos` as `(full_name, archived)`.
async fn org_server(repos: &[(&str, bool)]) -> MockServer {
    let server = MockServer::start().await;
    let body: Vec<serde_json::Value> = repos
        .iter()
        .map(|(name, archived)| serde_json::json!({ "full_name": name, "archived": archived }))
        .collect();
    Mock::given(method("GET"))
        .and(path("/orgs/acme/repos"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    server
}

/// The number of org-discovery requests `server` received.
async fn discovery_requests(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path().starts_with("/orgs/"))
        .count()
}

/// One configured repository, `acme/widget`.
fn listed_repos() -> Vec<RepositoryConfig> {
    vec![RepositoryConfig {
        path: PathBuf::from("/nonexistent/widget"),
        name: Some("widget".to_string()),
        ..Default::default()
    }]
}

fn parse(yaml: &str) -> GithubConfig {
    serde_yaml::from_str(yaml).expect("github section parses")
}

/// A config that lists repos and omits `discover_orgs` makes no discovery
/// request and fetches only the listed repos.
#[tokio::test]
async fn listed_repos_without_discover_orgs_make_no_discovery_request() {
    let server = org_server(&[("acme/widget", false), ("acme/other", false)]).await;
    let cfg = parse("org: acme\nfetch_prs: true\n");

    let discovered = run_github_org_discovery(&cfg, &RunBudget::new(), &server.uri()).await;

    assert_eq!(
        discovery_requests(&server).await,
        0,
        "no org discovery request may be made when discover_orgs is omitted"
    );
    assert!(discovered.is_empty(), "{discovered:?}");
    assert_eq!(
        resolve_github_repos_with_discovered(&cfg, &listed_repos(), &discovered),
        vec![("acme".to_string(), "widget".to_string())],
        "only the listed repo is fetched"
    );
}

/// `discover_orgs: true` still discovers the org's repos and unions them in.
#[tokio::test]
async fn discover_orgs_true_still_discovers_org_repos() {
    let server = org_server(&[("acme/widget", false), ("acme/other", false)]).await;
    let cfg = parse("org: acme\nfetch_prs: true\ndiscover_orgs: true\n");

    let discovered = run_github_org_discovery(&cfg, &RunBudget::new(), &server.uri()).await;

    assert_eq!(discovery_requests(&server).await, 1);
    assert_eq!(
        resolve_github_repos_with_discovered(&cfg, &listed_repos(), &discovered),
        vec![
            ("acme".to_string(), "widget".to_string()),
            ("acme".to_string(), "other".to_string()),
        ]
    );
}

/// Discovery drops archived repositories (#188's second closure condition).
#[tokio::test]
async fn org_discovery_skips_archived_repositories() {
    let server = org_server(&[("acme/live", false), ("acme/old", true)]).await;
    let cfg = parse("org: acme\nfetch_prs: true\ndiscover_orgs: true\n");

    let discovered = run_github_org_discovery(&cfg, &RunBudget::new(), &server.uri()).await;

    assert_eq!(
        discovered,
        vec![("acme".to_string(), "live".to_string())],
        "an archived repo must not be discovered"
    );
}

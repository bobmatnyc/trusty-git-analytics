//! #188: token resolution for every GitHub client.
//!
//! Why: the PR client read only `github.token`, so a run with `GITHUB_TOKEN`
//! exported and no token in config sent its PR calls unauthenticated. These
//! tests pin the precedence (config, then env), the error arms (nothing usable
//! means no `Authorization` header and `has_token() == false`), and that the
//! token never reaches an error message. The env is injected through a lookup
//! closure, as in #5313, so no test sets a process env var.

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::collect::github::repo_resolver::resolve_github_token_with;
use crate::collect::github::GitHubClient;
use crate::collect::pr_provider::PrProvider;
use crate::core::config::GithubConfig;

/// A lookup that resolves only `GITHUB_TOKEN`, to `value`.
fn env_token(value: &'static str) -> impl Fn(&str) -> Option<String> {
    move |name| (name == "GITHUB_TOKEN").then(|| value.to_string())
}

/// A lookup where no variable is set.
fn no_env(_: &str) -> Option<String> {
    None
}

/// A PR-fetching GitHub config with `token` as its `github.token`.
fn cfg(token: Option<&str>) -> GithubConfig {
    GithubConfig {
        token: token.map(str::to_string),
        fetch_prs: true,
        ..GithubConfig::default()
    }
}

/// The `Authorization` header each request to the PR list carried.
async fn pr_list_auth_headers(token: Option<String>) -> (bool, Vec<Option<String>>) {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/repos/acme/widget/pulls"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    let client = GitHubClient::new_for_prs_with_token(
        vec![("acme".to_string(), "widget".to_string())],
        token,
    )
    .expect("client builds")
    .with_api_base(server.uri());
    let has_token = client.has_token();
    let _ = PrProvider::fetch_pull_requests(&client).await;
    let headers = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.url.path() == "/repos/acme/widget/pulls")
        .map(|r| {
            r.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string)
        })
        .collect();
    (has_token, headers)
}

/// `GITHUB_TOKEN` is the fallback when `github.token` is unset or blank, and a
/// non-blank `github.token` wins over it.
#[test]
fn env_github_token_is_the_fallback_and_config_wins() {
    assert_eq!(
        resolve_github_token_with(&cfg(None), env_token("env-tok")).as_deref(),
        Some("env-tok"),
        "unset github.token must fall back to GITHUB_TOKEN"
    );
    assert_eq!(
        resolve_github_token_with(&cfg(Some("  ")), env_token("env-tok")).as_deref(),
        Some("env-tok"),
        "a blank github.token must fall back to GITHUB_TOKEN"
    );
    assert_eq!(
        resolve_github_token_with(&cfg(Some("cfg-tok")), env_token("env-tok")).as_deref(),
        Some("cfg-tok"),
        "a configured github.token wins over GITHUB_TOKEN"
    );
}

/// Error arm: nothing usable resolves to no token at all, never an empty one.
/// An unset `${GITHUB_TOKEN}` placeholder used to resolve to `Some("")`, which
/// sent `Authorization: Bearer ` and reported the client as authenticated.
#[test]
fn nothing_usable_resolves_to_no_token() {
    assert_eq!(resolve_github_token_with(&cfg(None), no_env), None);
    assert_eq!(
        resolve_github_token_with(&cfg(Some("${GITHUB_TOKEN}")), no_env),
        None,
        "an unset placeholder is no token"
    );
    assert_eq!(
        resolve_github_token_with(&cfg(None), env_token(" \t")),
        None,
        "a blank GITHUB_TOKEN is no token"
    );
}

/// With `GITHUB_TOKEN` set and no `github.token`, PR fetches carry it.
#[tokio::test]
async fn pr_fetch_authenticates_with_the_env_token() {
    let token = resolve_github_token_with(&cfg(None), env_token("env-tok"));
    let (has_token, headers) = pr_list_auth_headers(token).await;
    assert!(has_token, "the client must report itself authenticated");
    assert!(!headers.is_empty(), "the PR list must be requested");
    assert!(
        headers
            .iter()
            .all(|h| h.as_deref() == Some("Bearer env-tok")),
        "every PR request must carry the env token, got {headers:?}"
    );
}

/// Error arm: with no usable token the client says it is unauthenticated and
/// sends no `Authorization` header, so the collector's anonymous warning fires
/// and nothing proceeds as if authenticated.
#[tokio::test]
async fn pr_fetch_without_a_token_is_not_treated_as_authenticated() {
    let token = resolve_github_token_with(&cfg(Some("${GITHUB_TOKEN}")), no_env);
    let (has_token, headers) = pr_list_auth_headers(token).await;
    assert!(!has_token, "no usable token must read as unauthenticated");
    assert!(!headers.is_empty(), "the PR list must be requested");
    assert!(
        headers.iter().all(Option::is_none),
        "no Authorization header may be sent, got {headers:?}"
    );
}

/// Error arm: a token that is not a valid header value fails construction, and
/// the error names neither the token nor any part of it.
#[test]
fn an_invalid_token_fails_construction_without_echoing_it() {
    let token = resolve_github_token_with(&cfg(None), env_token("ghp_secret\nzq9xk"));
    let err = match GitHubClient::new_for_prs_with_token(
        vec![("acme".to_string(), "widget".to_string())],
        token,
    ) {
        Ok(_) => panic!("an invalid token must not build a client"),
        Err(e) => e.to_string(),
    };
    assert!(
        !err.contains("ghp_secret") && !err.contains("zq9xk"),
        "the error must not echo the token: {err}"
    );
}

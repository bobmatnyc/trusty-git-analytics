//! GitHub repository resolution helpers.
//!
//! Why: decouples the URL-parsing / repo-resolution logic from the HTTP client
//! so each can be tested in isolation without touching the network.
//! What: provides `resolve_github_repos`, `parse_slug`, `build_http_client`,
//! and the URL-extraction helpers used by collection and org-discovery.
//! Test: `resolve_github_repos_*` and `extract_owner_repo_from_url_*` unit
//! tests live in `github/client.rs` (in the `tests` module) because they
//! exercise the full resolution pipeline end-to-end.

use tracing::{debug, warn};

use crate::collect::errors::{CollectError, Result};
use crate::collect::github::client::USER_AGENT_VALUE;
use crate::core::config::{GithubConfig, RepositoryConfig};

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};

use crate::collect::env_expand::expand_env_var_with;

/// Parse an `owner/name` slug, returning a [`CollectError::Config`] on
/// malformed input. Extracted so both [`super::client::GitHubClient::new`] and
/// [`resolve_github_repos`] share one error message format.
///
/// Why: centralises slug parsing so every consumer gets the same error message
/// and validation rules.
/// What: splits on `/`, validates both parts are non-empty.
/// Test: `parse_slug_validates_input` in `client.rs` tests.
pub fn parse_slug(slug: &str) -> Result<(String, String)> {
    // #266: one slug rule, shared with config load.
    let (owner, repo) = crate::core::config::split_github_slug(slug).ok_or_else(|| {
        CollectError::Config(format!("github repo must be 'owner/name', got '{slug}'"))
    })?;
    Ok((owner.to_string(), repo.to_string()))
}

/// The token every GitHub client built from `config` authenticates with.
///
/// Reads the process environment; see [`resolve_github_token_with`].
pub(crate) fn resolve_github_token(config: &GithubConfig) -> Option<String> {
    resolve_github_token_with(config, |name| std::env::var(name).ok())
}

/// [`resolve_github_token`], with the environment lookup supplied by the caller.
///
/// Why (#188): the PR client read only `github.token`, while validation also
/// accepted `GITHUB_TOKEN`, so a run that passed validation sent its PR calls
/// unauthenticated. One resolver for every GitHub client ends that split. The
/// lookup is injected, as in #5313, so tests never set a process env var.
/// What: `github.token`, with a `${VAR}` placeholder expanded through
/// `lookup`, when it is non-blank; otherwise `GITHUB_TOKEN` from `lookup` when
/// that is non-blank; otherwise `None`. The result is trimmed and never empty,
/// so an unset placeholder cannot read as a credential.
/// Test: `crate::collect::github::token_tests`.
pub(crate) fn resolve_github_token_with(
    config: &GithubConfig,
    lookup: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let usable = |v: String| {
        let v = v.trim();
        (!v.is_empty()).then(|| v.to_string())
    };
    config
        .token
        .as_deref()
        .and_then(|raw| usable(expand_env_var_with(raw, &lookup)))
        // #188: env fallback, the same precedence validation applies.
        .or_else(|| lookup(trusty_common::env_vars::ENV_GITHUB_TOKEN).and_then(usable))
}

/// Build the shared authenticated `reqwest::Client` for all GitHub HTTP traffic.
///
/// Why: org-discovery, reviewer-ingestion, and the PR client all need the same
/// authed client; `pub(crate)` visibility avoids duplicating header-build logic
/// without widening the public API surface.
/// What: [`build_http_client_with_token`] with the token from
/// [`resolve_github_token`].
/// Test: used by all GitHub call sites — covered indirectly by their tests.
pub(crate) fn build_http_client(config: &GithubConfig) -> Result<reqwest::Client> {
    build_http_client_with_token(resolve_github_token(config).as_deref())
}

/// Build the GitHub `reqwest::Client` for an already-resolved token.
///
/// What: sets `Authorization: Bearer <token>` when `token` is `Some`, the
/// GitHub `Accept` header, and a 30-second timeout.
///
/// # Errors
///
/// [`CollectError::Config`] when the token is not a valid header value. The
/// message never contains the token.
pub(crate) fn build_http_client_with_token(token: Option<&str>) -> Result<reqwest::Client> {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/vnd.github+json"),
    );
    if let Some(token) = token {
        let val = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| CollectError::Config(format!("invalid token header: {e}")))?;
        headers.insert(AUTHORIZATION, val);
    }
    // #255: shared builder adds a connect timeout to the 30 s total.
    Ok(
        crate::core::http::client_builder(crate::core::http::DEFAULT_TOTAL_TIMEOUT)
            .default_headers(headers)
            .build()?,
    )
}

/// What a clone's `origin` remote yields for GitHub addressing.
///
/// #266: each fallback outcome logs its own per-repo warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OriginProbe {
    /// `origin` parsed to a GitHub `(owner, repo)`.
    GitHub((String, String)),
    /// `origin` exists, but its URL is not GitHub or does not parse.
    NotGitHub(String),
    /// The path is not a git repo, or it has no `origin` with a URL.
    Missing,
}

/// Probe the `origin` remote of the clone at `repo_path`.
///
/// Why (#266): origin is the canonical GitHub `owner/name`; the config
/// `name` is a report label and can differ from it.
/// What: opens the repo with `git2` and classifies `origin`'s URL.
/// Test: `repo_resolver_tests::probe_origin_classifies_each_outcome`.
pub(crate) fn probe_origin(repo_path: &std::path::Path) -> OriginProbe {
    let Ok(repo) = git2::Repository::open(repo_path) else {
        return OriginProbe::Missing;
    };
    let Ok(remote) = repo.find_remote("origin") else {
        return OriginProbe::Missing;
    };
    let Some(url) = remote.url() else {
        return OriginProbe::Missing;
    };
    match extract_owner_repo_from_url(url) {
        Some(pair) => OriginProbe::GitHub(pair),
        None => OriginProbe::NotGitHub(url.to_string()),
    }
}

/// GitHub host this resolver accepts a remote from.
///
/// The shared parser is host-agnostic; the GitHub REST client this feeds is
/// not, so the filter lives here rather than in `trusty-common`.
const GITHUB_HOST: &str = "github.com";

/// Pure-string helper: extracts `owner/name` from a GitHub remote URL string.
/// Returns `None` for non-GitHub URLs or malformed input.
///
/// Why: separating the pure URL-parse logic from the disk-touching
/// [`probe_origin`] makes the URL-parse path independently testable.
/// What: delegates to `trusty_common::github_path::parse_remote_url` — the
/// workspace's one git-remote-URL parser (#6657) — and keeps only the
/// GitHub-host filter this crate needs. HTTPS, SSH scp-syntax, `ssh://`, and
/// `https://user@` forms all reach it through that parser.
/// Test: `extract_owner_repo_from_url_handles_common_forms` in `client.rs` tests.
pub fn extract_owner_repo_from_url(url: &str) -> Option<(String, String)> {
    let remote = trusty_common::github_path::parse_remote_url(url).ok()?;
    (remote.host == GITHUB_HOST).then_some((remote.owner, remote.repo))
}

/// Case-folded `(owner, repo)` used to dedup GitHub targets within a run.
///
/// #266: GitHub slugs are case-insensitive, so origin `acme/api` and a
/// discovered `Acme/api` are one repository and must be fetched once.
pub(crate) fn slug_key(pair: &(String, String)) -> (String, String) {
    (pair.0.to_lowercase(), pair.1.to_lowercase())
}

/// The `(owner, repo)` the config itself names: `repo.org` (else
/// `default_org`) plus `name` (else the path basename).
fn configured_pair(
    repo_cfg: &RepositoryConfig,
    default_org: Option<&str>,
) -> Option<(String, String)> {
    let repo_name = repo_cfg.name.clone().or_else(|| {
        repo_cfg
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
    })?;
    let owner = repo_cfg.org.as_deref().or(default_org)?;
    (!repo_name.is_empty()).then(|| (owner.to_string(), repo_name))
}

/// Resolve one repository entry to the GitHub `(owner, repo)` its API calls
/// target.
///
/// Why (#266): the config `name` is a report label. `duetto-repos` lives at
/// `duettoresearch/duetto`, so `org + name` 404'd on every PR fetch.
/// What: precedence, first match wins:
/// 1. the entry's `github_repo: owner/name`;
/// 2. the clone's `origin` remote, when it is a GitHub URL;
/// 3. `repo.org` (else `default_org`) plus `name` (else the path basename).
///
/// When origin equals the configured org + name ignoring case, the
/// configured spelling is returned, so stored PR rows keep their key. When
/// they differ, one warning names both. A missing, non-GitHub or unparseable
/// `origin` logs a per-repo warning before step 3. `Ok(None)` means nothing
/// resolves. Shared by the PR fetcher and `tga deployments`.
///
/// # Errors
///
/// [`CollectError::Config`] when `github_repo` is not `owner/name`. Config
/// load rejects that first; this arm keeps a hand-built config from
/// dropping the repo silently.
///
/// Test: `repo_resolver_tests`.
pub(crate) fn resolve_repo_slug(
    repo_cfg: &RepositoryConfig,
    default_org: Option<&str>,
) -> Result<Option<(String, String)>> {
    // #266: an explicit `github_repo` wins; a malformed one is an error.
    if let Some(slug) = &repo_cfg.github_repo {
        return parse_slug(slug).map(Some);
    }

    let configured = configured_pair(repo_cfg, default_org);
    // #266: origin first; `name` is only the fallback.
    match probe_origin(&repo_cfg.path) {
        OriginProbe::GitHub(origin) => match configured {
            // #266: same repo, legacy spelling — keep the stored key.
            Some(cfg) if slug_key(&cfg) == slug_key(&origin) => Ok(Some(cfg)),
            Some(cfg) => {
                warn!(
                    path = %repo_cfg.path.display(),
                    origin = %format!("{}/{}", origin.0, origin.1),
                    configured = %format!("{}/{}", cfg.0, cfg.1),
                    "origin remote differs from configured org + name; using origin"
                );
                Ok(Some(origin))
            }
            None => Ok(Some(origin)),
        },
        OriginProbe::NotGitHub(url) => {
            warn!(
                path = %repo_cfg.path.display(),
                url = %url,
                "origin remote is not a GitHub URL; falling back to org + name"
            );
            Ok(configured)
        }
        OriginProbe::Missing => {
            warn!(
                path = %repo_cfg.path.display(),
                "repository has no readable origin remote; falling back to org + name"
            );
            Ok(configured)
        }
    }
}

/// Resolve the set of `(owner, repo)` pairs the GitHub PR fetcher should
/// scan, given the GitHub config and the project's repository list.
///
/// Resolution rules:
/// 1. `github.repo` (single-repo mode) — when set, returns a single-entry
///    list and ignores `repositories[]` / `github.org`.
/// 2. Each `RepositoryConfig` through [`resolve_repo_slug`] with
///    `github.org` as the default owner: `github_repo`, then `origin`, then
///    org + name.
/// 3. Deduplicate case-insensitively; preserve first-seen order and spelling.
///
/// Why: org-wide deployments (issue #87) need to drive PR collection from
/// `repositories[]` rather than a single `github.repo`.
/// What: walks the rules above and returns a deduped vec.
///
/// # Errors
///
/// #266: [`CollectError::Config`] when any entry's `github_repo` is
/// malformed. The caller fails the stage; no repo drops out silently.
///
/// Test: `resolve_github_repos_*` cases in `client.rs` tests and
/// `repo_resolver_tests`.
pub fn resolve_github_repos(
    github: &GithubConfig,
    repositories: &[RepositoryConfig],
) -> Result<Vec<(String, String)>> {
    if let Some(slug) = &github.repo {
        if let Ok(pair) = parse_slug(slug) {
            return Ok(vec![pair]);
        } else {
            tracing::warn!(slug = %slug, "github.repo is malformed; falling back to repositories[]");
        }
    }

    let mut out: Vec<(String, String)> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();

    for repo_cfg in repositories {
        // #266: one shared per-repo resolver for PRs and deployments.
        if let Some(p) = resolve_repo_slug(repo_cfg, github.org.as_deref())? {
            // #266: case-insensitive dedup.
            if seen.insert(slug_key(&p)) {
                out.push(p);
            }
        } else {
            debug!(
                path = %repo_cfg.path.display(),
                "could not resolve owner/repo for repository; skipping for GitHub PR fetch"
            );
        }
    }

    Ok(out)
}

#[cfg(test)]
#[path = "repo_resolver_tests.rs"]
mod repo_resolver_tests;

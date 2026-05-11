//! Minimal GitHub REST API v3 client for fetching pull requests.

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use rusqlite::params;
use serde::Deserialize;
use tracing::{debug, warn};

use crate::collect::errors::{CollectError, Result};
use crate::core::config::GithubConfig;
use crate::core::db::Database;
use crate::core::models::{PrState, PullRequest};

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// GitHub REST API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
const PAGE_SIZE: u32 = 100;

/// Async GitHub REST client.
pub struct GitHubClient {
    client: reqwest::Client,
    token: Option<String>,
    /// `owner` in `owner/repo` (organization or user).
    owner: String,
    /// `repo` in `owner/repo`.
    repo: String,
}

#[derive(Debug, Deserialize)]
struct ApiPull {
    number: u64,
    title: String,
    user: Option<ApiUser>,
    state: String,
    created_at: DateTime<Utc>,
    merged_at: Option<DateTime<Utc>>,
    #[serde(default)]
    merge_commit_sha: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ApiUser {
    login: String,
}

impl GitHubClient {
    /// Build a client from a [`GithubConfig`].
    ///
    /// The config's `repo` field is expected in `owner/name` form. If the
    /// org-only mode is in use (`org` set, `repo` unset), per-repo calls
    /// will fail until a concrete repo is selected.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `repo` is missing or malformed.
    /// - [`CollectError::Http`] if the underlying `reqwest::Client` cannot
    ///   be built.
    pub fn new(config: &GithubConfig) -> Result<Self> {
        let repo_slug = config
            .repo
            .as_ref()
            .ok_or_else(|| CollectError::Config("github.repo is required (owner/name)".into()))?;
        let (owner, repo) = repo_slug.split_once('/').ok_or_else(|| {
            CollectError::Config(format!(
                "github.repo must be 'owner/name', got '{repo_slug}'"
            ))
        })?;

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/vnd.github+json"),
        );
        if let Some(token) = &config.token {
            let val = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| CollectError::Config(format!("invalid token header: {e}")))?;
            headers.insert(AUTHORIZATION, val);
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        Ok(Self {
            client,
            token: config.token.clone(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        })
    }

    /// Fetch all PRs (open + closed + merged) by paginating through the
    /// GitHub REST API.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport or non-success status,
    /// and [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        let mut out: Vec<PullRequest> = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls?state=all&per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            debug!(url = %url, "GET");
            let resp = self.client.get(&url).send().await?;

            // Respect rate-limit hints.
            if let Some(rem) = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<u32>().ok())
            {
                if rem < 5 {
                    warn!(remaining = rem, "GitHub rate limit nearly exhausted");
                }
            }

            let resp = resp.error_for_status()?;
            let pulls: Vec<ApiPull> = resp.json().await?;
            if pulls.is_empty() {
                break;
            }
            let n = pulls.len();
            for p in pulls {
                let state = if p.merged_at.is_some() {
                    PrState::Merged
                } else if p.state == "closed" {
                    PrState::Closed
                } else {
                    PrState::Open
                };
                let commit_shas = match &p.merge_commit_sha {
                    Some(s) => serde_json::to_string(&vec![s.clone()])?,
                    None => "[]".to_string(),
                };
                out.push(PullRequest {
                    id: 0,
                    pr_number: p.number,
                    title: p.title,
                    author: p.user.map(|u| u.login).unwrap_or_default(),
                    state,
                    created_at: p.created_at,
                    merged_at: p.merged_at,
                    commit_shas,
                });
            }
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Persist a batch of [`PullRequest`] rows into the database.
    ///
    /// Existing rows with the same `(pr_number)` are replaced.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::core::TgaError::DbError`] on SQL failures.
    pub fn store_pull_requests(
        &self,
        db: &Database,
        prs: &[PullRequest],
    ) -> crate::core::Result<usize> {
        let conn = db.connection();
        let mut count = 0usize;
        for pr in prs {
            conn.execute(
                "INSERT OR REPLACE INTO pull_requests \
                 (pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    pr.pr_number as i64,
                    pr.title,
                    pr.author,
                    pr.state.as_str(),
                    pr.created_at.to_rfc3339(),
                    pr.merged_at.map(|t| t.to_rfc3339()),
                    pr.commit_shas,
                ],
            )?;
            count += 1;
        }
        Ok(count)
    }

    /// Whether this client was constructed with an authentication token.
    pub fn has_token(&self) -> bool {
        self.token.is_some()
    }
}

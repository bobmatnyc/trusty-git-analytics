//! Minimal GitHub REST API v3 client for fetching pull requests.

use std::time::Duration;

use chrono::{DateTime, Utc};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, AUTHORIZATION, USER_AGENT};
use rusqlite::params;
use serde::Deserialize;
use tracing::{debug, warn};

use async_trait::async_trait;

use crate::collect::errors::{CollectError, Result};
use crate::collect::pr_provider::PrProvider;
use crate::core::config::GithubConfig;
use crate::core::db::Database;
use crate::core::models::{PrState, PullRequest};

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";
/// GitHub REST API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";
/// Page size for paginated list endpoints (GitHub max is 100).
const PAGE_SIZE: u32 = 100;
/// Maximum retry attempts for transient failures (5xx, 429).
const MAX_RETRIES: u32 = 3;
/// Base delay (in milliseconds) for exponential backoff: 1s, 2s, 4s.
const RETRY_BASE_MS: u64 = 1000;

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

/// A GitHub issue as returned by the REST API.
///
/// This is the normalized payload returned by
/// [`GitHubClient::fetch_issue`]. Only the subset of fields used by the
/// project-management adapter are deserialized.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubIssue {
    /// Issue number (the `N` in `#N`).
    pub number: u64,
    /// Issue title / summary.
    pub title: String,
    /// Workflow state — `"open"` or `"closed"`.
    pub state: String,
    /// Web URL to the issue on github.com.
    pub html_url: String,
    /// Labels applied to the issue.
    #[serde(default)]
    pub labels: Vec<GhLabel>,
    /// Issue body / description (Markdown). May be absent or empty.
    #[serde(default)]
    pub body: Option<String>,
}

/// A GitHub label as returned alongside a [`GitHubIssue`].
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhLabel {
    /// Label name (e.g. `"bug"`, `"enhancement"`).
    pub name: String,
}

/// A GitHub user reference as embedded in reviews and other payloads.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhUser {
    /// GitHub login (username).
    pub login: String,
}

/// Embedded git author metadata returned with a PR commit payload.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GhAuthor {
    /// Author display name from the git object.
    pub name: String,
    /// Author email from the git object.
    pub email: String,
    /// Author timestamp (ISO8601). May be absent on some endpoints.
    #[serde(default)]
    pub date: Option<String>,
}

/// Inner `commit` object shape returned by the PR commits endpoint.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubCommitDetail {
    /// Full commit message (subject + body).
    pub message: String,
    /// Optional author block (`name`, `email`, `date`).
    #[serde(default)]
    pub author: Option<GhAuthor>,
}

/// A commit reference returned by the PR commits endpoint
/// (`GET /repos/{owner}/{repo}/pulls/{number}/commits`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubPrCommit {
    /// Full 40-char commit SHA.
    pub sha: String,
    /// Nested commit metadata (message, author).
    pub commit: GitHubCommitDetail,
}

/// A pull-request review as returned by
/// `GET /repos/{owner}/{repo}/pulls/{number}/reviews`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
pub struct GitHubReview {
    /// Review id.
    pub id: u64,
    /// Review state (`APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, ...).
    pub state: String,
    /// Reviewer user (may be absent for deleted accounts).
    #[serde(default)]
    pub user: Option<GhUser>,
    /// ISO8601 submission timestamp. `None` for pending drafts.
    #[serde(default)]
    pub submitted_at: Option<String>,
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
            let resp = self.retry_request(&url).await?;

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
    /// Existing rows with the same `(provider, pr_number)` are replaced.
    /// The `provider` column is set to `'github'` for all rows written by
    /// this client (see migration `0010_pull_requests_provider.sql`).
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
                 (provider, pr_number, title, author, state, created_at, merged_at, commit_shas) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    "github",
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

    /// Fetch a single issue by number from the GitHub REST API.
    ///
    /// Hits `GET /repos/{owner}/{repo}/issues/{number}`. Uses the same
    /// `Bearer` token (if any) as the bulk PR fetch.
    ///
    /// Returns `Ok(None)` when the API responds with `404 Not Found`
    /// (deleted or invisible issue). All other non-success statuses, as
    /// well as transport and JSON-parse failures, are propagated as
    /// [`CollectError`].
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport or non-`404` non-success HTTP
    ///   responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_issue(&self, number: u64) -> Result<Option<GitHubIssue>> {
        let url = format!(
            "{GITHUB_API_BASE}/repos/{}/{}/issues/{number}",
            self.owner, self.repo
        );
        debug!(url = %url, "GET");
        let resp = self.client.get(&url).send().await?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }

        let resp = resp.error_for_status()?;
        let issue: GitHubIssue = resp.json().await?;
        Ok(Some(issue))
    }

    /// Send a GET request with exponential backoff on transient failures.
    ///
    /// Retries up to [`MAX_RETRIES`] times on HTTP 429 (rate limit) or any
    /// 5xx response. Delays follow `RETRY_BASE_MS * 2^attempt` — 1s, 2s, 4s
    /// for the default base.
    ///
    /// Why: GitHub occasionally returns 502/504 under load and 429 when the
    /// per-token rate limit drains; a tiny retry loop avoids surfacing those
    /// as pipeline failures.
    /// What: returns the final non-transient response (which may still be
    /// non-success — the caller is expected to call `.error_for_status()`).
    /// Test: covered indirectly by callers and by `wiremock` integration tests.
    async fn retry_request(&self, url: &str) -> Result<reqwest::Response> {
        let mut last_err: Option<reqwest::Error> = None;
        for attempt in 0..=MAX_RETRIES {
            debug!(url = %url, attempt, "GET (with retry)");
            match self.client.get(url).send().await {
                Ok(resp) => {
                    let status = resp.status();
                    let transient =
                        status.as_u16() == 429 || (500..=599).contains(&status.as_u16());
                    if !transient || attempt == MAX_RETRIES {
                        return Ok(resp);
                    }
                    let delay = RETRY_BASE_MS * (1u64 << attempt);
                    warn!(
                        status = %status,
                        attempt,
                        delay_ms = delay,
                        "GitHub returned transient status; retrying"
                    );
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
                Err(e) => {
                    if attempt == MAX_RETRIES {
                        return Err(CollectError::Http(e));
                    }
                    let delay = RETRY_BASE_MS * (1u64 << attempt);
                    warn!(error = %e, attempt, delay_ms = delay, "transport error; retrying");
                    last_err = Some(e);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                }
            }
        }
        // Unreachable in practice: the loop above always returns by
        // `attempt == MAX_RETRIES`. Fall back to the last seen transport
        // error if we ever do escape it.
        Err(CollectError::Http(
            last_err.expect("retry loop preserved error"),
        ))
    }

    /// Fetch all reviews for a given pull request, paginating until exhausted.
    ///
    /// Why: review counts, approval status, and review latency are core PR
    /// metrics; the bulk-PR endpoint omits reviews entirely.
    /// What: `GET /repos/{owner}/{repo}/pulls/{pr_number}/reviews?per_page=100`,
    /// looping pages until a short page indicates end-of-list.
    /// Test: deserialization shape covered by `github_review_deserializes`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pr_reviews(&self, pr_number: u64) -> Result<Vec<GitHubReview>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls/{pr_number}/reviews?per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubReview> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch all commits attached to a pull request, paginating until exhausted.
    ///
    /// Why: PR-level commit lists let us attribute work to the PR author and
    /// reconstruct review-window churn even when the merge commit alone is
    /// recorded on the default branch.
    /// What: `GET /repos/{owner}/{repo}/pulls/{pr_number}/commits?per_page=100`.
    /// Test: deserialization shape covered by `github_pr_commit_deserializes`.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn fetch_pr_commits(&self, pr_number: u64) -> Result<Vec<GitHubPrCommit>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/pulls/{pr_number}/commits?per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubPrCommit> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// List issues on the configured repository, paginating until exhausted.
    ///
    /// Note: the GitHub `issues` endpoint includes pull requests in its
    /// response. Callers needing pure issues should call [`Self::fetch_pull_requests`]
    /// for PR-specific work.
    ///
    /// Why: bulk issue listing is needed for backfilling ticket metadata
    /// when commit messages reference `#NNN` without a project prefix.
    /// What: `GET /repos/{owner}/{repo}/issues?state={state}&since={since}&per_page=100`.
    /// Test: integration-tested via the `pm` adapter suite; deserialization
    /// reuses `GitHubIssue` whose shape is unit-tested above.
    ///
    /// # Arguments
    ///
    /// * `state` — one of `"open"`, `"closed"`, or `"all"`.
    /// * `since` — optional ISO8601 timestamp; only issues updated at or
    ///   after this time are returned.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses
    ///   after retries are exhausted.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn list_issues(&self, state: &str, since: Option<&str>) -> Result<Vec<GitHubIssue>> {
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let mut url = format!(
                "{GITHUB_API_BASE}/repos/{}/{}/issues?state={state}&per_page={PAGE_SIZE}&page={page}",
                self.owner, self.repo
            );
            if let Some(s) = since {
                url.push_str("&since=");
                url.push_str(s);
            }
            let resp = self.retry_request(&url).await?.error_for_status()?;
            let batch: Vec<GitHubIssue> = resp.json().await?;
            let n = batch.len();
            out.extend(batch);
            if (n as u32) < PAGE_SIZE {
                break;
            }
            page += 1;
        }
        Ok(out)
    }
}

#[async_trait]
impl PrProvider for GitHubClient {
    fn name(&self) -> &str {
        "github"
    }

    async fn fetch_pull_requests(&self) -> Result<Vec<PullRequest>> {
        GitHubClient::fetch_pull_requests(self).await
    }

    fn store_pull_requests(
        &self,
        db: &Database,
        prs: &[PullRequest],
    ) -> crate::core::Result<usize> {
        GitHubClient::store_pull_requests(self, db, prs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirm that the wire shape returned by the GitHub Issues API
    /// deserializes into `GitHubIssue` exactly.
    ///
    /// Why: protects against silent schema drift if GitHub renames or
    /// nests one of the fields we depend on.
    /// What: parses a representative JSON document.
    /// Test: assert that all six fields round-trip with expected values.
    #[test]
    fn github_issue_deserializes_full_payload() {
        let json = r#"{
            "number": 42,
            "title": "Crash on startup",
            "state": "open",
            "html_url": "https://github.com/o/r/issues/42",
            "labels": [
                {"name": "bug"},
                {"name": "high-priority"}
            ],
            "body": "Stack trace: ..."
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
        assert_eq!(issue.number, 42);
        assert_eq!(issue.title, "Crash on startup");
        assert_eq!(issue.state, "open");
        assert_eq!(issue.html_url, "https://github.com/o/r/issues/42");
        assert_eq!(issue.labels.len(), 2);
        assert_eq!(issue.labels[0].name, "bug");
        assert_eq!(issue.labels[1].name, "high-priority");
        assert_eq!(issue.body.as_deref(), Some("Stack trace: ..."));
    }

    /// `body` and `labels` may be missing — GitHub omits empty arrays in
    /// some response shapes. Confirm the deserializer tolerates that.
    ///
    /// Why: serde defaults must apply, otherwise real API responses fail
    /// to parse.
    /// What: parses a minimal JSON document missing the optional fields.
    /// Test: assert defaults for `labels` (empty) and `body` (`None`).
    /// Verify the wire shape of a PR review payload deserializes correctly.
    ///
    /// Why: `submitted_at` may be `null` for pending reviews and `user`
    /// may be absent for deleted accounts — both must tolerate absence.
    /// What: parses a representative reviews JSON document.
    /// Test: assert state, user.login, and optional fields parse as expected.
    #[test]
    fn github_review_deserializes() {
        let json = r#"{
            "id": 12345,
            "state": "APPROVED",
            "user": {"login": "octocat"},
            "submitted_at": "2024-01-01T00:00:00Z"
        }"#;
        let r: GitHubReview = serde_json::from_str(json).expect("parses");
        assert_eq!(r.id, 12345);
        assert_eq!(r.state, "APPROVED");
        assert_eq!(r.user.as_ref().map(|u| u.login.as_str()), Some("octocat"));
        assert_eq!(r.submitted_at.as_deref(), Some("2024-01-01T00:00:00Z"));

        // Missing optional fields tolerated.
        let pending = r#"{"id": 1, "state": "PENDING"}"#;
        let r2: GitHubReview = serde_json::from_str(pending).expect("parses pending");
        assert!(r2.user.is_none());
        assert!(r2.submitted_at.is_none());
    }

    /// Verify the wire shape of a PR commit payload deserializes correctly.
    ///
    /// Why: PR commit responses nest the message and author under a
    /// `commit` object — the flat git2 shape doesn't apply here.
    /// What: parses a representative `/pulls/{n}/commits` element.
    /// Test: assert sha, message, and author fields all extract.
    #[test]
    fn github_pr_commit_deserializes() {
        let json = r#"{
            "sha": "deadbeefcafebabe",
            "commit": {
                "message": "feat: do the thing",
                "author": {
                    "name": "Ada Lovelace",
                    "email": "ada@example.com",
                    "date": "2024-01-01T00:00:00Z"
                }
            }
        }"#;
        let c: GitHubPrCommit = serde_json::from_str(json).expect("parses");
        assert_eq!(c.sha, "deadbeefcafebabe");
        assert_eq!(c.commit.message, "feat: do the thing");
        let author = c.commit.author.expect("author present");
        assert_eq!(author.name, "Ada Lovelace");
        assert_eq!(author.email, "ada@example.com");
        assert_eq!(author.date.as_deref(), Some("2024-01-01T00:00:00Z"));
    }

    #[test]
    fn github_issue_tolerates_missing_optional_fields() {
        let json = r#"{
            "number": 7,
            "title": "Q",
            "state": "closed",
            "html_url": "https://github.com/o/r/issues/7"
        }"#;
        let issue: GitHubIssue = serde_json::from_str(json).expect("parses");
        assert_eq!(issue.number, 7);
        assert!(issue.labels.is_empty());
        assert!(issue.body.is_none());
    }
}

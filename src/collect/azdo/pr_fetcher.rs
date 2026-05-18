//! Azure DevOps pull-request fetcher (Issue #84).
//!
//! Strategy: scan commit messages for the standard ADO merge-commit format
//! (`Merged PR NNNN:`), extract unique PR IDs, then fetch each PR's metadata
//! and reviewer list via the project-scoped ADO REST endpoint
//! `GET {org}/{project}/_apis/git/pullrequests/{id}`. This endpoint does not
//! require the repository GUID, which keeps configuration minimal.
//!
//! Why a separate file: the existing `azdo/client.rs` is already ~2.5k LOC and
//! covers work-item / WIQL flows. PR fetching is an independent surface area
//! (different DB tables, different commit-message regex) and is easier to test
//! in isolation here.

use std::collections::HashSet;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use regex::Regex;
use rusqlite::{params, Connection};
use serde::Deserialize;
use tracing::{debug, info, warn};

use crate::collect::azdo::client::AzdoError;
use crate::core::config::AzureDevOpsConfig;
use crate::core::errors::{Result as CoreResult, TgaError};

/// Regex matching the standard ADO merge-commit subject line.
///
/// ADO emits `Merged PR 1234: <title>` when a PR is completed via squash or
/// merge. The match is case-insensitive to tolerate hand-typed references.
fn merged_pr_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)Merged PR (\d+):").expect("MERGED_PR_RE is a static valid pattern")
    })
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// A normalized Azure DevOps pull request.
///
/// Mirrors only the subset of fields persisted in `pull_requests` /
/// `pr_reviewers`. The raw JSON shape from ADO is intentionally not exposed:
/// it changes between preview API versions and is not load-bearing for the
/// downstream report.
#[derive(Debug, Clone)]
pub struct AdoPullRequest {
    /// `pullRequestId` from ADO.
    pub pr_number: i64,
    /// Display title.
    pub title: String,
    /// Optional Markdown body. Often empty for squash merges.
    pub description: Option<String>,
    /// Author — `uniqueName` if present, otherwise `displayName`.
    pub author: String,
    /// PR creation timestamp.
    pub created_at: DateTime<Utc>,
    /// Time the PR was closed (merged or abandoned).
    pub closed_at: Option<DateTime<Utc>>,
    /// Source branch ref (e.g. `refs/heads/feature/foo`).
    pub source_branch: String,
    /// Target branch ref (e.g. `refs/heads/main`).
    pub target_branch: String,
    /// Lifecycle status: `"active"`, `"completed"`, `"abandoned"`.
    pub status: String,
    /// Reviewer list (may be empty).
    pub reviewers: Vec<AdoPrReviewer>,
    /// Merge commit SHA from `lastMergeCommit.commitId`. `None` for PRs that
    /// have never been merged (active/abandoned, or completed via squash
    /// where ADO has not populated the field). When present, this is the
    /// commit that appears on the target branch and matches the SHA in the
    /// `commits` table — enabling the same `pull_requests.commit_shas` →
    /// `commits.sha` join the GitHub provider exposes.
    pub merge_commit_sha: Option<String>,
}

/// A single reviewer entry attached to an [`AdoPullRequest`].
#[derive(Debug, Clone)]
pub struct AdoPrReviewer {
    /// Stable identifier — `uniqueName` from ADO (e.g. `user@contoso.com`).
    pub reviewer_id: String,
    /// Display name as shown in the ADO UI.
    pub display_name: String,
    /// ADO vote value: `10` approved, `5` approved-with-suggestions, `0`
    /// no-vote, `-5` waiting-for-author, `-10` rejected.
    pub vote: i32,
    /// Whether the reviewer was marked as required for the PR.
    pub is_required: bool,
    /// `true` for AD group reviewers (e.g. `[Project]\\Reviewers`).
    pub is_container: bool,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the set of unique ADO PR IDs referenced by a stream of commit
/// messages.
///
/// Why: ADO's standard merge-commit subject is `Merged PR 1234: <title>`, so
/// the union of commit-message matches gives the full list of PRs that
/// touched the analyzed history without needing a paginated repo-wide PR
/// query.
/// What: returns sorted unique IDs; messages with no match are ignored.
/// Test: covered by `extracts_unique_pr_ids` and `ignores_non_merge_lines`.
pub fn extract_pr_ids<I, S>(messages: I) -> Vec<i64>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut seen: HashSet<i64> = HashSet::new();
    let re = merged_pr_re();
    for msg in messages {
        for cap in re.captures_iter(msg.as_ref()) {
            if let Some(m) = cap.get(1) {
                if let Ok(id) = m.as_str().parse::<i64>() {
                    seen.insert(id);
                }
            }
        }
    }
    let mut out: Vec<i64> = seen.into_iter().collect();
    out.sort_unstable();
    out
}

/// Return the set of `pr_number`s already persisted for the given
/// `(provider, repository)` scope, so callers can skip work already on disk.
///
/// `repository` is the per-provider repository identifier as written by
/// [`upsert_pr`] (for Azure DevOps this is the project name); see migration
/// `0012_pull_requests_repository.sql`. Scoping to a single repository
/// matches the UNIQUE constraint and prevents one project's IDs from
/// masking another's.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn get_existing_pr_numbers(
    conn: &Connection,
    provider: &str,
    repository: &str,
) -> CoreResult<HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT pr_number FROM pull_requests WHERE provider = ?1 AND repository = ?2")?;
    let rows = stmt
        .query_map(params![provider, repository], |row| row.get::<_, i64>(0))
        .map_err(TgaError::from)?;
    let mut out = HashSet::new();
    for r in rows {
        out.insert(r.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// Upsert an [`AdoPullRequest`] into `pull_requests` (provider = 'azdo')
/// and return the row id (existing or newly inserted).
///
/// Why: ADO PRs reuse the shared `pull_requests` table; the
/// `(provider, repository, pr_number)` triple scopes uniqueness so neither
/// cross-provider IDs nor cross-project IDs collide. We need the row id
/// back to attach reviewers via FK.
/// What: `INSERT OR REPLACE` keyed by `(provider, repository, pr_number)`
/// per migration `0012_pull_requests_repository.sql`, then a `SELECT id`
/// to recover the row id (REPLACE may renumber on conflict). The
/// `repository` parameter is the ADO project name — Azure DevOps PR IDs
/// are project-scoped, not org-scoped, so the project is the right
/// uniqueness boundary.
/// Test: `upsert_pr_round_trips_basic_fields` exercises insert + re-insert.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn upsert_pr(conn: &Connection, pr: &AdoPullRequest, repository: &str) -> CoreResult<i64> {
    // Map ADO status to our PrState enum's string form so reports that
    // group by `state` (open/closed/merged) keep working.
    let state = match pr.status.to_ascii_lowercase().as_str() {
        "completed" => "merged",
        "abandoned" => "closed",
        _ => "open",
    };

    // Match the shape the GitHub fetcher writes (see
    // `src/collect/github/client.rs::collect_pull_requests`): a JSON array
    // containing the merge commit SHA, or `[]` when none is available.
    // Issue #92: this used to be hardcoded to `"[]"`, breaking the
    // `pull_requests.commit_shas` → `commits.sha` join that downstream
    // reports rely on.
    let commit_shas = match &pr.merge_commit_sha {
        Some(sha) => serde_json::to_string(&[sha.as_str()])?,
        None => "[]".to_string(),
    };

    conn.execute(
        "INSERT OR REPLACE INTO pull_requests \
         (provider, repository, pr_number, title, author, state, created_at, merged_at, commit_shas) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            "azdo",
            repository,
            pr.pr_number,
            pr.title,
            pr.author,
            state,
            pr.created_at.to_rfc3339(),
            pr.closed_at.map(|t| t.to_rfc3339()),
            commit_shas,
        ],
    )?;

    let id: i64 = conn
        .query_row(
            "SELECT id FROM pull_requests WHERE provider = ?1 AND repository = ?2 AND pr_number = ?3",
            params!["azdo", repository, pr.pr_number],
            |row| row.get(0),
        )
        .map_err(TgaError::from)?;
    Ok(id)
}

/// Upsert a single reviewer row attached to `pr_db_id`.
///
/// Uses `INSERT OR REPLACE` on the unique `(pr_id, provider, reviewer_id)`
/// index so re-running collection refreshes the vote without duplicating rows.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] on SQL failure.
pub fn upsert_pr_reviewer(
    conn: &Connection,
    pr_db_id: i64,
    reviewer: &AdoPrReviewer,
) -> CoreResult<()> {
    conn.execute(
        "INSERT OR REPLACE INTO pr_reviewers \
         (pr_id, provider, reviewer_id, display_name, vote, is_required, is_container) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            pr_db_id,
            "azdo",
            reviewer.reviewer_id,
            reviewer.display_name,
            reviewer.vote,
            reviewer.is_required as i32,
            reviewer.is_container as i32,
        ],
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// HTTP client
// ---------------------------------------------------------------------------

/// Minimal ADO PR fetcher. Owns its own `reqwest::Client` so it can be used
/// without keeping the larger work-item client alive.
pub struct AdoPrFetcher {
    config: AzureDevOpsConfig,
    client: reqwest::Client,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PrRaw {
    pull_request_id: i64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    created_by: Option<IdentityRaw>,
    creation_date: DateTime<Utc>,
    #[serde(default)]
    closed_date: Option<DateTime<Utc>>,
    #[serde(default)]
    source_ref_name: String,
    #[serde(default)]
    target_ref_name: String,
    #[serde(default)]
    reviewers: Vec<ReviewerRaw>,
    #[serde(default)]
    last_merge_commit: Option<CommitRefRaw>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CommitRefRaw {
    #[serde(default)]
    commit_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IdentityRaw {
    #[serde(default)]
    unique_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReviewerRaw {
    #[serde(default)]
    unique_name: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    vote: i32,
    #[serde(default)]
    is_required: bool,
    #[serde(default)]
    is_container: bool,
}

impl From<PrRaw> for AdoPullRequest {
    fn from(raw: PrRaw) -> Self {
        let author = raw
            .created_by
            .as_ref()
            .and_then(|i| i.unique_name.clone().or_else(|| i.display_name.clone()))
            .unwrap_or_default();
        let reviewers = raw
            .reviewers
            .into_iter()
            .map(|r| {
                let display = r.display_name.unwrap_or_default();
                let id = r.unique_name.unwrap_or_else(|| display.clone());
                AdoPrReviewer {
                    reviewer_id: id,
                    display_name: display,
                    vote: r.vote,
                    is_required: r.is_required,
                    is_container: r.is_container,
                }
            })
            .collect();
        // Pull the merge commit SHA from `lastMergeCommit.commitId` only
        // for *completed* PRs. ADO populates `lastMergeCommit` even for
        // active PRs — it's the most recent merge attempt, which for
        // unmerged PRs is a virtual preview merge on `refs/pull/N/merge`,
        // not a commit that ever landed on the target branch. Writing
        // that SHA into `commit_shas` would produce non-joinable rows
        // against the `commits` table (issue #92 review feedback). For
        // GitHub parity we only emit a SHA once the PR has actually
        // landed (status == "completed"). Empty strings are also treated
        // as missing — some ADO previews return `lastMergeCommit: {}`.
        let merge_commit_sha = if raw.status.eq_ignore_ascii_case("completed") {
            raw.last_merge_commit
                .and_then(|c| c.commit_id)
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        AdoPullRequest {
            pr_number: raw.pull_request_id,
            title: raw.title,
            description: raw.description,
            author,
            created_at: raw.creation_date,
            closed_at: raw.closed_date,
            source_branch: raw.source_ref_name,
            target_branch: raw.target_ref_name,
            status: raw.status,
            reviewers,
            merge_commit_sha,
        }
    }
}

impl AdoPrFetcher {
    /// Construct a new fetcher.
    ///
    /// # Errors
    ///
    /// Returns [`AzdoError::Request`] if the underlying `reqwest::Client`
    /// cannot be built.
    pub fn new(config: AzureDevOpsConfig) -> std::result::Result<Self, AzdoError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            reqwest::header::HeaderValue::from_static(concat!("tga/", env!("CARGO_PKG_VERSION"))),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(AzdoError::Request)?;
        Ok(Self { config, client })
    }

    fn org_url(&self) -> &str {
        self.config.organization_url.trim_end_matches('/')
    }

    /// Fetch a single PR by ID via the project-scoped endpoint.
    ///
    /// Calls `GET {org}/{project}/_apis/git/pullrequests/{pr_id}?api-version=7.1`.
    /// Returns `Ok(None)` on HTTP 404 (deleted PR or wrong project).
    ///
    /// # Errors
    ///
    /// * [`AzdoError::Unauthorized`] / [`AzdoError::Forbidden`] on 401/403.
    /// * [`AzdoError::Http`] on any other non-success status.
    /// * [`AzdoError::Request`] on transport failure.
    /// * [`AzdoError::Parse`] on payload parse failure.
    pub async fn fetch_pr(
        &self,
        pr_id: i64,
    ) -> std::result::Result<Option<AdoPullRequest>, AzdoError> {
        let url = format!(
            "{}/{}/_apis/git/pullrequests/{pr_id}?api-version=7.1",
            self.org_url(),
            encode_segment(&self.config.project),
        );
        debug!(url = %url, pr_id, "GET ADO PR");

        let resp = self
            .client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await
            .map_err(AzdoError::Request)?;

        match resp.status().as_u16() {
            200 => {
                let raw: PrRaw = resp
                    .json()
                    .await
                    .map_err(|e| AzdoError::Parse(e.to_string()))?;
                Ok(Some(raw.into()))
            }
            404 => Ok(None),
            401 => Err(AzdoError::Unauthorized),
            403 => Err(AzdoError::Forbidden),
            s => {
                let message = resp.text().await.unwrap_or_default();
                Err(AzdoError::Http { status: s, message })
            }
        }
    }

    /// Fetch a batch of PRs serially.
    ///
    /// Serial fetching is intentional: the upstream issue notes that ~7.4
    /// PRs/sec is sufficient for typical analytics windows, and serial calls
    /// keep error handling simple (one bad ID can't poison a parallel batch).
    /// Errors from individual PRs are logged and skipped; the caller gets only
    /// the successful results.
    pub async fn fetch_prs(&self, ids: &[i64]) -> Vec<AdoPullRequest> {
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            match self.fetch_pr(id).await {
                Ok(Some(pr)) => out.push(pr),
                Ok(None) => {
                    debug!(pr_id = id, "ADO PR not found (404), skipping");
                }
                Err(e) => {
                    warn!(pr_id = id, error = %e, "ADO PR fetch failed");
                }
            }
        }
        out
    }

    /// Top-level driver: extract PR IDs from `commit_messages`, skip any
    /// already persisted under provider `'azdo'`, fetch the rest, and write
    /// the PRs and their reviewers to the database.
    ///
    /// Returns the number of PR rows newly written / refreshed.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] for SQL failures. HTTP failures on
    /// individual PRs are logged and do not abort the whole run.
    pub async fn run<I, S>(&self, conn: &Connection, commit_messages: I) -> CoreResult<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let ids = extract_pr_ids(commit_messages);
        if ids.is_empty() {
            info!("No 'Merged PR N:' references found; skipping ADO PR fetch");
            return Ok(0);
        }
        let project = self.config.project.clone();
        let existing = get_existing_pr_numbers(conn, "azdo", &project)?;
        let to_fetch: Vec<i64> = ids
            .into_iter()
            .filter(|id| !existing.contains(id))
            .collect();
        if to_fetch.is_empty() {
            info!("All referenced ADO PRs already cached; skipping fetch");
            return Ok(0);
        }
        info!(count = to_fetch.len(), "Fetching ADO PRs");

        let prs = self.fetch_prs(&to_fetch).await;
        let mut stored = 0usize;
        for pr in &prs {
            let pr_db_id = upsert_pr(conn, pr, &project)?;
            for reviewer in &pr.reviewers {
                upsert_pr_reviewer(conn, pr_db_id, reviewer)?;
            }
            stored += 1;
        }
        info!(stored, "Persisted ADO PRs");
        Ok(stored)
    }
}

/// Percent-encode a single path segment (project name).
fn encode_segment(s: &str) -> String {
    fn is_unreserved(b: u8) -> bool {
        b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
    }
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if is_unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    fn sample_pr() -> AdoPullRequest {
        AdoPullRequest {
            pr_number: 12345,
            title: "feat: add widget".into(),
            description: Some("body".into()),
            author: "alice@contoso.com".into(),
            created_at: "2024-01-15T10:30:00Z".parse().unwrap(),
            closed_at: Some("2024-01-16T14:00:00Z".parse().unwrap()),
            source_branch: "refs/heads/feature/widget".into(),
            target_branch: "refs/heads/main".into(),
            status: "completed".into(),
            reviewers: vec![AdoPrReviewer {
                reviewer_id: "bob@contoso.com".into(),
                display_name: "Bob".into(),
                vote: 10,
                is_required: true,
                is_container: false,
            }],
            merge_commit_sha: Some("deadbeefcafef00d1234567890abcdef12345678".into()),
        }
    }

    #[test]
    fn extracts_unique_pr_ids() {
        let messages = vec![
            "Merged PR 100: do thing",
            "Some other commit",
            "merged pr 200: another (case-insensitive)",
            "Merged PR 100: duplicate",
            "Refactored: Merged PR 300: nested phrase",
        ];
        let ids = extract_pr_ids(messages);
        assert_eq!(ids, vec![100, 200, 300]);
    }

    #[test]
    fn ignores_non_merge_lines() {
        let messages = vec!["fix: typo", "PR #42", "merge branch 'foo'"];
        let ids = extract_pr_ids(messages);
        assert!(ids.is_empty(), "no merge-PR pattern should match: {ids:?}");
    }

    #[test]
    fn extract_pr_ids_handles_empty_input() {
        let ids: Vec<i64> = extract_pr_ids(Vec::<&str>::new());
        assert!(ids.is_empty());
    }

    #[test]
    fn upsert_pr_round_trips_basic_fields() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr();
        let row_id = upsert_pr(conn, &pr, "MyProject").expect("first upsert");
        assert!(row_id > 0);

        // Re-upsert: should not duplicate, should return the same logical
        // identity (provider, repository, pr_number).
        let row_id2 = upsert_pr(conn, &pr, "MyProject").expect("second upsert");
        assert!(row_id2 > 0);

        // Count rows for this (provider, repository, pr_number) — must be exactly 1.
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_requests \
                 WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
                params![pr.pr_number],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            n, 1,
            "should have exactly one row per (provider, repository, pr_number)"
        );
    }

    #[test]
    fn upsert_pr_reviewer_round_trips() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr();
        let pr_db_id = upsert_pr(conn, &pr, "MyProject").expect("pr upsert");
        for r in &pr.reviewers {
            upsert_pr_reviewer(conn, pr_db_id, r).expect("reviewer upsert");
        }
        // Re-upsert should not duplicate.
        for r in &pr.reviewers {
            upsert_pr_reviewer(conn, pr_db_id, r).expect("reviewer upsert (2)");
        }
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pr_reviewers WHERE pr_id = ?1",
                params![pr_db_id],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(n, pr.reviewers.len() as i64);

        // Vote and required flags persist correctly.
        let (vote, required): (i32, i32) = conn
            .query_row(
                "SELECT vote, is_required FROM pr_reviewers WHERE pr_id = ?1 AND reviewer_id = ?2",
                params![pr_db_id, "bob@contoso.com"],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("query reviewer");
        assert_eq!(vote, 10);
        assert_eq!(required, 1);
    }

    #[test]
    fn get_existing_pr_numbers_returns_persisted_ids() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr();
        upsert_pr(conn, &pr, "MyProject").expect("upsert");

        let ids = get_existing_pr_numbers(conn, "azdo", "MyProject").expect("query");
        assert!(ids.contains(&pr.pr_number));

        let ids_gh = get_existing_pr_numbers(conn, "github", "MyProject").expect("query gh");
        assert!(
            !ids_gh.contains(&pr.pr_number),
            "provider scoping must hold"
        );

        // Cross-project scoping: same provider, different repository → must
        // not return the row. This is the regression guard for #88.
        let ids_other = get_existing_pr_numbers(conn, "azdo", "OtherProject").expect("query other");
        assert!(
            !ids_other.contains(&pr.pr_number),
            "repository scoping must hold for #88"
        );
    }

    #[test]
    fn upsert_pr_allows_same_pr_number_in_different_repositories() {
        // Regression test for issue #88: two PRs with the same pr_number in
        // different repositories must coexist (no INSERT OR REPLACE
        // collision).
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr();

        let id_a = upsert_pr(conn, &pr, "ProjectA").expect("upsert A");
        let id_b = upsert_pr(conn, &pr, "ProjectB").expect("upsert B");
        assert_ne!(id_a, id_b, "different repos must produce different rows");

        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_requests WHERE provider = 'azdo' AND pr_number = ?1",
                params![pr.pr_number],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(
            total, 2,
            "same pr_number across two repos must yield two rows"
        );
    }

    #[test]
    fn upsert_pr_writes_commit_shas_when_merge_sha_present() {
        // Regression test for issue #92: ADO PRs with a known
        // `lastMergeCommit.commitId` must be persisted with a
        // single-element JSON array in `commit_shas`, matching the
        // GitHub fetcher's shape so downstream PR↔commit joins work.
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr();
        upsert_pr(conn, &pr, "MyProject").expect("upsert");

        let stored: String = conn
            .query_row(
                "SELECT commit_shas FROM pull_requests \
                 WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
                params![pr.pr_number],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(
            stored, r#"["deadbeefcafef00d1234567890abcdef12345678"]"#,
            "merge commit SHA must be persisted as a JSON array"
        );
    }

    #[test]
    fn upsert_pr_writes_empty_commit_shas_when_no_merge_sha() {
        // PRs without a merge commit (active, abandoned, or pre-merge
        // squash) must still upsert cleanly and store an empty JSON array
        // — the same fallback the GitHub provider uses.
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let mut pr = sample_pr();
        pr.merge_commit_sha = None;
        upsert_pr(conn, &pr, "MyProject").expect("upsert");

        let stored: String = conn
            .query_row(
                "SELECT commit_shas FROM pull_requests \
                 WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
                params![pr.pr_number],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(stored, "[]");
    }

    #[test]
    fn pr_raw_deserializes_full_payload() {
        let json = r#"{
            "pullRequestId": 12345,
            "title": "feat: add widget",
            "description": "body",
            "status": "completed",
            "createdBy": {
                "uniqueName": "alice@contoso.com",
                "displayName": "Alice"
            },
            "creationDate": "2024-01-15T10:30:00Z",
            "closedDate": "2024-01-16T14:00:00Z",
            "sourceRefName": "refs/heads/feature/widget",
            "targetRefName": "refs/heads/main",
            "reviewers": [
                {
                    "uniqueName": "bob@contoso.com",
                    "displayName": "Bob",
                    "vote": 10,
                    "isRequired": true,
                    "isContainer": false
                }
            ],
            "lastMergeCommit": {
                "commitId": "deadbeefcafef00d1234567890abcdef12345678",
                "url": "https://dev.azure.com/.../commits/deadbeef..."
            }
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(pr.pr_number, 12345);
        assert_eq!(pr.title, "feat: add widget");
        assert_eq!(pr.author, "alice@contoso.com");
        assert_eq!(pr.status, "completed");
        assert_eq!(pr.target_branch, "refs/heads/main");
        assert_eq!(pr.reviewers.len(), 1);
        assert_eq!(pr.reviewers[0].vote, 10);
        assert!(pr.reviewers[0].is_required);
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("deadbeefcafef00d1234567890abcdef12345678"),
            "lastMergeCommit.commitId should be threaded through"
        );
    }

    #[test]
    fn pr_raw_treats_empty_last_merge_commit_as_none() {
        // ADO's preview API sometimes returns `lastMergeCommit: {}` for
        // PRs that haven't been merged. Either an absent object or an
        // empty `commitId` should map to `None` so callers don't try to
        // join against an empty SHA. Use `status: completed` so the
        // status gate below doesn't mask the empty-payload logic we're
        // exercising here.
        let json = r#"{
            "pullRequestId": 7,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "lastMergeCommit": {}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(pr.merge_commit_sha.is_none());

        let json = r#"{
            "pullRequestId": 8,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "lastMergeCommit": {"commitId": ""}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(pr.merge_commit_sha.is_none());
    }

    #[test]
    fn pr_raw_drops_merge_sha_for_non_completed_status() {
        // Issue #92 design review: ADO populates `lastMergeCommit` even
        // for *active* PRs — it's a preview merge that never landed on
        // the target branch, so writing it to `commit_shas` would create
        // a non-joinable row against the `commits` table. Only completed
        // PRs should expose a merge SHA, matching GitHub semantics.
        for status in ["active", "abandoned", "notSet", "", "ACTIVE"] {
            let json = format!(
                r#"{{
                    "pullRequestId": 42,
                    "creationDate": "2024-01-15T10:30:00Z",
                    "status": "{status}",
                    "lastMergeCommit": {{"commitId": "feedfacecafef00d1234567890abcdef12345678"}}
                }}"#
            );
            let raw: PrRaw = serde_json::from_str(&json).expect("parse");
            let pr: AdoPullRequest = raw.into();
            assert!(
                pr.merge_commit_sha.is_none(),
                "non-completed status {status:?} must not expose a merge SHA"
            );
        }

        // Sanity check: completed PRs still get the SHA (case-insensitive).
        for status in ["completed", "Completed", "COMPLETED"] {
            let json = format!(
                r#"{{
                    "pullRequestId": 43,
                    "creationDate": "2024-01-15T10:30:00Z",
                    "status": "{status}",
                    "lastMergeCommit": {{"commitId": "feedfacecafef00d1234567890abcdef12345678"}}
                }}"#
            );
            let raw: PrRaw = serde_json::from_str(&json).expect("parse");
            let pr: AdoPullRequest = raw.into();
            assert_eq!(
                pr.merge_commit_sha.as_deref(),
                Some("feedfacecafef00d1234567890abcdef12345678"),
                "completed status {status:?} should pass the gate (case-insensitive)",
            );
        }
    }

    #[test]
    fn pr_raw_tolerates_missing_optional_fields() {
        let json = r#"{
            "pullRequestId": 7,
            "creationDate": "2024-01-15T10:30:00Z"
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse minimal");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(pr.pr_number, 7);
        assert!(pr.author.is_empty());
        assert!(pr.reviewers.is_empty());
        assert!(pr.closed_at.is_none());
        assert!(pr.description.is_none());
    }

    #[test]
    fn fetch_prs_config_deserializes_with_fetch_prs_true() {
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
fetch_prs: true
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert!(parsed.fetch_prs);
    }

    #[test]
    fn fetch_prs_defaults_to_false() {
        let yaml = r#"
organization_url: "https://dev.azure.com/myorg"
pat: "secret-pat"
project: "MyProject"
"#;
        let parsed: AzureDevOpsConfig =
            serde_yaml::from_str(yaml).expect("should deserialize cleanly");
        assert!(!parsed.fetch_prs, "fetch_prs default must be false");
    }

    #[test]
    fn status_maps_to_pr_state_string() {
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let mut pr = sample_pr();
        pr.status = "abandoned".into();
        let id = upsert_pr(conn, &pr, "MyProject").expect("upsert");
        let state: String = conn
            .query_row(
                "SELECT state FROM pull_requests WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(state, "closed");

        pr.status = "active".into();
        upsert_pr(conn, &pr, "MyProject").expect("upsert");
        let state: String = conn
            .query_row(
                "SELECT state FROM pull_requests \
                 WHERE provider = 'azdo' AND repository = 'MyProject' AND pr_number = ?1",
                params![pr.pr_number],
                |row| row.get(0),
            )
            .expect("query");
        assert_eq!(state, "open");
    }
}

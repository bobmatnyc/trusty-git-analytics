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
//!
//! # `merge_commit_sha` emission matrix (issue #96)
//!
//! ADO's `lastMergeCommit.commitId` is only join-compatible with the `commits`
//! table for *true* merge commits. Squash and rebase completions rewrite
//! history and produce SHAs that never appear on the target branch, so
//! emitting them would create orphan `pr_commits` rows. The gate below
//! restricts emission to strategies that preserve the merge SHA.
//!
//! | `status`     | `mergeStrategy`            | emitted `merge_commit_sha`        | rationale                                                              |
//! |--------------|----------------------------|-----------------------------------|------------------------------------------------------------------------|
//! | `completed`  | `noFastForward` or absent  | `Some(lastMergeCommit.commitId)`  | true merge — SHA lands on target branch and joins to `commits`         |
//! | `completed`  | `squash`                   | `None`                            | squash rewrites history; merge SHA isn't on target branch              |
//! | `completed`  | `rebase`                   | `None`                            | rebase replays commits; merge SHA isn't on target branch               |
//! | `completed`  | `rebaseMerge`              | `None`                            | rebase-merge replays then merges; the SHA we'd emit isn't reliable     |
//! | any non-`completed` (e.g. `active`, `abandoned`) | * | `None` | preview merge on `refs/pull/N/merge` never landed (issue #92)          |
//!
//! Strategy comparison is case-insensitive. The strategy is read from the
//! top-level `mergeStrategy` field, falling back to
//! `completionOptions.mergeStrategy` when the former is absent.

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
    /// Top-level merge strategy (`noFastForward` / `squash` / `rebase` /
    /// `rebaseMerge`). Preferred when present; otherwise we fall back to
    /// [`PrRaw::completion_options`]. See the module-level matrix.
    #[serde(default)]
    merge_strategy: Option<String>,
    /// Nested completion metadata. Older ADO API versions only surface the
    /// merge strategy here, so we deserialize both shapes.
    #[serde(default)]
    completion_options: Option<CompletionOptionsRaw>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CommitRefRaw {
    #[serde(default)]
    commit_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct CompletionOptionsRaw {
    #[serde(default)]
    merge_strategy: Option<String>,
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
        // for *completed* PRs whose merge strategy actually preserves a
        // merge commit on the target branch (issue #96). ADO populates
        // `lastMergeCommit` even for active PRs — it's the most recent
        // merge attempt, which for unmerged PRs is a virtual preview
        // merge on `refs/pull/N/merge`, not a commit that ever landed on
        // the target branch (issue #92). For squash / rebase / rebaseMerge
        // completions the SHA likewise does not appear on the target
        // branch, so emitting it would produce non-joinable rows against
        // the `commits` table. We accept the SHA only when the strategy
        // is `noFastForward` or absent (older API versions / true merges).
        // Empty strings are treated as missing — some ADO previews return
        // `lastMergeCommit: {}`.
        let strategy_allows_merge_sha = {
            let strategy = raw.merge_strategy.as_deref().or_else(|| {
                raw.completion_options
                    .as_ref()
                    .and_then(|co| co.merge_strategy.as_deref())
            });
            match strategy {
                None => true,
                Some(s) => s.eq_ignore_ascii_case("noFastForward"),
            }
        };
        let merge_commit_sha =
            if raw.status.eq_ignore_ascii_case("completed") && strategy_allows_merge_sha {
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
    /// * [`AzdoError::Config`] if `config.projects()` is empty (both
    ///   `project` and `projects` blank/omitted). This is the load-bearing
    ///   invariant that prevents a misconfigured fetcher from being
    ///   constructed — without it, a config with `fetch_prs: true` but no
    ///   `project`/`projects` would silently produce `Ok(None)` from every
    ///   `fetch_pr` call (follow-up to issue #91). URL- and PAT-shape
    ///   checks are delegated to [`ConfigValidator`](crate::core::config::ConfigValidator)
    ///   preflight.
    /// * [`AzdoError::Request`] if the underlying `reqwest::Client`
    ///   cannot be built.
    pub fn new(config: AzureDevOpsConfig) -> std::result::Result<Self, AzdoError> {
        // Fail fast: enforce the empty-projects invariant from
        // AzureDevOpsConfig::validate() so that no caller can construct an
        // AdoPrFetcher with no resolvable project. ConfigValidator's
        // preflight check also covers this, but defending at the
        // type-construction boundary makes the invariant load-bearing
        // even for callers that skip preflight — without this check a
        // config with `fetch_prs: true` but empty `project`/`projects`
        // would silently return `Ok(None)` from every fetch_pr call.
        //
        // We only assert the projects()-non-empty subset of validate()
        // here: URL- and PAT-shape checks are intentionally delegated to
        // ConfigValidator preflight so tests (and integration callers
        // using non-cloud mock URLs) can construct a fetcher without
        // tripping the cloud-URL gate.
        if config.projects().is_empty() {
            return Err(AzdoError::Config(
                "pm.azure_devops.project (or .projects) must not be empty".into(),
            ));
        }

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

    /// Fetch a single PR by ID via the project-scoped endpoint, trying each
    /// configured project in turn until a 200 hit (issue #91).
    ///
    /// Calls `GET {org}/{project}/_apis/git/pullrequests/{pr_id}?api-version=7.1`
    /// for each project from [`AzureDevOpsConfig::projects`]. Returns the PR
    /// paired with the project name it was found in, or `Ok(None)` if every
    /// configured project returns 404.
    ///
    /// Why iterate: ADO PR IDs are project-scoped, so a PR in project B will
    /// 404 against project A. Single-project configs (one project in
    /// `projects()`) issue exactly one request — no overhead. Multi-project
    /// configs stop at first hit (first-hit-wins) to avoid N×P requests.
    ///
    /// # First-hit-wins policy
    ///
    /// Project iteration order is determined by
    /// [`AzureDevOpsConfig::projects`], which yields the legacy single
    /// `project` field first (when set), followed by entries from the
    /// `projects` list in their configured order. The first project that
    /// returns 200 OK wins: its PR is recorded and remaining projects are
    /// not queried.
    ///
    /// If two configured projects share a PR ID (i.e. the same numeric
    /// identifier exists in both project A and project B as legitimately
    /// distinct PRs), the first-listed project's PR is recorded and the
    /// other is silently shadowed. This is deterministic but order-sensitive
    /// — users with overlapping PR IDs across projects should be aware that
    /// configuration order chooses the winner. We do not probe all projects
    /// to detect such collisions because doing so would require N×P HTTP
    /// calls per PR ID in the worst case, defeating the efficiency goal of
    /// the first-hit-wins design.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::Unauthorized`] / [`AzdoError::Forbidden`] on 401/403 from
    ///   any project (auth errors are fatal — we don't keep guessing).
    /// * [`AzdoError::Http`] on any other non-success status.
    /// * [`AzdoError::Request`] on transport failure.
    /// * [`AzdoError::Parse`] on payload parse failure.
    pub async fn fetch_pr(
        &self,
        pr_id: i64,
    ) -> std::result::Result<Option<(AdoPullRequest, String)>, AzdoError> {
        for project in self.config.projects() {
            let url = format!(
                "{}/{}/_apis/git/pullrequests/{pr_id}?api-version=7.1",
                self.org_url(),
                encode_segment(project),
            );
            debug!(url = %url, pr_id, project = %project, "GET ADO PR");

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
                    let pr: AdoPullRequest = raw.into();
                    return Ok(Some((pr, project.to_string())));
                }
                404 => {
                    // Fall through to the next project — PR IDs are
                    // project-scoped and a miss in project A doesn't preclude
                    // a hit in project B.
                    debug!(pr_id, project = %project, "404 in project; trying next");
                    continue;
                }
                401 => return Err(AzdoError::Unauthorized),
                403 => return Err(AzdoError::Forbidden),
                s => {
                    let message = resp.text().await.unwrap_or_default();
                    return Err(AzdoError::Http { status: s, message });
                }
            }
        }
        Ok(None)
    }

    /// Fetch a batch of PRs serially.
    ///
    /// Serial fetching is intentional: the upstream issue notes that ~7.4
    /// PRs/sec is sufficient for typical analytics windows, and serial calls
    /// keep error handling simple (one bad ID can't poison a parallel batch).
    /// Errors from individual PRs are logged and skipped; the caller gets only
    /// the successful results. Each result is paired with the project name
    /// the PR was found in (issue #91).
    pub async fn fetch_prs(&self, ids: &[i64]) -> Vec<(AdoPullRequest, String)> {
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            match self.fetch_pr(id).await {
                Ok(Some(pair)) => out.push(pair),
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
    /// Equivalent to [`AdoPrFetcher::run_with_options`] with
    /// `force_refresh = false`. Retained for callers that do not need to
    /// bypass the deduplication cache.
    ///
    /// Returns the number of PR rows newly written / refreshed.
    ///
    /// # Cache behavior
    ///
    /// In single-project mode (`projects().len() == 1`) we pre-filter PR IDs
    /// against the per-project cache so already-stored PRs are not refetched.
    /// In multi-project mode we deliberately skip the cache pre-filter: ADO
    /// PR IDs are project-scoped, and unioning cached IDs across projects
    /// would collapse that scoping and silently drop legitimately distinct
    /// PRs that happen to share a numeric ID. Idempotency on repeated runs
    /// is guaranteed by `upsert_pr`'s `INSERT OR REPLACE` on the
    /// `(provider, repository, pr_number)` unique key, at the cost of
    /// refetching already-stored PRs.
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
        self.run_with_options(conn, commit_messages, false).await
    }

    /// Top-level driver with an explicit cache-bypass option.
    ///
    /// Extracts PR IDs from `commit_messages`, optionally skips IDs already
    /// persisted under provider `'azdo'`, fetches the rest, and writes the
    /// PRs and their reviewers to the database.
    ///
    /// When `force_refresh` is `true`, the [`get_existing_pr_numbers`]
    /// deduplication step is bypassed so every referenced PR is re-fetched
    /// and re-upserted. This backfills rows persisted before v1.0.9 that
    /// still store `commit_shas` as `'[]'`; [`upsert_pr`]'s
    /// `INSERT OR REPLACE` keeps the operation idempotent.
    ///
    /// Returns the number of PR rows newly written / refreshed.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] for SQL failures. HTTP failures on
    /// individual PRs are logged and do not abort the whole run.
    pub async fn run_with_options<I, S>(
        &self,
        conn: &Connection,
        commit_messages: I,
        force_refresh: bool,
    ) -> CoreResult<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let ids = extract_pr_ids(commit_messages);
        if ids.is_empty() {
            info!("No 'Merged PR N:' references found; skipping ADO PR fetch");
            return Ok(0);
        }
        // Cache pre-filter: only safe in single-project mode. ADO PR IDs are
        // project-scoped, so unioning cached IDs across multiple projects
        // would mask cross-project collisions and silently drop PRs (issue
        // #91). In multi-project mode we let upsert_pr's INSERT OR IGNORE
        // handle idempotency.
        let projects = self.config.projects();
        let to_fetch: Vec<i64> = if force_refresh {
            info!(
                count = ids.len(),
                "force-refresh-prs: bypassing PR-ID dedup cache"
            );
            ids
        } else if projects.len() == 1 {
            let existing = get_existing_pr_numbers(conn, "azdo", projects[0])?;
            ids.into_iter()
                .filter(|id| !existing.contains(id))
                .collect()
        } else {
            debug!(
                projects_len = projects.len(),
                "Multi-project ADO config: skipping cross-project PR cache to avoid masking collisions"
            );
            ids
        };
        if to_fetch.is_empty() {
            info!("All referenced ADO PRs already cached; skipping fetch");
            return Ok(0);
        }
        info!(count = to_fetch.len(), "Fetching ADO PRs");

        let prs = self.fetch_prs(&to_fetch).await;
        let mut stored = 0usize;
        for (pr, project) in &prs {
            let pr_db_id = upsert_pr(conn, pr, project)?;
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
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn multi_project_config(server_url: &str, projects: Vec<&str>) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: server_url.to_string(),
            pat: "secret-pat".into(),
            project: None,
            projects: projects.iter().map(|s| s.to_string()).collect(),
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: true,
        }
    }

    fn pr_body_json(pr_id: i64) -> serde_json::Value {
        serde_json::json!({
            "pullRequestId": pr_id,
            "title": "feat: multi-project",
            "status": "completed",
            "createdBy": {
                "uniqueName": "alice@contoso.com",
                "displayName": "Alice"
            },
            "creationDate": "2024-01-15T10:30:00Z",
            "closedDate": "2024-01-16T14:00:00Z",
            "sourceRefName": "refs/heads/feature/x",
            "targetRefName": "refs/heads/main",
            "reviewers": []
        })
    }

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
    fn ado_pr_fetcher_new_rejects_empty_projects() {
        // Regression for the validation gap surfaced after issue #91:
        // a config with `fetch_prs: true` but both `project: None` and
        // `projects: vec![]` must fail at the construction boundary so
        // misconfigured callers can't silently produce Ok(None) for every
        // PR. The check is independent of URL/PAT shape — pass a non-cloud
        // URL to confirm only the empty-projects invariant is exercised.
        let cfg = AzureDevOpsConfig {
            organization_url: "http://localhost".to_string(),
            pat: "secret-pat".into(),
            project: None,
            projects: vec![],
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
            fetch_prs: true,
        };
        match AdoPrFetcher::new(cfg) {
            Ok(_) => panic!("empty projects must be rejected"),
            Err(AzdoError::Config(msg)) => assert!(
                msg.contains("project"),
                "expected message to mention project, got: {msg}"
            ),
            Err(other) => panic!("expected AzdoError::Config, got: {other:?}"),
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
            stored,
            r#"["deadbeefcafef00d1234567890abcdef12345678"]"#, // pragma: allowlist secret
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
                    "mergeStrategy": "noFastForward",
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

        // Sanity check: completed PRs still get the SHA (case-insensitive)
        // when the strategy gate (issue #96) allows it.
        for status in ["completed", "Completed", "COMPLETED"] {
            let json = format!(
                r#"{{
                    "pullRequestId": 43,
                    "creationDate": "2024-01-15T10:30:00Z",
                    "status": "{status}",
                    "mergeStrategy": "noFastForward",
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
    fn pr_raw_emits_merge_sha_for_no_fast_forward_strategy() {
        // Issue #96: noFastForward is the only strategy that lands a true
        // merge commit on the target branch, so the gate must let it
        // through. Spot-check a case-insensitive variant as well.
        for strategy in ["noFastForward", "NOFASTFORWARD", "nofastforward"] {
            let json = format!(
                r#"{{
                    "pullRequestId": 100,
                    "creationDate": "2024-01-15T10:30:00Z",
                    "status": "completed",
                    "mergeStrategy": "{strategy}",
                    "lastMergeCommit": {{"commitId": "feedfacecafef00d1234567890abcdef12345678"}}
                }}"#
            );
            let raw: PrRaw = serde_json::from_str(&json).expect("parse");
            let pr: AdoPullRequest = raw.into();
            assert_eq!(
                pr.merge_commit_sha.as_deref(),
                Some("feedfacecafef00d1234567890abcdef12345678"),
                "noFastForward variant {strategy:?} must pass the gate",
            );
        }
    }

    #[test]
    fn pr_raw_emits_merge_sha_when_strategy_absent() {
        // Older ADO API versions may omit `mergeStrategy` entirely. Preserve
        // the pre-#96 behavior of emitting the SHA in that case.
        let json = r#"{
            "pullRequestId": 101,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "lastMergeCommit": {"commitId": "feedfacecafef00d1234567890abcdef12345678"}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("feedfacecafef00d1234567890abcdef12345678"),
            "absent mergeStrategy must default to allowed (pre-#96 behavior)",
        );
    }

    #[test]
    fn pr_raw_drops_merge_sha_for_squash_strategy() {
        // Issue #96: squash rewrites history, so `lastMergeCommit.commitId`
        // does not appear on the target branch.
        for strategy in ["squash", "SQUASH", "Squash"] {
            let json = format!(
                r#"{{
                    "pullRequestId": 102,
                    "creationDate": "2024-01-15T10:30:00Z",
                    "status": "completed",
                    "mergeStrategy": "{strategy}",
                    "lastMergeCommit": {{"commitId": "feedfacecafef00d1234567890abcdef12345678"}}
                }}"#
            );
            let raw: PrRaw = serde_json::from_str(&json).expect("parse");
            let pr: AdoPullRequest = raw.into();
            assert!(
                pr.merge_commit_sha.is_none(),
                "squash variant {strategy:?} must drop the merge SHA",
            );
        }
    }

    #[test]
    fn pr_raw_drops_merge_sha_for_rebase_strategy() {
        // Issue #96: rebase replays commits onto the target; no merge
        // commit lands.
        let json = r#"{
            "pullRequestId": 103,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "mergeStrategy": "rebase",
            "lastMergeCommit": {"commitId": "feedfacecafef00d1234567890abcdef12345678"}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(pr.merge_commit_sha.is_none());
    }

    #[test]
    fn pr_raw_drops_merge_sha_for_rebase_merge_strategy() {
        // Issue #96: rebaseMerge replays then merges; the SHA we'd emit
        // does not reliably appear on the target branch.
        let json = r#"{
            "pullRequestId": 104,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "mergeStrategy": "rebaseMerge",
            "lastMergeCommit": {"commitId": "feedfacecafef00d1234567890abcdef12345678"}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(pr.merge_commit_sha.is_none());
    }

    #[test]
    fn pr_raw_reads_merge_strategy_from_completion_options_fallback() {
        // Issue #96: some ADO API versions only surface `mergeStrategy`
        // nested inside `completionOptions`. We accept both shapes.
        let json = r#"{
            "pullRequestId": 105,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "completionOptions": {"mergeStrategy": "squash"},
            "lastMergeCommit": {"commitId": "feedfacecafef00d1234567890abcdef12345678"}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert!(
            pr.merge_commit_sha.is_none(),
            "squash via completionOptions fallback must drop the merge SHA",
        );
    }

    #[test]
    fn pr_raw_prefers_top_level_merge_strategy_over_completion_options() {
        // Issue #96: when both shapes are present the top-level value
        // wins. Here completionOptions claims `squash` but the top-level
        // strategy is the merge-preserving `noFastForward`.
        let json = r#"{
            "pullRequestId": 106,
            "creationDate": "2024-01-15T10:30:00Z",
            "status": "completed",
            "mergeStrategy": "noFastForward",
            "completionOptions": {"mergeStrategy": "squash"},
            "lastMergeCommit": {"commitId": "feedfacecafef00d1234567890abcdef12345678"}
        }"#;
        let raw: PrRaw = serde_json::from_str(json).expect("parse");
        let pr: AdoPullRequest = raw.into();
        assert_eq!(
            pr.merge_commit_sha.as_deref(),
            Some("feedfacecafef00d1234567890abcdef12345678"),
            "top-level mergeStrategy must take precedence over completionOptions",
        );
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

    /// Mirror of the `to_fetch` selection inside
    /// [`AdoPrFetcher::run_with_options`]. Exercising the decision in
    /// isolation lets us verify the `--force-refresh-prs` semantics without
    /// standing up an HTTP server for `fetch_prs`.
    fn select_to_fetch(
        conn: &Connection,
        project: &str,
        ids: Vec<i64>,
        force_refresh: bool,
    ) -> Vec<i64> {
        if force_refresh {
            ids
        } else {
            let existing = get_existing_pr_numbers(conn, "azdo", project).expect("query existing");
            ids.into_iter()
                .filter(|id| !existing.contains(id))
                .collect()
        }
    }

    #[test]
    fn force_refresh_false_skips_existing_pr_ids() {
        // Default behavior: a PR already in `pull_requests` must be excluded
        // from the fetch set so `tga collect` reruns are cheap.
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr(); // pr_number = 12345
        upsert_pr(conn, &pr, "MyProject").expect("upsert");

        let to_fetch = select_to_fetch(conn, "MyProject", vec![12345, 999], false);
        assert_eq!(
            to_fetch,
            vec![999],
            "cached PR 12345 must be skipped when force_refresh is false"
        );
    }

    #[test]
    fn force_refresh_true_re_fetches_existing_pr_ids() {
        // With --force-refresh-prs, the dedup cache is bypassed: every
        // referenced PR ID is re-fetched so stale `commit_shas = '[]'` rows
        // can be backfilled (issue #95).
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let pr = sample_pr(); // pr_number = 12345
        upsert_pr(conn, &pr, "MyProject").expect("upsert");

        let to_fetch = select_to_fetch(conn, "MyProject", vec![12345, 999], true);
        assert_eq!(
            to_fetch,
            vec![12345, 999],
            "force_refresh must NOT skip already-cached PR IDs"
        );
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

    // ----- Issue #91: multi-project HTTP tests via wiremock -----

    #[tokio::test]
    async fn fetch_pr_single_project_hit() {
        // Single configured project that returns 200 — fetch_pr returns
        // Some((pr, project)) with the project name we configured.
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ProjectA/_apis/git/pullrequests/100"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(100)))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let result = fetcher.fetch_pr(100).await.expect("fetch ok");
        let (pr, project) = result.expect("PR present");
        assert_eq!(pr.pr_number, 100);
        assert_eq!(project, "ProjectA");
        drop(server);
    }

    #[tokio::test]
    async fn fetch_pr_falls_through_404_to_next_project() {
        // Core #91 regression: ProjectA returns 404, ProjectB returns the PR.
        // Must return Some((pr, "ProjectB")).
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ProjectA/_apis/git/pullrequests/200"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ProjectB/_apis/git/pullrequests/200"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(200)))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let result = fetcher.fetch_pr(200).await.expect("fetch ok");
        let (pr, project) = result.expect("PR present");
        assert_eq!(pr.pr_number, 200);
        assert_eq!(
            project, "ProjectB",
            "must report the project where PR was found"
        );
        drop(server);
    }

    #[tokio::test]
    async fn fetch_pr_all_projects_404_returns_none() {
        // Every project 404s → Ok(None) (no error).
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/A/_apis/git/pullrequests/300"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/B/_apis/git/pullrequests/300"))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["A", "B"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let result = fetcher.fetch_pr(300).await.expect("fetch ok");
        assert!(result.is_none(), "all 404s must produce Ok(None)");
        drop(server);
    }

    #[tokio::test]
    async fn fetch_pr_first_hit_wins_no_query_to_subsequent_projects() {
        // ProjectA returns the PR — ProjectB must NEVER be queried.
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/ProjectA/_apis/git/pullrequests/400"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(400)))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/ProjectB/_apis/git/pullrequests/400"))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(400)))
            .expect(0) // First-hit-wins: ProjectB MUST NOT be called.
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let (pr, project) = fetcher
            .fetch_pr(400)
            .await
            .expect("fetch ok")
            .expect("PR present");
        assert_eq!(pr.pr_number, 400);
        assert_eq!(project, "ProjectA");
        // Drop(server) verifies the .expect(0) on ProjectB mock.
        drop(server);
    }

    #[tokio::test]
    async fn run_persists_pr_under_project_where_found() {
        // ProjectA=404, ProjectB=PR. After run(), the persisted row's
        // `repository` column must be 'ProjectB' (not 'ProjectA').
        let server = MockServer::start().await;
        let pr_id: i64 = 500;

        Mock::given(method("GET"))
            .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();

        let commit_messages = vec![format!("Merged PR {pr_id}: feat: multi-project test")];
        let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
        assert_eq!(stored, 1, "exactly one PR should be persisted");

        // Verify the persisted row's `repository` column = 'ProjectB'.
        let repo: String = conn
            .query_row(
                "SELECT repository FROM pull_requests \
                 WHERE provider = 'azdo' AND pr_number = ?1",
                params![pr_id],
                |row| row.get(0),
            )
            .expect("query persisted repository");
        assert_eq!(
            repo, "ProjectB",
            "persisted repository must be the project where the PR was found"
        );
        drop(server);
    }

    #[tokio::test]
    async fn run_multi_project_does_not_skip_cached_pr_under_other_project() {
        // Regression test: in multi-project mode the cross-project cache
        // pre-filter MUST be disabled. A PR row already persisted under
        // ProjectA must NOT cause run() to skip fetching PR 123 from
        // ProjectB. Otherwise project-scoped PR ID collisions are masked.
        let server = MockServer::start().await;
        let pr_id: i64 = 123;

        // Pre-populate DB with PR 123 under ProjectA.
        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let mut existing = sample_pr();
        existing.pr_number = pr_id;
        existing.title = "stale: ProjectA copy".into();
        upsert_pr(conn, &existing, "ProjectA").expect("seed ProjectA row");

        Mock::given(method("GET"))
            .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(404))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
            .expect(1)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");

        let commit_messages = vec![format!("Merged PR {pr_id}: feat: collision case")];
        let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
        assert_eq!(stored, 1, "ProjectB hit must produce one persisted PR");

        // Two rows for pr_number=123: one under ProjectA (seeded), one
        // under ProjectB (fetched by run()).
        let total: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pull_requests \
                 WHERE provider = 'azdo' AND pr_number = ?1",
                params![pr_id],
                |row| row.get(0),
            )
            .expect("count rows");
        assert_eq!(
            total, 2,
            "expected two rows for pr_number=123 (one per project)"
        );
        let repos: Vec<String> = {
            let mut stmt = conn
                .prepare(
                    "SELECT repository FROM pull_requests \
                     WHERE provider = 'azdo' AND pr_number = ?1 ORDER BY repository",
                )
                .expect("prepare");
            let rows = stmt
                .query_map(params![pr_id], |row| row.get::<_, String>(0))
                .expect("query")
                .map(|r| r.expect("row"))
                .collect();
            rows
        };
        assert_eq!(repos, vec!["ProjectA".to_string(), "ProjectB".to_string()]);
        drop(server);
    }

    #[tokio::test]
    async fn run_single_project_still_uses_cache_short_circuit() {
        // Single-project mode preserves the cache short-circuit: a PR
        // already persisted under the sole configured project must NOT be
        // re-fetched. The HTTP mock with .expect(0) asserts no request is
        // issued.
        let server = MockServer::start().await;
        let pr_id: i64 = 123;

        let db = Database::open_in_memory().expect("db");
        let conn = db.connection();
        let mut existing = sample_pr();
        existing.pr_number = pr_id;
        upsert_pr(conn, &existing, "ProjectA").expect("seed ProjectA row");

        Mock::given(method("GET"))
            .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
            .expect(0)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");

        let commit_messages = vec![format!("Merged PR {pr_id}: cached case")];
        let stored = fetcher.run(conn, commit_messages).await.expect("run ok");
        assert_eq!(stored, 0, "cached PR must not be re-fetched");
        // Drop(server) enforces the .expect(0) on the ProjectA mock.
        drop(server);
    }

    #[tokio::test]
    async fn fetch_pr_aborts_iteration_on_401_does_not_query_next_project() {
        // 401 from ProjectA must short-circuit the project loop — ProjectB
        // must NEVER be queried.
        let server = MockServer::start().await;
        let pr_id: i64 = 901;

        Mock::given(method("GET"))
            .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(401))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
            .expect(0)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let err = fetcher
            .fetch_pr(pr_id)
            .await
            .expect_err("401 must surface as Err");
        assert!(
            matches!(err, AzdoError::Unauthorized),
            "expected AzdoError::Unauthorized, got: {err:?}"
        );
        // Drop(server) enforces the .expect(0) on ProjectB mock.
        drop(server);
    }

    #[tokio::test]
    async fn fetch_pr_aborts_iteration_on_500_does_not_query_next_project() {
        // 5xx from ProjectA must short-circuit the project loop — ProjectB
        // must NEVER be queried.
        let server = MockServer::start().await;
        let pr_id: i64 = 902;

        Mock::given(method("GET"))
            .and(path(format!("/ProjectA/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(500))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/ProjectB/_apis/git/pullrequests/{pr_id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(pr_body_json(pr_id)))
            .expect(0)
            .mount(&server)
            .await;

        let cfg = multi_project_config(&server.uri(), vec!["ProjectA", "ProjectB"]);
        let fetcher = AdoPrFetcher::new(cfg).expect("fetcher");
        let err = fetcher
            .fetch_pr(pr_id)
            .await
            .expect_err("500 must surface as Err");
        assert!(
            matches!(err, AzdoError::Http { status: 500, .. }),
            "expected AzdoError::Http {{ status: 500, .. }}, got: {err:?}"
        );
        drop(server);
    }
}

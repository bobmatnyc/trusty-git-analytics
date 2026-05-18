//! Unified project-management adapter trait.
//!
//! This module defines a common interface — [`PmAdapter`] — that abstracts
//! over the various PM/ticketing systems we integrate with (JIRA, GitHub
//! Issues, Linear, Azure DevOps). The goal is to let the classify/collect
//! pipeline enrich commits with ticket metadata without caring which backend
//! is actually serving the data.
//!
//! ## Architecture
//!
//! Each PM client implements [`PmAdapter`] and exposes:
//! - [`fetch_ticket`](PmAdapter::fetch_ticket) — single-ticket lookup (returns
//!   `Ok(None)` for "not found", reserving `Err(_)` for transport/auth errors).
//! - [`fetch_tickets`](PmAdapter::fetch_tickets) — batch lookup (default
//!   implementation runs sequentially; adapters with native batch endpoints
//!   should override).
//! - [`detect_ticket_refs`](PmAdapter::detect_ticket_refs) — recognize
//!   ticket-shaped strings (e.g. `PROJ-123`, `#42`, `AB#7`) in arbitrary text.
//! - [`health_check`](PmAdapter::health_check) — connectivity / auth probe.
//!
//! All ticket payloads are normalized to [`PmTicket`] — the system-specific
//! response JSON is preserved in [`PmTicket::raw`] for forward compatibility.
//!
//! ## Factory
//!
//! [`build_adapters`] instantiates every PM adapter that is configured in the
//! supplied [`Config`]. Adapters whose config is absent or invalid are simply
//! skipped (with a `tracing::warn!`) so the caller does not have to know which
//! integrations are enabled.

use std::sync::OnceLock;

use async_trait::async_trait;
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::core::config::Config;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Source PM system that produced a [`PmTicket`].
///
/// Used by downstream consumers (reports, classification rules) to apply
/// system-specific logic — e.g. distinguishing `AB#42` (ADO) from `#42`
/// (GitHub) when both could appear in the same commit corpus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PmSource {
    /// Atlassian JIRA (Cloud or Server).
    Jira,
    /// GitHub Issues / Pull Requests.
    GitHub,
    /// Linear (linear.app).
    Linear,
    /// Microsoft Azure DevOps (work items).
    AzureDevOps,
}

impl PmSource {
    /// Stable, lowercase string label suitable for logs, DB rows, and report
    /// columns.
    pub fn as_str(&self) -> &'static str {
        match self {
            PmSource::Jira => "jira",
            PmSource::GitHub => "github",
            PmSource::Linear => "linear",
            PmSource::AzureDevOps => "azure_devops",
        }
    }
}

/// Normalized ticket payload returned by every [`PmAdapter`] implementation.
///
/// Fields that don't exist in a given source system are filled with sensible
/// defaults (`""` for strings, empty `Vec` for `labels`, `None` for `url`).
/// The full upstream JSON is preserved verbatim in [`PmTicket::raw`] so callers
/// that need backend-specific fields (e.g. JIRA custom fields) don't have to
/// re-fetch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PmTicket {
    /// Canonical ticket identifier as reported by the PM system
    /// (e.g. `"PROJ-123"`, `"#42"`, `"AB#7"`).
    pub id: String,
    /// Short human-readable title / summary.
    pub title: String,
    /// Current workflow status (e.g. `"Done"`, `"In Progress"`, `"closed"`).
    pub status: String,
    /// Issue type / classification (e.g. `"story"`, `"bug"`, `"task"`,
    /// `"epic"`). Backends that don't expose this concept return `""`.
    pub ticket_type: String,
    /// Labels / tags. Empty when unavailable.
    pub labels: Vec<String>,
    /// Web URL to the ticket in the PM system, if known.
    pub url: Option<String>,
    /// Source PM system this ticket originated from.
    pub source: PmSource,
    /// Raw upstream payload — preserved for forward compatibility and for
    /// downstream consumers that need fields not in the normalized struct.
    pub raw: serde_json::Value,
}

/// Errors returned by [`PmAdapter`] implementations.
///
/// `From` conversions exist for the common low-level error types so that
/// implementations can use `?` against `reqwest::Error`, `serde_json::Error`,
/// etc. without manual mapping.
#[derive(Debug, thiserror::Error)]
pub enum PmError {
    /// HTTP transport or response error.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// Authentication failed — bad credentials, missing token, expired PAT, …
    #[error("authentication failed for {system}: {message}")]
    Auth {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },

    /// Ticket not found. Adapters should prefer returning `Ok(None)` for the
    /// "looked but didn't find it" case; this variant is for situations where
    /// not-found is genuinely an error condition (e.g. an explicit lookup-by-id
    /// API where the caller asserted the ticket exists).
    #[error("ticket not found: {id}")]
    NotFound {
        /// Ticket identifier that was looked up.
        id: String,
    },

    /// Rate-limited by the upstream system.
    #[error("rate limited by {system}")]
    RateLimited {
        /// System label (see [`PmSource::as_str`]).
        system: String,
    },

    /// JSON serialization/deserialization failure.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// Configuration was missing or invalid for the requested operation.
    #[error("configuration error for {system}: {message}")]
    Config {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },

    /// Catch-all for backend-specific errors that don't fit the variants above.
    #[error("{system}: {message}")]
    Other {
        /// System label (see [`PmSource::as_str`]).
        system: String,
        /// Human-readable detail.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// Common interface for all PM system clients.
///
/// Implementations must be `Send + Sync` so adapters can be stored in a
/// `Vec<Box<dyn PmAdapter>>` and shared across the async pipeline.
#[async_trait]
pub trait PmAdapter: Send + Sync {
    /// Stable, lowercase name of the PM system (e.g. `"jira"`, `"github"`,
    /// `"linear"`, `"azure_devops"`). Used for logging and error messages.
    fn name(&self) -> &str;

    /// Source enum corresponding to [`name`](Self::name).
    fn source(&self) -> PmSource;

    /// Fetch a ticket by its system-native identifier.
    ///
    /// Returns:
    /// - `Ok(Some(ticket))` on success.
    /// - `Ok(None)` if the ticket does not exist or is not visible with the
    ///   configured credentials (i.e. an authoritative "not found").
    /// - `Err(_)` on transport, auth, parsing, or rate-limit failures.
    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError>;

    /// Batch-fetch multiple tickets.
    ///
    /// The default implementation calls [`fetch_ticket`](Self::fetch_ticket)
    /// sequentially. Adapters with a native batch endpoint (e.g. JIRA's
    /// `/search`, ADO's `/workitemsbatch`) should override for efficiency.
    async fn fetch_tickets(&self, ticket_ids: &[&str]) -> Vec<Result<Option<PmTicket>, PmError>> {
        let mut out = Vec::with_capacity(ticket_ids.len());
        for id in ticket_ids {
            out.push(self.fetch_ticket(id).await);
        }
        out
    }

    /// Detect strings in `text` that look like ticket references for this
    /// system. Each adapter scopes its detection to its own format —
    /// e.g. JIRA matches `[A-Z][A-Z0-9]*-\d+`, GitHub matches `#\d+`,
    /// ADO matches `AB#\d+`.
    ///
    /// Returns the deduplicated list of matches in first-seen order.
    fn detect_ticket_refs(&self, text: &str) -> Vec<String>;

    /// Test connectivity and authentication against the upstream system.
    ///
    /// Implementations should perform a cheap call (e.g. `GET /myself` for
    /// JIRA, `GET _apis/connectionData` for ADO) and return `Ok(())` on
    /// success.
    async fn health_check(&self) -> Result<(), PmError>;
}

// ---------------------------------------------------------------------------
// Detection helpers (shared regex set)
// ---------------------------------------------------------------------------

/// Lazily-compiled JIRA / Linear identifier regex (`[A-Z][A-Z0-9]*-\d+`).
fn jira_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b([A-Z][A-Z0-9]{0,9})-(\d+)\b").expect("jira regex compiles"))
}

/// Lazily-compiled GitHub bare-issue regex (`#\d+` after start-of-line or
/// whitespace, so we don't match hex colors).
fn github_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?m)(?:^|\s)(#\d+)\b").expect("github regex compiles"))
}

/// Lazily-compiled Azure DevOps regex (`AB#\d+`).
fn azdo_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\b(AB#\d+)\b").expect("azdo regex compiles"))
}

/// Compile a user-supplied ticket-detection regex.
///
/// Why: lets users override the hardcoded JIRA / GitHub / Linear detection
/// patterns to accommodate real-world commit-message conventions
/// (lowercase keys, longer prefixes, `Fix:#123`, etc.) without code changes.
/// What: attempts to compile `pattern`; on compile failure or when the
/// regex has zero capture groups, emits a `tracing::warn!` and returns
/// `None` so the caller falls back to the default pattern.
/// Test: assert that `compile_user_regex("x", Some("\\d+"))` returns `None`
/// (no capture group), and that `compile_user_regex("x", Some("(\\d+)"))`
/// returns `Some(_)`.
fn compile_user_regex(system: &str, pattern: Option<&str>) -> Option<Regex> {
    let pat = pattern?;
    match Regex::new(pat) {
        Ok(re) => {
            if re.captures_len() < 2 {
                warn!(
                    system = system,
                    pattern = pat,
                    "ticket_regex has no capture group; ignoring and using default pattern"
                );
                None
            } else {
                Some(re)
            }
        }
        Err(e) => {
            // Should be unreachable when called from build_adapters because
            // Config::load already validates compilability — kept for defense
            // in depth and for callers that construct adapters by hand.
            warn!(
                system = system,
                pattern = pat,
                error = %e,
                "ticket_regex failed to compile; using default pattern"
            );
            None
        }
    }
}

/// Extract deduplicated matches for the user-supplied regex's first capture
/// group from `text`.
///
/// Why: user-supplied regexes always expose the ticket ID in group 1; this
/// differs from the built-in JIRA pattern, which uses two groups and joins
/// them. Keeping the logic separate avoids over-generalizing
/// [`extract_unique`].
fn extract_user_regex(re: &Regex, text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str().to_string();
            if seen.insert(s.clone()) {
                out.push(s);
            }
        }
    }
    out
}

/// Extract deduplicated matches for `re`'s first capture group from `text`.
fn extract_unique(re: &Regex, text: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        // Some patterns have one group (whole match), some have two.
        let m = cap.get(1).map(|m| m.as_str().to_string());
        if let Some(s) = m {
            // For JIRA pattern we want the full "KEY-N", not just "KEY".
            // Detect by checking if there's a 2nd group.
            let full = if cap.len() > 2 {
                match (cap.get(1), cap.get(2)) {
                    (Some(a), Some(b)) => format!("{}-{}", a.as_str(), b.as_str()),
                    _ => s,
                }
            } else {
                s
            };
            if seen.insert(full.clone()) {
                out.push(full);
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Adapter implementations
// ---------------------------------------------------------------------------

/// PM adapter wrapping [`crate::collect::jira::JiraClient`].
pub struct JiraAdapter {
    inner: crate::collect::jira::JiraClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default JIRA pattern.
    ticket_regex: Option<Regex>,
}

impl JiraAdapter {
    /// Construct from an existing [`crate::collect::jira::JiraClient`]
    /// using the default JIRA detection regex.
    pub fn new(inner: crate::collect::jira::JiraClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex
    /// string. The string is pre-validated at config-load time, so a parse
    /// failure here is treated as a non-fatal warning and the adapter falls
    /// back to the default pattern. A regex with no capture groups is also
    /// rejected with a warning.
    pub fn with_ticket_regex(
        inner: crate::collect::jira::JiraClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("jira", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for JiraAdapter {
    fn name(&self) -> &str {
        "jira"
    }

    fn source(&self) -> PmSource {
        PmSource::Jira
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        match self.inner.fetch_issue(ticket_id).await {
            Ok(Some(issue)) => {
                let raw = serde_json::json!({
                    "key": issue.key,
                    "summary": issue.summary,
                    "status": issue.status,
                    "issuetype": issue.issue_type,
                });
                Ok(Some(PmTicket {
                    id: issue.key,
                    title: issue.summary,
                    status: issue.status,
                    ticket_type: issue.issue_type,
                    labels: Vec::new(),
                    url: None,
                    source: PmSource::Jira,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("jira", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(jira_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // No dedicated health endpoint wired yet — issue a benign lookup that
        // returns Ok(None) on 404 and Err on transport/auth failure.
        match self.inner.fetch_issue("HEALTH-0").await {
            Ok(_) => Ok(()),
            Err(e) => Err(collect_err_to_pm("jira", e)),
        }
    }
}

/// PM adapter wrapping [`crate::collect::github::GitHubClient`].
///
/// GitHub's `Issues` API is a superset of its `Pulls` API — both share the
/// `#N` namespace. `fetch_ticket` accepts either `"#42"` or `"42"` and
/// delegates to [`crate::collect::github::GitHubClient::fetch_issue`].
pub struct GitHubAdapter {
    inner: crate::collect::github::GitHubClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default GitHub pattern.
    ticket_regex: Option<Regex>,
}

impl GitHubAdapter {
    /// Construct from an existing [`crate::collect::github::GitHubClient`]
    /// using the default GitHub detection regex.
    pub fn new(inner: crate::collect::github::GitHubClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex.
    /// See [`JiraAdapter::with_ticket_regex`] for semantics.
    pub fn with_ticket_regex(
        inner: crate::collect::github::GitHubClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("github", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for GitHubAdapter {
    fn name(&self) -> &str {
        "github"
    }

    fn source(&self) -> PmSource {
        PmSource::GitHub
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        // GitHub ticket refs may carry a leading `#`. Strip it so callers
        // can pass either `#42` or `42`. A non-numeric id is treated as
        // "not a GitHub issue" → `Ok(None)`.
        let numeric = ticket_id.trim_start_matches('#');
        let number: u64 = match numeric.parse() {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };

        match self.inner.fetch_issue(number).await {
            Ok(Some(issue)) => {
                let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
                let url = issue.html_url.clone();
                let raw = serde_json::to_value(&issue)?;
                Ok(Some(PmTicket {
                    id: format!("#{}", issue.number),
                    title: issue.title,
                    status: issue.state,
                    ticket_type: "issue".into(),
                    labels,
                    url: Some(url),
                    source: PmSource::GitHub,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("github", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(github_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // Token-presence check — until a dedicated `/zen` ping is added,
        // we just assert that *some* token is configured.
        if self.inner.has_token() {
            Ok(())
        } else {
            Err(PmError::Auth {
                system: "github".into(),
                message: "no token configured".into(),
            })
        }
    }
}

/// PM adapter wrapping [`crate::collect::linear::LinearClient`].
pub struct LinearAdapter {
    inner: crate::collect::linear::LinearClient,
    /// Optional user-supplied detection regex. When `None`, the adapter
    /// falls back to the shared default JIRA-shaped pattern.
    ticket_regex: Option<Regex>,
}

impl LinearAdapter {
    /// Construct from an existing [`crate::collect::linear::LinearClient`]
    /// using the default Linear detection regex.
    pub fn new(inner: crate::collect::linear::LinearClient) -> Self {
        Self {
            inner,
            ticket_regex: None,
        }
    }

    /// Construct from a client and an optional user-supplied detection regex.
    /// See [`JiraAdapter::with_ticket_regex`] for semantics.
    pub fn with_ticket_regex(
        inner: crate::collect::linear::LinearClient,
        pattern: Option<&str>,
    ) -> Self {
        Self {
            inner,
            ticket_regex: compile_user_regex("linear", pattern),
        }
    }
}

#[async_trait]
impl PmAdapter for LinearAdapter {
    fn name(&self) -> &str {
        "linear"
    }

    fn source(&self) -> PmSource {
        PmSource::Linear
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        match self.inner.fetch_issue(ticket_id).await {
            Ok(Some(issue)) => {
                let raw = serde_json::to_value(&issue)?;
                Ok(Some(PmTicket {
                    id: issue.identifier,
                    title: issue.title,
                    status: issue.state,
                    ticket_type: String::new(),
                    labels: Vec::new(),
                    url: if issue.url.is_empty() {
                        None
                    } else {
                        Some(issue.url)
                    },
                    source: PmSource::Linear,
                    raw,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(collect_err_to_pm("linear", e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        // Linear identifiers are a strict subset of the JIRA `KEY-N` shape
        // by default; users can override via `linear.ticket_regex` to
        // accommodate workspace-specific team prefixes.
        match &self.ticket_regex {
            Some(re) => extract_user_regex(re, text),
            None => extract_unique(jira_ref_re(), text),
        }
    }

    async fn health_check(&self) -> Result<(), PmError> {
        // No cheap health endpoint exposed yet — do a no-op fetch.
        match self.inner.fetch_issue("HEALTH-0").await {
            Ok(_) => Ok(()),
            Err(e) => Err(collect_err_to_pm("linear", e)),
        }
    }
}

/// PM adapter wrapping [`crate::collect::azdo::AzureDevOpsClient`].
///
/// Work-item fetching is gated behind ADO Phase 6; until then,
/// `fetch_ticket` returns `Ok(None)`. `health_check` uses the
/// `GET _apis/connectionData` probe that already exists.
pub struct AzureDevOpsAdapter {
    inner: crate::collect::azdo::AzureDevOpsClient,
}

impl AzureDevOpsAdapter {
    /// Construct from an existing [`crate::collect::azdo::AzureDevOpsClient`].
    pub fn new(inner: crate::collect::azdo::AzureDevOpsClient) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl PmAdapter for AzureDevOpsAdapter {
    fn name(&self) -> &str {
        "azure_devops"
    }

    fn source(&self) -> PmSource {
        PmSource::AzureDevOps
    }

    async fn fetch_ticket(&self, ticket_id: &str) -> Result<Option<PmTicket>, PmError> {
        // ADO IDs come in two flavors: bare integers (`123`) or `AB#123`.
        // Strip the `AB#` prefix when present so callers can pass either.
        let numeric = ticket_id.trim_start_matches("AB#");
        let id: u32 = match numeric.parse() {
            Ok(n) => n,
            Err(_) => return Ok(None),
        };

        match self.inner.get_work_items(&[id]).await {
            Ok(items) => Ok(items.into_iter().next().map(|w| {
                let raw = serde_json::json!({
                    "id": w.id,
                    "title": w.title,
                    "state": w.state,
                    "workItemType": w.work_item_type,
                    "tags": w.tags,
                    "teamProject": w.team_project,
                    "url": w.url,
                });
                PmTicket {
                    id: format!("AB#{}", w.id),
                    title: w.title,
                    status: w.state,
                    ticket_type: w.work_item_type,
                    labels: w.tags,
                    url: w.url,
                    source: PmSource::AzureDevOps,
                    raw,
                }
            })),
            // Defensive: any residual NotImplemented variants in the future
            // should degrade gracefully rather than fail the pipeline.
            Err(crate::collect::azdo::AzdoError::NotImplemented { .. }) => Ok(None),
            Err(e) => Err(azdo_err_to_pm(e)),
        }
    }

    fn detect_ticket_refs(&self, text: &str) -> Vec<String> {
        extract_unique(azdo_ref_re(), text)
    }

    async fn health_check(&self) -> Result<(), PmError> {
        match self.inner.test_connection().await {
            Ok(_) => Ok(()),
            Err(e) => Err(azdo_err_to_pm(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Error conversions (kept internal so adapter authors don't have to wire them)
// ---------------------------------------------------------------------------

/// Best-effort mapping from [`crate::collect::errors::CollectError`] to
/// [`PmError`]. Tagged with `system` so the resulting error message names the
/// PM backend that produced it.
fn collect_err_to_pm(system: &'static str, e: crate::collect::errors::CollectError) -> PmError {
    use crate::collect::errors::CollectError;
    match e {
        CollectError::Http(err) => PmError::Http(err),
        CollectError::Json(err) => PmError::Serialization(err),
        CollectError::Config(msg) => PmError::Config {
            system: system.to_string(),
            message: msg,
        },
        other => PmError::Other {
            system: system.to_string(),
            message: other.to_string(),
        },
    }
}

/// Best-effort mapping from [`crate::collect::azdo::AzdoError`] to
/// [`PmError`]. HTTP-status variants surface as `Auth`, `NotFound`, etc.
fn azdo_err_to_pm(e: crate::collect::azdo::AzdoError) -> PmError {
    use crate::collect::azdo::AzdoError;
    match e {
        AzdoError::Unauthorized => PmError::Auth {
            system: "azure_devops".into(),
            message: "401 unauthorized".into(),
        },
        AzdoError::Forbidden => PmError::Auth {
            system: "azure_devops".into(),
            message: "403 forbidden".into(),
        },
        AzdoError::InvalidCredentials(msg) => PmError::Auth {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::NotFound => PmError::NotFound {
            id: "(connection)".into(),
        },
        AzdoError::Request(err) => PmError::Http(err),
        AzdoError::Config(msg) => PmError::Config {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::Parse(msg) | AzdoError::InvalidUrl(msg) => PmError::Other {
            system: "azure_devops".into(),
            message: msg,
        },
        AzdoError::Http { status, message } => PmError::Other {
            system: "azure_devops".into(),
            message: format!("HTTP {status}: {message}"),
        },
        AzdoError::NotImplemented { method, phase } => PmError::Other {
            system: "azure_devops".into(),
            message: format!("not implemented: {method} (phase {phase})"),
        },
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build every PM adapter that is configured in `config`.
///
/// Adapters whose configuration section is missing are silently skipped.
/// Adapters whose configuration section is present but invalid (e.g. JIRA
/// without a `url`) are skipped with a `tracing::warn!` so a single bad
/// integration doesn't fail the whole pipeline.
///
/// Returned adapters are boxed as `dyn PmAdapter` so the caller can iterate
/// uniformly without knowing the concrete types.
pub fn build_adapters(config: &Config) -> Vec<Box<dyn PmAdapter>> {
    let mut out: Vec<Box<dyn PmAdapter>> = Vec::new();

    if let Some(cfg) = &config.jira {
        match crate::collect::jira::JiraClient::new(cfg) {
            Ok(client) => out.push(Box::new(JiraAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping JIRA adapter: invalid config"),
        }
    }

    if let Some(cfg) = &config.github {
        match crate::collect::github::GitHubClient::new(cfg) {
            Ok(client) => out.push(Box::new(GitHubAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping GitHub adapter: invalid config"),
        }
    }

    if let Some(cfg) = &config.linear {
        match crate::collect::linear::LinearClient::new(cfg) {
            Ok(client) => out.push(Box::new(LinearAdapter::with_ticket_regex(
                client,
                cfg.ticket_regex.as_deref(),
            ))),
            Err(e) => warn!(error = %e, "skipping Linear adapter: invalid config"),
        }
    }

    if let Some(cfg) = config.azure_devops_config() {
        let client = crate::collect::azdo::AzureDevOpsClient::new(cfg.clone());
        out.push(Box::new(AzureDevOpsAdapter::new(client)));
    }

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_source_as_str_is_stable() {
        assert_eq!(PmSource::Jira.as_str(), "jira");
        assert_eq!(PmSource::GitHub.as_str(), "github");
        assert_eq!(PmSource::Linear.as_str(), "linear");
        assert_eq!(PmSource::AzureDevOps.as_str(), "azure_devops");
    }

    #[test]
    fn jira_ref_re_extracts_keys() {
        let out = extract_unique(jira_ref_re(), "PROJ-123 and ENG-456 and PROJ-123 again");
        assert_eq!(out, vec!["PROJ-123".to_string(), "ENG-456".to_string()]);
    }

    #[test]
    fn github_ref_re_extracts_numbers() {
        let out = extract_unique(github_ref_re(), "fixes #42 see also #99 and #42 again");
        assert_eq!(out, vec!["#42".to_string(), "#99".to_string()]);
    }

    #[test]
    fn github_ref_re_ignores_hex_colors() {
        let out = extract_unique(github_ref_re(), "color #abc123 not a ticket");
        assert!(out.is_empty());
    }

    #[test]
    fn azdo_ref_re_extracts_ab_refs() {
        let out = extract_unique(azdo_ref_re(), "AB#1234 and AB#7 and AB#1234 again");
        assert_eq!(out, vec!["AB#1234".to_string(), "AB#7".to_string()]);
    }

    #[test]
    fn build_adapters_returns_empty_for_default_config() {
        let cfg = Config::default();
        let adapters = build_adapters(&cfg);
        assert!(adapters.is_empty());
    }

    #[test]
    fn build_adapters_includes_ado_when_configured() {
        use crate::core::config::{AzureDevOpsConfig, PmConfig};
        let cfg = Config {
            pm: Some(PmConfig {
                azure_devops: Some(AzureDevOpsConfig {
                    organization_url: "https://dev.azure.com/myorg".into(),
                    pat: "x".into(),
                    project: Some("MyProject".into()),
                    projects: vec![],
                    ticket_regex: r"AB#(\d+)".into(),
                    team_keys: vec![],
                    fetch_on_reference: true,
                    fetch_prs: false,
                }),
            }),
            ..Default::default()
        };
        let adapters = build_adapters(&cfg);
        assert_eq!(adapters.len(), 1);
        assert_eq!(adapters[0].name(), "azure_devops");
        assert_eq!(adapters[0].source(), PmSource::AzureDevOps);
    }

    /// Verify the `GitHubIssue` → `PmTicket` mapping used inside
    /// `GitHubAdapter::fetch_ticket`. Constructed directly so the test
    /// does not depend on network access.
    ///
    /// Why: protects the contract that `id` is re-prefixed with `#`,
    /// labels are flattened to strings, and `ticket_type` is `"issue"`.
    /// What: builds a `GitHubIssue`, runs the same mapping, asserts fields.
    /// Test: pass when `id == "#42"`, labels are `["bug", "p1"]`, type is
    /// `"issue"`, status mirrors `state`.
    #[test]
    fn github_issue_maps_to_pm_ticket() {
        use crate::collect::github::{GhLabel, GitHubIssue};

        let issue = GitHubIssue {
            number: 42,
            title: "Crash".into(),
            state: "open".into(),
            html_url: "https://github.com/o/r/issues/42".into(),
            labels: vec![
                GhLabel { name: "bug".into() },
                GhLabel { name: "p1".into() },
            ],
            body: Some("repro".into()),
        };

        // Replicate the mapping inside fetch_ticket.
        let labels: Vec<String> = issue.labels.iter().map(|l| l.name.clone()).collect();
        let url = issue.html_url.clone();
        let raw = serde_json::to_value(&issue).expect("raw serializes");
        let ticket = PmTicket {
            id: format!("#{}", issue.number),
            title: issue.title.clone(),
            status: issue.state.clone(),
            ticket_type: "issue".into(),
            labels,
            url: Some(url),
            source: PmSource::GitHub,
            raw,
        };

        assert_eq!(ticket.id, "#42");
        assert_eq!(ticket.title, "Crash");
        assert_eq!(ticket.status, "open");
        assert_eq!(ticket.ticket_type, "issue");
        assert_eq!(ticket.labels, vec!["bug".to_string(), "p1".to_string()]);
        assert_eq!(
            ticket.url.as_deref(),
            Some("https://github.com/o/r/issues/42")
        );
        assert_eq!(ticket.source, PmSource::GitHub);
        assert!(ticket.raw.get("body").is_some());
    }

    /// Verify that a user-supplied regex with no capture group is rejected
    /// at adapter-construction time (returns `None`, falls back to default).
    ///
    /// Why: protects the documented contract that capture group 1 is the
    /// ticket ID; a zero-group regex would silently break detection.
    /// What: calls `compile_user_regex` with a pattern that has no groups
    /// and asserts the result is `None`.
    /// Test: `compile_user_regex("x", Some("\\d+"))` returns `None`.
    #[test]
    fn compile_user_regex_rejects_zero_capture_groups() {
        assert!(compile_user_regex("jira", Some(r"\d+")).is_none());
        assert!(compile_user_regex("jira", Some(r"(\d+)")).is_some());
        assert!(compile_user_regex("jira", None).is_none());
    }

    /// Verify that an invalid regex string yields `None` (caller falls back
    /// to the default pattern).
    #[test]
    fn compile_user_regex_handles_invalid_pattern() {
        assert!(compile_user_regex("github", Some("[")).is_none());
    }

    /// Verify `extract_user_regex` extracts and deduplicates group-1 matches.
    #[test]
    fn extract_user_regex_dedupes_group_one() {
        let re = Regex::new(r"(?i)([a-z]+-\d+)").expect("compiles");
        let out = extract_user_regex(&re, "fix proj-123 and PROJ-123 and other-9");
        assert_eq!(
            out,
            vec![
                "proj-123".to_string(),
                "PROJ-123".to_string(),
                "other-9".to_string()
            ]
        );
    }

    /// JIRA adapter with a custom regex picks up lowercase keys.
    #[test]
    fn jira_adapter_uses_user_regex_for_lowercase_keys() {
        // Default JIRA pattern rejects lowercase — verify override fixes it.
        let cfg = crate::core::config::JiraConfig {
            url: Some("https://x.atlassian.net".into()),
            username: Some("u".into()),
            token: Some("t".into()),
            ..Default::default()
        };
        let client = crate::collect::jira::JiraClient::new(&cfg).expect("client");
        let adapter = JiraAdapter::with_ticket_regex(client, Some(r"(?i)\b([A-Z][A-Z0-9]*-\d+)\b"));
        let refs = adapter.detect_ticket_refs("see proj-123 and ENG-456");
        assert!(refs.contains(&"proj-123".to_string()));
        assert!(refs.contains(&"ENG-456".to_string()));
    }

    /// GitHub adapter with a custom regex picks up `Fix:#123` (no leading space).
    #[test]
    fn github_adapter_uses_user_regex_for_tight_refs() {
        let cfg = crate::core::config::GithubConfig {
            token: Some("t".into()),
            repo: Some("owner/name".into()),
            ..Default::default()
        };
        let client = crate::collect::github::GitHubClient::new(&cfg).expect("client");
        let adapter = GitHubAdapter::with_ticket_regex(client, Some(r"(#\d+)"));
        let refs = adapter.detect_ticket_refs("Fix:#123 and (#456) and closes#42");
        assert_eq!(
            refs,
            vec!["#123".to_string(), "#456".to_string(), "#42".to_string()]
        );
    }

    /// Linear adapter falls back to the default pattern when no override.
    #[test]
    fn linear_adapter_defaults_when_no_override() {
        let cfg = crate::core::config::LinearConfig {
            api_key: Some("k".into()),
            ..Default::default()
        };
        let client = crate::collect::linear::LinearClient::new(&cfg).expect("client");
        let adapter = LinearAdapter::with_ticket_regex(client, None);
        let refs = adapter.detect_ticket_refs("ENG-1 and FE-2");
        assert_eq!(refs, vec!["ENG-1".to_string(), "FE-2".to_string()]);
    }

    /// Smoke-test: an adapter built behind `dyn PmAdapter` can still call
    /// `detect_ticket_refs` (verifies object-safety).
    #[test]
    fn adapters_are_object_safe_for_detect() {
        use crate::core::config::{AzureDevOpsConfig, PmConfig};
        let cfg = Config {
            pm: Some(PmConfig {
                azure_devops: Some(AzureDevOpsConfig {
                    organization_url: "https://dev.azure.com/myorg".into(),
                    pat: "x".into(),
                    project: Some("P".into()),
                    projects: vec![],
                    ticket_regex: r"AB#(\d+)".into(),
                    team_keys: vec![],
                    fetch_on_reference: true,
                    fetch_prs: false,
                }),
            }),
            ..Default::default()
        };
        let adapters = build_adapters(&cfg);
        let refs = adapters[0].detect_ticket_refs("see AB#7 and AB#8");
        assert_eq!(refs, vec!["AB#7".to_string(), "AB#8".to_string()]);
    }

    // ---------------------------------------------------------------------
    // Issue #76 — fill test coverage gaps for the ticket_regex fix
    // ---------------------------------------------------------------------

    /// End-to-end: in-memory SQLite + an ADO adapter with detected ticket
    /// references is persisted via the existing `work_items` /
    /// `commit_work_items` writers.
    ///
    /// Why: previously no test exercised the full path from detection through
    /// to SQLite persistence — gap #1 in #76. This test verifies that
    /// detection output can be persisted with the existing DB API so any
    /// future change to detection that breaks the schema mapping surfaces
    /// here.
    /// What: opens an in-memory `Database`, runs `detect_ticket_refs` on a
    /// list of commit messages, upserts the resulting `WorkItemRow`s, links
    /// them to fake commit SHAs, then reads them back via
    /// `get_work_items_for_commit` and asserts the round-trip is faithful.
    /// Test: passes when 2 unique work items are stored and both are linked
    /// to commit "sha1".
    #[test]
    fn collector_persists_detected_ado_refs_to_sqlite() {
        use crate::core::config::{AzureDevOpsConfig, PmConfig};
        use crate::core::db::{Database, WorkItemRow};

        // 1. Build an in-memory DB (runs migrations + WAL pragma).
        let mut db = Database::open_in_memory().expect("open in-memory db");

        // 2. Build an ADO adapter via the public factory.
        let cfg = Config {
            pm: Some(PmConfig {
                azure_devops: Some(AzureDevOpsConfig {
                    organization_url: "https://dev.azure.com/myorg".into(),
                    pat: "x".into(),
                    project: Some("P".into()),
                    projects: vec![],
                    ticket_regex: r"AB#(\d+)".into(),
                    team_keys: vec![],
                    fetch_on_reference: true,
                    fetch_prs: false,
                }),
            }),
            ..Default::default()
        };
        let adapters = build_adapters(&cfg);
        let adapter = adapters
            .iter()
            .find(|a| a.source() == PmSource::AzureDevOps)
            .expect("ADO adapter built");

        // 3. Run detection against a small commit corpus.
        let messages = [
            ("sha1", "Fixes AB#42 and AB#100"),
            ("sha1", "another commit referencing AB#42 again"),
            ("sha2", "no ticket here"),
        ];
        let mut detected: Vec<(String, String)> = Vec::new();
        for (sha, msg) in &messages {
            for id in adapter.detect_ticket_refs(msg) {
                detected.push(((*sha).to_string(), id));
            }
        }
        assert!(!detected.is_empty(), "detection produced refs");

        // 4. Persist via the existing DB writers in a single transaction.
        let conn = db.connection_mut();
        let tx = conn.transaction().expect("begin tx");
        let mut seen = std::collections::HashSet::new();
        for (sha, id) in &detected {
            let row = WorkItemRow {
                id: id.trim_start_matches("AB#").to_string(),
                source: "azdo".into(),
                title: format!("ticket {id}"),
                status: "Active".into(),
                item_type: "Bug".into(),
                tags: None,
                project: Some("P".into()),
                url: None,
                raw_json: None,
            };
            if seen.insert(row.id.clone()) {
                crate::core::db::work_items::upsert_work_item(&tx, &row).expect("upsert work item");
            }
            crate::core::db::work_items::link_commit_work_item(&tx, sha, &row.id, "azdo")
                .expect("link commit");
        }
        tx.commit().expect("commit tx");

        // 5. Read back and assert the round-trip.
        let conn = db.connection();
        let sha1_items = crate::core::db::work_items::get_work_items_for_commit(conn, "sha1")
            .expect("query sha1 items");
        let mut sha1_ids: Vec<String> = sha1_items.iter().map(|w| w.id.clone()).collect();
        sha1_ids.sort();
        assert_eq!(sha1_ids, vec!["100".to_string(), "42".to_string()]);

        let all = crate::core::db::work_items::list_work_items(conn, "azdo")
            .expect("list azdo work items");
        assert_eq!(all.len(), 2, "two unique work items stored");
    }

    /// YAML → `build_adapters` → `detect_ticket_refs` round-trip with a
    /// non-default `ticket_regex` actually changing detection output.
    ///
    /// Why: gap #2 in #76. The config struct's YAML tests prove the field
    /// parses; the adapter's unit tests prove a constructed regex changes
    /// behavior — but nothing previously connected the two. A regression
    /// where `build_adapters` forgets to forward `ticket_regex` would have
    /// gone undetected.
    /// What: parses a YAML config string with a lowercase-friendly
    /// `jira.ticket_regex`, calls `build_adapters`, and asserts the JIRA
    /// adapter's `detect_ticket_refs` honors the override (matches lowercase
    /// keys the default pattern would reject).
    /// Test: passes when `proj-123` is detected with the override and would
    /// be rejected by the default JIRA pattern.
    #[test]
    fn pm_yaml_custom_ticket_regex_flows_to_adapter_detection() {
        // Override: case-insensitive JIRA-shape keys (default is upper only).
        let yaml = r#"
jira:
  url: "https://example.atlassian.net"
  username: "u"
  token: "t"
  ticket_regex: "(?i)\\b([A-Z][A-Z0-9]*-\\d+)\\b"
"#;
        let cfg: Config = serde_yaml::from_str(yaml).expect("yaml parses");
        let adapters = build_adapters(&cfg);
        let jira = adapters
            .iter()
            .find(|a| a.source() == PmSource::Jira)
            .expect("jira adapter built from yaml");

        let refs = jira.detect_ticket_refs("see proj-123 and ENG-456");
        // The override permits lowercase; default JIRA pattern would not.
        assert!(refs.iter().any(|s| s == "proj-123"));
        assert!(refs.iter().any(|s| s == "ENG-456"));

        // Sanity: without the override, lowercase is rejected.
        let default_adapter = JiraAdapter::with_ticket_regex(
            crate::collect::jira::JiraClient::new(cfg.jira.as_ref().unwrap()).expect("client"),
            None,
        );
        let default_refs = default_adapter.detect_ticket_refs("see proj-123 and ENG-456");
        assert!(!default_refs.iter().any(|s| s == "proj-123"));
        assert!(default_refs.iter().any(|s| s == "ENG-456"));
    }

    /// Capture-group *content* behavior: a regex whose group 1 matches but
    /// captures non-ticket-shaped strings is accepted at compile time and
    /// returns whatever group 1 captured.
    ///
    /// Why: gap #3 in #76. `compile_user_regex` validates capture-group
    /// *count* (must be >= 1) but cannot statically know whether group 1
    /// will capture useful content. This test documents the current
    /// contract: garbage-in, garbage-out — detection returns whatever the
    /// regex says, never silently fabricates results, never panics.
    /// What: feeds a regex with an *optional* capture group (which can match
    /// while capturing nothing) and a regex that captures non-ticket text;
    /// asserts behavior is well-defined and non-silent.
    /// Test: optional group matches but group 1 may be empty/absent — output
    /// contains only the captures the regex actually produced; no panic.
    #[test]
    fn user_regex_with_useless_capture_group_returns_well_defined_output() {
        // Optional capture group: matches every space-separated word but
        // group 1 may capture nothing. `extract_user_regex` only pushes
        // when `cap.get(1)` is Some, so a never-captured group yields [].
        let re = Regex::new(r"foo(\d+)?bar").expect("compiles");
        // "foobar" — group 1 is None for this match → no output pushed.
        let out = extract_user_regex(&re, "foobar and foobar");
        assert!(
            out.is_empty(),
            "optional group with no capture yields empty"
        );

        // Group 1 captures non-numeric text — extraction still returns it
        // verbatim. Downstream code that needs a u32 must handle parse
        // errors; this layer is intentionally type-agnostic so per-system
        // adapters can apply their own validation.
        let re = Regex::new(r"BUG-([A-Z]+)").expect("compiles");
        let out = extract_user_regex(&re, "see BUG-ABC and BUG-XYZ");
        assert_eq!(out, vec!["ABC".to_string(), "XYZ".to_string()]);

        // Zero-width assertion only: `(?:^)` — no capture group, fails
        // `compile_user_regex`'s capture-count gate → falls back to default.
        assert!(compile_user_regex("jira", Some(r"^")).is_none());
    }

    /// Verifies that supplying a regex with no capture group emits a
    /// `tracing::warn!` carrying the offending pattern.
    ///
    /// Why: gap #4 in #76. The fallback to the default pattern is now
    /// defense-in-depth — if the warn ever stops firing, the silent-failure
    /// path is back. This test pins the observable side-effect, not just
    /// the return value.
    /// What: invokes `compile_user_regex` with a zero-capture pattern under
    /// the `tracing_test::traced_test` macro and asserts a log line with
    /// the pattern text was recorded at WARN level.
    /// Test: `logs_contain` returns true for a substring of the warn body.
    #[test]
    #[tracing_test::traced_test]
    fn compile_user_regex_emits_warn_when_no_capture_groups() {
        let result = compile_user_regex("jira", Some(r"\d+"));
        assert!(result.is_none());
        // The warn message includes the literal text "no capture group" and
        // the offending pattern as a field — tracing_test captures both.
        assert!(logs_contain("no capture group"));
        assert!(logs_contain("\\d+"));
    }

    /// Same as above but for the invalid-pattern branch (the `Err(e)`
    /// match arm of `compile_user_regex`).
    #[test]
    #[tracing_test::traced_test]
    fn compile_user_regex_emits_warn_when_pattern_is_invalid() {
        let result = compile_user_regex("github", Some("["));
        assert!(result.is_none());
        assert!(logs_contain("failed to compile"));
    }

    /// Empty-corpus path: detection on `""` and on a non-matching string
    /// returns an empty Vec without panicking, for every adapter.
    ///
    /// Why: gap #5 in #76. Empty / no-match inputs are easy to overlook
    /// and are real (e.g. binary-only commits, merge commits with no body).
    /// What: walks every adapter built from a fully-populated `Config` and
    /// asserts `detect_ticket_refs("")` and a no-ticket sentence are both
    /// empty.
    /// Test: every adapter returns `vec![]` for both inputs.
    #[test]
    fn detect_ticket_refs_handles_empty_corpus() {
        use crate::core::config::{
            AzureDevOpsConfig, GithubConfig, JiraConfig, LinearConfig, PmConfig,
        };
        let cfg = Config {
            jira: Some(JiraConfig {
                url: Some("https://x.atlassian.net".into()),
                username: Some("u".into()),
                token: Some("t".into()),
                ..Default::default()
            }),
            github: Some(GithubConfig {
                token: Some("t".into()),
                repo: Some("o/n".into()),
                ..Default::default()
            }),
            linear: Some(LinearConfig {
                api_key: Some("k".into()),
                ..Default::default()
            }),
            pm: Some(PmConfig {
                azure_devops: Some(AzureDevOpsConfig {
                    organization_url: "https://dev.azure.com/myorg".into(),
                    pat: "x".into(),
                    project: Some("P".into()),
                    projects: vec![],
                    ticket_regex: r"AB#(\d+)".into(),
                    team_keys: vec![],
                    fetch_on_reference: true,
                    fetch_prs: false,
                }),
            }),
            ..Default::default()
        };

        let adapters = build_adapters(&cfg);
        assert!(!adapters.is_empty(), "expected at least one adapter");

        for adapter in &adapters {
            let empty = adapter.detect_ticket_refs("");
            assert!(
                empty.is_empty(),
                "{} adapter must return empty for empty input",
                adapter.name()
            );

            let no_match = adapter.detect_ticket_refs("a commit message with no ticket refs");
            assert!(
                no_match.is_empty(),
                "{} adapter must return empty for no-match input, got {:?}",
                adapter.name(),
                no_match
            );
        }
    }
}

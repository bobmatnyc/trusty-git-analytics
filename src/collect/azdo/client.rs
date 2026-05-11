//! Azure DevOps client — Phases 2–6.
//!
//! Implements an authenticated HTTP session against the Azure DevOps REST API
//! (`api-version=7.1`) with the following endpoints:
//!
//! * [`AzureDevOpsClient::test_connection`] — `GET _apis/connectionData`
//! * [`AzureDevOpsClient::get_projects`]    — `GET _apis/projects`
//! * [`AzureDevOpsClient::get_work_item_types`] — `GET {proj}/_apis/wit/workitemtypes` (Phase 3)
//! * [`AzureDevOpsClient::get_fields`]      — `GET {proj}/_apis/wit/fields` (Phase 3)
//! * [`AzureDevOpsClient::run_wiql`]        — `POST {proj}/_apis/wit/wiql` (Phase 4)
//! * [`AzureDevOpsClient::get_recent_work_item_ids`] — convenience WIQL (Phase 4)
//! * [`AzureDevOpsClient::get_work_items`]  — `POST _apis/wit/workitemsbatch` (Phase 5)
//!
//! Authentication uses HTTP Basic with an empty username and the PAT as the
//! password — the standard ADO convention.

use serde::{Deserialize, Serialize};

use crate::core::config::AzureDevOpsConfig;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by the Azure DevOps client.
#[derive(Debug, thiserror::Error)]
pub enum AzdoError {
    /// The method is not yet implemented. `phase` indicates the planned
    /// phase number (e.g. 6 for work items).
    #[error("not implemented: {method} is planned for Phase {phase}")]
    NotImplemented {
        /// Name of the method that would have performed work.
        method: String,
        /// Phase number in which this method will be implemented.
        phase: u32,
    },

    /// Credentials were rejected at the format-validation stage.
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    /// The configured URL is malformed or not an Azure DevOps URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    /// HTTP request returned an unhandled status code.
    #[error("HTTP error {status}: {message}")]
    Http {
        /// HTTP status code.
        status: u16,
        /// Response body or reason phrase.
        message: String,
    },

    /// HTTP 401 — PAT is missing, malformed, or rejected by ADO.
    #[error("authentication failed (401): check PAT and organisation URL")]
    Unauthorized,

    /// HTTP 403 — PAT is valid but lacks scope for the requested resource.
    #[error("access denied (403): PAT lacks required scope")]
    Forbidden,

    /// HTTP 404 — the organisation URL is wrong or the resource does not exist.
    #[error("organisation not found (404): check organization_url")]
    NotFound,

    /// Transport-level failure (DNS, TLS, timeout, connection reset, ...).
    #[error("request error: {0}")]
    Request(#[from] reqwest::Error),

    /// Response body could not be parsed as the expected JSON shape.
    #[error("response parse error: {0}")]
    Parse(String),
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Result of an Azure DevOps connection probe.
///
/// `status` is `"connected"` on a successful `GET _apis/connectionData`,
/// `"failed"` if the probe completed but ADO returned a non-success status
/// (this variant is not currently returned — failures bubble as `AzdoError`
/// instead), or `"stub"` if produced by [`AzureDevOpsClient::test_connection_stub`].
#[derive(Debug, Clone, Serialize)]
pub struct AzdoConnectionInfo {
    /// Probe status: `"connected"`, `"failed"`, or `"stub"`.
    pub status: String,
    /// Phase that produced this result.
    pub phase: u32,
    /// Organisation URL echoed back from config.
    pub organization_url: String,
    /// Human-readable note about the probe outcome.
    pub message: String,
    /// Authenticated user GUID (Phase 2+, present on success).
    pub user_id: Option<String>,
    /// Authenticated user display name (Phase 2+, present on success).
    pub user_name: Option<String>,
    /// ADO instance GUID (Phase 2+, present on success).
    pub instance_id: Option<String>,
}

/// ADO work item (Phase 5 — batch fetch shape).
///
/// Populated from `POST _apis/wit/workitemsbatch` using the field projection
/// listed in [`AzureDevOpsClient::get_work_items`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoWorkItem {
    /// ADO work-item integer ID (the `N` in `AB#N`).
    pub id: u32,
    /// `System.Title`.
    pub title: String,
    /// `System.State` (e.g. `"Active"`, `"Closed"`).
    pub state: String,
    /// `System.WorkItemType` (e.g. `"Bug"`, `"User Story"`, `"Task"`).
    pub work_item_type: String,
    /// `System.Tags` — split on `; ` and trimmed. Empty if no tags.
    pub tags: Vec<String>,
    /// `System.TeamProject`.
    pub team_project: String,
    /// Self URL from ADO (if present in response).
    pub url: Option<String>,
}

/// Backwards-compatible alias for the original Phase-1 placeholder type.
///
/// `WorkItem` was the stub struct used before Phase 5 batch fetch existed.
/// New code should prefer [`AzdoWorkItem`].
pub type WorkItem = AzdoWorkItem;

/// ADO work item type descriptor (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoWorkItemType {
    /// Display name (e.g. `"User Story"`).
    pub name: String,
    /// Stable reference name (e.g. `"Microsoft.VSTS.WorkItemTypes.UserStory"`).
    pub reference_name: String,
    /// Human-readable description, if provided by ADO.
    pub description: String,
    /// Hex color (e.g. `"009CCC"`), without leading `#`.
    pub color: String,
    /// Icon identifier, if provided by ADO.
    pub icon: String,
}

/// ADO field descriptor (Phase 3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoField {
    /// Display name (e.g. `"Title"`).
    pub name: String,
    /// Stable reference name (e.g. `"System.Title"`).
    pub reference_name: String,
    /// Field type as reported by ADO (e.g. `"string"`, `"integer"`,
    /// `"dateTime"`, `"html"`).
    pub field_type: String,
}

/// Reference to a work item returned by a WIQL query (Phase 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItemRef {
    /// Work item ID.
    pub id: u32,
    /// Self URL.
    pub url: String,
}

/// Result of a WIQL query (Phase 4).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WiqlResult {
    /// `"flat"`, `"oneHop"`, or `"tree"` as reported by ADO.
    pub query_type: String,
    /// Returned work-item references.
    pub work_items: Vec<WorkItemRef>,
}

/// ADO project descriptor (Phase 2 — list-projects shape).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoProject {
    /// ADO project GUID.
    pub id: String,
    /// Project display name.
    pub name: String,
    /// Lifecycle state — `"wellFormed"`, `"createPending"`, `"deleting"`, ...
    pub state: String,
    /// Visibility — `"private"` or `"public"`.
    pub visibility: String,
}

// ---------------------------------------------------------------------------
// Internal response shapes (ADO REST API, partial)
// ---------------------------------------------------------------------------

/// ADO `_apis/connectionData` response (partial — only fields tga uses).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ConnectionDataResponse {
    authenticated_user: AuthenticatedUser,
    instance_id: String,
    #[serde(default)]
    #[allow(dead_code)]
    deployment_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthenticatedUser {
    id: String,
    provider_display_name: String,
}

/// ADO `_apis/projects` response envelope.
#[derive(Debug, Deserialize)]
struct ProjectsResponse {
    #[allow(dead_code)]
    count: u32,
    value: Vec<AzdoProjectRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AzdoProjectRaw {
    id: String,
    name: String,
    state: String,
    visibility: String,
}

/// Generic ADO list envelope: `{ "count": N, "value": [...] }`.
#[derive(Debug, Deserialize)]
struct ListEnvelope<T> {
    #[allow(dead_code)]
    #[serde(default)]
    count: u32,
    value: Vec<T>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemTypeRaw {
    name: String,
    reference_name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    icon: IconRaw,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IconRaw {
    #[serde(default)]
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FieldRaw {
    name: String,
    reference_name: String,
    #[serde(rename = "type", default)]
    field_type: String,
}

/// WIQL response envelope (Phase 4).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WiqlResponseRaw {
    #[serde(default)]
    query_type: String,
    #[serde(default)]
    work_items: Vec<WorkItemRefRaw>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkItemRefRaw {
    id: u32,
    #[serde(default)]
    url: String,
}

/// Work-items-batch response envelope (Phase 5).
#[derive(Debug, Deserialize)]
struct WorkItemBatchResponse {
    #[allow(dead_code)]
    #[serde(default)]
    count: u32,
    value: Vec<WorkItemRaw>,
}

#[derive(Debug, Deserialize)]
struct WorkItemRaw {
    id: u32,
    #[serde(default)]
    fields: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    url: Option<String>,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Azure DevOps client. Holds config + a lazily built `reqwest::Client`.
pub struct AzureDevOpsClient {
    config: AzureDevOpsConfig,
}

/// Percent-encode a single path segment (e.g. an ADO project name).
///
/// Encodes any byte outside the unreserved set
/// (`ALPHA / DIGIT / "-" / "." / "_" / "~"`) as `%HH`. This is conservative
/// but correct: it never produces an invalid URL, even if the project name
/// contains spaces, slashes, or non-ASCII characters.
fn encode_path_segment(s: &str) -> String {
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

/// Build an authenticated [`reqwest::Client`] for ADO API calls.
///
/// * Uses HTTP Basic auth with an empty username and `pat` as the password
///   via reqwest's per-request [`reqwest::RequestBuilder::basic_auth`] — no
///   `base64` dependency required.
/// * Sets a 30-second total request timeout.
/// * Identifies via `User-Agent: tga/{CARGO_PKG_VERSION}`.
fn build_client() -> Result<reqwest::Client, AzdoError> {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::USER_AGENT,
        reqwest::header::HeaderValue::from_static(concat!("tga/", env!("CARGO_PKG_VERSION"))),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );

    reqwest::Client::builder()
        .default_headers(headers)
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(AzdoError::Request)
}

impl AzureDevOpsClient {
    /// Construct a new client. Does not validate or contact ADO.
    pub fn new(config: AzureDevOpsConfig) -> Self {
        Self { config }
    }

    /// Borrow the underlying config.
    pub fn config(&self) -> &AzureDevOpsConfig {
        &self.config
    }

    /// Validate credentials format only — no HTTP probe.
    ///
    /// # Errors
    ///
    /// Returns [`AzdoError::InvalidCredentials`] if the PAT is empty or
    /// whitespace-only.
    pub fn validate_credentials(&self) -> Result<(), AzdoError> {
        if self.config.pat.trim().is_empty() {
            return Err(AzdoError::InvalidCredentials(
                "PAT is empty (a non-empty PAT is required)".into(),
            ));
        }
        Ok(())
    }

    /// Phase 1 stub retained for tests that must not touch the network.
    ///
    /// Phase 2 callers should prefer [`Self::test_connection`].
    pub fn test_connection_stub(&self) -> AzdoConnectionInfo {
        AzdoConnectionInfo {
            status: "stub".to_string(),
            phase: 1,
            organization_url: self.config.organization_url.clone(),
            message: "stub probe — call test_connection() for a real check".to_string(),
            user_id: None,
            user_name: None,
            instance_id: None,
        }
    }

    /// Trim a trailing slash from `organization_url` (if any).
    fn org_url(&self) -> &str {
        self.config.organization_url.trim_end_matches('/')
    }

    /// Test connection by calling `GET _apis/connectionData`.
    ///
    /// Returns [`AzdoConnectionInfo`] with `status = "connected"` on success,
    /// populated with the authenticated user identity and instance GUID.
    ///
    /// # Errors
    ///
    /// * [`AzdoError::InvalidCredentials`] — empty PAT (pre-flight check).
    /// * [`AzdoError::Unauthorized`] — HTTP 401 (invalid PAT).
    /// * [`AzdoError::Forbidden`] — HTTP 403 (PAT lacks scope).
    /// * [`AzdoError::NotFound`] — HTTP 404 (wrong organisation URL).
    /// * [`AzdoError::Http`] — any other non-2xx response.
    /// * [`AzdoError::Request`] — transport failure (network, TLS, timeout).
    /// * [`AzdoError::Parse`] — response body did not match expected shape.
    pub async fn test_connection(&self) -> Result<AzdoConnectionInfo, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/_apis/connectionData?connectOptions=none&api-version=7.1",
            self.org_url()
        );

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        let status = resp.status();
        match status.as_u16() {
            200 => {
                let body: ConnectionDataResponse = resp
                    .json()
                    .await
                    .map_err(|e| AzdoError::Parse(e.to_string()))?;
                Ok(AzdoConnectionInfo {
                    status: "connected".to_string(),
                    phase: 2,
                    organization_url: self.config.organization_url.clone(),
                    message: format!(
                        "authenticated as {} (instance {})",
                        body.authenticated_user.provider_display_name, body.instance_id
                    ),
                    user_id: Some(body.authenticated_user.id),
                    user_name: Some(body.authenticated_user.provider_display_name),
                    instance_id: Some(body.instance_id),
                })
            }
            401 => Err(AzdoError::Unauthorized),
            403 => Err(AzdoError::Forbidden),
            404 => Err(AzdoError::NotFound),
            s => {
                let message = resp.text().await.unwrap_or_default();
                Err(AzdoError::Http { status: s, message })
            }
        }
    }

    /// List ADO projects via `GET _apis/projects`.
    ///
    /// Returns up to 100 projects in a single page. Phase 4 will add
    /// continuation-token pagination.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_projects(&self) -> Result<Vec<AzdoProject>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!("{}/_apis/projects?api-version=7.1&$top=100", self.org_url());

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        let status = resp.status();
        match status.as_u16() {
            200 => {
                let body: ProjectsResponse = resp
                    .json()
                    .await
                    .map_err(|e| AzdoError::Parse(e.to_string()))?;
                let projects = body
                    .value
                    .into_iter()
                    .map(|p| AzdoProject {
                        id: p.id,
                        name: p.name,
                        state: p.state,
                        visibility: p.visibility,
                    })
                    .collect();
                Ok(projects)
            }
            401 => Err(AzdoError::Unauthorized),
            403 => Err(AzdoError::Forbidden),
            404 => Err(AzdoError::NotFound),
            s => {
                let message = resp.text().await.unwrap_or_default();
                Err(AzdoError::Http { status: s, message })
            }
        }
    }

    /// Map an HTTP status to the standard [`AzdoError`] variants used by all
    /// ADO endpoints. 401/403/404 get dedicated variants; everything else
    /// falls back to [`AzdoError::Http`].
    async fn map_status(resp: reqwest::Response) -> AzdoError {
        let status = resp.status().as_u16();
        match status {
            401 => AzdoError::Unauthorized,
            403 => AzdoError::Forbidden,
            404 => AzdoError::NotFound,
            s => {
                let message = resp.text().await.unwrap_or_default();
                AzdoError::Http { status: s, message }
            }
        }
    }

    /// List work-item types available in a project (Phase 3).
    ///
    /// Calls `GET {org}/{project}/_apis/wit/workitemtypes?api-version=7.1`.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_work_item_types(
        &self,
        project: &str,
    ) -> Result<Vec<AzdoWorkItemType>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/workitemtypes?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "GET work item types");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<WorkItemTypeRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|t| AzdoWorkItemType {
                name: t.name,
                reference_name: t.reference_name,
                description: t.description,
                color: t.color,
                icon: t.icon.id,
            })
            .collect())
    }

    /// List fields available in a project (Phase 3).
    ///
    /// Calls `GET {org}/{project}/_apis/wit/fields?api-version=7.1`.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_fields(&self, project: &str) -> Result<Vec<AzdoField>, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/fields?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "GET fields");

        let resp = client
            .get(&url)
            .basic_auth("", Some(&self.config.pat))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: ListEnvelope<FieldRaw> = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(body
            .value
            .into_iter()
            .map(|f| AzdoField {
                name: f.name,
                reference_name: f.reference_name,
                field_type: f.field_type,
            })
            .collect())
    }

    /// Run a WIQL query against a project (Phase 4).
    ///
    /// Calls `POST {org}/{project}/_apis/wit/wiql?api-version=7.1` with the
    /// body `{ "query": <query> }`. Returns the list of matching work-item
    /// references (IDs + self URLs).
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn run_wiql(&self, project: &str, query: &str) -> Result<WiqlResult, AzdoError> {
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/{}/_apis/wit/wiql?api-version=7.1",
            self.org_url(),
            encode_path_segment(project),
        );
        tracing::debug!(url = %url, "POST wiql");

        let resp = client
            .post(&url)
            .basic_auth("", Some(&self.config.pat))
            .json(&serde_json::json!({ "query": query }))
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(Self::map_status(resp).await);
        }

        let body: WiqlResponseRaw = resp
            .json()
            .await
            .map_err(|e| AzdoError::Parse(e.to_string()))?;

        Ok(WiqlResult {
            query_type: body.query_type,
            work_items: body
                .work_items
                .into_iter()
                .map(|w| WorkItemRef {
                    id: w.id,
                    url: w.url,
                })
                .collect(),
        })
    }

    /// Convenience helper: returns the IDs of work items modified in the last
    /// `since_days` days, ordered by `[System.ChangedDate] DESC` (Phase 4).
    ///
    /// # Errors
    ///
    /// Same set as [`Self::run_wiql`].
    pub async fn get_recent_work_item_ids(
        &self,
        project: &str,
        since_days: u32,
    ) -> Result<Vec<u32>, AzdoError> {
        let query = format!(
            "SELECT [System.Id] FROM WorkItems \
             WHERE [System.TeamProject] = @project \
             AND [System.ChangedDate] >= @today - {since_days} \
             ORDER BY [System.ChangedDate] DESC"
        );
        let result = self.run_wiql(project, &query).await?;
        Ok(result.work_items.into_iter().map(|w| w.id).collect())
    }

    /// Fetch work items by ID via the batch endpoint (Phase 5).
    ///
    /// Calls `POST {org}/_apis/wit/workitemsbatch?api-version=7.1`. IDs are
    /// chunked in groups of 200 (the ADO server-side limit). The projected
    /// field set is:
    ///
    /// * `System.Id`
    /// * `System.Title`
    /// * `System.State`
    /// * `System.WorkItemType`
    /// * `System.Tags`
    /// * `System.TeamProject`
    ///
    /// Returns work items in the order ADO returns them — callers that need
    /// input-order alignment should re-sort by ID.
    ///
    /// An empty `ids` slice short-circuits to `Ok(vec![])` without any HTTP
    /// traffic.
    ///
    /// # Errors
    ///
    /// Same set as [`Self::test_connection`].
    pub async fn get_work_items(&self, ids: &[u32]) -> Result<Vec<AzdoWorkItem>, AzdoError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.validate_credentials()?;

        let client = build_client()?;
        let url = format!(
            "{}/_apis/wit/workitemsbatch?api-version=7.1",
            self.org_url()
        );

        let fields = [
            "System.Id",
            "System.Title",
            "System.State",
            "System.WorkItemType",
            "System.Tags",
            "System.TeamProject",
        ];

        let mut all = Vec::with_capacity(ids.len());

        for chunk in ids.chunks(200) {
            tracing::debug!(url = %url, count = chunk.len(), "POST workitemsbatch");
            let resp = client
                .post(&url)
                .basic_auth("", Some(&self.config.pat))
                .json(&serde_json::json!({
                    "ids": chunk,
                    "fields": fields,
                }))
                .send()
                .await?;

            if !resp.status().is_success() {
                return Err(Self::map_status(resp).await);
            }

            let body: WorkItemBatchResponse = resp
                .json()
                .await
                .map_err(|e| AzdoError::Parse(e.to_string()))?;

            for w in body.value {
                all.push(parse_work_item(w));
            }
        }

        Ok(all)
    }
}

/// Project a raw ADO work item (with arbitrary fields map) into our
/// flat [`AzdoWorkItem`] shape. Missing fields default to empty strings.
fn parse_work_item(raw: WorkItemRaw) -> AzdoWorkItem {
    let get_str = |key: &str| -> String {
        raw.fields
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let tags_raw = get_str("System.Tags");
    let tags = if tags_raw.is_empty() {
        Vec::new()
    } else {
        tags_raw
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };

    AzdoWorkItem {
        id: raw.id,
        title: get_str("System.Title"),
        state: get_str("System.State"),
        work_item_type: get_str("System.WorkItemType"),
        tags,
        team_project: get_str("System.TeamProject"),
        url: raw.url,
    }
}

/// Extract Azure DevOps work-item IDs from arbitrary text.
///
/// Matches `AB#\d+` case-insensitively (so `ab#42`, `Ab#42`, `AB#42` all
/// resolve to `42`). IDs are deduplicated in first-seen order.
pub fn extract_work_item_refs(text: &str) -> Vec<u32> {
    use std::collections::HashSet;
    use std::sync::OnceLock;
    static RE: OnceLock<regex::Regex> = OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"(?i)\bAB#(\d+)\b").expect("AB# regex compiles"));
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(text) {
        if let Some(m) = cap.get(1) {
            if let Ok(id) = m.as_str().parse::<u32>() {
                if seen.insert(id) {
                    out.push(id);
                }
            }
        }
    }
    out
}

/// Scan a list of commit messages (or other text) for `AB#N` references and
/// fetch the referenced work items in a single batch call (Phase 6).
///
/// IDs are deduplicated across all messages. `project` is currently unused —
/// the batch endpoint is organisation-scoped — but is retained in the API for
/// future per-project filtering and for symmetry with the other methods.
///
/// Returns an empty vector if no `AB#N` references are found.
///
/// # Errors
///
/// Same set as [`AzureDevOpsClient::get_work_items`].
pub async fn fetch_referenced_work_items(
    client: &AzureDevOpsClient,
    messages: &[&str],
    _project: &str,
) -> Result<Vec<AzdoWorkItem>, AzdoError> {
    use std::collections::HashSet;
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for msg in messages {
        for id in extract_work_item_refs(msg) {
            if seen.insert(id) {
                ids.push(id);
            }
        }
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    client.get_work_items(&ids).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_config_for(server_url: &str) -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: server_url.to_string(),
            pat: "secret-pat".into(),
            project: "MyProject".into(),
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
        }
    }

    fn sample_config() -> AzureDevOpsConfig {
        AzureDevOpsConfig {
            organization_url: "https://dev.azure.com/myorg".into(),
            pat: "secret-pat".into(),
            project: "MyProject".into(),
            ticket_regex: r"AB#(\d+)".into(),
            team_keys: vec![],
            fetch_on_reference: true,
        }
    }

    // ----- Phase 1 carry-over tests -----

    #[test]
    fn stub_connection_info_has_phase_1() {
        let client = AzureDevOpsClient::new(sample_config());
        let info = client.test_connection_stub();
        assert_eq!(info.phase, 1);
        assert_eq!(info.status, "stub");
        assert_eq!(info.organization_url, "https://dev.azure.com/myorg");
    }

    #[test]
    fn validate_credentials_accepts_non_empty_pat() {
        let client = AzureDevOpsClient::new(sample_config());
        client
            .validate_credentials()
            .expect("non-empty PAT should validate");
    }

    #[test]
    fn validate_credentials_rejects_empty_pat() {
        let mut cfg = sample_config();
        cfg.pat = "   ".into();
        let client = AzureDevOpsClient::new(cfg);
        let err = client
            .validate_credentials()
            .expect_err("whitespace PAT should be rejected");
        assert!(matches!(err, AzdoError::InvalidCredentials(_)));
    }

    #[tokio::test]
    async fn get_work_items_empty_ids_short_circuits() {
        // No HTTP server — verifies we don't hit the network for empty input.
        let client = AzureDevOpsClient::new(sample_config());
        let out = client
            .get_work_items(&[])
            .await
            .expect("empty ids should short-circuit to Ok(vec![])");
        assert!(out.is_empty());
    }

    // ----- Phase 2: HTTP tests via wiremock -----

    /// Expected Basic-auth header for `":secret-pat"` (empty user + PAT).
    /// `:secret-pat` → base64 → `OnNlY3JldC1wYXQ=`
    const EXPECTED_AUTH: &str = "Basic OnNlY3JldC1wYXQ=";

    #[tokio::test]
    async fn test_connection_succeeds_on_200() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "authenticatedUser": {
                "id": "11111111-1111-1111-1111-111111111111",
                "providerDisplayName": "John Doe",
                "subjectDescriptor": "aad.xxx"
            },
            "instanceId": "22222222-2222-2222-2222-222222222222",
            "deploymentType": "hosted"
        });

        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .and(query_param("api-version", "7.1"))
            .and(query_param("connectOptions", "none"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let info = client
            .test_connection()
            .await
            .expect("200 should yield connected info");
        assert_eq!(info.status, "connected");
        assert_eq!(info.phase, 2);
        assert_eq!(info.user_name.as_deref(), Some("John Doe"));
        assert_eq!(
            info.user_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );
        assert_eq!(
            info.instance_id.as_deref(),
            Some("22222222-2222-2222-2222-222222222222")
        );
    }

    #[tokio::test]
    async fn test_connection_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_forbidden_on_403() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("403 should err");
        assert!(matches!(err, AzdoError::Forbidden), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_not_found_on_404() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("404 should err");
        assert!(matches!(err, AzdoError::NotFound), "got {err:?}");
    }

    #[tokio::test]
    async fn test_connection_returns_http_on_500() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/connectionData"))
            .respond_with(ResponseTemplate::new(500).set_body_string("upstream boom"))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.test_connection().await.expect_err("500 should err");
        match err {
            AzdoError::Http { status, message } => {
                assert_eq!(status, 500);
                assert!(message.contains("upstream boom"), "msg: {message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_connection_rejects_empty_pat_pre_flight() {
        let mut cfg = sample_config();
        cfg.pat = "   ".into();
        let client = AzureDevOpsClient::new(cfg);
        let err = client
            .test_connection()
            .await
            .expect_err("empty PAT short-circuits before HTTP");
        assert!(matches!(err, AzdoError::InvalidCredentials(_)));
    }

    #[tokio::test]
    async fn get_projects_returns_list_on_200() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    "name": "MyProject",
                    "state": "wellFormed",
                    "visibility": "private",
                    "lastUpdateTime": "2025-01-01T00:00:00Z"
                },
                {
                    "id": "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
                    "name": "OtherProject",
                    "state": "wellFormed",
                    "visibility": "public",
                    "lastUpdateTime": "2025-01-02T00:00:00Z"
                }
            ]
        });

        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .and(query_param("api-version", "7.1"))
            .and(query_param("$top", "100"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let projects = client.get_projects().await.expect("200 should yield list");
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "MyProject");
        assert_eq!(projects[0].state, "wellFormed");
        assert_eq!(projects[0].visibility, "private");
        assert_eq!(projects[1].name, "OtherProject");
        assert_eq!(projects[1].visibility, "public");
    }

    #[tokio::test]
    async fn get_projects_returns_empty_on_zero_count() {
        let server = MockServer::start().await;

        let body = serde_json::json!({
            "count": 0,
            "value": []
        });

        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let projects = client.get_projects().await.expect("200 empty list ok");
        assert!(projects.is_empty());
    }

    #[tokio::test]
    async fn get_projects_returns_unauthorized_on_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.get_projects().await.expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized), "got {err:?}");
    }

    // ----- Phase 3: work item types & fields -----

    #[tokio::test]
    async fn get_work_item_types_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "name": "Bug",
                    "referenceName": "Microsoft.VSTS.WorkItemTypes.Bug",
                    "description": "Tracks a defect",
                    "color": "CC293D",
                    "icon": { "id": "icon_insect", "url": "https://x" }
                },
                {
                    "name": "User Story",
                    "referenceName": "Microsoft.VSTS.WorkItemTypes.UserStory",
                    "description": "",
                    "color": "009CCC",
                    "icon": { "id": "icon_book" }
                }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workitemtypes"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let types = client
            .get_work_item_types("MyProject")
            .await
            .expect("200 ok");
        assert_eq!(types.len(), 2);
        assert_eq!(types[0].name, "Bug");
        assert_eq!(types[0].reference_name, "Microsoft.VSTS.WorkItemTypes.Bug");
        assert_eq!(types[0].color, "CC293D");
        assert_eq!(types[0].icon, "icon_insect");
        assert_eq!(types[1].name, "User Story");
        assert_eq!(types[1].icon, "icon_book");
    }

    #[tokio::test]
    async fn get_work_item_types_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/workitemtypes"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .get_work_item_types("MyProject")
            .await
            .expect_err("401 should err");
        assert!(matches!(err, AzdoError::Unauthorized));
    }

    #[tokio::test]
    async fn get_fields_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                { "name": "Title", "referenceName": "System.Title", "type": "string" },
                { "name": "State", "referenceName": "System.State", "type": "string" }
            ]
        });
        Mock::given(method("GET"))
            .and(path("/MyProject/_apis/wit/fields"))
            .and(query_param("api-version", "7.1"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let fields = client.get_fields("MyProject").await.expect("200 ok");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].reference_name, "System.Title");
        assert_eq!(fields[0].field_type, "string");
    }

    #[tokio::test]
    async fn get_fields_encodes_project_with_spaces() {
        let server = MockServer::start().await;
        // "My Project" → "My%20Project"
        Mock::given(method("GET"))
            .and(path("/My%20Project/_apis/wit/fields"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let fields = client
            .get_fields("My Project")
            .await
            .expect("space in project name should encode");
        assert!(fields.is_empty());
    }

    // ----- Phase 4: WIQL -----

    #[tokio::test]
    async fn run_wiql_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "queryType": "flat",
            "queryResultType": "workItem",
            "workItems": [
                { "id": 42, "url": "https://x/42" },
                { "id": 43, "url": "https://x/43" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let result = client
            .run_wiql("MyProject", "SELECT [System.Id] FROM WorkItems")
            .await
            .expect("200 ok");
        assert_eq!(result.query_type, "flat");
        assert_eq!(result.work_items.len(), 2);
        assert_eq!(result.work_items[0].id, 42);
        assert_eq!(result.work_items[0].url, "https://x/42");
    }

    #[tokio::test]
    async fn get_recent_work_item_ids_returns_ids() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "queryType": "flat",
            "workItems": [
                { "id": 1, "url": "" },
                { "id": 2, "url": "" }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let ids = client
            .get_recent_work_item_ids("MyProject", 30)
            .await
            .expect("200 ok");
        assert_eq!(ids, vec![1, 2]);
    }

    #[tokio::test]
    async fn run_wiql_maps_401() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/MyProject/_apis/wit/wiql"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client
            .run_wiql("MyProject", "SELECT [System.Id] FROM WorkItems")
            .await
            .expect_err("401");
        assert!(matches!(err, AzdoError::Unauthorized));
    }

    // ----- Phase 5: work items batch -----

    #[tokio::test]
    async fn get_work_items_batch_parses_response() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": 42,
                    "url": "https://x/42",
                    "fields": {
                        "System.Id": 42,
                        "System.Title": "Fix login bug",
                        "System.State": "Active",
                        "System.WorkItemType": "Bug",
                        "System.Tags": "frontend; urgent ; ",
                        "System.TeamProject": "MyProject"
                    }
                },
                {
                    "id": 43,
                    "fields": {
                        "System.Id": 43,
                        "System.Title": "New feature",
                        "System.State": "New",
                        "System.WorkItemType": "User Story",
                        "System.TeamProject": "MyProject"
                    }
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .and(query_param("api-version", "7.1"))
            .and(header("authorization", EXPECTED_AUTH))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let items = client.get_work_items(&[42, 43]).await.expect("200 ok");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].id, 42);
        assert_eq!(items[0].title, "Fix login bug");
        assert_eq!(items[0].state, "Active");
        assert_eq!(items[0].work_item_type, "Bug");
        assert_eq!(items[0].tags, vec!["frontend", "urgent"]);
        assert_eq!(items[0].team_project, "MyProject");
        assert_eq!(items[0].url.as_deref(), Some("https://x/42"));
        // Missing tags → empty vec.
        assert!(items[1].tags.is_empty());
        assert!(items[1].url.is_none());
    }

    #[tokio::test]
    async fn get_work_items_chunks_in_batches_of_200() {
        let server = MockServer::start().await;
        // Mount a permissive responder; we'll assert call count via wiremock.
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .expect(2) // 250 ids = 2 chunks (200 + 50)
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let ids: Vec<u32> = (1..=250).collect();
        let items = client.get_work_items(&ids).await.expect("200 ok");
        assert!(items.is_empty());
        // Drop() on server verifies `.expect(2)`.
        drop(server);
    }

    #[tokio::test]
    async fn get_work_items_maps_403() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let err = client.get_work_items(&[1]).await.expect_err("403");
        assert!(matches!(err, AzdoError::Forbidden));
    }

    // ----- Phase 6: AB# reference detection -----

    #[test]
    fn extract_work_item_refs_finds_ids() {
        let out = extract_work_item_refs("Fixes AB#42 and AB#100 and AB#42 again");
        assert_eq!(out, vec![42, 100]);
    }

    #[test]
    fn extract_work_item_refs_is_case_insensitive() {
        let out = extract_work_item_refs("see ab#7 and Ab#8 and AB#9");
        assert_eq!(out, vec![7, 8, 9]);
    }

    #[test]
    fn extract_work_item_refs_returns_empty_when_no_match() {
        let out = extract_work_item_refs("nothing to see here #42 PROJ-1");
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn fetch_referenced_work_items_aggregates_from_messages() {
        let server = MockServer::start().await;
        let body = serde_json::json!({
            "count": 2,
            "value": [
                {
                    "id": 7,
                    "fields": {
                        "System.Id": 7,
                        "System.Title": "Seven",
                        "System.State": "Active",
                        "System.WorkItemType": "Task",
                        "System.TeamProject": "MyProject"
                    }
                },
                {
                    "id": 9,
                    "fields": {
                        "System.Id": 9,
                        "System.Title": "Nine",
                        "System.State": "Closed",
                        "System.WorkItemType": "Bug",
                        "System.TeamProject": "MyProject"
                    }
                }
            ]
        });
        Mock::given(method("POST"))
            .and(path("/_apis/wit/workitemsbatch"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let client = AzureDevOpsClient::new(sample_config_for(&server.uri()));
        let msgs = ["fix AB#7", "AB#7 again and AB#9"];
        let items = fetch_referenced_work_items(&client, &msgs, "MyProject")
            .await
            .expect("batch ok");
        assert_eq!(items.len(), 2);
        let ids: Vec<u32> = items.iter().map(|w| w.id).collect();
        assert!(ids.contains(&7));
        assert!(ids.contains(&9));
    }

    #[tokio::test]
    async fn fetch_referenced_work_items_empty_when_no_refs() {
        let client = AzureDevOpsClient::new(sample_config());
        let msgs = ["nothing here", "PROJ-1 only"];
        let items = fetch_referenced_work_items(&client, &msgs, "MyProject")
            .await
            .expect("no HTTP should be triggered");
        assert!(items.is_empty());
    }

    // ----- Path encoding helper -----

    #[test]
    fn encode_path_segment_passes_through_unreserved() {
        assert_eq!(
            encode_path_segment("MyProject_1.2-3~x"),
            "MyProject_1.2-3~x"
        );
    }

    #[test]
    fn encode_path_segment_encodes_space() {
        assert_eq!(encode_path_segment("My Project"), "My%20Project");
    }

    #[test]
    fn encode_path_segment_encodes_slash() {
        assert_eq!(encode_path_segment("a/b"), "a%2Fb");
    }

    #[tokio::test]
    async fn org_url_trailing_slash_is_trimmed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/_apis/projects"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "count": 0,
                "value": []
            })))
            .mount(&server)
            .await;

        // Append a trailing slash to the org URL.
        let mut cfg = sample_config_for(&server.uri());
        cfg.organization_url.push('/');
        let client = AzureDevOpsClient::new(cfg);
        let projects = client
            .get_projects()
            .await
            .expect("trailing slash should be tolerated");
        assert!(projects.is_empty());
    }
}

//! Azure DevOps client — Phase 2.
//!
//! Phase 2 implements an authenticated HTTP session against the Azure DevOps
//! REST API (`api-version=7.1`). Two endpoints are wired up:
//!
//! * [`AzureDevOpsClient::test_connection`] — `GET _apis/connectionData`
//!   (auth probe + identity echo).
//! * [`AzureDevOpsClient::get_projects`]    — `GET _apis/projects`
//!   (project list, capped at 100; pagination is deferred to Phase 4).
//!
//! Authentication uses HTTP Basic with an empty username and the PAT as the
//! password — the standard ADO convention.
//!
//! Phase 6 will add work-item fetching on top of this session.

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

/// Placeholder work-item type. Filled in for Phase 6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    /// ADO work-item integer ID (the `N` in `AB#N`).
    pub id: u32,
    /// Title of the work item.
    pub title: String,
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

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Azure DevOps client. Holds config + a lazily built `reqwest::Client`.
pub struct AzureDevOpsClient {
    config: AzureDevOpsConfig,
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

    /// Fetch work items by ID. **NOT IMPLEMENTED** — Phase 6.
    ///
    /// # Errors
    ///
    /// Always returns [`AzdoError::NotImplemented`] with `phase = 6`.
    pub async fn get_work_items(&self, _ids: &[u32]) -> Result<Vec<WorkItem>, AzdoError> {
        Err(AzdoError::NotImplemented {
            method: "get_work_items".to_string(),
            phase: 6,
        })
    }
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
    async fn get_work_items_returns_not_implemented() {
        let client = AzureDevOpsClient::new(sample_config());
        let err = client
            .get_work_items(&[1, 2, 3])
            .await
            .expect_err("Phase 2 still does not implement get_work_items");
        match err {
            AzdoError::NotImplemented { method, phase } => {
                assert_eq!(method, "get_work_items");
                assert_eq!(phase, 6);
            }
            other => panic!("unexpected error: {other:?}"),
        }
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

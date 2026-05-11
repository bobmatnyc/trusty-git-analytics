//! Azure DevOps stub client — Phase 1.
//!
//! Phase 1 is intentionally inert at runtime. No HTTP calls are made.
//! All data methods return [`AzdoError::NotImplemented`] with the phase
//! in which the method will be implemented.
//!
//! Phase 2 will add the HTTP session and `GET _apis/connectionData`
//! auth probe; Phase 6 will add work-item fetching.

use serde::{Deserialize, Serialize};

use crate::core::config::AzureDevOpsConfig;

/// Errors returned by the Azure DevOps client.
///
/// In Phase 1 only `NotImplemented` and `InvalidCredentials` are produced;
/// `InvalidUrl` exists for symmetry with Phase 2 URL parsing.
#[derive(Debug, thiserror::Error)]
pub enum AzdoError {
    /// The method is not yet implemented. `phase` indicates the planned
    /// phase number (e.g. 2 for connection probe, 6 for work items).
    #[error("not implemented: {method} is planned for Phase {phase}")]
    NotImplemented {
        /// Name of the method that would have performed work.
        method: String,
        /// Phase number in which this method will be implemented.
        phase: u32,
    },

    /// Credentials were rejected at the format-validation stage
    /// (Phase 1 does not make an HTTP probe).
    #[error("invalid credentials: {0}")]
    InvalidCredentials(String),

    /// The configured URL is malformed or not an Azure DevOps URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
}

/// Stub connection probe result. Phase 2 will replace `status = "stub"`
/// with the result of `GET _apis/connectionData`.
#[derive(Debug, Serialize)]
pub struct AzdoConnectionInfo {
    /// Either `"stub"` (Phase 1) or `"connected"` / `"failed"` in Phase 2+.
    pub status: String,
    /// Phase that produced this result. Always `1` in this implementation.
    pub phase: u32,
    /// Organisation URL echoed back from config.
    pub organization_url: String,
    /// Human-readable note describing what a real probe would do.
    pub message: String,
}

/// Placeholder work-item type. Filled in for Phase 6.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkItem {
    /// ADO work-item integer ID (the `N` in `AB#N`).
    pub id: u32,
    /// Title of the work item.
    pub title: String,
}

/// Placeholder ADO project descriptor. Filled in for Phase 2 (`GET _apis/projects`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AzdoProject {
    /// ADO project GUID.
    pub id: String,
    /// Project display name.
    pub name: String,
}

/// Azure DevOps stub client. Holds config; performs no network I/O in Phase 1.
pub struct AzureDevOpsClient {
    config: AzureDevOpsConfig,
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

    /// Validate credentials format only — no HTTP probe in Phase 1.
    ///
    /// Phase 2 will additionally call `GET _apis/connectionData` to confirm
    /// the PAT is authorized for the configured organisation.
    ///
    /// # Errors
    ///
    /// Returns [`AzdoError::InvalidCredentials`] if the PAT is empty or
    /// whitespace-only.
    pub fn validate_credentials(&self) -> Result<(), AzdoError> {
        if self.config.pat.trim().is_empty() {
            return Err(AzdoError::InvalidCredentials(
                "PAT is empty (Phase 1 requires a non-empty PAT)".into(),
            ));
        }
        Ok(())
    }

    /// Stub connection test. Phase 2 will replace this with a real
    /// `GET _apis/connectionData` call.
    pub fn test_connection_stub(&self) -> AzdoConnectionInfo {
        AzdoConnectionInfo {
            status: "stub".to_string(),
            phase: 1,
            organization_url: self.config.organization_url.clone(),
            message: "Phase 2 will verify against _apis/connectionData".to_string(),
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

    /// List ADO projects. **NOT IMPLEMENTED** — Phase 2.
    ///
    /// # Errors
    ///
    /// Always returns [`AzdoError::NotImplemented`] with `phase = 2`.
    pub async fn get_projects(&self) -> Result<Vec<AzdoProject>, AzdoError> {
        Err(AzdoError::NotImplemented {
            method: "get_projects".to_string(),
            phase: 2,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn stub_connection_info_has_phase_1() {
        let client = AzureDevOpsClient::new(sample_config());
        let info = client.test_connection_stub();
        assert_eq!(info.phase, 1);
        assert_eq!(info.status, "stub");
        assert_eq!(info.organization_url, "https://dev.azure.com/myorg");
        assert!(info.message.contains("_apis/connectionData"));
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
            .expect_err("Phase 1 should not implement get_work_items");
        match err {
            AzdoError::NotImplemented { method, phase } => {
                assert_eq!(method, "get_work_items");
                assert_eq!(phase, 6);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[tokio::test]
    async fn get_projects_returns_not_implemented() {
        let client = AzureDevOpsClient::new(sample_config());
        let err = client
            .get_projects()
            .await
            .expect_err("Phase 1 should not implement get_projects");
        match err {
            AzdoError::NotImplemented { method, phase } => {
                assert_eq!(method, "get_projects");
                assert_eq!(phase, 2);
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }
}

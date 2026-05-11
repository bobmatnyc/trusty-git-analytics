//! Minimal JIRA REST client for fetching individual issues.

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use tga_core::config::JiraConfig;
use tracing::debug;

use crate::errors::{CollectError, Result};

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Async JIRA Cloud / Server client.
pub struct JiraClient {
    client: reqwest::Client,
    base_url: String,
    /// `(username, token)` for HTTP Basic Auth.
    credentials: Option<(String, String)>,
    /// Default project key for filtered queries.
    project_key: String,
}

/// Subset of fields extracted from a JIRA issue payload.
#[derive(Debug, Clone)]
pub struct JiraIssue {
    /// Issue key, e.g. `PROJ-123`.
    pub key: String,
    /// Short summary / title.
    pub summary: String,
    /// Current status name, e.g. `Done`.
    pub status: String,
    /// Issue type, e.g. `Bug`, `Story`, `Task`.
    pub issue_type: String,
}

#[derive(Debug, Deserialize)]
struct ApiIssue {
    key: String,
    fields: ApiFields,
}

#[derive(Debug, Deserialize)]
struct ApiFields {
    #[serde(default)]
    summary: String,
    status: ApiNamed,
    #[serde(rename = "issuetype")]
    issue_type: ApiNamed,
}

#[derive(Debug, Deserialize)]
struct ApiNamed {
    name: String,
}

impl JiraClient {
    /// Construct a client from a [`JiraConfig`].
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `url` is missing.
    /// - [`CollectError::Http`] if the underlying client cannot be built.
    pub fn new(config: &JiraConfig) -> Result<Self> {
        let base = config
            .url
            .as_ref()
            .ok_or_else(|| CollectError::Config("jira.url is required".into()))?
            .trim_end_matches('/')
            .to_string();

        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(USER_AGENT_VALUE));
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .timeout(std::time::Duration::from_secs(30))
            .build()?;

        let credentials = match (&config.username, &config.token) {
            (Some(u), Some(t)) => Some((u.clone(), t.clone())),
            _ => None,
        };

        Ok(Self {
            client,
            base_url: base,
            credentials,
            project_key: config.project_key.clone().unwrap_or_default(),
        })
    }

    /// Fetch a single issue by its key, returning `None` on 404.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport / non-404 status errors,
    /// or [`CollectError::Json`] on payload parse failure.
    pub async fn fetch_issue(&self, key: &str) -> Result<Option<JiraIssue>> {
        let url = format!("{}/rest/api/3/issue/{}", self.base_url, key);
        debug!(url = %url, "GET");
        let mut req = self.client.get(&url);
        if let Some((user, token)) = &self.credentials {
            req = req.basic_auth(user, Some(token));
        }
        let resp = req.send().await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let resp = resp.error_for_status()?;
        let issue: ApiIssue = resp.json().await?;
        Ok(Some(JiraIssue {
            key: issue.key,
            summary: issue.fields.summary,
            status: issue.fields.status.name,
            issue_type: issue.fields.issue_type.name,
        }))
    }

    /// Default project key supplied at construction.
    pub fn project_key(&self) -> &str {
        &self.project_key
    }
}

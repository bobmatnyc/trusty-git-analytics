//! Linear GraphQL API client for issue enrichment.
//!
//! Uses the Linear GraphQL API (<https://api.linear.app/graphql>).
//! Authentication: `Authorization: <api_key>` header (no "Bearer" prefix).
//!
//! Issue identifiers are matched against commit messages with the pattern
//! `[A-Z][A-Z0-9]{0,9}-\d+` (e.g. `ENG-123`, `FE-456`).

use std::collections::HashSet;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::collect::errors::{CollectError, Result};
use crate::core::config::LinearConfig;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Linear GraphQL endpoint.
const LINEAR_GRAPHQL_URL: &str = "https://api.linear.app/graphql";

/// A Linear issue fetched from the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinearIssue {
    /// Linear issue ID (e.g. "ENG-123").
    pub identifier: String,
    /// Issue title.
    pub title: String,
    /// Current state name (e.g. "In Progress", "Done").
    pub state: String,
    /// Team name.
    pub team: String,
    /// Assignee display name (if any).
    pub assignee: Option<String>,
    /// Issue priority (0=none, 1=urgent, 2=high, 3=medium, 4=low).
    pub priority: u8,
    /// URL to the issue in Linear.
    pub url: String,
}

/// Async Linear GraphQL client.
#[derive(Debug)]
pub struct LinearClient {
    client: Client,
    api_key: String,
}

impl LinearClient {
    /// Create a new Linear client from config.
    ///
    /// Resolves `${LINEAR_API_KEY}` env var substitution in the `api_key` field.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Config`] if `api_key` is missing or resolves to empty.
    /// - [`CollectError::Http`] if the HTTP client cannot be built.
    pub fn new(config: &LinearConfig) -> Result<Self> {
        let raw_key = config.api_key.as_deref().unwrap_or("");
        let api_key = expand_env_var(raw_key);
        if api_key.is_empty() {
            return Err(CollectError::Config("Linear api_key is required".into()));
        }
        let client = Client::builder()
            .user_agent(USER_AGENT_VALUE)
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(CollectError::Http)?;
        Ok(Self { client, api_key })
    }

    /// Fetch a single Linear issue by identifier (e.g. "ENG-123").
    ///
    /// Returns `Ok(None)` if the issue is not found or access is denied.
    ///
    /// # Errors
    ///
    /// Returns [`CollectError::Http`] on transport-level failures.
    pub async fn fetch_issue(&self, identifier: &str) -> Result<Option<LinearIssue>> {
        let query = format!(
            r#"query {{
                issue(id: "{identifier}") {{
                    identifier
                    title
                    state {{ name }}
                    team {{ name }}
                    assignee {{ displayName }}
                    priority
                    url
                }}
            }}"#
        );

        let body = serde_json::json!({ "query": query });

        let resp = self
            .client
            .post(LINEAR_GRAPHQL_URL)
            .header("Authorization", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(CollectError::Http)?;

        if !resp.status().is_success() {
            tracing::warn!(
                status = %resp.status(),
                identifier = %identifier,
                "Linear API non-success"
            );
            return Ok(None);
        }

        let json: serde_json::Value = resp.json().await.map_err(CollectError::Http)?;

        // GraphQL errors are returned with 200 OK; check for errors array.
        if let Some(errors) = json.get("errors").and_then(|v| v.as_array()) {
            if !errors.is_empty() {
                tracing::warn!(
                    identifier = %identifier,
                    errors = ?errors,
                    "Linear GraphQL errors"
                );
                return Ok(None);
            }
        }

        let issue_val = &json["data"]["issue"];
        if issue_val.is_null() {
            return Ok(None);
        }

        Ok(Some(LinearIssue {
            identifier: issue_val["identifier"]
                .as_str()
                .unwrap_or(identifier)
                .to_string(),
            title: issue_val["title"].as_str().unwrap_or("").to_string(),
            state: issue_val["state"]["name"]
                .as_str()
                .unwrap_or("Unknown")
                .to_string(),
            team: issue_val["team"]["name"]
                .as_str()
                .unwrap_or("Unknown")
                .to_string(),
            assignee: issue_val["assignee"]["displayName"]
                .as_str()
                .map(String::from),
            priority: issue_val["priority"].as_u64().unwrap_or(0) as u8,
            url: issue_val["url"].as_str().unwrap_or("").to_string(),
        }))
    }

    /// Extract Linear issue identifiers from a commit message.
    ///
    /// Matches patterns like `ENG-123`, `FE-456`, `PROJ-789`.
    /// Returns a deduplicated list of identifiers found (order preserved).
    pub fn extract_issue_ids(message: &str) -> Vec<String> {
        let re = regex::Regex::new(r"\b([A-Z][A-Z0-9]{0,9}-\d+)\b").expect("valid regex");
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for cap in re.captures_iter(message) {
            let id = cap[1].to_string();
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        out
    }

    /// Fetch all issues referenced in the given commit messages.
    ///
    /// Deduplicates issue IDs across messages. Non-fatal: issues that fail
    /// to fetch are logged as warnings and skipped.
    ///
    /// If `team_filter` is non-empty, only issues whose team key (the prefix
    /// before the `-`) matches one of the provided keys (case-insensitive) are
    /// fetched.
    pub async fn fetch_referenced_issues(
        &self,
        messages: &[&str],
        team_filter: &[String],
    ) -> Vec<LinearIssue> {
        let mut seen = HashSet::new();
        let mut all_ids: Vec<String> = Vec::new();
        for msg in messages {
            for id in Self::extract_issue_ids(msg) {
                if seen.insert(id.clone()) {
                    all_ids.push(id);
                }
            }
        }

        let ids: Vec<String> = if team_filter.is_empty() {
            all_ids
        } else {
            all_ids
                .into_iter()
                .filter(|id| {
                    let team_key = id.split('-').next().unwrap_or("");
                    team_filter.iter().any(|t| t.eq_ignore_ascii_case(team_key))
                })
                .collect()
        };

        let mut issues = Vec::new();
        for id in &ids {
            match self.fetch_issue(id).await {
                Ok(Some(issue)) => issues.push(issue),
                Ok(None) => tracing::debug!("Linear issue not found: {id}"),
                Err(e) => tracing::warn!("Failed to fetch Linear issue {id}: {e}"),
            }
        }
        issues
    }
}

/// Resolve a `${VAR_NAME}` placeholder against the process environment.
///
/// If `raw` has the form `${NAME}`, returns the value of `NAME` (empty string
/// if unset). Otherwise returns `raw` unchanged.
fn expand_env_var(raw: &str) -> String {
    if raw.starts_with("${") && raw.ends_with('}') && raw.len() > 3 {
        let var = &raw[2..raw.len() - 1];
        std::env::var(var).unwrap_or_default()
    } else {
        raw.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_issue_ids_finds_linear_patterns() {
        let msg = "ENG-123: add login feature, also fixes FE-456";
        let ids = LinearClient::extract_issue_ids(msg);
        assert!(ids.contains(&"ENG-123".to_string()));
        assert!(ids.contains(&"FE-456".to_string()));
    }

    #[test]
    fn extract_issue_ids_deduplicates() {
        let msg = "ENG-123 ENG-123 duplicate";
        let ids = LinearClient::extract_issue_ids(msg);
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0], "ENG-123");
    }

    #[test]
    fn extract_issue_ids_ignores_lowercase_prefix() {
        let msg = "abc-123 should not match";
        let ids = LinearClient::extract_issue_ids(msg);
        assert!(ids.is_empty());
    }

    #[test]
    fn expand_env_var_returns_literal_when_no_placeholder() {
        assert_eq!(expand_env_var("lin_api_xyz"), "lin_api_xyz");
    }

    #[test]
    fn new_rejects_missing_api_key() {
        let cfg = LinearConfig::default();
        let err = LinearClient::new(&cfg).expect_err("should reject empty key");
        match err {
            CollectError::Config(msg) => assert!(msg.contains("api_key")),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Live integration test — only runs when `LINEAR_API_KEY` env var is set.
    #[tokio::test]
    async fn fetch_issue_live() {
        let key = match std::env::var("LINEAR_API_KEY") {
            Ok(k) => k,
            Err(_) => {
                eprintln!("SKIP: set LINEAR_API_KEY to run");
                return;
            }
        };
        let config = LinearConfig {
            api_key: Some(key),
            ..Default::default()
        };
        let client = LinearClient::new(&config).expect("client");
        let result = client.fetch_issue("ENG-1").await;
        assert!(result.is_ok(), "fetch should not error: {:?}", result);
        println!("Result: {:?}", result);
    }
}

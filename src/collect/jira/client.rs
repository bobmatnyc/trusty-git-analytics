//! Minimal JIRA REST client for fetching individual issues.

use std::sync::Mutex;

use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, USER_AGENT};
use serde::Deserialize;
use serde_json::{json, Value};
use tracing::debug;

use crate::collect::errors::{CollectError, Result};
use crate::core::config::JiraConfig;

/// HTTP `User-Agent` string sent on every request.
const USER_AGENT_VALUE: &str = "trusty-git-analytics/0.1";

/// Page size for JQL search pagination.
const SEARCH_PAGE_SIZE: usize = 50;

/// Async JIRA Cloud / Server client.
pub struct JiraClient {
    client: reqwest::Client,
    base_url: String,
    /// `(username, token)` for HTTP Basic Auth.
    credentials: Option<(String, String)>,
    /// Default project key for filtered queries.
    project_key: String,
    /// Cached story-point custom field key (e.g. `customfield_10016`).
    /// `None` = uncached; `Some(None)` = discovered to be absent;
    /// `Some(Some(_))` = discovered key.
    story_point_field: Mutex<Option<Option<String>>>,
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
    /// Story points (numeric estimate). Extracted from the configured
    /// custom field if discoverable; `None` when the field is absent or
    /// unset on the issue.
    pub story_points: Option<f64>,
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
    /// Capture all other fields so we can pluck the story-point custom field
    /// (whose key varies per JIRA instance) without modeling each.
    #[serde(flatten)]
    extra: std::collections::HashMap<String, Value>,
}

#[derive(Debug, Deserialize)]
struct ApiNamed {
    name: String,
}

/// `GET /rest/api/3/field` returns a flat list of field descriptors.
#[derive(Debug, Deserialize)]
struct FieldDescriptor {
    id: String,
    name: String,
}

/// Wire shape of a JQL search response.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    issues: Vec<ApiIssue>,
    #[serde(default)]
    total: u64,
    #[serde(rename = "startAt", default)]
    start_at: u64,
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
            story_point_field: Mutex::new(None),
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
        let story_field = self.get_story_point_field().await?;
        Ok(Some(Self::convert_issue(issue, story_field.as_deref())))
    }

    /// Default project key supplied at construction.
    pub fn project_key(&self) -> &str {
        &self.project_key
    }

    /// Search JIRA issues by JQL, paginating in `SEARCH_PAGE_SIZE` chunks.
    ///
    /// Why: many JIRA workflows (sprint rollups, ticket-id enrichment for
    /// commit messages) need bulk reads; single-issue fetches would be O(N)
    /// HTTP round-trips.
    /// What: `POST /rest/api/3/search` with `{ jql, startAt, maxResults }`,
    /// loops until either `max_results` issues are collected or the server
    /// reports no more pages.
    /// Test: covered by `jira_search_response_deserializes` (wire shape).
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn search_issues(&self, jql: &str, max_results: usize) -> Result<Vec<JiraIssue>> {
        let url = format!("{}/rest/api/3/search", self.base_url);
        let story_field = self.get_story_point_field().await?;
        // Request the story-point field explicitly when we know its key so
        // JIRA includes it; otherwise rely on `*all` to get every field.
        let fields: Vec<String> = match &story_field {
            Some(key) => vec![
                "summary".into(),
                "status".into(),
                "issuetype".into(),
                key.clone(),
            ],
            None => vec!["*all".into()],
        };

        let mut out: Vec<JiraIssue> = Vec::new();
        let mut start_at = 0u64;
        loop {
            let remaining = max_results.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            let page_size = remaining.min(SEARCH_PAGE_SIZE);
            let body = json!({
                "jql": jql,
                "startAt": start_at,
                "maxResults": page_size,
                "fields": fields,
            });
            debug!(url = %url, %jql, start_at, "POST");
            let mut req = self.client.post(&url).json(&body);
            if let Some((user, token)) = &self.credentials {
                req = req.basic_auth(user, Some(token));
            }
            let resp = req.send().await?.error_for_status()?;
            let parsed: SearchResponse = resp.json().await?;
            let n = parsed.issues.len();
            for issue in parsed.issues {
                out.push(Self::convert_issue(issue, story_field.as_deref()));
                if out.len() >= max_results {
                    break;
                }
            }
            if n < page_size {
                break;
            }
            start_at = parsed.start_at + n as u64;
            if start_at >= parsed.total {
                break;
            }
        }
        Ok(out)
    }

    /// Discover the JIRA custom-field key for "Story Points", cached for
    /// the lifetime of the client.
    ///
    /// Why: the field id (e.g. `customfield_10016`) is per-instance, so we
    /// must look it up at runtime rather than hard-coding.
    /// What: `GET /rest/api/3/field`, scans for a field whose `name`
    /// matches `"Story Points"` or `"Story point estimate"` (case-insensitive).
    /// Test: deserialization shape covered by `field_descriptor_deserializes`.
    ///
    /// Returns `Ok(None)` if no matching field exists on the instance.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Http`] on transport / non-success HTTP responses.
    /// - [`CollectError::Json`] on payload parse failures.
    pub async fn get_story_point_field(&self) -> Result<Option<String>> {
        // Fast path: serve from cache.
        {
            let guard = self
                .story_point_field
                .lock()
                .map_err(|e| CollectError::Config(format!("story-point cache poisoned: {e}")))?;
            if let Some(cached) = guard.as_ref() {
                return Ok(cached.clone());
            }
        }

        let url = format!("{}/rest/api/3/field", self.base_url);
        debug!(url = %url, "GET");
        let mut req = self.client.get(&url);
        if let Some((user, token)) = &self.credentials {
            req = req.basic_auth(user, Some(token));
        }
        let resp = req.send().await?.error_for_status()?;
        let fields: Vec<FieldDescriptor> = resp.json().await?;

        let found = fields
            .into_iter()
            .find(|f| {
                let n = f.name.to_ascii_lowercase();
                n == "story points" || n == "story point estimate"
            })
            .map(|f| f.id);

        // Persist (whether hit or miss) so we don't refetch.
        let mut guard = self
            .story_point_field
            .lock()
            .map_err(|e| CollectError::Config(format!("story-point cache poisoned: {e}")))?;
        *guard = Some(found.clone());
        Ok(found)
    }

    /// Convert an `ApiIssue` wire-form into our public [`JiraIssue`], plucking
    /// the story-point custom field when its key is known.
    fn convert_issue(api: ApiIssue, story_field_key: Option<&str>) -> JiraIssue {
        let story_points =
            story_field_key.and_then(|key| api.fields.extra.get(key).and_then(|v| v.as_f64()));
        JiraIssue {
            key: api.key,
            summary: api.fields.summary,
            status: api.fields.status.name,
            issue_type: api.fields.issue_type.name,
            story_points,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Confirm a JQL search response shape parses end-to-end.
    ///
    /// Why: pagination logic depends on `total` and `startAt` fields; if
    /// JIRA renames either, our loop terminates incorrectly.
    /// What: parse a representative search payload with one issue.
    /// Test: assert `total`, `startAt`, and inner issue fields all populate.
    #[test]
    fn jira_search_response_deserializes() {
        let json = r#"{
            "startAt": 0,
            "total": 1,
            "issues": [
                {
                    "key": "PROJ-1",
                    "fields": {
                        "summary": "Fix bug",
                        "status": {"name": "Done"},
                        "issuetype": {"name": "Bug"},
                        "customfield_10016": 5.0
                    }
                }
            ]
        }"#;
        let resp: SearchResponse = serde_json::from_str(json).expect("parses");
        assert_eq!(resp.total, 1);
        assert_eq!(resp.start_at, 0);
        assert_eq!(resp.issues.len(), 1);
        let issue = JiraClient::convert_issue(
            resp.issues.into_iter().next().expect("one"),
            Some("customfield_10016"),
        );
        assert_eq!(issue.key, "PROJ-1");
        assert_eq!(issue.summary, "Fix bug");
        assert_eq!(issue.status, "Done");
        assert_eq!(issue.issue_type, "Bug");
        assert_eq!(issue.story_points, Some(5.0));
    }

    /// Confirm field descriptor wire shape deserializes.
    ///
    /// Why: cache discovery hinges on this exact shape.
    /// What: parse a representative `/rest/api/3/field` element.
    /// Test: assert both fields extract.
    #[test]
    fn field_descriptor_deserializes() {
        let json = r#"[
            {"id": "customfield_10016", "name": "Story Points"},
            {"id": "summary", "name": "Summary"}
        ]"#;
        let fields: Vec<FieldDescriptor> = serde_json::from_str(json).expect("parses");
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].id, "customfield_10016");
        assert_eq!(fields[0].name, "Story Points");
    }

    /// Story points should be `None` when the custom field is absent.
    ///
    /// Why: not every JIRA instance has a configured story-point field;
    /// missing fields must degrade gracefully.
    /// What: convert an issue payload that omits the custom field.
    /// Test: assert `story_points` is `None`.
    #[test]
    fn convert_issue_returns_none_when_field_missing() {
        let json = r#"{
            "key": "PROJ-2",
            "fields": {
                "summary": "x",
                "status": {"name": "Open"},
                "issuetype": {"name": "Task"}
            }
        }"#;
        let api: ApiIssue = serde_json::from_str(json).expect("parses");
        let issue = JiraClient::convert_issue(api, Some("customfield_10016"));
        assert!(issue.story_points.is_none());
    }
}

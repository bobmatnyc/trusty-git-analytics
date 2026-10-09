//! The Linear issue model and its GraphQL field selection.
//!
//! Why: #190 — the bulk sync stored 11 fields and a flat copy of tga's own
//! struct as `raw_json`, so the Linear-aware effort and work extractors
//! (`core::pm_effort::extract`, `core::pm_work::extract`) found no estimate,
//! parent or creator. Rows were keyed by `identifier`, which changes when an
//! issue moves team. This module owns the one field selection every Linear
//! issue query sends, the parsed [`LinearIssue`], and the raw node kept beside
//! it.
//! What: [`ISSUE_FIELDS`], [`LinearIssue`], and [`parse_issue_node`]. Lifted
//! out of `client.rs` so that file does not grow.
//! Test: `tests` below; `collect::linear::bulk::tests` covers the HTTP walk.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The `Issue` selection set every Linear issue query sends (#190).
///
/// `labelIds` is the complete id list and costs no query complexity; the
/// `labels` connection supplies names for the first 50. `description` is left
/// out on purpose: `work_items.raw_json` is scanned by `tga inspect attest`,
/// and adding ticket bodies to it is step 3 of #190, not this step.
pub const ISSUE_FIELDS: &str = "id identifier title url priority estimate dueDate \
     createdAt updatedAt startedAt completedAt canceledAt archivedAt \
     state { name type } team { id name key } \
     assignee { id name displayName email } creator { id name } \
     project { id } cycle { id } parent { id identifier } \
     labelIds labels(first: 50) { nodes { id name } }";

/// A Linear issue fetched from the API.
///
/// `#[non_exhaustive]` since #190: the struct grows with each Linear field
/// tga stores, and each such field would otherwise be a SemVer break. Build
/// one with `LinearIssue { identifier, ..Default::default() }` inside this
/// crate, or with [`parse_issue_node`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LinearIssue {
    /// Linear's stable issue id (a UUID). `None` only for an issue that was
    /// deserialized from a payload written before #190.
    #[serde(default)]
    pub linear_id: Option<String>,
    /// Human-readable identifier (e.g. "ENG-123"). Changes when the issue
    /// moves to another team.
    pub identifier: String,
    /// Issue title.
    pub title: String,
    /// Current state name (e.g. "In Progress", "Done").
    pub state: String,
    /// Current state category: `backlog`, `unstarted`, `started`,
    /// `completed`, `canceled` or `triage`.
    #[serde(default)]
    pub state_type: Option<String>,
    /// Team name.
    pub team: String,
    /// Team id (UUID).
    #[serde(default)]
    pub team_id: Option<String>,
    /// Team key (e.g. "ENG") as the API reports it, not the identifier
    /// prefix — the two differ after a team move.
    #[serde(default)]
    pub team_key: Option<String>,
    /// Assignee display name (if any).
    pub assignee: Option<String>,
    /// Assignee id (UUID).
    #[serde(default)]
    pub assignee_id: Option<String>,
    /// Assignee full name.
    #[serde(default)]
    pub assignee_name: Option<String>,
    /// Assignee email, where the API key may read it.
    #[serde(default)]
    pub assignee_email: Option<String>,
    /// Creator id (UUID).
    #[serde(default)]
    pub creator_id: Option<String>,
    /// Issue priority (0=none, 1=urgent, 2=high, 3=medium, 4=low).
    pub priority: u8,
    /// Estimate in the team's estimation scale, when set.
    #[serde(default)]
    pub estimate: Option<f64>,
    /// Project id (UUID), when the issue belongs to a project.
    #[serde(default)]
    pub project_id: Option<String>,
    /// Cycle id (UUID), when the issue is in a cycle.
    #[serde(default)]
    pub cycle_id: Option<String>,
    /// Parent issue id (UUID), when the issue is a sub-issue.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Every label id on the issue.
    #[serde(default)]
    pub label_ids: Vec<String>,
    /// Label names, for the first 50 labels.
    #[serde(default)]
    pub label_names: Vec<String>,
    /// Due date as Linear sends it (`YYYY-MM-DD`).
    #[serde(default)]
    pub due_date: Option<String>,
    /// URL to the issue in Linear.
    pub url: String,
    /// When the issue was created (issue #7139).
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    /// When the issue was last updated. Drives the bulk sync's incremental
    /// cursor — see [`crate::collect::linear::sync::next_cursor`].
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    /// When the issue entered a "started" state, if it ever did.
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
    /// When the issue was completed, if it was. Paired with `created_at`,
    /// this is what makes lead-time-from-ticket computable for a
    /// Linear-only engagement.
    #[serde(default)]
    pub completed_at: Option<DateTime<Utc>>,
    /// When the issue was canceled, if it was.
    #[serde(default)]
    pub canceled_at: Option<DateTime<Utc>>,
    /// When the issue was archived. `Some` means the issue is archived.
    #[serde(default)]
    pub archived_at: Option<DateTime<Utc>>,
    /// The GraphQL issue node exactly as Linear returned it. Written to
    /// `raw_json` so the provider-aware extractors read Linear's own field
    /// names. Never serialized into itself.
    #[serde(skip)]
    pub raw: Option<serde_json::Value>,
}

impl LinearIssue {
    /// Whether Linear reports the issue as archived.
    #[must_use]
    pub fn is_archived(&self) -> bool {
        self.archived_at.is_some()
    }

    /// The payload to store as `raw_json`: the GraphQL node when the issue
    /// came from the API, else this struct serialized (a pre-#190 shape).
    #[must_use]
    pub fn raw_json(&self) -> Option<String> {
        match &self.raw {
            Some(node) => Some(node.to_string()),
            None => serde_json::to_string(self).ok(),
        }
    }
}

/// Parse one GraphQL issue node into a [`LinearIssue`], keeping the node.
///
/// `identifier_fallback` is used only when the node carries no `identifier`,
/// which the single-issue query relies on since it addresses the node by
/// identifier already. A missing or malformed optional field parses as
/// `None`; the node itself is kept whole in [`LinearIssue::raw`].
///
/// Test: `parse_issue_node_reads_every_field`,
/// `parse_issue_node_tolerates_absent_optional_fields`.
#[must_use]
pub fn parse_issue_node(identifier_fallback: &str, node: &serde_json::Value) -> LinearIssue {
    let parse_dt = |field: &str| -> Option<DateTime<Utc>> {
        node[field]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
    };
    let text = |v: &serde_json::Value| v.as_str().map(String::from);
    let strings = |v: &serde_json::Value| -> Vec<String> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let label_names = node["labels"]["nodes"]
        .as_array()
        .map(|a| a.iter().filter_map(|l| text(&l["name"])).collect())
        .unwrap_or_default();
    LinearIssue {
        linear_id: text(&node["id"]),
        identifier: node["identifier"]
            .as_str()
            .unwrap_or(identifier_fallback)
            .to_string(),
        title: node["title"].as_str().unwrap_or("").to_string(),
        state: node["state"]["name"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string(),
        state_type: text(&node["state"]["type"]),
        team: node["team"]["name"]
            .as_str()
            .unwrap_or("Unknown")
            .to_string(),
        team_id: text(&node["team"]["id"]),
        team_key: text(&node["team"]["key"]),
        assignee: text(&node["assignee"]["displayName"]),
        assignee_id: text(&node["assignee"]["id"]),
        assignee_name: text(&node["assignee"]["name"]),
        assignee_email: text(&node["assignee"]["email"]),
        creator_id: text(&node["creator"]["id"]),
        priority: node["priority"].as_u64().unwrap_or(0) as u8,
        estimate: node["estimate"].as_f64(),
        project_id: text(&node["project"]["id"]),
        cycle_id: text(&node["cycle"]["id"]),
        parent_id: text(&node["parent"]["id"]),
        label_ids: strings(&node["labelIds"]),
        label_names,
        due_date: text(&node["dueDate"]),
        url: node["url"].as_str().unwrap_or("").to_string(),
        created_at: parse_dt("createdAt"),
        updated_at: parse_dt("updatedAt"),
        started_at: parse_dt("startedAt"),
        completed_at: parse_dt("completedAt"),
        canceled_at: parse_dt("canceledAt"),
        archived_at: parse_dt("archivedAt"),
        raw: Some(node.clone()),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A full GraphQL issue node, shaped like Linear's `issues` response.
    pub(crate) fn full_node(id: &str, identifier: &str, team_key: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "identifier": identifier,
            "title": format!("Title for {identifier}"),
            "url": format!("https://linear.app/acme/issue/{identifier}"),
            "priority": 2,
            "estimate": 3.0,
            "dueDate": "2026-03-01",
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-05T00:00:00.000Z",
            "startedAt": "2026-01-02T00:00:00.000Z",
            "completedAt": "2026-01-04T00:00:00.000Z",
            "canceledAt": null,
            "archivedAt": null,
            "state": {"name": "Done", "type": "completed"},
            "team": {"id": format!("team-{team_key}"), "name": "Engineering", "key": team_key},
            "assignee": {"id": "user-1", "name": "Ada Lovelace", "displayName": "ada", "email": "ada@acme.test"},
            "creator": {"id": "user-2", "name": "Grace Hopper"},
            "project": {"id": "project-1"},
            "cycle": {"id": "cycle-1"},
            "parent": {"id": "parent-uuid", "identifier": format!("{team_key}-1")},
            "labelIds": ["label-1", "label-2"],
            "labels": {"nodes": [{"id": "label-1", "name": "bug"}, {"id": "label-2", "name": "api"}]}
        })
    }

    #[test]
    fn parse_issue_node_reads_every_field() {
        let node = full_node("uuid-7", "ENG-7", "ENG");
        let issue = parse_issue_node("", &node);
        assert_eq!(issue.linear_id.as_deref(), Some("uuid-7"));
        assert_eq!(issue.identifier, "ENG-7");
        assert_eq!(issue.state_type.as_deref(), Some("completed"));
        assert_eq!(issue.team_id.as_deref(), Some("team-ENG"));
        assert_eq!(issue.team_key.as_deref(), Some("ENG"));
        assert_eq!(issue.assignee.as_deref(), Some("ada"));
        assert_eq!(issue.assignee_id.as_deref(), Some("user-1"));
        assert_eq!(issue.assignee_name.as_deref(), Some("Ada Lovelace"));
        assert_eq!(issue.assignee_email.as_deref(), Some("ada@acme.test"));
        assert_eq!(issue.creator_id.as_deref(), Some("user-2"));
        assert_eq!(issue.estimate, Some(3.0));
        assert_eq!(issue.project_id.as_deref(), Some("project-1"));
        assert_eq!(issue.cycle_id.as_deref(), Some("cycle-1"));
        assert_eq!(issue.parent_id.as_deref(), Some("parent-uuid"));
        assert_eq!(issue.label_ids, vec!["label-1", "label-2"]);
        assert_eq!(issue.label_names, vec!["bug", "api"]);
        assert_eq!(issue.due_date.as_deref(), Some("2026-03-01"));
        assert!(issue.completed_at.is_some());
        assert!(!issue.is_archived());
        assert_eq!(issue.raw.as_ref(), Some(&node));
    }

    #[test]
    fn parse_issue_node_tolerates_absent_optional_fields() {
        let node = serde_json::json!({"identifier": "ENG-1", "title": "t"});
        let issue = parse_issue_node("", &node);
        assert_eq!(issue.linear_id, None);
        assert_eq!(issue.estimate, None);
        assert!(issue.label_ids.is_empty());
        assert_eq!(issue.state, "Unknown");
    }
}

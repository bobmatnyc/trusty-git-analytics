//! Linear workspace guard and read-only check (#272).
//!
//! Why: an API key can belong to any Linear workspace. When the operator
//! names the one workspace tga may read (`linear.workspace`), a key for any
//! other workspace must not read a single issue. Linear access is also
//! read-only by policy, so no GraphQL mutation may leave this process.
//! What: [`WorkspaceGuard`] reads `organization { urlKey }` once per API key
//! per guard (one guard per Linear client or resolver) and caches the
//! verdict under a BLAKE3 hash of the key, failure included, so every later
//! request with that key fails closed without re-asking, and a request with
//! any other key is checked on its own. [`ensure_read_only`] rejects any GraphQL document that is not a
//! query; every Linear send site calls it.
//! Test: `guard_passes_on_matching_url_key`,
//! `guard_fails_closed_on_other_url_key`, `guard_fails_closed_on_query_failure`,
//! `guard_queries_once`, `read_only_check_rejects_mutations`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::OnceCell;
use trusty_common::credentials::scrub_secrets;

/// The one GraphQL document the guard sends.
pub const ORGANIZATION_QUERY: &str = "query { organization { urlKey } }";

/// Why the guard refused a Linear request.
///
/// `Clone` because one cached verdict answers every request of the run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkspaceGuardError {
    /// The API key belongs to a workspace other than the configured one.
    #[error(
        "Linear workspace mismatch: config `linear.workspace` expects urlKey `{expected}`, \
         but the API key belongs to urlKey `{actual}`; refusing to read Linear"
    )]
    Mismatch {
        /// The configured urlKey.
        expected: String,
        /// The urlKey Linear reported for the key.
        actual: String,
    },
    /// The organization query itself failed, so the workspace is unknown.
    #[error(
        "Linear workspace check failed (config expects urlKey `{expected}`): {reason}; \
         refusing to read Linear"
    )]
    Unverified {
        /// The configured urlKey.
        expected: String,
        /// Credential-free description of the failure.
        reason: String,
    },
    /// A GraphQL document that is not a query was about to be sent.
    #[error("refusing to send a Linear GraphQL document that is not a read-only query")]
    NotReadOnly,
}

/// Whether `document` is a read-only GraphQL query.
///
/// Why: Linear access is read-only (#272). A single choke point every send
/// site calls makes that a property of the code, not of a review.
/// What: `Ok` when the trimmed document starts with `query` or `{` (the
/// shorthand query form); otherwise [`WorkspaceGuardError::NotReadOnly`].
/// Test: `read_only_check_rejects_mutations`.
///
/// # Errors
///
/// [`WorkspaceGuardError::NotReadOnly`] for a mutation, a subscription, or
/// an empty document.
pub fn ensure_read_only(document: &str) -> Result<(), WorkspaceGuardError> {
    let doc = document.trim_start();
    if doc.starts_with("query") || doc.starts_with('{') {
        Ok(())
    } else {
        Err(WorkspaceGuardError::NotReadOnly)
    }
}

/// Once-per-run check that the Linear API key belongs to the expected
/// workspace.
///
/// Why: see the module doc. Asking once keeps the cost at one request per
/// run; caching a failure keeps the guard fail-closed.
/// What: holds the expected urlKey and a lazily filled verdict.
/// Test: see the module doc.
#[derive(Debug)]
pub struct WorkspaceGuard {
    expected: String,
    /// #272: one verdict per API key, keyed by the key's BLAKE3 hash — the
    /// key itself is never stored here. A pass for one key never answers for
    /// another.
    verdicts: Mutex<HashMap<[u8; 32], Arc<Verdict>>>,
}

/// A lazily filled verdict for one API key.
type Verdict = OnceCell<Result<(), WorkspaceGuardError>>;

impl WorkspaceGuard {
    /// A guard expecting the workspace `expected` (a Linear urlKey).
    pub fn new(expected: impl Into<String>) -> Self {
        Self {
            expected: expected.into().trim().to_string(),
            verdicts: Mutex::new(HashMap::new()),
        }
    }

    /// The guard `linear.workspace` asks for, or `None` when it is unset or
    /// blank — the unchanged, unguarded behaviour.
    pub fn from_config(workspace: Option<&str>) -> Option<Arc<Self>> {
        workspace
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(|w| Arc::new(Self::new(w)))
    }

    /// The configured urlKey.
    pub fn expected(&self) -> &str {
        &self.expected
    }

    /// Verify the workspace, asking Linear on the first call per API key.
    ///
    /// Why: every Linear request of the run must pass through here before it
    /// is sent, so the first request pays for the check and the rest reuse it.
    /// What: sends [`ORGANIZATION_QUERY`] to `endpoint` with `api_key` once,
    /// compares `data.organization.urlKey` with the expected key (ASCII
    /// case-insensitive), and caches the verdict. A transport error, a
    /// non-2xx status, GraphQL errors or a missing urlKey all yield
    /// [`WorkspaceGuardError::Unverified`]; nothing skips the guard.
    /// Test: `guard_passes_on_matching_url_key`,
    /// `guard_fails_closed_on_other_url_key`,
    /// `guard_fails_closed_on_query_failure`, `guard_queries_once`.
    ///
    /// # Errors
    ///
    /// [`WorkspaceGuardError::Mismatch`] or
    /// [`WorkspaceGuardError::Unverified`], cached for the guard's lifetime.
    pub async fn verify(
        &self,
        client: &reqwest::Client,
        endpoint: &str,
        api_key: &str,
    ) -> Result<(), WorkspaceGuardError> {
        let id: [u8; 32] = blake3::hash(api_key.as_bytes()).into();
        let cell = {
            // A poisoned lock fails closed: no verdict, no request.
            let mut map = self
                .verdicts
                .lock()
                .map_err(|_| WorkspaceGuardError::Unverified {
                    expected: self.expected.clone(),
                    reason: "workspace verdict cache is poisoned".into(),
                })?;
            Arc::clone(map.entry(id).or_default())
        };
        cell.get_or_init(|| self.query_organization(client, endpoint, api_key))
            .await
            .clone()
    }

    /// One `organization { urlKey }` round-trip, compared with the expected key.
    async fn query_organization(
        &self,
        client: &reqwest::Client,
        endpoint: &str,
        api_key: &str,
    ) -> Result<(), WorkspaceGuardError> {
        ensure_read_only(ORGANIZATION_QUERY)?;
        let unverified = |reason: String| WorkspaceGuardError::Unverified {
            expected: self.expected.clone(),
            reason: scrub_secrets(&reason, &[api_key]),
        };
        let resp = client
            .post(endpoint)
            .header("Authorization", api_key)
            .header("Content-Type", "application/json")
            .json(&serde_json::json!({ "query": ORGANIZATION_QUERY }))
            .send()
            .await
            .map_err(|e| unverified(format!("organization query failed: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(unverified(format!(
                "organization query returned HTTP {}",
                status.as_u16()
            )));
        }
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| unverified(format!("organization response is not JSON: {e}")))?;
        if json
            .get("errors")
            .and_then(|e| e.as_array())
            .is_some_and(|a| !a.is_empty())
        {
            return Err(unverified(
                "organization query returned GraphQL errors".into(),
            ));
        }
        let actual = json["data"]["organization"]["urlKey"]
            .as_str()
            .ok_or_else(|| unverified("organization response carries no urlKey".into()))?;
        if actual.eq_ignore_ascii_case(&self.expected) {
            Ok(())
        } else {
            Err(WorkspaceGuardError::Mismatch {
                expected: self.expected.clone(),
                actual: actual.to_string(),
            })
        }
    }
}

#[cfg(test)]
#[path = "workspace_tests.rs"]
mod tests;

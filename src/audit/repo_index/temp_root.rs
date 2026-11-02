//! Indexing a checkout that lives under an OS-temp prefix (#214).
//!
//! Why: trusty-search refuses `/tmp`, `/private/tmp` and `/var/folders` roots
//! by design (trusty-tools#767). In trusty-search 0.62.1 `trusty-search index
//! <path>` applies that refusal with no opt-in: the CLI checks the root before
//! it dials the daemon, and the daemon's reindex root check refuses it again.
//! A scratch work dir, such as a taudit acceptance run under `/private/tmp`, is
//! therefore never indexed by the CLI route. The one opt-in an indexer has is
//! `search.index.create`'s `allow_sensitive_path`, meant for a client that
//! names one exact root. It relaxes only the temp-prefix rows. The allowlist
//! gate still applies, so the root must also be approved with `trusty-search
//! index add --allow-sensitive-path` (taudit's approval step does this).
//!
//! What: [`TempRootRoute`], which decides whether a checkout takes this route,
//! and [`index_over_socket`]: create the index with the opt-in, queue a reindex
//! with no root override, and wait for the walk that started after the trigger.
//! A walk that stalls is a failure, never a partial success, and any failure
//! after the create deregisters the index so the next run retries it.
//!
//! Test: `super::temp_root_tests`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use trusty_common::search_rpc::{
    call_blocking, METHOD_INDEX_CREATE, METHOD_INDEX_DELETE, METHOD_INDEX_REINDEX,
    METHOD_INDEX_STATUS,
};

use super::RepoIndexStatus;

/// The OS-temp prefixes trusty-search refuses without the opt-in.
///
/// A copy of the temp rows of trusty-search's `SENSITIVE_PATH_PREFIXES`
/// (`allowlist/mod.rs`, 0.62.1). The `Library/Application Support` row is left
/// out: no audit work dir lives there. If this list drifts, a root it misses
/// takes the CLI route and is refused as before; a root it wrongly includes is
/// still indexed, because the opt-in is harmless on a non-temp root.
const TEMP_PREFIXES: [&str; 4] = [
    "/tmp",
    "/private/tmp",
    "/var/folders",
    "/private/var/folders",
];

/// One call's budget. A create can reload a cold-parked index before it answers.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Interval between status polls.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long the walk may go without progress before the wait gives up.
///
/// 120 s, the same stall bound `trusty-search index` uses for its foreground
/// wait, so both routes give up on a stuck index at the same point.
const STALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Where, and for which checkouts, the temp-root route runs.
///
/// Why: the socket and the timings come from the environment and constants,
/// which would make the route untestable if it read them itself. A value lets a
/// test drive it against a stub socket with short timings, the same split
/// [`super::super::SearchGuard`] uses.
/// What: the daemon socket, the prefixes that select this route, and the three
/// budgets.
/// Test: `super::temp_root_tests::the_production_route_selects_only_temp_roots`.
#[derive(Debug, Clone)]
pub(in crate::audit) struct TempRootRoute {
    /// The trusty-search daemon's socket.
    pub socket: PathBuf,
    /// A checkout under one of these takes the socket route.
    pub prefixes: Vec<PathBuf>,
    /// Budget for each RPC call.
    pub call_timeout: Duration,
    /// Interval between status polls.
    pub poll_interval: Duration,
    /// How long the walk may make no progress before the wait detaches.
    pub stall_timeout: Duration,
}

impl TempRootRoute {
    /// The route `tga audit` runs with: the socket the search guard dials, and
    /// trusty-search's temp prefixes.
    pub(in crate::audit) fn from_env() -> Self {
        Self {
            socket: crate::audit::search_daemon::search_socket(),
            prefixes: TEMP_PREFIXES.iter().map(PathBuf::from).collect(),
            call_timeout: CALL_TIMEOUT,
            poll_interval: POLL_INTERVAL,
            stall_timeout: STALL_TIMEOUT,
        }
    }

    /// A route that selects nothing, so every checkout takes the CLI route.
    #[cfg(test)]
    pub(in crate::audit) fn disabled() -> Self {
        Self {
            prefixes: Vec::new(),
            ..Self::from_env()
        }
    }

    /// Whether `checkout` is under one of the prefixes.
    ///
    /// The check runs on the canonical path, as trusty-search's own does: macOS
    /// resolves `/tmp` and `/var` to `/private/...`. A path that cannot be
    /// canonicalised is checked as given.
    pub(in crate::audit) fn applies_to(&self, checkout: &Path) -> bool {
        let canonical = std::fs::canonicalize(checkout).unwrap_or_else(|_| checkout.to_path_buf());
        self.prefixes
            .iter()
            .any(|prefix| canonical.starts_with(prefix))
    }
}

/// Create, reindex and wait for `checkout` under `index_id` over the socket.
///
/// Why/What: see the module docs. Every failure is a
/// [`RepoIndexStatus::Failed`] carrying one line; nothing here panics or
/// propagates.
///
/// # Postconditions
/// `Indexed` means the daemon registered the index and finished a walk that
/// started after this call's trigger. `Failed` covers a refused create, a
/// refused reindex, a walk error, and a walk that made no progress for the
/// stall bound, whatever its chunk count. After any failure that follows a
/// successful create, the index has been deregistered (corpus kept), or the
/// reason says why that failed. A registered index answers the CLI membership
/// probe even when empty, so leaving it would make the next run skip it.
///
/// Test: `super::temp_root_tests::{a_temp_root_is_indexed_over_the_socket_with_the_opt_in,
/// a_refused_create_is_a_gap_and_sends_no_reindex,
/// a_refused_reindex_is_a_gap_and_polls_no_status,
/// a_reindex_that_never_produces_chunks_is_a_gap,
/// a_stalled_walk_with_chunks_is_a_gap,
/// a_walk_error_is_a_gap,
/// a_failed_temp_root_index_is_retried_on_the_next_run,
/// a_failed_deregister_is_named_in_the_reason}`.
pub(in crate::audit) fn index_over_socket(
    route: &TempRootRoute,
    checkout: &Path,
    index_id: &str,
) -> RepoIndexStatus {
    let root = match std::fs::canonicalize(checkout) {
        Ok(root) => root,
        Err(e) => {
            return RepoIndexStatus::Failed(format!(
                "could not resolve `{}`: {e}",
                checkout.display()
            ))
        }
    };
    let call = |method: &str, params: Value| {
        call_blocking(&route.socket, method, params, route.call_timeout).map_err(|e| {
            format!(
                "trusty-search {method} for `{}` failed: {e:#}",
                root.display()
            )
        })
    };

    // #214: the per-request opt-in is the only way 0.62.1 indexes a temp root.
    let create = json!({
        "id": index_id,
        "root_path": root.to_string_lossy(),
        "allow_sensitive_path": true,
    });
    if let Err(reason) = call(METHOD_INDEX_CREATE, create) {
        return RepoIndexStatus::Failed(reason);
    }
    let triggered = chrono::Utc::now();
    // #214: no root override; the daemon's reindex root check has no opt-in.
    let status = match call(METHOD_INDEX_REINDEX, json!({ "index_id": index_id })) {
        Err(reason) => RepoIndexStatus::Failed(reason),
        Ok(_) => wait_for_walk(route, index_id, triggered, |params| {
            call(METHOD_INDEX_STATUS, params)
        }),
    };
    let RepoIndexStatus::Failed(reason) = status else {
        return status;
    };
    // #214: `index-status` exits 0 for any registered index, so an index left
    // registered after a failure reads as served on the next run.
    let deregister = json!({
        "index_id": index_id,
        "delete_data": false,
        "expected_root_path": root.to_string_lossy(),
    });
    match call(METHOD_INDEX_DELETE, deregister) {
        Ok(_) => RepoIndexStatus::Failed(reason),
        Err(cause) => RepoIndexStatus::Failed(format!(
            "{reason}; the index could not be deregistered, so the next run may skip it: {cause}"
        )),
    }
}

/// Poll the index status until the walk the trigger started is done.
///
/// Why: `search.index.reindex` only queues the walk, and the renderer reads the
/// index straight after this returns. Returning at once would hand it an empty
/// index.
/// What: done when a walk that started at or after `triggered` has finished.
/// A reported walk error fails at once. Progress is the chunk count plus the
/// files seen; when neither moves for `stall_timeout`, the wait fails. A walk
/// can end with neither `last_indexed` nor `last_walk_error` (an aborted or
/// unsettled run in trusty-search 0.62.1), leaving a partial corpus, so chunks
/// alone never make it indexed. A status call that errors counts as no
/// progress, and its last error is the reason.
fn wait_for_walk(
    route: &TempRootRoute,
    index_id: &str,
    triggered: chrono::DateTime<chrono::Utc>,
    status: impl Fn(Value) -> Result<Value, String>,
) -> RepoIndexStatus {
    let mut last_progress = Instant::now();
    let mut progress: Option<(u64, u64)> = None;
    let mut chunks = 0;
    loop {
        let last_error = match status(json!({ "index_id": index_id })) {
            Ok(body) => {
                if let Some(error) = body.get("last_walk_error").and_then(Value::as_str) {
                    return RepoIndexStatus::Failed(format!(
                        "the trusty-search walk failed: {error}"
                    ));
                }
                if walk_finished_since(&body, triggered) {
                    return RepoIndexStatus::Indexed;
                }
                let seen = (
                    count(&body, "chunk_count"),
                    count(&body, "last_walk_files_seen"),
                );
                chunks = seen.0;
                if progress != Some(seen) {
                    progress = Some(seen);
                    last_progress = Instant::now();
                }
                None
            }
            Err(reason) => Some(reason),
        };
        if last_progress.elapsed() >= route.stall_timeout {
            return stalled(route.stall_timeout, chunks, last_error);
        }
        std::thread::sleep(route.poll_interval);
    }
}

/// The outcome of a wait that hit the stall bound: always a failure.
fn stalled(bound: Duration, chunks: u64, last_error: Option<String>) -> RepoIndexStatus {
    if let Some(reason) = last_error {
        return RepoIndexStatus::Failed(reason);
    }
    // #214: a stalled walk may have ended unsettled, leaving a partial corpus.
    RepoIndexStatus::Failed(format!(
        "the trusty-search walk made no progress for {}s with {chunks} chunks indexed, so \
         the index may be partial",
        bound.as_secs()
    ))
}

/// Whether the status body reports a walk that started at or after `since` and
/// then finished.
fn walk_finished_since(body: &Value, since: chrono::DateTime<chrono::Utc>) -> bool {
    let at = |key: &str| {
        body.get(key)
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&chrono::Utc))
    };
    match (at("last_walk_started_at"), at("last_indexed")) {
        (Some(started), Some(finished)) => started >= since && finished >= started,
        _ => false,
    }
}

/// A non-negative integer field, or 0 when it is absent.
fn count(body: &Value, key: &str) -> u64 {
    body.get(key).and_then(Value::as_u64).unwrap_or(0)
}

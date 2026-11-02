//! Tests for the temp-root route (#214): `super::temp_root`.
//!
//! The daemon is a stub socket that records every call and answers per method,
//! so each test states exactly what trusty-search said and asserts what the
//! route sent and concluded.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::temp_root::{index_over_socket, TempRootRoute};
use super::{ensure_repositories_routed, index_id_for, RepoIndexStatus};
use crate::report::dd_manifest::DdRepositoryEntry;

/// Every `(method, params)` the stub daemon received, in order.
type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A stub trusty-search socket under `dir`.
///
/// `answer` gets the method, its params and every earlier call, and returns the
/// JSON-RPC member to send: `{"result": …}` or `{"error": …}`.
fn serve(
    dir: &Path,
    answer: impl Fn(&str, &Value, &[(String, Value)]) -> Value + Send + Sync + 'static,
) -> (PathBuf, Calls) {
    let socket = dir.join("sockets").join("trusty-search.sock");
    let listener = trusty_common::uds::bind_hardened(&socket).expect("bind the stub socket");
    let calls: Calls = Arc::default();
    let seen = Arc::clone(&calls);
    let answer = Arc::new(answer);
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            let seen = Arc::clone(&seen);
            let answer = Arc::clone(&answer);
            tokio::spawn(async move {
                use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
                let (read, mut write) = conn.split();
                let mut line = String::new();
                let _ = tokio::io::BufReader::new(read).read_line(&mut line).await;
                let request: Value = serde_json::from_str(&line).expect("a JSON-RPC frame");
                let method = request["method"].as_str().unwrap_or_default().to_string();
                let params = request["params"].clone();
                let mut reply = {
                    let mut calls = seen.lock().expect("calls lock");
                    let reply = answer(&method, &params, &calls);
                    calls.push((method, params));
                    reply
                };
                reply["jsonrpc"] = json!("2.0");
                reply["id"] = json!(1);
                let _ = write.write_all(reply.to_string().as_bytes()).await;
                let _ = write.write_all(b"\n").await;
                let _ = write.flush().await;
            });
        }
    });
    (socket, calls)
}

/// A daemon that accepts the create and the reindex, and answers status with
/// `status` once a reindex has been received.
fn daemon(dir: &Path, status: fn() -> Value) -> (PathBuf, Calls) {
    serve(dir, move |method, _, _| match method {
        "search.index.create" => json!({"result": {"created": true}}),
        "search.index.reindex" => json!({"result": {"queued": true}}),
        "search.index.status" => status(),
        _ => json!({"error": {"code": -32601, "message": "method not found"}}),
    })
}

/// A status body for a walk that started and finished now.
fn finished_now() -> Value {
    let now = chrono::Utc::now().to_rfc3339();
    json!({"result": {
        "chunk_count": 3,
        "last_walk_files_seen": 1,
        "last_walk_started_at": now,
        "last_indexed": now,
    }})
}

/// A route over `socket` that selects everything under `dir`, with short budgets.
fn route(socket: PathBuf, dir: &Path) -> TempRootRoute {
    TempRootRoute {
        socket,
        prefixes: vec![std::fs::canonicalize(dir).expect("canonical temp dir")],
        call_timeout: Duration::from_secs(5),
        poll_interval: Duration::from_millis(20),
        stall_timeout: Duration::from_millis(300),
    }
}

/// A checkout directory under `dir`.
fn checkout(dir: &Path) -> PathBuf {
    let path = dir.join("repos").join("same-file");
    std::fs::create_dir_all(&path).expect("create the checkout");
    path
}

/// The methods the stub received, in order.
fn methods(calls: &Calls) -> Vec<String> {
    calls
        .lock()
        .expect("calls lock")
        .iter()
        .map(|(m, _)| m.clone())
        .collect()
}

/// The defect: `trusty-search index` refuses a temp root, so the sweep sends a
/// temp checkout over the socket with the opt-in instead. The CLI stub fails
/// any `index` call, so a CLI spawn would show up as a failure and in its log.
#[tokio::test]
async fn a_temp_root_is_indexed_over_the_socket_with_the_opt_in() {
    use std::os::unix::fs::PermissionsExt as _;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, calls) = daemon(dir.path(), finished_now);
    let log = dir.path().join("cli-calls");
    let stub = dir.path().join("trusty-search-stub");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$@\" >> {}\n[ \"$1\" = index-status ] && exit 1\nexit 7\n",
            log.display()
        ),
    )
    .expect("write the stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let entries = vec![DdRepositoryEntry {
        name: "same-file".to_string(),
        path: path.clone(),
        authorship: None,
    }];

    let outcomes = ensure_repositories_routed(
        stub.display().to_string(),
        route(socket, dir.path()),
        &entries,
    )
    .await;

    assert_eq!(outcomes[0].status, RepoIndexStatus::Indexed, "{outcomes:?}");
    let id = index_id_for(&path).expect("id");
    let calls = calls.lock().expect("calls lock").clone();
    assert_eq!(calls[0].0, "search.index.create");
    assert_eq!(
        calls[0].1,
        json!({
            "id": id,
            "root_path": std::fs::canonicalize(&path).expect("canonical").to_string_lossy(),
            "allow_sensitive_path": true,
        })
    );
    assert_eq!(calls[1].0, "search.index.reindex");
    assert_eq!(
        calls[1].1,
        json!({"index_id": id}),
        "a reindex root override is checked with no opt-in, so none is sent"
    );
    assert_eq!(calls[2].0, "search.index.status");
    let cli = std::fs::read_to_string(&log).unwrap_or_default();
    assert_eq!(
        cli.trim(),
        format!("index-status {id}"),
        "the CLI only probes"
    );
}

/// A refused create is the repository's gap, quoting the daemon, and no
/// reindex follows it.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_create_is_a_gap_and_sends_no_reindex() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, calls) = serve(
        dir.path(),
        |_, _, _| json!({"error": {"code": -32602, "message": "indexing refused: root is not allowlisted"}}),
    );

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    match status {
        RepoIndexStatus::Failed(reason) => {
            assert!(reason.contains("root is not allowlisted"), "{reason}");
            assert_eq!(reason.lines().count(), 1, "{reason}");
        }
        other => panic!("a refused create must be a gap: {other:?}"),
    }
    assert_eq!(methods(&calls), vec!["search.index.create"]);
}

/// Error arm: a reindex that never produces a chunk is not an index, even
/// though the wait detaches at the stall bound.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reindex_that_never_produces_chunks_is_a_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = daemon(dir.path(), || json!({"result": {"chunk_count": 0}}));

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert!(
        matches!(&status, RepoIndexStatus::Failed(r) if r.contains("produced no chunks")),
        "{status:?}"
    );
}

/// Error arm: a status call that keeps failing is the gap, not an index. The
/// daemon went away after queueing the walk, so nothing confirms any chunk.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_daemon_that_stops_answering_status_is_a_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = daemon(
        dir.path(),
        || json!({"error": {"code": -32004, "message": "unknown index"}}),
    );

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert!(
        matches!(&status, RepoIndexStatus::Failed(r) if r.contains("unknown index")),
        "{status:?}"
    );
}

/// A walk error the daemon reports fails the repository at once.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_walk_error_is_a_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = daemon(
        dir.path(),
        || json!({"result": {"chunk_count": 0, "last_walk_error": "permission denied"}}),
    );

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert!(
        matches!(&status, RepoIndexStatus::Failed(r) if r.contains("permission denied")),
        "{status:?}"
    );
}

/// CLI parity: a walk that holds chunks and stops making progress detaches as
/// indexed, as `trusty-search index`'s foreground wait does.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_walk_with_chunks_detaches_as_indexed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = daemon(dir.path(), || json!({"result": {"chunk_count": 5}}));

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert_eq!(status, RepoIndexStatus::Indexed);
}

/// The production route selects exactly the OS-temp roots trusty-search
/// refuses, and the disabled route selects none.
#[test]
fn the_production_route_selects_only_temp_roots() {
    let production = TempRootRoute::from_env();
    for temp in [
        "/tmp/t208/work/repos/same-file",
        "/private/tmp/t208/work/repos/same-file",
        "/private/var/folders/9x/T/work/repos/same-file",
    ] {
        assert!(production.applies_to(Path::new(temp)), "{temp}");
    }
    for kept in [
        "/Users/r/.trusty-tools/trusty-audit/work/repos/xsv",
        "/tmpfoo/repos/xsv",
    ] {
        assert!(!production.applies_to(Path::new(kept)), "{kept}");
    }
    assert!(!TempRootRoute::disabled().applies_to(Path::new("/tmp/t208/work/repos/same-file")));
}

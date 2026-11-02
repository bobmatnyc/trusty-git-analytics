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

/// A daemon that accepts the create, the reindex and the deregister, and
/// answers status with `status`.
fn daemon(dir: &Path, status: fn() -> Value) -> (PathBuf, Calls) {
    serve(dir, move |method, _, _| match method {
        "search.index.create" => json!({"result": {"created": true}}),
        "search.index.reindex" => json!({"result": {"queued": true}}),
        "search.index.delete" => json!({"result": {"deleted": true}}),
        "search.index.status" => status(),
        _ => json!({"error": {"code": -32601, "message": "method not found"}}),
    })
}

/// A `trusty-search` CLI stub under `dir` that logs to `log`. Its
/// `index-status` exits 0 only while `registered` exists, as the real CLI exits
/// 0 for any registered index; any `index` call exits 7.
fn cli_stub(dir: &Path, log: &Path, registered: &Path) -> String {
    use std::os::unix::fs::PermissionsExt as _;

    let stub = dir.join("trusty-search-stub");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\necho \"$@\" >> {log}\n\
             if [ \"$1\" = index-status ]; then [ -e {registered} ] && exit 0; exit 1; fi\n\
             exit 7\n",
            log = log.display(),
            registered = registered.display(),
        ),
    )
    .expect("write the stub");
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    stub.display().to_string()
}

/// One manifest entry for `path`.
fn entry(path: &Path) -> Vec<DdRepositoryEntry> {
    vec![DdRepositoryEntry {
        name: "same-file".to_string(),
        path: path.to_path_buf(),
        authorship: None,
    }]
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
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, calls) = daemon(dir.path(), finished_now);
    let log = dir.path().join("cli-calls");
    let stub = cli_stub(dir.path(), &log, &dir.path().join("never-registered"));

    let outcomes = ensure_repositories_routed(stub, route(socket, dir.path()), &entry(&path)).await;

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
        matches!(&status, RepoIndexStatus::Failed(r)
            if r.contains("no progress") && r.contains("0 chunks")),
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

/// Error arm: a walk that holds chunks but stops making progress is a gap. In
/// trusty-search 0.62.1 a walk can end with neither `last_indexed` nor
/// `last_walk_error` (aborted, or promotion unsettled), leaving a partial
/// corpus, so the chunk count proves nothing.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_walk_with_chunks_is_a_gap() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = daemon(dir.path(), || json!({"result": {"chunk_count": 5}}));

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert!(
        matches!(&status, RepoIndexStatus::Failed(r)
            if r.contains("no progress") && r.contains("5 chunks")),
        "{status:?}"
    );
}

/// A refused reindex is the gap, quoting the daemon. No status poll follows,
/// and the index the create registered is deregistered.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refused_reindex_is_a_gap_and_polls_no_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, calls) = serve(dir.path(), |method, _, _| match method {
        "search.index.create" => json!({"result": {"created": true}}),
        "search.index.reindex" => {
            json!({"error": {"code": -32009, "message": "reindex_already_running"}})
        }
        "search.index.delete" => json!({"result": {"deleted": true}}),
        _ => json!({"result": {"chunk_count": 1}}),
    });

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    assert!(
        matches!(&status, RepoIndexStatus::Failed(r) if r.contains("reindex_already_running")),
        "{status:?}"
    );
    assert_eq!(
        methods(&calls),
        vec![
            "search.index.create",
            "search.index.reindex",
            "search.index.delete"
        ]
    );
}

/// Error arm: a failure after a successful create must not leave the index
/// registered. `index-status` exits 0 for any registered index, so the next
/// run would report the repository as already served and never index it.
/// The stub's registry is a marker file the CLI probe reads: the create sets
/// it and the deregister clears it.
#[tokio::test]
async fn a_failed_temp_root_index_is_retried_on_the_next_run() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let registered = dir.path().join("registered");
    let walk_fails = Arc::new(AtomicBool::new(true));
    let (socket, calls) = {
        let registered = registered.clone();
        let walk_fails = Arc::clone(&walk_fails);
        serve(dir.path(), move |method, _, _| match method {
            "search.index.create" => {
                std::fs::write(&registered, b"").expect("register");
                json!({"result": {"created": true}})
            }
            "search.index.reindex" => json!({"result": {"queued": true}}),
            "search.index.delete" => {
                let _ = std::fs::remove_file(&registered);
                json!({"result": {"deleted": true}})
            }
            _ if walk_fails.load(Ordering::SeqCst) => {
                json!({"result": {"chunk_count": 2, "last_walk_error": "prune failed"}})
            }
            _ => finished_now(),
        })
    };
    let stub = cli_stub(dir.path(), &dir.path().join("cli-calls"), &registered);

    let first = ensure_repositories_routed(
        stub.clone(),
        route(socket.clone(), dir.path()),
        &entry(&path),
    )
    .await;
    walk_fails.store(false, Ordering::SeqCst);
    let second = ensure_repositories_routed(stub, route(socket, dir.path()), &entry(&path)).await;

    assert!(first[0].failed(), "{first:?}");
    assert_eq!(
        second[0].status,
        RepoIndexStatus::Indexed,
        "the failed index must be retried, not reported as served"
    );
    let id = index_id_for(&path).expect("id");
    let calls = calls.lock().expect("calls lock").clone();
    let delete = calls
        .iter()
        .find(|(m, _)| m == "search.index.delete")
        .expect("the failed index is deregistered");
    assert_eq!(
        delete.1,
        json!({
            "index_id": id,
            "delete_data": false,
            "expected_root_path": std::fs::canonicalize(&path).expect("canonical").to_string_lossy(),
        }),
        "deregister only, and only the registration at this root"
    );
    let creates = calls
        .iter()
        .filter(|(m, _)| m == "search.index.create")
        .count();
    assert_eq!(creates, 2, "the second run creates the index again");
}

/// A deregister that fails is named in the reason, since the next run may
/// then skip the repository.
// The route blocks its thread, so the stub needs a second worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_deregister_is_named_in_the_reason() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = checkout(dir.path());
    let (socket, _) = serve(dir.path(), |method, _, _| match method {
        "search.index.create" => json!({"result": {"created": true}}),
        "search.index.reindex" => json!({"result": {"queued": true}}),
        "search.index.delete" => {
            json!({"error": {"code": -32009, "message": "root_path mismatch"}})
        }
        _ => json!({"result": {"chunk_count": 0, "last_walk_error": "permission denied"}}),
    });

    let status = index_over_socket(&route(socket, dir.path()), &path, "same-file-1");

    match status {
        RepoIndexStatus::Failed(reason) => {
            assert!(reason.contains("permission denied"), "{reason}");
            assert!(reason.contains("could not be deregistered"), "{reason}");
            assert!(reason.contains("root_path mismatch"), "{reason}");
            assert_eq!(reason.lines().count(), 1, "{reason}");
        }
        other => panic!("a walk error must be a gap: {other:?}"),
    }
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

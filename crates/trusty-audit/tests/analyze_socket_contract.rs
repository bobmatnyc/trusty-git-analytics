//! trusty-audit dials the trusty-analyze socket every trusty crate resolves.
//!
//! Why: before the split, consumer 4 of trusty-tools'
//! `crates/trusty-crate-contracts/tests/analyze_uds_consumers.rs` proved this
//! against a live daemon. That test cannot take a Cargo edge on trusty-audit
//! from another repository, so the contract moves here, beside tga's
//! `audit::tests::the_analyze_socket_is_the_path_every_trusty_crate_resolves`.
//! The daemon binds `trusty_common::daemon_socket_path("trusty-analyze")`
//! (`trusty_analyze::service::rpc::socket_path`). A trusty-audit path that
//! differs reads a serving daemon as absent, and `ensure_analyze` then spawns a
//! second one beside it (trusty-tools#6287, #4246).
//!
//! What: points the shared data-dir resolver at a temp dir, then checks that
//! `Tools::pinned` and `daemons::analyze_socket` land on the shared path, and
//! that `daemons::ensure_analyze` accepts a healthy daemon serving there. The
//! analyze binary is a path that cannot exist, so a pass also proves the fast
//! path returned without spawning anything.
//!
//! This file holds exactly one test because it writes the process environment
//! (`TRUSTY_DATA_DIR_OVERRIDE`, `TRUSTY_ANALYZE_SOCKET`). Cargo runs each file
//! under `tests/` as its own process, so no sibling test can observe the change
//! — the same arrangement `collectors_real_resolve.rs` uses.
//!
//! Test: `the_analyze_socket_is_the_path_every_trusty_crate_resolves`.

#![cfg(unix)]

use std::path::{Path, PathBuf};

use trusty_audit::grounding::{Tools, daemons};

/// The service name every trusty crate passes to `daemon_socket_path`.
const ANALYZE_SERVICE: &str = "trusty-analyze";

/// Serve `analyze.health` as a healthy, search-reachable daemon on `socket`.
///
/// The reply mirrors the stub in `grounding::grounding_tests`: one framed
/// JSON-RPC result per connection, read after the client closes its half.
fn serve_healthy_analyze(socket: &Path) {
    let listener = trusty_common::uds::bind_hardened(socket).expect("bind the analyze socket");
    tokio::spawn(async move {
        while let Ok((mut conn, _)) = listener.accept().await {
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                let mut sink = Vec::new();
                let _ = conn.read_to_end(&mut sink).await;
                let reply = if String::from_utf8_lossy(&sink).contains("analyze.health") {
                    r#"{"jsonrpc":"2.0","id":1,"result":{"status":"ok","version":"0.0.0","search_reachable":true}}"#
                } else {
                    r#"{"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no such method"}}"#
                };
                let _ = conn.write_all(reply.as_bytes()).await;
                let _ = conn.write_all(b"\n").await;
                let _ = conn.flush().await;
            });
        }
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn the_analyze_socket_is_the_path_every_trusty_crate_resolves() {
    let tmp = tempfile::tempdir().expect("tempdir");
    // SAFETY: the only test in this process (see the module docs), and set
    // before anything here reads the environment. Clearing the socket override
    // makes every side fall through to the data-dir resolver, which is the
    // agreement under test; a developer's exported override would otherwise
    // point trusty-audit at their own daemon.
    unsafe {
        std::env::set_var("TRUSTY_DATA_DIR_OVERRIDE", tmp.path());
        std::env::remove_var(daemons::ENV_ANALYZE_SOCKET);
    }

    let shared = trusty_common::daemon_socket_path(ANALYZE_SERVICE)
        .expect("resolve the shared trusty-analyze socket");
    assert!(
        shared.starts_with(tmp.path()),
        "the override must be in effect, so this test never binds a real daemon's socket: {}",
        shared.display()
    );
    // The layout every consumer shares, stated outright so a trusty-common
    // upgrade that moves it shows up here rather than as a false `down`.
    let data_dir =
        trusty_common::resolve_data_dir(ANALYZE_SERVICE).expect("resolve the data directory");
    assert_eq!(shared, data_dir.join("trusty-analyze.sock"));

    assert_eq!(
        daemons::analyze_socket(),
        shared,
        "trusty-audit's resolver must derive the socket trusty-analyze binds"
    );
    let mut tools = Tools::pinned(
        PathBuf::from("/nonexistent/trusty-search"),
        PathBuf::from("/nonexistent/trusty-analyze"),
    );
    assert_eq!(
        tools.analyze_socket, shared,
        "`Tools::pinned` must hand ensure_analyze the shared path"
    );
    // This case exercises the analyze guard alone. Point the search half at a
    // path nothing binds, so an edit that made ensure_analyze dial it fails
    // here rather than reaching whatever daemon the machine happens to run.
    tools.search_socket = tmp.path().join("absent-search.sock");

    serve_healthy_analyze(&shared);
    daemons::ensure_analyze(&tools)
        .await
        .expect("trusty-audit must accept a healthy daemon on the shared socket without spawning");
}

//! Log lines go to stderr, never stdout (#111).
//!
//! Why: `tga eval`, `tga inspect --json` and other commands are piped into
//! files and parsers; a tracing line on stdout corrupts that output.

use std::process::Command;

use tga::core::db::Database;

const BIN: &str = env!("CARGO_BIN_EXE_tga");

/// Why: tracing-subscriber's `fmt()` writes to stdout unless told otherwise.
/// What: runs `tga inspect schema` with `RUST_LOG=info` and a config file
/// present (so `loading config` is logged at INFO); stdout must hold the
/// schema and no INFO line, and stderr must hold the INFO line.
/// Test: this function.
#[test]
fn info_logs_go_to_stderr_not_stdout() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("tga.db");
    drop(Database::open(&db).expect("create db"));
    let config = dir.path().join("config.yaml");
    std::fs::write(&config, "version: \"1.0\"\nrepositories: []\n").expect("config");

    let out = Command::new(BIN)
        .arg("--config")
        .arg(&config)
        .arg("--database")
        .arg(&db)
        .args(["inspect", "schema"])
        .env("RUST_LOG", "info")
        .env("TRUSTY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("spawn tga");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "tga failed; stderr:\n{stderr}");
    assert!(
        stdout.contains("commits"),
        "schema missing from stdout:\n{stdout}"
    );
    assert!(!stdout.contains("INFO"), "log lines on stdout:\n{stdout}");
    assert!(stderr.contains("INFO"), "no INFO line on stderr:\n{stderr}");
}

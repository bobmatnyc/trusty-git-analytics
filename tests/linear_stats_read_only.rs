//! #190: `tga linear stats` reads the database as it stands.
//!
//! Runs the `tga` binary, so the `main` dispatch is exercised, not one
//! helper. A report command that created or migrated the file would print a
//! zeroed report for a wrong `--database` path and exit 0.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use tga::core::db::migrations::{run_through, MIGRATIONS};

/// Run `tga linear stats --json` against `db`, from inside `dir`.
fn stats(dir: &Path, db: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tga"))
        .current_dir(dir)
        .env("TRUSTY_NO_UPDATE_CHECK", "1")
        .env_remove("LINEAR_API_KEY")
        .args(["--config", "absent-config.yaml", "--database"])
        .arg(db)
        .args(["linear", "stats", "--json"])
        .output()
        .expect("run tga")
}

/// A database migrated through `version`.
fn fixture(dir: &Path, version: i64) -> PathBuf {
    let path = dir.join("tga.db");
    let mut conn = Connection::open(&path).expect("create fixture");
    run_through(&mut conn, version).expect("migrate fixture");
    drop(conn);
    path
}

fn latest() -> i64 {
    MIGRATIONS.last().expect("migrations").version
}

#[test]
fn a_missing_database_fails_naming_the_path_and_creates_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("wrong.db");
    let out = stats(dir.path(), &db);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "exit 0 for a missing database");
    assert!(stderr.contains("wrong.db"), "path not named: {stderr}");
    assert!(!db.exists(), "the missing database was created");
}

#[test]
fn a_database_with_pending_migrations_fails_and_is_left_alone() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), latest() - 1);
    let before = fs::read(&db).expect("read fixture");
    let out = stats(dir.path(), &db);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "exit 0 for an unmigrated database");
    assert!(stderr.contains("pending migrations"), "{stderr}");
    assert!(
        stderr.contains("tga linear sync"),
        "no remedy named: {stderr}"
    );
    assert!(
        !stderr.contains("--dry-run"),
        "dry-run advice leaked: {stderr}"
    );
    assert!(
        fs::read(&db).expect("read fixture") == before,
        "database changed"
    );
}

#[test]
fn a_migrated_database_prints_the_json_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), latest());
    let out = stats(dir.path(), &db);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON on stdout");
    assert_eq!(v["schema_version"], 1);
    assert_eq!(v["population"]["issues"], 0);
}

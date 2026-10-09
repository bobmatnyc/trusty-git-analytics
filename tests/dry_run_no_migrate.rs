//! #189: a `--dry-run` never migrates, re-journals or creates the database.
//!
//! Runs the `tga` binary against database files built here, so the whole
//! `main` dispatch is exercised, not one helper. No network: the update check
//! is disabled, no config names a remote, and the sync commands stop at the
//! schema or credential check before any request.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use tga::core::db::migrations::{run_through, MIGRATIONS};

/// The schema version the older-schema fixture stops at.
const OLD_VERSION: i64 = 30;

/// Every `tga backfill` subcommand, as its CLI words.
const BACKFILL: &[&[&str]] = &[
    &["ai-detection"],
    &["revert-flags"],
    &["ticket-ids"],
    &["reachability"],
    &["effort"],
    &["complexity"],
    &["ticketed"],
    &["ai-detection-commits"],
    &["top-level"],
    &["effort-tshirt"],
    &["quality"],
    &["pm-work"],
    &["pm-effort"],
];

/// The `--dry-run` paths that read the database, each as CLI words.
fn db_reading_dry_runs() -> Vec<Vec<&'static str>> {
    let mut all: Vec<Vec<&str>> = BACKFILL
        .iter()
        .map(|sub| {
            let mut v = vec!["backfill"];
            v.extend_from_slice(sub);
            v.push("--dry-run");
            v
        })
        .collect();
    all.push(vec!["linear", "sync", "--team", "ENG", "--dry-run"]);
    all.push(vec!["jira", "sync", "--project", "ENG", "--dry-run"]);
    all.push(vec!["profile", "dev@example.com", "--dry-run"]);
    all
}

/// The `--dry-run` paths whose pipeline runs on an in-memory shadow database.
fn shadow_dry_runs() -> Vec<Vec<&'static str>> {
    vec![
        vec!["collect", "--dry-run", "--no-validate"],
        vec!["analyze", "--dry-run", "--no-validate"],
    ]
}

/// A fixture database in the rollback-journal mode an un-migrated file has,
/// at schema `version`, holding one author and one revert commit.
fn fixture(dir: &Path, version: i64) -> PathBuf {
    let path = dir.join("tga.db");
    let mut conn = Connection::open(&path).expect("create fixture");
    run_through(&mut conn, version).expect("migrate fixture");
    conn.execute_batch(
        "INSERT INTO authors (canonical_name, canonical_email) \
             VALUES ('Dev', 'dev@example.com'); \
         INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository) \
             VALUES ('abc123', 'Dev', 'dev@example.com', '2026-01-05T10:00:00Z', \
                     'Revert \"feat: x\"', 'svc');",
    )
    .expect("seed fixture");
    drop(conn);
    path
}

/// Run `tga` with `words` against `db`, from inside `dir`.
fn tga(dir: &Path, db: &Path, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tga"))
        .current_dir(dir)
        .env("TRUSTY_NO_UPDATE_CHECK", "1")
        .env_remove("LINEAR_API_KEY")
        .env_remove("JIRA_API_TOKEN")
        .args(["--config", "absent-config.yaml", "--database"])
        .arg(db)
        .args(words)
        .output()
        .expect("run tga")
}

/// The schema version, journal mode and table list `path` holds, read without
/// writing — for an assertion message that says what changed.
fn describe(path: &Path) -> String {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .expect("open read-only");
    let version: i64 = conn
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |r| {
            r.get(0)
        })
        .expect("version");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .expect("journal mode");
    let tables: i64 = conn
        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |r| r.get(0))
        .expect("tables");
    format!("schema v{version}, journal {mode}, {tables} schema objects")
}

/// Assert `db` still holds `before` byte for byte and no journal sidecar was
/// left beside it.
fn assert_untouched(db: &Path, before: &[u8], words: &[&str], out: &Output) {
    let after = fs::read(db).expect("read fixture");
    assert!(
        after == before,
        "`tga {}` changed the database (now {}); stderr: {}",
        words.join(" "),
        describe(db),
        String::from_utf8_lossy(&out.stderr)
    );
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut side = db.as_os_str().to_owned();
        side.push(suffix);
        assert!(
            !Path::new(&side).exists(),
            "`tga {}` left {suffix} beside the database",
            words.join(" ")
        );
    }
}

/// Why (#189): `main` opened every database through `Database::open`, which
/// switches it to WAL and runs every pending migration, before any command
/// read its own `--dry-run` flag. A preview moved a production file from
/// schema 30 to 32.
/// What: against a schema-30 rollback-journal file, every database-reading
/// dry run exits non-zero naming migrations 31 and 32, and the file keeps
/// every byte.
/// Test: this test.
#[test]
fn dry_run_refuses_an_older_schema_and_leaves_it_untouched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), OLD_VERSION);
    let before = fs::read(&db).expect("read fixture");
    let pending: Vec<i64> = MIGRATIONS
        .iter()
        .map(|m| m.version)
        .filter(|v| *v > OLD_VERSION)
        .collect();

    for words in db_reading_dry_runs() {
        let out = tga(dir.path(), &db, &words);
        assert_untouched(&db, &before, &words, &out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "`tga {}` reported success against schema {OLD_VERSION}: {stderr}",
            words.join(" ")
        );
        for v in &pending {
            assert!(
                stderr.contains(&format!("v{v}")),
                "`tga {}` did not name pending migration v{v}: {stderr}",
                words.join(" ")
            );
        }
    }
}

/// Why (#189): `collect` and `analyze` already ran their dry-run pipeline on an
/// in-memory shadow, yet `main` still migrated the real file first.
/// What: both succeed against the schema-30 file and leave every byte of it.
/// Test: this test.
#[test]
fn shadow_dry_runs_never_open_the_database() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), OLD_VERSION);
    let before = fs::read(&db).expect("read fixture");

    for words in shadow_dry_runs() {
        let out = tga(dir.path(), &db, &words);
        assert_untouched(&db, &before, &words, &out);
        assert!(
            out.status.success(),
            "`tga {}` failed: {}",
            words.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Why (#189): a read-only open must still let a dry run on a current schema
/// do its job, and a dry run that wrote anything would now fail loudly.
/// What: against a current-schema file, every backfill dry run and the profile
/// dry run succeed and the file keeps every byte; `revert-flags` reports the
/// one revert commit the fixture holds. The sync commands stop at their
/// missing-credential error with the file untouched.
/// Test: this test.
#[test]
fn dry_run_reads_a_current_schema_without_writing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), i64::MAX);
    let before = fs::read(&db).expect("read fixture");

    for mut words in db_reading_dry_runs() {
        let is_sync = words[0] == "linear" || words[0] == "jira";
        if words[0] == "profile" {
            words.extend(["--output", "profile-out"]);
        }
        let out = tga(dir.path(), &db, &words);
        assert_untouched(&db, &before, &words, &out);
        let stderr = String::from_utf8_lossy(&out.stderr);
        if is_sync {
            assert!(!out.status.success(), "no credentials: {stderr}");
            assert!(!stderr.contains("pending"), "schema is current: {stderr}");
        } else {
            assert!(
                out.status.success(),
                "`tga {}` failed: {stderr}",
                words.join(" ")
            );
        }
        if words[1] == "revert-flags" {
            let stdout = String::from_utf8_lossy(&out.stdout);
            assert!(
                stdout.contains("Would set is_revert on 1 of 1 scanned commits"),
                "revert-flags preview: {stdout}"
            );
        }
    }
}

/// Why (#189): `Database::open` creates a missing file, so a dry run pointed
/// at a mistyped path left an empty, fully migrated database behind.
/// What: every dry run against an absent path leaves it absent; the
/// database-reading ones exit non-zero, the shadow ones succeed.
/// Test: this test.
#[test]
fn dry_run_creates_no_missing_database_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = dir.path().join("absent.db");

    for words in db_reading_dry_runs() {
        let out = tga(dir.path(), &db, &words);
        assert!(!db.exists(), "`tga {}` created the file", words.join(" "));
        assert!(
            !out.status.success(),
            "`tga {}` succeeded with no database",
            words.join(" ")
        );
    }
    for words in shadow_dry_runs() {
        let out = tga(dir.path(), &db, &words);
        assert!(!db.exists(), "`tga {}` created the file", words.join(" "));
        assert!(
            out.status.success(),
            "`tga {}` failed: {}",
            words.join(" "),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Why (#189): the fix must not reach the writable path, which keeps WAL and
/// migrating on every open.
/// What: `backfill revert-flags` without `--dry-run` moves the schema-30 file
/// to the newest version and to WAL, and sets the flag.
/// Test: this test.
#[test]
fn a_real_run_still_migrates_and_switches_to_wal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = fixture(dir.path(), OLD_VERSION);
    let latest = MIGRATIONS.last().expect("migrations").version;

    let out = tga(dir.path(), &db, &["backfill", "revert-flags"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let described = describe(&db);
    assert!(
        described.starts_with(&format!("schema v{latest}, journal wal")),
        "{described}"
    );
}

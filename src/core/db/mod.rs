//! SQLite database access layer.
//!
//! All databases opened by this crate are configured with the following
//! pragmas on every connection open (see `Database::apply_pragmas`):
//!
//! - `journal_mode = WAL` — concurrent reads during write-heavy collection
//! - `synchronous = NORMAL` — durability with reasonable performance
//! - `foreign_keys = ON` — enforce FK constraints
//! - `cache_size = -65536` — 64 MB page cache (negative = KB)
//! - `temp_store = MEMORY` — temporary tables / indexes held in RAM
//! - `mmap_size = 268435456` — 256 MB memory-mapped I/O window
//!
//! WAL mode is **mandatory** per project conventions and is set on every
//! [`Database::open`] call.
//!
//! ## Connection pooling
//!
//! `tga` is a single-process CLI, not a server. A single
//! [`rusqlite::Connection`] guarded by `&mut` borrow is sufficient: the
//! collection / classification / report stages each run sequentially and
//! never share the connection across threads. We deliberately do **not**
//! pull in `r2d2-sqlite` — a pool adds locking and threading overhead with
//! zero benefit at our concurrency level (1). If a future stage needs
//! parallel SQLite access, prefer `rusqlite`'s built-in
//! `Connection::open_with_flags` per worker over a pool.
//!
//! See `docs/adr/0001-sqlite-tuning.md` for the rationale behind each
//! pragma value.

use std::path::Path;

use rusqlite::Connection;
use tracing::{debug, info};

use crate::core::config::expand_path;
use crate::core::errors::{Result, TgaError};

pub mod azdo_iterations;
pub mod collection_runs;
pub mod migrations;
pub mod work_items;

pub use azdo_iterations::{list_iterations, upsert_iteration};
pub use collection_runs::{is_week_collected, record_collection_run, repo_count_for_week};
pub use work_items::{
    get_work_item, get_work_items_for_commit, link_commit_work_item, list_work_items,
    upsert_work_item, WorkItemRow,
};

/// Wrapper around a [`rusqlite::Connection`] with project-standard pragmas
/// applied and migrations run.
pub struct Database {
    conn: Connection,
}

impl Database {
    /// Open or create a SQLite database at `path`, apply pragmas, and run
    /// any pending migrations.
    ///
    /// Tilde-expansion is applied to `path`.
    ///
    /// # Errors
    ///
    /// - [`TgaError::DbError`] for SQLite-level failures.
    /// - [`TgaError::MigrationError`] if a migration fails.
    pub fn open(path: &Path) -> Result<Database> {
        let resolved = expand_path(path);
        debug!(path = %resolved.display(), "opening database");
        let conn = Connection::open(&resolved)?;
        Self::apply_pragmas(&conn)?;
        let mut db = Database { conn };
        migrations::run(&mut db.conn)?;
        info!(path = %resolved.display(), "database ready");
        Ok(db)
    }

    /// Open an in-memory database. Primarily intended for tests.
    ///
    /// # Errors
    ///
    /// See [`Database::open`].
    pub fn open_in_memory() -> Result<Database> {
        let conn = Connection::open_in_memory()?;
        Self::apply_pragmas(&conn)?;
        let mut db = Database { conn };
        migrations::run(&mut db.conn)?;
        Ok(db)
    }

    /// Apply the canonical pragma set on a fresh connection.
    ///
    /// Pragmas applied (see module-level docs for rationale):
    /// - `journal_mode = WAL`
    /// - `synchronous = NORMAL`
    /// - `foreign_keys = ON`
    /// - `cache_size = -65536` (64 MB)
    /// - `temp_store = MEMORY`
    /// - `mmap_size = 268435456` (256 MB)
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        // `journal_mode` is a query-style pragma; use query_row to honor it.
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(TgaError::from)?;
        debug!(journal_mode = %mode, "applied WAL pragma");
        // Bundle the remaining pragmas in a single batch — none of them
        // return rows so `execute_batch` is appropriate.
        conn.execute_batch(
            "PRAGMA synchronous = NORMAL; \
             PRAGMA foreign_keys = ON; \
             PRAGMA cache_size = -65536; \
             PRAGMA temp_store = MEMORY; \
             PRAGMA mmap_size = 268435456;",
        )?;
        debug!("applied SQLite tuning pragmas (cache=64MB, mmap=256MB, temp=memory)");
        Ok(())
    }

    /// Borrow the underlying connection (read-only).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Borrow the underlying connection mutably.
    pub fn connection_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Return the active journal mode (e.g. `"wal"` or `"memory"`).
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] if the pragma query fails.
    pub fn journal_mode(&self) -> Result<String> {
        let mode: String = self
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(TgaError::from)?;
        Ok(mode)
    }

    /// Return the highest applied migration version.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::DbError`] if the query fails.
    pub fn schema_version(&self) -> Result<i64> {
        let v: i64 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
                [],
                |row| row.get(0),
            )
            .map_err(TgaError::from)?;
        Ok(v)
    }
}

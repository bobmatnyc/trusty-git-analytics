//! SQLite database access layer.
//!
//! All databases opened by this crate are configured with:
//! - `journal_mode = WAL` — concurrent reads during write-heavy collection
//! - `synchronous = NORMAL` — durability with reasonable performance
//! - `foreign_keys = ON` — enforce FK constraints
//!
//! WAL mode is **mandatory** per project conventions and is set on every
//! [`Database::open`] call.

use std::path::Path;

use rusqlite::Connection;
use tracing::{debug, info};

use crate::config::expand_path;
use crate::errors::{Result, TgaError};

pub mod migrations;

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

    /// Apply the canonical pragma set: WAL journal, normal sync, FK enforcement.
    fn apply_pragmas(conn: &Connection) -> Result<()> {
        // `journal_mode` is a query-style pragma; use query_row to honor it.
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))
            .map_err(TgaError::from)?;
        debug!(journal_mode = %mode, "applied WAL pragma");
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON;")?;
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

//! Persistence helpers for the `azdo_iterations` table (Phase 4).
//!
//! Iteration rows mirror the shape of
//! [`crate::collect::azdo::AzdoIteration`]. Upserts are idempotent —
//! re-running collection refreshes the `fetched_at` default and any
//! mutable fields (name, dates, time frame) without producing duplicate
//! rows.

use rusqlite::{params, Connection};
use tracing::debug;

use crate::collect::azdo::AzdoIteration;
use crate::core::errors::{Result, TgaError};

/// Insert or replace an iteration row.
///
/// Uses `INSERT OR REPLACE` against the `id` primary key so re-fetching
/// the same iteration updates its mutable columns. Idempotent.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the underlying SQL execution fails.
pub fn upsert_iteration(conn: &Connection, project: &str, iteration: &AzdoIteration) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO azdo_iterations \
         (id, project, name, path, start_date, finish_date, time_frame, fetched_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, datetime('now'))",
        params![
            iteration.id,
            project,
            iteration.name,
            iteration.path,
            iteration.start_date,
            iteration.finish_date,
            iteration.time_frame,
        ],
    )
    .map_err(TgaError::from)?;
    debug!(id = %iteration.id, project = %project, "upserted azdo iteration");
    Ok(())
}

/// List all iterations stored for a given project, ordered by `start_date`
/// ascending with NULL dates last.
///
/// Returns an empty `Vec` if the project has no iterations recorded.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] if the SQL query or row decoding fails.
pub fn list_iterations(conn: &Connection, project: &str) -> Result<Vec<AzdoIteration>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, path, start_date, finish_date, time_frame \
             FROM azdo_iterations \
             WHERE project = ?1 \
             ORDER BY (start_date IS NULL), start_date, id",
        )
        .map_err(TgaError::from)?;
    let rows = stmt
        .query_map(params![project], |row| {
            Ok(AzdoIteration {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                start_date: row.get(3)?,
                finish_date: row.get(4)?,
                time_frame: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(TgaError::from)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::Database;

    fn sample(id: &str, name: &str, start: Option<&str>) -> AzdoIteration {
        AzdoIteration {
            id: id.into(),
            name: name.into(),
            path: format!("MyProject\\{name}"),
            start_date: start.map(String::from),
            finish_date: None,
            time_frame: "current".into(),
        }
    }

    #[test]
    fn upsert_then_list_roundtrips() {
        let db = Database::open_in_memory().expect("open in-memory");
        upsert_iteration(
            db.connection(),
            "MyProject",
            &sample("aaa", "Sprint 1", Some("2025-01-01")),
        )
        .expect("upsert");
        upsert_iteration(
            db.connection(),
            "MyProject",
            &sample("bbb", "Sprint 2", Some("2025-01-15")),
        )
        .expect("upsert");

        let rows = list_iterations(db.connection(), "MyProject").expect("list");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "aaa");
        assert_eq!(rows[1].id, "bbb");
    }

    #[test]
    fn upsert_is_idempotent_and_refreshes() {
        let db = Database::open_in_memory().expect("open in-memory");
        let mut it = sample("aaa", "Sprint 1", Some("2025-01-01"));
        upsert_iteration(db.connection(), "MyProject", &it).expect("first");
        it.name = "Sprint 1 (renamed)".into();
        upsert_iteration(db.connection(), "MyProject", &it).expect("second");

        let rows = list_iterations(db.connection(), "MyProject").expect("list");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Sprint 1 (renamed)");
    }

    #[test]
    fn list_filters_by_project() {
        let db = Database::open_in_memory().expect("open in-memory");
        upsert_iteration(
            db.connection(),
            "ProjA",
            &sample("aaa", "Sprint 1", Some("2025-01-01")),
        )
        .expect("upsert");
        upsert_iteration(
            db.connection(),
            "ProjB",
            &sample("bbb", "Sprint 1", Some("2025-01-01")),
        )
        .expect("upsert");

        let a = list_iterations(db.connection(), "ProjA").expect("list a");
        let b = list_iterations(db.connection(), "ProjB").expect("list b");
        assert_eq!(a.len(), 1);
        assert_eq!(b.len(), 1);
        assert_eq!(a[0].id, "aaa");
        assert_eq!(b[0].id, "bbb");
    }

    #[test]
    fn list_orders_null_dates_last() {
        let db = Database::open_in_memory().expect("open in-memory");
        upsert_iteration(
            db.connection(),
            "MyProject",
            &sample("with-date", "Sprint A", Some("2025-01-01")),
        )
        .expect("upsert dated");
        upsert_iteration(
            db.connection(),
            "MyProject",
            &sample("no-date", "Sprint B", None),
        )
        .expect("upsert undated");
        let rows = list_iterations(db.connection(), "MyProject").expect("list");
        assert_eq!(rows[0].id, "with-date");
        assert_eq!(rows[1].id, "no-date");
    }
}

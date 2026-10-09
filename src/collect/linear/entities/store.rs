//! Persist Linear reference entities into the v34 tables (#190).
//!
//! Why: each entity set must land whole or not at all, and a re-sync must
//! update rows in place rather than add new ones.
//! What: [`commit_entity_sync`] upserts one set's nodes, keyed by Linear's
//! `id`, and records the set's [`EntitySyncState`] in one transaction.
//! [`upsert_entities_in`] is the same write inside a caller's transaction.
//! Each row keeps the node as `raw_json`; the named columns are listed in
//! `sql/0034_linear_reference_entities.sql`.
//! Test: `commands::linear::entity_sync_tests`.

use chrono::{DateTime, Utc};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value as Json;

use super::{is_archived_node, EntityKind};
use crate::core::db::Database;
use crate::core::errors::{Result, TgaError};

/// The last successful sync of one entity set.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EntitySyncState {
    /// Newest `updatedAt` stored for the set (RFC 3339); never moves back.
    pub max_updated_at: Option<String>,
    /// Wall-clock time of the last successful sync (RFC 3339).
    pub last_run_at: String,
    /// Nodes the last successful sync wrote.
    pub rows_synced: i64,
}

/// Read the stored sync state for `kind`; `None` before its first sync.
///
/// # Errors
///
/// [`TgaError::DbError`] on a failed read.
pub fn get_entity_sync_state(
    conn: &Connection,
    kind: EntityKind,
) -> Result<Option<EntitySyncState>> {
    conn.query_row(
        "SELECT max_updated_at, last_run_at, rows_synced \
         FROM linear_entity_sync_state WHERE entity = ?1",
        params![kind.name()],
        |r| {
            Ok(EntitySyncState {
                max_updated_at: r.get(0)?,
                last_run_at: r.get(1)?,
                rows_synced: r.get(2)?,
            })
        },
    )
    .optional()
    .map_err(TgaError::from)
}

/// Upsert one entity set and record its sync state in one transaction.
///
/// Why: #190 — a set written page by page could leave a partial set
/// behind a failed walk; the caller fetches the whole set first and this
/// writes it, with its state row, as one unit.
/// What: [`upsert_entities_in`] then the `linear_entity_sync_state` row,
/// both inside one transaction. A failure rolls both back. Returns the
/// number of nodes written.
/// Test: `commands::linear::entity_sync_tests::rerun_is_idempotent`,
/// `commands::linear::entity_sync_tests::mid_pagination_failure_writes_nothing_and_keeps_the_state`.
///
/// # Errors
///
/// [`TgaError::DbError`] on SQL failure; [`TgaError::ValidationError`] on a
/// node without an `id`.
pub fn commit_entity_sync(db: &mut Database, kind: EntityKind, nodes: &[Json]) -> Result<usize> {
    let tx = db.connection_mut().transaction()?;
    let written = upsert_entities_in(&tx, kind, nodes)?;
    record_state(&tx, kind, nodes)?;
    tx.commit()?;
    Ok(written)
}

/// Upsert `nodes` into `kind`'s table without opening a transaction.
///
/// What: one `INSERT ... ON CONFLICT(id) DO UPDATE` per node, so a re-run
/// rewrites each row in place. Nothing commits here.
///
/// # Errors
///
/// As [`commit_entity_sync`].
pub fn upsert_entities_in(conn: &Connection, kind: EntityKind, nodes: &[Json]) -> Result<usize> {
    let fetched_at = Utc::now().to_rfc3339();
    for node in nodes {
        let mut cols = columns(kind, node)?;
        cols.push((
            "archived",
            Value::Integer(i64::from(is_archived_node(node))),
        ));
        cols.push(("raw_json", Value::Text(node.to_string())));
        cols.push(("fetched_at", Value::Text(fetched_at.clone())));
        let names: Vec<&str> = cols.iter().map(|(c, _)| *c).collect();
        let marks: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
        let updates: Vec<String> = names
            .iter()
            .filter(|c| **c != "id")
            .map(|c| format!("{c} = excluded.{c}"))
            .collect();
        let sql = format!(
            "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT(id) DO UPDATE SET {}",
            kind.table(),
            names.join(", "),
            marks.join(", "),
            updates.join(", ")
        );
        let values: Vec<Value> = cols.into_iter().map(|(_, v)| v).collect();
        conn.execute(&sql, rusqlite::params_from_iter(values))?;
    }
    Ok(nodes.len())
}

/// Write the set's state row; `max_updated_at` only moves forward.
fn record_state(conn: &Connection, kind: EntityKind, nodes: &[Json]) -> Result<()> {
    let newest = nodes
        .iter()
        .filter_map(|n| n["updatedAt"].as_str())
        .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
        .max()
        .map(|d| d.to_rfc3339());
    conn.execute(
        "INSERT INTO linear_entity_sync_state (entity, max_updated_at, last_run_at, rows_synced) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(entity) DO UPDATE SET \
           max_updated_at = CASE \
             WHEN excluded.max_updated_at IS NULL THEN max_updated_at \
             WHEN max_updated_at IS NULL OR excluded.max_updated_at > max_updated_at \
               THEN excluded.max_updated_at \
             ELSE max_updated_at END, \
           last_run_at = excluded.last_run_at, rows_synced = excluded.rows_synced",
        params![
            kind.name(),
            newest,
            Utc::now().to_rfc3339(),
            nodes.len() as i64
        ],
    )?;
    Ok(())
}

fn text(node: &Json, key: &str) -> Value {
    node[key]
        .as_str()
        .map_or(Value::Null, |s| Value::Text(s.to_string()))
}

fn nested_id(node: &Json, key: &str) -> Value {
    text(&node[key], "id")
}

fn real(node: &Json, key: &str) -> Value {
    node[key].as_f64().map_or(Value::Null, Value::Real)
}

fn flag(node: &Json, key: &str) -> Value {
    node[key]
        .as_bool()
        .map_or(Value::Null, |b| Value::Integer(i64::from(b)))
}

/// The last element of a numeric history array, when it has one.
fn last(node: &Json, key: &str) -> Option<f64> {
    node[key].as_array()?.last()?.as_f64()
}

fn int(v: Option<f64>) -> Value {
    // Linear types counts and cycle numbers as Float; they are whole numbers.
    v.map_or(Value::Null, |f| Value::Integer(f as i64))
}

/// The named columns for one node of `kind`, `id` first.
fn columns(kind: EntityKind, n: &Json) -> Result<Vec<(&'static str, Value)>> {
    let id = n["id"].as_str().ok_or_else(|| {
        TgaError::ValidationError(format!(
            "Linear {} node has no id; nothing was written for {}",
            kind.root_field(),
            kind.name()
        ))
    })?;
    let mut c = vec![("id", Value::Text(id.to_string()))];
    match kind {
        EntityKind::Teams => c.extend([
            ("key", text(n, "key")),
            ("name", text(n, "name")),
            ("private", flag(n, "private")),
            ("timezone", text(n, "timezone")),
            ("issue_estimation_type", text(n, "issueEstimationType")),
            (
                "issue_estimation_allow_zero",
                flag(n, "issueEstimationAllowZero"),
            ),
            (
                "issue_estimation_extended",
                flag(n, "issueEstimationExtended"),
            ),
            ("default_issue_estimate", real(n, "defaultIssueEstimate")),
            ("cycles_enabled", flag(n, "cyclesEnabled")),
            ("cycle_duration", real(n, "cycleDuration")),
            ("cycle_cooldown_time", real(n, "cycleCooldownTime")),
            ("cycle_start_day", real(n, "cycleStartDay")),
            ("upcoming_cycle_count", real(n, "upcomingCycleCount")),
        ]),
        EntityKind::Users => c.extend([
            ("name", text(n, "name")),
            ("display_name", text(n, "displayName")),
            ("email", text(n, "email")),
            ("active", flag(n, "active")),
            ("admin", flag(n, "admin")),
            ("guest", flag(n, "guest")),
        ]),
        EntityKind::Labels => c.extend([
            ("name", text(n, "name")),
            ("color", text(n, "color")),
            ("is_group", flag(n, "isGroup")),
            ("parent_id", nested_id(n, "parent")),
            ("team_id", nested_id(n, "team")),
        ]),
        EntityKind::Projects => {
            let team_ids: Vec<&str> = n["teams"]["nodes"]
                .as_array()
                .map(|a| a.iter().filter_map(|t| t["id"].as_str()).collect())
                .unwrap_or_default();
            let team_ids = serde_json::to_string(&team_ids)
                .map_err(|e| TgaError::ValidationError(format!("project {id} team ids: {e}")))?;
            c.extend([
                ("name", text(n, "name")),
                ("slug_id", text(n, "slugId")),
                ("url", text(n, "url")),
                ("state", text(n, "state")),
                ("status_name", text(&n["status"], "name")),
                ("status_type", text(&n["status"], "type")),
                ("progress", real(n, "progress")),
                ("start_date", text(n, "startDate")),
                ("target_date", text(n, "targetDate")),
                ("started_at", text(n, "startedAt")),
                ("completed_at", text(n, "completedAt")),
                ("canceled_at", text(n, "canceledAt")),
                ("lead_id", nested_id(n, "lead")),
                ("team_ids", Value::Text(team_ids)),
            ]);
        }
        EntityKind::Milestones => c.extend([
            ("project_id", nested_id(n, "project")),
            ("name", text(n, "name")),
            ("target_date", text(n, "targetDate")),
            ("sort_order", real(n, "sortOrder")),
        ]),
        EntityKind::Cycles => {
            let scope = last(n, "issueCountHistory");
            // #190: zero latest scope is flagged empty; no history yet is
            // unknown (NULL), not empty.
            let is_empty = scope.map_or(Value::Null, |s| Value::Integer(i64::from(s == 0.0)));
            c.extend([
                ("team_id", nested_id(n, "team")),
                ("number", int(n["number"].as_f64())),
                ("name", text(n, "name")),
                ("starts_at", text(n, "startsAt")),
                ("ends_at", text(n, "endsAt")),
                ("completed_at", text(n, "completedAt")),
                ("progress", real(n, "progress")),
                ("scope_count", int(scope)),
                (
                    "completed_count",
                    int(last(n, "completedIssueCountHistory")),
                ),
                (
                    "scope_estimate",
                    last(n, "scopeHistory").map_or(Value::Null, Value::Real),
                ),
                (
                    "completed_estimate",
                    last(n, "completedScopeHistory").map_or(Value::Null, Value::Real),
                ),
                ("is_empty", is_empty),
            ]);
        }
    }
    c.extend([
        ("created_at", text(n, "createdAt")),
        ("updated_at", text(n, "updatedAt")),
        ("archived_at", text(n, "archivedAt")),
    ]);
    Ok(c)
}

//! `fact_notion_activity` and `notion_sync_cursor` reads and writes (#252).
//!
//! Why: the cursor must move only with the events it covers, or a failed
//! write would skip those pages for good. #252: the pipeline calls
//! `persist` once per batch, so this holds per batch, not per sync.
//! What: [`read_watermark`] and [`persist`]; `persist` upserts the events by
//! `event_id` and writes the cursor in ONE transaction, so a failure leaves
//! both unchanged. The cursor never moves backward.
//! Test: `persist_is_idempotent_and_keeps_the_cursor_monotonic`, and the
//! pipeline's `a_failed_write_leaves_the_cursor_unchanged`.

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};

use super::client::parse_ts;
use super::error::NotionError;
use super::events::{rfc3339, ActivityEvent};

/// The stored watermark for `workspace`, `None` before the first sync.
///
/// # Errors
///
/// [`NotionError::Db`] on a read failure, [`NotionError::Timestamp`] on a
/// stored value that is not RFC 3339.
pub fn read_watermark(
    conn: &Connection,
    workspace: &str,
) -> Result<Option<DateTime<Utc>>, NotionError> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT last_edited_watermark FROM notion_sync_cursor WHERE workspace_id = ?1",
            params![workspace],
            |row| row.get(0),
        )
        .optional()?;
    raw.as_deref().map(parse_ts).transpose()
}

/// Upsert `events` and advance the cursor to `new_watermark`, atomically.
///
/// The cursor is written only when `new_watermark` is `Some`, and to the
/// later of it and the stored value. Returns the number of event rows
/// written.
///
/// # Errors
///
/// [`NotionError::Db`]; the transaction is rolled back, so neither the
/// events nor the cursor change.
pub fn persist(
    conn: &mut Connection,
    workspace: &str,
    events: &[ActivityEvent],
    new_watermark: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<usize, NotionError> {
    let synced_at = rfc3339(now);
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare(
            "INSERT INTO fact_notion_activity \
               (event_id, kind, page_id, workspace_id, actor_id, actor_email, actor_name, \
                occurred_at, iso_year, iso_week, body_len, synced_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12) \
             ON CONFLICT(event_id) DO UPDATE SET \
               actor_id = excluded.actor_id, \
               actor_email = excluded.actor_email, \
               actor_name = excluded.actor_name, \
               body_len = excluded.body_len, \
               synced_at = excluded.synced_at",
        )?;
        for e in events {
            stmt.execute(params![
                e.event_id,
                e.kind.as_str(),
                e.page_id,
                workspace,
                e.actor_id,
                e.actor_email,
                e.actor_name,
                rfc3339(e.occurred_at),
                e.iso_year,
                e.iso_week,
                e.body_len,
                synced_at,
            ])?;
        }
    }
    if let Some(mark) = new_watermark {
        let stored: Option<String> = tx
            .query_row(
                "SELECT last_edited_watermark FROM notion_sync_cursor WHERE workspace_id = ?1",
                params![workspace],
                |row| row.get(0),
            )
            .optional()?;
        let stored = stored.as_deref().map(parse_ts).transpose()?;
        let next = stored.map_or(mark, |s| s.max(mark));
        tx.execute(
            "INSERT INTO notion_sync_cursor (workspace_id, last_edited_watermark, synced_at) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT(workspace_id) DO UPDATE SET \
               last_edited_watermark = excluded.last_edited_watermark, \
               synced_at = excluded.synced_at",
            params![workspace, rfc3339(next), synced_at],
        )?;
    }
    tx.commit()?;
    Ok(events.len())
}

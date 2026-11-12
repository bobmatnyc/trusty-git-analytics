//! The Notion step of `tga collect` (#252).
//!
//! Why: Notion activity is a separate per-person weekly signal. It must
//! never be summed into effort, and a failed sync must neither report
//! success nor move the cursor past pages it did not store.
//! What: [`fetch_and_store_notion_activity`] runs only when `config.notion`
//! is set. [`sync_workspace`] reads the cursor, fetches pages edited since
//! it, the comments on those pages and the users, builds the events, and
//! persists events and cursor in one transaction. Every failure is recorded
//! with `stats.fail_stage` and returns before any write, or rolls the write
//! back, so the cursor stays where it was (Fail-Open Check).
//! Test: `pipeline_tests` — `a_failed_fetch_leaves_the_cursor_unchanged`,
//! `a_failed_write_leaves_the_cursor_unchanged`,
//! `sync_stores_events_and_advances_the_cursor`.

use chrono::Utc;
use tracing::info;

use super::client::{parse_ts, NotionClient};
use super::error::NotionError;
use super::events::{build_events, ActorDirectory};
use super::store;
use super::wire::{CommentMeta, NotionUser, PageMeta};
use crate::collect::collector::CollectionStats;
use crate::collect::identity::IdentityResolver;
use crate::core::config::Config;
use crate::core::db::Database;

/// What one completed workspace sync wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotionSyncOutcome {
    /// Event rows upserted.
    pub events: usize,
    /// Distinct actors left with a NULL email (bots, no email, unlisted).
    pub unresolved_actors: usize,
}

/// Run the Notion step when `config.notion` is set; no-op otherwise.
///
/// Failures land in `stats` as stage failures; nothing here aborts the run.
pub(crate) async fn fetch_and_store_notion_activity(
    db: &mut Database,
    config: &Config,
    resolver: &IdentityResolver,
    stats: &mut CollectionStats,
) {
    let Some(cfg) = &config.notion else {
        return;
    };
    let client = match NotionClient::new(cfg) {
        Ok(c) => c,
        Err(e) => {
            stats.fail_stage(format!("Notion client init failed: {e}"));
            return;
        }
    };
    sync_workspace(db, &client, cfg.workspace_label(), resolver, stats).await;
}

/// Sync one workspace; see the module doc for the failure contract.
pub(crate) async fn sync_workspace(
    db: &mut Database,
    client: &NotionClient,
    workspace: &str,
    resolver: &IdentityResolver,
    stats: &mut CollectionStats,
) -> Option<NotionSyncOutcome> {
    match try_sync(db, client, workspace, resolver).await {
        Ok(outcome) => {
            info!(
                workspace,
                events = outcome.events,
                unresolved_actors = outcome.unresolved_actors,
                "Notion activity stored"
            );
            stats.notion_events += outcome.events;
            stats.notion_unresolved_actors += outcome.unresolved_actors;
            Some(outcome)
        }
        Err(e) => {
            // #252 Fail-Open Check: the cursor is untouched on every arm
            // that reaches here — fetch errors return before `persist`, and
            // a `persist` error rolls its transaction back.
            stats.fail_stage(format!("Notion sync failed for workspace {workspace}: {e}"));
            None
        }
    }
}

async fn try_sync(
    db: &mut Database,
    client: &NotionClient,
    workspace: &str,
    resolver: &IdentityResolver,
) -> Result<NotionSyncOutcome, NotionError> {
    let watermark = store::read_watermark(db.connection(), workspace)?;
    let Fetched {
        pages,
        comments,
        users,
    } = fetch_all(client, watermark).await?;

    let new_watermark = pages
        .iter()
        .map(|p| parse_ts(&p.last_edited_time))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max();
    let actors = ActorDirectory::build(&users, resolver);
    let (events, unresolved_actors) = build_events(&pages, &comments, &actors)?;
    let written = store::persist(
        db.connection_mut(),
        workspace,
        &events,
        new_watermark,
        Utc::now(),
    )?;
    Ok(NotionSyncOutcome {
        events: written,
        unresolved_actors,
    })
}

/// Everything one sync reads, fetched before anything is written.
struct Fetched {
    pages: Vec<PageMeta>,
    comments: Vec<CommentMeta>,
    users: Vec<NotionUser>,
}

/// Fetch everything before writing anything, so a fetch error leaves the
/// database exactly as it was.
async fn fetch_all(
    client: &NotionClient,
    watermark: Option<chrono::DateTime<Utc>>,
) -> Result<Fetched, NotionError> {
    let pages = client.search_pages_since(watermark).await?;
    let mut comments = Vec::new();
    for p in &pages {
        comments.extend(client.list_comments(&p.id).await?);
    }
    let users = if pages.is_empty() {
        Vec::new()
    } else {
        client.list_users().await?
    };
    Ok(Fetched {
        pages,
        comments,
        users,
    })
}

//! The Notion step of `tga collect` (#252).
//!
//! Why: Notion activity is a separate per-person weekly signal. It must
//! never be summed into effort, and a failed sync must neither report
//! success nor move the cursor past pages it did not store. A first sync
//! reads the whole workspace and can run for most of an hour, so one late
//! failure must not discard everything fetched before it.
//! What: [`fetch_and_store_notion_activity`] runs only when `config.notion`
//! is set. [`sync_workspace`] reads the cursor, searches pages edited since
//! it, lists the users, then walks the pages oldest edit first, fetching
//! comments and committing events plus cursor in one transaction per batch
//! of [`BATCH_PAGES`] pages. A failure records `stats.fail_stage`; batches
//! already committed stay, and the cursor never passes a page whose events
//! were not stored (Fail-Open Check). A rerun resumes after the last batch.
//! Test: `pipeline_tests` — `a_failed_fetch_leaves_the_cursor_unchanged`,
//! `a_failed_write_leaves_the_cursor_unchanged`,
//! `sync_stores_events_and_advances_the_cursor`,
//! `a_persistent_failure_keeps_earlier_batches_and_stops_the_cursor_there`,
//! `a_rerun_after_a_failure_resumes_without_duplicates`.

use std::collections::HashSet;

use chrono::Utc;
use tracing::{debug, info, warn};

use super::client::{parse_ts, NotionClient};
use super::error::NotionError;
use super::events::{build_events, people_without_any_email, unresolved_actor_ids, ActorDirectory};
use super::store;
use crate::collect::collector::CollectionStats;
use crate::collect::identity::IdentityResolver;
use crate::core::config::Config;
use crate::core::db::Database;

/// Pages per committed batch (#252).
pub(crate) const BATCH_PAGES: usize = 100;
/// Pages between two comments progress lines (#252).
const PROGRESS_EVERY: usize = 100;

/// What one completed workspace sync wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NotionSyncOutcome {
    /// Event rows upserted.
    pub events: usize,
    /// Distinct actors left with a NULL email (bots, no email, unlisted).
    pub unresolved_actors: usize,
    /// `/v1/users` listed people but none with an email: the token most
    /// likely lacks the "read user information including email addresses"
    /// capability (#252).
    pub email_capability_missing: bool,
    /// Pages whose `/v1/comments` answered 403 or 404; their comments were
    /// skipped, their page events stored (#252).
    pub unreadable_comment_pages: usize,
}

/// The fault text recorded when no listed person carries an email (#252).
pub(crate) const EMAIL_CAPABILITY_MISSING: &str =
    "Notion: /v1/users listed people but none with an email; every actor stays \
     unresolved (NULL actor_email). The token most likely lacks the \"read user \
     information including email addresses\" capability";

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
    let mut progress = Progress::default();
    match try_sync(db, client, workspace, resolver, &mut progress).await {
        Ok(()) => {
            let outcome = progress.outcome();
            info!(
                workspace,
                events = outcome.events,
                unresolved_actors = outcome.unresolved_actors,
                "Notion activity stored"
            );
            stats.notion_events += outcome.events;
            stats.notion_unresolved_actors += outcome.unresolved_actors;
            if outcome.unreadable_comment_pages > 0 {
                // #252: non-fatal — pages and users are stored, only those
                // pages' comments are missing.
                let msg = format!(
                    "Notion: comments unreadable on {} page(s) (HTTP 403/404); those pages' \
                     comments were skipped. The token most likely lacks the \"read \
                     comments\" capability, or cannot read comments on those pages \
                     (workspace {workspace})",
                    outcome.unreadable_comment_pages
                );
                warn!(workspace, "{msg}");
                stats.skip_item(msg);
            }
            if outcome.email_capability_missing {
                // #252: non-fatal — the events are stored, but identity is not.
                warn!(workspace, "{EMAIL_CAPABILITY_MISSING}");
                stats.skip_item(format!(
                    "{EMAIL_CAPABILITY_MISSING} (workspace {workspace})"
                ));
            }
            Some(outcome)
        }
        Err(e) => {
            // #252 Fail-Open Check: the sync still fails. Batches committed
            // before the error stay stored, and the cursor sits at the last
            // committed batch, never past a page whose events were not
            // stored; a rerun resumes there.
            stats.notion_events += progress.events;
            stats.fail_stage(format!(
                "Notion sync failed for workspace {workspace}: {e} ({} event(s) in {} \
                 batch(es) stored before the failure; the next run resumes after them)",
                progress.events, progress.batches
            ));
            None
        }
    }
}

/// What a sync has stored so far; read on success and on failure.
#[derive(Default)]
struct Progress {
    events: usize,
    batches: usize,
    unresolved: HashSet<String>,
    email_capability_missing: bool,
    unreadable_comment_pages: usize,
}

impl Progress {
    fn outcome(&self) -> NotionSyncOutcome {
        NotionSyncOutcome {
            events: self.events,
            unresolved_actors: self.unresolved.len(),
            email_capability_missing: self.email_capability_missing,
            unreadable_comment_pages: self.unreadable_comment_pages,
        }
    }
}

/// Search, then walk the pages oldest edit first, committing every
/// [`BATCH_PAGES`] pages.
///
/// #252: each batch's events and the cursor move to that batch's newest
/// `last_edited_time` commit in one transaction. Pages are processed in
/// ascending `last_edited_time`, so every page of this search older than the
/// cursor is stored. A page sharing the cursor's timestamp but not yet stored
/// is re-read, because the next search keeps pages at or after
/// `cursor - lookback`. Re-reads upsert by `event_id`.
async fn try_sync(
    db: &mut Database,
    client: &NotionClient,
    workspace: &str,
    resolver: &IdentityResolver,
    progress: &mut Progress,
) -> Result<(), NotionError> {
    let watermark = store::read_watermark(db.connection(), workspace)?;
    let pages = client.search_pages_since(watermark).await?;
    if pages.is_empty() {
        return Ok(());
    }
    // #252: the search answers newest first; the cursor needs oldest first.
    let mut keyed = pages
        .into_iter()
        .map(|p| Ok((parse_ts(&p.last_edited_time)?, p)))
        .collect::<Result<Vec<_>, NotionError>>()?;
    keyed.sort_by_key(|(t, _)| *t);
    let (times, pages): (Vec<_>, Vec<_>) = keyed.into_iter().unzip();
    let total = pages.len();

    let users = client.list_users().await?;
    progress.email_capability_missing = people_without_any_email(&users);
    let actors = ActorDirectory::build(&users, resolver);

    let mut done = 0;
    for (batch, batch_times) in pages.chunks(BATCH_PAGES).zip(times.chunks(BATCH_PAGES)) {
        let mut comments = Vec::new();
        for p in batch {
            match client.list_comments(&p.id).await {
                Ok(c) => comments.extend(c),
                Err(e) if e.is_access_denied() => {
                    debug!(page_id = %p.id, error = %e, "Notion comments unreadable; skipped");
                    progress.unreadable_comment_pages += 1;
                }
                Err(e) => return Err(e),
            }
            done += 1;
            if done % PROGRESS_EVERY == 0 {
                info!("notion: comments page {done} of {total}");
            }
        }
        let (events, _) = build_events(batch, &comments, &actors)?;
        // Sorted ascending: the batch's last time is its newest.
        let mark = batch_times.last().copied();
        let written = store::persist(db.connection_mut(), workspace, &events, mark, Utc::now())?;
        progress.events += written;
        progress.batches += 1;
        progress
            .unresolved
            .extend(unresolved_actor_ids(&events).map(str::to_string));
        debug!(workspace, done, total, written, "Notion batch committed");
    }
    Ok(())
}

//! The Notion step of `tga collect` (#252).
//!
//! Why: Notion activity is a separate per-person weekly signal. It must
//! never be summed into effort, and a failed sync must neither report
//! success nor move the cursor past pages it did not store. A first sync
//! reads the whole workspace and can run for most of an hour, so one late
//! failure must not discard everything fetched before it.
//! What: [`fetch_and_store_notion_activity`] runs only when `config.notion`
//! is set. [`sync_workspace`] reads the cursor, lists the users, searches
//! pages edited since the cursor, then walks the pages oldest edit first,
//! fetching comments and committing events plus cursor in one transaction per batch
//! of [`BATCH_PAGES`] pages. A failure records `stats.fail_stage`; batches
//! already committed stay, and the cursor never passes a page whose events
//! were not stored (Fail-Open Check). A rerun resumes after the last batch.
//! A 403/404 on `/v1/users` (a personal access token cannot list users) is
//! a non-fatal fault: actors stay unresolved and the sync continues.
//! Test: `pipeline_tests` — `a_failed_fetch_leaves_the_cursor_unchanged`,
//! `a_failed_write_leaves_the_cursor_unchanged`,
//! `sync_stores_events_and_advances_the_cursor`,
//! `a_persistent_failure_keeps_earlier_batches_and_stops_the_cursor_there`,
//! `a_rerun_after_a_failure_resumes_without_duplicates`,
//! `a_refused_user_listing_stores_events_with_actors_unresolved`,
//! `a_non_access_user_listing_failure_still_fails_the_sync`.

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
    /// `/v1/users` answered 403 or 404, so no actor was resolved (#252).
    pub users_unlisted: bool,
}

/// The fault text recorded when no listed person carries an email (#252).
pub(crate) const EMAIL_CAPABILITY_MISSING: &str =
    "Notion: /v1/users listed people but none with an email; every actor stays \
     unresolved (NULL actor_email). The token most likely lacks the \"read user \
     information including email addresses\" capability";

// #252: a personal access token gets 403 `restricted_resource` on /v1/users.
/// The fault text recorded when `/v1/users` refuses the listing (#252).
pub(crate) const USERS_UNLISTED: &str =
    "Notion: token cannot list users (/v1/users answered HTTP 403/404); actors unresolved \
     (NULL actor_email). Events are stored without identity";

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
    let result = try_sync(db, client, workspace, resolver, &mut progress).await;
    if let Some(cause) = &progress.users_refused {
        // #252: non-fatal, recorded once whether or not the sync later fails.
        warn!(workspace, "{USERS_UNLISTED}: {cause}");
        stats.skip_item(format!("{USERS_UNLISTED} (workspace {workspace}): {cause}"));
    }
    match result {
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
    /// #252: the `/v1/users` refusal (403/404), when the listing was refused.
    users_refused: Option<String>,
}

impl Progress {
    fn outcome(&self) -> NotionSyncOutcome {
        NotionSyncOutcome {
            events: self.events,
            unresolved_actors: self.unresolved.len(),
            email_capability_missing: self.email_capability_missing,
            unreadable_comment_pages: self.unreadable_comment_pages,
            users_unlisted: self.users_refused.is_some(),
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
    // #252: list users before the search, so a refusal or a failure costs no
    // long page walk first. Only 403/404 degrades to "no users"; any other
    // error (transient ones already retried) still fails (Fail-Open Check).
    // Page and comment payloads carry only the user id, so nothing else can
    // resolve an actor.
    let users = match client.list_users().await {
        Ok(users) => users,
        Err(e) if e.is_access_denied() => {
            progress.users_refused = Some(e.to_string());
            Vec::new()
        }
        Err(e) => return Err(e),
    };
    progress.email_capability_missing = people_without_any_email(&users);
    let actors = ActorDirectory::build(&users, resolver);

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

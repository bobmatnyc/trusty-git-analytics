//! Turn fetched Notion pages and comments into v1 activity events (#252).
//!
//! Why: the stored grain is "one event per actor action", keyed so a
//! re-sync upserts rather than duplicates, and bucketed by ISO week so a
//! reader can count per person per week.
//! What: [`build_events`] emits `page_created`, `page_edited` (at most one per
//! page per sync, a lower bound) and `comment_created`; [`ActorDirectory`]
//! maps a Notion user id to the canonical email through the existing
//! [`IdentityResolver`]. Bots, email-less users and users the token cannot
//! list keep a NULL email and are counted as unresolved.
//! Test: `identity_resolves_people_and_leaves_bots_and_emailless_null`,
//! `iso_week_buckets_follow_iso_8601`, `events_are_keyed_for_idempotent_upsert`.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};

use super::client::parse_ts;
use super::error::NotionError;
use super::wire::{CommentMeta, NotionUser, PageMeta, UserRef};
use crate::collect::identity::IdentityResolver;

/// The three v1 event kinds, as stored in `fact_notion_activity.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    /// A page's `created_by` / `created_time`.
    PageCreated,
    /// A page's `last_edited_by` / `last_edited_time` snapshot.
    PageEdited,
    /// A comment's `created_by` / `created_time`.
    CommentCreated,
}

impl EventKind {
    /// The stored `kind` string.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PageCreated => "page_created",
            Self::PageEdited => "page_edited",
            Self::CommentCreated => "comment_created",
        }
    }
}

/// One `fact_notion_activity` row, ready to upsert.
#[derive(Debug, Clone, PartialEq)]
pub struct ActivityEvent {
    /// Deterministic key; see the v40 migration header.
    pub event_id: String,
    /// Event kind.
    pub kind: EventKind,
    /// The page the event belongs to.
    pub page_id: String,
    /// Notion user id of the actor, when the API named one.
    pub actor_id: Option<String>,
    /// Canonical email; `None` for bots, email-less and unlisted users.
    pub actor_email: Option<String>,
    /// Canonical (or Notion) display name, when known.
    pub actor_name: Option<String>,
    /// When it happened, UTC.
    pub occurred_at: DateTime<Utc>,
    /// ISO 8601 week-numbering year of `occurred_at`.
    pub iso_year: i32,
    /// ISO 8601 week of `occurred_at`.
    pub iso_week: u32,
    /// Comment body length in characters (`comment_created` only).
    pub body_len: Option<i64>,
}

/// `(iso_year, iso_week)` of a UTC instant.
pub fn iso_week_of(t: DateTime<Utc>) -> (i32, u32) {
    let w = t.iso_week();
    (w.year(), w.week())
}

/// A resolved actor: canonical email (if any) and display name (if any).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Actor {
    /// Canonical email, `None` when unresolved.
    pub email: Option<String>,
    /// Display name.
    pub name: Option<String>,
}

/// Notion user id → resolved [`Actor`].
pub struct ActorDirectory {
    actors: HashMap<String, Actor>,
}

impl ActorDirectory {
    /// Resolve every listed user once.
    ///
    /// A `person` with a non-empty email goes through
    /// [`IdentityResolver::resolve`] (as `feed_azdo_users` feeds ADO users),
    /// and the canonical email it returns is kept; an empty canonical email
    /// falls back to the Notion email. A bot, or a person with no email,
    /// keeps its name and a `None` email.
    pub fn build(users: &[NotionUser], resolver: &IdentityResolver) -> Self {
        let mut actors = HashMap::with_capacity(users.len());
        for u in users {
            let name = u.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
            let email = if u.kind.as_deref() == Some("person") {
                u.person
                    .as_ref()
                    .and_then(|p| p.email.as_deref())
                    .map(str::trim)
                    .filter(|e| !e.is_empty())
            } else {
                None
            };
            let actor = match email {
                Some(raw) => {
                    let (canon_name, canon_email) = resolver.resolve(name.unwrap_or(""), raw);
                    let canon_email = canon_email.trim();
                    let email = if canon_email.is_empty() {
                        raw
                    } else {
                        canon_email
                    };
                    let name = if canon_name.trim().is_empty() {
                        name.map(str::to_string)
                    } else {
                        Some(canon_name)
                    };
                    Actor {
                        email: Some(email.to_string()),
                        name,
                    }
                }
                None => Actor {
                    email: None,
                    name: name.map(str::to_string),
                },
            };
            actors.insert(u.id.clone(), actor);
        }
        Self { actors }
    }

    /// The actor for a user reference; unlisted ids resolve to nothing.
    pub fn lookup(&self, user: Option<&UserRef>) -> (Option<String>, Actor) {
        match user {
            Some(u) => (
                Some(u.id.clone()),
                self.actors.get(&u.id).cloned().unwrap_or_default(),
            ),
            None => (None, Actor::default()),
        }
    }
}

/// Build the v1 events for one sync.
///
/// Every page yields one `page_created` and one `page_edited` (keyed by its
/// current `last_edited_time`, so the same snapshot is stored once); every
/// comment yields one `comment_created`. Returns the events and the number of
/// distinct actors whose email stayed unresolved.
///
/// # Errors
///
/// [`NotionError::Timestamp`] for a timestamp that is not RFC 3339.
pub fn build_events(
    pages: &[PageMeta],
    comments: &[CommentMeta],
    actors: &ActorDirectory,
) -> Result<(Vec<ActivityEvent>, usize), NotionError> {
    let mut events = Vec::with_capacity(pages.len() * 2 + comments.len());
    for p in pages {
        let created = parse_ts(&p.created_time)?;
        events.push(event(
            format!("page_created:{}", p.id),
            EventKind::PageCreated,
            &p.id,
            actors.lookup(p.created_by.as_ref()),
            created,
            None,
        ));
        let edited = parse_ts(&p.last_edited_time)?;
        events.push(event(
            format!("page_edited:{}:{}", p.id, rfc3339(edited)),
            EventKind::PageEdited,
            &p.id,
            actors.lookup(p.last_edited_by.as_ref()),
            edited,
            None,
        ));
    }
    for c in comments {
        let created = parse_ts(&c.created_time)?;
        events.push(event(
            format!("comment_created:{}", c.id),
            EventKind::CommentCreated,
            &c.page_id,
            actors.lookup(c.created_by.as_ref()),
            created,
            Some(i64::try_from(c.body_len).unwrap_or(i64::MAX)),
        ));
    }
    let unresolved = events
        .iter()
        .filter(|e| e.actor_email.is_none())
        .filter_map(|e| e.actor_id.as_deref())
        .collect::<HashSet<&str>>()
        .len();
    Ok((events, unresolved))
}

/// RFC 3339, whole seconds, `Z` — the stored form of every timestamp.
pub fn rfc3339(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn event(
    event_id: String,
    kind: EventKind,
    page_id: &str,
    (actor_id, actor): (Option<String>, Actor),
    occurred_at: DateTime<Utc>,
    body_len: Option<i64>,
) -> ActivityEvent {
    let (iso_year, iso_week) = iso_week_of(occurred_at);
    ActivityEvent {
        event_id,
        kind,
        page_id: page_id.to_string(),
        actor_id,
        actor_email: actor.email,
        actor_name: actor.name,
        occurred_at,
        iso_year,
        iso_week,
        body_len,
    }
}

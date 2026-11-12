//! Notion activity as a per-person weekly signal (#252).
//!
//! Why: Notion pages and comments are engineering activity beside git, PRs
//! and tickets. They are stored as their own signal in
//! `fact_notion_activity` and never summed into effort or any existing
//! volume metric.
//! What: the client (`/v1/search`, `/v1/comments`, `/v1/users`), the v1
//! event builder (`page_created`, `page_edited` as a lower bound,
//! `comment_created` with body length only), the store, and the collect
//! step. Opt-in through the `notion:` config block.
//! Test: `client_tests` and `pipeline_tests` against a local mock server; no
//! test calls the live API.
//!
//! Not measurable through the public API, and so not recorded: per-edit
//! history, editors other than the last one, page views, resolved comments,
//! pages not shared with the integration, and history before the first sync.

mod client;
mod error;
mod events;
mod pipeline;
mod store;
mod wire;

#[cfg(test)]
mod client_tests;
#[cfg(test)]
mod pipeline_tests;

pub use client::NotionClient;
pub use error::NotionError;
pub use events::{iso_week_of, ActivityEvent, Actor, ActorDirectory, EventKind};
pub use pipeline::NotionSyncOutcome;
pub use wire::{CommentMeta, NotionUser, PageMeta, PersonWire, UserRef};

pub(crate) use pipeline::fetch_and_store_notion_activity;

//! Pipeline, store and event tests (#252), against wiremock and an
//! in-memory database. No live Notion call.

use chrono::{TimeZone, Utc};
use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::client_tests::{client, list, page};
use super::events::{build_events, iso_week_of, ActorDirectory, EventKind};
use super::pipeline::sync_workspace;
use super::store;
use super::wire::{CommentMeta, NotionUser, PageMeta, PersonWire, UserRef};
use crate::collect::collector::CollectionStats;
use crate::collect::identity::IdentityResolver;
use crate::core::db::Database;

const WS: &str = "acme";

fn users_body() -> serde_json::Value {
    list(
        vec![
            json!({"object": "user", "id": "u1", "type": "person", "name": "Ada L",
                   "person": {"email": "ada@personal.example"}}),
            json!({"object": "user", "id": "bot1", "type": "bot", "name": "Zapier", "bot": {}}),
        ],
        None,
    )
}

/// Mount a workspace with one page `p1` edited at `edited`, one comment on
/// it, and the users list. `comments_status` lets a test fail the comments
/// fetch.
async fn mount(server: &MockServer, edited: &str, comments_status: u16) {
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![page("p1", "2026-11-02T09:00:00.000Z", edited, "u1")],
            None,
        )))
        .mount(server)
        .await;
    let comments = if comments_status == 200 {
        ResponseTemplate::new(200).set_body_json(list(
            vec![json!({"object": "comment", "id": "c1",
                        "created_time": "2026-11-03T10:00:00.000Z",
                        "created_by": {"object": "user", "id": "bot1"},
                        "rich_text": [{"plain_text": "four"}]})],
            None,
        ))
    } else {
        ResponseTemplate::new(comments_status)
    };
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .respond_with(comments)
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(server)
        .await;
}

fn watermark(db: &Database) -> Option<String> {
    store::read_watermark(db.connection(), WS)
        .expect("read")
        .map(super::events::rfc3339)
}

fn event_count(db: &Database) -> i64 {
    db.connection()
        .query_row("SELECT COUNT(*) FROM fact_notion_activity", [], |r| {
            r.get(0)
        })
        .expect("count")
}

#[tokio::test]
async fn sync_stores_events_and_advances_the_cursor() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    let mut db = Database::open_in_memory().expect("db");
    let resolver = IdentityResolver::new(None);
    let mut stats = CollectionStats::default();

    let out = sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats)
        .await
        .expect("synced");
    assert!(stats.stage_failures().is_empty(), "{:?}", stats.errors);
    // page_created + page_edited + comment_created.
    assert_eq!(out.events, 3);
    assert_eq!(out.unresolved_actors, 1, "the bot");
    assert_eq!(stats.notion_events, 3);
    assert_eq!(stats.notion_unresolved_actors, 1);
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));

    // A re-sync of the same data upserts: no new rows.
    sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats).await;
    assert_eq!(event_count(&db), 3, "re-sync is idempotent");

    let (body_len, email): (Option<i64>, Option<String>) = db
        .connection()
        .query_row(
            "SELECT body_len, actor_email FROM fact_notion_activity \
             WHERE kind = 'comment_created'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .expect("comment row");
    assert_eq!(body_len, Some(4));
    assert_eq!(email, None, "a bot keeps a NULL email");
}

/// #252 Fail-Open Check: a fetch failure must record a stage failure and
/// must NOT advance `notion_sync_cursor`. Fails if the cursor moves to the
/// second sync's newer page, or if any of its events are written.
#[tokio::test]
async fn a_failed_fetch_leaves_the_cursor_unchanged() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    let mut db = Database::open_in_memory().expect("db");
    let resolver = IdentityResolver::new(None);
    let mut stats = CollectionStats::default();
    sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats).await;
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));
    let before = event_count(&db);

    // The page is edited again, but its comments fetch now fails.
    server.reset().await;
    mount(&server, "2026-11-12T08:00:00.000Z", 500).await;
    let mut stats = CollectionStats::default();
    let out = sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats).await;

    assert!(out.is_none());
    assert_eq!(stats.stage_failures().len(), 1, "{:?}", stats.errors);
    assert_eq!(
        watermark(&db).as_deref(),
        Some("2026-11-10T10:00:00Z"),
        "the cursor must not advance past a failed fetch"
    );
    assert_eq!(event_count(&db), before, "nothing from the failed sync");
    assert_eq!(stats.notion_events, 0);
}

/// #252 Fail-Open Check, write arm: a failed persist rolls back and leaves
/// no cursor row.
#[tokio::test]
async fn a_failed_write_leaves_the_cursor_unchanged() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    let mut db = Database::open_in_memory().expect("db");
    db.connection()
        .execute_batch("DROP TABLE fact_notion_activity")
        .expect("drop");
    let mut stats = CollectionStats::default();
    let out = sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await;

    assert!(out.is_none());
    assert_eq!(stats.stage_failures().len(), 1, "{:?}", stats.errors);
    assert_eq!(watermark(&db), None, "no cursor after a failed write");
}

fn user(id: &str, kind: &str, name: &str, email: Option<&str>) -> NotionUser {
    NotionUser {
        id: id.into(),
        kind: Some(kind.into()),
        name: Some(name.into()),
        person: email.map(|e| PersonWire {
            email: Some(e.into()),
        }),
    }
}

fn page_by(id: &str, by: &str) -> PageMeta {
    PageMeta {
        object: "page".into(),
        id: id.into(),
        created_time: "2026-11-02T09:00:00.000Z".into(),
        last_edited_time: "2026-11-10T10:00:00.000Z".into(),
        created_by: Some(UserRef { id: by.into() }),
        last_edited_by: Some(UserRef { id: by.into() }),
    }
}

#[test]
fn identity_resolves_people_and_leaves_bots_and_emailless_null() {
    let mut resolver = IdentityResolver::new(None);
    resolver.add_alias("ada@corp.example", "Ada Lovelace");
    resolver.add_alias("ada@personal.example", "Ada Lovelace");
    let users = vec![
        user("u1", "person", "Ada L", Some("ada@personal.example")),
        user("u2", "person", "Grace", Some("grace@corp.example")),
        user("bot1", "bot", "Zapier", None),
        user("u3", "person", "No Mail", None),
    ];
    let actors = ActorDirectory::build(&users, &resolver);
    let pages = ["u1", "u2", "bot1", "u3", "unlisted"]
        .iter()
        .enumerate()
        .map(|(i, by)| page_by(&format!("p{i}"), by))
        .collect::<Vec<_>>();
    let (events, unresolved) = build_events(&pages, &[], &actors).expect("events");

    let email_of = |page: &str| {
        events
            .iter()
            .find(|e| e.page_id == page && e.kind == EventKind::PageCreated)
            .and_then(|e| e.actor_email.clone())
    };
    assert_eq!(
        email_of("p0").as_deref(),
        Some("ada@corp.example"),
        "resolved to canonical"
    );
    assert_eq!(email_of("p1").as_deref(), Some("grace@corp.example"));
    assert_eq!(email_of("p2"), None, "bot");
    assert_eq!(email_of("p3"), None, "no email");
    assert_eq!(email_of("p4"), None, "not in /v1/users");
    assert_eq!(unresolved, 3, "bot, no-email and unlisted actors");
}

#[test]
fn iso_week_buckets_follow_iso_8601() {
    let at = |y, m, d| Utc.with_ymd_and_hms(y, m, d, 12, 0, 0).single().expect("t");
    assert_eq!(
        iso_week_of(at(2026, 1, 1)),
        (2026, 1),
        "Thursday opens week 1"
    );
    assert_eq!(
        iso_week_of(at(2027, 1, 1)),
        (2026, 53),
        "Friday: last ISO year"
    );
    assert_eq!(
        iso_week_of(at(2024, 12, 30)),
        (2025, 1),
        "Monday: next ISO year"
    );
    assert_eq!(iso_week_of(at(2026, 11, 12)), (2026, 46));
}

#[test]
fn events_are_keyed_for_idempotent_upsert() {
    let actors = ActorDirectory::build(&[], &IdentityResolver::new(None));
    let comment = CommentMeta {
        id: "c9".into(),
        page_id: "p0".into(),
        created_time: "2026-11-03T10:00:00.000Z".into(),
        created_by: None,
        body_len: 7,
    };
    let (events, _) = build_events(&[page_by("p0", "u1")], &[comment], &actors).expect("events");
    let ids: Vec<&str> = events.iter().map(|e| e.event_id.as_str()).collect();
    assert_eq!(
        ids,
        [
            "page_created:p0",
            "page_edited:p0:2026-11-10T10:00:00Z",
            "comment_created:c9"
        ]
    );
    assert_eq!(events[2].body_len, Some(7));
    assert_eq!((events[0].iso_year, events[0].iso_week), (2026, 45));
}

#[test]
fn persist_is_idempotent_and_keeps_the_cursor_monotonic() {
    let mut db = Database::open_in_memory().expect("db");
    let actors = ActorDirectory::build(&[], &IdentityResolver::new(None));
    let (events, _) = build_events(&[page_by("p0", "u1")], &[], &actors).expect("events");
    let late = Utc.with_ymd_and_hms(2026, 11, 10, 10, 0, 0).single();
    let early = Utc.with_ymd_and_hms(2026, 11, 1, 0, 0, 0).single();
    let now = Utc::now();
    let conn = db.connection_mut();
    store::persist(conn, WS, &events, late, now).expect("first");
    store::persist(conn, WS, &events, early, now).expect("second");
    assert_eq!(event_count(&db), 2);
    assert_eq!(
        watermark(&db).as_deref(),
        Some("2026-11-10T10:00:00Z"),
        "never backward"
    );
}

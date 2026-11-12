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

/// #252 HIGH: Notion search is indexed asynchronously, so a page edited just
/// before the stored watermark can first appear after the sync that set it.
/// The walk re-reads a lookback window below the watermark; without it this
/// page is skipped forever.
#[tokio::test]
async fn a_late_indexed_page_below_the_watermark_is_collected() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    let mut db = Database::open_in_memory().expect("db");
    let resolver = IdentityResolver::new(None);
    let mut stats = CollectionStats::default();
    sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats)
        .await
        .expect("first sync");
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));

    // p2 was edited at 09:30, before the watermark, but indexed only now.
    server.reset().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                page(
                    "p1",
                    "2026-11-02T09:00:00.000Z",
                    "2026-11-10T10:00:00.000Z",
                    "u1",
                ),
                page(
                    "p2",
                    "2026-11-10T09:00:00.000Z",
                    "2026-11-10T09:30:00.000Z",
                    "u1",
                ),
            ],
            None,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), None)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(&server)
        .await;
    let mut stats = CollectionStats::default();
    sync_workspace(&mut db, &client(&server), WS, &resolver, &mut stats).await;

    assert!(stats.stage_failures().is_empty(), "{:?}", stats.errors);
    let p2_rows: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_notion_activity WHERE page_id = 'p2'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(
        p2_rows, 2,
        "p2's page_created and page_edited are collected"
    );
    assert_eq!(
        watermark(&db).as_deref(),
        Some("2026-11-10T10:00:00Z"),
        "the cursor never moves backward"
    );
}

/// #252: people listed with no email means the token lacks the email
/// capability. The run stores its events, warns, and records a non-fatal
/// fault naming the capability.
#[tokio::test]
async fn people_without_emails_report_the_missing_capability() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![page(
                "p1",
                "2026-11-02T09:00:00.000Z",
                "2026-11-10T10:00:00.000Z",
                "u1",
            )],
            None,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), None)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                json!({"object": "user", "id": "u1", "type": "person", "name": "Ada L",
                       "person": {}}),
                json!({"object": "user", "id": "bot1", "type": "bot", "name": "Zapier", "bot": {}}),
            ],
            None,
        )))
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    let out = sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await
    .expect("the sync itself succeeds");

    assert!(out.email_capability_missing);
    assert!(
        stats.stage_failures().is_empty(),
        "non-fatal: {:?}",
        stats.errors
    );
    assert!(
        stats.errors.iter().any(
            |f| !f.is_stage_failure() && f.message.contains("user information including email")
        ),
        "a non-fatal fault names the capability: {:?}",
        stats.errors
    );
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));
}

#[test]
fn email_capability_check_needs_people_and_no_email() {
    use super::events::people_without_any_email;
    let with_email = user("u1", "person", "Ada", Some("ada@corp.example"));
    let without = user("u2", "person", "Bo", None);
    let bot = user("bot1", "bot", "Zapier", None);
    assert!(people_without_any_email(&[without.clone(), bot.clone()]));
    assert!(!people_without_any_email(&[with_email, without]));
    assert!(!people_without_any_email(&[bot]), "bots alone say nothing");
    assert!(!people_without_any_email(&[]));
}

/// Mount a search with `p1` (comments answer `p1_status`) and `p2` (one
/// readable comment), plus the users list.
async fn mount_two_pages(server: &MockServer, p1_status: u16) {
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                page(
                    "p1",
                    "2026-11-02T09:00:00.000Z",
                    "2026-11-10T10:00:00.000Z",
                    "u1",
                ),
                page(
                    "p2",
                    "2026-11-03T09:00:00.000Z",
                    "2026-11-09T10:00:00.000Z",
                    "u1",
                ),
            ],
            None,
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .respond_with(ResponseTemplate::new(p1_status).set_body_json(json!({
            "object": "error", "status": p1_status,
            "code": "object_not_found", "message": "Could not find block with ID: p1."
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![json!({"object": "comment", "id": "c2",
                        "created_time": "2026-11-09T11:00:00.000Z",
                        "created_by": {"object": "user", "id": "u1"},
                        "rich_text": [{"plain_text": "ok"}]})],
            None,
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(server)
        .await;
}

/// #252 live-check failure: one page's comments answering `status` must skip
/// that page's comments only. Page events of both pages, the readable page's
/// comment, a non-fatal fault counting 1 page, and an advanced cursor.
async fn assert_unreadable_comments_are_skipped(status: u16) {
    let server = MockServer::start().await;
    mount_two_pages(&server, status).await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await;

    assert!(
        stats.stage_failures().is_empty(),
        "HTTP {status} on one page's comments must not fail the sync: {:?}",
        stats.errors
    );
    let count = |sql: &str| -> i64 {
        db.connection()
            .query_row(sql, [], |r| r.get(0))
            .expect("count")
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM fact_notion_activity WHERE kind IN ('page_created','page_edited')"),
        4,
        "page events of both pages"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM fact_notion_activity WHERE kind = 'comment_created'"),
        1,
        "the readable page's comment"
    );
    let faults: Vec<&str> = stats
        .errors
        .iter()
        .filter(|f| !f.is_stage_failure() && f.message.contains("comments unreadable"))
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(faults.len(), 1, "{:?}", stats.errors);
    assert!(faults[0].contains("on 1 page(s)"), "{}", faults[0]);
    assert!(faults[0].contains("read comments"), "{}", faults[0]);
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));
}

#[tokio::test]
async fn a_404_on_one_pages_comments_skips_only_those_comments() {
    assert_unreadable_comments_are_skipped(404).await;
}

#[tokio::test]
async fn a_403_on_one_pages_comments_skips_only_those_comments() {
    assert_unreadable_comments_are_skipped(403).await;
}

/// #252: a database (or any non-page) search result is skipped, and its id
/// never reaches `/v1/comments`.
#[tokio::test]
async fn a_database_search_result_is_skipped_and_never_commented() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![
                json!({"object": "database", "id": "db1",
                       "created_time": "2026-11-10T09:00:00.000Z",
                       "last_edited_time": "2026-11-11T10:00:00.000Z"}),
                page(
                    "p1",
                    "2026-11-02T09:00:00.000Z",
                    "2026-11-10T10:00:00.000Z",
                    "u1",
                ),
            ],
            None,
        )))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "db1"))
        .respond_with(ResponseTemplate::new(404))
        .expect(0)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), None)))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await;

    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    let db_rows: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_notion_activity WHERE page_id = 'db1'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(db_rows, 0, "no events for a database object");
    assert_eq!(
        watermark(&db).as_deref(),
        Some("2026-11-10T10:00:00Z"),
        "the database's later edit time does not set the cursor"
    );
}

/// #252 (a): `/v1/comments` answers 503, then 502, then 200. The retries
/// carry the sync to completion with every event stored.
#[tokio::test]
async fn transient_comment_failures_are_retried_and_the_sync_completes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&calls);
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(
            move |_: &wiremock::Request| match seen.fetch_add(1, Ordering::SeqCst) {
                0 => ResponseTemplate::new(503),
                1 => ResponseTemplate::new(502),
                _ => ResponseTemplate::new(200).set_body_json(list(
                    vec![json!({"object": "comment", "id": "c1",
                                "created_time": "2026-11-03T10:00:00.000Z",
                                "created_by": {"object": "user", "id": "u1"},
                                "rich_text": [{"plain_text": "ok"}]})],
                    None,
                )),
            },
        )
        .with_priority(1)
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    let out = sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await
    .expect("the sync completes");

    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    assert_eq!(calls.load(Ordering::SeqCst), 3, "two failures, one success");
    assert_eq!(out.events, 3, "page_created, page_edited, comment_created");
    assert_eq!(event_count(&db), 3);
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));
}

/// Pages in the many-page workspace below.
const MANY: usize = 250;
/// The page whose comments fail in the failing run: the first page of the
/// third batch, in ascending edit order.
const FAILING: usize = 201;

/// `last_edited_time` of page `i`: one hour per page from 2026-01-01.
fn edited_at(i: usize) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("t")
        + chrono::Duration::hours(i64::try_from(i).expect("small"))
}

/// Mount `MANY` pages, newest first as the API sorts them, empty comments,
/// and the users. `failing`: that page's comments always answer 500.
async fn mount_many(server: &MockServer, failing: Option<usize>) {
    let pages = (1..=MANY)
        .rev()
        .map(|i| {
            let edited = edited_at(i).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            page(
                &format!("p{i:03}"),
                "2025-12-01T00:00:00.000Z",
                &edited,
                "u1",
            )
        })
        .collect();
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(pages, None)))
        .mount(server)
        .await;
    if let Some(i) = failing {
        Mock::given(method("GET"))
            .and(path("/v1/comments"))
            .and(query_param("block_id", format!("p{i:03}").as_str()))
            .respond_with(ResponseTemplate::new(500))
            .with_priority(1)
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), None)))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(200).set_body_json(users_body()))
        .mount(server)
        .await;
}

/// Run one sync against `server` with a fresh stats record.
async fn sync_once(
    db: &mut Database,
    server: &MockServer,
) -> (Option<super::pipeline::NotionSyncOutcome>, CollectionStats) {
    let mut stats = CollectionStats::default();
    let out = sync_workspace(
        db,
        &client(server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await;
    (out, stats)
}

/// #252 (d) Fail-Open Check: page 201's comments fail on every retry. The
/// two batches before it (pages 1-200) stay stored, the cursor stops at
/// page 200's edit time and no further, and the sync reports failure.
#[tokio::test]
async fn a_persistent_failure_keeps_earlier_batches_and_stops_the_cursor_there() {
    let server = MockServer::start().await;
    mount_many(&server, Some(FAILING)).await;
    let mut db = Database::open_in_memory().expect("db");
    let (out, stats) = sync_once(&mut db, &server).await;

    assert!(out.is_none(), "a failed sync never reports success");
    let failures = stats.stage_failures();
    assert_eq!(failures.len(), 1, "{:?}", stats.errors);
    assert!(
        failures[0].message.contains("400 event(s) in 2 batch(es)"),
        "{}",
        failures[0].message
    );
    assert_eq!(event_count(&db), 400, "pages 1-200: created + edited");
    assert_eq!(stats.notion_events, 400, "stored events are reported");
    let stored_pages: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_notion_activity WHERE page_id >= 'p201'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(stored_pages, 0, "nothing from the failed batch");
    assert_eq!(
        watermark(&db),
        Some(super::events::rfc3339(edited_at(FAILING - 1))),
        "the cursor is the last fully stored page, page 200"
    );
}

/// #252 (e): after the (d) failure, a rerun resumes at the cursor — it reads
/// comments only for pages from `cursor - lookback` on, not all 250 — and
/// finishes with every event stored exactly once.
#[tokio::test]
async fn a_rerun_after_a_failure_resumes_without_duplicates() {
    let server = MockServer::start().await;
    mount_many(&server, Some(FAILING)).await;
    let mut db = Database::open_in_memory().expect("db");
    sync_once(&mut db, &server).await;

    server.reset().await;
    mount_many(&server, None).await;
    let (out, stats) = sync_once(&mut db, &server).await;

    assert!(stats.errors.is_empty(), "{:?}", stats.errors);
    assert!(out.is_some());
    let comment_reads = server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .filter(|r| r.url.path() == "/v1/comments")
        .count();
    // Cursor at page 200, lookback 60 min = one page: pages 199-250.
    assert_eq!(comment_reads, 52, "resumed at the cursor, not from zero");
    assert_eq!(event_count(&db), 500, "250 pages x 2 events, no duplicates");
    let distinct: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(DISTINCT page_id || kind) FROM fact_notion_activity",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(distinct, 500);
    assert_eq!(
        watermark(&db),
        Some(super::events::rfc3339(edited_at(MANY)))
    );
}

/// #252 live-check failure: a personal access token gets 403
/// `restricted_resource` on `/v1/users`. The sync must still store its
/// events and advance the cursor, leave every actor unresolved, and record
/// one non-fatal fault naming the cause. Fails on 584206e, where the 403
/// failed the whole stage with 0 events stored.
#[tokio::test]
async fn a_refused_user_listing_stores_events_with_actors_unresolved() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "object": "error", "status": 403, "code": "restricted_resource",
            "message": "Personal access tokens cannot list users."
        })))
        .with_priority(1)
        .expect(1)
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    let mut stats = CollectionStats::default();
    let out = sync_workspace(
        &mut db,
        &client(&server),
        WS,
        &IdentityResolver::new(None),
        &mut stats,
    )
    .await
    .expect("a refused user listing must not fail the sync");

    assert!(
        stats.stage_failures().is_empty(),
        "non-fatal: {:?}",
        stats.errors
    );
    assert!(out.users_unlisted);
    assert!(!out.email_capability_missing, "no people were listed");
    assert_eq!(out.events, 3);
    assert_eq!(out.unresolved_actors, 2, "u1 and bot1");
    assert_eq!(event_count(&db), 3);
    assert_eq!(watermark(&db).as_deref(), Some("2026-11-10T10:00:00Z"));
    let resolved: i64 = db
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM fact_notion_activity WHERE actor_email IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(resolved, 0, "no actor resolves without the user list");
    let faults: Vec<&str> = stats
        .errors
        .iter()
        .filter(|f| !f.is_stage_failure() && f.message.contains("cannot list users"))
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(faults.len(), 1, "{:?}", stats.errors);
    assert!(faults[0].contains("actors unresolved"), "{}", faults[0]);
    assert!(faults[0].contains("restricted_resource"), "{}", faults[0]);
}

/// #252 Fail-Open Check: only 403/404 on `/v1/users` degrades. A 401 still
/// fails the sync, stores nothing, and (users listed first) never starts
/// the page search.
#[tokio::test]
async fn a_non_access_user_listing_failure_still_fails_the_sync() {
    let server = MockServer::start().await;
    mount(&server, "2026-11-10T10:00:00.000Z", 200).await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(401))
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(Vec::new(), None)))
        .with_priority(1)
        .expect(0)
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
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
    assert!(
        !stats
            .errors
            .iter()
            .any(|f| f.message.contains("cannot list users")),
        "a 401 is not a refusal: {:?}",
        stats.errors
    );
    assert_eq!(event_count(&db), 0);
    assert_eq!(watermark(&db), None);
}

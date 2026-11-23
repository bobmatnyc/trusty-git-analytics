//! Tests for the `notion.user_map` id → person mapping (#257).

use std::collections::BTreeMap;

use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::client_tests::{client, list, page};
use super::events::{build_events, ActorDirectory};
use super::pipeline::{sync_workspace_with_map, NotionSyncOutcome};
use super::wire::{PageMeta, UserRef};
use crate::collect::collector::CollectionStats;
use crate::collect::identity::IdentityResolver;
use crate::core::config::NotionConfig;
use crate::core::db::Database;

const WS: &str = "acme";

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

fn resolver() -> IdentityResolver {
    let mut r = IdentityResolver::new(None);
    r.add_alias("ada@corp.example.com", "Ada Lovelace");
    r.add_alias("ada@example.com", "Ada Lovelace");
    r
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

/// A workspace whose token cannot list users (403), with page `p1` by `u1`
/// and one comment on it by `u2`.
async fn mount_unlisted(server: &MockServer) {
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
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/comments"))
        .and(query_param("block_id", "p1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list(
            vec![json!({"object": "comment", "id": "c1",
                        "created_time": "2026-11-03T10:00:00.000Z",
                        "created_by": {"object": "user", "id": "u2"},
                        "rich_text": [{"plain_text": "four"}]})],
            None,
        )))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/users"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "object": "error", "status": 403, "code": "restricted_resource",
            "message": "Personal access tokens cannot list users"})))
        .mount(server)
        .await;
}

async fn sync(
    db: &mut Database,
    server: &MockServer,
    user_map: &BTreeMap<String, String>,
) -> NotionSyncOutcome {
    let mut stats = CollectionStats::default();
    let out =
        sync_workspace_with_map(db, &client(server), WS, user_map, &resolver(), &mut stats).await;
    out.expect("sync succeeds")
}

fn email_of(db: &Database, actor_id: &str) -> Vec<Option<String>> {
    let mut stmt = db
        .connection()
        .prepare(
            "SELECT actor_email FROM fact_notion_activity WHERE actor_id = ?1 ORDER BY event_id",
        )
        .expect("prepare");
    stmt.query_map([actor_id], |r| r.get(0))
        .expect("query")
        .collect::<Result<_, _>>()
        .expect("rows")
}

/// #257: a mapped id resolves through the identity resolver to the
/// canonical email and name, with no `/v1/users` listing at all.
#[test]
fn a_mapped_actor_resolves_to_the_canonical_identity() {
    let actors =
        ActorDirectory::build_with_map(&[], &map(&[("u1", "ada@example.com")]), &resolver());
    let (events, unresolved) = build_events(&[page_by("p1", "u1")], &[], &actors).expect("events");
    assert_eq!(unresolved, 0);
    for e in &events {
        assert_eq!(
            e.actor_email.as_deref(),
            Some("ada@corp.example.com"),
            "canonical email"
        );
        assert_eq!(e.actor_name.as_deref(), Some("Ada Lovelace"));
    }
}

/// #257: an unmapped id keeps a NULL email and is counted as unresolved,
/// while the mapped one in the same sync is attributed.
#[tokio::test]
async fn an_unmapped_actor_stays_unresolved_and_counted() {
    let server = MockServer::start().await;
    mount_unlisted(&server).await;
    let mut db = Database::open_in_memory().expect("db");
    let out = sync(&mut db, &server, &map(&[("u1", "ada@example.com")])).await;
    assert_eq!(out.unresolved_actors, 1, "only u2 is unresolved");
    assert_eq!(email_of(&db, "u2"), vec![None]);
    assert_eq!(
        email_of(&db, "u1"),
        vec![Some("ada@corp.example.com".to_string()); 2]
    );
}

/// #257: rows stored before the id was mapped are attributed on the next
/// sync, and a further sync changes 0 rows.
#[tokio::test]
async fn the_backfill_attributes_stored_rows_once() {
    let server = MockServer::start().await;
    mount_unlisted(&server).await;
    let mut db = Database::open_in_memory().expect("db");
    let first = sync(&mut db, &server, &BTreeMap::new()).await;
    assert_eq!(first.backfilled, 0);
    assert_eq!(email_of(&db, "u2"), vec![None]);

    let user_map = map(&[("u2", "ada@example.com")]);
    let second = sync(&mut db, &server, &user_map).await;
    assert_eq!(second.backfilled, 1, "the stored comment row");
    assert_eq!(
        email_of(&db, "u2"),
        vec![Some("ada@corp.example.com".to_string())]
    );

    let third = sync(&mut db, &server, &user_map).await;
    assert_eq!(third.backfilled, 0, "idempotent");
}

/// #257: the map holds work emails; `Debug` (what reaches a log) prints
/// only the entry count, never an id or an email.
#[test]
fn debug_prints_the_user_map_size_only() {
    let cfg = NotionConfig {
        user_map: map(&[("u-secret-id", "ada@example.com")]),
        ..NotionConfig::default()
    };
    let rendered = format!("{cfg:?}");
    assert!(rendered.contains("user_map_entries: 1"), "{rendered}");
    assert!(!rendered.contains("ada@example.com"), "{rendered}");
    assert!(!rendered.contains("u-secret-id"), "{rendered}");
}

#[test]
fn user_map_parses_from_yaml() {
    let cfg: crate::core::config::Config = serde_yaml::from_str(
        "notion:\n  token: \"${NOTION_TOKEN}\"\n  user_map:\n    u1: ada@example.com\n",
    )
    .expect("parses");
    let notion = cfg.notion.expect("block present");
    assert_eq!(notion.user_map, map(&[("u1", "ada@example.com")]));
}

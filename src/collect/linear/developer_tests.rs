//! Tests for the Linear identity → developer match (#272).

use std::collections::HashMap;

use super::*;
use crate::core::db::Database;

pub(crate) fn insert_author(conn: &Connection, name: &str, email: &str, aliases: &str) -> i64 {
    conn.execute(
        "INSERT INTO authors (canonical_name, canonical_email, aliases) VALUES (?1, ?2, ?3)",
        rusqlite::params![name, email, aliases],
    )
    .expect("insert author");
    conn.last_insert_rowid()
}

fn resolver(map: &[(&str, &[&str])]) -> IdentityResolver {
    let map: HashMap<String, Vec<String>> = map
        .iter()
        .map(|(k, v)| (k.to_string(), v.iter().map(|s| s.to_string()).collect()))
        .collect();
    IdentityResolver::from_alias_map(&map)
}

/// #272 rule 1: the email matches case-insensitively, before any alias.
#[test]
fn matches_by_email_ignoring_case() {
    let db = Database::open_in_memory().expect("db");
    let ada = insert_author(db.connection(), "Ada Lovelace", "ada@flyr.com", "[]");
    let index = DeveloperIndex::load(db.connection(), resolver(&[])).expect("index");
    let hit = index
        .match_identity(Some("ADA@Flyr.com"), Some("Ada L"))
        .expect("match");
    assert_eq!((hit.id, hit.rule), (ada, MatchRule::Email));
    assert_eq!(hit.name, "Ada Lovelace");
}

/// #272 rules 2 and 3: a stored email alias, and a configured alias that
/// maps the Linear email to the author's canonical email.
#[test]
fn matches_by_stored_and_configured_alias() {
    let db = Database::open_in_memory().expect("db");
    let ada = insert_author(
        db.connection(),
        "Ada Lovelace",
        "ada@flyr.com",
        r#"["ada@pace.io", "ada-gh"]"#,
    );
    let bob = insert_author(db.connection(), "Bob Smith", "bob@flyr.com", "[]");
    let index = DeveloperIndex::load(
        db.connection(),
        resolver(&[("Bob Smith", &["bob@flyr.com", "robert@pace.io"])]),
    )
    .expect("index");

    let stored = index
        .match_identity(Some("Ada@Pace.io"), None)
        .expect("stored alias");
    assert_eq!((stored.id, stored.rule), (ada, MatchRule::Alias));
    let configured = index
        .match_identity(Some("robert@pace.io"), Some("Robert"))
        .expect("configured alias");
    assert_eq!((configured.id, configured.rule), (bob, MatchRule::Alias));
    // A provider login stored as a non-email alias.
    assert_eq!(index.match_login("ADA-GH").map(|m| m.id), Some(ada));
}

/// #272: an identity no rule matches is unmatched; a fuzzy near-spelling is
/// not guessed onto a developer, and no `authors` row is written.
#[test]
fn unmatched_identity_is_not_guessed() {
    let db = Database::open_in_memory().expect("db");
    insert_author(db.connection(), "Ada Lovelace", "ada@flyr.com", "[]");
    let index = DeveloperIndex::load(
        db.connection(),
        resolver(&[("Ada Lovelace", &["ada@flyr.com"])]),
    )
    .expect("index");
    assert_eq!(
        index.match_identity(Some("adah@flyr.com"), Some("Ada Lovelase")),
        None
    );
    let authors: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM authors", [], |r| r.get(0))
        .expect("count");
    assert_eq!(authors, 1);
}

/// #272 (Fail-Open check): a malformed `authors.aliases` fails the load and
/// names the row.
#[test]
fn malformed_aliases_fail_the_load() {
    let db = Database::open_in_memory().expect("db");
    let id = insert_author(db.connection(), "Ada", "ada@flyr.com", "not json");
    let Err(err) = DeveloperIndex::load(db.connection(), resolver(&[])) else {
        panic!("a malformed aliases list must fail the load");
    };
    assert!(
        err.to_string().contains(&format!("author id {id}")),
        "{err}"
    );
}

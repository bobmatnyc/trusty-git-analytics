//! Tests for `collect::linear::store` (#190): the `linear_id` key, the team
//! move, the pre-v33 row upgrade, and the no-op re-sync.

use super::*;
use crate::collect::linear::issue::parse_issue_node;
use crate::collect::linear::issue::tests::full_node;

/// An issue parsed from a full fixture node, with `updatedAt` overridden.
fn issue(lid: &str, identifier: &str, team_key: &str, updated: &str) -> LinearIssue {
    let mut node = full_node(lid, identifier, team_key);
    node["updatedAt"] = serde_json::json!(updated);
    parse_issue_node("", &node)
}

fn rows(db: &Database) -> Vec<(Option<String>, String, String)> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT linear_id, identifier, team_key FROM linear_issues ORDER BY id")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows")
}

/// #190 acceptance 4: a team move (new identifier, same id) updates the one
/// row. Keyed on `identifier`, the second write was a new row.
#[test]
fn moved_issue_updates_one_row() {
    let db = Database::open_in_memory().expect("db");
    let first = upsert_linear_issues(
        &db,
        &[issue("uuid-5", "ENG-5", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect("first");
    assert_eq!(first, vec![IssueChange::New]);

    let second = upsert_linear_issues(
        &db,
        &[issue("uuid-5", "OPS-12", "OPS", "2026-01-02T00:00:00Z")],
    )
    .expect("move");
    assert_eq!(second, vec![IssueChange::Moved]);
    assert_eq!(
        rows(&db),
        vec![(
            Some("uuid-5".to_string()),
            "OPS-12".to_string(),
            "OPS".to_string()
        )]
    );
}

/// A pre-v33 row (NULL `linear_id`) under the moved-to identifier is a stale
/// copy of the same issue; the move absorbs it instead of failing on the
/// UNIQUE `identifier` index.
#[test]
fn moved_issue_absorbs_a_legacy_row_under_its_new_identifier() {
    let db = Database::open_in_memory().expect("db");
    upsert_linear_issues(
        &db,
        &[issue("uuid-5", "ENG-5", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect("first");
    db.connection()
        .execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at) \
             VALUES ('OPS-12', 'old', 'Todo', 'Ops', 'OPS', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("legacy row");

    upsert_linear_issues(
        &db,
        &[issue("uuid-5", "OPS-12", "OPS", "2026-01-02T00:00:00Z")],
    )
    .expect("move");
    assert_eq!(rows(&db).len(), 1);
}

/// Two issue ids claiming one identifier is an error, and neither row moves.
#[test]
fn identifier_held_by_another_id_errors() {
    let db = Database::open_in_memory().expect("db");
    upsert_linear_issues(
        &db,
        &[issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect("first");
    let err = upsert_linear_issues(
        &db,
        &[issue("uuid-2", "ENG-1", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect_err("conflict");
    assert!(err.to_string().contains("ENG-1"), "{err}");
    assert_eq!(rows(&db).len(), 1);
}

/// #190 acceptance 5: a row written before v33 (no `linear_id`) is filled in
/// place by the first sync that returns its issue — same integer id, no
/// second row.
#[test]
fn legacy_row_is_filled_in_place() {
    let db = Database::open_in_memory().expect("db");
    db.connection()
        .execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at) \
             VALUES ('ENG-7', 'old', 'Todo', 'Engineering', 'ENG', '2026-01-01T00:00:00Z')",
            [],
        )
        .expect("legacy row");
    let before: i64 = db
        .connection()
        .query_row("SELECT id FROM linear_issues", [], |r| r.get(0))
        .expect("id");

    let changes = upsert_linear_issues(
        &db,
        &[issue("uuid-7", "ENG-7", "ENG", "2026-01-05T00:00:00Z")],
    )
    .expect("upsert");
    assert_eq!(changes, vec![IssueChange::Changed]);
    let (id, lid, estimate): (i64, Option<String>, Option<f64>) = db
        .connection()
        .query_row(
            "SELECT id, linear_id, estimate FROM linear_issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("row");
    assert_eq!(id, before);
    assert_eq!(lid.as_deref(), Some("uuid-7"));
    assert_eq!(estimate, Some(3.0));
}

/// #190 acceptance 7: the same `updatedAt` twice writes nothing the second
/// time.
#[test]
fn unchanged_issue_is_not_rewritten() {
    let db = Database::open_in_memory().expect("db");
    let one = issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z");
    upsert_linear_issues(&db, std::slice::from_ref(&one)).expect("first");
    db.connection()
        .execute("UPDATE linear_issues SET fetched_at = 'marker'", [])
        .expect("mark");

    let again = upsert_linear_issues(&db, &[one]).expect("second");
    assert_eq!(again, vec![IssueChange::Unchanged]);
    let fetched_at: String = db
        .connection()
        .query_row("SELECT fetched_at FROM linear_issues", [], |r| r.get(0))
        .expect("row");
    assert_eq!(
        fetched_at, "marker",
        "an unchanged issue must not be rewritten"
    );
}

#[test]
fn plan_reports_without_writing() {
    let db = Database::open_in_memory().expect("db");
    upsert_linear_issues(
        &db,
        &[issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect("seed");
    let plan = plan_linear_issues(
        db.connection(),
        &[
            issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z"),
            issue("uuid-2", "ENG-2", "ENG", "2026-01-01T00:00:00Z"),
        ],
    )
    .expect("plan");
    assert_eq!(plan, vec![IssueChange::Unchanged, IssueChange::New]);
    assert_eq!(rows(&db).len(), 1);
    let counts = ChangeCounts::of(&plan);
    assert_eq!((counts.new, counts.unchanged, counts.written()), (1, 1, 1));
}

/// Every new column round-trips, and the stored `raw_json` is the node.
#[test]
fn every_issue_field_is_stored() {
    let db = Database::open_in_memory().expect("db");
    let mut node = full_node("uuid-9", "ENG-9", "ENG");
    node["archivedAt"] = serde_json::json!("2026-02-01T00:00:00.000Z");
    let issue = parse_issue_node("", &node);
    upsert_linear_issues(&db, &[issue]).expect("store");

    let row: (String, String, f64, String, String, String, String, String) = db
        .connection()
        .query_row(
            "SELECT state_type, team_id, estimate, project_id, cycle_id, parent_id, \
             label_names, due_date FROM linear_issues",
            [],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .expect("row");
    assert_eq!(
        row,
        (
            "completed".into(),
            "team-ENG".into(),
            3.0,
            "project-1".into(),
            "cycle-1".into(),
            "parent-uuid".into(),
            r#"["bug","api"]"#.into(),
            "2026-03-01".into()
        )
    );
    let (assignee_email, creator, archived, archived_at, raw): (
        String,
        String,
        i64,
        String,
        String,
    ) = db
        .connection()
        .query_row(
            "SELECT assignee_email, creator_id, archived, archived_at, raw_json \
             FROM linear_issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row");
    assert_eq!(assignee_email, "ada@acme.test");
    assert_eq!(creator, "user-2");
    assert_eq!(archived, 1);
    assert!(archived_at.starts_with("2026-02-01"));
    let stored: serde_json::Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(stored, node);
}

/// #190 acceptance 5: migration v33 keeps every row and column a v32
/// database holds.
#[test]
fn migration_v33_preserves_a_v32_linear_row() {
    use crate::core::db::migrations::{run, run_through};
    let mut conn = rusqlite::Connection::open_in_memory().expect("conn");
    run_through(&mut conn, 32).expect("v32");
    conn.execute(
        "INSERT INTO linear_issues (identifier, title, state, team, team_key, assignee, \
         priority, url, fetched_at, created_at) \
         VALUES ('ENG-3', 'kept', 'Todo', 'Engineering', 'ENG', 'ada', 2, 'u', 'f', 'c')",
        [],
    )
    .expect("seed");
    run(&mut conn).expect("migrate");
    let row: (String, String, Option<String>, i64, Option<String>) = conn
        .query_row(
            "SELECT identifier, title, assignee, archived, linear_id FROM linear_issues",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .expect("row");
    assert_eq!(
        row,
        ("ENG-3".into(), "kept".into(), Some("ada".into()), 0, None)
    );
}

/// #190 step 3: migration v35 gives each existing Linear `work_items` row
/// the Linear id its `linear_issues` row holds, and changes nothing else. A
/// row with no such `linear_issues` row (written before v33, or a stale copy
/// a move left behind) keeps a NULL key; another source's row is untouched.
#[test]
fn migration_v35_keys_existing_linear_work_items() {
    use crate::core::db::migrations::{run, run_through};
    let mut conn = rusqlite::Connection::open_in_memory().expect("conn");
    run_through(&mut conn, 34).expect("v34");
    conn.execute_batch(
        "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
         linear_id) VALUES ('OPS-12', 't', 'Todo', 'Ops', 'OPS', 'f', 'uuid-5'); \
         INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at) \
         VALUES ('ENG-9', 't', 'Todo', 'Engineering', 'ENG', 'f'); \
         INSERT INTO work_items (id, source, title, status, item_type) VALUES \
         ('OPS-12', 'linear', 'current', 'Todo', 'Issue'), \
         ('ENG-5', 'linear', 'stale copy', 'Todo', 'Issue'), \
         ('ENG-9', 'linear', 'pre-v33', 'Todo', 'Issue'), \
         ('OPS-12', 'jira', 'other source', 'Open', 'Task');",
    )
    .expect("seed v34 rows");

    run(&mut conn).expect("migrate");
    let rows: Vec<(String, String, String, Option<String>)> = conn
        .prepare("SELECT id, source, title, stable_id FROM work_items ORDER BY source, id")
        .expect("prepare")
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows");
    assert_eq!(
        rows,
        vec![
            ("OPS-12".into(), "jira".into(), "other source".into(), None),
            ("ENG-5".into(), "linear".into(), "stale copy".into(), None),
            ("ENG-9".into(), "linear".into(), "pre-v33".into(), None),
            (
                "OPS-12".into(),
                "linear".into(),
                "current".into(),
                Some("uuid-5".into())
            ),
        ]
    );
}

/// A pre-v33 row (NULL `linear_id`) the way migration v33 left it.
fn legacy_row(db: &Database, identifier: &str) -> i64 {
    db.connection()
        .execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at) \
             VALUES (?1, 'old', 'Todo', 'Engineering', 'ENG', '2026-01-01T00:00:00Z')",
            [identifier],
        )
        .expect("legacy row");
    db.connection().last_insert_rowid()
}

/// `issue` carrying Linear's `previousIdentifiers` for a moved issue.
fn moved(lid: &str, identifier: &str, team_key: &str, previous: &[&str]) -> LinearIssue {
    let mut node = full_node(lid, identifier, team_key);
    node["previousIdentifiers"] = serde_json::json!(previous);
    parse_issue_node("", &node)
}

/// #190 step 3: an issue that moved team before its first post-v33 sync left
/// a pre-v33 row under its old identifier. Linear lists that identifier in
/// `previousIdentifiers`, so the sync adopts the row in place instead of
/// inserting a second one.
#[test]
fn orphan_under_a_previous_identifier_is_adopted() {
    let db = Database::open_in_memory().expect("db");
    let legacy_id = legacy_row(&db, "ENG-5");

    let changes =
        upsert_linear_issues(&db, &[moved("uuid-5", "OPS-12", "OPS", &["ENG-5"])]).expect("sync");
    assert_eq!(changes, vec![IssueChange::Moved]);
    assert_eq!(
        rows(&db),
        vec![(
            Some("uuid-5".to_string()),
            "OPS-12".to_string(),
            "OPS".to_string()
        )]
    );
    let id: i64 = db
        .connection()
        .query_row("SELECT id FROM linear_issues", [], |r| r.get(0))
        .expect("id");
    assert_eq!(id, legacy_id, "the legacy row is updated in place");
}

/// #190 step 3: the state step 1 left after such a move — the pre-v33 row
/// under the old identifier beside the post-v33 row under the new one. The
/// next sync of the issue drops the stale copy.
#[test]
fn stale_copy_under_a_previous_identifier_is_evicted() {
    let db = Database::open_in_memory().expect("db");
    legacy_row(&db, "ENG-5");
    upsert_linear_issues(
        &db,
        &[issue("uuid-5", "OPS-12", "OPS", "2026-01-01T00:00:00Z")],
    )
    .expect("step-1 sync");
    assert_eq!(rows(&db).len(), 2, "precondition: the orphan exists");

    upsert_linear_issues(&db, &[moved("uuid-5", "OPS-12", "OPS", &["ENG-5"])]).expect("re-sync");
    assert_eq!(
        rows(&db),
        vec![(
            Some("uuid-5".to_string()),
            "OPS-12".to_string(),
            "OPS".to_string()
        )]
    );
}

/// A row under a previous identifier that carries a DIFFERENT `linear_id`
/// is another issue's row; it is never evicted.
#[test]
fn previous_identifier_held_by_another_issue_is_kept() {
    let db = Database::open_in_memory().expect("db");
    upsert_linear_issues(
        &db,
        &[issue("uuid-other", "ENG-5", "ENG", "2026-01-01T00:00:00Z")],
    )
    .expect("other issue");

    upsert_linear_issues(&db, &[moved("uuid-5", "OPS-12", "OPS", &["ENG-5"])]).expect("sync");
    assert_eq!(rows(&db).len(), 2);
}

/// #190 step 3: a field added to the query (here `description`) reaches an
/// issue whose `updatedAt` did not change. Comparing `updatedAt` alone
/// reported it `Unchanged` and never stored the new field.
#[test]
fn widened_field_set_rewrites_an_issue_with_the_same_updated_at() {
    let db = Database::open_in_memory().expect("db");
    let before = issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z");
    upsert_linear_issues(&db, &[before]).expect("first");

    let mut node = full_node("uuid-1", "ENG-1", "ENG");
    node["updatedAt"] = serde_json::json!("2026-01-01T00:00:00Z");
    node["description"] = serde_json::json!("Body text.");
    let changes = upsert_linear_issues(&db, &[parse_issue_node("", &node)]).expect("second");
    assert_eq!(changes, vec![IssueChange::Changed]);
    let raw: String = db
        .connection()
        .query_row("SELECT raw_json FROM linear_issues", [], |r| r.get(0))
        .expect("row");
    assert!(raw.contains("Body text."), "{raw}");
}

#[test]
fn store_linear_issues_handles_missing_assignee() {
    let db = Database::open_in_memory().expect("db");
    let mut one = issue("uuid-1", "ENG-1", "ENG", "2026-01-01T00:00:00Z");
    one.assignee = None;
    assert_eq!(store_linear_issues(&db, &[one]).expect("store"), 1);
    let assignee: Option<String> = db
        .connection()
        .query_row("SELECT assignee FROM linear_issues", [], |r| r.get(0))
        .expect("row");
    assert_eq!(assignee, None);
}

/// The `linear_comment_due` rows, in issue-id order.
fn due_rows(db: &Database) -> Vec<(String, String, String)> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT issue_id, team_key, reason FROM linear_comment_due ORDER BY issue_id")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<std::result::Result<_, _>>()
        .expect("rows")
}

/// #190: `tga collect` writes `linear_issues` through `store_linear_issues`.
/// An issue it sees moved into a team, or new to a team but created before
/// the team's comment window, must be queued for a full comment read, keyed
/// on the issue's own team cursor. A team with no cursor queues nothing; a
/// re-run of the same batch queues no second row.
#[test]
fn store_linear_issues_queues_comment_reads_for_moved_and_old_new_issues() {
    let db = Database::open_in_memory().expect("db");
    store_linear_issues(
        &db,
        &[
            issue("uuid-5", "OPS-3", "OPS", "2026-01-01T00:00:00Z"),
            issue("uuid-8", "ENG-8", "ENG", "2026-01-01T00:00:00Z"),
        ],
    )
    .expect("first store");
    // ENG has a comment cursor; DES has none.
    db.connection()
        .execute(
            "INSERT INTO linear_comment_cursor \
             (team_key, cursor_updated_at, last_run_at, comments_synced) \
             VALUES ('ENG', '2026-01-10T00:00:00+00:00', '2026-01-10T00:00:00+00:00', 1)",
            [],
        )
        .expect("ENG cursor");

    let batch = [
        // Moved OPS -> ENG: queued under ENG.
        issue("uuid-5", "ENG-5", "ENG", "2026-01-11T00:00:00Z"),
        // Moved ENG -> DES: DES has no cursor, so its next walk reads it all.
        issue("uuid-8", "DES-1", "DES", "2026-01-11T00:00:00Z"),
        // New to ENG, created 2026-01-01, before ENG's window: queued.
        issue("uuid-9", "ENG-9", "ENG", "2026-01-11T00:00:00Z"),
    ];
    store_linear_issues(&db, &batch).expect("second store");
    let expected = vec![
        ("uuid-5".to_string(), "ENG".to_string(), "moved".to_string()),
        ("uuid-9".to_string(), "ENG".to_string(), "new".to_string()),
    ];
    assert_eq!(due_rows(&db), expected);

    store_linear_issues(&db, &batch).expect("re-run");
    assert_eq!(due_rows(&db), expected, "a re-run queues no second row");
}

//! Tests for `fact_linear_closed_weekly` (#272).

use super::*;
use crate::collect::identity::IdentityResolver;
use crate::collect::linear::developer::tests::insert_author;
use crate::core::db::Database;

/// Insert a `linear_issues` row with the fields the weekly table reads.
pub(crate) fn insert_issue(
    conn: &Connection,
    identifier: &str,
    state_type: &str,
    completed_at: Option<&str>,
    assignee: Option<(&str, &str)>,
) {
    conn.execute(
        "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
                                    linear_id, state_type, completed_at, assignee_id, \
                                    assignee_name, assignee_email) \
         VALUES (?1, 't', 'Done', 'Pace', 'PLA', '2026-01-01T00:00:00Z', ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            identifier,
            state_type,
            completed_at,
            assignee.map(|(n, _)| format!("user-{n}")),
            assignee.map(|(n, _)| n),
            assignee.map(|(_, e)| e),
        ],
    )
    .expect("insert issue");
}

fn index(db: &Database) -> DeveloperIndex {
    DeveloperIndex::load(db.connection(), IdentityResolver::new(None)).expect("index")
}

type Row = (Option<i64>, String, String, i64, String);

fn rows(db: &Database) -> Vec<Row> {
    let mut stmt = db
        .connection()
        .prepare(
            "SELECT canonical_developer_id, developer_name, iso_week, closed_count, \
                    closed_issue_keys FROM fact_linear_closed_weekly \
             ORDER BY iso_week, developer_name",
        )
        .expect("prepare");
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .expect("query")
    .collect::<std::result::Result<_, _>>()
    .expect("rows")
}

/// #272: ISO week 53 of 2026 runs into January 2027, and week 1 of 2026
/// starts in December 2025. The week is taken in UTC, not local time.
#[test]
fn weeks_cross_the_year_boundary() {
    let mut db = Database::open_in_memory().expect("db");
    let ada = insert_author(db.connection(), "Ada", "ada@example.com", "[]");
    let a = Some(("Ada", "ada@example.com"));
    let c = db.connection();
    insert_issue(c, "PLA-1", "completed", Some("2025-12-29T10:00:00Z"), a);
    insert_issue(
        c,
        "PLA-2",
        "completed",
        Some("2026-01-01T00:30:00+02:00"),
        a,
    );
    insert_issue(c, "PLA-3", "completed", Some("2026-12-31T12:00:00Z"), a);
    insert_issue(c, "PLA-4", "completed", Some("2027-01-03T23:59:59Z"), a);
    insert_issue(c, "PLA-5", "completed", Some("2027-01-04T00:00:00Z"), a);

    let idx = index(&db);
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("rebuild");
    assert_eq!(
        rows(&db),
        vec![
            (
                Some(ada),
                "Ada".into(),
                "2026-W01".into(),
                2,
                r#"["PLA-1","PLA-2"]"#.into()
            ),
            (
                Some(ada),
                "Ada".into(),
                "2026-W53".into(),
                2,
                r#"["PLA-3","PLA-4"]"#.into()
            ),
            (
                Some(ada),
                "Ada".into(),
                "2027-W01".into(),
                1,
                r#"["PLA-5"]"#.into()
            ),
        ]
    );
    let monday: String = db
        .connection()
        .query_row(
            "SELECT week_start FROM fact_linear_closed_weekly WHERE iso_week = '2026-W53'",
            [],
            |r| r.get(0),
        )
        .expect("week_start");
    assert_eq!(monday, "2026-12-28");
}

/// #272: canceled issues, and completed issues with no `completed_at`, are
/// not closed; an issue with no assignee is counted, not credited; an
/// unmatched assignee keeps a row under its Linear name.
#[test]
fn canceled_issues_are_excluded() {
    let mut db = Database::open_in_memory().expect("db");
    insert_author(db.connection(), "Ada", "ada@example.com", "[]");
    let a = Some(("Ada", "ada@example.com"));
    let c = db.connection();
    insert_issue(c, "PLA-1", "completed", Some("2026-03-02T00:00:00Z"), a);
    insert_issue(c, "PLA-2", "canceled", Some("2026-03-02T00:00:00Z"), a);
    insert_issue(c, "PLA-3", "completed", None, a);
    insert_issue(c, "PLA-4", "completed", Some("2026-03-02T00:00:00Z"), None);
    insert_issue(
        c,
        "PLA-5",
        "completed",
        Some("2026-03-02T00:00:00Z"),
        Some(("Zed Stranger", "zed@example.org")),
    );

    let idx = index(&db);
    let outcome = rebuild_closed_weekly(db.connection_mut(), &idx).expect("rebuild");
    assert_eq!(
        outcome,
        ClosedWeeklyOutcome {
            rows: 2,
            credited: 2,
            unassigned: 1,
            unmatched_issues: 1
        }
    );
    let got = rows(&db);
    assert_eq!(got[0].1, "Ada");
    assert_eq!(got[0].4, r#"["PLA-1"]"#);
    assert_eq!(
        got[1],
        (
            None,
            "Zed Stranger".into(),
            "2026-W10".into(),
            1,
            r#"["PLA-5"]"#.into()
        )
    );
}

/// #272: a second rebuild writes the same rows; a reopened issue drops its
/// credit on the next rebuild.
#[test]
fn rebuild_is_idempotent() {
    let mut db = Database::open_in_memory().expect("db");
    insert_author(db.connection(), "Ada", "ada@example.com", "[]");
    let a = Some(("Ada", "ada@example.com"));
    insert_issue(
        db.connection(),
        "PLA-1",
        "completed",
        Some("2026-03-02T00:00:00Z"),
        a,
    );
    insert_issue(
        db.connection(),
        "PLA-2",
        "completed",
        Some("2026-03-03T00:00:00Z"),
        a,
    );
    let idx = index(&db);
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("first");
    let first = rows(&db);
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("second");
    assert_eq!(rows(&db), first);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].3, 2);

    db.connection()
        .execute(
            "UPDATE linear_issues SET state_type = 'started', completed_at = NULL \
             WHERE identifier = 'PLA-2'",
            [],
        )
        .expect("reopen");
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("third");
    assert_eq!(rows(&db)[0].4, r#"["PLA-1"]"#);
}

/// #272 (D1f/D09): two issues of one Linear user, one carrying the email and
/// one not, resolve once and credit one row.
#[test]
fn one_assignee_id_gives_one_row() {
    let mut db = Database::open_in_memory().expect("db");
    insert_author(db.connection(), "Ada Lovelace", "ada@example.com", "[]");
    let c = db.connection();
    insert_issue(
        c,
        "PLA-1",
        "completed",
        Some("2026-03-02T00:00:00Z"),
        Some(("Ada", "ada@example.com")),
    );
    insert_issue(c, "PLA-2", "completed", Some("2026-03-03T00:00:00Z"), None);
    c.execute(
        "UPDATE linear_issues SET assignee_id = 'user-Ada', assignee_name = 'Ada' \
         WHERE identifier = 'PLA-2'",
        [],
    )
    .expect("same user, no email");

    let idx = index(&db);
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("rebuild");
    let got = rows(&db);
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].1, "Ada Lovelace");
    assert_eq!(got[0].4, r#"["PLA-1","PLA-2"]"#);
}

/// #272 (Fail-Open check): an unparseable `completed_at` fails the rebuild,
/// names the issue, and leaves the previous rows in place.
#[test]
fn unparseable_completed_at_fails_and_keeps_rows() {
    let mut db = Database::open_in_memory().expect("db");
    insert_author(db.connection(), "Ada", "ada@example.com", "[]");
    let a = Some(("Ada", "ada@example.com"));
    insert_issue(
        db.connection(),
        "PLA-1",
        "completed",
        Some("2026-03-02T00:00:00Z"),
        a,
    );
    let idx = index(&db);
    rebuild_closed_weekly(db.connection_mut(), &idx).expect("first");
    insert_issue(db.connection(), "PLA-9", "completed", Some("yesterday"), a);
    let err = rebuild_closed_weekly(db.connection_mut(), &idx).expect_err("must fail");
    assert!(err.to_string().contains("PLA-9"), "{err}");
    assert_eq!(rows(&db).len(), 1);
}

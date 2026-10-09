//! A synthetic two-team Linear workspace for the stats tests (#190 step 4).
//!
//! Teams ENG and OPS of the workspace `acme`. Rows are written with plain SQL,
//! not through the sync writers, so the stats read exactly what a test put
//! there. Every expected figure in `tests.rs` is computed from [`issues`] by
//! the test's own loops, never by the code under test.

use crate::core::db::Database;

/// The as-of instant every fixture test uses.
pub(super) const AS_OF: &str = "2026-10-09T12:00:00Z";

/// One fixture issue.
#[derive(Debug, Clone)]
pub(super) struct Fx {
    pub id: &'static str,
    pub team: &'static str,
    pub state: &'static str,
    pub title: &'static str,
    pub created: &'static str,
    pub started: Option<&'static str>,
    pub completed: Option<&'static str>,
    pub canceled: Option<&'static str>,
    pub archived: bool,
    pub assignee: Option<&'static str>,
    pub estimate: Option<f64>,
    pub labels: &'static [&'static str],
    pub project: Option<&'static str>,
    pub due: Option<&'static str>,
    pub cycle: Option<&'static str>,
    pub parent: Option<&'static str>,
}

fn fx(id: &'static str, team: &'static str, state: &'static str, created: &'static str) -> Fx {
    Fx {
        id,
        team,
        state,
        title: "Plain title",
        created,
        started: None,
        completed: None,
        canceled: None,
        archived: false,
        assignee: None,
        estimate: None,
        labels: &[],
        project: None,
        due: None,
        cycle: None,
        parent: None,
    }
}

impl Fx {
    fn done(mut self, started: Option<&'static str>, completed: &'static str) -> Self {
        self.started = started;
        self.completed = Some(completed);
        self
    }
    fn cancel(mut self, at: &'static str) -> Self {
        self.canceled = Some(at);
        self
    }
    fn who(mut self, assignee: &'static str) -> Self {
        self.assignee = Some(assignee);
        self
    }
    fn title(mut self, title: &'static str) -> Self {
        self.title = title;
        self
    }
}

/// The 31 fixture issues.
///
/// ENG completes in every quarter 2024-Q4..2026-Q3 (stable); OPS does not.
/// Some shares and medians land on an exact `.x5` tie, which pins the
/// half-even rounding.
pub(super) fn issues() -> Vec<Fx> {
    let mut v = vec![
        fx("ENG-1", "ENG", "completed", "2023-04-03T08:00:00Z")
            .done(Some("2023-04-05T08:00:00Z"), "2023-05-02T17:30:00Z")
            .who("u-ann"),
        fx("ENG-2", "ENG", "completed", "2024-10-01T09:00:00Z")
            .done(Some("2024-10-03T09:00:00Z"), "2024-10-20T15:00:00Z")
            .who("u-ann"),
        fx("ENG-3", "ENG", "completed", "2024-12-20T10:00:00Z")
            .done(None, "2025-01-14T11:00:00Z")
            .who("u-bob"),
        fx("ENG-4", "ENG", "completed", "2025-04-01T07:00:00Z")
            .done(Some("2025-04-02T07:00:00Z"), "2025-05-30T18:00:00Z")
            .who("u-ann"),
        fx("ENG-5", "ENG", "completed", "2025-07-01T06:00:00Z")
            .done(Some("2025-07-08T06:00:00Z"), "2025-09-30T23:30:00Z")
            .who("u-cat"),
        fx("ENG-6", "ENG", "completed", "2025-10-02T12:00:00Z")
            .done(Some("2025-10-04T12:00:00Z"), "2025-11-11T10:00:00Z")
            .who("u-ann"),
        fx("ENG-7", "ENG", "completed", "2026-01-05T08:00:00Z")
            .done(Some("2026-01-06T08:00:00Z"), "2026-02-02T09:00:00Z")
            .who("u-bob"),
        fx("ENG-8", "ENG", "completed", "2026-04-01T08:00:00Z")
            .done(Some("2026-04-03T08:00:00Z"), "2026-05-05T16:00:00Z")
            .who("u-ann"),
        fx("ENG-9", "ENG", "completed", "2026-07-01T08:00:00Z")
            .done(Some("2026-07-02T08:00:00Z"), "2026-07-21T13:00:00Z")
            .who("u-dan"),
        fx("ENG-10", "ENG", "completed", "2026-07-10T08:00:00Z")
            .done(Some("2026-07-12T08:00:00Z"), "2026-09-14T08:00:00Z")
            .who("u-ann"),
        fx("ENG-11", "ENG", "completed", "2026-09-20T08:00:00Z")
            .done(Some("2026-09-25T08:00:00Z"), "2026-10-02T08:00:00Z")
            .who("u-eve"),
        fx("ENG-12", "ENG", "canceled", "2025-02-01T08:00:00Z").cancel("2025-03-05T08:00:00Z"),
        fx("ENG-13", "ENG", "duplicate", "2025-05-01T08:00:00Z").cancel("2025-06-01T08:00:00Z"),
        fx("ENG-14", "ENG", "backlog", "2024-01-15T08:00:00Z"),
        fx("ENG-15", "ENG", "started", "2026-08-01T08:00:00Z").who("u-ann"),
        fx("ENG-16", "ENG", "backlog", "2023-01-10T08:00:00Z"),
        fx("ENG-17", "ENG", "completed", "2026-04-10T08:00:00Z")
            .done(Some("2026-04-11T08:00:00Z"), "2026-06-01T08:00:00Z")
            .who("u-bob"),
        fx("ENG-18", "ENG", "completed", "2026-04-12T08:00:00Z")
            .done(Some("2026-04-20T08:00:00Z"), "2026-06-02T09:00:00Z"),
        fx("ENG-19", "ENG", "completed", "2026-05-01T08:00:00Z")
            .done(Some("2026-05-02T08:00:00Z"), "2026-06-20T19:00:00Z")
            .who("u-ann"),
        fx("ENG-20", "ENG", "completed", "2026-05-03T08:00:00Z")
            .done(Some("2026-05-03T09:00:00Z"), "2026-06-21T07:00:00Z")
            .who("u-cat"),
        fx("OPS-1", "OPS", "completed", "2025-01-10T08:00:00Z")
            .done(Some("2025-01-12T08:00:00Z"), "2025-02-11T14:00:00Z")
            .who("u-ann")
            .title("[Acme] [Site 1] [X1] Setup"),
        fx("OPS-2", "OPS", "completed", "2026-01-02T08:00:00Z")
            .done(Some("2026-01-03T08:00:00Z"), "2026-03-30T08:00:00Z")
            .who("u-fay")
            .title("[Acme] [Site 2] Setup"),
        fx("OPS-3", "OPS", "completed", "2026-04-02T08:00:00Z")
            .done(None, "2026-04-28T21:00:00Z")
            .who("u-fay")
            .title("[Acme] Setup"),
        fx("OPS-4", "OPS", "canceled", "2026-01-15T08:00:00Z")
            .cancel("2026-02-01T08:00:00Z")
            .title("[Acme] [Site 3 unclosed"),
        fx("OPS-5", "OPS", "canceled", "2026-01-16T08:00:00Z")
            .cancel("2026-02-01T09:00:00Z")
            .title("  [Acme]  [Site 4]   [X4] Setup"),
        fx("OPS-6", "OPS", "triage", "2026-09-01T08:00:00Z"),
        fx("OPS-7", "OPS", "unstarted", "2026-03-01T08:00:00Z"),
        fx("OPS-8", "OPS", "completed", "2026-07-01T08:00:00Z")
            .done(Some("2026-07-01T10:00:00Z"), "2026-08-15T08:00:00Z")
            .who("u-fay"),
        fx("OPS-9", "OPS", "completed", "2026-07-05T08:00:00Z")
            .done(Some("2026-07-09T08:00:00Z"), "2026-09-01T08:00:00Z")
            .who("u-ann"),
        fx("OPS-10", "OPS", "completed", "2026-08-01T08:00:00Z")
            .done(Some("2026-08-02T08:00:00Z"), "2026-09-29T12:00:00Z"),
        // Identifier prefix OLD, current team ENG: counts under ENG.
        fx("OLD-1", "ENG", "completed", "2025-08-01T08:00:00Z")
            .done(Some("2025-08-03T08:00:00Z"), "2025-08-10T20:00:00Z")
            .who("u-bob"),
    ];
    // Field-use variety, set after the fact so the table above stays legible.
    for i in &mut v {
        match i.id {
            "ENG-2" | "ENG-7" | "ENG-19" => i.estimate = Some(3.0),
            "ENG-8" => i.estimate = Some(0.0),
            "OPS-2" => i.estimate = Some(2.0),
            _ => {}
        }
        match i.id {
            "ENG-2" | "ENG-6" | "OPS-2" | "OPS-8" => i.labels = &["lbl-bug"],
            "ENG-9" => i.labels = &["lbl-bug", "lbl-ui"],
            _ => {}
        }
        match i.id {
            "ENG-6" | "ENG-7" | "ENG-8" | "OPS-1" | "OPS-9" => i.project = Some("prj-1"),
            "ENG-17" => i.project = Some("prj-2"),
            _ => {}
        }
        match i.id {
            "ENG-7" | "OPS-8" => i.due = Some("2026-03-01"),
            _ => {}
        }
        match i.id {
            "ENG-8" | "ENG-17" | "ENG-18" | "ENG-19" => i.cycle = Some("cyc-1"),
            _ => {}
        }
        if matches!(i.id, "ENG-18" | "OPS-10") {
            i.parent = Some("uuid-ENG-17");
        }
        if matches!(i.id, "ENG-3" | "ENG-16" | "OPS-4") {
            i.archived = true;
        }
    }
    v
}

/// A database holding [`issues`] and the reference entities.
pub(super) fn seeded() -> Database {
    let db = Database::open_in_memory().expect("db");
    insert_issues(&db, &issues());
    insert_entities(&db);
    db
}

/// Write `rows` into `linear_issues`, each with a Linear id.
pub(super) fn insert_issues(db: &Database, rows: &[Fx]) {
    let conn = db.connection();
    for i in rows {
        let labels = serde_json::to_string(i.labels).expect("labels");
        conn.execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
             linear_id, state_type, created_at, started_at, completed_at, canceled_at, \
             archived, archived_at, assignee_id, estimate, label_ids, project_id, due_date, \
             cycle_id, parent_id) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6, ?3, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, \
             ?16, ?17, ?18, ?19)",
            rusqlite::params![
                i.id,
                i.title,
                i.state,
                i.team,
                AS_OF,
                format!("uuid-{}", i.id),
                i.created,
                i.started,
                i.completed,
                i.canceled,
                i64::from(i.archived),
                i.archived.then_some("2026-09-30T00:00:00Z"),
                i.assignee,
                i.estimate,
                labels,
                i.project,
                i.due,
                i.cycle,
                i.parent,
            ],
        )
        .expect("insert issue");
    }
}

/// Teams, users, labels, projects, milestones and cycles.
fn insert_entities(db: &Database) {
    let conn = db.connection();
    let exec = |sql: &str| conn.execute_batch(sql).expect("insert entities");
    exec(
        "INSERT INTO linear_teams (id, key, name, issue_estimation_type, raw_json, fetched_at) \
         VALUES ('team-eng', 'ENG', 'Engineering', 'fibonacci', '{}', 'x'), \
                ('team-ops', 'OPS', 'Operations', 'notUsed', '{}', 'x'); \
         INSERT INTO linear_teams (id, key, name, raw_json, fetched_at, removed_at) \
         VALUES ('team-old', 'OLD', 'Gone', '{}', 'x', '2026-01-01T00:00:00Z');",
    );
    exec(
        "INSERT INTO linear_users (id, name, email, active, raw_json, fetched_at) \
         VALUES ('u-ann', 'Ann', 'ann@example.com', 1, '{}', 'x'), \
                ('u-bob', 'Bob', 'bob@example.com', 1, '{}', 'x'), \
                ('u-cat', 'Cat', 'cat@example.com', 0, '{}', 'x');",
    );
    exec(
        "INSERT INTO linear_labels (id, name, raw_json, fetched_at) \
         VALUES ('lbl-bug', 'bug', '{}', 'x'), ('lbl-ui', 'ui', '{}', 'x');",
    );
    // P1 late by four calendar days; P2 done on its target day; P3 early;
    // P4 started and past target; P5 started, target ahead; P6 planned, no
    // target; P7 completed, no target; P8 canceled; P9 removed.
    exec(
        "INSERT INTO linear_projects (id, name, state, target_date, completed_at, lead_id, \
         raw_json, fetched_at, removed_at) VALUES \
         ('prj-1', 'Alpha', 'completed', '2026-03-01', '2026-03-05T10:00:00Z', 'u-ann', '{}', 'x', NULL), \
         ('prj-2', 'Beta', 'completed', '2026-04-01', '2026-04-01T15:00:00Z', NULL, '{}', 'x', NULL), \
         ('prj-3', 'Gamma', 'completed', '2026-05-10', '2026-05-08T06:00:00Z', 'u-bob', '{}', 'x', NULL), \
         ('prj-4', 'Delta', 'started', '2026-09-01', NULL, 'u-ann', '{}', 'x', NULL), \
         ('prj-5', 'Epsilon', 'started', '2026-12-01', NULL, NULL, '{}', 'x', NULL), \
         ('prj-6', 'Zeta', 'planned', NULL, NULL, NULL, '{}', 'x', NULL), \
         ('prj-7', 'Eta', 'completed', NULL, '2026-02-01T00:00:00Z', 'u-cat', '{}', 'x', NULL), \
         ('prj-8', 'Theta', 'canceled', '2026-01-01', NULL, NULL, '{}', 'x', NULL), \
         ('prj-9', 'Iota', 'completed', '2026-01-01', '2026-02-01T00:00:00Z', NULL, '{}', 'x', \
          '2026-05-01T00:00:00Z');",
    );
    exec(
        "INSERT INTO linear_milestones (id, project_id, name, target_date, raw_json, fetched_at) \
         VALUES ('ms-1', 'prj-1', 'M1', '2026-02-01', '{}', 'x'), \
                ('ms-2', 'prj-1', 'M2', NULL, '{}', 'x'), \
                ('ms-3', 'prj-2', 'M3', NULL, '{}', 'x');",
    );
    // Two measured ENG cycles in 2026 (10/6 and 5/5), one empty, one not
    // completed, one with no completed count, one OPS cycle in 2025.
    exec(
        "INSERT INTO linear_cycles (id, team_id, starts_at, ends_at, completed_at, scope_count, \
         completed_count, is_empty, raw_json, fetched_at) VALUES \
         ('cyc-1', 'team-eng', '2026-05-04T00:00:00Z', '2026-05-18T00:00:00Z', '2026-05-18T00:00:00Z', 10, 6, 0, '{}', 'x'), \
         ('cyc-2', 'team-eng', '2026-05-18T00:00:00Z', '2026-05-25T00:00:00Z', '2026-05-25T00:00:00Z', 5, 5, 0, '{}', 'x'), \
         ('cyc-3', 'team-eng', '2026-05-25T00:00:00Z', '2026-06-08T00:00:00Z', '2026-06-08T00:00:00Z', 0, 0, 1, '{}', 'x'), \
         ('cyc-4', 'team-eng', '2026-10-05T00:00:00Z', '2026-10-19T00:00:00Z', NULL, 7, 2, 0, '{}', 'x'), \
         ('cyc-5', 'team-eng', '2026-06-08T00:00:00Z', '2026-06-22T00:00:00Z', '2026-06-22T00:00:00Z', 4, NULL, 0, '{}', 'x'), \
         ('cyc-6', 'team-ops', '2025-03-03T00:00:00Z', '2025-03-17T00:00:00Z', '2025-03-17T00:00:00Z', 8, 2, 0, '{}', 'x');",
    );
}

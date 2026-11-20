//! Tests for the pre-flight call plan and `--plan` (#272, D94).

use super::plan::{FullRead, SyncPlan, TeamPlan};
use super::*;
use tga::collect::linear::bulk::tests::{mock_client, node, page_response};
use tga::collect::linear::issue::ISSUE_FIELDS_VERSION;
use wiremock::matchers::{body_string_contains, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn team(key: &str, stored: usize, history: bool, full_read: Option<FullRead>) -> TeamPlan {
    TeamPlan {
        team_key: key.to_string(),
        full_read,
        stored_issues: stored,
        history,
    }
}

/// #272: calls = 1 per team + ceil(issues / 50) + (with --history) issues,
/// plus 1 team-list query under --all-teams.
#[test]
fn plan_arithmetic() {
    let cases = [
        (0, false, 1),
        (1, false, 2),
        (50, false, 2),
        (51, false, 3),
        (29_850, false, 1 + 597),
        (120, true, 1 + 3 + 120),
    ];
    for (stored, history, want) in cases {
        let t = team("PLA", stored, history, Some(FullRead::Backfill));
        assert_eq!(t.calls(), want, "stored={stored} history={history}");
    }
    let plan = SyncPlan {
        team_list_queries: 1,
        teams: vec![
            team("PLA", 120, true, Some(FullRead::FirstSync)),
            team("DP", 51, false, None),
        ],
    };
    assert_eq!(plan.total_calls(), 1 + (1 + 3 + 120) + (1 + 2));
    let text = plan.render();
    assert!(
        text.starts_with("Linear sync plan: unknown (at least 128) API call(s) across 2 team(s)")
    );
    assert!(text.contains("No issue was fetched"));
}

/// #272 (D94): a first sync's plan is a floor; it never prints "about 1".
#[test]
fn first_sync_plan_is_unknown_not_about_one() {
    let plan = SyncPlan {
        team_list_queries: 0,
        teams: vec![team("PLA", 0, false, Some(FullRead::FirstSync))],
    };
    let text = plan.render();
    assert!(!text.contains("about 1"), "{text}");
    assert!(text.contains("unknown (at least 1)"), "{text}");
    let notice = plan.teams[0].notice().expect("notice");
    assert!(!notice.contains("about 1"), "{notice}");
    assert!(notice.contains("at least"), "{notice}");
}

fn args(team: &str) -> LinearSyncArgs {
    LinearSyncArgs {
        team: Some(team.to_string()),
        ..Default::default()
    }
}

/// #272 (PR 2 LOW finding): a first sync names itself, a `--since` run makes
/// no full-read claim, and only a stored older field set reads as a refresh.
#[test]
fn notice_names_the_real_reason() {
    let db = Database::open_in_memory().expect("db");
    let first = plan::team_plan(&db, "ENG", &args("ENG"), None).expect("plan");
    assert_eq!(first.full_read, Some(FullRead::FirstSync));
    let notice = first.notice().expect("first-sync notice");
    assert!(notice.contains("first sync"), "{notice}");
    assert!(!notice.contains("field set"), "{notice}");

    let since = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    let bounded = plan::team_plan(&db, "ENG", &args("ENG"), since).expect("plan");
    assert_eq!(
        bounded.notice(),
        None,
        "a --since run reads no full history"
    );

    tga::core::db::set_linear_cursor(db.connection(), "ENG", "2026-01-01T00:00:00+00:00", 1)
        .expect("cursor");
    let refresh = plan::team_plan(&db, "ENG", &args("ENG"), None).expect("plan");
    assert_eq!(
        refresh.full_read,
        Some(FullRead::FieldRefresh {
            old: 0,
            new: ISSUE_FIELDS_VERSION
        })
    );
    let history = LinearSyncArgs {
        history: true,
        ..args("ENG")
    };
    let with_history = plan::team_plan(&db, "ENG", &history, since).expect("plan");
    assert!(with_history
        .notice()
        .is_some_and(|n| n.contains("history walk")));
}

/// #272 (D94): `--plan` sends no issue request. Single team: no request at
/// all. `--all-teams`: the team-list query only.
#[tokio::test]
async fn plan_flag_fetches_no_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(body_string_contains("teams(first"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "data": {"teams": {"nodes": [{"id": "t1", "key": "ENG", "name": "ENG"}],
                "pageInfo": {"hasNextPage": false, "endCursor": null}}}
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .respond_with(page_response(
            vec![node("ENG-1", "2026-01-01T00:01:00.000Z", None)],
            false,
            None,
        ))
        .mount(&server)
        .await;
    let client = mock_client(&server.uri());
    let mut db = Database::open_in_memory().expect("db");

    let single = LinearSyncArgs {
        plan: true,
        ..args("ENG")
    };
    let out = run_sync_with(&client, &Config::default(), &mut db, &single)
        .await
        .expect("plan");
    assert!(out.is_empty());
    let sent = server.received_requests().await.expect("recorded");
    assert!(
        sent.is_empty(),
        "single-team --plan sent {} request(s)",
        sent.len()
    );

    let all = LinearSyncArgs {
        plan: true,
        all_teams: true,
        ..Default::default()
    };
    run_sync_with(&client, &Config::default(), &mut db, &all)
        .await
        .expect("plan");
    let sent = server.received_requests().await.expect("recorded");
    assert_eq!(sent.len(), 1, "only the team list");
    let body = String::from_utf8_lossy(&sent[0].body).to_string();
    assert!(body.contains("teams(first"), "{body}");
    let issues: i64 = db
        .connection()
        .query_row("SELECT COUNT(*) FROM linear_issues", [], |r| r.get(0))
        .expect("count");
    assert_eq!(issues, 0);
}

/// #272: a non-dry sync rebuilds `fact_linear_closed_weekly`.
#[tokio::test]
async fn sync_rebuilds_the_weekly_table() {
    let server = MockServer::start().await;
    let mut done = node("ENG-1", "2026-03-02T00:00:00.000Z", None);
    done["state"] = serde_json::json!({"name": "Done", "type": "completed"});
    done["completedAt"] = serde_json::json!("2026-03-02T00:00:00.000Z");
    done["assignee"] = serde_json::json!({"id": "u1", "name": "Ada", "email": "ada@example.com"});
    Mock::given(method("POST"))
        .and(body_string_contains("\"eq\":\"ENG\""))
        .respond_with(page_response(vec![done], false, None))
        .mount(&server)
        .await;
    let mut db = Database::open_in_memory().expect("db");
    db.connection()
        .execute(
            "INSERT INTO authors (canonical_name, canonical_email) VALUES ('Ada', 'ada@example.com')",
            [],
        )
        .expect("author");
    run_sync_with(
        &mock_client(&server.uri()),
        &Config::default(),
        &mut db,
        &args("ENG"),
    )
    .await
    .expect("sync");
    let (dev, week, n): (Option<i64>, String, i64) = db
        .connection()
        .query_row(
            "SELECT canonical_developer_id, iso_week, closed_count FROM fact_linear_closed_weekly",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("one row");
    assert_eq!((dev, week.as_str(), n), (Some(1), "2026-W10", 1));
}

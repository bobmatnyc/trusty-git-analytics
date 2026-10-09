//! Golden and failure tests for `report::linear_stats` (#190 step 4).
//!
//! Each golden test recomputes its figures from the fixture rows with the
//! test's own loops (quarters from string slices, its own percentile and
//! rounding), and pins the headline numbers by hand. Neither path calls the
//! code under test.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{DateTime, NaiveDate, Utc};

use super::dist::{dist, quarter_minus, round1};
use super::test_fixture::{insert_issues, issues, seeded, Fx, AS_OF};
use super::*;
use crate::core::db::Database;

fn t(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .expect("ts")
        .with_timezone(&Utc)
}

fn opts() -> StatsOptions {
    StatsOptions::new(t(AS_OF))
}

fn stats_with(o: &StatsOptions) -> LinearStats {
    compute_linear_stats(seeded().connection(), o).expect("stats")
}

fn stats() -> LinearStats {
    stats_with(&opts())
}

fn naive_quarter(s: &str) -> String {
    let m: u32 = s[5..7].parse().expect("month");
    format!("{}-Q{}", &s[..4], (m - 1) / 3 + 1)
}

fn year(s: &str) -> i32 {
    s[..4].parse().expect("year")
}

fn naive_days(a: &str, b: &str) -> f64 {
    (t(b) - t(a)).num_seconds() as f64 / 86_400.0
}

/// One decimal, rounding the exact binary value with ties to even, as the
/// reference's `round(x, 1)` does. The fixture lands on exact ties (81.25,
/// 19.25) and on values whose decimal form is a tie but whose binary value is
/// not, so `(x * 10).round()` would disagree with the reference both ways.
/// `round1_is_half_even_on_the_binary_value` pins the rule itself.
fn r1(x: f64) -> f64 {
    format!("{x:.1}").parse().expect("float")
}

fn naive_pct(sorted: &[f64], p: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let pos = (sorted.len() - 1) as f64 * p;
    let below = pos.floor() as usize;
    let above = (below + 1).min(sorted.len() - 1);
    Some(r1(
        sorted[below] + (sorted[above] - sorted[below]) * (pos - below as f64)
    ))
}

fn assert_dist(actual: &Dist, mut values: Vec<f64>, ctx: &str) {
    values.sort_by(f64::total_cmp);
    assert_eq!(actual.n, values.len(), "{ctx}: n");
    assert_eq!(actual.median, naive_pct(&values, 0.5), "{ctx}: median");
    assert_eq!(actual.p75, naive_pct(&values, 0.75), "{ctx}: p75");
    assert_eq!(actual.p90, naive_pct(&values, 0.9), "{ctx}: p90");
}

fn naive_share(num: usize, den: usize) -> Option<f64> {
    (den > 0).then(|| r1(100.0 * num as f64 / den as f64))
}

fn completed(i: &Fx) -> Option<&'static str> {
    i.completed.filter(|_| i.state == "completed")
}

/// Pinned: type 7 gives p90 7.6 here; nearest-rank would give 10.
#[test]
fn percentile_is_type_7_linear() {
    let d = dist(&[10.0, 1.0, 4.0, 2.0, 3.0]);
    assert_eq!(
        (d.n, d.median, d.p75, d.p90),
        (5, Some(3.0), Some(4.0), Some(7.6))
    );
    let even = dist(&[4.0, 1.0, 3.0, 2.0]);
    assert_eq!(even.median, Some(2.5), "even n: mean of the middle two");
    assert_eq!(dist(&[]).median, None);
}

#[test]
fn round1_is_half_even_on_the_binary_value() {
    assert_eq!(round1(0.25), 0.2, "exact tie goes to even");
    assert_eq!(round1(0.75), 0.8, "exact tie goes to even");
    assert_eq!(round1(0.35), 0.3, "0.35 is below the tie in binary");
    assert_eq!(round1(-1.25), -1.2);
}

#[test]
fn windows_are_the_eight_whole_quarters_before_the_as_of_quarter() {
    assert_eq!(quarter_minus((2025, 1), 1), (2024, 4));
    let s = stats();
    assert_eq!(
        s.completions.recent_window,
        ["2025-Q4", "2026-Q1", "2026-Q2", "2026-Q3"]
    );
    assert_eq!(
        s.completions.prior_window,
        ["2024-Q4", "2025-Q1", "2025-Q2", "2025-Q3"]
    );
}

/// C0.
#[test]
fn golden_population() {
    let s = stats();
    let fx = issues();
    let p = &s.population;
    assert_eq!(p.issues, fx.len());
    assert_eq!(p.issues, 31);
    let archived = fx.iter().filter(|i| i.archived).count();
    assert_eq!(p.archived, archived);
    assert_eq!(p.archived_pct, naive_share(archived, fx.len()));
    let mut by_state = BTreeMap::new();
    let mut by_team = BTreeMap::new();
    for i in &fx {
        *by_state.entry(i.state.to_string()).or_insert(0) += 1;
        *by_team.entry(i.team.to_string()).or_insert(0) += 1;
    }
    assert_eq!(p.by_state_type, by_state);
    assert_eq!(p.issues_by_team, by_team);
    assert_eq!(
        p.issues_by_team.get("OLD"),
        None,
        "team is the current team key"
    );
    assert_eq!(
        (
            p.teams,
            p.users,
            p.labels,
            p.projects,
            p.milestones,
            p.cycles
        ),
        (2, 3, 2, 8, 3, 6),
        "removed rows are not counted"
    );
    assert_eq!((p.rows_without_linear_id, p.without_state_type), (0, 0));
}

/// C1, C1b, C1c.
#[test]
fn golden_completions() {
    let s = stats();
    let mut all: BTreeMap<String, usize> = BTreeMap::new();
    let mut teams: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
    for i in issues() {
        if let Some(c) = completed(&i) {
            *all.entry(naive_quarter(c)).or_insert(0) += 1;
            *teams
                .entry(i.team.into())
                .or_default()
                .entry(naive_quarter(c))
                .or_insert(0) += 1;
        }
    }
    assert_eq!(s.completions.per_quarter.all, all);
    assert_eq!(s.completions.per_quarter.teams, teams);

    let prior = ["2024-Q4", "2025-Q1", "2025-Q2", "2025-Q3"];
    let recent = ["2025-Q4", "2026-Q1", "2026-Q2", "2026-Q3"];
    let sum = |m: &BTreeMap<String, usize>, w: &[&str]| -> usize {
        w.iter().map(|q| m.get(*q).copied().unwrap_or(0)).sum()
    };
    let change = |p: usize, r: usize| (p > 0).then(|| r1(100.0 * (r as f64 - p as f64) / p as f64));
    for (team, m) in &teams {
        let tr = &s.completions.trend.teams[team];
        let (p, r) = (sum(m, &prior), sum(m, &recent));
        assert_eq!(
            (tr.prior, tr.recent, tr.change_pct),
            (p, r, change(p, r)),
            "{team}"
        );
    }
    let a = &s.completions.trend.all;
    assert_eq!((a.prior, a.recent), (sum(&all, &prior), sum(&all, &recent)));

    // Pinned by hand: ENG 5 -> 9 (+80.0%), all 6 -> 14 (+133.3%).
    let eng = &s.completions.trend.teams["ENG"];
    assert_eq!((eng.prior, eng.recent, eng.change_pct), (5, 9, Some(80.0)));
    assert_eq!((a.prior, a.recent, a.change_pct), (6, 14, Some(133.3)));
    assert_eq!(s.completions.stable.teams, ["ENG"]);
    assert_eq!(s.completions.stable.trend, eng.clone());
}

/// C2.
#[test]
fn golden_lead_cycle() {
    let s = stats();
    let mut lead: BTreeMap<(String, i32), Vec<f64>> = BTreeMap::new();
    let mut cycle: BTreeMap<(String, i32), Vec<f64>> = BTreeMap::new();
    let mut no_start: BTreeMap<(String, i32), usize> = BTreeMap::new();
    for i in issues() {
        let Some(c) = completed(&i) else { continue };
        for group in ["*".to_string(), i.team.to_string()] {
            let key = (group, year(c));
            lead.entry(key.clone())
                .or_default()
                .push(naive_days(i.created, c));
            match i.started {
                Some(st) => cycle.entry(key).or_default().push(naive_days(st, c)),
                None => *no_start.entry(key).or_insert(0) += 1,
            }
        }
    }
    for ((group, y), values) in &lead {
        let got = if group == "*" {
            &s.lead_cycle.all[y]
        } else {
            &s.lead_cycle.teams[group][y]
        };
        let ctx = format!("{group} {y}");
        assert_dist(&got.lead_days, values.clone(), &ctx);
        let key = (group.clone(), *y);
        assert_dist(
            &got.cycle_days,
            cycle.get(&key).cloned().unwrap_or_default(),
            &ctx,
        );
        assert_eq!(
            got.no_started_at,
            no_start.get(&key).copied().unwrap_or(0),
            "{ctx}"
        );
    }
    assert_eq!(s.lead_cycle.all.len(), 4, "years 2023..2026");
}

/// C3, pinned by hand from the six fixture cycles.
#[test]
fn golden_cycles() {
    let c = stats().cycles;
    assert_eq!(
        (c.total, c.completed_non_empty, c.missing_completed_count),
        (6, 4, 1)
    );
    let eng = &c.by_year.teams["ENG"][&2026];
    assert_eq!((eng.cycles, eng.scope, eng.done), (2, 15, 11));
    assert_eq!(eng.completion_rate_pooled_pct, Some(73.3));
    assert_eq!(eng.carryover_issue_slots_per_cycle, Some(2.0));
    assert_eq!(eng.completion_rate_median_cycle_pct, Some(80.0));
    assert_eq!(eng.cycle_length_median_days, Some(10.5));
    let ops = &c.by_year.teams["OPS"][&2025];
    assert_eq!(
        (
            ops.cycles,
            ops.completion_rate_pooled_pct,
            ops.carryover_issue_slots_per_cycle
        ),
        (1, Some(25.0), Some(6.0))
    );
    assert_eq!(c.by_year.all[&2026], eng.clone());
}

/// C5: the histogram, and both readings of tolerance `c5-title-tag-groups`.
#[test]
fn golden_title_tags() {
    let s = stats();
    let ops = &s.title_tags.teams["OPS"];
    assert_eq!(ops, &BTreeMap::from([(0, 5), (1, 2), (2, 1), (3, 2)]));
    assert_eq!(s.title_tags.teams["ENG"], BTreeMap::from([(0, 21)]));
    let published: usize = ops.range(3..).map(|(_, n)| n).sum();
    assert_eq!(published, 2);
    // The reference code's pattern, run here on the raw titles.
    let code = regex::Regex::new(r"^\s*\[[^\]]+\]\s*\[").expect("re");
    let code_count = issues()
        .iter()
        .filter(|i| i.team == "OPS" && code.is_match(i.title))
        .count();
    let two_or_more: usize = ops.range(2..).map(|(_, n)| n).sum();
    assert_eq!(
        code_count,
        two_or_more + 1,
        "the +1 is the unclosed second tag"
    );
}

/// C9, without and with a creation lower bound.
#[test]
fn golden_field_use() {
    for since in [None, Some("2025-01-01")] {
        let mut o = opts();
        o.field_use_created_since = since.map(|d| d.parse::<NaiveDate>().expect("date"));
        let s = stats_with(&o);
        let pop: Vec<Fx> = issues()
            .into_iter()
            .filter(|i| since.is_none_or(|d| i.created >= d))
            .collect();
        let check = |got: &FieldShares, rows: &[&Fx], ctx: &str| {
            let n = rows.len();
            let pct =
                |f: &dyn Fn(&Fx) -> bool| naive_share(rows.iter().filter(|i| f(i)).count(), n);
            assert_eq!(got.n, n, "{ctx}");
            assert_eq!(
                got.estimate_pct,
                pct(&|i| i.estimate.is_some()),
                "{ctx} estimate"
            );
            assert_eq!(got.label_pct, pct(&|i| !i.labels.is_empty()), "{ctx} label");
            assert_eq!(
                got.assignee_pct,
                pct(&|i| i.assignee.is_some()),
                "{ctx} assignee"
            );
            assert_eq!(
                got.project_pct,
                pct(&|i| i.project.is_some()),
                "{ctx} project"
            );
            assert_eq!(got.due_date_pct, pct(&|i| i.due.is_some()), "{ctx} due");
            assert_eq!(got.cycle_pct, pct(&|i| i.cycle.is_some()), "{ctx} cycle");
            assert_eq!(got.parent_pct, pct(&|i| i.parent.is_some()), "{ctx} parent");
        };
        check(
            &s.field_use.shares.all,
            &pop.iter().collect::<Vec<_>>(),
            "all",
        );
        for team in ["ENG", "OPS"] {
            let rows: Vec<&Fx> = pop.iter().filter(|i| i.team == team).collect();
            check(&s.field_use.shares.teams[team], &rows, team);
        }
        let est = &s.field_use.estimating;
        assert_eq!(est.teams, ["ENG"], "OPS does not estimate; OLD is removed");
        let eng: Vec<&Fx> = pop.iter().filter(|i| i.team == "ENG").collect();
        let done: Vec<&&Fx> = eng.iter().filter(|i| i.state == "completed").collect();
        assert_eq!(est.n, eng.len());
        let estimated = |v: &[&Fx]| v.iter().filter(|i| i.estimate.is_some()).count();
        assert_eq!(est.estimated_pct, naive_share(estimated(&eng), eng.len()));
        assert_eq!(est.completed_n, done.len());
        let done_est = done.iter().filter(|i| i.estimate.is_some()).count();
        assert_eq!(
            est.completed_estimated_pct,
            naive_share(done_est, done.len())
        );
        assert_eq!(s.field_use_created_since.as_deref(), since);
    }
}

/// C11, pinned by hand; covers tolerance `c11-same-day-late`.
#[test]
fn golden_projects() {
    let p = stats().projects;
    assert_eq!(p.total, 8, "the removed project is left out");
    assert_eq!(
        p.by_state,
        BTreeMap::from([
            ("canceled".into(), 1),
            ("completed".into(), 4),
            ("planned".into(), 1),
            ("started".into(), 2)
        ])
    );
    assert_eq!(
        p.no_target_by_state,
        BTreeMap::from([("completed".into(), 1), ("planned".into(), 1)])
    );
    assert_eq!(p.started_past_target, 1);
    assert_eq!(p.started_overdue_days.median, Some(38.5));
    assert_eq!(p.completed_with_target, 3);
    assert_eq!((p.late, p.late_same_day, p.late_code_rule), (1, 1, 2));
    assert_eq!(p.late_slip_days.median, Some(4.0), "whole calendar days");
    let l = &p.late_code_rule_slip_days;
    assert_eq!(
        (l.n, l.median, l.p75, l.p90),
        (2, Some(2.5), Some(3.5), Some(4.0))
    );
    assert_eq!(p.lead_set_pct, Some(50.0));
    let in_project = issues().iter().filter(|i| i.project.is_some()).count();
    assert_eq!(p.issues_in_project_pct, naive_share(in_project, 31));
    assert_eq!(
        (p.milestones, p.milestones_with_target_pct),
        (3, Some(33.3))
    );
}

/// C12.
#[test]
fn golden_cancel_rate() {
    let s = stats();
    let mut want: BTreeMap<(String, i32), [usize; 3]> = BTreeMap::new();
    for i in issues() {
        let Some(closed_at) = i.completed.or(i.canceled) else {
            continue;
        };
        let slot = match i.state {
            "completed" => 0,
            "canceled" => 1,
            "duplicate" => 2,
            _ => continue,
        };
        for g in ["*".to_string(), i.team.to_string()] {
            want.entry((g, year(closed_at))).or_insert([0; 3])[slot] += 1;
        }
    }
    for ((g, y), [done, canceled, dup]) in &want {
        let got = if g == "*" {
            &s.cancel_rate.all[y]
        } else {
            &s.cancel_rate.teams[g][y]
        };
        let closed = done + canceled + dup;
        assert_eq!(
            (got.closed, got.completed, got.canceled, got.duplicate),
            (closed, *done, *canceled, *dup),
            "{g} {y}"
        );
        assert_eq!(got.cancel_pct, naive_share(*canceled, closed), "{g} {y}");
    }
    assert_eq!(
        s.cancel_rate.all[&2025].duplicate, 1,
        "duplicate counts in the denominator"
    );
}

/// C14, without and with a completion lower bound.
#[test]
fn golden_people() {
    for since in [None, Some("2026-01-01")] {
        let mut o = opts();
        o.concentration_completed_since = since.map(|d| d.parse::<NaiveDate>().expect("date"));
        let s = stats_with(&o);
        assert_eq!((s.people.users, s.people.active_users), (3, 2));
        let mut contributors: BTreeMap<(String, i32), BTreeSet<&str>> = BTreeMap::new();
        let mut counts: BTreeMap<String, (usize, HashMap<&str, usize>)> = BTreeMap::new();
        for i in issues() {
            let Some(c) = completed(&i) else { continue };
            for g in ["*".to_string(), i.team.to_string()] {
                if let Some(a) = i.assignee {
                    contributors
                        .entry((g.clone(), year(c)))
                        .or_default()
                        .insert(a);
                }
                if since.is_none_or(|d| c >= d) {
                    let e = counts.entry(g).or_default();
                    e.0 += 1;
                    if let Some(a) = i.assignee {
                        *e.1.entry(a).or_insert(0) += 1;
                    }
                }
            }
        }
        for ((g, y), set) in &contributors {
            let got = if g == "*" {
                s.people.contributors.all[y]
            } else {
                s.people.contributors.teams[g][y]
            };
            assert_eq!(got, set.len(), "contributors {g} {y}");
        }
        for (g, (done, per)) in &counts {
            let got = if g == "*" {
                &s.people.concentration.all
            } else {
                &s.people.concentration.teams[g]
            };
            let assigned: usize = per.values().sum();
            let mut v: Vec<usize> = per.values().copied().collect();
            v.sort_unstable_by(|a, b| b.cmp(a));
            let top = |k: usize| naive_share(v.iter().take(k).sum(), assigned);
            assert_eq!(
                (got.completed, got.assigned, got.unassigned, got.assignees),
                (*done, assigned, done - assigned, per.len()),
                "{g}"
            );
            assert_eq!(
                (got.top1_pct, got.top3_pct, got.top5_pct),
                (top(1), top(3), top(5)),
                "{g}"
            );
        }
    }
}

#[test]
fn output_names_every_tolerance() {
    let ids: Vec<&str> = stats().tolerances.iter().map(|t| t.id).collect();
    assert_eq!(
        ids,
        [
            "c3-carryover-slots",
            "c5-title-tag-groups",
            "c11-same-day-late"
        ]
    );
}

fn single(f: impl FnOnce(&mut Fx)) -> Database {
    let db = Database::open_in_memory().expect("db");
    let mut row = issues().remove(1);
    f(&mut row);
    insert_issues(&db, &[row]);
    db
}

#[test]
fn unparseable_timestamp_fails_with_the_issue_named() {
    let db = single(|i| i.completed = Some("not-a-time"));
    let err = compute_linear_stats(db.connection(), &opts()).expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("ENG-2") && msg.contains("not-a-time"), "{msg}");
}

#[test]
fn missing_created_at_fails_with_the_issue_named() {
    let db = single(|_| {});
    db.connection()
        .execute("UPDATE linear_issues SET created_at = NULL", [])
        .expect("update");
    let err = compute_linear_stats(db.connection(), &opts()).expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("ENG-2") && msg.contains("created_at"), "{msg}");
}

#[test]
fn missing_cycle_end_fails_with_the_cycle_named() {
    let db = seeded();
    db.connection()
        .execute(
            "UPDATE linear_cycles SET ends_at = NULL WHERE id = 'cyc-2'",
            [],
        )
        .expect("update");
    let err = compute_linear_stats(db.connection(), &opts()).expect_err("must fail");
    assert!(err.to_string().contains("cyc-2"), "{err}");
}

#[test]
fn unparseable_label_list_fails_with_the_issue_named() {
    let db = single(|_| {});
    db.connection()
        .execute("UPDATE linear_issues SET label_ids = 'lbl-bug,lbl-ui'", [])
        .expect("update");
    let err = compute_linear_stats(db.connection(), &opts()).expect_err("must fail");
    assert!(err.to_string().contains("ENG-2"), "{err}");
}

#[test]
fn unparseable_target_date_fails_with_the_project_named() {
    let db = seeded();
    db.connection()
        .execute(
            "UPDATE linear_projects SET target_date = '01/03/2026' WHERE id = 'prj-1'",
            [],
        )
        .expect("update");
    let err = compute_linear_stats(db.connection(), &opts()).expect_err("must fail");
    assert!(err.to_string().contains("prj-1"), "{err}");
}

/// A row written before the Linear id was stored is counted, never measured.
#[test]
fn rows_without_linear_id_are_counted_and_left_out() {
    let db = seeded();
    db.connection()
        .execute(
            "INSERT INTO linear_issues (identifier, title, state, team, team_key, fetched_at, \
             state_type, created_at, completed_at) VALUES ('ENG-90', 't', 'Done', 'ENG', 'ENG', \
             'x', 'completed', '2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z')",
            [],
        )
        .expect("insert");
    let s = compute_linear_stats(db.connection(), &opts()).expect("stats");
    assert_eq!(
        (s.population.issues, s.population.rows_without_linear_id),
        (31, 1)
    );
    assert_eq!(
        s.completions.per_quarter.all,
        stats().completions.per_quarter.all
    );
}

/// A completed issue with no completion time is counted, never dated.
#[test]
fn completed_without_completed_at_is_counted_not_dated() {
    let db = seeded();
    let mut row = issues().remove(1);
    row.id = "ENG-91";
    row.completed = None;
    insert_issues(&db, &[row]);
    let s = compute_linear_stats(db.connection(), &opts()).expect("stats");
    assert_eq!(s.population.completed_without_completed_at, 1);
    assert_eq!(
        s.completions.per_quarter.all,
        stats().completions.per_quarter.all
    );
    assert_eq!(s.lead_cycle.all, stats().lead_cycle.all);
}

/// A cycle whose team row is missing shows under its team id.
#[test]
fn cycle_with_unknown_team_keeps_its_team_id() {
    let db = seeded();
    db.connection()
        .execute(
            "INSERT INTO linear_cycles (id, team_id, starts_at, ends_at, completed_at, \
             scope_count, completed_count, raw_json, fetched_at) VALUES ('cyc-9', 'team-xyz', \
             '2026-01-05T00:00:00Z', '2026-01-19T00:00:00Z', '2026-01-19T00:00:00Z', 3, 1, '{}', 'x')",
            [],
        )
        .expect("insert");
    let s = compute_linear_stats(db.connection(), &opts()).expect("stats");
    assert_eq!(s.cycles.by_year.teams["team-xyz"][&2026].scope, 3);
}

#[test]
fn empty_database_yields_zero_counts_not_an_error() {
    let db = Database::open_in_memory().expect("db");
    let s = compute_linear_stats(db.connection(), &opts()).expect("stats");
    assert_eq!(s.population.issues, 0);
    assert_eq!(s.completions.trend.all.change_pct, None);
    assert!(s.completions.stable.teams.is_empty());
}

#[test]
fn markdown_block_names_each_metric() {
    let s = stats();
    let md = render_markdown(&s);
    for needle in [
        "## Linear delivery",
        "As of 2026-10-09T12:00:00",
        "| ENG | 5 | 9 | 80.0 |",
        "Stable teams: ENG",
        "c11-same-day-late",
        "Late (later calendar day): 1",
    ] {
        assert!(md.contains(needle), "missing {needle:?} in:\n{md}");
    }
}

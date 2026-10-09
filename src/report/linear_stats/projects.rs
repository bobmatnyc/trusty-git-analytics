//! Cycle (C3) and project (C11) metrics (#190 step 4).

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Utc};

use super::dist::{days, dist, midnight, percentile, round1, share};
use super::issues::{add, map_grouped};
use super::load::{CycleRow, Snapshot};
use super::model::{CycleStats, CycleYear, Grouped, ProjectStats};

#[derive(Default)]
struct CycleAcc {
    cycles: usize,
    scope: i64,
    done: i64,
    rates: Vec<f64>,
    lengths: Vec<f64>,
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    xs.sort_by(f64::total_cmp);
    percentile(&xs, 0.5)
}

/// C3: completed cycles whose last issue count is above 0, by team and UTC
/// year of `endsAt`. Scope and done are the last values of the issue-count
/// and completed-issue-count histories; carry-over is `scope - done` summed
/// over cycles (issue slots, tolerance `c3-carryover-slots`).
pub(super) fn cycles(rows: &[CycleRow]) -> CycleStats {
    let mut stats = CycleStats {
        total: rows.len(),
        ..CycleStats::default()
    };
    let mut acc: Grouped<BTreeMap<i32, CycleAcc>> = Grouped::default();
    for c in rows {
        let scope = c.scope.unwrap_or(0);
        if !c.completed || scope <= 0 {
            continue;
        }
        stats.completed_non_empty += 1;
        let Some(done) = c.done else {
            stats.missing_completed_count += 1;
            continue;
        };
        let length = days(c.starts_at, c.ends_at);
        add(&mut acc, &c.team, |m| {
            let a = m.entry(c.ends_at.year()).or_default();
            a.cycles += 1;
            a.scope += scope;
            a.done += done;
            a.rates.push(done as f64 / scope as f64);
            a.lengths.push(length);
        });
    }
    stats.by_year = map_grouped(acc, |m| {
        m.into_iter()
            .map(|(y, a)| {
                let cy = CycleYear {
                    cycles: a.cycles,
                    scope: a.scope,
                    done: a.done,
                    completion_rate_pooled_pct: share(a.done, a.scope),
                    carryover_issue_slots_per_cycle: Some(round1(
                        (a.scope - a.done) as f64 / a.cycles as f64,
                    )),
                    completion_rate_median_cycle_pct: median(a.rates).map(|r| round1(100.0 * r)),
                    cycle_length_median_days: median(a.lengths).map(round1),
                };
                (y, cy)
            })
            .collect()
    });
    stats
}

/// C11: projects not marked removed. Slip is measured from midnight UTC of
/// the target date; see tolerance `c11-same-day-late` for the two late
/// rules.
pub(super) fn projects(snap: &Snapshot, as_of: DateTime<Utc>) -> ProjectStats {
    let total = snap.projects.len();
    let mut p = ProjectStats {
        total,
        milestones: snap.milestones,
        ..ProjectStats::default()
    };
    let (mut overdue, mut late_days, mut code_days) = (Vec::new(), Vec::new(), Vec::new());
    let mut leads = 0_i64;
    for pr in &snap.projects {
        *p.by_state.entry(pr.state.clone()).or_insert(0) += 1;
        leads += i64::from(pr.has_lead);
        let Some(target) = pr.target_date else {
            *p.no_target_by_state.entry(pr.state.clone()).or_insert(0) += 1;
            continue;
        };
        if pr.state == "started" && target < as_of.date_naive() {
            p.started_past_target += 1;
            overdue.push(days(midnight(target), as_of));
        }
        let Some(done) = pr.completed_at.filter(|_| pr.state == "completed") else {
            continue;
        };
        p.completed_with_target += 1;
        let slip = days(midnight(target), done);
        if done.date_naive() > target {
            p.late += 1;
            late_days.push((done.date_naive() - target).num_days() as f64);
        } else if slip > 0.0 {
            p.late_same_day += 1;
        }
        if slip > 0.0 {
            p.late_code_rule += 1;
            code_days.push(slip);
        }
    }
    p.started_overdue_days = dist(&overdue);
    p.late_slip_days = dist(&late_days);
    p.late_code_rule_slip_days = dist(&code_days);
    p.lead_set_pct = share(leads, total as i64);
    let in_project = snap.issues.iter().filter(|i| i.has_project).count();
    p.issues_in_project_pct = share(in_project as i64, snap.issues.len() as i64);
    p.milestones_with_target_pct =
        share(snap.milestones_with_target as i64, snap.milestones as i64);
    p
}

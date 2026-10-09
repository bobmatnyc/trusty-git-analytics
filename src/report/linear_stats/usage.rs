//! Field use (C9) and people (C14) (#190 step 4).
//!
//! Assignees are counted by Linear user id. No name or email reaches the
//! output.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::{Datelike, NaiveDate};

use super::dist::share;
use super::issues::{add, map_grouped};
use super::load::{IssueRow, Snapshot};
use super::model::{Concentration, EstimatingTeams, FieldShares, FieldUse, Grouped, People};

#[derive(Default)]
struct FieldAcc {
    n: i64,
    estimate: i64,
    label: i64,
    assignee: i64,
    project: i64,
    due: i64,
    cycle: i64,
    parent: i64,
}

impl FieldAcc {
    fn push(&mut self, i: &IssueRow) {
        self.n += 1;
        self.estimate += i64::from(i.has_estimate);
        self.label += i64::from(i.has_labels);
        self.assignee += i64::from(i.assignee_id.is_some());
        self.project += i64::from(i.has_project);
        self.due += i64::from(i.has_due_date);
        self.cycle += i64::from(i.has_cycle);
        self.parent += i64::from(i.has_parent);
    }

    fn shares(self) -> FieldShares {
        FieldShares {
            n: usize::try_from(self.n).unwrap_or(0),
            estimate_pct: share(self.estimate, self.n),
            label_pct: share(self.label, self.n),
            assignee_pct: share(self.assignee, self.n),
            project_pct: share(self.project, self.n),
            due_date_pct: share(self.due, self.n),
            cycle_pct: share(self.cycle, self.n),
            parent_pct: share(self.parent, self.n),
        }
    }
}

/// C9: share of issues with each field set, every state, archived included,
/// created on or after `since` (UTC date) when given. The estimating block
/// covers teams whose current estimation type is set and not `notUsed`.
pub(super) fn field_use(snap: &Snapshot, since: Option<NaiveDate>) -> FieldUse {
    let in_scope = |i: &&IssueRow| since.is_none_or(|d| i.created_at.date_naive() >= d);
    let mut acc: Grouped<FieldAcc> = Grouped::default();
    for i in snap.issues.iter().filter(in_scope) {
        add(&mut acc, &i.team, |a| a.push(i));
    }
    let estimating: BTreeSet<&str> = snap
        .teams
        .iter()
        .filter(|t| t.estimation_type.as_deref().is_some_and(|e| e != "notUsed"))
        .map(|t| t.key.as_str())
        .collect();
    let (mut n, mut est, mut done, mut done_est) = (0_i64, 0_i64, 0_i64, 0_i64);
    for i in snap.issues.iter().filter(in_scope) {
        if !estimating.contains(i.team.as_str()) {
            continue;
        }
        n += 1;
        est += i64::from(i.has_estimate);
        if i.is("completed") {
            done += 1;
            done_est += i64::from(i.has_estimate);
        }
    }
    FieldUse {
        shares: map_grouped(acc, FieldAcc::shares),
        estimating: EstimatingTeams {
            teams: estimating.iter().map(|k| (*k).to_string()).collect(),
            n: usize::try_from(n).unwrap_or(0),
            estimated_pct: share(est, n),
            completed_n: usize::try_from(done).unwrap_or(0),
            completed_estimated_pct: share(done_est, done),
        },
    }
}

#[derive(Default)]
struct ConcAcc {
    completed: usize,
    per_assignee: HashMap<String, usize>,
}

impl ConcAcc {
    fn finish(self) -> Concentration {
        let assigned: usize = self.per_assignee.values().sum();
        let mut counts: Vec<usize> = self.per_assignee.values().copied().collect();
        counts.sort_unstable_by(|a, b| b.cmp(a));
        let top = |k: usize| share(counts.iter().take(k).sum::<usize>() as i64, assigned as i64);
        Concentration {
            completed: self.completed,
            assigned,
            unassigned: self.completed - assigned,
            assignees: counts.len(),
            top1_pct: top(1),
            top3_pct: top(3),
            top5_pct: top(5),
        }
    }
}

/// C14: distinct assignees of completed issues per UTC completion year, and
/// the share of completed issues held by the busiest one, three and five
/// assignees among issues completed on or after `since` (UTC date) when
/// given. The share denominator is the assigned issues.
pub(super) fn people(snap: &Snapshot, since: Option<NaiveDate>) -> People {
    let mut contributors: Grouped<BTreeMap<i32, BTreeSet<&str>>> = Grouped::default();
    let mut conc: Grouped<ConcAcc> = Grouped::default();
    for i in &snap.issues {
        let Some(c) = i.completed_at.filter(|_| i.is("completed")) else {
            continue;
        };
        if let Some(a) = i.assignee_id.as_deref() {
            add(&mut contributors, &i.team, |m| {
                m.entry(c.year()).or_default().insert(a);
            });
        }
        if since.is_none_or(|d| c.date_naive() >= d) {
            add(&mut conc, &i.team, |acc| {
                acc.completed += 1;
                if let Some(a) = &i.assignee_id {
                    *acc.per_assignee.entry(a.clone()).or_insert(0) += 1;
                }
            });
        }
    }
    People {
        users: snap.users,
        active_users: snap.active_users,
        contributors: map_grouped(contributors, |m| {
            m.into_iter().map(|(y, s)| (y, s.len())).collect()
        }),
        concentration: map_grouped(conc, ConcAcc::finish),
    }
}

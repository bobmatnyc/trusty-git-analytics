//! Issue-level metrics: C0, C1/C1b/C1c, C2, C5 and C12 (#190 step 4).
//!
//! Each function is one pass over the issue rows. "Completed" is
//! `state.type == completed`; a completed issue with no `completedAt` is
//! counted in C0 and dated nowhere.

use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Utc};

use super::dist::{days, dist, quarter, quarter_label, quarter_minus, quarter_of, share};
use super::load::{IssueRow, Snapshot};
use super::model::{CancelYear, Completions, Grouped, LeadCycle, Population, StableTrend, Trend};

/// Apply `f` to the workspace entry and to `team`'s entry.
pub(super) fn add<T: Default>(g: &mut Grouped<T>, team: &str, mut f: impl FnMut(&mut T)) {
    f(&mut g.all);
    f(g.teams.entry(team.to_string()).or_default());
}

/// Map every entry of `g` through `f`.
pub(super) fn map_grouped<A, B>(g: Grouped<A>, f: impl Fn(A) -> B) -> Grouped<B> {
    Grouped {
        all: f(g.all),
        teams: g.teams.into_iter().map(|(k, v)| (k, f(v))).collect(),
    }
}

/// The issue's `completedAt`, when it is a completed issue.
fn completion(i: &IssueRow) -> Option<DateTime<Utc>> {
    i.completed_at.filter(|_| i.is("completed"))
}

/// C0.
pub(super) fn population(snap: &Snapshot) -> Population {
    let mut p = Population {
        issues: snap.issues.len(),
        rows_without_linear_id: snap.rows_without_linear_id,
        teams: snap.teams.len(),
        users: snap.users,
        labels: snap.labels,
        projects: snap.projects.len(),
        milestones: snap.milestones,
        cycles: snap.cycles.len(),
        ..Population::default()
    };
    for i in &snap.issues {
        *p.issues_by_team.entry(i.team.clone()).or_insert(0) += 1;
        match &i.state_type {
            Some(t) => *p.by_state_type.entry(t.clone()).or_insert(0) += 1,
            None => p.without_state_type += 1,
        }
        if i.archived {
            p.archived += 1;
        }
        if i.is("completed") && i.completed_at.is_none() {
            p.completed_without_completed_at += 1;
        }
    }
    p.archived_pct = share(p.archived as i64, p.issues as i64);
    p
}

/// C1 (per quarter), C1b (prior vs recent four quarters), C1c (stable teams).
///
/// The recent window is the four whole quarters before the as-of quarter;
/// the prior window the four before those. A team is stable when it has a
/// completion in each of the eight.
pub(super) fn completions(issues: &[IssueRow], as_of: DateTime<Utc>) -> Completions {
    let mut per_quarter: Grouped<BTreeMap<String, usize>> = Grouped::default();
    for i in issues {
        if let Some(c) = completion(i) {
            let q = quarter(c);
            add(&mut per_quarter, &i.team, |m| {
                *m.entry(q.clone()).or_insert(0) += 1
            });
        }
    }
    let now = quarter_of(as_of);
    let window = |from: u32| -> Vec<String> {
        (from..from + 4)
            .rev()
            .map(|back| quarter_label(quarter_minus(now, back)))
            .collect()
    };
    let (prior_window, recent_window) = (window(5), window(1));
    let sum = |m: &BTreeMap<String, usize>, w: &[String]| -> usize {
        w.iter().map(|q| m.get(q).copied().unwrap_or(0)).sum()
    };
    let trend_of = |prior: usize, recent: usize| Trend {
        prior,
        recent,
        change_pct: share(recent as i64 - prior as i64, prior as i64),
    };
    let of_map =
        |m: &BTreeMap<String, usize>| trend_of(sum(m, &prior_window), sum(m, &recent_window));
    let trend = Grouped {
        all: of_map(&per_quarter.all),
        teams: per_quarter
            .teams
            .iter()
            .map(|(k, m)| (k.clone(), of_map(m)))
            .collect(),
    };
    let stable_teams: Vec<String> = per_quarter
        .teams
        .iter()
        .filter(|(_, m)| {
            prior_window
                .iter()
                .chain(&recent_window)
                .all(|q| m.get(q).is_some_and(|n| *n > 0))
        })
        .map(|(k, _)| k.clone())
        .collect();
    let (sp, sr) = stable_teams.iter().fold((0, 0), |(p, r), k| {
        (p + trend.teams[k].prior, r + trend.teams[k].recent)
    });
    Completions {
        per_quarter,
        prior_window,
        recent_window,
        trend,
        stable: StableTrend {
            teams: stable_teams,
            trend: trend_of(sp, sr),
        },
    }
}

#[derive(Default)]
struct LeadAcc {
    lead: Vec<f64>,
    cycle: Vec<f64>,
    no_start: usize,
}

/// C2: lead (`createdAt` to `completedAt`) and cycle (`startedAt` to
/// `completedAt`) by UTC year of completion. A negative span is dropped; an
/// issue with no `startedAt` stays in lead time and counts in
/// `no_started_at`.
pub(super) fn lead_cycle(issues: &[IssueRow]) -> Grouped<BTreeMap<i32, LeadCycle>> {
    let mut acc: Grouped<BTreeMap<i32, LeadAcc>> = Grouped::default();
    for i in issues {
        let Some(c) = completion(i) else { continue };
        let lead = days(i.created_at, c);
        let cycle = i.started_at.map(|s| days(s, c));
        add(&mut acc, &i.team, |m| {
            let a = m.entry(c.year()).or_default();
            if lead >= 0.0 {
                a.lead.push(lead);
            }
            match cycle {
                Some(d) if d >= 0.0 => a.cycle.push(d),
                Some(_) => {}
                None => a.no_start += 1,
            }
        });
    }
    map_grouped(acc, |m| {
        m.into_iter()
            .map(|(y, a)| {
                let lc = LeadCycle {
                    lead_days: dist(&a.lead),
                    cycle_days: dist(&a.cycle),
                    no_started_at: a.no_start,
                };
                (y, lc)
            })
            .collect()
    })
}

/// Number of complete bracketed tags (`[...]`, at least one character) a
/// title starts with, whitespace allowed before and between them.
pub(super) fn leading_tags(title: &str) -> usize {
    let mut rest = title;
    let mut n = 0;
    loop {
        rest = rest.trim_start();
        let Some(after) = rest.strip_prefix('[') else {
            return n;
        };
        match after.find(']') {
            Some(end) if end > 0 => {
                n += 1;
                rest = &after[end + 1..];
            }
            _ => return n,
        }
    }
}

/// C5: issues per number of leading bracketed title tags. See tolerance
/// `c5-title-tag-groups` for how the two readings sum from it.
pub(super) fn title_tags(issues: &[IssueRow]) -> Grouped<BTreeMap<usize, usize>> {
    let mut g: Grouped<BTreeMap<usize, usize>> = Grouped::default();
    for i in issues {
        let n = leading_tags(&i.title);
        add(&mut g, &i.team, |m| *m.entry(n).or_insert(0) += 1);
    }
    g
}

/// C12: per UTC closure year (`completedAt`, else `canceledAt`), completed,
/// canceled and duplicate issues. Duplicates count in the denominator only.
pub(super) fn cancel_rate(issues: &[IssueRow]) -> Grouped<BTreeMap<i32, CancelYear>> {
    let mut g: Grouped<BTreeMap<i32, CancelYear>> = Grouped::default();
    for i in issues {
        let Some(closed_at) = i.completed_at.or(i.canceled_at) else {
            continue;
        };
        let slot = match i.state_type.as_deref() {
            Some("completed") => 0,
            Some("canceled") => 1,
            Some("duplicate") => 2,
            _ => continue,
        };
        add(&mut g, &i.team, |m| {
            let y = m.entry(closed_at.year()).or_default();
            y.closed += 1;
            match slot {
                0 => y.completed += 1,
                1 => y.canceled += 1,
                _ => y.duplicate += 1,
            }
        });
    }
    map_grouped(g, |m| {
        m.into_iter()
            .map(|(year, mut y)| {
                y.cancel_pct = share(y.canceled as i64, y.closed as i64);
                (year, y)
            })
            .collect()
    })
}

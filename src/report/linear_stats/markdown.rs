//! [`LinearStats`] as a Markdown report block (#190 step 4).
//!
//! Why: the JSON carries every figure; a reader needs the headline tables.
//! What: [`render_markdown`] writes one `##` section with a `###` block per
//! metric. A missing value prints as `-`.
//! Test: `report::linear_stats::tests::markdown_block_names_each_metric`.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::dist::Dist;
use super::model::{Grouped, LinearStats};

fn f(v: Option<f64>) -> String {
    v.map_or_else(|| "-".to_string(), |x| format!("{x:.1}"))
}

fn d(v: &Dist) -> String {
    format!("n {}, median {}, p90 {}", v.n, f(v.median), f(v.p90))
}

/// Rows for the workspace (`All`) and then each team.
fn rows<T>(g: &Grouped<T>) -> impl Iterator<Item = (&str, &T)> {
    std::iter::once(("All", &g.all)).chain(g.teams.iter().map(|(k, v)| (k.as_str(), v)))
}

fn counts<K: std::fmt::Display>(m: &BTreeMap<K, usize>) -> String {
    let parts: Vec<String> = m.iter().map(|(k, v)| format!("{k} {v}")).collect();
    if parts.is_empty() {
        "none".to_string()
    } else {
        parts.join(", ")
    }
}

/// Render `s` as a Markdown block headed `## Linear delivery`.
#[must_use]
pub fn render_markdown(s: &LinearStats) -> String {
    let mut o = String::new();
    // `write!` into a `String` cannot fail.
    let _ = writeln!(o, "## Linear delivery\n");
    let _ = writeln!(
        o,
        "As of {}. Teams by current team key; archived issues included; UTC quarters; \
         percentiles by linear interpolation (type 7).\n",
        s.as_of
    );
    population(&mut o, s);
    completions(&mut o, s);
    lead_cycle(&mut o, s);
    cycles(&mut o, s);
    let _ = writeln!(o, "### Title tags (C5)\n");
    for (team, m) in rows(&s.title_tags) {
        let _ = writeln!(o, "- {team}: issues by leading tag count: {}", counts(m));
    }
    let _ = writeln!(o);
    field_use(&mut o, s);
    projects(&mut o, s);
    cancel_rate(&mut o, s);
    people(&mut o, s);
    let _ = writeln!(o, "### Tolerances\n");
    for t in &s.tolerances {
        let _ = writeln!(
            o,
            "- `{}` ({}): published: {}. Code: {}. Fields: {}.",
            t.id, t.metric, t.published, t.code, t.fields
        );
    }
    o
}

fn population(o: &mut String, s: &LinearStats) {
    let p = &s.population;
    let _ = writeln!(o, "### Population (C0)\n");
    let _ = writeln!(
        o,
        "Issues: {} ({} archived, {}%). Teams {}, users {}, labels {}, projects {}, \
         milestones {}, cycles {}.",
        p.issues,
        p.archived,
        f(p.archived_pct),
        p.teams,
        p.users,
        p.labels,
        p.projects,
        p.milestones,
        p.cycles
    );
    let _ = writeln!(o, "State types: {}.", counts(&p.by_state_type));
    let _ = writeln!(
        o,
        "Not measured: {} row(s) without a Linear id, {} issue(s) without a state type, \
         {} completed issue(s) without a completion time.\n",
        p.rows_without_linear_id, p.without_state_type, p.completed_without_completed_at
    );
}

fn completions(o: &mut String, s: &LinearStats) {
    let c = &s.completions;
    let _ = writeln!(o, "### Completions (C1, C1b, C1c)\n");
    let _ = writeln!(
        o,
        "Prior window {}; recent window {}.\n",
        c.prior_window.join(", "),
        c.recent_window.join(", ")
    );
    let _ = writeln!(o, "| Team | Prior | Recent | Change % |\n|---|---|---|---|");
    for (team, t) in rows(&c.trend) {
        let _ = writeln!(
            o,
            "| {team} | {} | {} | {} |",
            t.prior,
            t.recent,
            f(t.change_pct)
        );
    }
    let st = &c.stable;
    let _ = writeln!(
        o,
        "\nStable teams: {} ({} -> {}, {}%).\n",
        if st.teams.is_empty() {
            "none".to_string()
        } else {
            st.teams.join(", ")
        },
        st.trend.prior,
        st.trend.recent,
        f(st.trend.change_pct)
    );
}

fn lead_cycle(o: &mut String, s: &LinearStats) {
    let _ = writeln!(o, "### Lead and cycle time, days (C2)\n");
    let _ = writeln!(
        o,
        "| Team | Year | Lead | Cycle | No start |\n|---|---|---|---|---|"
    );
    for (team, m) in rows(&s.lead_cycle) {
        for (y, lc) in m {
            let _ = writeln!(
                o,
                "| {team} | {y} | {} | {} | {} |",
                d(&lc.lead_days),
                d(&lc.cycle_days),
                lc.no_started_at
            );
        }
    }
    let _ = writeln!(o);
}

fn cycles(o: &mut String, s: &LinearStats) {
    let c = &s.cycles;
    let _ = writeln!(o, "### Cycles (C3)\n");
    let _ = writeln!(
        o,
        "{} cycles, {} completed and non-empty, {} without a completed count.\n",
        c.total, c.completed_non_empty, c.missing_completed_count
    );
    let _ = writeln!(
        o,
        "| Team | Year | Cycles | Done % | Carry-over slots per cycle | Median length (days) |\n\
         |---|---|---|---|---|---|"
    );
    for (team, m) in rows(&c.by_year) {
        for (y, cy) in m {
            let _ = writeln!(
                o,
                "| {team} | {y} | {} | {} | {} | {} |",
                cy.cycles,
                f(cy.completion_rate_pooled_pct),
                f(cy.carryover_issue_slots_per_cycle),
                f(cy.cycle_length_median_days)
            );
        }
    }
    let _ = writeln!(o);
}

fn field_use(o: &mut String, s: &LinearStats) {
    let _ = writeln!(o, "### Field use (C9)\n");
    if let Some(since) = &s.field_use_created_since {
        let _ = writeln!(o, "Issues created on or after {since}.\n");
    }
    let _ = writeln!(
        o,
        "| Team | n | Assignee % | Cycle % | Project % | Label % | Estimate % | Due % | Parent % |\n\
         |---|---|---|---|---|---|---|---|---|"
    );
    for (team, x) in rows(&s.field_use.shares) {
        let _ = writeln!(
            o,
            "| {team} | {} | {} | {} | {} | {} | {} | {} | {} |",
            x.n,
            f(x.assignee_pct),
            f(x.cycle_pct),
            f(x.project_pct),
            f(x.label_pct),
            f(x.estimate_pct),
            f(x.due_date_pct),
            f(x.parent_pct)
        );
    }
    let e = &s.field_use.estimating;
    let _ = writeln!(
        o,
        "\nEstimating teams ({}): {}% of {} issues estimated, {}% of {} completed.\n",
        e.teams.join(", "),
        f(e.estimated_pct),
        e.n,
        f(e.completed_estimated_pct),
        e.completed_n
    );
}

fn projects(o: &mut String, s: &LinearStats) {
    let p = &s.projects;
    let _ = writeln!(o, "### Projects (C11)\n");
    let _ = writeln!(o, "Projects: {} ({}).", p.total, counts(&p.by_state));
    let _ = writeln!(o, "No target date: {}.", counts(&p.no_target_by_state));
    let _ = writeln!(
        o,
        "Started past target: {} (overdue days {}).",
        p.started_past_target,
        d(&p.started_overdue_days)
    );
    let _ = writeln!(
        o,
        "Completed with a target: {}. Late (later calendar day): {} (slip days {}). \
         Finished on the target day: {}. Late under the code rule: {} (slip days {}).",
        p.completed_with_target,
        p.late,
        d(&p.late_slip_days),
        p.late_same_day,
        p.late_code_rule,
        d(&p.late_code_rule_slip_days)
    );
    let _ = writeln!(
        o,
        "Lead set: {}%. Issues in a project: {}%. Milestones with a target: {}% of {}.\n",
        f(p.lead_set_pct),
        f(p.issues_in_project_pct),
        f(p.milestones_with_target_pct),
        p.milestones
    );
}

fn cancel_rate(o: &mut String, s: &LinearStats) {
    let _ = writeln!(o, "### Cancel rate (C12)\n");
    let _ = writeln!(
        o,
        "| Team | Year | Closed | Canceled | Duplicate | Cancel % |\n|---|---|---|---|---|---|"
    );
    for (team, m) in rows(&s.cancel_rate) {
        for (y, c) in m {
            let _ = writeln!(
                o,
                "| {team} | {y} | {} | {} | {} | {} |",
                c.closed,
                c.canceled,
                c.duplicate,
                f(c.cancel_pct)
            );
        }
    }
    let _ = writeln!(o);
}

fn people(o: &mut String, s: &LinearStats) {
    let p = &s.people;
    let _ = writeln!(o, "### People (C14)\n");
    let _ = writeln!(o, "Users: {} ({} active).", p.users, p.active_users);
    for (team, m) in rows(&p.contributors) {
        let _ = writeln!(
            o,
            "- {team}: contributors by completion year: {}",
            counts(m)
        );
    }
    if let Some(since) = &s.concentration_completed_since {
        let _ = writeln!(
            o,
            "\nConcentration over issues completed on or after {since}."
        );
    }
    let _ = writeln!(
        o,
        "\n| Team | Completed | Unassigned | Assignees | Top-1 % | Top-3 % | Top-5 % |\n\
         |---|---|---|---|---|---|---|"
    );
    for (team, c) in rows(&p.concentration) {
        let _ = writeln!(
            o,
            "| {team} | {} | {} | {} | {} | {} | {} |",
            c.completed,
            c.unassigned,
            c.assignees,
            f(c.top1_pct),
            f(c.top3_pct),
            f(c.top5_pct)
        );
    }
    let _ = writeln!(o);
}

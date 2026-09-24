//! Markdown rendering of a [`ScoreReport`].

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::records::StrataSummary;
use super::score::{PrecisionRow, ScoreReport};

fn pct(v: Option<f64>) -> String {
    v.map_or_else(|| "—".to_string(), |v| format!("{:.1}%", v * 100.0))
}

fn table(out: &mut String, title: &str, key: &str, rows: &[PrecisionRow]) {
    let _ = writeln!(out, "## {title}\n");
    if rows.is_empty() {
        out.push_str("No labelled commits.\n\n");
        return;
    }
    let _ = writeln!(
        out,
        "| {key} | n | correct | precision | 95% CI (Wilson) | no answer |"
    );
    out.push_str("|---|---:|---:|---:|---|---:|\n");
    for r in rows {
        let ci = match (r.ci_low, r.ci_high) {
            (Some(lo), Some(hi)) => format!("[{:.3}, {:.3}]", lo, hi),
            _ => "—".to_string(),
        };
        let _ = writeln!(
            out,
            "| `{}` | {} | {} | {} | {} | {} |",
            r.key,
            r.n,
            r.correct,
            pct(r.precision),
            ci,
            r.excluded
        );
    }
    out.push('\n');
}

/// Render the report as Markdown.
pub(crate) fn render(report: &ScoreReport, strata: &StrataSummary) -> String {
    let mut out = String::new();
    out.push_str("# Classifier precision report\n\n");
    out.push_str("> Contains commit-derived text. Store privately, outside any repository.\n\n");
    let _ = writeln!(
        out,
        "Window {} → {} ({} weeks), population {}, seed {}, cap {}.\n",
        strata.window_start,
        strata.window_end,
        strata.weeks,
        strata.population,
        strata.seed,
        strata.cap
    );
    // #111: no-answer counts per label, so release_merge shows on its own.
    let _ = writeln!(
        out,
        "Sample {} · merges excluded {} · labelled {} · scored {} · unclear {} · mixed {} · \
         release_merge {} · unresolved disagreements {}\n",
        report.sample_size,
        report.merges_excluded,
        report.labelled,
        report.scored,
        report.unclear,
        report.mixed,
        report.release_merge,
        report.unresolved_disagreements
    );

    out.push_str("## Summary\n\n");
    let _ = writeln!(
        out,
        "- Precision and coverage use the labels in `{}` (the first --labels file)",
        report.scored_rater
    );
    let _ = writeln!(
        out,
        "- {} rows excluded as merges (2+ parents), with their labels",
        report.merges_excluded
    );
    match &report.weighted_accuracy {
        Some(w) => {
            let _ = writeln!(
                out,
                "- Stratum-weighted accuracy: **{}** (95% CI [{:.3}, {:.3}]), covering {} of the population",
                pct(Some(w.estimate)),
                w.ci_low,
                w.ci_high,
                pct(Some(w.population_covered))
            );
        }
        None => out.push_str("- Stratum-weighted accuracy: — (no scored labels)\n"),
    }
    let a = &report.abstention;
    let _ = writeln!(
        out,
        "- Abstention share: **{}** (catch_all {} + unknown {} of {})",
        pct(Some(a.share)),
        a.catch_all,
        a.unknown,
        a.population
    );
    match &report.kappa {
        Some(k) => {
            let _ = writeln!(
                out,
                "- Cohen's kappa: **{}** over {} commits (observed {:.3}, chance {:.3})",
                k.kappa
                    .map_or_else(|| "—".to_string(), |v| format!("{v:.3}")),
                k.n,
                k.observed,
                k.expected
            );
        }
        None => out.push_str("- Cohen's kappa: — (one rater)\n"),
    }
    out.push('\n');

    table(
        &mut out,
        "Precision per stratum",
        "stratum",
        &report.per_stratum,
    );
    table(
        &mut out,
        "Precision per method",
        "method",
        &report.per_method,
    );
    table(&mut out, "Precision per rule", "rule", &report.per_rule);

    out.push_str("## Coverage at precision\n\n");
    out.push_str("| min confidence | coverage | precision | n |\n|---:|---:|---:|---:|\n");
    for p in &report.coverage_curve {
        let _ = writeln!(
            out,
            "| {:.2} | {} | {} | {} |",
            p.threshold,
            pct(Some(p.coverage)),
            pct(p.precision),
            p.n
        );
    }
    out.push('\n');

    out.push_str("## Confusion matrix (rows: predicted, columns: label)\n\n");
    let labels: BTreeSet<&String> = report.confusion.values().flat_map(|m| m.keys()).collect();
    if labels.is_empty() {
        out.push_str("No labelled commits.\n");
        return out;
    }
    out.push_str("| predicted |");
    for l in &labels {
        let _ = write!(out, " {l} |");
    }
    out.push_str("\n|---|");
    out.push_str(&"---:|".repeat(labels.len()));
    out.push('\n');
    for (pred, row) in &report.confusion {
        let _ = write!(out, "| {pred} |");
        for l in &labels {
            let _ = write!(out, " {} |", row.get(*l).copied().unwrap_or(0));
        }
        out.push('\n');
    }
    out
}

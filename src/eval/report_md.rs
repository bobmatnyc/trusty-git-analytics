//! Markdown rendering of a [`ScoreReport`].

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::records::StrataSummary;
use super::score::{PrecisionRow, ScoreReport, WeightedAccuracy};
use super::score_buckets::BucketScores;

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

fn ci(row: &PrecisionRow) -> String {
    match (row.ci_low, row.ci_high) {
        (Some(lo), Some(hi)) => format!("[{lo:.3}, {hi:.3}]"),
        _ => "—".to_string(),
    }
}

fn weighted_line(out: &mut String, what: &str, w: Option<&WeightedAccuracy>) {
    match w {
        Some(w) => {
            let _ = writeln!(
                out,
                "- {what} stratum-weighted accuracy: **{}** (95% CI [{:.3}, {:.3}]), covering {} of the population",
                pct(Some(w.estimate)),
                w.ci_low,
                w.ci_high,
                pct(Some(w.population_covered))
            );
        }
        None => {
            let _ = writeln!(
                out,
                "- {what} stratum-weighted accuracy: — (no scored labels)"
            );
        }
    }
}

/// #111: primary and secondary accuracy, overall and per bucket.
fn buckets_section(out: &mut String, b: &BucketScores) {
    out.push_str("## Primary and secondary accuracy\n\n");
    let _ = writeln!(
        out,
        "Primary is the bucket of the label versus the bucket of the prediction, over \
         every scored row. Secondary is the fine category, over rows whose label is in \
         {}. A prediction with no bucket is wrong at both levels; {} scored rows had a \
         prediction with no bucket and {} a label the map does not name (left out).\n",
        if b.secondary_buckets.is_empty() {
            "no bucket with more than one category".to_string()
        } else {
            b.secondary_buckets.join(", ")
        },
        b.unmapped_predictions,
        b.unmapped_labels
    );
    out.push_str("| level | n | correct | accuracy | 95% CI (Wilson, unweighted) |\n");
    out.push_str("|---|---:|---:|---:|---|\n");
    for row in [&b.primary_counts, &b.secondary_counts] {
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            row.key,
            row.n,
            row.correct,
            pct(row.precision),
            ci(row)
        );
    }
    out.push_str(
        "\n| bucket | categories | labelled | predicted | primary correct | primary | \
                  secondary correct | secondary | secondary 95% CI (Wilson) |\n",
    );
    out.push_str("|---|---|---:|---:|---:|---:|---:|---:|---|\n");
    for (row, bucket) in b.per_bucket.iter().zip(b.map.buckets()) {
        let (sc, sp, sci) = match &row.secondary {
            Some(s) => (s.correct.to_string(), pct(s.precision), ci(s)),
            None => ("—".into(), "— (one category)".into(), "—".into()),
        };
        let _ = writeln!(
            out,
            "| {} | {} | {} | {} | {} | {} | {sc} | {sp} | {sci} |",
            row.bucket,
            bucket.categories.join(", "),
            row.labelled,
            row.predicted,
            row.primary.correct,
            pct(row.primary.precision)
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
        "Window {} → {} ({} weeks), population {}{}, seed {}, cap {}.\n",
        strata.window_start,
        strata.window_end,
        strata.weeks,
        strata.population,
        if report.window_merges_estimated > 0 {
            " (estimated, merges removed)"
        } else {
            ""
        },
        strata.seed,
        strata.cap
    );
    // #111: populations adjusted by a merge-share estimate say so.
    if report.window_merges_estimated > 0 {
        let exact = report
            .window_merges_exact
            .map_or_else(|| "— (pass --db)".to_string(), |n| n.to_string());
        let _ = writeln!(
            out,
            "Stratum populations are estimated: {} merge commits were removed by each \
             stratum's merge share in the sample. Merges in the window, exact from the \
             database: {exact}.\n",
            report.window_merges_estimated
        );
    } else if let Some(n) = report.window_merges_exact {
        let _ = writeln!(out, "Merges in the window, exact from the database: {n}.\n");
    }
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
    // #111: the two-level figures sit next to the fine one.
    let b = &report.buckets;
    weighted_line(&mut out, "Primary (bucket)", b.primary.as_ref());
    weighted_line(
        &mut out,
        "Secondary (fine within bucket)",
        b.secondary.as_ref(),
    );
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

    buckets_section(&mut out, &report.buckets);
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

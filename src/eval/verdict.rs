//! One commit's verdict as the eval harness evaluates it (#111).
//!
//! Why: `tga eval sample` and `tga eval repredict` must reach the same verdict
//! for the same commit under the same config, or a re-predicted sample would
//! measure a different classifier than a freshly drawn one.
//! What: [`resolve_verdicts`] runs the traced rule engine on each commit's
//! message and merge flag — the inputs `tga classify` gives the cascade — and
//! carries a stored manual, LLM, external-source or repo-fallback verdict
//! instead, since those tiers are never re-run offline.
//! Test: `tests/eval_harness.rs::sample_then_score_end_to_end`,
//! `tests/eval_harness.rs::repredict_keeps_rows_and_follows_the_new_rules`.

use super::population::CommitRow;
use crate::classify::{ClassificationEngine, TraceTier};

/// A commit's verdict as the harness evaluates it.
pub(crate) struct Resolved {
    pub tier: TraceTier,
    pub rule_id: String,
    pub category: String,
    pub confidence: f64,
    /// The verdict was carried from the database, not re-derived.
    pub carried: bool,
}

/// Map a stored `method` decided outside the rule engine to its trace.
fn stored_override(method: &str, traced: TraceTier) -> Option<(TraceTier, &'static str)> {
    match method {
        "manual" => Some((TraceTier::Manual, "manual_override")),
        "llm_fallback" => Some((TraceTier::Llm, "llm")),
        "repo_category_fallback" => Some((TraceTier::RepoCategory, "repo_category")),
        // The engine's own JIRA-project and issue-type tiers also store
        // `external_source`; only a verdict the engine did not reproduce came
        // from the pipeline's external resolver.
        "external_source" if !matches!(traced, TraceTier::JiraProject | TraceTier::IssueType) => {
            Some((TraceTier::ExternalSource, "external_source"))
        }
        _ => None,
    }
}

/// Resolve each commit's verdict, in input order, and count drift.
///
/// Why: see the module doc. What: classifies `(message, is_merge)` with
/// [`ClassificationEngine::classify_batch_traced`]; a commit whose stored
/// `method` names a tier outside the rule engine keeps its stored verdict.
/// The second value counts rule-engine commits whose stored category differs
/// from the re-derived one.
/// Test: see the module doc.
pub(crate) fn resolve_verdicts(
    engine: &ClassificationEngine,
    commits: &[&CommitRow],
) -> (Vec<Resolved>, u64) {
    let pairs: Vec<(&str, bool)> = commits
        .iter()
        .map(|c| (c.message.as_str(), c.is_merge))
        .collect();
    let traced = engine.classify_batch_traced(&pairs);
    let mut drifted = 0u64;
    let resolved = commits
        .iter()
        .zip(traced)
        .map(|(c, t)| {
            if let Some((cat, conf, method)) = &c.stored {
                if let Some((tier, rule)) = stored_override(method, t.trace.tier) {
                    return Resolved {
                        tier,
                        rule_id: rule.to_string(),
                        category: cat.clone(),
                        confidence: *conf,
                        carried: true,
                    };
                }
                if cat != &t.verdict.category {
                    drifted += 1;
                }
            }
            Resolved {
                tier: t.trace.tier,
                rule_id: t.trace.rule_id,
                category: t.verdict.category,
                confidence: t.verdict.confidence,
                carried: false,
            }
        })
        .collect();
    (resolved, drifted)
}

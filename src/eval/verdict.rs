//! One commit's verdict as the eval harness evaluates it (#111).
//!
//! Why: `tga eval sample` and `tga eval repredict` must reach the same verdict
//! for the same commit under the same config, or a re-predicted sample would
//! measure a different classifier than a freshly drawn one. Both aim at what
//! `tga classify` would store under that config, so they share one policy.
//! What: [`resolve_verdicts`] runs the traced rule engine on each commit's
//! message and merge flag — the inputs `tga classify` gives the cascade. A
//! stored verdict from a tier that is never re-run offline (manual, LLM,
//! external source, repo fallback) is carried only when the config's cascade
//! would still reach that tier ([`CarryPolicy`]); otherwise the re-derived
//! verdict wins and the stored one counts as superseded.
//! Test: `tests/eval_harness.rs::sample_then_score_end_to_end`,
//! `tests/eval_harness.rs::repredict_keeps_rows_and_follows_the_new_rules`,
//! `tests/eval_harness.rs::repredict_carries_a_stored_verdict_only_when_its_tier_is_reached`.

use super::population::CommitRow;
use crate::classify::{ClassificationEngine, TraceTier, TracedVerdict};
use crate::core::config::Config;

/// A commit's verdict as the harness evaluates it.
pub(crate) struct Resolved {
    pub tier: TraceTier,
    pub rule_id: String,
    pub category: String,
    pub confidence: f64,
    /// The verdict was carried from the database, not re-derived.
    pub carried: bool,
    /// A stored non-rule verdict existed but the config no longer reaches
    /// its tier, so the re-derived verdict replaced it.
    pub superseded: bool,
}

/// Which non-rule tiers the config's `tga classify` cascade would reach.
///
/// Why: #111 review — a stored LLM or external verdict must not survive a
/// config that no longer runs that tier. What: mirrors
/// `ClassificationPipeline`: the LLM tier runs when an `llm:` section exists or
/// `classification.use_llm` is set, on verdicts at or below
/// `llm_fallback_threshold`; external sources run when any is configured and
/// `no_external` is off. The stored verdict does not say which source produced
/// it, so any configured source keeps it.
/// Test: `tests/eval_harness.rs::repredict_carries_a_stored_verdict_only_when_its_tier_is_reached`.
pub(crate) struct CarryPolicy {
    use_llm: bool,
    llm_threshold: f64,
    external: bool,
}

impl CarryPolicy {
    /// Read the policy from `config`.
    pub(crate) fn from_config(config: &Config) -> Self {
        let c = config.classification.as_ref();
        Self {
            use_llm: config.llm.is_some() || c.is_some_and(|c| c.use_llm),
            llm_threshold: c.map_or(0.65, |c| c.llm_fallback_threshold),
            external: c.is_some_and(|c| !c.no_external && !c.sources.is_empty()),
        }
    }

    /// Whether the cascade reaches `stored` given the re-derived verdict `t`.
    fn reaches(&self, stored: TraceTier, t: &TracedVerdict) -> bool {
        match stored {
            TraceTier::Manual => true,
            TraceTier::Llm => self.use_llm && t.verdict.confidence <= self.llm_threshold,
            TraceTier::RepoCategory => t.trace.tier == TraceTier::Unclassified,
            TraceTier::ExternalSource => self.external,
            _ => false,
        }
    }
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
/// `method` names a tier outside the rule engine keeps its stored verdict when
/// `policy` says the cascade reaches that tier. The second value counts
/// rule-engine commits whose stored category differs from the re-derived one.
/// Test: see the module doc.
pub(crate) fn resolve_verdicts(
    engine: &ClassificationEngine,
    policy: &CarryPolicy,
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
            let mut superseded = false;
            if let Some((cat, conf, method)) = &c.stored {
                match stored_override(method, t.trace.tier) {
                    // #111: carry only a tier the config's cascade reaches.
                    Some((tier, rule)) if policy.reaches(tier, &t) => {
                        return Resolved {
                            tier,
                            rule_id: rule.to_string(),
                            category: cat.clone(),
                            confidence: *conf,
                            carried: true,
                            superseded: false,
                        };
                    }
                    Some(_) => superseded = true,
                    None if cat != &t.verdict.category => drifted += 1,
                    None => {}
                }
            }
            Resolved {
                tier: t.trace.tier,
                rule_id: t.trace.rule_id,
                category: t.verdict.category,
                confidence: t.verdict.confidence,
                carried: false,
                superseded,
            }
        })
        .collect();
    (resolved, drifted)
}

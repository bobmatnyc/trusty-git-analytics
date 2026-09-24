//! Rule tracing: name the rule behind each classification verdict (#111).
//!
//! Why: the classifications table stores a verdict's category, confidence and
//! method, but not which rule fired, so per-rule precision cannot be measured.
//! The eval harness (`tga eval`) needs that name. A downstream consumer reads
//! `tga.db`, so the name must never reach it.
//! What: [`TracedVerdict`] pairs a [`ClassificationResult`] with a
//! [`RuleTrace`]. It is produced by
//! [`ClassificationEngine::classify_sync_traced`](crate::classify::ClassificationEngine::classify_sync_traced)
//! and is never persisted — the untraced cascade that `tga classify` runs is a
//! projection of the traced one, so both return the same verdict.
//! Test: `classify::trace::tests`, and `tests/classify_byte_identical.rs` for
//! the unchanged database rows.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::classify::rules::Rule;
use crate::classify::tiers::ClassificationResult;

/// Rule id of the built-in lowest-priority catch-all regex rule.
pub const CATCH_ALL_RULE_ID: &str = "catch-all";

/// Source label for rules compiled into the binary.
pub const BUILTIN_SOURCE: &str = "builtin";

/// Source label for rules whose origin the engine was not told.
pub const UNKNOWN_SOURCE: &str = "ruleset";

/// The cascade tier that produced a verdict.
///
/// Finer than [`crate::core::models::ClassificationMethod`]: the built-in
/// catch-all is a regex rule in the database (`regex_rule`, confidence 0.3)
/// but a tier of its own here, and the issue-type and JIRA project tiers both
/// store `external_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TraceTier {
    /// Tier 0: a manual override row.
    Manual,
    /// Tier 1: an exact-keyword rule.
    Exact,
    /// Tier 1.5: a PM issue-type mapping.
    IssueType,
    /// Tier 1.6: a JIRA project-key mapping.
    JiraProject,
    /// Tier 2: a regex rule other than the catch-all.
    Regex,
    /// Tier 2: the lowest-priority catch-all regex rule.
    CatchAll,
    /// Tier 2.5: the weighted-sum signal model.
    WeightedSum,
    /// Tier 3.5: a fuzzy heuristic.
    Fuzzy,
    /// Pipeline Tier 0.5: an external ticket source (JIRA / GitHub Issues).
    ExternalSource,
    /// Pipeline Tier 4: the LLM fallback.
    Llm,
    /// Tier 5: the `repo_categories` fallback.
    RepoCategory,
    /// No tier matched.
    Unclassified,
}

impl TraceTier {
    /// Stable snake_case label, identical to the serde form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Exact => "exact",
            Self::IssueType => "issue_type",
            Self::JiraProject => "jira_project",
            Self::Regex => "regex",
            Self::CatchAll => "catch_all",
            Self::WeightedSum => "weighted_sum",
            Self::Fuzzy => "fuzzy",
            Self::ExternalSource => "external_source",
            Self::Llm => "llm",
            Self::RepoCategory => "repo_category",
            Self::Unclassified => "unclassified",
        }
    }
}

/// Names the rule behind one verdict.
///
/// `rule_id` is stable across runs of the same configuration:
/// - exact / regex rules: `<source>#<rule id>`, where `<source>` is
///   [`BUILTIN_SOURCE`] or the rules file path as configured;
/// - the built-in catch-all: `catch_all`;
/// - weighted sum: `weighted_sum:<category>/<dominant signal>`;
/// - fuzzy: `fuzzy:<heuristic>`;
/// - issue type / JIRA project: `issue_type:<type>` / `jira_project:<KEY>`;
/// - manual override: `manual_override`; no match: `unclassified`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleTrace {
    /// Tier that produced the verdict.
    pub tier: TraceTier,
    /// Stable identifier of the rule within that tier.
    pub rule_id: String,
}

impl RuleTrace {
    /// Trace for a tier whose rule id is a fixed label.
    pub fn new(tier: TraceTier, rule_id: impl Into<String>) -> Self {
        Self {
            tier,
            rule_id: rule_id.into(),
        }
    }

    /// Trace for an exact or regex rule, resolving its source file.
    ///
    /// Why: a rule id alone is ambiguous once several rules files are
    /// layered; the source makes it stable and attributable.
    /// What: the built-in catch-all becomes tier [`TraceTier::CatchAll`] with
    /// id `catch_all`; a rules file that redefines `catch-all` keeps its
    /// source-qualified id but still reports the catch-all tier.
    /// Test: `tests::rule_trace_qualifies_by_source`,
    /// `tests::rule_trace_names_the_builtin_catch_all`.
    pub fn for_rule(tier: TraceTier, rule: &Rule, sources: &RuleSources) -> Self {
        let source = sources.source_of(&rule.id);
        if rule.id == CATCH_ALL_RULE_ID && tier == TraceTier::Regex {
            let rule_id = if source == BUILTIN_SOURCE {
                "catch_all".to_string()
            } else {
                format!("{source}#{}", rule.id)
            };
            return Self::new(TraceTier::CatchAll, rule_id);
        }
        Self::new(tier, format!("{source}#{}", rule.id))
    }
}

/// A verdict plus the rule that produced it. Held in memory only.
#[derive(Debug, Clone, PartialEq)]
pub struct TracedVerdict {
    /// The verdict, exactly as the untraced cascade returns it.
    pub verdict: ClassificationResult,
    /// The rule that produced it.
    pub trace: RuleTrace,
}

impl TracedVerdict {
    /// The verdict returned when no tier matches.
    pub fn unclassified() -> Self {
        Self {
            verdict: ClassificationResult::unclassified(),
            trace: RuleTrace::new(TraceTier::Unclassified, "unclassified"),
        }
    }
}

/// Where each rule in an engine's rule set came from.
///
/// Why: `RuleSet::merge` drops provenance, and [`Rule`] has no source field
/// (adding one would break every struct literal of this public type).
/// What: maps rule id to the rules file that last defined it; ids not in the
/// map report the fallback label — [`BUILTIN_SOURCE`] for sets the pipeline
/// assembled, [`UNKNOWN_SOURCE`] when the engine was built without sources.
/// Test: `tests::rule_trace_qualifies_by_source`.
#[derive(Debug, Clone)]
pub struct RuleSources {
    by_id: HashMap<String, String>,
    fallback: &'static str,
}

impl Default for RuleSources {
    fn default() -> Self {
        Self {
            by_id: HashMap::new(),
            fallback: UNKNOWN_SOURCE,
        }
    }
}

impl RuleSources {
    /// Sources for a rule set whose unrecorded ids are the built-ins.
    pub fn builtin() -> Self {
        Self {
            by_id: HashMap::new(),
            fallback: BUILTIN_SOURCE,
        }
    }

    /// Record that every rule in `rules` was (re)defined by `path`.
    ///
    /// Later calls win, matching `RuleSet::merge`.
    pub fn record_file(&mut self, path: &Path, rules: &[Rule]) {
        let label = path.display().to_string();
        for rule in rules {
            self.by_id.insert(rule.id.clone(), label.clone());
        }
    }

    /// The source label for `rule_id`.
    pub fn source_of(&self, rule_id: &str) -> &str {
        self.by_id
            .get(rule_id)
            .map(String::as_str)
            .unwrap_or(self.fallback)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str) -> Rule {
        Rule {
            id: id.to_string(),
            category: "x".to_string(),
            subcategory: None,
            keywords: vec![],
            patterns: vec![],
            priority: 1,
            confidence: 0.3,
        }
    }

    /// Why: layered rules files must be attributable; a builtin and a file
    /// rule with different ids must name different sources.
    /// What: records one file rule, then traces it and a builtin id.
    /// Test: this function.
    #[test]
    fn rule_trace_qualifies_by_source() {
        let mut sources = RuleSources::builtin();
        sources.record_file(Path::new("team/rules.yaml"), &[rule("deploy")]);
        let file = RuleTrace::for_rule(TraceTier::Exact, &rule("deploy"), &sources);
        assert_eq!(file.rule_id, "team/rules.yaml#deploy");
        assert_eq!(file.tier, TraceTier::Exact);
        let builtin = RuleTrace::for_rule(TraceTier::Regex, &rule("cc-fix"), &sources);
        assert_eq!(builtin.rule_id, "builtin#cc-fix");
        let unknown = RuleTrace::for_rule(TraceTier::Regex, &rule("r"), &RuleSources::default());
        assert_eq!(unknown.rule_id, "ruleset#r");
    }

    /// Why: the catch-all is its own eval stratum even though the database
    /// stores it as `regex_rule`.
    /// What: the built-in catch-all traces as `catch_all`; a file override of
    /// the same id keeps the tier but names its file.
    /// Test: this function.
    #[test]
    fn rule_trace_names_the_builtin_catch_all() {
        let builtin = RuleTrace::for_rule(
            TraceTier::Regex,
            &rule(CATCH_ALL_RULE_ID),
            &RuleSources::builtin(),
        );
        assert_eq!(builtin, RuleTrace::new(TraceTier::CatchAll, "catch_all"));
        let mut sources = RuleSources::builtin();
        sources.record_file(Path::new("r.yaml"), &[rule(CATCH_ALL_RULE_ID)]);
        let custom = RuleTrace::for_rule(TraceTier::Regex, &rule(CATCH_ALL_RULE_ID), &sources);
        assert_eq!(custom.tier, TraceTier::CatchAll);
        assert_eq!(custom.rule_id, "r.yaml#catch-all");
    }
}

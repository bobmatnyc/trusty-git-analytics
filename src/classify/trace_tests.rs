//! Rule tracing through the engine cascade (#111).

use std::collections::HashMap;
use std::path::Path;

use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::rules::{default_rules, Rule, RuleSet};
use crate::classify::tiers::weighted_sum::WeightedSumConfig;
use crate::classify::trace::{RuleSources, TraceTier};

fn deploy_rule() -> Rule {
    Rule {
        id: "deploy".to_string(),
        category: "deployment".to_string(),
        subcategory: None,
        keywords: vec!["deploy:".to_string()],
        patterns: vec![],
        priority: 110,
        confidence: 0.9,
    }
}

/// An engine over one custom rule with the given `extend_defaults` and
/// weighted-sum toggle, so a test can reach the weighted-sum and fuzzy tiers
/// the built-in catch-all otherwise pre-empts.
fn custom_engine(extend_defaults: bool, weighted_sum: bool) -> ClassificationEngine {
    let ruleset = RuleSet {
        version: None,
        extend_defaults,
        rules: vec![deploy_rule()],
    };
    let cfg = ClassificationEngineConfig {
        weighted_sum: WeightedSumConfig {
            enabled: weighted_sum,
            ..Default::default()
        },
        ..ClassificationEngineConfig::default()
    };
    ClassificationEngine::new(ruleset, cfg).expect("engine")
}

fn jira_engine() -> ClassificationEngine {
    let mut mappings = HashMap::new();
    mappings.insert("TQL".to_string(), "bugfix".to_string());
    ClassificationEngine::with_taxonomy_and_mappings(
        default_rules(),
        ClassificationEngineConfig::default(),
        Vec::new(),
        mappings,
        None,
    )
    .expect("engine")
}

/// Why (#111): `tga classify` must write the verdict it wrote before rule
/// tracing, so the traced cascade may only add a trace, never change a verdict.
/// What: across four engines that together reach every synchronous tier
/// (exact, JIRA project, regex, catch-all, weighted sum, fuzzy, none), the
/// traced verdict equals the untraced one for every message.
/// Test: this function.
#[test]
fn traced_cascade_matches_untraced_verdicts() {
    let engines = [
        ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())
            .expect("engine"),
        jira_engine(),
        custom_engine(false, true),
        custom_engine(true, false),
    ];
    let messages: [(&str, bool); 10] = [
        ("fix: handle null user", false),
        ("deploy: prod", false),
        ("TQL-9 login fails", false),
        ("update the readme wording for install steps", false),
        ("some unstructured prose about nothing in particular", false),
        ("Merge branch 'main'", true),
        ("Revert \"add cache\"", false),
        ("PROJ-12 adjust thing", false),
        ("tidy", false),
        ("", false),
    ];
    for engine in &engines {
        let untraced = engine.classify_batch(&messages);
        let traced = engine.classify_batch_traced(&messages);
        for ((u, t), (msg, _)) in untraced.iter().zip(&traced).zip(&messages) {
            assert_eq!(u, &t.verdict, "verdict drifted for {msg:?}");
        }
    }
}

/// Why (#111): per-rule precision needs a stable id per tier, qualified by
/// the rules file for exact and regex rules.
/// What: asserts the trace tier and rule id for one message per tier.
/// Test: this function.
#[test]
fn traced_cascade_names_rule_sources() {
    let trace = |engine: &ClassificationEngine, msg: &str, merge: bool| {
        engine
            .classify_sync_traced(msg, merge, None, None, None)
            .map(|t| (t.trace.tier, t.trace.rule_id))
    };

    let builtin = jira_engine().with_rule_sources(RuleSources::builtin());
    let (tier, id) = trace(&builtin, "fix: handle null user", false).expect("exact");
    assert_eq!(tier, TraceTier::Exact);
    assert!(id.starts_with("builtin#"), "{id}");
    assert_eq!(
        trace(&builtin, "TQL-9 login fails", false),
        Some((TraceTier::JiraProject, "jira_project:TQL".to_string()))
    );
    assert_eq!(
        trace(&builtin, "zqx vbn wrt plk", false),
        Some((TraceTier::CatchAll, "catch_all".to_string()))
    );

    let mut sources = RuleSources::builtin();
    sources.record_file(Path::new("team-rules.yaml"), &[deploy_rule()]);
    let custom = custom_engine(false, true).with_rule_sources(sources);
    assert_eq!(
        trace(&custom, "deploy: prod", false),
        Some((TraceTier::Exact, "team-rules.yaml#deploy".to_string()))
    );
    let (tier, id) = trace(&custom, "fix null pointer regression hotfix", false).expect("weighted");
    assert_eq!(tier, TraceTier::WeightedSum);
    assert_eq!(id, "weighted_sum:bugfix/keyword");

    let fuzzy = custom_engine(true, false);
    assert_eq!(
        trace(&fuzzy, "Merge branch 'main'", true),
        Some((TraceTier::Fuzzy, "fuzzy:merge".to_string()))
    );
    assert_eq!(
        fuzzy.classify_batch_traced(&[("", false)])[0].trace.tier,
        TraceTier::Unclassified
    );
}

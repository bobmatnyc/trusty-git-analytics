//! #131: `extend_defaults: false` turns the weighted-sum tier off unless
//! `weighted_sum.enabled: true` opts it back in. Configs are parsed from YAML
//! exactly as `tga` reads them; no network.

use std::io::Write;

use super::*;
use crate::core::config::{ClassificationConfig, Config};
use crate::core::models::ClassificationMethod;

/// The weighted-sum tier answers `feature` for this message.
const FEATURE: &str = "implement new dashboard feature";

/// A custom-only rules file whose declared `Feature` category holds the
/// weighted-sum tier's `feature` vote, so the #165 in-set filter keeps it.
/// Only the #131 gate can stop the tier here.
const CUSTOM_ONLY: &str = "extend_defaults: false
rules:
  - id: never
    category: defect
    keywords: [\"zzz-never-matches-zzz\"]
categories:
  - name: Feature
";

/// A rules file layered over the built-ins, with one rule that never matches.
const LAYERED: &str = "extend_defaults: true
rules:
  - id: never
    category: defect
    keywords: [\"zzz-never-matches-zzz\"]
";

/// A fixed commit set spanning the built-in tiers and the weighted-sum tier.
const MESSAGES: &[(&str, bool)] = &[
    ("implement new dashboard feature", false),
    ("chore(deps): bump openssl for CVE-2026-1234", false),
    ("fix null pointer regression hotfix", false),
    ("docs: update README", false),
    ("refactor: extract helper", false),
    ("Merge branch 'main' into dev", true),
    ("PROJ-123 tweak", false),
    ("zqx vbn wrt plk", false),
    ("", false),
];

fn rules_file(yaml: &str) -> tempfile::NamedTempFile {
    let mut f = tempfile::Builder::new()
        .suffix(".yaml")
        .tempfile()
        .expect("tempfile");
    f.write_all(yaml.as_bytes()).expect("write rules");
    f
}

/// The engine `tga classify` and `tga rules test` build from a
/// `classification:` section given as YAML.
fn engine(classification_yaml: &str) -> ClassificationEngine {
    let classification: ClassificationConfig =
        serde_yaml::from_str(classification_yaml).expect("classification yaml");
    ClassificationPipeline::new(Config {
        classification: Some(classification),
        ..Config::default()
    })
    .build_rule_engine()
    .expect("rule engine")
}

fn with_rules(rules: &tempfile::NamedTempFile, extra: &str) -> ClassificationEngine {
    engine(&format!(
        "rules_files: [\"{}\"]\n{extra}",
        rules.path().display()
    ))
}

/// One line per message: `category|method|confidence|top_level|ticket`.
fn dump(engine: &ClassificationEngine) -> String {
    MESSAGES
        .iter()
        .map(
            |(msg, is_merge)| match engine.classify_sync(msg, *is_merge) {
                Some(v) => format!(
                    "{}|{}|{:.4}|{:?}|{:?}\n",
                    v.category,
                    v.method.as_str(),
                    v.confidence,
                    v.top_level,
                    v.ticket_id
                ),
                None => "none\n".to_string(),
            },
        )
        .collect()
}

/// Why (#131, owner decision 2026-09-24): `extend_defaults: false` means the
/// rules define all classification, but the weighted-sum tier still voted.
/// #165 dropped votes outside the set; a vote inside it still fired.
/// What: with the key unset, the custom-only engine gives no verdict for a
/// message the tier answers `feature`, a category the rules declare.
/// Test: this test.
#[test]
fn extend_defaults_false_turns_the_weighted_sum_tier_off() {
    let rules = rules_file(CUSTOM_ONLY);
    let engine = with_rules(&rules, "");
    assert_eq!(engine.classify_sync(FEATURE, false), None);
}

/// Why (#131): an explicit `weighted_sum.enabled: true` still opts in, and
/// `enabled: false` stays off.
/// What: the same custom-only rules; opted in, the tier answers `Feature`
/// in the rules' spelling (#165); opted out, nothing answers.
/// Test: this test.
#[test]
fn explicit_enabled_true_opts_a_custom_only_ruleset_back_in() {
    let rules = rules_file(CUSTOM_ONLY);
    let on = with_rules(&rules, "weighted_sum:\n  enabled: true\n");
    let v = on
        .classify_sync(FEATURE, false)
        .expect("weighted-sum verdict");
    assert_eq!(v.method, ClassificationMethod::WeightedSum);
    assert_eq!(v.category, "Feature");

    let off = with_rules(&rules, "weighted_sum:\n  enabled: false\n");
    assert_eq!(off.classify_sync(FEATURE, false), None);
}

/// Built-in rules only (no rules file): verdicts captured on origin/main
/// 03475f3, before #131.
const BUILTIN_GOLDEN: &str = r#"monitoring|exact_rule|0.8000|Some(PlatformWork)|None
security|exact_rule|0.9000|Some(Bugfix)|Some("CVE-2026")
bugfix|exact_rule|0.9500|Some(Bugfix)|None
documentation|exact_rule|0.9000|Some(Content)|None
refactor|exact_rule|0.9000|Some(Maintenance)|None
merge|exact_rule|0.9500|Some(Maintenance)|None
feature|regex_rule|0.7000|Some(Feature)|Some("PROJ-123")
maintenance|regex_rule|0.3000|Some(Maintenance)|None
none
"#;
/// [`LAYERED`] (`extend_defaults: true`): verdicts captured on origin/main
/// 03475f3, before #131. The built-in rules answer every message first.
const LAYERED_GOLDEN: &str = BUILTIN_GOLDEN;
/// An `extend_defaults: true` ruleset without the built-in catch-all, so the
/// weighted-sum tier answers: verdicts captured on origin/main 03475f3.
const DIRECT_GOLDEN: &str = r#"feature|weighted_sum|0.7500|Some(Feature)|None
chore|weighted_sum|0.6000|Some(Maintenance)|Some("CVE-2026")
bugfix|weighted_sum|0.7500|Some(Bugfix)|None
docs|weighted_sum|0.6000|Some(Content)|None
none
merge|weighted_sum|0.6500|Some(Maintenance)|None
feature|fuzzy_match|0.6000|Some(Feature)|Some("PROJ-123")
none
none
"#;

/// An engine over `extend_defaults: true` and one never-matching rule,
/// built directly so no built-in rule pre-empts the weighted-sum tier.
fn direct_engine() -> ClassificationEngine {
    let ruleset = RuleSet {
        extend_defaults: true,
        rules: vec![Rule {
            id: "never".to_string(),
            category: "defect".to_string(),
            subcategory: None,
            keywords: vec!["zzz-never-matches-zzz".to_string()],
            patterns: vec![],
            priority: 110,
            confidence: 0.9,
        }],
        ..RuleSet::default()
    };
    ClassificationEngine::new(ruleset, ClassificationEngineConfig::default()).expect("engine")
}

/// Why (#131): only `extend_defaults: false` changes. With the built-in
/// rules, or `extend_defaults: true`, with `weighted_sum.enabled` unset or
/// `true`, every verdict must match origin/main byte for byte.
/// What: dumps a fixed commit set through each engine and compares it with
/// the dump captured on origin/main 03475f3.
/// Test: this test; `tests/classify_byte_identical.rs` pins the full
/// 3,970-commit corpus the same way.
#[test]
fn default_and_extend_defaults_true_verdicts_are_unchanged() {
    let layered = rules_file(LAYERED);
    let cases = [
        ("builtin, key unset", engine(""), BUILTIN_GOLDEN),
        (
            "builtin, enabled: true",
            engine("weighted_sum:\n  enabled: true\n"),
            BUILTIN_GOLDEN,
        ),
        (
            "layered, key unset",
            with_rules(&layered, ""),
            LAYERED_GOLDEN,
        ),
        (
            "layered, enabled: true",
            with_rules(&layered, "weighted_sum:\n  enabled: true\n"),
            LAYERED_GOLDEN,
        ),
        (
            "direct, extend_defaults: true",
            direct_engine(),
            DIRECT_GOLDEN,
        ),
    ];
    let diffs: Vec<String> = cases
        .into_iter()
        .filter_map(|(name, engine, golden)| {
            let got = dump(&engine);
            (got != golden).then(|| format!("{name}:\n{got:?}"))
        })
        .collect();
    assert!(diffs.is_empty(), "{}", diffs.join("\n"));
}

/// Why (#131): an absent `enabled` must stay distinguishable from an
/// explicit one, and a YAML `null` must not read as absent.
/// What: absent is `None` and follows `extend_defaults`; `true` and `false`
/// win either way; `null` is an error.
/// Test: this test.
#[test]
fn weighted_sum_enabled_is_tri_state() {
    use crate::classify::tiers::weighted_sum::WeightedSumConfig;
    let parse = |yaml: &str| serde_yaml::from_str::<WeightedSumConfig>(yaml);

    let unset = parse("min_confidence: 0.55").expect("unset");
    assert_eq!(unset.enabled, None);
    assert!(unset.is_enabled(true));
    assert!(!unset.is_enabled(false));

    let on = parse("enabled: true").expect("on");
    assert!(on.is_enabled(true) && on.is_enabled(false));
    let off = parse("enabled: false").expect("off");
    assert!(!off.is_enabled(true) && !off.is_enabled(false));

    assert!(parse("enabled: null").is_err());
}

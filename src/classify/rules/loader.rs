//! Rule file loader and built-in default ruleset.

use std::path::Path;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::types::{Rule, RuleSet};

/// Load a [`RuleSet`] from a YAML or JSON file.
///
/// Format is detected by extension (`.json` → JSON, anything else → YAML).
///
/// # Errors
///
/// - [`ClassifyError::Io`] if the file cannot be read.
/// - [`ClassifyError::Yaml`] / [`ClassifyError::Json`] on parse failure.
pub fn load_rules(path: &Path) -> Result<RuleSet> {
    let text = std::fs::read_to_string(path)?;
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);

    let set: RuleSet = if is_json {
        serde_json::from_str(&text)?
    } else {
        serde_yaml::from_str(&text)?
    };

    if set.rules.is_empty() {
        return Err(ClassifyError::RuleLoad(format!(
            "rule file {} contained no rules",
            path.display()
        )));
    }
    Ok(set)
}

/// Return the built-in default ruleset.
///
/// Covers conventional commit prefixes (`feat:`, `fix:`, `chore:`, `docs:`,
/// `refactor:`, `test:`, `ci:`, `perf:`, `style:`, `build:`, `revert:`),
/// JIRA-style ticket patterns (e.g. `PROJ-123`), and common keywords such
/// as `hotfix`, `bug`, `breaking change`, `merge`, and `revert`.
pub fn default_rules() -> RuleSet {
    let rules = vec![
        // -- Conventional commits (high priority, regex-anchored) --
        Rule {
            id: "cc-feat".into(),
            category: "feature".into(),
            subcategory: None,
            keywords: vec!["feat:".into(), "feature:".into()],
            patterns: vec![r"(?i)^\s*feat(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-fix".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["fix:".into(), "bugfix:".into(), "hotfix".into()],
            patterns: vec![r"(?i)^\s*fix(\([^)]*\))?!?:".into()],
            priority: 100,
            confidence: 0.95,
        },
        Rule {
            id: "cc-chore".into(),
            category: "chore".into(),
            subcategory: None,
            keywords: vec!["chore:".into()],
            patterns: vec![r"(?i)^\s*chore(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-docs".into(),
            category: "documentation".into(),
            subcategory: None,
            keywords: vec!["docs:".into(), "doc:".into()],
            patterns: vec![r"(?i)^\s*docs?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-refactor".into(),
            category: "refactor".into(),
            subcategory: None,
            keywords: vec!["refactor:".into()],
            patterns: vec![r"(?i)^\s*refactor(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-test".into(),
            category: "test".into(),
            subcategory: None,
            keywords: vec!["test:".into(), "tests:".into()],
            patterns: vec![r"(?i)^\s*tests?(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-ci".into(),
            category: "ci".into(),
            subcategory: None,
            keywords: vec!["ci:".into()],
            patterns: vec![r"(?i)^\s*ci(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-perf".into(),
            category: "performance".into(),
            subcategory: None,
            keywords: vec!["perf:".into()],
            patterns: vec![r"(?i)^\s*perf(\([^)]*\))?!?:".into()],
            priority: 90,
            confidence: 0.9,
        },
        Rule {
            id: "cc-style".into(),
            category: "style".into(),
            subcategory: None,
            keywords: vec!["style:".into()],
            patterns: vec![r"(?i)^\s*style(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-build".into(),
            category: "build".into(),
            subcategory: None,
            keywords: vec!["build:".into()],
            patterns: vec![r"(?i)^\s*build(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.85,
        },
        Rule {
            id: "cc-revert".into(),
            category: "revert".into(),
            subcategory: None,
            keywords: vec!["revert:".into()],
            patterns: vec![r"(?i)^\s*revert(\([^)]*\))?!?:".into()],
            priority: 80,
            confidence: 0.9,
        },
        // -- Breaking change marker --
        Rule {
            id: "breaking-change".into(),
            category: "breaking".into(),
            subcategory: Some("api".into()),
            keywords: vec!["breaking change".into(), "breaking-change".into()],
            patterns: vec![r"(?i)breaking[\s-]change".into()],
            priority: 110,
            confidence: 0.9,
        },
        // -- JIRA-style ticket --
        Rule {
            id: "jira-ticket".into(),
            category: "feature".into(),
            subcategory: Some("ticketed".into()),
            keywords: vec![],
            patterns: vec![r"\b[A-Z][A-Z0-9]+-\d+\b".into()],
            priority: 50,
            confidence: 0.75,
        },
        // -- Lower-priority keyword fallbacks --
        Rule {
            id: "kw-bug".into(),
            category: "bugfix".into(),
            subcategory: None,
            keywords: vec!["bug".into(), "defect".into()],
            patterns: vec![],
            priority: 40,
            confidence: 0.7,
        },
        Rule {
            id: "kw-security".into(),
            category: "bugfix".into(),
            subcategory: Some("security".into()),
            keywords: vec!["security".into(), "cve-".into(), "vulnerability".into()],
            patterns: vec![],
            priority: 60,
            confidence: 0.85,
        },
    ];

    RuleSet {
        version: Some("1.0".into()),
        extend_defaults: true,
        rules,
    }
}

//! Rule and rule-set data structures.

use serde::{Deserialize, Serialize};

/// A single classification rule.
///
/// A rule matches when **any** of its `keywords` is present in the commit
/// message (Tier 1) or **any** of its `patterns` matches (Tier 2). The
/// resulting verdict carries the rule's `category`, `subcategory`, and
/// `confidence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Unique rule identifier (used in logs and overrides).
    pub id: String,

    /// Classification category (e.g. `"feature"`, `"bugfix"`, `"chore"`).
    pub category: String,

    /// Optional subcategory (e.g. `"security"`, `"performance"`).
    #[serde(default)]
    pub subcategory: Option<String>,

    /// Exact keywords to match against the commit message
    /// (case-insensitive, substring match).
    #[serde(default)]
    pub keywords: Vec<String>,

    /// Regex patterns to match against the commit message.
    #[serde(default)]
    pub patterns: Vec<String>,

    /// Priority (higher = checked first). Defaults to 0.
    #[serde(default)]
    pub priority: i32,

    /// Confidence score assigned when this rule matches (0.0–1.0).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
}

fn default_confidence() -> f64 {
    0.85
}

/// A collection of rules loaded from a file or built into the binary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleSet {
    /// Optional schema version for forward compatibility.
    #[serde(default)]
    pub version: Option<String>,

    /// When `true` (default), custom rules are merged on top of the built-in
    /// default ruleset. Default rules fire first (lower priority numbers win
    /// only if both match the same message and default has higher priority).
    /// Custom rules that share an `id` with a default rule **override** that rule.
    /// Set to `false` to use only the rules in this file.
    #[serde(default = "default_true")]
    pub extend_defaults: bool,

    /// All rules in this set. Order is not significant; see [`Rule::priority`].
    pub rules: Vec<Rule>,
}

fn default_true() -> bool {
    true
}

impl RuleSet {
    /// Return rules sorted by descending priority. Stable sort preserves
    /// declaration order for rules with equal priority.
    pub fn by_priority(&self) -> Vec<&Rule> {
        let mut refs: Vec<&Rule> = self.rules.iter().collect();
        refs.sort_by(|a, b| b.priority.cmp(&a.priority));
        refs
    }
}

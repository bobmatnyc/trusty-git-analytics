//! Tier 1: exact keyword matching via Aho-Corasick.
//!
//! All keywords from all rules are compiled into a single case-insensitive
//! Aho-Corasick automaton. On match, the pattern id is mapped back to the
//! originating rule. If multiple rules match, the one with the highest
//! [`crate::classify::rules::Rule::priority`] wins.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, MatchKind};

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::rules::Rule;

/// Tier-1 exact matcher.
pub struct ExactMatcher {
    /// The compiled automaton. `None` if there were no keywords across all rules.
    automaton: Option<AhoCorasick>,
    /// For each pattern id, the index of its rule in `rules`.
    pattern_rule_idx: Vec<usize>,
    /// Owned copy of the input rules.
    rules: Vec<Rule>,
}

impl ExactMatcher {
    /// Build a new matcher from the given rules.
    ///
    /// # Errors
    ///
    /// Returns [`ClassifyError::RuleLoad`] if the automaton fails to build.
    pub fn new(rules: &[Rule]) -> Result<Self> {
        let mut patterns: Vec<String> = Vec::new();
        let mut pattern_rule_idx: Vec<usize> = Vec::new();

        for (idx, rule) in rules.iter().enumerate() {
            for kw in &rule.keywords {
                if kw.is_empty() {
                    continue;
                }
                patterns.push(kw.clone());
                pattern_rule_idx.push(idx);
            }
        }

        let automaton = if patterns.is_empty() {
            None
        } else {
            let ac = AhoCorasickBuilder::new()
                .ascii_case_insensitive(true)
                .match_kind(MatchKind::LeftmostLongest)
                .build(&patterns)
                .map_err(|e| ClassifyError::RuleLoad(format!("aho-corasick build: {e}")))?;
            Some(ac)
        };

        Ok(Self {
            automaton,
            pattern_rule_idx,
            rules: rules.to_vec(),
        })
    }

    /// Classify `message` using exact keyword matching.
    ///
    /// Returns the highest-priority matching rule, or `None` if no keyword matches.
    pub fn classify(&self, message: &str) -> Option<&Rule> {
        let ac = self.automaton.as_ref()?;
        let mut best: Option<&Rule> = None;
        for m in ac.find_iter(message) {
            let rule_idx = self.pattern_rule_idx[m.pattern().as_usize()];
            let rule = &self.rules[rule_idx];
            best = match best {
                Some(prev) if prev.priority >= rule.priority => Some(prev),
                _ => Some(rule),
            };
        }
        best
    }
}

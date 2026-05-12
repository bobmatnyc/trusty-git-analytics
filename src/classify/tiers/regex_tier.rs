//! Tier 2: regex pattern matching.
//!
//! All `patterns` across all rules are compiled at construction time. On
//! classification, patterns are tested in descending priority order; the
//! first hit wins.

use regex::Regex;

use crate::classify::errors::Result;
use crate::classify::rules::Rule;

/// Tier-2 regex matcher.
pub struct RegexMatcher {
    /// Compiled `(regex, rule)` pairs, sorted by descending rule priority.
    compiled: Vec<(Regex, Rule)>,
}

impl RegexMatcher {
    /// Pre-compile all `patterns` from `rules`.
    ///
    /// Rules with no patterns are silently skipped.
    ///
    /// # Errors
    ///
    /// Returns [`crate::classify::errors::ClassifyError::Regex`] if any pattern fails
    /// to compile.
    pub fn new(rules: &[Rule]) -> Result<Self> {
        let mut compiled: Vec<(Regex, Rule)> = Vec::new();
        for rule in rules {
            for pat in &rule.patterns {
                let re = Regex::new(pat)?;
                compiled.push((re, rule.clone()));
            }
        }
        compiled.sort_by_key(|c| std::cmp::Reverse(c.1.priority));
        Ok(Self { compiled })
    }

    /// Return the first rule whose pattern matches `message`, in
    /// descending priority order.
    pub fn classify(&self, message: &str) -> Option<&Rule> {
        for (re, rule) in &self.compiled {
            if re.is_match(message) {
                return Some(rule);
            }
        }
        None
    }

    /// Find the first ticket-like identifier (`PROJ-123`) in `message`.
    ///
    /// Used by the engine to attach a `ticket_id` to verdicts that didn't
    /// match the dedicated JIRA rule directly.
    pub fn extract_ticket_id(message: &str) -> Option<String> {
        // Compiled lazily on each call; the regex crate caches internally
        // for repeated identical patterns when wrapped in `lazy_static`,
        // but a single compile per commit is cheap relative to LLM/IO.
        let re = Regex::new(r"\b[A-Z][A-Z0-9]+-\d+\b").ok()?;
        re.find(message).map(|m| m.as_str().to_string())
    }
}

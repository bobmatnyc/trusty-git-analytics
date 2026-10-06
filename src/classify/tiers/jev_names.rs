//! The name set and name matcher behind the Jev pseudonymizer (#111).
//!
//! Why: people, repositories and operator terms are matched by a list, not a
//! shape; the list grows during a run (authors, trailer names) and its
//! matching rules (name parts, stop-listed given names, apostrophes) are
//! their own concern. Split out of `jev_obfuscate.rs` for the size cap.
//! What: [`NameSet`] collects the names and the stop-listed given names
//! that match only in Titlecase (`Frank`);
//! [`super::jev_matcher::NameMatcher`] compiles them.
//! Test: `classify::tiers::jev_obfuscate_tests`,
//! `classify::tiers::jev_review_tests::name_tokens_apostrophes_hyphens_titlecase`,
//! `classify::tiers::jev_round3_tests::category_text_survives_learning`.

use std::collections::HashSet;

use super::jev_obfuscate::TokenKind;
use super::jev_patterns::is_name_stop_word;
use super::jev_trailers::strip_brackets;
use super::jev_vocab::Vocab;

/// Lowercase with whitespace runs collapsed and `’` read as `'`: the
/// identity of a name.
pub(super) fn name_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .replace('\u{2019}', "'")
        .to_lowercase()
}

/// Whether `part` is a name part: letters, with inner `'`, `’` or `-`.
fn is_name_part(part: &str) -> bool {
    part.chars().any(char::is_alphabetic)
        && part
            .chars()
            .all(|c| c.is_alphabetic() || matches!(c, '\'' | '\u{2019}' | '-'))
}

/// `word` in Titlecase (`frank` → `Frank`).
fn titlecase(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |f| {
        f.to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect()
    })
}

/// Every name the run must hide, deduplicated case-insensitively.
#[derive(Default)]
pub(super) struct NameSet {
    pub(super) entries: Vec<(String, TokenKind)>,
    seen: HashSet<String>,
    /// Stop-listed given names of known people, matched only in Titlecase.
    pub(super) titled: Vec<String>,
    /// Entries left unmatched case-insensitively (too short, a stop-list
    /// name token, or category vocabulary).
    pub(super) skipped: usize,
    /// Classification vocabulary (categories, rules, [`super::jev_vocab::TECH_WORDS`]);
    /// a person name or part made only of it is never matched (#111).
    vocab: Vocab,
}

impl NameSet {
    /// Words that are never a name (#111, gate B): see [`Vocab::new`].
    pub(super) fn set_vocab(&mut self, texts: &[String]) {
        self.vocab = Vocab::new(texts);
    }

    /// Whether `word` is classification vocabulary.
    pub(super) fn is_common(&self, word: &str) -> bool {
        self.vocab.is_common(word)
    }

    /// Whether every word of `name` is classification vocabulary.
    fn is_vocab(&self, name: &str) -> bool {
        self.vocab.is_all_common(name)
    }

    /// Add `name` as `kind`; `true` when it is new. A person name made only
    /// of classification vocabulary (`test`, `deploy-bot`, `ci`) is skipped.
    pub(super) fn push(&mut self, name: &str, kind: TokenKind) -> bool {
        let name = name.trim();
        if kind == TokenKind::Person && self.is_vocab(name) {
            self.skipped += 1;
            return false;
        }
        if name.is_empty() || !self.seen.insert(name_key(name)) {
            return false;
        }
        self.entries.push((name.to_string(), kind));
        true
    }

    /// Add the Titlecase form of a stop-listed given name; `true` when new.
    fn push_titled(&mut self, part: &str) -> bool {
        let t = titlecase(part);
        if self.is_vocab(&t) || self.titled.contains(&t) {
            return false;
        }
        self.titled.push(t);
        true
    }

    /// Add a person name and its parts.
    ///
    /// What (#111): a bot account (`x[bot]`, `x-bot`, `x_bot`, `bot`) is
    /// not a person and is skipped. Any other name of two or more
    /// characters is matched whole (`jdoe`, `Max`, `jroe-acme`), unless
    /// every word of it is classification vocabulary (gate B: `test`,
    /// `admin`, `ci`, `deploy`). For a multi-word name only (bracketed
    /// parts dropped; `.` and `_` split like spaces), each token and each
    /// hyphen part of three or more characters — letters with inner `'`,
    /// `’` or `-` (`O'Brien`, `Mary-Jane`, `Jane`) — is matched on its own,
    /// unless it is vocabulary (never matched alone) or a stop-list given
    /// name (matched only in Titlecase and not at a sentence start:
    /// `ask Frank`, not `frank` or `Frank said`). A single-token login
    /// teaches no parts. Skips are counted.
    /// Test: `jev_review_tests::name_tokens_apostrophes_hyphens_titlecase`,
    /// `jev_round3_tests::category_text_survives_learning`,
    /// `jev_gateb_tests::generic_logins_are_not_learned`.
    pub(super) fn push_person(&mut self, name: &str) -> bool {
        let name = name.trim();
        let lower = name.to_lowercase();
        let bot = lower == "bot" || ["[bot]", "-bot", "_bot"].iter().any(|s| lower.ends_with(s));
        if name.chars().count() < 2 || bot {
            self.skipped += usize::from(!name.is_empty());
            return false;
        }
        let mut changed = self.push(name, TokenKind::Person);
        let unbracketed = strip_brackets(name);
        let tokens: Vec<&str> = unbracketed
            .split(|c: char| c.is_whitespace() || c == '.' || c == '_')
            .map(|t| t.trim_matches(|c: char| !c.is_alphanumeric()))
            .filter(|t| !t.is_empty())
            .collect();
        if tokens.len() < 2 {
            return changed;
        }
        for t in tokens {
            let hyphen_parts = t.split('-').filter(|_| t.contains('-'));
            for part in std::iter::once(t).chain(hyphen_parts) {
                if part.chars().count() < 3 || !is_name_part(part) {
                    continue;
                }
                if self.is_vocab(part) {
                    self.skipped += 1;
                    continue;
                }
                if is_name_stop_word(part) {
                    self.skipped += 1;
                    changed |= self.push_titled(part);
                    continue;
                }
                changed |= self.push(part, TokenKind::Person);
            }
        }
        changed
    }
}

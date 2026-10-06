//! The name set and name matcher behind the Jev pseudonymizer (#111).
//!
//! Why: people, repositories and operator terms are matched by a list, not a
//! shape; the list grows during a run (authors, trailer names) and its
//! matching rules (name parts, stop-listed given names, apostrophes) are
//! their own concern. Split out of `jev_obfuscate.rs` for the size cap.
//! What: [`NameSet`] collects the names; [`NameMatcher::build`] compiles
//! them into a case-insensitive regex plus a case-sensitive one for
//! stop-listed given names, which match only in Titlecase (`Frank`).
//! Test: `classify::tiers::jev_obfuscate_tests`,
//! `classify::tiers::jev_review_tests::name_tokens_apostrophes_hyphens_titlecase`,
//! `classify::tiers::jev_round3_tests::category_text_survives_learning`.

use std::collections::{HashMap, HashSet};

use regex::{Captures, Regex, RegexBuilder};

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
    titled: Vec<String>,
    /// Entries left unmatched case-insensitively (too short, a stop-list
    /// name token, or category vocabulary).
    pub(super) skipped: usize,
    /// Classification vocabulary (categories, rules, [`super::jev_vocab::TECH_WORDS`]);
    /// a person name or part made only of it is never matched (#111).
    vocab: Vocab,
}

/// Punctuation after which a Titlecase word starts a sentence or item.
const SENTENCE_BREAKS: [char; 14] = [
    '.', '!', '?', ':', ';', '\n', '*', '-', '>', '#', '(', '[', '"', '\'',
];

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

/// The compiled name matcher.
pub(super) struct NameMatcher {
    re: Option<Regex>,
    titled: Option<Regex>,
    kinds: HashMap<String, TokenKind>,
}

impl NameMatcher {
    /// Compile `set`; `None` when it holds no name.
    ///
    /// # Errors
    ///
    /// `(names, error)` when a matcher would exceed `size_limit`.
    pub(super) fn build(
        set: &NameSet,
        size_limit: usize,
    ) -> Result<Option<Self>, (usize, regex::Error)> {
        let list: Vec<String> = set.entries.iter().map(|(n, _)| n.clone()).collect();
        let count = list.len() + set.titled.len();
        let compile = |names: &[String], fold: bool| {
            if names.is_empty() {
                return Ok(None);
            }
            names_regex(names, size_limit, fold)
                .map(Some)
                .map_err(|e| (count, e))
        };
        let re = compile(&list, true)?;
        let titled = compile(&set.titled, false)?;
        if re.is_none() && titled.is_none() {
            return Ok(None);
        }
        let mut kinds: HashMap<String, TokenKind> =
            set.entries.iter().map(|(n, k)| (name_key(n), *k)).collect();
        for t in &set.titled {
            kinds.entry(name_key(t)).or_insert(TokenKind::Person);
        }
        Ok(Some(Self { re, titled, kinds }))
    }

    /// Replace every matched name in `text` via `token`.
    pub(super) fn apply(
        &self,
        text: &str,
        mut token: impl FnMut(TokenKind, &str) -> String,
    ) -> String {
        let mut out = text.to_string();
        for (i, re) in [&self.re, &self.titled].into_iter().enumerate() {
            let Some(re) = re else { continue };
            let src = out.clone();
            out = re
                .replace_all(&src, |c: &Captures| {
                    // #111 (gate B): a Titlecase-only given name at a sentence
                    // or item start is an ordinary word (`Mark as done`).
                    let start = c.get(0).map_or(0, |m| m.start());
                    let before = src[..start].trim_end_matches([' ', '\t']);
                    if i == 1 && (before.is_empty() || before.ends_with(SENTENCE_BREAKS)) {
                        return c[0].to_string();
                    }
                    // An unknown case-folded spelling still gets a pseudonym.
                    let kind = self
                        .kinds
                        .get(&name_key(&c[0]))
                        .copied()
                        .unwrap_or(TokenKind::Term);
                    token(kind, &c[0])
                })
                .into_owned();
        }
        out
    }
}

/// The whole-word pattern for one configured name.
///
/// What: the name, escaped, with each whitespace run matching any
/// whitespace, and an ASCII word boundary on each side whose edge character
/// is an ASCII word character (so `c++` and `José` still match).
fn bounded(name: &str) -> String {
    let edge = |c: Option<char>| c.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    // #111: `'` and `’` match each other (`O'Brien`, `O’Brien`).
    let body = name
        .split_whitespace()
        .map(|w| {
            regex::escape(w)
                .chars()
                .map(|c| match c {
                    '\'' | '\u{2019}' => "['\u{2019}]".to_string(),
                    c => c.to_string(),
                })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join(r"\s+");
    let pre = if edge(name.chars().next()) {
        r"(?-u:\b)"
    } else {
        ""
    };
    let post = if edge(name.chars().last()) {
        r"(?-u:\b)"
    } else {
        ""
    };
    format!("{pre}{body}{post}")
}

/// One regex over `names`, case-insensitive when `fold` (#111).
///
/// Why: a leftmost-longest literal matcher returned only the longest
/// candidate at a position, so when it failed the whole-word check
/// (`acme-web` inside `acme-webhooks`) the shorter overlapping name
/// (`acme`) was never tried.
/// What: `(?i)(?:n1|n2|…)` with each name [`bounded`], sorted longest
/// first; the regex engine falls back to the next alternative when a
/// boundary fails. `size_limit` caps the compiled program.
/// Test: `jev_obfuscate_tests::overlapping_names_fall_back_to_the_shorter`.
///
/// # Errors
///
/// The compiled matcher would exceed `size_limit`.
fn names_regex(names: &[String], size_limit: usize, fold: bool) -> Result<Regex, regex::Error> {
    let mut sorted: Vec<&String> = names.iter().collect();
    sorted.sort_by(|a, b| {
        b.chars()
            .count()
            .cmp(&a.chars().count())
            .then_with(|| a.cmp(b))
    });
    let alternation = sorted
        .iter()
        .map(|n| bounded(n))
        .collect::<Vec<_>>()
        .join("|");
    RegexBuilder::new(&format!("(?:{alternation})"))
        .case_insensitive(fold)
        .size_limit(size_limit)
        .dfa_size_limit(size_limit)
        .build()
}

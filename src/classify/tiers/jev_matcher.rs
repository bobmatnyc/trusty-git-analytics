//! The compiled name matcher behind the Jev pseudonymizer (#111).
//!
//! Why (gate B 2): the matcher used to be one regex alternation with one
//! branch per name. A production database names about 225,000 people,
//! repositories and file basenames; that regex needed about 1 GiB and more
//! than 22 minutes to build in a debug build, so the run failed closed at
//! the default cap. An Aho-Corasick automaton holds the same names in a
//! few bytes per name and builds in linear time.
//! What: [`NameMatcher::build`] compiles a [`NameSet`] into two
//! [`Lexicon`]s, a case-folded one for every entry and a case-sensitive one
//! for the Titlecase-only given names. [`NameMatcher::apply`] keeps the
//! regex's semantics: leftmost match first; at one position the longest
//! name, ties by spelling; an ASCII word boundary on each name edge that is
//! an ASCII word character, so no name matches inside a longer word and a
//! longer name that fails the boundary falls back to a shorter one;
//! whitespace runs match any whitespace run, and `'` matches `’`.
//! Test: `classify::tiers::jev_round5_tests::a_name_inside_a_longer_word_is_not_replaced`,
//! `classify::tiers::jev_round5_tests::the_longest_of_two_overlapping_names_wins`,
//! `classify::tiers::jev_round5_tests::half_a_million_names_build_at_the_default_cap`,
//! `classify::tiers::jev_obfuscate_tests::overlapping_names_fall_back_to_the_shorter`.

use std::collections::HashMap;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder, AhoCorasickKind, MatchKind};

use super::jev_names::{name_key, NameSet};
use super::jev_obfuscate::TokenKind;

/// Punctuation after which a Titlecase word starts a sentence or item.
const SENTENCE_BREAKS: [char; 14] = [
    '.', '!', '?', ':', ';', '\n', '*', '-', '>', '#', '(', '[', '"', '\'',
];

/// Why a name matcher could not be built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MatcherError {
    /// The compiled matcher needs more than `limit` heap bytes.
    TooBig {
        /// The cap it exceeded (`llm.jev.name_matcher_bytes`).
        limit: usize,
    },
    /// The automaton could not be built (its state count overflowed).
    Build,
}

/// One name as the automaton knows it.
struct Pat {
    /// Order at one position: longest name first, then by spelling.
    rank: u32,
    kind: TokenKind,
    /// The first character is an ASCII word character: the text before
    /// the match must not be one.
    word_start: bool,
    /// The last character is an ASCII word character: the text after the
    /// match must not be one.
    word_end: bool,
}

/// An automaton over normalized names, plus each name's match rules.
struct Lexicon {
    ac: AhoCorasick,
    pats: Vec<Pat>,
    fold: bool,
}

/// An ASCII word character, as `(?-u:\b)` reads it.
fn is_word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The only character `it` yields, if it yields exactly one.
fn single(mut it: impl Iterator<Item = char>) -> Option<char> {
    match (it.next(), it.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

/// One representative of `c`'s simple case-folding class.
///
/// What: lowercase, then uppercase, then lowercase again, each step only
/// when it maps to a single character, so `K`, `k` and the Kelvin sign, or
/// `Σ`, `σ` and `ς`, fold together as they do under a case-insensitive
/// regex.
fn fold_char(c: char) -> char {
    if c.is_ascii() {
        return c.to_ascii_lowercase();
    }
    let lower = single(c.to_lowercase()).unwrap_or(c);
    let upper = single(lower.to_uppercase()).unwrap_or(lower);
    single(upper.to_lowercase()).unwrap_or(lower)
}

/// `text` as the automaton reads it, and the byte offset in `text` of each
/// normalized byte, plus one entry for the end.
///
/// What: each whitespace run becomes one space, `’` becomes `'`, and with
/// `fold` each character is case-folded. Offsets map a normalized match
/// back to the original span, whose bytes may differ in length.
fn normalize(text: &str, fold: bool) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len() + 1);
    let mut in_space = false;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if !in_space {
                out.push(' ');
                map.push(i);
            }
            in_space = true;
            continue;
        }
        in_space = false;
        let n = match c {
            '\u{2019}' => '\'',
            c if fold => fold_char(c),
            c => c,
        };
        out.push(n);
        map.resize(out.len(), i);
    }
    map.push(text.len());
    (out, map)
}

impl Lexicon {
    /// Compile `names`; `None` when there are none.
    fn build(names: &[(&str, TokenKind)], fold: bool) -> Result<Option<Self>, MatcherError> {
        if names.is_empty() {
            return Ok(None);
        }
        let mut order: Vec<usize> = (0..names.len()).collect();
        let chars: Vec<usize> = names.iter().map(|(n, _)| n.chars().count()).collect();
        order.sort_by(|&a, &b| {
            chars[b]
                .cmp(&chars[a])
                .then_with(|| names[a].0.cmp(names[b].0))
        });
        let mut rank = vec![0_u32; names.len()];
        for (r, &i) in order.iter().enumerate() {
            rank[i] = u32::try_from(r).map_err(|_| MatcherError::Build)?;
        }
        let edge = |c: Option<char>| c.is_some_and(|c| u8::try_from(c).is_ok_and(is_word_byte));
        let pats = names
            .iter()
            .zip(rank)
            .map(|((n, kind), rank)| Pat {
                rank,
                kind: *kind,
                word_start: edge(n.chars().next()),
                word_end: edge(n.chars().last()),
            })
            .collect();
        let normalized = names.iter().map(|(n, _)| normalize(n, fold).0);
        let ac = AhoCorasickBuilder::new()
            .match_kind(MatchKind::Standard)
            // A DFA for a small set can outweigh an NFA for a larger one;
            // one kind keeps the size cap monotone in the names.
            .kind(Some(AhoCorasickKind::ContiguousNFA))
            .build(normalized)
            .map_err(|_| MatcherError::Build)?;
        Ok(Some(Self { ac, pats, fold }))
    }

    /// Heap bytes held by the automaton and the per-name rules.
    fn memory_usage(&self) -> usize {
        self.ac.memory_usage() + self.pats.capacity() * std::mem::size_of::<Pat>()
    }

    /// The matches in `text` a leftmost-first regex over the names, longest
    /// first, would report: `(start, end, name index)`, non-overlapping.
    ///
    /// What: every occurrence of every name (overlapping search), minus
    /// those failing a word boundary, sorted by start then rank; the first
    /// at each position wins and the scan resumes at its end.
    fn find(&self, text: &str) -> Vec<(usize, usize, usize)> {
        let (norm, map) = normalize(text, self.fold);
        let bytes = text.as_bytes();
        let mut hits: Vec<(usize, u32, usize, usize)> = self
            .ac
            .find_overlapping_iter(&norm)
            .filter_map(|m| {
                let id = m.pattern().as_usize();
                let p = &self.pats[id];
                let (s, e) = (map[m.start()], map[m.end()]);
                let start_ok = !p.word_start || s == 0 || !is_word_byte(bytes[s - 1]);
                let end_ok = !p.word_end || e == bytes.len() || !is_word_byte(bytes[e]);
                (start_ok && end_ok).then_some((s, p.rank, e, id))
            })
            .collect();
        hits.sort_unstable();
        let mut out = Vec::new();
        let mut pos = 0;
        for (s, _, e, id) in hits {
            if s >= pos {
                out.push((s, e, id));
                pos = e;
            }
        }
        out
    }
}

/// The compiled name matcher.
pub(super) struct NameMatcher {
    folded: Option<Lexicon>,
    titled: Option<Lexicon>,
}

impl NameMatcher {
    /// Compile `set`; `None` when it holds no name.
    ///
    /// What: a Titlecase-only name takes the kind of a same-keyed entry,
    /// else `Person`, as the regex matcher's kind lookup did.
    ///
    /// # Errors
    ///
    /// `(names, error)` when the automaton cannot be built or its heap
    /// bytes exceed `size_limit`.
    pub(super) fn build(
        set: &NameSet,
        size_limit: usize,
    ) -> Result<Option<Self>, (usize, MatcherError)> {
        let count = set.entries.len() + set.titled.len();
        let fail = |e| (count, e);
        let entries: Vec<(&str, TokenKind)> =
            set.entries.iter().map(|(n, k)| (n.as_str(), *k)).collect();
        let folded = Lexicon::build(&entries, true).map_err(fail)?;
        let mut titled_kind: HashMap<String, TokenKind> = set
            .titled
            .iter()
            .map(|t| (name_key(t), TokenKind::Person))
            .collect();
        if !titled_kind.is_empty() {
            for (n, k) in &set.entries {
                if let Some(kind) = titled_kind.get_mut(&name_key(n)) {
                    *kind = *k;
                }
            }
        }
        let titled: Vec<(&str, TokenKind)> = set
            .titled
            .iter()
            .map(|t| {
                let kind = titled_kind.get(&name_key(t)).copied();
                (t.as_str(), kind.unwrap_or(TokenKind::Person))
            })
            .collect();
        let titled = Lexicon::build(&titled, false).map_err(fail)?;
        let m = Self { folded, titled };
        if m.memory_usage() > size_limit {
            return Err(fail(MatcherError::TooBig { limit: size_limit }));
        }
        Ok((m.folded.is_some() || m.titled.is_some()).then_some(m))
    }

    /// Heap bytes held by both automata, compared with the size cap.
    pub(super) fn memory_usage(&self) -> usize {
        [&self.folded, &self.titled]
            .into_iter()
            .flatten()
            .map(Lexicon::memory_usage)
            .sum()
    }

    /// Replace every matched name in `text` via `token`.
    pub(super) fn apply(
        &self,
        text: &str,
        mut token: impl FnMut(TokenKind, &str) -> String,
    ) -> String {
        let mut out = text.to_string();
        for (i, lex) in [&self.folded, &self.titled].into_iter().enumerate() {
            let Some(lex) = lex else { continue };
            let src = out;
            let mut next = String::with_capacity(src.len());
            let mut last = 0;
            for (s, e, id) in lex.find(&src) {
                // #111 (gate B): a Titlecase-only given name at a sentence
                // or item start is an ordinary word (`Mark as done`).
                let before = src[..s].trim_end_matches([' ', '\t']);
                if i == 1 && (before.is_empty() || before.ends_with(SENTENCE_BREAKS)) {
                    continue;
                }
                next.push_str(&src[last..s]);
                next.push_str(&token(lex.pats[id].kind, &src[s..e]));
                last = e;
            }
            next.push_str(&src[last..]);
            out = next;
        }
        out
    }
}

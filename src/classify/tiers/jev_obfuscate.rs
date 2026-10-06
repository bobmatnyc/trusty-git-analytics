//! Pseudonymization in front of the Jev provider (#111).
//!
//! Why: Jev is a third-party hosted model. A commit message can name people,
//! e-mail addresses, tickets, URLs, hosts, file paths, branches, repositories
//! and internal services; none of that may leave the host. Only the Jev path
//! uses this module; the other providers are unchanged.
//! What: [`Obfuscator::obfuscate`] replaces each sensitive span with a
//! per-run pseudonym (`EMAIL_1`, `PERSON_1`, `TICKET_1`, `URL_1`, `HOST_1`,
//! `PATH_1`, `BRANCH_1`, `REPO_1`, `TERM_1`, `ID_1`), numbered in first-seen
//! order and stable for the run. A file keeps its extension (`PATH_1.md`).
//! Other text passes through unchanged. The pseudonym → original map stays
//! in this struct: it is never serialized, sent or logged, and the
//! hand-written `Debug` prints counts only. [`ObfuscatedText`] is the only
//! text type the Jev request builder accepts. Detection rules live in
//! [`super::jev_patterns`], trailer parsing in [`super::jev_trailers`].
//! Test: `classify::tiers::jev_obfuscate_tests`,
//! `classify::tiers::jev_redaction_tests`.

use std::collections::HashMap;

use regex::{Captures, Regex};
use serde::{Serialize, Serializer};
use tracing::{info, warn};

use super::jev_error::JevError;
use super::jev_names::{name_key, NameMatcher, NameSet};
use super::jev_patterns::{
    classify_dotted, is_ipv6, is_path, is_placeholder, is_public_branch, known_ext, patterns,
    split_trailing_punct, Dotted, Patterns, FILE_STEMS,
};
use super::jev_tickets::{is_record_id, ticket_start};
use super::jev_trailers::{item_names, items, names_in, scan, Role, Trailer};

/// Compiled-size cap for the name matcher. Thousands of names fit well
/// inside it; past it, building the matcher is an error, never a silent
/// skip (#111).
#[cfg(test)]
pub(crate) const NAME_MATCHER_SIZE_LIMIT: usize =
    crate::core::config::JEV_DEFAULT_NAME_MATCHER_BYTES;

/// The categories of sensitive span, one pseudonym prefix each.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum TokenKind {
    Email,
    Person,
    Ticket,
    Url,
    Host,
    Path,
    Branch,
    Repo,
    Term,
    Id,
}

impl TokenKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Email => "EMAIL",
            Self::Person => "PERSON",
            Self::Ticket => "TICKET",
            Self::Url => "URL",
            Self::Host => "HOST",
            Self::Path => "PATH",
            Self::Branch => "BRANCH",
            Self::Repo => "REPO",
            Self::Term => "TERM",
            Self::Id => "ID",
        }
    }

    /// Case-insensitive kinds share one pseudonym across spellings; names
    /// also across whitespace. Ticket keys are case-insensitive (#111).
    fn key(self, original: &str) -> String {
        match self {
            Self::Url | Self::Path | Self::Branch | Self::Id => original.to_string(),
            Self::Person | Self::Repo | Self::Term => name_key(original),
            Self::Email | Self::Host | Self::Ticket => original.to_lowercase(),
        }
    }
}

/// Text that is safe to send: the output of [`Obfuscator::obfuscate`], or a
/// fixed string compiled into tga.
///
/// Why: the Jev request types hold this, never `String`, so a raw commit
/// message cannot reach the HTTP layer without a compile error.
/// What: a private `String`; the only constructors are the obfuscator and
/// [`ObfuscatedText::fixed`], which takes `&'static str` only.
/// Test: `jev_obfuscate_tests::no_generated_secret_survives`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ObfuscatedText(String);

impl ObfuscatedText {
    /// Wrap a string literal compiled into tga (instructions, criteria).
    pub(crate) fn fixed(text: &'static str) -> Self {
        Self(text.to_string())
    }

    /// Wrap operator-written category text (a category name or its
    /// description). #111 (gate B): it is configuration, not commit text,
    /// and is sent verbatim so the model sees the vocabulary it classifies
    /// into; do not put customer names in category descriptions.
    pub(crate) fn config(text: String) -> Self {
        Self(text)
    }

    /// The pseudonymized text.
    #[cfg(test)]
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ObfuscatedText {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.0)
    }
}

/// Names the name pass looks for, from the tga config and the database.
#[derive(Clone, Default)]
pub(crate) struct KnownNames {
    /// Repository names, directory basenames and owners (`REPO_n`).
    pub(crate) repos: Vec<String>,
    /// Roster names, aliases, logins, and the run's author names and e-mail
    /// local-parts (`PERSON_n`).
    pub(crate) people: Vec<String>,
    /// Operator-supplied sensitive terms, e.g. service names (`TERM_n`).
    pub(crate) terms: Vec<String>,
    /// Operator-supplied record-id regexes (`ID_n`, `llm.jev.id_patterns`).
    pub(crate) id_patterns: Vec<String>,
    /// Category names and descriptions: their words are never matched as
    /// a person name on their own (#111).
    pub(crate) vocab: Vec<String>,
}

/// Names a run learns from the database before its first request (#111).
#[derive(Clone, Default)]
pub(crate) struct RunNames {
    /// Every person the database knows, including every identity-trailer
    /// name in every stored commit message (`PERSON_n`).
    pub(crate) people: Vec<String>,
    /// Every repository file path the database records (`PATH_n`).
    pub(crate) paths: Vec<String>,
    /// Every repository name the database stores (`REPO_n`).
    pub(crate) repos: Vec<String>,
}

/// Per-run pseudonymizer.
///
/// Why / What: see the module doc. After a failed matcher rebuild the
/// pseudonymizer is poisoned: names it has accepted may be missing from the
/// matcher, so every later call fails closed with [`JevError::Poisoned`].
/// Test: `jev_obfuscate_tests::*`, `jev_redaction_tests::*`.
pub(crate) struct Obfuscator {
    p: &'static Patterns,
    tokens: HashMap<(TokenKind, String), String>,
    /// Pseudonym → the first original spelling it replaced; on-host only,
    /// for the payload dump's token map (#111).
    originals: HashMap<String, String>,
    counters: HashMap<TokenKind, usize>,
    names: NameSet,
    matcher: Option<NameMatcher>,
    /// Operator record-id regexes, run before every other pass.
    id_patterns: Vec<Regex>,
    size_limit: usize,
    /// #111: set when a rebuild failed; every later call is refused.
    poisoned: bool,
}

// #111: counts only — the map holds the originals and must never be logged.
impl std::fmt::Debug for Obfuscator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Obfuscator")
            .field("pseudonyms", &self.tokens.len())
            .field("names", &self.names.entries.len())
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

impl Obfuscator {
    /// A pseudonymizer that also replaces the config-derived `names`.
    ///
    /// # Errors
    ///
    /// As [`Self::with_size_limit`].
    #[cfg(test)]
    pub(crate) fn new(names: &KnownNames) -> Result<Self, JevError> {
        Self::with_size_limit(names, NAME_MATCHER_SIZE_LIMIT)
    }

    /// [`Self::new`] with an explicit compiled-size cap for the matcher.
    ///
    /// # Errors
    ///
    /// A built-in pattern or an operator id pattern does not compile, or
    /// the matcher would exceed `size_limit` — a hard error, so no run
    /// proceeds with names silently unmatched (#111).
    pub(crate) fn with_size_limit(names: &KnownNames, size_limit: usize) -> Result<Self, JevError> {
        let p = patterns()?;
        let mut set = NameSet::default();
        set.set_vocab(&names.vocab);
        // Terms first: an operator term wins over a same-spelled name.
        for t in &names.terms {
            set.push(t, TokenKind::Term);
        }
        for r in names.repos.iter().filter(|r| r.trim().chars().count() >= 2) {
            set.push(r, TokenKind::Repo);
        }
        for person in &names.people {
            set.push_person(person);
        }
        // #111 (gate B re-run): bare build files are repository file names.
        for stem in FILE_STEMS.iter().chain(&["codeowners"]) {
            set.push(stem, TokenKind::Path);
        }
        let id_patterns = names
            .id_patterns
            .iter()
            .enumerate()
            .map(|(i, pat)| Regex::new(pat).map_err(|_| JevError::IdPattern(i)))
            .collect::<Result<Vec<_>, _>>()?;
        let mut obf = Self {
            p,
            tokens: HashMap::new(),
            originals: HashMap::new(),
            counters: HashMap::new(),
            names: set,
            matcher: None,
            id_patterns,
            size_limit,
            poisoned: false,
        };
        obf.rebuild()?;
        Ok(obf)
    }

    fn check(&self) -> Result<(), JevError> {
        if self.poisoned {
            Err(JevError::Poisoned)
        } else {
            Ok(())
        }
    }

    /// Learn repository file names from the paths the database records.
    ///
    /// Why (#111, gate B re-run): a bare file name in prose (`Makefile`,
    /// `invoice_sync.py`, a file with an unusual or no extension) names the
    /// repository layout; the extension rule alone misses some.
    /// What: for each path, its basename (three or more characters) and,
    /// when the part before its first `.` is identifier-like (four or more
    /// characters with `_`, `-`, a digit or a camelCase step, e.g.
    /// `invoice_sync`, `PriceTable`), that stem too, become `PATH_n` names in
    /// the same matcher as people, under the same fail-closed size cap. An
    /// extension-less basename that is classification vocabulary (`build`)
    /// is skipped.
    /// Test: `jev_gateb2_tests::db_file_names_are_redacted`.
    ///
    /// # Errors
    ///
    /// As [`Self::add_people`].
    pub(crate) fn add_files(&mut self, paths: &[String]) -> Result<(), JevError> {
        self.check()?;
        let ident = |s: &str| {
            s.chars().count() >= 4
                && (s.contains(['_', '-'])
                    || s.bytes().any(|b| b.is_ascii_digit())
                    || s.as_bytes()
                        .windows(2)
                        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase()))
        };
        let mut changed = false;
        for path in paths {
            let base = path.rsplit(['/', '\\']).next().unwrap_or("").trim();
            if base.chars().count() < 3 || (!base.contains('.') && self.names.is_common(base)) {
                continue;
            }
            changed |= self.names.push(base, TokenKind::Path);
            if let Some((stem, _)) = base.split_once('.') {
                if ident(stem) && !self.names.is_common(stem) {
                    changed |= self.names.push(stem, TokenKind::Path);
                }
            }
        }
        if changed {
            self.rebuild()?;
        }
        Ok(())
    }

    /// Learn repository and org names the database stores.
    ///
    /// Why (#111, critic HIGH 2): `commits.repository` and
    /// `pull_requests.repository` name repositories the config may not
    /// list, such as ones found by org-wide discovery.
    /// What: each name and, for an `owner/name` slug, each part, of two or
    /// more characters, becomes a `REPO_n` name in the same matcher as
    /// people, under the same fail-closed size cap. The column default
    /// `unknown` and classification vocabulary (`platform`, `docs`) are
    /// skipped, so a category word is never hidden.
    /// Test: `jev_round4_tests::org_and_repo_names_from_every_source_are_redacted`.
    ///
    /// # Errors
    ///
    /// As [`Self::add_people`].
    pub(crate) fn add_repos(&mut self, repos: &[String]) -> Result<(), JevError> {
        self.check()?;
        let mut changed = false;
        for repo in repos {
            let parts = repo.split('/').filter(|_| repo.contains('/'));
            for name in std::iter::once(repo.as_str()).chain(parts).map(str::trim) {
                let skip = name.chars().count() < 2
                    || name.eq_ignore_ascii_case("unknown")
                    || self.names.is_common(name);
                if !skip {
                    changed |= self.names.push(name, TokenKind::Repo);
                }
            }
        }
        if changed {
            self.rebuild()?;
        }
        Ok(())
    }

    /// Add person names (the run's authors) and rebuild the matcher.
    ///
    /// # Errors
    ///
    /// Poisoned, or the rebuild fails (which poisons).
    pub(crate) fn add_people(&mut self, people: &[String]) -> Result<(), JevError> {
        self.check()?;
        let mut changed = false;
        for person in people {
            changed |= self.names.push_person(person);
        }
        if changed {
            self.rebuild()?;
        }
        Ok(())
    }

    /// Remember every name in `text`'s identity trailers for the run.
    ///
    /// # Errors
    ///
    /// As [`Self::add_people`].
    pub(crate) fn learn_trailers(&mut self, text: &str) -> Result<(), JevError> {
        let found = names_in(self.p, text);
        self.add_people(&found)
    }

    fn rebuild(&mut self) -> Result<(), JevError> {
        if self.names.skipped > 0 {
            // #111: counts only; a name is never logged.
            info!(
                skipped = self.names.skipped,
                "jev pseudonymizer: name entries not matched on their own (stop-list or too short)"
            );
        }
        match NameMatcher::build(&self.names, self.size_limit) {
            Ok(m) => {
                self.matcher = m;
                Ok(())
            }
            Err((count, e)) => {
                // #111: the name set now holds names the matcher lacks; a
                // later call would pass them through. Refuse every one.
                self.poisoned = true;
                let why = match e {
                    regex::Error::CompiledTooBig(limit) => format!("exceeds {limit} bytes"),
                    _ => "is invalid".to_string(),
                };
                warn!(
                    names = count,
                    "jev pseudonymizer disabled: matcher rebuild failed"
                );
                Err(JevError::Matcher { count, why })
            }
        }
    }

    /// The pseudonym for `original`, allocating the next one on first sight.
    fn token(&mut self, kind: TokenKind, original: &str) -> String {
        let key = (kind, kind.key(original));
        if let Some(t) = self.tokens.get(&key) {
            return t.clone();
        }
        let n = self.counters.entry(kind).or_insert(0);
        *n += 1;
        let t = format!("{}_{n}", kind.prefix());
        self.tokens.insert(key, t.clone());
        self.originals.insert(t.clone(), original.to_string());
        t
    }

    /// Every pseudonym in `text` and the original it replaced.
    ///
    /// Why (#111, gate B): the owner reports redaction loss per payload; a
    /// scanner needs, for each dumped body, which original words each token
    /// replaced. On-host only: the payload dump writes it next to the body
    /// and nothing sends it.
    /// Test: `jev_gateb2_tests::dump_writes_the_token_map`.
    pub(crate) fn originals_in(
        &self,
        text: &ObfuscatedText,
    ) -> std::collections::BTreeMap<String, String> {
        self.p
            .placeholder
            .find_iter(&text.0)
            .filter_map(|m| {
                let t = m.as_str();
                self.originals.get(t).map(|o| (t.to_string(), o.clone()))
            })
            .collect()
    }

    /// Replace every sensitive span in `text` with its pseudonym.
    ///
    /// Why / What: see the module doc. Identity trailers keep their prefix
    /// and token (`Co-authored-by:`) and have each name and address
    /// replaced, as do their folded continuation lines; a name found in a
    /// trailer is replaced wherever it appears in this or any later message
    /// of the run.
    /// Test: `jev_obfuscate_tests::trailers_become_person_and_email`,
    /// `jev_tests::outbound_body_carries_no_sensitive_string`.
    ///
    /// # Errors
    ///
    /// Poisoned, or a new trailer name makes the matcher too big to build.
    pub(crate) fn obfuscate(&mut self, text: &str) -> Result<ObfuscatedText, JevError> {
        self.check()?;
        self.learn_trailers(text)?;
        let p = self.p;
        let lines: Vec<String> = scan(p, text)
            .into_iter()
            .zip(text.split('\n'))
            .map(|(role, line)| match role {
                Role::Trailer(t) => self.trailer(&t),
                Role::Continuation {
                    indent,
                    value,
                    usernames,
                    ..
                } => format!("{indent}{}", self.value(value, usernames)),
                Role::Text => self.line(line),
            })
            .collect();
        Ok(ObfuscatedText(lines.join("\n")))
    }

    /// `Token: Name <email>` → `Token: PERSON_n <EMAIL_n>`.
    fn trailer(&mut self, t: &Trailer<'_>) -> String {
        let value = self.value(t.value, t.usernames);
        if value.is_empty() {
            return format!("{}{}:", t.prefix, t.token);
        }
        format!("{}{}: {value}", t.prefix, t.token)
    }

    /// A trailer value with every name → `PERSON_n` and address → `EMAIL_n`;
    /// nothing else of it survives.
    fn value(&mut self, value: &str, usernames: bool) -> String {
        let p = self.p;
        items(value, usernames)
            .into_iter()
            .map(|item| {
                let tag = if item.trim_start().starts_with('#') {
                    "#"
                } else {
                    ""
                };
                let mut parts: Vec<String> = item_names(p, item)
                    .iter()
                    .map(|n| format!("{tag}{}", self.token(TokenKind::Person, n)))
                    .collect();
                for e in p.email.find_iter(item) {
                    parts.push(format!("<{}>", self.token(TokenKind::Email, e.as_str())));
                }
                parts.join(" ")
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Apply every pass to one non-trailer line.
    ///
    /// #111: operator id patterns run first; known names run before the
    /// host, path and ticket shapes, so a remembered `jane.doe` is a
    /// person, never a host. URLs and addresses go whole before names, so
    /// one address keeps one pseudonym.
    fn line(&mut self, line: &str) -> String {
        let p = self.p;
        let mut s = self.merge_refs(line);
        for i in 0..self.id_patterns.len() {
            let re = self.id_patterns[i].clone();
            s = self.regex_pass(&s, &re, TokenKind::Id, false);
        }
        let s = self.regex_pass(&s, &p.url, TokenKind::Url, true);
        let s = self.regex_pass(&s, &p.email, TokenKind::Email, false);
        let s = self.names(&s);
        let s = self.ipv6(&s);
        let s = self.paths(&s);
        let s = self.dotted(&s);
        let s = self.tickets(&s);
        let s = self.record_ids(&s);
        self.mentions(&s)
    }

    /// Replace each `re` match; with `trim`, sentence punctuation at the end
    /// of the match stays outside the pseudonym.
    fn regex_pass(&mut self, text: &str, re: &Regex, kind: TokenKind, trim: bool) -> String {
        re.replace_all(text, |c: &Captures| {
            let m = &c[0];
            let (core, tail) = if trim {
                split_trailing_punct(m)
            } else {
                (m, "")
            };
            if core.is_empty() || is_placeholder(core) {
                return m.to_string();
            }
            format!("{}{tail}", self.token(kind, core))
        })
        .into_owned()
    }

    /// #111: branch names in merge subjects → `BRANCH_n`; public names
    /// (`main`) are kept.
    fn merge_refs(&mut self, line: &str) -> String {
        let p = self.p;
        let branch = |obf: &mut Self, name: &str| -> String {
            let (core, tail) = split_trailing_punct(name);
            if core.is_empty() || is_placeholder(core) || is_public_branch(core) {
                return name.to_string();
            }
            format!("{}{tail}", obf.token(TokenKind::Branch, core))
        };
        let mut s = line.to_string();
        if p.merge_branch.is_match(&s) {
            s = p
                .merge_of
                .replace_all(&s, |c: &Captures| {
                    format!("{}{}", &c[1], self.token(TokenKind::Url, &c[2]))
                })
                .into_owned();
            s = p
                .quoted
                .replace_all(&s, |c: &Captures| {
                    format!("{}{}{}", &c[1], branch(self, &c[2]), &c[3])
                })
                .into_owned();
        }
        for re in [&p.merge_pr, &p.merged_in] {
            s = re
                .replace_all(&s, |c: &Captures| {
                    format!("{}{}", &c[1], branch(self, &c[2]))
                })
                .into_owned();
        }
        p.merge_request
            .replace_all(&s, |c: &Captures| {
                format!("{}{}{}", &c[1], self.token(TokenKind::Repo, &c[2]), &c[3])
            })
            .into_owned()
    }

    fn ipv6(&mut self, text: &str) -> String {
        self.p
            .ipv6
            .replace_all(text, |c: &Captures| {
                let end = c.get(0).map_or(0, |m| m.end());
                if is_ipv6(&c[2], text[end..].chars().next()) {
                    format!("{}{}", &c[1], self.token(TokenKind::Host, &c[2]))
                } else {
                    c[0].to_string()
                }
            })
            .into_owned()
    }

    /// `PATH_n` for a file name, keeping a known extension (`PATH_1.md`).
    fn file_token(&mut self, original: &str) -> String {
        let t = self.token(TokenKind::Path, original);
        match known_ext(original.rsplit(['/', '\\']).next().unwrap_or("")) {
            Some(ext) => format!("{t}.{ext}"),
            None => t,
        }
    }

    /// Slash paths → `PATH_n[.ext]`, whole; #111 (gate B re-run): no
    /// directory or basename stays visible.
    fn paths(&mut self, text: &str) -> String {
        self.p
            .slash
            .replace_all(text, |c: &Captures| {
                let m = &c[0];
                let (core, tail) = split_trailing_punct(m);
                if core.is_empty() || is_placeholder(core) || !is_path(core) {
                    return m.to_string();
                }
                format!("{}{tail}", self.file_token(core))
            })
            .into_owned()
    }

    /// Hosts, domains, IPv4 addresses and bare file names.
    fn dotted(&mut self, text: &str) -> String {
        self.p
            .dotted
            .replace_all(text, |c: &Captures| {
                let m = &c[0];
                let end = c.get(0).map_or(0, |x| x.end());
                match classify_dotted(m, text[end..].chars().next()) {
                    Dotted::Keep => m.to_string(),
                    Dotted::Host => self.token(TokenKind::Host, m),
                    Dotted::File(ext) => {
                        format!("{}.{ext}", self.token(TokenKind::Path, m))
                    }
                    Dotted::FileName => self.token(TokenKind::Path, m),
                }
            })
            .into_owned()
    }

    /// Ticket keys → `TICKET_n`; see [`ticket_start`].
    fn tickets(&mut self, text: &str) -> String {
        let hits: Vec<(usize, usize)> = self
            .p
            .ticket
            .captures_iter(text)
            .filter_map(|c| {
                let m = c.get(0)?;
                let common = |w: &str| self.names.is_common(w);
                ticket_start(text, m.start(), m.end(), &c[1], &common)
                    .map(|off| (m.start() + off, m.end()))
            })
            .collect();
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        for (s, e) in hits {
            out.push_str(&text[last..s]);
            out.push_str(&self.token(TokenKind::Ticket, &text[s..e]));
            last = e;
        }
        out.push_str(&text[last..]);
        out
    }

    /// Record ids (`H1234`, `AB12345`) → `ID_n` (gate B).
    fn record_ids(&mut self, text: &str) -> String {
        self.p
            .id
            .replace_all(text, |c: &Captures| {
                if is_record_id(&c[1]) {
                    self.token(TokenKind::Id, &c[0])
                } else {
                    c[0].to_string()
                }
            })
            .into_owned()
    }

    /// Configured, database and trailer names, longest first with fallback.
    fn names(&mut self, text: &str) -> String {
        let Some(m) = self.matcher.take() else {
            return text.to_string();
        };
        let out = m.apply(text, |kind, original| self.token(kind, original));
        self.matcher = Some(m);
        out
    }

    fn mentions(&mut self, text: &str) -> String {
        self.p
            .mention
            .replace_all(text, |c: &Captures| {
                if is_placeholder(&c[2]) {
                    return c[0].to_string();
                }
                format!("{}@{}", &c[1], self.token(TokenKind::Person, &c[2]))
            })
            .into_owned()
    }
}

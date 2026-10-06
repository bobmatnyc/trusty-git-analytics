//! Ticket-key and record-id rules for the Jev pseudonymizer (#111).
//!
//! Why: issue keys leak project names and customer work in every case and
//! shape (`ABC-12`, `abc-1`, `[abc-1]`, `abc-1_fix`, `build_abc-7`), while
//! prose is full of look-alikes (`utf-8`, `python-3`, `step-2`). Split out
//! of `jev_patterns.rs` for the size cap.
//! What: [`ticket_start`] decides whether a `ticket` regex match is a key,
//! and where it starts; [`is_record_id`] decides an `id` match and
//! [`is_hex_id`] a `hex` match.
//! Test: `classify::tiers::jev_obfuscate_tests::lowercase_ticket_rule`,
//! `jev_gateb_tests::lowercase_ticket_keys_any_digits`.

use regex::{Captures, Regex};

/// `text` with each `re` match that `is_id` accepts replaced by `token`.
///
/// What: the record-id and hash passes of the pseudonymizer; the record-id
/// rule is [`is_record_id`], the hash rule [`is_hex_id`].
/// Test: `jev_obfuscate_tests::record_ids_become_id`,
/// `jev_round5_tests::a_full_sha_in_a_version_string_becomes_id`.
pub(super) fn replace_ids(
    text: &str,
    re: &Regex,
    is_id: impl Fn(&Captures) -> bool,
    mut token: impl FnMut(&str) -> String,
) -> String {
    re.replace_all(text, |c: &Captures| {
        if is_id(c) {
            token(&c[0])
        } else {
            c[0].to_string()
        }
    })
    .into_owned()
}

/// Standard names shaped like issue keys (`UTF-8`, `SHA-256`); kept.
const NOT_TICKETS: &[&str] = &[
    "UTF", "UCS", "SHA", "MD", "ISO", "RFC", "CVE", "CWE", "GHSA", "HTTP", "TLS", "SSL", "AES",
    "RSA", "ECDSA", "IPV", "X", "PEP", "ES", "ECMA", "IEEE", "WCAG", "PKCS", "ANSI", "BASE",
];

/// Lowercase prefixes that make `word-12` a version or an ordinal, not a
/// ticket (`python-3`, `step-2`, `gpt-4`); kept (#111).
const TICKET_STOP_WORDS: &[&str] = &[
    "alpine",
    "android",
    "angular",
    "arm",
    "attempt",
    "batch",
    "case",
    "centos",
    "chapter",
    "chrome",
    "claude",
    "clang",
    "covid",
    "day",
    "debian",
    "django",
    "dotnet",
    "ecma",
    "fedora",
    "firefox",
    "gcc",
    "gemini",
    "gpt",
    "grade",
    "ios",
    "item",
    "iteration",
    "java",
    "jdk",
    "jre",
    "k8s",
    "level",
    "line",
    "llama",
    "llvm",
    "log4j",
    "macos",
    "month",
    "mysql",
    "next",
    "node",
    "npm",
    "option",
    "opus",
    "page",
    "part",
    "pg",
    "phase",
    "php",
    "postgres",
    "python",
    "rails",
    "rc",
    "react",
    "redis",
    "rev",
    "rhel",
    "round",
    "ruby",
    "run",
    "rust",
    "safari",
    "section",
    "sonnet",
    "spring",
    "sprint",
    "stage",
    "step",
    "table",
    "tier",
    "top",
    "try",
    "ubuntu",
    "version",
    "vue",
    "wave",
    "week",
    "win",
    "windows",
    "year",
];

/// Whether an `id` match is a record id: its letter prefix is not a
/// standard name (`RFC3339`, `ISO8601`, `ES2015`).
/// Test: `jev_obfuscate_tests::record_ids_become_id`.
pub(super) fn is_record_id(prefix: &str) -> bool {
    !NOT_TICKETS.iter().any(|n| n.eq_ignore_ascii_case(prefix))
}

/// Whether the `hex` match `text[start..end]` is a commit hash, content
/// hash or UUID (#111, gate B 2).
///
/// Why: a hash names a real commit or artefact; gate B 2 found full commit
/// hashes inside version strings in two payload bodies.
/// What: the match must not continue a word: the character before it is
/// none, not a letter or digit, or a `g` that is itself not after one
/// (`git describe` output: `v1.2-3-g3f9c2a7`); the character after it is
/// none or not a letter or digit. Then a UUID, any run of 32 or more hex
/// characters (a 64-character hash, a 40-digit number), or a run of 7 to
/// 31 that holds both a digit and a letter (`3f9c2a7`) is an id. A
/// shorter run, a pure-letter run (`decade`, `facade`, `deadbeef`) and a
/// pure-digit run under 32 (`20261006`, `1234567`) are not.
/// Test: `jev_round5_tests::an_abbreviated_sha_after_g_and_other_joiners_becomes_id`,
/// `jev_round5_tests::hex_letter_words_and_plain_numbers_survive`,
/// `jev_round5_tests::a_64_character_hash_becomes_id`.
pub(super) fn is_hex_id(text: &str, start: usize, end: usize) -> bool {
    let word = |c: Option<char>| c.is_some_and(char::is_alphanumeric);
    let mut before = text[..start].chars().rev();
    let free_start = match before.next() {
        None => true,
        Some('g' | 'G') => !word(before.next()),
        Some(c) => !c.is_alphanumeric(),
    };
    if !free_start || word(text[end..].chars().next()) {
        return false;
    }
    let run = &text[start..end];
    let digit = run.bytes().any(|b| b.is_ascii_digit());
    let letter = run.bytes().any(|b| b.is_ascii_alphabetic());
    run.contains('-') || run.len() >= 32 || (digit && letter)
}

/// Whether `prefix-…` is a key, given the character `before` it and the
/// text `after` its number (#111).
///
/// What: a prefix on [`NOT_TICKETS`] (any case), or a key run on into a
/// letter or digit (`ABC-12x`), is never a key; one followed by `_`
/// (`abc-1_fix`) can be. An all-uppercase prefix is. A bracketed key
/// (`[abc-1]`) is when its prefix is letters. A prefix with a lowercase
/// letter is a key only when it is 2–10 letters, not on
/// [`TICKET_STOP_WORDS`], not classification vocabulary (`common`: `fix-1`,
/// `qa-2` stay), does not continue a hyphenated, dotted or snake word
/// (`release-branch-2`), and its number is not a version (`python-3.11`).
/// Any number of digits counts (gate B: `abc-1`, `proj-7`).
fn key(prefix: &str, before: Option<char>, after: &str, common: &dyn Fn(&str) -> bool) -> bool {
    let mut rest = after.chars();
    let first = rest.next();
    if NOT_TICKETS.iter().any(|n| n.eq_ignore_ascii_case(prefix))
        || first.is_some_and(char::is_alphanumeric)
    {
        return false;
    }
    if !prefix.bytes().any(|b| b.is_ascii_lowercase()) {
        return true;
    }
    let letters = prefix.bytes().all(|b| b.is_ascii_alphabetic());
    if before == Some('[') && first == Some(']') {
        return letters;
    }
    let version = first == Some('.') && rest.next().is_some_and(|c| c.is_ascii_digit());
    let lower = prefix.to_ascii_lowercase();
    (2..=10).contains(&prefix.len())
        && letters
        && !TICKET_STOP_WORDS.contains(&lower.as_str())
        && !common(prefix)
        && !matches!(before, Some('-' | '.' | '_'))
        && !version
}

/// Where the ticket key in the `ticket` match `start..end` of `text` (its
/// prefix group `prefix`) starts, relative to the match; `None` when the
/// match holds no key.
///
/// What: the whole match per [`key`]; failing that, the part after the last
/// `_` of the prefix (`build_ABC-12` → `ABC-12`, `build_abc-7` → `abc-7`),
/// judged as if the `_` were a word boundary.
/// Test: `jev_obfuscate_tests::lowercase_ticket_rule`,
/// `jev_redaction_tests::ticket_keys_any_case_and_bracketed`,
/// `jev_gateb_tests::lowercase_ticket_keys_any_digits`.
pub(super) fn ticket_start(
    text: &str,
    start: usize,
    end: usize,
    prefix: &str,
    common: &dyn Fn(&str) -> bool,
) -> Option<usize> {
    let before = text[..start].chars().next_back();
    let after = &text[end..];
    if key(prefix, before, after, common) {
        return Some(0);
    }
    let (head, tail) = prefix.rsplit_once('_')?;
    let off = head.len() + 1;
    (tail.len() >= 2 && key(tail, None, after, common)).then_some(off)
}

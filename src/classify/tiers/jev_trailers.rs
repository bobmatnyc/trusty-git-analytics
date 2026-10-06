//! Identity-trailer parsing for the Jev pseudonymizer (#111).
//!
//! Why: git and Phabricator trailers name people verbatim, and a stress run
//! over real bodies leaked Phabricator usernames. The trailer shapes seen in
//! history are wider than `Token: Name <email>`: a squash body indents or
//! bullets them (`* Reviewers: jdoe`), git folds long values onto indented
//! continuation lines, Phabricator lists usernames separated by spaces and
//! marks blocking reviewers with `!`, and a value can read
//! `jdoe (Jane Doe)`.
//! What: [`scan`] classifies each line of a message as a trailer, a
//! continuation of the trailer above, or text; [`items`] and
//! [`item_names`] take a trailer value apart into the names to hide.
//! [`super::jev_obfuscate`] owns the replacement.
//! Test: `classify::tiers::jev_redaction_tests`.

use super::jev_patterns::Patterns;
use crate::eval::redact::is_identity_trailer;

/// Phabricator trailer names whose values are usernames or project tags,
/// separated by commas or spaces (gate B, #111).
const PHABRICATOR_USERNAME_TRAILERS: &[&str] =
    &["Reviewers", "Reviewed By", "Subscribers", "Auditors"];

/// Role words that make a one- or two-word trailer token name people, in
/// the singular or with a plural `s` (`Reviewer:`, `Code Owners:`); a plural
/// value is a username list (#111).
const ROLE_WORDS: &[&str] = &["reviewer", "author", "owner", "assignee", "approver"];

/// Last words that make a trailer token name a person: `Approved by`,
/// `Paired-with`, `Thanks-to` (#111).
const LINK_WORDS: &[&str] = &["by", "with", "to"];

/// `(identity, username list, weak)` for a trailer token, from its words
/// (split on whitespace and `-`, compared case-insensitively). A weak token
/// (`-with`, `-to`, a role word) also heads ordinary prose (`Tested with:`,
/// `Moved to:`), so its values teach the run only name- or login-shaped
/// names (see [`learnable`]).
fn token_role(token: &str) -> (bool, bool, bool) {
    let words: Vec<String> = token
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty()
        || words.len() > 4
        || !words
            .iter()
            .all(|w| w.bytes().all(|b| b.is_ascii_alphabetic()))
    {
        return (false, false, false);
    }
    let last = words[words.len() - 1].as_str();
    if words.len() >= 2 && LINK_WORDS.contains(&last) {
        return (true, false, last != "by");
    }
    let singular = last.strip_suffix('s').unwrap_or(last);
    if words.len() <= 2 && ROLE_WORDS.contains(&singular) {
        return (true, singular != last, true);
    }
    (false, false, false)
}

/// Whether a weak trailer's item names someone (#111, review round 3).
///
/// What: name-shaped — one to four words, each starting with an uppercase
/// letter and made of letters, `'`, `’` or `-` (`Jane Roe`, `O'Brien`) — or
/// login-shaped — one token of letters, digits, `.`, `_` and `-` carrying a
/// digit or one of those separators (`jroe-acme`, `q.voss`, `bob_s`).
/// `chrome and firefox`, `bug fix` and `new API` are neither.
/// Test: `jev_round3_tests::category_text_survives_learning`.
fn learnable(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    let name_word = |w: &&str| {
        w.starts_with(|c: char| c.is_uppercase())
            && w.chars()
                .all(|c| c.is_alphabetic() || matches!(c, '\'' | '\u{2019}' | '-'))
    };
    let named = (1..=4).contains(&words.len()) && words.iter().all(name_word);
    let login = words.len() == 1
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && name
            .chars()
            .any(|c| c.is_ascii_digit() || matches!(c, '.' | '_' | '-'));
    named || login
}

/// One `Token: value` identity trailer line, split.
pub(super) struct Trailer<'a> {
    /// Leading whitespace and list or quote markers (`  * `, `> `).
    pub(super) prefix: &'a str,
    /// The token as written, without the colon.
    pub(super) token: &'a str,
    /// Everything after the first colon.
    pub(super) value: &'a str,
    /// Phabricator username list: items split on whitespace too.
    pub(super) usernames: bool,
    /// Weak token: values teach the run only [`learnable`] names.
    pub(super) weak: bool,
}

/// What one line of a message is.
pub(super) enum Role<'a> {
    /// An identity trailer.
    Trailer(Trailer<'a>),
    /// A folded continuation of the identity trailer above it: more
    /// indented, non-blank, and not itself `Token: value`.
    Continuation {
        /// The line's leading whitespace.
        indent: &'a str,
        /// The rest of the line.
        value: &'a str,
        /// As [`Trailer::usernames`] of the trailer it continues.
        usernames: bool,
        /// As [`Trailer::weak`] of the trailer it continues.
        weak: bool,
    },
    /// Anything else.
    Text,
}

/// Length of the leading whitespace and list/quote markers of `line`.
fn prefix_len(line: &str) -> usize {
    let mut rest = line;
    loop {
        let trimmed = rest.trim_start();
        let mut chars = trimmed.chars();
        let consumed = match chars.next() {
            Some('>') => Some(1),
            Some(c @ ('*' | '-' | '+' | '•')) if chars.next().is_some_and(char::is_whitespace) => {
                Some(c.len_utf8())
            }
            _ => None,
        };
        match consumed {
            Some(n) => rest = &trimmed[n..],
            None => return line.len() - trimmed.len(),
        }
    }
}

fn indent_len(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// Whether `s` reads `Token: value` with a one-word token.
fn trailer_shaped(s: &str) -> bool {
    s.split_once(':').is_some_and(|(t, v)| {
        !t.is_empty()
            && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && v.starts_with(char::is_whitespace)
    })
}

/// Split `line` when it is an identity trailer (#111).
///
/// What: after the prefix, the line is `Token: value` and either the token
/// is one of [`is_identity_trailer`]'s, a Phabricator username trailer
/// (any case and spacing), a token whose last word (split on whitespace and
/// `-`, any case) is `by`, `with` or `to` (`Approved by`, `Paired-with`,
/// `Thanks-to`), a [`ROLE_WORDS`] token singular or plural (`Reviewer`,
/// `Owners`), or a word (letters, digits, `-`) whose value holds an e-mail
/// address after a space. The value need not hold an address. The line's
/// value is always replaced; only a strong trailer teaches every name.
/// Test: `jev_review_tests::trailer_tokens_any_case_and_spacing`.
pub(super) fn parse<'a>(p: &Patterns, line: &'a str) -> Option<Trailer<'a>> {
    let cut = prefix_len(line);
    let rest = &line[cut..];
    let (token, value) = rest.split_once(':')?;
    let words = token.split_whitespace().collect::<Vec<_>>().join(" ");
    let is = |list: &[&str]| list.iter().any(|t| t.eq_ignore_ascii_case(&words));
    let (role, plural, weak_role) = token_role(token);
    let phabricator = is(PHABRICATOR_USERNAME_TRAILERS);
    let usernames = phabricator || plural;
    let t = token.trim_end();
    let shaped = t.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let spaced = value.starts_with(char::is_whitespace);
    let listed = is_identity_trailer(rest) || phabricator;
    let identity = listed || usernames || role || (shaped && spaced && p.email.is_match(value));
    identity.then_some(Trailer {
        prefix: &line[..cut],
        token,
        value,
        usernames,
        weak: weak_role && !listed,
    })
}

/// The role of each `\n`-separated line of `text`, in order.
/// Test: `jev_redaction_tests::folded_and_bulleted_trailers`,
/// `jev_review_tests::folded_username_list`.
pub(super) fn scan<'a>(p: &Patterns, text: &'a str) -> Vec<Role<'a>> {
    // (trailer indent, username list, value ends in a list separator, weak)
    let mut open: Option<(usize, bool, bool, bool)> = None;
    let continues = |v: &str| v.trim_end().ends_with([',', ';']);
    text.split('\n')
        .map(|line| {
            if let Some(t) = parse(p, line) {
                open = Some((indent_len(line), t.usernames, continues(t.value), t.weak));
                return Role::Trailer(t);
            }
            let lead = indent_len(line);
            let body = &line[lead..];
            match open {
                // A fold continues a username list, follows a trailing
                // `,`/`;`, or carries an address.
                Some((ind, usernames, more, weak))
                    if lead > ind
                        && !body.trim().is_empty()
                        && !trailer_shaped(body)
                        && (usernames || more || p.email.is_match(body)) =>
                {
                    open = Some((ind, usernames, continues(body), weak));
                    Role::Continuation {
                        indent: &line[..lead],
                        value: body,
                        usernames,
                        weak,
                    }
                }
                _ => {
                    open = None;
                    Role::Text
                }
            }
        })
        .collect()
}

/// The items of a trailer value: comma- or semicolon-separated, and for a
/// username list whitespace-separated too.
pub(super) fn items(value: &str, usernames: bool) -> Vec<&str> {
    value
        .split(|c: char| c == ',' || c == ';' || (usernames && c.is_whitespace()))
        .filter(|i| !i.trim().is_empty())
        .collect()
}

/// `name` without `(…)` and `[…]` parts (`Claude (1M context)` → `Claude`).
pub(super) fn strip_brackets(name: &str) -> String {
    bracket_split(name).0
}

/// `(outside, [inside…])`: the text outside `(…)`/`[…]` and each bracketed
/// part.
fn bracket_split(s: &str) -> (String, Vec<String>) {
    let mut depth = 0_usize;
    let (mut outside, mut inside, mut cur) = (String::new(), Vec::new(), String::new());
    for c in s.chars() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 && !cur.is_empty() {
                    inside.push(std::mem::take(&mut cur));
                }
            }
            _ if depth == 0 => outside.push(c),
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        inside.push(cur);
    }
    (outside, inside)
}

/// The names in one trailer item (#111).
///
/// What: the item without addresses and `<>`; the text outside brackets and
/// each bracketed part are separate names (`jdoe (Jane Doe)` → `jdoe`,
/// `Jane Doe`), each trimmed of edge punctuation (`#tag`, a blocking
/// reviewer's `jdoe!`). A part of more than four words is prose, not a name.
/// Test: `jev_redaction_tests::phabricator_username_shapes`.
pub(super) fn item_names(p: &Patterns, item: &str) -> Vec<String> {
    let bare = p.email.replace_all(item, "").replace(['<', '>'], "");
    let (outside, inside) = bracket_split(&bare);
    std::iter::once(outside)
        .chain(inside)
        .map(|n| {
            n.trim()
                .trim_matches(|c: char| !c.is_alphanumeric())
                .to_string()
        })
        .filter(|n| !n.is_empty() && n.split_whitespace().count() <= 4)
        .collect()
}

/// [`names_in`] over every message of `messages`, deduplicated, for a
/// caller outside the tiers module (#111: the stored commit messages).
/// Test: `jev_round4_tests::trailer_names_from_stored_commits_are_redacted`.
///
/// # Errors
///
/// A built-in pattern does not compile.
pub(crate) fn trailer_names<'a>(
    messages: impl IntoIterator<Item = &'a str>,
) -> Result<std::collections::BTreeSet<String>, super::jev_error::JevError> {
    let p = super::jev_patterns::patterns()?;
    Ok(messages.into_iter().flat_map(|m| names_in(p, m)).collect())
}

/// Every name in `text`'s identity trailers and their continuations, plus
/// the local-part of each address in them (`cc mtolsk` in prose).
/// Test: `jev_redaction_tests::git_trailers_hide_names_and_emails`.
pub(super) fn names_in(p: &Patterns, text: &str) -> Vec<String> {
    scan(p, text)
        .into_iter()
        .filter_map(|role| match role {
            Role::Trailer(t) => Some((t.value, t.usernames, t.weak)),
            Role::Continuation {
                value,
                usernames,
                weak,
                ..
            } => Some((value, usernames, weak)),
            Role::Text => None,
        })
        .flat_map(|(value, usernames, weak)| {
            let locals = p
                .email
                .find_iter(value)
                .filter_map(|m| m.as_str().split('@').next().map(str::to_string));
            items(value, usernames)
                .into_iter()
                .flat_map(|i| item_names(p, i))
                .filter(|n| !weak || learnable(n))
                .chain(locals)
                .collect::<Vec<_>>()
        })
        .collect()
}

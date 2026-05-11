//! Ticket-reference detection for commit messages.
//!
//! A commit is considered *ticketed* if its message contains any reference
//! to an external work-tracking system. We currently recognize:
//!
//! - **JIRA / Linear style**: `PROJ-123`, `ENG-456`, `ABC-9` —
//!   uppercase project key, hyphen, digits. The Linear identifier format
//!   (`ENG-123`, `FE-456`) is a subset of this pattern.
//! - **GitHub action-keyword refs**: `fixes #123`, `closes #45`,
//!   `resolves #7` (case-insensitive, also matches `fix`/`close`/`resolve`).
//! - **GitHub bare issue refs**: `#123` preceded by start-of-string or
//!   whitespace, so we don't false-positive on things like a hex color
//!   `#abc123` inside another token.
//! - **Azure DevOps work-item refs**: `AB#123`. Bare `#N` is intentionally
//!   excluded from the ADO pattern because it collides with GitHub PR/issue
//!   numbers (the existing GitHub bare-`#N` rule above still applies).
//!
//! Patterns are compiled exactly once on first use via [`OnceLock`].

use std::sync::OnceLock;

use regex::Regex;

/// Compiled regexes used by [`is_ticketed`].
struct TicketPatterns {
    jira: Regex,
    gh_action: Regex,
    gh_bare: Regex,
    azdo: Regex,
}

/// Global, lazily-initialized pattern set.
fn patterns() -> &'static TicketPatterns {
    static PATTERNS: OnceLock<TicketPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        // SAFETY of unwrap: these literals are validated by the test
        // [`patterns_compile`] below — any regression is caught at test
        // time, not at runtime.
        TicketPatterns {
            // JIRA / Linear: uppercase letters (>=1), optional digits, '-', digits.
            // Word boundaries prevent matching inside `FOO-BAR-1`-style identifiers'
            // middle, while still catching the trailing `BAR-1` segment.
            jira: Regex::new(r"\b[A-Z][A-Z0-9]*-\d+\b").expect("jira pattern compiles"),
            // GitHub action keyword: fix(es|ed)?|close(s|d)?|resolve(s|d)?  #123
            gh_action: Regex::new(r"(?i)\b(?:fix(?:es|ed)?|close[sd]?|resolve[sd]?)\s+#\d+\b")
                .expect("gh_action pattern compiles"),
            // Bare `#123` preceded by start-of-line or whitespace.
            gh_bare: Regex::new(r"(?m)(?:^|\s)#\d+\b").expect("gh_bare pattern compiles"),
            // Azure DevOps work-item reference: AB#123.
            // Bare #N intentionally excluded — collides with GitHub PR/issue numbers.
            azdo: Regex::new(r"\bAB#\d+\b").expect("azdo pattern compiles"),
        }
    })
}

/// Return `true` if `message` contains any recognized ticket reference.
///
/// The check is performed against the full message (subject + body); a
/// reference anywhere in the text — including later lines of a multi-line
/// commit body — flags the commit as ticketed.
///
/// # Examples
///
/// ```
/// use tga::collect::ticket::is_ticketed;
///
/// assert!(is_ticketed("ENG-123: add feature"));
/// assert!(is_ticketed("Fix login (closes #42)"));
/// assert!(!is_ticketed("misc cleanup"));
/// ```
pub fn is_ticketed(message: &str) -> bool {
    let p = patterns();
    p.jira.is_match(message)
        || p.gh_action.is_match(message)
        || p.gh_bare.is_match(message)
        || p.azdo.is_match(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_compile() {
        // Force lazy init; if any pattern is malformed this will panic.
        let _ = patterns();
    }

    #[test]
    fn jira_style_is_ticketed() {
        assert!(is_ticketed("ENG-123: add feature"));
        assert!(is_ticketed("PROJ-1 initial commit"));
        assert!(is_ticketed("Backport from upstream (ABC-4567)"));
    }

    #[test]
    fn linear_style_is_ticketed() {
        // Linear identifiers are a subset of the JIRA pattern.
        assert!(is_ticketed("FE-456 fix login"));
        assert!(is_ticketed("API-9 add endpoint"));
    }

    #[test]
    fn github_action_keyword_is_ticketed() {
        assert!(is_ticketed("Fix race condition, fixes #123"));
        assert!(is_ticketed("closes #45"));
        assert!(is_ticketed("Resolves #7 by reworking auth"));
        assert!(is_ticketed("CLOSED #99")); // case-insensitive
    }

    #[test]
    fn github_bare_hash_ref_is_ticketed() {
        assert!(is_ticketed("Bug from #123 still present"));
        assert!(is_ticketed("#42 follow-up"));
    }

    #[test]
    fn plain_message_is_not_ticketed() {
        assert!(!is_ticketed("misc cleanup"));
        assert!(!is_ticketed("update README"));
        assert!(!is_ticketed("bump version to 1.2.3"));
        // Hex color shouldn't false-positive — `#abc123` is not preceded by
        // whitespace+digits-only.
        assert!(!is_ticketed("set color to #abc123"));
        // Lowercase project key is not a JIRA identifier.
        assert!(!is_ticketed("eng-123 lowercase doesn't count"));
    }

    #[test]
    fn multiline_body_with_ticket_is_ticketed() {
        let msg = "Refactor module structure\n\nMoves things around.\nRelates to PROJ-789.\n";
        assert!(is_ticketed(msg));

        let msg2 = "First line no ticket\n\nSecond paragraph mentions #321 explicitly.";
        assert!(is_ticketed(msg2));
    }

    #[test]
    fn azdo_ab_ref_is_ticketed() {
        assert!(is_ticketed("AB#1234 implement new feature"));
        assert!(is_ticketed("Refactor module (AB#42)"));
        assert!(is_ticketed("First line\n\nbody mentions AB#7 explicitly"));
    }

    #[test]
    fn bare_hash_without_ab_prefix_is_not_azdo() {
        // GitHub bare `#N` still matches via the gh_bare pattern, but it
        // must NOT match the ADO `AB#` pattern specifically.
        let p = patterns();
        assert!(!p.azdo.is_match("#1234 some work"));
        assert!(!p.azdo.is_match("fixes #99"));
        // And the existing JIRA pattern must not accidentally fire on AB#N
        // (different separator: `#` vs `-`).
        assert!(!p.jira.is_match("AB#1234"));
    }

    #[test]
    fn empty_message_is_not_ticketed() {
        assert!(!is_ticketed(""));
        assert!(!is_ticketed("\n\n"));
    }
}

//! Identity redaction for the rater sheet (`labels.csv`).
//!
//! Why: the sheet hides the author behind a salted hash, but commit bodies
//! carry git trailers (`Co-authored-by: Name <email>`) that name people
//! verbatim. Raters need the change, not the people.
//! What: [`strip_trailers`] drops identity trailer lines; [`redact_emails`]
//! replaces any remaining e-mail address with `<email>`. `sample.jsonl` keeps
//! the full text.
//! Test: `tests::strips_identity_trailers`, `tests::redacts_emails`.

/// Trailer tokens that name a person, compared case-insensitively.
const IDENTITY_TRAILERS: [&str; 8] = [
    "co-authored-by",
    "signed-off-by",
    "reviewed-by",
    "acked-by",
    "reported-by",
    "tested-by",
    "helped-by",
    "cc",
];

fn is_identity_trailer(line: &str) -> bool {
    line.trim_start().split_once(':').is_some_and(|(token, _)| {
        let token = token.trim_end();
        IDENTITY_TRAILERS
            .iter()
            .any(|t| t.eq_ignore_ascii_case(token))
    })
}

/// Drop every `Token: value` line whose token is an identity trailer.
pub(crate) fn strip_trailers(body: &str) -> String {
    body.lines()
        .filter(|l| !is_identity_trailer(l))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn local_char(c: char) -> bool {
    c.is_alphanumeric() || "._%+-".contains(c)
}

fn domain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '.' || c == '-'
}

/// Replace every `local@domain.tld` address with `<email>`.
pub(crate) fn redact_emails(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '@' {
            let mut start = i;
            while start > 0 && local_char(chars[start - 1]) {
                start -= 1;
            }
            let mut end = i + 1;
            while end < chars.len() && domain_char(chars[end]) {
                end += 1;
            }
            // Trailing dots belong to the sentence, not the domain.
            while end > i + 1 && chars[end - 1] == '.' {
                end -= 1;
            }
            let domain: String = chars[i + 1..end].iter().collect();
            if start < i && domain.contains('.') && !domain.starts_with('.') {
                // `start..i` was already copied; drop it before the marker.
                let local_len: usize = chars[start..i].iter().map(|c| c.len_utf8()).sum();
                out.truncate(out.len() - local_len);
                out.push_str("<email>");
                i = end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: trailers are the main source of names on the label sheet.
    /// What: identity trailers go wherever they sit and whatever their case;
    /// other `Token:` lines and prose stay.
    /// Test: this function.
    #[test]
    fn strips_identity_trailers() {
        let body = "Fix the parser.\n\
                    CO-AUTHORED-BY: Jane Doe <jane@example.com>\n\
                    Keeps the old path for v1.\n\
                    Refs: #12\n\
                    Signed-off-by: Joe Bloggs <joe@example.org>\n\
                    cc: team@example.net\n\
                    Reviewed-By : Ann <ann@example.com>";
        assert_eq!(
            strip_trailers(body),
            "Fix the parser.\nKeeps the old path for v1.\nRefs: #12"
        );
        assert_eq!(
            strip_trailers("Note: tested-by hand"),
            "Note: tested-by hand"
        );
    }

    /// Why: addresses also appear outside trailers (subjects, PR titles).
    /// What: each address becomes `<email>`; bare `@` mentions, `@` without a
    /// dotted domain and sentence-final dots are left alone.
    /// Test: this function.
    #[test]
    fn redacts_emails() {
        assert_eq!(
            redact_emails("mail jane.doe+x@mail.example.com, or <b@c.io>."),
            "mail <email>, or <<email>>."
        );
        assert_eq!(
            redact_emails("thanks @jane, see a@localhost"),
            "thanks @jane, see a@localhost"
        );
        assert_eq!(redact_emails("naïve ü@x.de"), "naïve <email>");
    }
}

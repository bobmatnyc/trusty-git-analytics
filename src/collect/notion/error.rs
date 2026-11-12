//! Error type for the Notion client and pipeline (#252).

use thiserror::Error;

/// Everything that can stop a Notion sync.
///
/// No variant carries the token: the client never formats it, and
/// `reqwest::Error`'s `Display` names the URL, never request headers.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum NotionError {
    /// The `notion:` block cannot produce a working client.
    #[error("Notion config: {0}")]
    Config(String),

    /// Transport failure (connect, timeout, body read).
    #[error("Notion HTTP transport: {0}")]
    Transport(#[from] reqwest::Error),

    /// A non-success status that is not retried.
    // #252: `code` / `message` come from Notion's error body so a failure
    // names its cause (e.g. `object_not_found`). Never the token or headers.
    #[error("Notion API returned HTTP {status} for {endpoint}{}", render_body(.code, .message))]
    Status {
        /// HTTP status code.
        status: u16,
        /// The endpoint path that answered.
        endpoint: String,
        /// Notion's error `code` (e.g. `object_not_found`), when the body had one.
        code: Option<String>,
        /// Notion's error `message`, truncated, when the body had one.
        message: Option<String>,
    },

    /// 429 / 529 on every one of the bounded attempts.
    #[error(
        "Notion API throttled {endpoint} (HTTP {status}) on all {attempts} attempts{}",
        render_body(.code, .message)
    )]
    Throttled {
        /// The last throttling status seen.
        status: u16,
        /// The endpoint path that answered.
        endpoint: String,
        /// How many attempts were made.
        attempts: u32,
        /// Notion's error `code` on the last attempt, when present.
        code: Option<String>,
        /// Notion's error `message` on the last attempt, when present.
        message: Option<String>,
    },

    /// A response body that does not match the expected shape.
    #[error("Notion API response from {endpoint} did not decode: {message}")]
    Decode {
        /// The endpoint path that answered.
        endpoint: String,
        /// The decoder's message.
        message: String,
    },

    /// A timestamp the API returned that is not RFC 3339.
    #[error("Notion timestamp `{0}` is not RFC 3339")]
    Timestamp(String),

    /// A pagination response that claims more results with no cursor.
    #[error("Notion API reported has_more with no next_cursor on {0}")]
    Pagination(String),

    /// A pagination response that repeats the cursor just requested (#252).
    #[error(
        "Notion API returned the same next_cursor twice on {endpoint}; stopping to avoid a loop"
    )]
    CursorRepeated {
        /// The endpoint path that answered.
        endpoint: String,
    },

    /// SQLite read or write failure.
    #[error("Notion store: {0}")]
    Db(#[from] rusqlite::Error),
}

impl NotionError {
    /// Whether this is a per-object access answer (403 / 404), which the
    /// comments walk skips instead of failing the sync (#252).
    pub fn is_access_denied(&self) -> bool {
        matches!(
            self,
            Self::Status {
                status: 403 | 404,
                ..
            }
        )
    }
}

/// `": <code>: <message>"` from a Notion error body, or `""` without one.
fn render_body(code: &Option<String>, message: &Option<String>) -> String {
    match (code, message) {
        (Some(c), Some(m)) => format!(": {c}: {m}"),
        (Some(c), None) => format!(": {c}"),
        (None, Some(m)) => format!(": {m}"),
        (None, None) => String::new(),
    }
}

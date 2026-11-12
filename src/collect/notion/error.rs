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
    #[error("Notion API returned HTTP {status} for {endpoint}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// The endpoint path that answered.
        endpoint: String,
    },

    /// 429 / 529 on every one of the bounded attempts.
    #[error("Notion API throttled {endpoint} (HTTP {status}) on all {attempts} attempts")]
    Throttled {
        /// The last throttling status seen.
        status: u16,
        /// The endpoint path that answered.
        endpoint: String,
        /// How many attempts were made.
        attempts: u32,
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

    /// SQLite read or write failure.
    #[error("Notion store: {0}")]
    Db(#[from] rusqlite::Error),
}

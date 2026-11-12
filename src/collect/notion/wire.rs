//! Notion API response shapes the client reads (#252).
//!
//! Only the fields the v1 events need are declared; serde ignores the rest.
//! Comment bodies are read only to count their characters.

use serde::Deserialize;

/// One page of a paginated Notion list (`/v1/search`, `/v1/comments`,
/// `/v1/users`).
#[derive(Debug, Deserialize)]
pub(crate) struct ListPage<T> {
    /// The items on this page.
    #[serde(default = "Vec::new")]
    pub results: Vec<T>,
    /// Cursor for the next page; `None` on the last one.
    #[serde(default)]
    pub next_cursor: Option<String>,
    /// Whether another page follows.
    #[serde(default)]
    pub has_more: bool,
}

/// A `{ "object": "user", "id": … }` reference.
#[derive(Debug, Clone, Deserialize)]
pub struct UserRef {
    /// Notion user id.
    pub id: String,
}

/// The page fields `/v1/search` returns that the events need.
#[derive(Debug, Clone, Deserialize)]
pub struct PageMeta {
    /// `"page"` for a page; search results can also hold databases.
    #[serde(default)]
    pub object: String,
    /// Notion page id.
    pub id: String,
    /// RFC 3339 creation time.
    pub created_time: String,
    /// RFC 3339 last edit time (Notion rounds it to the minute).
    pub last_edited_time: String,
    /// Who created the page.
    #[serde(default)]
    pub created_by: Option<UserRef>,
    /// Who edited the page last.
    #[serde(default)]
    pub last_edited_by: Option<UserRef>,
}

/// One rich-text run of a comment body.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct RichText {
    /// The run's text. Read only to count characters.
    #[serde(default)]
    pub plain_text: String,
}

/// The comment fields `/v1/comments` returns, before the body is reduced to
/// its length.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CommentWire {
    /// Notion comment id.
    pub id: String,
    /// RFC 3339 creation time.
    pub created_time: String,
    /// Who wrote the comment.
    #[serde(default)]
    pub created_by: Option<UserRef>,
    /// The body. Dropped after [`CommentWire::into_meta`].
    #[serde(default)]
    pub rich_text: Vec<RichText>,
}

impl CommentWire {
    /// Reduce the comment to its metadata; the body becomes a length.
    pub(crate) fn into_meta(self, page_id: &str) -> CommentMeta {
        let body_len = self
            .rich_text
            .iter()
            .map(|r| r.plain_text.chars().count())
            .sum();
        CommentMeta {
            id: self.id,
            page_id: page_id.to_string(),
            created_time: self.created_time,
            created_by: self.created_by,
            body_len,
        }
    }
}

/// A comment with its body reduced to a character count.
#[derive(Debug, Clone)]
pub struct CommentMeta {
    /// Notion comment id.
    pub id: String,
    /// The page the comment was listed under.
    pub page_id: String,
    /// RFC 3339 creation time.
    pub created_time: String,
    /// Who wrote the comment.
    pub created_by: Option<UserRef>,
    /// Body length in characters.
    pub body_len: usize,
}

/// The `person` block of a Notion user.
#[derive(Debug, Clone, Deserialize)]
pub struct PersonWire {
    /// The person's email. Absent when the integration lacks the
    /// "read user information including email addresses" capability.
    #[serde(default)]
    pub email: Option<String>,
}

/// A Notion user from `/v1/users`.
#[derive(Debug, Clone, Deserialize)]
pub struct NotionUser {
    /// Notion user id.
    pub id: String,
    /// `"person"` or `"bot"`.
    #[serde(rename = "type", default)]
    pub kind: Option<String>,
    /// Display name.
    #[serde(default)]
    pub name: Option<String>,
    /// Present for `type = "person"`.
    #[serde(default)]
    pub person: Option<PersonWire>,
}

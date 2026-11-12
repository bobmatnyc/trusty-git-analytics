//! The opt-in `notion:` config block (#252).
//!
//! Why: Notion activity (pages created, pages edited, comments) is an
//! engineering-activity signal beside git, PRs and tickets. It is read only
//! when the block is present, so a config without it makes no Notion request.
//! What: [`NotionConfig`], deserialized from the `notion:` YAML key. `Debug`
//! and `Serialize` are hand-written in `credential_debug` and
//! `credential_serialize` so the token never renders.
//! Test: `debug_never_renders_the_notion_token`,
//! `serialize_never_renders_the_notion_token`, `notion_block_parses_from_yaml`.

use serde::Deserialize;

/// The workspace label used when `notion.workspace_id` is unset.
pub const NOTION_DEFAULT_WORKSPACE: &str = "default";

/// Default overlap window, in minutes, re-read below the stored watermark.
pub const NOTION_DEFAULT_LOOKBACK_MINUTES: u32 = 60;

/// Notion integration settings (`notion:` in YAML, #252).
///
/// The block is opt-in: `Config::notion` is `None` unless the YAML names it,
/// and `tga collect` reads Notion only when it is set.
// #252: `Debug` and `Serialize` are hand-written (credential_debug /
// credential_serialize) — a derived one would print `token` in the clear.
#[derive(Clone, Default, Deserialize)]
#[non_exhaustive]
pub struct NotionConfig {
    /// Notion internal-integration token.
    ///
    /// Supports `${NOTION_TOKEN}` env-var substitution, which is the
    /// documented form: keep the token out of the YAML file.
    #[serde(default)]
    pub token: Option<String>,

    /// Label for the Notion workspace the token reads. It keys the
    /// `notion_sync_cursor` row and fills `fact_notion_activity.workspace_id`.
    /// Unset: [`NOTION_DEFAULT_WORKSPACE`].
    #[serde(default)]
    pub workspace_id: Option<String>,

    /// Minutes below the stored watermark each sync re-reads (#252). Notion
    /// search is indexed asynchronously, so a page edited just before the
    /// watermark can appear after the sync that set it. Unset:
    /// [`NOTION_DEFAULT_LOOKBACK_MINUTES`].
    #[serde(default)]
    pub lookback_minutes: Option<u32>,
}

impl NotionConfig {
    /// The workspace label rows and the cursor are stored under.
    pub fn workspace_label(&self) -> &str {
        self.workspace_id
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(NOTION_DEFAULT_WORKSPACE)
    }

    /// The overlap window in minutes.
    pub fn lookback_minutes(&self) -> u32 {
        self.lookback_minutes
            .unwrap_or(NOTION_DEFAULT_LOOKBACK_MINUTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::Config;

    #[test]
    fn notion_block_parses_from_yaml() {
        let cfg: Config =
            serde_yaml::from_str("notion:\n  token: \"${NOTION_TOKEN}\"\n  workspace_id: acme\n")
                .expect("parse");
        let notion = cfg.notion.expect("block present");
        assert_eq!(notion.token.as_deref(), Some("${NOTION_TOKEN}"));
        assert_eq!(notion.workspace_label(), "acme");
        assert_eq!(notion.lookback_minutes(), NOTION_DEFAULT_LOOKBACK_MINUTES);

        let absent: Config = serde_yaml::from_str("repositories: []\n").expect("parse");
        assert!(absent.notion.is_none(), "the block must stay opt-in");
    }

    #[test]
    fn workspace_label_defaults_when_unset_or_blank() {
        assert_eq!(
            NotionConfig::default().workspace_label(),
            NOTION_DEFAULT_WORKSPACE
        );
        let blank = NotionConfig {
            workspace_id: Some("  ".into()),
            ..NotionConfig::default()
        };
        assert_eq!(blank.workspace_label(), NOTION_DEFAULT_WORKSPACE);
    }
}

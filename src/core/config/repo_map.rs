//! `classification.repo_map`: how `classification.repo_categories` applies (#167).
//!
//! Why (owner ruling 2026-10-07): "The repo map should be the floor, with qa
//! and bugfix etc inferred." The #158 hard override stays available, and
//! stays the default, so a config without this block behaves as 10.2.0 did.
//! What: [`RepoMapConfig`] holds the mode, the exception categories and the
//! confidence floor for an exception. The values are applied by
//! `classify::pipeline_repo_map`.
//! Test: `tests` below, `classify::pipeline_repo_map_floor_tests`.

use serde::{Deserialize, Serialize};

/// How a mapped repository's category combines with the cascade (#167).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RepoMapMode {
    /// #158: the mapped category replaces every tier's verdict, and a mapped
    /// commit never reaches the LLM.
    #[default]
    Override,
    /// The mapped category is the default. The cascade still runs, LLM
    /// included; its verdict survives only when its category is one of
    /// [`RepoMapConfig::exceptions`] and its confidence is at or above
    /// [`RepoMapConfig::min_confidence`].
    Floor,
}

/// The `classification.repo_map` block (#167).
///
/// Example (YAML):
/// ```yaml
/// classification:
///   repo_map:
///     mode: floor
///     exceptions: [qa, security, devops, bug_fix]
///     min_confidence: 0.8
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RepoMapConfig {
    /// `override` (default) or `floor`.
    #[serde(default)]
    pub mode: RepoMapMode,
    /// Categories a cascade verdict may keep in `floor` mode, matched
    /// case-insensitively. Default `[qa, security, devops, bug_fix]`.
    #[serde(default = "default_exceptions")]
    pub exceptions: Vec<String>,
    /// Lowest confidence at which an exception verdict is kept, in
    /// `[0.0, 1.0]`. Default `0.8`.
    #[serde(default = "default_min_confidence")]
    pub min_confidence: f64,
}

/// The owner-confirmed default exceptions (#167).
pub const REPO_MAP_DEFAULT_EXCEPTIONS: [&str; 4] = ["qa", "security", "devops", "bug_fix"];

/// The owner-confirmed default exception confidence (#167).
pub const REPO_MAP_DEFAULT_MIN_CONFIDENCE: f64 = 0.8;

fn default_exceptions() -> Vec<String> {
    REPO_MAP_DEFAULT_EXCEPTIONS
        .iter()
        .map(|s| s.to_string())
        .collect()
}

fn default_min_confidence() -> f64 {
    REPO_MAP_DEFAULT_MIN_CONFIDENCE
}

impl Default for RepoMapConfig {
    fn default() -> Self {
        Self {
            mode: RepoMapMode::default(),
            exceptions: default_exceptions(),
            min_confidence: default_min_confidence(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::ClassificationConfig;

    fn parse(yaml: &str) -> Result<ClassificationConfig, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Why (#167): a config without the block keeps the 10.2.0 hard
    /// override; an empty block takes every default.
    /// What: the absent block, an empty block and a full block.
    /// Test: this test.
    #[test]
    fn the_block_defaults_to_override_and_parses_every_field() {
        let absent = parse("repo_categories: {}\n").expect("absent");
        assert_eq!(absent.repo_map, RepoMapConfig::default());
        assert_eq!(absent.repo_map.mode, RepoMapMode::Override);
        assert_eq!(absent.repo_map.exceptions, REPO_MAP_DEFAULT_EXCEPTIONS);
        assert!((absent.repo_map.min_confidence - 0.8).abs() < 1e-12);

        let empty = parse("repo_map: {}\n").expect("empty");
        assert_eq!(empty.repo_map, RepoMapConfig::default());

        let full = parse("repo_map:\n  mode: floor\n  exceptions: [qa]\n  min_confidence: 0.9\n")
            .expect("full");
        assert_eq!(full.repo_map.mode, RepoMapMode::Floor);
        assert_eq!(full.repo_map.exceptions, ["qa"]);
        assert!((full.repo_map.min_confidence - 0.9).abs() < 1e-12);
    }

    /// Why (#167): the block follows the section's `deny_unknown_fields`
    /// convention, so a typo fails the load instead of being ignored.
    /// What: an unknown key and an unknown mode both fail.
    /// Test: this test.
    #[test]
    fn unknown_keys_and_modes_fail_the_load() {
        let err = parse("repo_map:\n  mode: floor\n  min_confidance: 0.9\n")
            .expect_err("unknown key")
            .to_string();
        assert!(err.contains("min_confidance"), "{err}");
        let err = parse("repo_map:\n  mode: soft\n")
            .expect_err("unknown mode")
            .to_string();
        assert!(err.contains("soft"), "{err}");
    }
}

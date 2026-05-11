//! Configuration types deserialized from YAML.
//!
//! The full configuration schema is documented in
//! `docs/requirements/configuration.md`. This module implements the practical
//! subset needed by the pipeline; unknown YAML keys are ignored (forward
//! compatible) so newer config files can be loaded by older binaries without
//! a hard failure.
//!
//! Paths support tilde-expansion (`~`, `~/foo`) via [`expand_path`].
//!
//! # Example
//!
//! ```ignore
//! use std::path::Path;
//! use tga::core::config::Config;
//!
//! let cfg = Config::load(Path::new("config.yaml")).expect("load");
//! println!("repos: {}", cfg.repositories.len());
//! ```

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::errors::{Result, TgaError};

/// Top-level configuration root.
///
/// Mirrors the YAML schema from the Python predecessor. All top-level
/// sections are optional except `repositories`, which must contain at
/// least one entry to be useful.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    /// Repositories to analyze.
    #[serde(default)]
    pub repositories: Vec<RepositoryConfig>,

    /// Team / member roster and aliases.
    #[serde(default)]
    pub team: Option<TeamConfig>,

    /// Output destination and format flags.
    #[serde(default)]
    pub output: Option<OutputConfig>,

    /// Classification cascade settings.
    #[serde(default)]
    pub classification: Option<ClassificationConfig>,

    /// GitHub API credentials and scope.
    #[serde(default)]
    pub github: Option<GithubConfig>,

    /// JIRA API credentials and scope.
    #[serde(default)]
    pub jira: Option<JiraConfig>,

    /// Schema version string (e.g. `"1.0"`).
    ///
    /// Stored for forward compatibility with the Python predecessor's YAML
    /// format. Not enforced by the Rust loader — present so files written
    /// for the Python tool deserialize cleanly.
    #[serde(default)]
    pub version: Option<String>,

    /// Named profile (e.g. `"balanced"`).
    ///
    /// Stored for forward compatibility with the Python predecessor. Not
    /// currently consumed by the Rust pipeline.
    #[serde(default)]
    pub profile: Option<String>,

    /// Python-compatible flat alias map: canonical name → list of email
    /// addresses or login aliases.
    ///
    /// When non-empty, takes precedence over [`TeamConfig::members`] for
    /// identity resolution (see [`Config::resolved_aliases`]).
    #[serde(default)]
    pub developer_aliases: HashMap<String, Vec<String>>,

    /// Analysis settings (ML categorization, etc.).
    ///
    /// Parsed for forward compatibility; individual sub-features gate their
    /// own behavior on its presence.
    #[serde(default)]
    pub analysis: Option<AnalysisConfig>,

    /// Cache directory and related settings.
    #[serde(default)]
    pub cache: Option<CacheConfig>,
}

/// Analysis pipeline configuration (forward-compat with Python schema).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnalysisConfig {
    /// ML-based commit categorization settings.
    #[serde(default)]
    pub ml_categorization: Option<MlCategorizationConfig>,
}

/// ML categorization toggle and model selection.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MlCategorizationConfig {
    /// Whether ML categorization is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Optional model identifier.
    #[serde(default)]
    pub model: Option<String>,
}

/// Cache layer configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CacheConfig {
    /// Filesystem directory used for cached artifacts. Supports `~` expansion.
    #[serde(default)]
    pub directory: Option<PathBuf>,
}

/// A single repository to collect commits from.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RepositoryConfig {
    /// Local filesystem path to the repository (supports `~` expansion).
    pub path: PathBuf,

    /// Display name used in reports. Falls back to the directory basename.
    #[serde(default)]
    pub name: Option<String>,

    /// Branch override; if `None`, the default branch is auto-detected.
    #[serde(default)]
    pub branch: Option<String>,

    /// Inclusive start date for commit collection (ISO 8601).
    #[serde(default)]
    pub since_date: Option<String>,

    /// Inclusive end date for commit collection (ISO 8601).
    #[serde(default)]
    pub until_date: Option<String>,
}

/// Team roster and identity aliases.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamConfig {
    /// Canonical team members.
    #[serde(default)]
    pub members: Vec<TeamMember>,

    /// Free-form aliases map: alias → canonical name.
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

/// A canonical team member with optional alias list.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TeamMember {
    /// Canonical display name.
    pub name: String,

    /// Primary email address (canonical).
    pub email: String,

    /// Alternative names/emails that map to this member.
    #[serde(default)]
    pub aliases: Vec<String>,
}

/// Output / reporting configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputConfig {
    /// Single output format identifier (`csv`, `json`, `markdown`).
    ///
    /// Retained for backward compatibility; prefer [`OutputConfig::formats`].
    #[serde(default)]
    pub format: Option<String>,

    /// Destination directory for reports.
    ///
    /// Accepts both `directory` (Python-compat) and `output_path` (legacy
    /// Rust) keys in the YAML.
    #[serde(default, alias = "output_path")]
    pub directory: Option<PathBuf>,

    /// Output format list (e.g. `["csv", "markdown"]`).
    #[serde(default)]
    pub formats: Vec<String>,

    /// Include unclassified commits in output.
    #[serde(default)]
    pub include_unclassified: bool,

    /// Include merge commits in output.
    #[serde(default)]
    pub include_merges: bool,

    /// Include file-level details in output.
    #[serde(default)]
    pub include_files: bool,
}

/// Classification cascade configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassificationConfig {
    /// Path to user-supplied rules YAML/JSON.
    #[serde(default)]
    pub rules_file: Option<PathBuf>,

    /// Whether to engage the LLM fallback tier.
    #[serde(default)]
    pub use_llm: bool,

    /// LLM model identifier (provider-specific).
    #[serde(default)]
    pub llm_model: Option<String>,

    /// Minimum confidence required to accept a classification.
    #[serde(default = "default_confidence_threshold")]
    pub confidence_threshold: f64,
}

fn default_confidence_threshold() -> f64 {
    0.7
}

/// GitHub API integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GithubConfig {
    /// Personal access token (often sourced from `GITHUB_TOKEN`).
    #[serde(default)]
    pub token: Option<String>,

    /// Organization slug for org-wide queries.
    #[serde(default)]
    pub org: Option<String>,

    /// Single-repository slug (`owner/name`).
    #[serde(default)]
    pub repo: Option<String>,

    /// Whether to fetch pull request metadata.
    #[serde(default)]
    pub fetch_prs: bool,
}

/// JIRA Cloud / Server integration settings.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct JiraConfig {
    /// Base URL of the JIRA instance.
    #[serde(default)]
    pub url: Option<String>,

    /// API username (typically an email address for Cloud).
    #[serde(default)]
    pub username: Option<String>,

    /// API token.
    #[serde(default)]
    pub token: Option<String>,

    /// Project key for filtering issues (e.g. `API`).
    #[serde(default)]
    pub project_key: Option<String>,
}

/// Expand a leading `~` in a path to the current user's home directory.
///
/// Returns the path unchanged if it does not start with `~`. If `~` is
/// present but the home directory cannot be determined, the path is also
/// returned unchanged.
pub fn expand_path(path: &Path) -> PathBuf {
    let s = match path.to_str() {
        Some(s) => s,
        None => return path.to_path_buf(),
    };
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    } else if s == "~" {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home);
        }
    }
    path.to_path_buf()
}

impl Config {
    /// Load a YAML configuration from disk.
    ///
    /// # Errors
    ///
    /// - [`TgaError::IoError`] if the file cannot be read.
    /// - [`TgaError::SerdeYamlError`] if YAML parsing fails.
    pub fn load(path: &Path) -> Result<Config> {
        let resolved = expand_path(path);
        tracing::debug!(path = %resolved.display(), "loading config");
        let text = std::fs::read_to_string(&resolved)?;
        let cfg: Config = serde_yaml::from_str(&text)?;
        Ok(cfg)
    }

    /// Resolve identity aliases from either the Python-compatible
    /// [`Config::developer_aliases`] map or from [`TeamConfig::members`].
    ///
    /// `developer_aliases` (when non-empty) takes precedence. The returned
    /// map is keyed by canonical name; values are the list of email
    /// addresses or login aliases that should resolve to that name.
    pub fn resolved_aliases(&self) -> HashMap<String, Vec<String>> {
        if !self.developer_aliases.is_empty() {
            self.developer_aliases.clone()
        } else if let Some(team) = &self.team {
            team.members
                .iter()
                .map(|m| (m.name.clone(), m.aliases.clone()))
                .collect()
        } else {
            HashMap::new()
        }
    }

    /// Validate cross-field invariants of the config.
    ///
    /// # Errors
    ///
    /// Returns [`TgaError::ValidationError`] if any invariant is violated.
    pub fn validate(&self) -> Result<()> {
        if self.repositories.is_empty() {
            return Err(TgaError::ValidationError(
                "at least one repository must be configured".into(),
            ));
        }
        for r in &self.repositories {
            if r.path.as_os_str().is_empty() {
                return Err(TgaError::ValidationError(
                    "repository.path must not be empty".into(),
                ));
            }
        }
        Ok(())
    }
}

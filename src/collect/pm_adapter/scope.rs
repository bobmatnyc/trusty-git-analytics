//! Per-repo PM scope (#272): which PM system may resolve a repo's ticket keys.
//!
//! Why: JIRA and Linear keys share the `KEY-N` shape, so a repo tracked in
//! Linear can carry keys (`PLA-12`, `DP-3`, `FD-9`) that collide with a JIRA
//! project prefix. Every adapter scans every commit today, so such a key was
//! also looked up — and linked — in JIRA.
//! What: `linear.repos` binds repos to Linear. [`bound_source`] answers which
//! source a repository is bound to, and [`in_scope`] whether an adapter may
//! resolve that repository's keys.
//!
//! Precedence, first match wins:
//! 1. an entry equal to the repository label (`name`, else path basename);
//! 2. an entry equal to the repo's owner (`org`, else the owner half of
//!    `github_repo`);
//! 3. no match: the repo is unbound and every adapter scans it, as before.
//!
//! A bound repo is scanned only by its bound source's adapter, and the
//! classify resolver queries only that source's sources for it.
//! Test: `bound_repo_resolves_pla_to_linear_only`,
//! `unbound_repo_keeps_todays_behaviour`, `binding_matches_label_or_owner`.

use crate::collect::pm_adapter::PmSource;
use crate::core::config::{split_github_slug, Config, RepositoryConfig};

/// A repository's label as stored in `commits.repository`.
///
/// Delegates to [`crate::report::repo_name`], the one rule the extractor
/// writes `commits.repository` with (#5453), so a binding matches stored rows.
pub fn repo_label(repo: &RepositoryConfig) -> String {
    crate::report::repo_name(repo.name.as_deref(), &repo.path)
}

/// The PM source `repository` is bound to, or `None` when it is unbound.
///
/// Why/What: see the module doc for the precedence. An entry equal to the
/// repository label binds it even when the repo is absent from
/// `repositories`, so a stored label alone suffices.
/// Test: `binding_matches_label_or_owner`.
pub fn bound_source(config: &Config, repository: &str) -> Option<PmSource> {
    let entries = &config.linear.as_ref()?.repos;
    if entries.is_empty() {
        return None;
    }
    // 1. Label match.
    if entries.iter().any(|e| e == repository) {
        return Some(PmSource::Linear);
    }
    // 2. Owner match, for the configured repo carrying this label.
    let repo = config
        .repositories
        .iter()
        .find(|r| repo_label(r) == repository)?;
    let owner = repo.org.as_deref().or_else(|| {
        repo.github_repo
            .as_deref()
            .and_then(split_github_slug)
            .map(|(owner, _)| owner)
    })?;
    entries
        .iter()
        .any(|e| e.eq_ignore_ascii_case(owner))
        .then_some(PmSource::Linear)
}

/// Whether an adapter for `source` may resolve keys from `repository`.
///
/// `true` for an unbound repo and for the bound source; `false` otherwise.
pub fn in_scope(config: &Config, repository: &str, source: PmSource) -> bool {
    !matches!(bound_source(config, repository), Some(bound) if bound != source)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        serde_yaml::from_str(
            r#"
repositories:
  - path: /src/pla-app
    org: pacerevenue
  - path: /src/rates
    github_repo: pacerevenue/rates
  - path: /src/duetto
    name: duetto
    org: duettoresearch
linear:
  repos: [pacerevenue, solo-repo]
"#,
        )
        .expect("yaml parses")
    }

    #[test]
    fn binding_matches_label_or_owner() {
        let cfg = config();
        assert_eq!(bound_source(&cfg, "pla-app"), Some(PmSource::Linear));
        assert_eq!(bound_source(&cfg, "rates"), Some(PmSource::Linear));
        assert_eq!(bound_source(&cfg, "solo-repo"), Some(PmSource::Linear));
        assert_eq!(bound_source(&cfg, "duetto"), None);
        assert_eq!(bound_source(&Config::default(), "pla-app"), None);
    }

    #[test]
    fn bound_repo_resolves_pla_to_linear_only() {
        let cfg = config();
        assert!(in_scope(&cfg, "pla-app", PmSource::Linear));
        assert!(!in_scope(&cfg, "pla-app", PmSource::Jira));
        assert!(!in_scope(&cfg, "pla-app", PmSource::GitHub));
    }

    #[test]
    fn unbound_repo_keeps_todays_behaviour() {
        let cfg = config();
        for source in [
            PmSource::Jira,
            PmSource::Linear,
            PmSource::GitHub,
            PmSource::AzureDevOps,
        ] {
            assert!(in_scope(&cfg, "duetto", source));
        }
    }
}

//! Anchor the relative paths a config file names to that file's directory.
//!
//! Why: #111 — `classification.rules_file` resolved against the process CWD,
//! while `database:` resolved against the config's directory, so
//! `tga --config /abs/config.yaml` run from elsewhere failed to find a rules
//! file that sat next to the config.
//! What: [`anchor_relative_paths`] rewrites the listed path fields that are
//! still relative after `~` expansion, using the same rule as
//! [`super::database_path::resolve`]. `database:` and `aliases_file:` are
//! already anchored where they are read. `repositories[].path` is left as
//! written, still relative to the CWD: repository names and GitHub slug
//! lookup derive from that path, and anchoring it is a separate decision.
//! Test: `tests::relative_paths_anchor_to_the_config_dir`,
//! `tests::repository_paths_stay_as_written`,
//! `tests::path_dot_resolves_the_github_slug_as_before`,
//! `tests/eval_harness.rs::rules_file_resolves_from_another_cwd`.

use std::path::{Path, PathBuf};

use super::database_path::resolve_with_home;
use super::Config;

/// Rewrite `config`'s relative path fields against `config_dir`.
///
/// Why: see the module doc. What: anchors `classification.rules_files`,
/// `output.directory`, `cache.directory` and `dora.datadog_dir`; absolute and
/// `~` paths are only `~`-expanded. Repository paths are not touched.
/// Test: `tests::relative_paths_anchor_to_the_config_dir`,
/// `tests::repository_paths_stay_as_written`.
pub(crate) fn anchor_relative_paths(config: &mut Config, config_dir: &Path, home: Option<&Path>) {
    let anchor = |p: &mut PathBuf| {
        if let Some(resolved) = resolve_with_home(Some(p), Some(config_dir), home) {
            *p = resolved;
        }
    };
    if let Some(c) = config.classification.as_mut() {
        c.rules_files.iter_mut().for_each(anchor);
    }
    if let Some(dir) = config.output.as_mut().and_then(|o| o.directory.as_mut()) {
        anchor(dir);
    }
    if let Some(dir) = config.cache.as_mut().and_then(|c| c.directory.as_mut()) {
        anchor(dir);
    }
    if let Some(dir) = config.dora.as_mut().and_then(|d| d.datadog_dir.as_mut()) {
        anchor(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> Config {
        serde_yaml::from_str(yaml).expect("parse config")
    }

    /// Why: #111 — a relative rules, output, cache or datadog path in a config
    /// means "next to this config".
    /// What: each of those joins onto the config dir; an absolute path is
    /// unchanged and `~` expands against the supplied home.
    /// Test: this function.
    #[test]
    fn relative_paths_anchor_to_the_config_dir() {
        let mut cfg = parse(
            "classification:\n  rules_files: [rules.yaml, /etc/r.yaml, \"~/r.yaml\"]\n\
             output:\n  directory: out\ncache:\n  directory: .cache\n\
             dora:\n  datadog_dir: incidents\n",
        );
        anchor_relative_paths(&mut cfg, Path::new("/cfg"), Some(Path::new("/home/u")));
        let rules = &cfg.classification.as_ref().expect("section").rules_files;
        assert_eq!(
            rules,
            &[
                PathBuf::from("/cfg/rules.yaml"),
                PathBuf::from("/etc/r.yaml"),
                PathBuf::from("/home/u/r.yaml"),
            ]
        );
        let out = cfg.output.and_then(|o| o.directory);
        assert_eq!(out, Some(PathBuf::from("/cfg/out")));
        let cache = cfg.cache.and_then(|c| c.directory);
        assert_eq!(cache, Some(PathBuf::from("/cfg/.cache")));
        let dd = cfg.dora.and_then(|d| d.datadog_dir);
        assert_eq!(dd, Some(PathBuf::from("/cfg/incidents")));
    }

    /// Why: #111 review — a repository's stored name and GitHub slug derive
    /// from its path; anchoring it would rename `path: .` and split its
    /// history, so repository paths keep a96727c's behaviour.
    /// What: `.`, `.` with `name: ""`, and `repos/a` keep their paths and
    /// names; the stored names are `.`, `.` and `a`.
    /// Test: this function.
    #[test]
    fn repository_paths_stay_as_written() {
        let mut cfg =
            parse("repositories:\n  - path: .\n  - path: .\n    name: \"\"\n  - path: repos/a\n");
        anchor_relative_paths(&mut cfg, Path::new("/cfg"), None);
        let paths: Vec<&Path> = cfg.repositories.iter().map(|r| r.path.as_path()).collect();
        assert_eq!(
            paths,
            vec![Path::new("."), Path::new("."), Path::new("repos/a")]
        );
        let names: Vec<Option<&str>> = cfg.repositories.iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, vec![None, Some(""), None]);
        let stored: Vec<String> = cfg
            .repositories
            .iter()
            .map(|r| crate::report::repo_name(r.name.as_deref(), &r.path))
            .collect();
        assert_eq!(stored, vec![".", ".", "a"]);
    }

    /// Why: #111 review — `path: .` with `github.org` and no `name` finds its
    /// GitHub slug from the `origin` remote of the CWD's clone, as on a96727c,
    /// not from the config directory's clone.
    /// What: a config inside a clone whose origin is `acme/widget` loads with
    /// `path: .` unchanged and resolves exactly the slug the CWD's remote
    /// gives, with no `name` written.
    /// Test: this function.
    #[test]
    fn path_dot_resolves_the_github_slug_as_before() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = git2::Repository::init(dir.path()).expect("git init");
        repo.remote("origin", "https://github.com/acme/widget.git")
            .expect("remote");
        let cfg_path = dir.path().join("config.yaml");
        std::fs::write(
            &cfg_path,
            "repositories:\n  - path: .\ngithub:\n  org: acme\n",
        )
        .expect("write config");
        let cfg = Config::load(&cfg_path).expect("load");
        let r = &cfg.repositories[0];
        assert_eq!(
            (r.path.as_path(), r.name.as_deref()),
            (Path::new("."), None)
        );
        let github = cfg.github.as_ref().expect("github section");
        let from_cwd =
            crate::collect::github::repo_resolver::owner_repo_from_remote(Path::new("."));
        assert_eq!(
            crate::collect::github::resolve_github_repos(github, &cfg.repositories),
            from_cwd.into_iter().collect::<Vec<_>>()
        );
    }
}

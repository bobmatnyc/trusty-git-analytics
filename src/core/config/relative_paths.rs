//! Anchor the relative paths a config file names to that file's directory.
//!
//! Why: #111 — `classification.rules_file` resolved against the process CWD,
//! while `database:` resolved against the config's directory, so
//! `tga --config /abs/config.yaml` run from elsewhere failed to find a rules
//! file that sat next to the config. The Python predecessor anchors
//! repository, output and cache paths to the config directory too.
//! What: [`anchor_relative_paths`] rewrites every path field that is still
//! relative after `~` expansion, using the same rule as
//! [`super::database_path::resolve`]. `database:` and `aliases_file:` are
//! already anchored where they are read and are left alone here.
//! Test: `tests::relative_paths_anchor_to_the_config_dir`,
//! `tests::a_nameless_repo_keeps_its_old_name`,
//! `tests::path_dot_resolves_the_github_slug_from_the_remote`,
//! `tests/eval_harness.rs::rules_file_resolves_from_another_cwd`.

use std::path::{Path, PathBuf};

use super::database_path::resolve_with_home;
use super::Config;

/// Rewrite `config`'s relative path fields against `config_dir`.
///
/// Why: see the module doc. What: anchors `classification.rules_files`,
/// `repositories[].path`, `output.directory`, `cache.directory` and
/// `dora.datadog_dir`; absolute and `~` paths are only `~`-expanded. A
/// relative repository path is kept in `configured_path`, which names are
/// derived from ([`super::RepositoryConfig::name_path`]), so `path: .` keeps
/// its stored name and its GitHub slug still comes from the remote.
/// Test: `tests::relative_paths_anchor_to_the_config_dir`,
/// `tests::a_nameless_repo_keeps_its_old_name`,
/// `tests::path_dot_resolves_the_github_slug_from_the_remote`.
pub(crate) fn anchor_relative_paths(config: &mut Config, config_dir: &Path, home: Option<&Path>) {
    let anchor = |p: &mut PathBuf| {
        if let Some(resolved) = resolve_with_home(Some(p), Some(config_dir), home) {
            *p = resolved;
        }
    };
    if let Some(c) = config.classification.as_mut() {
        c.rules_files.iter_mut().for_each(anchor);
    }
    for repo in &mut config.repositories {
        // #111: names derive from the path as written (`name_path`), never
        // from the anchored one, and `name` itself is left untouched.
        if !super::expand_path_with(&repo.path, home).is_absolute() {
            repo.configured_path = Some(repo.path.clone());
        }
        anchor(&mut repo.path);
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

    /// Why: #111 — a relative path in a config means "next to this config".
    /// What: every anchored field joins onto the config dir; an absolute
    /// path is unchanged and `~` expands against the supplied home.
    /// Test: this function.
    #[test]
    fn relative_paths_anchor_to_the_config_dir() {
        let mut cfg = parse(
            "repositories:\n  - path: repos/a\n  - path: /abs/b\n\
             classification:\n  rules_files: [rules.yaml, /etc/r.yaml, \"~/r.yaml\"]\n\
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
        assert_eq!(cfg.repositories[0].path, PathBuf::from("/cfg/repos/a"));
        assert_eq!(cfg.repositories[1].path, PathBuf::from("/abs/b"));
        let out = cfg.output.and_then(|o| o.directory);
        assert_eq!(out, Some(PathBuf::from("/cfg/out")));
        let cache = cfg.cache.and_then(|c| c.directory);
        assert_eq!(cache, Some(PathBuf::from("/cfg/.cache")));
        let dd = cfg.dora.and_then(|d| d.datadog_dir);
        assert_eq!(dd, Some(PathBuf::from("/cfg/incidents")));
    }

    /// Why: a repository's stored name comes from its path's last component,
    /// falling back to the whole path; anchoring `.` would rename it to the
    /// config directory's name and split its history in the database. A blank
    /// `name` counts as unset (#111 review).
    /// What: `name` is never written; the stored name (`report::repo_name`
    /// over `name_path`) is `.` for `path: .`, with or without `name: ""`,
    /// `a` for `repos/a`, and a set name wins.
    /// Test: this function.
    #[test]
    fn a_nameless_repo_keeps_its_old_name() {
        let mut cfg = parse(
            "repositories:\n  - path: .\n  - path: .\n    name: \"\"\n  - path: repos/a\n  \
             - path: ..\n    name: up\n",
        );
        anchor_relative_paths(&mut cfg, Path::new("/cfg"), None);
        let names: Vec<Option<&str>> = cfg.repositories.iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, vec![None, Some(""), None, Some("up")]);
        let stored: Vec<String> = cfg
            .repositories
            .iter()
            .map(|r| crate::report::repo_name(r.name.as_deref(), r.name_path()))
            .collect();
        assert_eq!(stored, vec![".", ".", "a", "up"]);
        assert_eq!(cfg.repositories[2].path, PathBuf::from("/cfg/repos/a"));
    }

    /// Why: #111 review — `path: .` with `github.org` and no `name` found its
    /// GitHub slug from the clone's `origin` remote; a derived name of `.` or
    /// of the config directory would ask GitHub for the wrong repository and
    /// report zero PRs without an error.
    /// What: a config inside a clone whose origin is `acme/widget` resolves to
    /// `acme/widget`, leaves `name` unset and keeps the stored name `.`.
    /// Test: this function.
    #[test]
    fn path_dot_resolves_the_github_slug_from_the_remote() {
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
        let github = cfg.github.as_ref().expect("github section");
        assert_eq!(
            crate::collect::github::resolve_github_repos(github, &cfg.repositories),
            vec![("acme".to_string(), "widget".to_string())]
        );
        let r = &cfg.repositories[0];
        assert_eq!(r.name, None);
        assert_eq!(
            crate::report::repo_name(r.name.as_deref(), r.name_path()),
            "."
        );
    }
}

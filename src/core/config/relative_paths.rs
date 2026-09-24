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
//! `tests/eval_harness.rs::rules_file_resolves_from_another_cwd`.

use std::path::{Path, PathBuf};

use super::database_path::resolve_with_home;
use super::Config;

/// Rewrite `config`'s relative path fields against `config_dir`.
///
/// Why: see the module doc. What: anchors `classification.rules_files`,
/// `repositories[].path`, `output.directory`, `cache.directory` and
/// `dora.datadog_dir`; absolute and `~` paths are only `~`-expanded. A
/// repository with no `name` whose path has no final component (`.`, `..`)
/// took its display name from the raw path; that name is pinned into `name`
/// so anchoring cannot rename the repository in stored data.
/// Test: `tests::relative_paths_anchor_to_the_config_dir`,
/// `tests::a_nameless_repo_keeps_its_old_name`.
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
        let relative = !super::expand_path_with(&repo.path, home).is_absolute();
        if relative && repo.name.is_none() && repo.path.file_name().is_none() {
            repo.name = Some(repo.path.display().to_string());
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
    /// config directory's name and split its history in the database.
    /// What: `.` without a name gets `name: "."`; `repos/a` gets no name, since
    /// its last component is unchanged by anchoring; a set name is kept.
    /// Test: this function.
    #[test]
    fn a_nameless_repo_keeps_its_old_name() {
        let mut cfg =
            parse("repositories:\n  - path: .\n  - path: repos/a\n  - path: ..\n    name: up\n");
        anchor_relative_paths(&mut cfg, Path::new("/cfg"), None);
        let names: Vec<Option<&str>> = cfg.repositories.iter().map(|r| r.name.as_deref()).collect();
        assert_eq!(names, vec![Some("."), None, Some("up")]);
        assert_eq!(cfg.repositories[1].path, PathBuf::from("/cfg/repos/a"));
    }
}

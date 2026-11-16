//! #266: tests for GitHub repo resolution from `github_repo`, `origin`, and
//! org + name.
//!
//! Loaded via `#[cfg(test)] #[path]` in `repo_resolver.rs`.

use std::path::Path;

use tracing_test::traced_test;

use super::{extract_owner_repo_from_url, probe_origin, resolve_repo_slug, OriginProbe};
use crate::collect::github::org_discovery::resolve_github_repos_with_discovered;
use crate::core::config::{Config, GithubConfig, RepositoryConfig};

/// A git repo at `dir` with `origin` set to `url` (no remote when `None`).
fn init_clone(dir: &Path, url: Option<&str>) {
    let repo = git2::Repository::init(dir).expect("init repo");
    if let Some(url) = url {
        repo.remote("origin", url).expect("add origin");
    }
}

/// A repo entry at `path` with `name` and `org`.
fn entry(path: &Path, name: &str, org: Option<&str>) -> RepositoryConfig {
    let mut r = RepositoryConfig::new(path);
    r.name = Some(name.to_string());
    r.org = org.map(str::to_string);
    r
}

fn pair(o: &str, r: &str) -> Option<(String, String)> {
    Some((o.to_string(), r.to_string()))
}

/// [`resolve_repo_slug`] for an entry that must not error.
fn slug(r: &RepositoryConfig, default_org: Option<&str>) -> Option<(String, String)> {
    resolve_repo_slug(r, default_org).expect("resolve")
}

/// The collector's resolve entry point, for inputs that must not error.
fn resolved(
    gh: &GithubConfig,
    repos: &[RepositoryConfig],
    discovered: &[(String, String)],
) -> Vec<(String, String)> {
    resolve_github_repos_with_discovered(gh, repos, discovered).expect("resolve")
}

/// Why: the parser must accept every GitHub remote form and reject the rest.
/// What: SSH scp-syntax, HTTPS with and without `.git`, `ssh://`; then a
/// non-GitHub host and junk input.
/// Test: this test.
#[test]
fn extract_owner_repo_from_url_covers_forms_and_failures() {
    for url in [
        "git@github.com:o/r.git",
        "git@github.com:o/r",
        "https://github.com/o/r.git",
        "https://github.com/o/r",
        "ssh://git@github.com/o/r.git",
    ] {
        assert_eq!(extract_owner_repo_from_url(url), pair("o", "r"), "{url}");
    }
    for url in [
        "git@gitlab.com:o/r.git",
        "https://bitbucket.org/o/r",
        "not a url",
        "",
        "https://github.com/",
    ] {
        assert_eq!(extract_owner_repo_from_url(url), None, "{url}");
    }
}

/// Why: the resolver logs each fallback by its cause, so the probe must
/// tell them apart.
/// What: GitHub origin, GitLab origin, repo without origin, and no repo.
/// Test: this test.
#[test]
fn probe_origin_classifies_each_outcome() {
    let gh = tempfile::tempdir().expect("tmp");
    init_clone(gh.path(), Some("git@github.com:acme/widgets.git"));
    assert_eq!(
        probe_origin(gh.path()),
        OriginProbe::GitHub(("acme".into(), "widgets".into()))
    );

    let gl = tempfile::tempdir().expect("tmp");
    init_clone(gl.path(), Some("git@gitlab.com:acme/widgets.git"));
    assert!(matches!(probe_origin(gl.path()), OriginProbe::NotGitHub(_)));

    let bare = tempfile::tempdir().expect("tmp");
    init_clone(bare.path(), None);
    assert_eq!(probe_origin(bare.path()), OriginProbe::Missing);

    let none = tempfile::tempdir().expect("tmp");
    assert_eq!(probe_origin(none.path()), OriginProbe::Missing);
}

/// Why: closure condition 1 (ssh) — origin wins over org + name.
/// What: ssh origin `real-org/real-repo`, config `org: acme`, `name: label`.
/// Test: this test.
#[test]
fn origin_ssh_remote_sets_the_fetch_target() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@github.com:real-org/real-repo.git"));
    let r = entry(d.path(), "label", Some("acme"));
    assert_eq!(slug(&r, None), pair("real-org", "real-repo"));
}

/// Why: closure condition 1 (https) — origin wins over org + name.
/// What: https origin without `.git`, config `org: acme`, `name: label`.
/// Test: this test.
#[test]
fn origin_https_remote_sets_the_fetch_target() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("https://github.com/real-org/real-repo"));
    let r = entry(d.path(), "label", Some("acme"));
    assert_eq!(slug(&r, None), pair("real-org", "real-repo"));
}

/// Why: closure condition 2 — the duetto case. `name` is a report label.
/// What: `name: duetto-repos`, `org: duettoresearch`, origin
/// `duettoresearch/duetto`, through the collector's entry point.
/// Test: this test.
#[test]
fn remote_named_differently_from_name_fetches_the_remote() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@github.com:duettoresearch/duetto.git"));
    let r = entry(d.path(), "duetto-repos", Some("duettoresearch"));
    let got = resolved(&GithubConfig::default(), &[r], &[]);
    assert_eq!(got, vec![("duettoresearch".into(), "duetto".into())]);
}

/// Why: closure condition 3 — no origin falls back to org + name, loudly.
/// What: repo with no remote, `org: acme`, `name: widgets`.
/// Test: this test.
#[test]
#[traced_test]
fn missing_origin_falls_back_to_org_and_name_with_warning() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), None);
    let r = entry(d.path(), "widgets", Some("acme"));
    assert_eq!(slug(&r, None), pair("acme", "widgets"));
    assert!(logs_contain("WARN"));
    assert!(logs_contain("no readable origin remote"));
}

/// Why: a non-GitHub origin also falls back with a per-repo warning.
/// What: GitLab origin, `github.org: acme` default owner.
/// Test: this test.
#[test]
#[traced_test]
fn non_github_origin_falls_back_with_warning() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@gitlab.com:x/y.git"));
    let r = entry(d.path(), "widgets", None);
    assert_eq!(slug(&r, Some("acme")), pair("acme", "widgets"));
    assert!(logs_contain("not a GitHub URL"));
}

/// Why: closure condition 4 — same-named clones under two orgs must not
/// collapse into one target.
/// What: two clones, both `name: api`, origins `org-a/api` and `org-b/api`,
/// with one shared `github.org`.
/// Test: this test.
#[test]
fn same_named_clones_under_two_orgs_resolve_distinctly() {
    let a = tempfile::tempdir().expect("tmp");
    let b = tempfile::tempdir().expect("tmp");
    init_clone(a.path(), Some("git@github.com:org-a/api.git"));
    init_clone(b.path(), Some("https://github.com/org-b/api.git"));
    let gh = GithubConfig {
        org: Some("org-a".into()),
        ..GithubConfig::default()
    };
    let repos = [entry(a.path(), "api", None), entry(b.path(), "api", None)];
    let got = resolved(&gh, &repos, &[]);
    assert_eq!(
        got,
        vec![
            ("org-a".into(), "api".into()),
            ("org-b".into(), "api".into())
        ]
    );
}

/// Why: `github_repo` parses from the gitflow-analytics YAML key.
/// What: deserialize a repository entry carrying it; omit it on another.
/// Test: this test.
#[test]
fn github_repo_field_parses_from_yaml() {
    let r: RepositoryConfig =
        serde_yaml::from_str("path: /x\nname: duetto-repos\ngithub_repo: duettoresearch/duetto\n")
            .expect("parse");
    assert_eq!(r.github_repo.as_deref(), Some("duettoresearch/duetto"));
    let r: RepositoryConfig = serde_yaml::from_str("path: /x\n").expect("parse");
    assert_eq!(r.github_repo, None);
}

/// Why: a malformed `github_repo` must fail the production load path, with
/// the same rule `parse_slug` applies.
/// What: `Config::load` over YAML files with good and bad slugs; each
/// verdict matches `parse_slug`'s.
/// Test: this test.
#[test]
fn load_rejects_malformed_github_repo() {
    let dir = tempfile::tempdir().expect("tmp");
    for (slug, ok) in [
        ("o/r", true),
        ("o", false),
        ("/r", false),
        ("o/", false),
        ("", false),
    ] {
        let path = dir.path().join("cfg.yaml");
        let yaml = format!("repositories:\n  - path: /x\n    github_repo: \"{slug}\"\n");
        std::fs::write(&path, yaml).expect("write");
        let loaded = Config::load(&path);
        assert_eq!(loaded.is_ok(), ok, "{slug}: {:?}", loaded.err());
        assert_eq!(super::parse_slug(slug).is_ok(), ok, "parse_slug {slug}");
    }
}

/// Why (#266): a malformed `github_repo` that reaches the resolver must be
/// an error, never a repo that silently drops out of collection.
/// What: a hand-built config (no load-time check) with `github_repo: bad`.
/// Test: this test.
#[test]
fn malformed_github_repo_is_a_resolution_error() {
    let mut r = RepositoryConfig::new("/no-such-clone");
    r.github_repo = Some("bad".into());
    assert!(resolve_repo_slug(&r, None).is_err());
    assert!(resolve_github_repos_with_discovered(&GithubConfig::default(), &[r], &[]).is_err());
}

/// Why (#266): stored PR rows key on the resolved slug, case-sensitively.
/// When origin is the configured repo in another case, the configured
/// spelling must win so existing rows do not split.
/// What: config `Acme/Widgets`, origin `acme/widgets`.
/// Test: this test.
#[test]
#[traced_test]
fn configured_spelling_kept_when_origin_matches_ignoring_case() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@github.com:acme/widgets.git"));
    let r = entry(d.path(), "Widgets", Some("Acme"));
    assert_eq!(slug(&r, None), pair("Acme", "Widgets"));
    assert!(!logs_contain("differs from configured"));
}

/// Why (#266): one repo must be fetched once even when origin and org
/// discovery spell it in different cases.
/// What: origin `acme/api`, discovered `Acme/api` and `ACME/API`.
/// Test: this test.
#[test]
fn dedup_is_case_insensitive_across_discovery() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@github.com:acme/api.git"));
    let r = entry(d.path(), "label", None);
    let discovered = [
        ("Acme".to_string(), "api".to_string()),
        ("ACME".to_string(), "API".to_string()),
    ];
    let got = resolved(&GithubConfig::default(), &[r], &discovered);
    assert_eq!(got, vec![("acme".into(), "api".into())]);
}

/// Why (#266): a configured org + name that origin overrides must be
/// visible, naming both.
/// What: config `acme/old`, origin `acme/new`.
/// Test: this test.
#[test]
#[traced_test]
fn origin_differing_from_config_warns_naming_both() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("https://github.com/acme/new.git"));
    let r = entry(d.path(), "old", Some("acme"));
    assert_eq!(slug(&r, None), pair("acme", "new"));
    assert!(logs_contain("differs from configured"));
    assert!(logs_contain("acme/new"));
    assert!(logs_contain("acme/old"));
}

/// Why: the brief's acceptance test — with `github_repo` set, collection
/// targets it over both `name` and a conflicting origin.
/// What: YAML entry `name: duetto-repos`, `org: duettoresearch`,
/// `github_repo: duettoresearch/duetto`, origin pointing elsewhere, through
/// the collector's entry point.
/// Test: this test.
#[test]
fn github_repo_overrides_name_and_origin_for_collection() {
    let d = tempfile::tempdir().expect("tmp");
    init_clone(d.path(), Some("git@github.com:someone/fork.git"));
    let yaml = format!(
        "path: {}\nname: duetto-repos\norg: duettoresearch\ngithub_repo: duettoresearch/duetto\n",
        d.path().display()
    );
    let r: RepositoryConfig = serde_yaml::from_str(&yaml).expect("parse");
    let got = resolved(&GithubConfig::default(), &[r], &[]);
    assert_eq!(got, vec![("duettoresearch".into(), "duetto".into())]);
}

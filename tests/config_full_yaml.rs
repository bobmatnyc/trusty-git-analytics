//! #137: config deserialization survives `#[non_exhaustive]`.
//!
//! Why: 10.0.0 marks every config struct and enum `#[non_exhaustive]`. That
//! blocks struct literals outside the crate but must not change what a YAML
//! file deserializes to. These tests run from outside the crate, through the
//! public [`Config::load`], so they see the API an external caller sees.
//! What: loads one config that sets every top-level section and asserts a key
//! field in each, then loads the shipped `configs/` fixtures.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use tga::classify::sources::SourceConfig;
use tga::core::config::{AliasFile, Config, LlmEffort, LlmFallbackScope, LlmSource};

/// One value for every top-level key `Config` accepts. Paths are absolute so
/// the loader's relative-path anchoring leaves them as written.
const FULL_CONFIG: &str = r#"
version: "1.0"
profile: engineering
database: /var/data/tga.db
aliases_file: /etc/tga/aliases.yaml
fuzzy_identity_fallback: false

repositories:
  - name: my-app
    path: /src/my-app
    branch: main
    since_date: "2025-01-01"
    until_date: "2025-06-30"
    owner: acme
    head_only: true
    fetch_timeout_secs: 30

team:
  canonical_domain: acme.com
  members:
    - name: Alice Smith
      email: alice@acme.com
      aliases: [alice@old.example]
  aliases:
    asmith@example.com: alice@acme.com

developer_aliases:
  Alice Smith:
    - alice.smith@work.example

output:
  directory: /var/reports
  formats: [csv, json, markdown]
  include_merges: true

classification:
  rules_file: /etc/tga/rules.yaml
  repo_categories:
    infra-api: platform_infrastructure
  use_llm: true
  confidence_threshold: 0.6
  llm_fallback_threshold: 0.5
  llm_fallback_scope: unanswered
  llm_fallback_concurrency: 4
  no_external: true
  checkpoint_every: 100
  sources:
    - type: jira
      base_url: https://acme.atlassian.net
      project_keys: [ENG]
    - type: github_issues
      repo: acme/widgets

github:
  token: test-token-value
  org: acme
  orgs: [acme, acme-labs]
  fetch_prs: true
  fetch_pr_reviews: false
  review_fetch_concurrency: 2

bitbucket:
  workspace: acme-bb
  repo_slug: api
  fetch_prs: true

jira:
  url: https://acme.atlassian.net
  project_key: ENG
  jira_project_mappings:
    SEC: security
  jira_project_mapping_confidence: 0.9

linear:
  team_keys: [ENG, FE]
  fetch_on_reference: false

pm:
  azure_devops:
    organization_url: https://dev.azure.com/acme
    pat: test-pat-value
    projects: [Web]
    fetch_prs: true

dora:
  deployment_source: git_tags
  production_branch: release
  failure_signals:
    - work_type: bug_fix
      within_hours: 48

reachability:
  track_tags: false
  release_branch_patterns: ["rel/*"]

analysis:
  ml_categorization:
    enabled: true
    model: small

cache:
  directory: /var/cache/tga

llm:
  source: bedrock
  region: us-east-1
  model: anthropic.claude-sonnet
  effort: high

audit:
  window_weeks: 12
"#;

fn write_config(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(&path, body).expect("write config");
    path
}

#[test]
fn full_config_every_section_deserializes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let cfg = Config::load(&write_config(dir.path(), FULL_CONFIG)).expect("config loads");

    assert_eq!(cfg.version.as_deref(), Some("1.0"));
    assert_eq!(cfg.profile.as_deref(), Some("engineering"));
    assert_eq!(cfg.database, Some(PathBuf::from("/var/data/tga.db")));
    assert_eq!(cfg.aliases_file.as_deref(), Some("/etc/tga/aliases.yaml"));
    assert_eq!(cfg.fuzzy_identity_fallback, Some(false));

    let repo = &cfg.repositories[0];
    assert_eq!(repo.path, PathBuf::from("/src/my-app"));
    assert_eq!(repo.name.as_deref(), Some("my-app"));
    assert_eq!(repo.org.as_deref(), Some("acme"), "`owner` aliases `org`");
    assert!(repo.head_only);
    assert_eq!(repo.fetch_timeout_secs, Some(30));

    let team = cfg.team.as_ref().expect("team");
    assert_eq!(team.canonical_domain.as_deref(), Some("acme.com"));
    assert_eq!(team.members[0].email, "alice@acme.com");
    assert_eq!(team.members[0].aliases, ["alice@old.example"]);
    assert_eq!(cfg.developer_aliases["Alice Smith"].len(), 1);

    let output = cfg.output.as_ref().expect("output");
    assert_eq!(output.directory, Some(PathBuf::from("/var/reports")));
    assert_eq!(output.formats, ["csv", "json", "markdown"]);
    assert!(output.include_merges);

    let class = cfg.classification.as_ref().expect("classification");
    assert_eq!(class.rules_files, [PathBuf::from("/etc/tga/rules.yaml")]);
    assert_eq!(
        class.repo_categories["infra-api"],
        "platform_infrastructure"
    );
    assert!(class.use_llm);
    assert_eq!(class.confidence_threshold, 0.6);
    assert_eq!(class.llm_fallback_scope, LlmFallbackScope::Unanswered);
    assert_eq!(class.llm_fallback_concurrency, 4);
    assert!(class.no_external);
    assert_eq!(class.checkpoint_every, 100);
    assert_eq!(class.sources.len(), 2);
    let SourceConfig::Jira(jira_src) = &class.sources[0] else {
        panic!("first source is jira: {:?}", class.sources[0]);
    };
    assert_eq!(jira_src.base_url, "https://acme.atlassian.net");
    assert_eq!(jira_src.token_env, "JIRA_API_TOKEN", "omitted key defaults");
    assert_eq!(jira_src.project_keys, ["ENG"]);
    let SourceConfig::GithubIssues(gh_src) = &class.sources[1] else {
        panic!("second source is github_issues: {:?}", class.sources[1]);
    };
    assert_eq!(gh_src.repo, "acme/widgets");

    let github = cfg.github.as_ref().expect("github");
    assert_eq!(github.token.as_deref(), Some("test-token-value"));
    assert_eq!(github.orgs, ["acme", "acme-labs"]);
    assert!(github.fetch_prs);
    assert!(!github.fetch_pr_reviews);
    assert_eq!(github.review_fetch_concurrency, 2);

    let bitbucket = cfg.bitbucket.as_ref().expect("bitbucket");
    assert_eq!(bitbucket.workspace.as_deref(), Some("acme-bb"));
    assert!(bitbucket.fetch_prs);

    let jira = cfg.jira.as_ref().expect("jira");
    assert_eq!(jira.project_key.as_deref(), Some("ENG"));
    assert_eq!(jira.jira_project_mappings["SEC"], "security");
    assert_eq!(jira.jira_project_mapping_confidence, Some(0.9));

    let linear = cfg.linear.as_ref().expect("linear");
    assert_eq!(linear.team_keys, ["ENG", "FE"]);
    assert!(!linear.fetch_on_reference);

    let ado = cfg
        .pm
        .as_ref()
        .and_then(|pm| pm.azure_devops.as_ref())
        .expect("pm.azure_devops");
    assert_eq!(ado.organization_url, "https://dev.azure.com/acme");
    assert_eq!(ado.projects, ["Web"]);
    assert!(ado.fetch_prs);
    assert!(ado.fetch_on_reference, "omitted key defaults to true");

    let dora = cfg.dora.as_ref().expect("dora");
    assert_eq!(dora.production_branch, "release");
    assert_eq!(
        dora.failure_signals[0].work_type.as_deref(),
        Some("bug_fix")
    );
    assert_eq!(dora.failure_signals[0].within_hours, 48);

    assert!(!cfg.reachability.track_tags);
    assert!(
        cfg.reachability.track_release_branches,
        "omitted key defaults to true"
    );
    assert_eq!(cfg.reachability.release_branch_patterns, ["rel/*"]);

    let ml = cfg
        .analysis
        .as_ref()
        .and_then(|a| a.ml_categorization.as_ref())
        .expect("analysis.ml_categorization");
    assert!(ml.enabled);
    assert_eq!(ml.model.as_deref(), Some("small"));

    let cache = cfg.cache.as_ref().expect("cache");
    assert_eq!(cache.directory, Some(PathBuf::from("/var/cache/tga")));

    let llm = cfg.llm.as_ref().expect("llm");
    assert_eq!(llm.source, LlmSource::Bedrock);
    assert_eq!(llm.region.as_deref(), Some("us-east-1"));
    assert_eq!(llm.effort, Some(LlmEffort::High));

    assert_eq!(cfg.audit.as_ref().and_then(|a| a.window_weeks), Some(12));
}

/// The `configs/` directory at the crate root, where the shipped examples live.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("configs")
        .join(name)
}

#[test]
fn shipped_example_configs_still_load() {
    let example = Config::load(&fixture("example-config.yaml")).expect("example-config loads");
    assert_eq!(example.repositories.len(), 2);
    assert_eq!(example.repositories[0].name.as_deref(), Some("my-app"));
    assert!(example.github.as_ref().expect("github").fetch_prs);
    assert_eq!(
        example.linear.as_ref().expect("linear").team_keys,
        ["ENG", "FE"]
    );
    let dora = example.dora.as_ref().expect("dora");
    assert_eq!(dora.failure_signals.len(), 2);
    assert_eq!(
        example.jira.as_ref().expect("jira").jira_project_mappings["TQL"],
        "bug_fix"
    );

    let self_analysis = Config::load(&fixture("self-analysis.yaml")).expect("self-analysis loads");
    assert_eq!(
        self_analysis.repositories[0].name.as_deref(),
        Some("trusty-git-analytics")
    );
    assert_eq!(
        self_analysis.output.as_ref().expect("output").formats,
        ["csv", "json", "markdown"]
    );

    let aliases = AliasFile::load(&fixture("example-aliases.yaml")).expect("aliases load");
    let john = &aliases.developers[0];
    assert_eq!(john.name, "John Doe");
    assert_eq!(john.github_username.as_deref(), Some("jdoe"));
    assert_eq!(
        aliases.developers[1].confidence, 1.0,
        "omitted key defaults"
    );
}

//! #137: each public construction path equals the minimal YAML.
//!
//! Why: `#[non_exhaustive]` makes a constructor or `Default` the only way to
//! build these types outside the crate. Each one documents itself as "the
//! value an omitted YAML key deserializes to", so a constructor that drifts
//! from serde's defaults (say, `token_env: ""`) must fail here.
//! What: one table row per constructor or `Default` impl. A row builds the
//! value through the public path, parses the minimal YAML, and returns a
//! mismatch description. The test collects every mismatch before failing.
//! Test: this file IS the test.

use serde::de::DeserializeOwned;
use tga::{
    classify::{
        rules::{CategoryDef, Rule, RuleSet},
        sources::{
            ConfluenceSourceConfig, DatadogSourceConfig, GithubIssuesSourceConfig,
            JiraSourceConfig, LinearSourceConfig, ShortcutSourceConfig,
        },
        taxonomy::{SubcategoryDef, TopLevelCategory},
    },
    core::config::{
        AliasFile, AzureDevOpsConfig, DeveloperAliasEntry, FailureSignal, RepositoryConfig,
        TeamMember,
    },
};

/// `None` when they match, else a description of the mismatch.
type Row = (&'static str, fn() -> Option<String>);

fn parse<T: DeserializeOwned>(yaml: &str) -> T {
    serde_yaml::from_str(yaml).unwrap_or_else(|e| panic!("minimal YAML {yaml:?} parses: {e}"))
}

/// Compare with `PartialEq`.
fn eq<T: PartialEq + std::fmt::Debug>(built: T, parsed: T) -> Option<String> {
    (built != parsed).then(|| format!("built {built:?}\n  != yaml {parsed:?}"))
}

/// Compare derived `Debug` output, for types without `PartialEq`. A derived
/// `Debug` prints every field, so a field added later is still compared.
fn debug_eq<T: std::fmt::Debug>(built: T, parsed: T) -> Option<String> {
    let (b, p) = (format!("{built:?}"), format!("{parsed:?}"));
    (b != p).then(|| format!("built {b}\n  != yaml {p}"))
}

const ROWS: &[Row] = &[
    ("JiraSourceConfig::new", || {
        eq(
            JiraSourceConfig::new("https://acme.atlassian.net"),
            parse("base_url: https://acme.atlassian.net"),
        )
    }),
    ("GithubIssuesSourceConfig::new", || {
        eq(
            GithubIssuesSourceConfig::new("acme/widgets"),
            parse("repo: acme/widgets"),
        )
    }),
    ("ConfluenceSourceConfig::new", || {
        eq(
            ConfluenceSourceConfig::new("https://acme.atlassian.net/wiki"),
            parse("base_url: https://acme.atlassian.net/wiki"),
        )
    }),
    ("LinearSourceConfig::default", || {
        eq(LinearSourceConfig::default(), parse("{}"))
    }),
    ("ShortcutSourceConfig::default", || {
        eq(ShortcutSourceConfig::default(), parse("{}"))
    }),
    ("DatadogSourceConfig::default", || {
        eq(DatadogSourceConfig::default(), parse("{}"))
    }),
    ("CategoryDef::new", || {
        eq(CategoryDef::new("feature"), parse("name: feature"))
    }),
    ("SubcategoryDef::new", || {
        debug_eq(
            SubcategoryDef::new("payments", TopLevelCategory::Feature),
            parse("name: payments\nparent: feature"),
        )
    }),
    ("Rule::new", || {
        debug_eq(
            Rule::new("r1", "feature"),
            parse("id: r1\ncategory: feature"),
        )
    }),
    ("RuleSet::default", || {
        debug_eq(RuleSet::default(), parse("rules: []"))
    }),
    ("AliasFile::default", || {
        debug_eq(AliasFile::default(), parse("developers: []"))
    }),
    ("RepositoryConfig::new", || {
        debug_eq(RepositoryConfig::new("/src/app"), parse("path: /src/app"))
    }),
    ("TeamMember::new", || {
        debug_eq(
            TeamMember::new("Alice", "alice@acme.com"),
            parse("name: Alice\nemail: alice@acme.com"),
        )
    }),
    ("FailureSignal::default", || {
        debug_eq(FailureSignal::default(), parse("{}"))
    }),
    ("AzureDevOpsConfig::new", || {
        // Field by field: `Debug` is hand-written and redacts `pat` (#5770).
        let b = AzureDevOpsConfig::new("https://dev.azure.com/acme", "pat-value");
        let p: AzureDevOpsConfig =
            parse("organization_url: https://dev.azure.com/acme\npat: pat-value");
        let same = b.organization_url == p.organization_url
            && b.pat == p.pat
            && b.project == p.project
            && b.projects == p.projects
            && b.ticket_regex == p.ticket_regex
            && b.team_keys == p.team_keys
            && b.fetch_on_reference == p.fetch_on_reference
            && b.fetch_prs == p.fetch_prs;
        (!same).then(|| "AzureDevOpsConfig fields differ".to_string())
    }),
    ("DeveloperAliasEntry::new", || {
        let b = DeveloperAliasEntry::new("John Doe", "john@acme.com");
        let p: DeveloperAliasEntry = parse("name: John Doe\nprimary_email: john@acme.com");
        let same = b.name == p.name
            && b.primary_email == p.primary_email
            && b.aliases == p.aliases
            && b.github_username == p.github_username
            && b.confidence == p.confidence
            && b.reasoning == p.reasoning;
        (!same).then(|| format!("built {b:?}\n  != yaml {p:?}"))
    }),
];

#[test]
fn every_public_construction_path_matches_minimal_yaml() {
    let failures: Vec<String> = ROWS
        .iter()
        .filter_map(|(name, check)| check().map(|diff| format!("{name}: {diff}")))
        .collect();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

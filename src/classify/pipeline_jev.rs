//! The pipeline's Jev wiring: category set and names to pseudonymize (#111).
//!
//! Why: the Jev provider needs a category set for its choice question even
//! when the rules extend the built-ins, and the names it must hide come from
//! the tga config. Kept out of `pipeline.rs`, which sits at the size cap.
//! What: [`ClassificationPipeline::attach_jev_context`], [`known_names`],
//! and the run-time [`db_people`], [`db_trailer_people`],
//! [`db_repositories`] and [`prepare_jev`].
//! Test: `classify::tiers::jev_tests::outbound_body_carries_no_sensitive_string`.

use std::collections::{BTreeMap, BTreeSet};

use crate::classify::classifier::ClassificationEngine;
use crate::classify::errors::{ClassifyError, Result};
use crate::classify::pipeline::{configured_categories, ClassificationPipeline};
use crate::classify::rules::CategoryDef;
use crate::classify::tiers::jev_obfuscate::{KnownNames, RunNames};
use crate::classify::tiers::jev_trailers::trailer_names;
use crate::classify::tiers::llm::LlmClassifier;
use crate::classify::tiers::llm_context::CommitContext;
use crate::core::config::Config;
use crate::core::db::Database;

use super::pipeline_db::CommitRow;

/// Push `value` and, for an `owner/name` slug, each of its parts.
fn push_repo(out: &mut Vec<String>, value: &str) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    out.push(value.to_string());
    if value.contains('/') {
        let parts = value.split('/').map(str::trim).filter(|p| !p.is_empty());
        out.extend(parts.map(String::from));
    }
}

/// The tenant name in a hosted-service URL: the org of
/// `https://dev.azure.com/{org}`, or the first label of
/// `{org}.visualstudio.com` and `{site}.atlassian.net`.
fn tenant(url: &str) -> Option<&str> {
    let url = url.trim();
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = authority.rsplit('@').next()?;
    let lower = host.to_ascii_lowercase();
    if lower == "dev.azure.com" {
        return path.split('/').next().filter(|s| !s.is_empty());
    }
    let hosted = [".visualstudio.com", ".atlassian.net"]
        .iter()
        .any(|s| lower.ends_with(s));
    hosted.then(|| host.split('.').next()).flatten()
}

/// Repository, org and workspace names, plus roster names and logins, from
/// `config`. E-mail addresses are left to the address pattern.
///
/// What (#111, critic HIGH 2): `REPO_n` names come from
/// `repositories[]` (`name`, `org`, the path basename), `github.org`,
/// `github.orgs` and `github.repo`, `bitbucket.workspace`, `workspaces` and
/// `repo_slug`, the Azure DevOps organisation (from `organization_url`) and
/// projects, the Jira site name (from `jira.url`), and every
/// `classification.repo_categories` key that is not a glob. An
/// `owner/name` slug also gives each part.
/// Test: `jev_round4_tests::org_and_repo_names_from_every_source_are_redacted`.
pub(crate) fn known_names(config: &Config) -> KnownNames {
    let mut names = KnownNames::default();
    let repos = &mut names.repos;
    for r in &config.repositories {
        r.name.iter().for_each(|n| push_repo(repos, n));
        r.org.iter().for_each(|n| push_repo(repos, n));
        if let Some(base) = r.path.file_name().and_then(|b| b.to_str()) {
            push_repo(repos, base);
        }
    }
    if let Some(g) = &config.github {
        g.org
            .iter()
            .chain(&g.orgs)
            .chain(&g.repo)
            .for_each(|n| push_repo(repos, n));
    }
    if let Some(b) = &config.bitbucket {
        let one = b.workspace.iter().chain(&b.repo_slug);
        one.chain(&b.workspaces).for_each(|n| push_repo(repos, n));
    }
    if let Some(ado) = config.pm.as_ref().and_then(|p| p.azure_devops.as_ref()) {
        tenant(&ado.organization_url)
            .into_iter()
            .for_each(|n| push_repo(repos, n));
        ado.project
            .iter()
            .chain(&ado.projects)
            .for_each(|n| push_repo(repos, n));
    }
    if let Some(site) = config
        .jira
        .as_ref()
        .and_then(|j| j.url.as_deref())
        .and_then(tenant)
    {
        push_repo(repos, site);
    }
    if let Some(c) = &config.classification {
        let keys = c.repo_categories.keys().filter(|k| !k.contains('*'));
        keys.for_each(|n| push_repo(repos, n));
    }
    let mut person = |n: &str| {
        if !n.contains('@') {
            names.people.push(n.to_string());
        }
    };
    if let Some(team) = &config.team {
        for m in &team.members {
            person(&m.name);
            m.aliases.iter().for_each(|a| person(a));
        }
        for (alias, canonical) in &team.aliases {
            person(alias);
            person(canonical);
        }
    }
    for (canonical, aliases) in &config.developer_aliases {
        person(canonical);
        aliases.iter().for_each(|a| person(a));
    }
    names
}

/// Every person the database knows: authors, pull-request authors and
/// reviewers, and issue assignees.
///
/// Why (#111): a message can name anyone the project ever saw, not only the
/// authors of this run's commits; a reviewer's GitHub login can differ from
/// every git address. A recovery run (`--force --shas`) sends a
/// few commits whose text names people whose own commits were classified in
/// earlier runs; the pseudonymizer must know every one before the first
/// request. The schema stores no committer, so committer names are covered
/// only through trailers.
/// What: every `commits.author_name` and `author_email` local-part (and
/// each `+`-separated part of a GitHub no-reply local-part), every
/// `authors.canonical_name` / `canonical_email` local-part, and every entry
/// of `authors.aliases` (a name, or an address's local-part); every
/// `pull_requests.author`, `pr_reviewers.reviewer_id` and `display_name`,
/// `linear_issues.assignee`, `fact_ticket_transitions.author` (Jira
/// changelog authors), `fact_jira_comment_detail.author` (Jira comment
/// authors), `fact_pm_effort.pm_name` (reporters) and the `author_email`
/// local-parts of `fact_weekly_quality` and `fact_weekly_engineer`.
/// `work_items` stores no assignee column. Deduplicated,
/// sorted. A malformed `aliases` value is an error, never skipped.
/// Test: `jev_tests::db_author_names_are_known_before_the_first_request`,
/// `jev_review_tests::names_from_earlier_runs_are_redacted`,
/// `jev_round3_tests::pr_and_issue_people_are_redacted`,
/// `jev_gateb_tests::jira_comment_and_reporter_names_are_redacted`.
///
/// # Errors
///
/// A database read fails, or an `aliases` value is not a JSON string list.
pub(super) fn db_people(db: &Database) -> Result<Vec<String>> {
    fn add(value: &str, out: &mut BTreeSet<String>) {
        match value.split_once('@') {
            Some((local, _)) => {
                out.insert(local.to_string());
                for p in local
                    .split('+')
                    .filter(|p| !p.bytes().all(|b| b.is_ascii_digit()))
                {
                    out.insert(p.to_string());
                }
            }
            None => {
                out.insert(value.to_string());
            }
        }
    }
    let conn = db.connection();
    let mut people = BTreeSet::new();
    let mut stmt = conn
        .prepare("SELECT DISTINCT author_name, author_email FROM commits")
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(crate::core::TgaError::from)?;
    for row in rows {
        let (name, email) = row.map_err(crate::core::TgaError::from)?;
        add(&name, &mut people);
        add(&email, &mut people);
    }
    let mut stmt = conn
        .prepare("SELECT canonical_name, canonical_email, aliases FROM authors")
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(crate::core::TgaError::from)?;
    for row in rows {
        let (name, email, aliases) = row.map_err(crate::core::TgaError::from)?;
        add(&name, &mut people);
        add(&email, &mut people);
        let aliases: Vec<String> = serde_json::from_str(&aliases)?;
        for a in &aliases {
            add(a, &mut people);
        }
    }
    // #111 (review round 3, gate B): people who never authored a commit.
    const OTHER_PEOPLE: [&str; 9] = [
        "SELECT author FROM pull_requests",
        "SELECT reviewer_id FROM pr_reviewers",
        "SELECT display_name FROM pr_reviewers",
        "SELECT assignee FROM linear_issues",
        "SELECT author FROM fact_ticket_transitions",
        "SELECT author FROM fact_jira_comment_detail",
        "SELECT pm_name FROM fact_pm_effort",
        "SELECT author_email FROM fact_weekly_quality",
        "SELECT author_email FROM fact_weekly_engineer",
    ];
    for sql in OTHER_PEOPLE {
        let mut stmt = conn.prepare(sql).map_err(crate::core::TgaError::from)?;
        let rows = stmt
            .query_map([], |r| r.get::<_, Option<String>>(0))
            .map_err(crate::core::TgaError::from)?;
        for row in rows {
            if let Some(v) = row.map_err(crate::core::TgaError::from)? {
                add(&v, &mut people);
            }
        }
    }
    Ok(people
        .into_iter()
        .filter(|p| !p.trim().is_empty())
        .collect())
}

/// Every distinct repository file path the database records
/// (`files.path`, #111 gate B re-run); the Jev pseudonymizer learns their
/// basenames.
///
/// # Errors
///
/// A database read fails.
pub(super) fn db_file_paths(db: &Database) -> Result<Vec<String>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT DISTINCT path FROM files")
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(crate::core::TgaError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(crate::core::TgaError::from)?);
    }
    Ok(out)
}

/// Every identity-trailer name in every commit message the database stores.
///
/// Why (#111, critic HIGH 1): a run classifies only some commits (the
/// unclassified ones by default, fewer under `--since`, `--repos` or
/// `--shas`), so a person named only in a trailer of an older, classified
/// commit (`Reviewers: jdoe`) was unknown when a new message named them.
/// What: streams `commits.message`, skipping messages with no ASCII `:` in
/// SQL (a trailer line needs one), and collects each trailer name the
/// pseudonymizer itself learns from a message. One pass: 0.25 s for
/// 300,000 synthetic commits in a release build, 3.5 s in a debug build
/// (`docs/requirements/configuration.md`).
/// Test: `jev_round4_tests::trailer_names_from_stored_commits_are_redacted`.
///
/// # Errors
///
/// A database read fails, or a built-in trailer pattern does not compile.
pub(super) fn db_trailer_people(db: &Database) -> Result<Vec<String>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT message FROM commits WHERE instr(message, ':') > 0")
        .map_err(crate::core::TgaError::from)?;
    let mut rows = stmt.query([]).map_err(crate::core::TgaError::from)?;
    let mut names = BTreeSet::new();
    while let Some(row) = rows.next().map_err(crate::core::TgaError::from)? {
        let message: String = row.get(0).map_err(crate::core::TgaError::from)?;
        names.extend(trailer_names([message.as_str()]).map_err(jev_init)?);
    }
    Ok(names.into_iter().collect())
}

/// Every distinct repository name in `commits` and `pull_requests` (#111,
/// critic HIGH 2); the Jev pseudonymizer learns them as `REPO_n`.
///
/// # Errors
///
/// A database read fails.
pub(super) fn db_repositories(db: &Database) -> Result<Vec<String>> {
    let conn = db.connection();
    let mut stmt = conn
        .prepare("SELECT repository FROM commits UNION SELECT repository FROM pull_requests")
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |r| r.get::<_, Option<String>>(0))
        .map_err(crate::core::TgaError::from)?;
    let mut out = Vec::new();
    for row in rows {
        out.extend(row.map_err(crate::core::TgaError::from)?);
    }
    Ok(out)
}

fn jev_init(e: impl std::fmt::Display) -> ClassifyError {
    ClassifyError::Config(format!("LLM provider init failed (jev): {e}"))
}

/// Give a Jev tier every name the database records and every message of the
/// run before the first request (#111); a no-op for any other tier, and for
/// Jev without `llm.jev.obfuscate` (owner ruling 2026-10-06: no scan runs).
///
/// What: people ([`db_people`] plus [`db_trailer_people`]), file paths
/// ([`db_file_paths`]) and repositories ([`db_repositories`]).
/// Test: `jev_text_mode_tests::default_config_scans_no_names`.
///
/// # Errors
///
/// A database read fails, or the name matcher cannot be built.
pub(super) fn prepare_jev(
    engine: &ClassificationEngine,
    db: &Database,
    commits: &[CommitRow],
    contexts: &BTreeMap<usize, CommitContext>,
) -> Result<()> {
    if !engine.llm_jev_obfuscates() {
        return Ok(());
    }
    let mut people = db_people(db)?;
    people.extend(db_trailer_people(db)?);
    let names = RunNames {
        people,
        paths: db_file_paths(db)?,
        repos: db_repositories(db)?,
    };
    let messages: Vec<&str> = commits.iter().map(|c| c.message.as_str()).collect();
    // #111: `llm.context` blocks too, in commit order (the map's key order).
    let contexts: Vec<&CommitContext> = contexts.values().collect();
    engine
        .llm_prepare(&messages, &contexts, &names)
        .map_err(jev_init)
}

impl ClassificationPipeline {
    /// Give a Jev classifier its category set and the config's names; any
    /// other classifier passes through unchanged.
    ///
    /// What: the category set is [`Self::llm_categories`] when the rules
    /// define the whole set, else every category the loaded rules can emit.
    /// When the consumer supplies the bucket map (`classification.buckets`
    /// or the rules file's `buckets:`), every fine category of it not
    /// already in the set follows, in map order (#111). tga's built-in
    /// fallback map adds none: its categories reach Jev only through the
    /// rules, as they reach the other LLM sources (owner ruling 2026-10-06).
    /// Test: `jev_tests::jev_choices_cover_every_bucket_category`.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, the bucket map names an unknown
    /// category, or the category set is unusable for Jev.
    pub(super) fn attach_jev_context(&self, llm: LlmClassifier) -> Result<LlmClassifier> {
        if !llm.is_jev() {
            return Ok(llm);
        }
        let (ruleset, _) = self.load_ruleset()?;
        // #111 (gate B): rule keywords and patterns are classification
        // vocabulary, never a learned name. #111: no names to learn without
        // `llm.jev.obfuscate`.
        let mut names = if llm.jev_obfuscates() {
            known_names(&self.config)
        } else {
            KnownNames::default()
        };
        for r in &ruleset.rules {
            names.vocab.push(r.category.clone());
            names.vocab.extend(r.keywords.iter().cloned());
            names.vocab.extend(r.patterns.iter().cloned());
        }
        let mut categories = match self.llm_categories()? {
            Some(c) => c,
            None => configured_categories(ruleset),
        };
        // #111: the one Jev question offers every fine category in a
        // consumer-supplied bucket map, so each arm can land in every bucket.
        let (map, source) = self.bucket_map_with_source()?;
        let offered = map
            .fine_categories()
            .filter(|_| source.is_consumer_supplied());
        for fine in offered {
            if !categories.iter().any(|c| c.name.eq_ignore_ascii_case(fine)) {
                names.vocab.push(fine.to_string());
                categories.push(CategoryDef::new(fine));
            }
        }
        llm.with_jev_context(categories, names)
            .map_err(|e| ClassifyError::Config(format!("LLM provider init failed (jev): {e}")))
    }
}

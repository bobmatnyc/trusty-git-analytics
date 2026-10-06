//! The pipeline's Jev wiring: category set and names to pseudonymize (#111).
//!
//! Why: the Jev provider needs a category set for its choice question even
//! when the rules extend the built-ins, and the names it must hide come from
//! the tga config. Kept out of `pipeline.rs`, which sits at the size cap.
//! What: [`ClassificationPipeline::attach_jev_context`], [`known_names`],
//! and the run-time [`db_people`] / [`prepare_jev`].
//! Test: `classify::tiers::jev_tests::outbound_body_carries_no_sensitive_string`.

use std::collections::BTreeSet;

use crate::classify::classifier::ClassificationEngine;
use crate::classify::errors::{ClassifyError, Result};
use crate::classify::pipeline::{configured_categories, ClassificationPipeline};
use crate::classify::tiers::jev_obfuscate::KnownNames;
use crate::classify::tiers::llm::LlmClassifier;
use crate::core::config::Config;
use crate::core::db::Database;

use super::pipeline_db::CommitRow;

/// Repository names, basenames and owners, plus roster names and logins,
/// from `config`. E-mail addresses are left to the address pattern.
pub(crate) fn known_names(config: &Config) -> KnownNames {
    let mut names = KnownNames::default();
    for r in &config.repositories {
        names.repos.extend(r.name.iter().cloned());
        names.repos.extend(r.org.iter().cloned());
        if let Some(base) = r.path.file_name().and_then(|b| b.to_str()) {
            names.repos.push(base.to_string());
        }
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

/// Give a Jev tier every author name in the database and every message of the run
/// before the first request (#111); a no-op for any other tier.
///
/// # Errors
///
/// A database read fails, or the name matcher cannot be built.
pub(super) fn prepare_jev(
    engine: &ClassificationEngine,
    db: &Database,
    commits: &[CommitRow],
) -> Result<()> {
    if !engine.llm_is_jev() {
        return Ok(());
    }
    let people = db_people(db)?;
    let paths = db_file_paths(db)?;
    let messages: Vec<&str> = commits.iter().map(|c| c.message.as_str()).collect();
    engine
        .llm_prepare(&messages, &people, &paths)
        .map_err(|e| ClassifyError::Config(format!("LLM provider init failed (jev): {e}")))
}

impl ClassificationPipeline {
    /// Give a Jev classifier its category set and the config's names; any
    /// other classifier passes through unchanged.
    ///
    /// What: the category set is [`Self::llm_categories`] when the rules
    /// define the whole set, else every category the loaded rules can emit.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, or the category set is unusable for Jev.
    pub(super) fn attach_jev_context(&self, llm: LlmClassifier) -> Result<LlmClassifier> {
        if !llm.is_jev() {
            return Ok(llm);
        }
        let (ruleset, _) = self.load_ruleset()?;
        // #111 (gate B): rule keywords and patterns are classification
        // vocabulary, never a learned name.
        let mut names = known_names(&self.config);
        for r in &ruleset.rules {
            names.vocab.push(r.category.clone());
            names.vocab.extend(r.keywords.iter().cloned());
            names.vocab.extend(r.patterns.iter().cloned());
        }
        let categories = match self.llm_categories()? {
            Some(c) => c,
            None => configured_categories(ruleset),
        };
        llm.with_jev_context(categories, names)
            .map_err(|e| ClassifyError::Config(format!("LLM provider init failed (jev): {e}")))
    }
}

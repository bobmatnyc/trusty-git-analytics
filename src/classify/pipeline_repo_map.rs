//! `classification.repo_categories`: the repo → category hard override (#158).
//!
//! Why (owner ruling 2026-10-07): some repositories map to one category for
//! every commit. The consumer declares that map in config, and it outranks
//! every tier: the manual override, the rules, external sources and the LLM.
//! What: [`RepoCategoryMap`] holds the checked map and builds the verdict of
//! a mapped commit; [`ClassificationPipeline::repo_category_map`] reads and
//! checks it from config. A key is the repository name tga stores in
//! `commits.repository` (`repositories[].name`, else the path basename),
//! matched whole and case-sensitively. A merge commit is never mapped.
//! Test: `classify::pipeline_repo_map_tests`,
//! `eval::verdict::tests::a_mapped_repo_resolves_to_the_repo_map_tier`.

use std::collections::{BTreeMap, HashMap};

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::pipeline::ClassificationPipeline;
use crate::classify::taxonomy::TaxonomyRegistry;
use crate::classify::tiers::regex_tier::RegexMatcher;
use crate::classify::tiers::ClassificationResult;
use crate::classify::trace::{RuleTrace, TraceTier, TracedVerdict};
use crate::core::models::ClassificationMethod;

use super::pipeline_db::CommitRow;

/// The checked `classification.repo_categories` map.
///
/// Invariant: every value is a category the config knows, in the spelling
/// the config gives it, and no key contains `*`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoCategoryMap {
    map: HashMap<String, String>,
}

impl RepoCategoryMap {
    /// Check `raw` against the category set `known`.
    ///
    /// What: a category matches a known name case-insensitively and is
    /// stored in the known spelling. A key holding `*` is refused: keys are
    /// exact repository names.
    ///
    /// # Errors
    ///
    /// [`ClassifyError::Config`] naming every glob key, or every repository
    /// whose category `known` does not hold, with that category.
    pub(crate) fn checked(raw: &HashMap<String, String>, known: &[String]) -> Result<Self> {
        let sorted: BTreeMap<&String, &String> = raw.iter().collect();
        let globs: Vec<&str> = sorted
            .keys()
            .filter(|k| k.contains('*'))
            .map(|k| k.as_str())
            .collect();
        if !globs.is_empty() {
            return Err(ClassifyError::Config(format!(
                "classification.repo_categories keys are exact repository names, not globs: {}",
                globs.join(", ")
            )));
        }
        let mut map = HashMap::with_capacity(raw.len());
        let mut unknown = Vec::new();
        for (repo, category) in sorted {
            match known.iter().find(|k| k.eq_ignore_ascii_case(category)) {
                Some(name) => {
                    map.insert(repo.clone(), name.clone());
                }
                None => unknown.push(format!("repository '{repo}' -> category '{category}'")),
            }
        }
        if !unknown.is_empty() {
            return Err(ClassifyError::Config(format!(
                "classification.repo_categories names categories this config does not know: {}",
                unknown.join(", ")
            )));
        }
        Ok(Self { map })
    }

    /// The repo map's verdict for one commit, or `None` when the repo is not
    /// mapped or the commit is a merge.
    ///
    /// What: the mapped category at confidence 1.0 with method
    /// [`ClassificationMethod::RepoMap`], the ticket id from the message, and
    /// the trace `repo_map:<repo>`.
    pub(crate) fn traced(
        &self,
        repo: &str,
        is_merge: bool,
        message: &str,
        taxonomy: &TaxonomyRegistry,
    ) -> Option<TracedVerdict> {
        // #158: merges keep their existing handling.
        if is_merge {
            return None;
        }
        let category = self.map.get(repo)?;
        let verdict = ClassificationResult {
            top_level: taxonomy.resolve(category),
            category: category.clone(),
            subcategory: None,
            confidence: 1.0,
            method: ClassificationMethod::RepoMap,
            ticket_id: RegexMatcher::extract_ticket_id(message),
            complexity: None,
        };
        let trace = RuleTrace::new(TraceTier::RepoMap, format!("repo_map:{repo}"));
        Some(TracedVerdict { verdict, trace })
    }

    /// Verdicts of the mapped commits in `commits`, keyed by commit row id.
    pub(super) fn verdicts(
        &self,
        commits: &[CommitRow],
        taxonomy: &TaxonomyRegistry,
    ) -> HashMap<i64, ClassificationResult> {
        if self.map.is_empty() {
            return HashMap::new();
        }
        commits
            .iter()
            .filter_map(|c| {
                self.traced(&c.repository, c.is_merge, &c.message, taxonomy)
                    .map(|t| (c.id, t.verdict))
            })
            .collect()
    }
}

impl ClassificationPipeline {
    /// The checked `classification.repo_categories` map (#158).
    ///
    /// Why: a misspelt category would put a whole repository in a category
    /// no report or eval knows, so it fails before any write.
    /// What: empty when the key is absent or empty. Otherwise checks it with
    /// [`RepoCategoryMap::checked`] against the configured category set: the
    /// rules files' categories when they define the whole set
    /// (`extend_defaults: false`, the set the LLM tier is restricted to),
    /// else the taxonomy plus every rule category.
    /// Test: `pipeline_repo_map_tests::an_unknown_mapped_category_is_rejected_before_any_write`,
    /// `pipeline_repo_map_tests::a_glob_key_is_rejected`.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, or the map fails the check.
    pub(crate) fn repo_category_map(&self) -> Result<RepoCategoryMap> {
        let raw = match self.config.classification.as_ref() {
            Some(c) if !c.repo_categories.is_empty() => &c.repo_categories,
            _ => return Ok(RepoCategoryMap::default()),
        };
        let known = match self.llm_categories()? {
            Some(defs) => defs.into_iter().map(|d| d.name).collect(),
            None => self.known_categories()?,
        };
        RepoCategoryMap::checked(raw, &known)
    }
}

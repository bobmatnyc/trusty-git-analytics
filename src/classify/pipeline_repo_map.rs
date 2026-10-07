//! `classification.repo_categories`: the repo → category map (#158, #167).
//!
//! Why (owner rulings 2026-10-07): some repositories map to one category for
//! every commit. #158 made the map a hard override of every tier. #167 adds
//! floor mode ("the repo map should be the floor, with qa and bugfix etc
//! inferred") and `<repo>:<prefix>` keys for the monorepo.
//! What: [`RepoCategoryMap`] holds the checked map and the mode, resolves a
//! commit to its mapped verdict, applies it as an override or a floor, and
//! warns about keys that match nothing stored;
//! [`ClassificationPipeline::repo_category_map`] reads and checks it from
//! config. A repository is the name tga stores in `commits.repository`
//! (`repositories[].name`, else the path basename), matched whole and
//! case-sensitively. A merge commit is never mapped.
//! Test: `classify::pipeline_repo_map_tests`,
//! `classify::pipeline_repo_map_floor_tests`,
//! `eval::verdict::tests::a_mapped_repo_resolves_to_the_repo_map_tier`,
//! `eval::verdict::tests::floor_mode_keeps_an_exception_and_floors_the_rest`.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{params, Connection};
use tracing::warn;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::pipeline::ClassificationPipeline;
use crate::classify::repo_map_keys::{parse_key, MapKey, RepoKeys};
use crate::classify::taxonomy::TaxonomyRegistry;
use crate::classify::tiers::regex_tier::RegexMatcher;
use crate::classify::tiers::ClassificationResult;
use crate::classify::trace::{RuleTrace, TraceTier, TracedVerdict};
use crate::core::config::{RepoMapConfig, RepoMapMode};
use crate::core::models::ClassificationMethod;

use super::pipeline_db::CommitRow;

/// Floor mode's exception rule (#167).
#[derive(Debug, Clone)]
struct Floor {
    exceptions: Vec<String>,
    min_confidence: f64,
}

/// The checked `classification.repo_categories` map and its mode.
///
/// Invariant: every category is one the config knows, in the config's
/// spelling; no key holds `*`; no two keys name the same repository and
/// prefix; a floor's `min_confidence` is within `[0, 1]`.
#[derive(Debug, Clone, Default)]
pub(crate) struct RepoCategoryMap {
    repos: HashMap<String, RepoKeys>,
    /// Raw key → parsed key, for the unmatched-key warning.
    keys: BTreeMap<String, MapKey>,
    /// `None` in override mode.
    floor: Option<Floor>,
}

/// Prefix of every map error, naming the config key.
const KEY: &str = "classification.repo_categories";

impl RepoCategoryMap {
    /// Check `raw` and `settings` against the category set `known`.
    ///
    /// What: a key is `<repo>` or `<repo>:<prefix>` ([`parse_key`]). A
    /// category matches a known name case-insensitively and is stored in the
    /// known spelling. In floor mode an exception `known` does not hold is
    /// warned about, not refused: the default list names categories a
    /// built-in taxonomy may lack.
    ///
    /// # Errors
    ///
    /// [`ClassifyError::Config`] naming every glob key, every malformed key,
    /// every pair of keys naming the same repository and prefix, every
    /// repository whose category `known` does not hold, or a
    /// `min_confidence` outside `[0, 1]`.
    pub(crate) fn checked(
        raw: &HashMap<String, String>,
        settings: &RepoMapConfig,
        known: &[String],
    ) -> Result<Self> {
        let sorted: BTreeMap<&String, &String> = raw.iter().collect();
        let globs: Vec<&str> = sorted
            .keys()
            .filter(|k| k.contains('*'))
            .map(|k| k.as_str())
            .collect();
        if !globs.is_empty() {
            return Err(ClassifyError::Config(format!(
                "{KEY} keys are exact repository names, not globs: {}",
                globs.join(", ")
            )));
        }
        // #167: `<repo>` or `<repo>:<prefix>`; anything else can never match.
        let mut keys = BTreeMap::new();
        let mut malformed = Vec::new();
        let mut seen: BTreeMap<MapKey, &str> = BTreeMap::new();
        let mut duplicates = Vec::new();
        for raw_key in sorted.keys() {
            match parse_key(raw_key) {
                Ok(key) => {
                    if let Some(first) = seen.insert(key.clone(), raw_key.as_str()) {
                        duplicates.push(format!("'{first}' and '{raw_key}'"));
                    }
                    keys.insert(raw_key.to_string(), key);
                }
                Err(why) => malformed.push(format!("'{raw_key}' ({why})")),
            }
        }
        if !malformed.is_empty() {
            return Err(ClassifyError::Config(format!(
                "{KEY} keys must be '<repo>' or '<repo>:<prefix>': {}",
                malformed.join(", ")
            )));
        }
        if !duplicates.is_empty() {
            return Err(ClassifyError::Config(format!(
                "{KEY} keys name the same repository and prefix: {}",
                duplicates.join(", ")
            )));
        }
        let mut repos: HashMap<String, RepoKeys> = HashMap::new();
        let mut unknown = Vec::new();
        for (raw_key, category) in sorted {
            match known.iter().find(|k| k.eq_ignore_ascii_case(category)) {
                Some(name) => {
                    let key = &keys[raw_key.as_str()];
                    repos
                        .entry(key.repo.clone())
                        .or_default()
                        .insert(key.prefix.clone(), name.clone());
                }
                None => unknown.push(format!("repository '{raw_key}' -> category '{category}'")),
            }
        }
        if !unknown.is_empty() {
            return Err(ClassifyError::Config(format!(
                "{KEY} names categories this config does not know: {}",
                unknown.join(", ")
            )));
        }
        Ok(Self {
            repos,
            keys,
            floor: Self::checked_floor(settings, known)?,
        })
    }

    /// The floor of `settings`, or `None` in override mode.
    fn checked_floor(settings: &RepoMapConfig, known: &[String]) -> Result<Option<Floor>> {
        if settings.mode != RepoMapMode::Floor {
            return Ok(None);
        }
        let min = settings.min_confidence;
        if !(0.0..=1.0).contains(&min) {
            return Err(ClassifyError::Config(format!(
                "classification.repo_map.min_confidence must be within [0, 1], got {min}"
            )));
        }
        for e in &settings.exceptions {
            if !known.iter().any(|k| k.eq_ignore_ascii_case(e)) {
                warn!(
                    "classification.repo_map.exceptions names '{e}', a category this config \
                     does not know; no verdict can match it"
                );
            }
        }
        Ok(Some(Floor {
            exceptions: settings.exceptions.clone(),
            min_confidence: min,
        }))
    }

    /// Whether the map is in floor mode (#167).
    pub(crate) fn is_floor(&self) -> bool {
        self.floor.is_some()
    }

    /// Whether floor mode keeps a cascade verdict of `category` at
    /// `confidence`: an exception, case-insensitively, at or above
    /// `min_confidence`. Always false in override mode.
    pub(crate) fn keeps(&self, category: &str, confidence: f64) -> bool {
        self.floor.as_ref().is_some_and(|f| {
            confidence >= f.min_confidence
                && f.exceptions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(category))
        })
    }

    /// Changed paths of the commits in `commits` (`(id, repository)`) whose
    /// repository has a prefix key; no read when none has.
    ///
    /// # Errors
    ///
    /// A database read fails.
    pub(crate) fn load_paths<'a>(
        &self,
        conn: &Connection,
        commits: impl IntoIterator<Item = (i64, &'a str)>,
    ) -> Result<HashMap<i64, Vec<String>>> {
        let ids: Vec<i64> = commits
            .into_iter()
            .filter(|(_, repo)| self.repos.get(*repo).is_some_and(RepoKeys::has_prefixes))
            .map(|(id, _)| id)
            .collect();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        crate::core::db::commit_context::load_paths(conn, &ids)
            .map_err(|e| crate::core::TgaError::from(e).into())
    }

    /// The repo map's verdict for one commit, or `None` when no key maps it
    /// or the commit is a merge.
    ///
    /// What: the category [`RepoKeys::resolve`] picks from `paths`, at
    /// confidence 1.0 with method [`ClassificationMethod::RepoMap`], the
    /// ticket id from the message, and the trace `repo_map:<key>` naming
    /// the deciding key.
    pub(crate) fn traced(
        &self,
        repo: &str,
        is_merge: bool,
        paths: &[String],
        message: &str,
        taxonomy: &TaxonomyRegistry,
    ) -> Option<TracedVerdict> {
        // #158: merges keep their existing handling.
        if is_merge {
            return None;
        }
        let (prefix, category) = self.repos.get(repo)?.resolve(paths)?;
        let verdict = ClassificationResult {
            top_level: taxonomy.resolve(category),
            category: category.to_string(),
            subcategory: None,
            confidence: 1.0,
            method: ClassificationMethod::RepoMap,
            ticket_id: RegexMatcher::extract_ticket_id(message),
            complexity: None,
        };
        let rule_id = match prefix {
            None => format!("repo_map:{repo}"),
            Some(p) => format!("repo_map:{repo}:{p}"),
        };
        let trace = RuleTrace::new(TraceTier::RepoMap, rule_id);
        Some(TracedVerdict { verdict, trace })
    }

    fn verdict_of(
        &self,
        c: &CommitRow,
        paths: &HashMap<i64, Vec<String>>,
        taxonomy: &TaxonomyRegistry,
    ) -> Option<ClassificationResult> {
        let p = paths.get(&c.id).map_or(&[][..], Vec::as_slice);
        self.traced(&c.repository, c.is_merge, p, &c.message, taxonomy)
            .map(|t| t.verdict)
    }

    /// Override mode: the verdicts of the mapped commits, keyed by commit
    /// row id, to apply ahead of every tier. Empty in floor mode.
    pub(super) fn verdicts(
        &self,
        commits: &[CommitRow],
        paths: &HashMap<i64, Vec<String>>,
        taxonomy: &TaxonomyRegistry,
    ) -> HashMap<i64, ClassificationResult> {
        if self.repos.is_empty() || self.is_floor() {
            return HashMap::new();
        }
        commits
            .iter()
            .filter_map(|c| self.verdict_of(c, paths, taxonomy).map(|v| (c.id, v)))
            .collect()
    }

    /// Floor mode (#167): replace each mapped commit's cascade verdict in
    /// `results` (index-aligned with `commits`) with the mapped one unless
    /// [`Self::keeps`] it. No-op in override mode.
    pub(super) fn apply_floor(
        &self,
        commits: &[CommitRow],
        paths: &HashMap<i64, Vec<String>>,
        results: &mut [ClassificationResult],
        taxonomy: &TaxonomyRegistry,
    ) {
        if !self.is_floor() {
            return;
        }
        for (c, r) in commits.iter().zip(results.iter_mut()) {
            if self.keeps(&r.category, r.confidence) {
                continue;
            }
            if let Some(v) = self.verdict_of(c, paths, taxonomy) {
                *r = v;
            }
        }
    }

    /// Warn once per key that matches nothing stored (#167).
    ///
    /// Why: 10.2.0 ignored a key naming no stored repository silently, so a
    /// misspelt or retired repository mapped nothing without a word.
    /// What: a key whose repository is not in `commits.repository`, or a
    /// prefix key whose repository holds no stored path under the prefix,
    /// gets one `warn!` naming the key. Call once per run.
    /// Test: `pipeline_repo_map_floor_tests::an_unmatched_key_warns_once_per_run`.
    ///
    /// # Errors
    ///
    /// A database read fails.
    pub(crate) fn warn_unmatched_keys(&self, conn: &Connection) -> Result<()> {
        if self.keys.is_empty() {
            return Ok(());
        }
        let db = |e: rusqlite::Error| ClassifyError::from(crate::core::TgaError::from(e));
        let mut stmt = conn
            .prepare("SELECT DISTINCT repository FROM commits")
            .map_err(db)?;
        let stored: HashSet<String> = stmt
            .query_map([], |r| r.get(0))
            .map_err(db)?
            .collect::<std::result::Result<_, _>>()
            .map_err(db)?;
        for (raw, key) in &self.keys {
            if !stored.contains(&key.repo) {
                warn!("{KEY} key '{raw}' matches no stored repository");
                continue;
            }
            let Some(prefix) = &key.prefix else { continue };
            let under = format!("{prefix}/");
            let hit: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM files f JOIN commits c ON c.id = f.commit_id \
                     WHERE c.repository = ?1 AND (f.path = ?2 OR substr(f.path, 1, ?3) = ?4))",
                    params![key.repo, prefix, under.chars().count() as i64, under],
                    |r| r.get(0),
                )
                .map_err(db)?;
            if !hit {
                warn!(
                    "{KEY} key '{raw}' matches no stored path under '{prefix}' in repository \
                     '{}'",
                    key.repo
                );
            }
        }
        Ok(())
    }
}

impl ClassificationPipeline {
    /// The checked `classification.repo_categories` map (#158) with its
    /// `classification.repo_map` mode (#167).
    ///
    /// Why: a misspelt category would put a whole repository in a category
    /// no report or eval knows, so it fails before any write.
    /// What: empty when the key is absent or empty. Otherwise checks it with
    /// [`RepoCategoryMap::checked`] against the configured category set: the
    /// rules files' categories when they define the whole set
    /// (`extend_defaults: false`, the set the LLM tier is restricted to),
    /// else the taxonomy plus every rule category.
    /// Test: `pipeline_repo_map_tests::an_unknown_mapped_category_is_rejected_before_any_write`,
    /// `pipeline_repo_map_tests::a_glob_key_is_rejected`,
    /// `pipeline_repo_map_floor_tests::a_malformed_key_is_rejected`.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, or the map fails the check.
    pub(crate) fn repo_category_map(&self) -> Result<RepoCategoryMap> {
        let (raw, settings) = match self.config.classification.as_ref() {
            Some(c) if !c.repo_categories.is_empty() => (&c.repo_categories, &c.repo_map),
            _ => return Ok(RepoCategoryMap::default()),
        };
        let known = match self.llm_categories()? {
            Some(defs) => defs.into_iter().map(|d| d.name).collect(),
            None => self.known_categories()?,
        };
        RepoCategoryMap::checked(raw, settings, &known)
    }
}

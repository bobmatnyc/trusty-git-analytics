//! End-to-end classification pipeline: read DB → classify → write back.

use std::collections::HashMap;

use rusqlite::params;
use tracing::{info, warn};

use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::errors::Result;
use crate::classify::rules::default_rules;
use crate::classify::sources::ExternalSourceResolver;
use crate::classify::tiers::bedrock::DEFAULT_BEDROCK_MODEL;
use crate::classify::tiers::llm::ANTHROPIC_DEFAULT_MODEL;
use crate::classify::tiers::ClassificationResult;
use crate::classify::trace::RuleSources;
use crate::core::config::{Config, LlmSource};
use crate::core::db::Database;
use crate::core::models::ClassificationMethod;

/// Default minimum coverage threshold (percent) below which the pipeline
/// emits a warning. Used when no config-level override is supplied.
#[allow(dead_code)]
const DEFAULT_MIN_COVERAGE_PCT: f64 = 20.0;

/// Rule categories (deduplicated, rule order) followed by any `categories:`
/// entry no rule names, with the entries' descriptions attached (#131).
fn configured_categories(
    ruleset: crate::classify::rules::RuleSet,
) -> Vec<crate::classify::rules::CategoryDef> {
    use crate::classify::rules::CategoryDef;
    let mut out: Vec<CategoryDef> = Vec::new();
    for rule in ruleset.rules {
        if !out.iter().any(|c| c.name == rule.category) {
            out.push(CategoryDef {
                name: rule.category,
                description: None,
            });
        }
    }
    for def in ruleset.categories {
        match out.iter_mut().find(|c| c.name == def.name) {
            Some(existing) => existing.description = def.description,
            None => out.push(def),
        }
    }
    out
}

/// Aggregate statistics from a single pipeline run.
///
/// Why: callers (CLI, tests) need a uniform shape describing how many
/// commits were classified and via which tier; coverage breakdowns let
/// reports surface gaps per repository.
/// What: counters per-tier (`by_method`), per-category (`by_category`),
/// and per-repo coverage. Populated by [`ClassificationPipeline::run`].
/// Test: covered by `tests::pipeline_runs_against_in_memory_db` and
/// `pipeline_force_reclassifies_rows`.
#[derive(Debug, Clone, Default)]
pub struct ClassificationStats {
    /// Total commits processed.
    pub total_commits: usize,
    /// Commits that received a non-uncategorized verdict.
    pub classified: usize,
    /// Count of verdicts per tier (`"exact_rule"`, `"regex_rule"`, ...).
    pub by_method: HashMap<String, usize>,
    /// Count of verdicts per category.
    pub by_category: HashMap<String, usize>,
    /// Overall classification coverage as a percentage (0–100).
    ///
    /// Defined as `classified / total_commits * 100`. Zero when
    /// `total_commits == 0`.
    pub coverage_pct: f64,
    /// Per-repository coverage (repo_name → coverage percentage).
    pub coverage_by_repo: HashMap<String, RepoCoverage>,
    /// #111: LLM calls made this run and the tokens they used.
    pub llm_usage: super::pipeline_llm::LlmUsageTotals,
}

/// Per-repository coverage breakdown.
///
/// Why: a global coverage number hides the case where one repo classifies
/// at 95% and another at 5%; surfacing per-repo coverage lets operators
/// drill in.
/// What: total commits, classified count, and percentage (0–100).
/// Test: covered by classification pipeline integration tests.
#[derive(Debug, Clone, Default)]
pub struct RepoCoverage {
    /// Total commits seen for this repository.
    pub total: usize,
    /// Commits with a non-`"uncategorized"` verdict.
    pub classified: usize,
    /// `classified / total * 100`.
    pub coverage_pct: f64,
}

/// Stage-2 pipeline: classify every unclassified commit currently in the DB.
///
/// Why: classification touches multiple tiers (rules / regex / fuzzy / LLM)
/// and the engine config / taxonomy / JIRA mappings all live in `Config`;
/// concentrating orchestration here keeps the binary's `commands/classify.rs`
/// thin.
/// What: holds the validated [`Config`] plus toggles for `force` re-classify,
/// `since`/`until` date bounds, and `repos` filter. Built via [`Self::new`] +
/// builder methods.
/// Test: covered by `classify::tests::pipeline_runs_against_in_memory_db`.
pub struct ClassificationPipeline {
    config: Config,
    /// When `true`, re-classify commits that already carry a verdict.
    ///
    /// Defaults to `false` (skip-if-classified). See [`Self::with_force`].
    force: bool,
    /// Optional lower bound on `commits.timestamp` (ISO8601: `YYYY-MM-DD`).
    ///
    /// Only consulted when `force == true`; without force the default
    /// "missing-verdict" filter is the only selector. See [`Self::with_since`].
    since: Option<String>,
    /// Optional upper bound on `commits.timestamp` (ISO8601: `YYYY-MM-DD`).
    ///
    /// Scopes re-classification to commits on or before this date.
    /// See [`Self::with_until`].
    until: Option<String>,
    /// Optional repository filter: only classify commits from these repos.
    ///
    /// When non-empty, only commits whose `repository` column matches one of
    /// the listed names are considered. See [`Self::with_repos`].
    repos: Vec<String>,
    /// #111: when `Some`, only these commit SHAs are candidates; see
    /// [`Self::with_shas`].
    shas: Option<Vec<String>>,
}

impl ClassificationPipeline {
    /// Construct a new pipeline bound to the given config.
    ///
    /// Why: pipelines start with re-classification disabled by default
    /// (the common "fill in missing verdicts" case); operators opt in to
    /// `force` via the builder.
    /// What: stores the config; sets `force = false`, `since = None`.
    /// Test: covered by `tests::pipeline_constructs_with_default_config`
    /// in `collect::tests` and the pipeline integration tests.
    pub fn new(config: Config) -> Self {
        Self {
            config,
            force: false,
            since: None,
            until: None,
            repos: Vec::new(),
            shas: None,
        }
    }

    /// Re-classify commits even if they already have a `classification_id`.
    ///
    /// Why: when the rule set is updated (or a JIRA project mapping is
    /// added), operators need to retroactively apply the new rules to
    /// historical data. Without this, the pipeline skips classified rows
    /// and the new rules never fire on them. Issue #205.
    /// What: flips the read query from "WHERE classification_id IS NULL" to
    /// "any commit", and the write-back replaces the existing
    /// `classifications` row in place (no orphan rows).
    /// Test: see `pipeline_force_reclassifies_rows` in this module.
    pub fn with_force(mut self, force: bool) -> Self {
        self.force = force;
        self
    }

    /// Bound `--force` rewrites to commits whose `timestamp` is on or after
    /// the given ISO8601 date.
    ///
    /// Why: full-corpus rewrites are expensive; the common case for the
    /// retroactive flow is "apply the new rules to the last quarter".
    /// What: stores the string verbatim; the read query appends a
    /// `timestamp >= ?` predicate when set. No-op when `force` is `false`.
    /// Test: covered by the same integration test as `with_force`.
    pub fn with_since(mut self, since: Option<String>) -> Self {
        self.since = since;
        self
    }

    /// Bound classification to commits on or before the given ISO8601 date.
    ///
    /// Why: complements `with_since` to allow a bounded window like
    /// `--since 2026-01-01 --until 2026-03-31` without touching commits
    /// outside that quarter.
    /// What: stores the string; the read query appends a `timestamp <= ?`
    /// predicate when set.
    /// Test: covered by pipeline integration tests that exercise date windows.
    pub fn with_until(mut self, until: Option<String>) -> Self {
        self.until = until;
        self
    }

    /// Restrict classification to commits from specific repositories.
    ///
    /// Why: the `--repos` filter lets operators classify only a slice of the
    /// DB (e.g. one service) without running across the full corpus.
    /// What: when non-empty, adds a `WHERE repository IN (…)` clause to the
    /// candidate commit query.
    /// Test: see `tests::classify_repos_filter_*` in this module.
    pub fn with_repos(mut self, repos: Vec<String>) -> Self {
        self.repos = repos;
        self
    }

    /// Restrict classification to an explicit list of commit SHAs (#111).
    ///
    /// Why: the eval runs the LLM on its sample only, without touching any
    /// other commit. `tga classify` writes, so this is meant for a scratch
    /// copy of the database.
    /// What: adds `sha IN (…)` to the candidate query. Fail-closed: the run
    /// errors before any write when the list is empty or names a SHA that is
    /// not in `commits` (exact, full-SHA match).
    /// Test: `pipeline_llm_tests::shas_subset_classifies_only_listed_commits`,
    /// `pipeline_llm_tests::unknown_sha_fails_before_any_write`.
    pub fn with_shas(mut self, shas: Option<Vec<String>>) -> Self {
        self.shas = shas;
        self
    }

    /// Execute the pipeline against `db`.
    ///
    /// Workflow:
    /// 1. Load rules from `config.classification.rules_file`, or fall back
    ///    to [`default_rules`].
    /// 2. Build the [`ClassificationEngine`].
    /// 3. Query all commits with `classification_id IS NULL`.
    /// 4. Classify in parallel (Rayon) using tiers 1–3.
    /// 5. Optionally invoke the async LLM tier for commits still uncategorized.
    /// 6. Write `classifications` rows and update each commit's
    ///    `classification_id` and `confidence`.
    ///
    /// # Errors
    ///
    /// Returns an error if the DB queries, rule loading, or migrations fail.
    /// Build the [`ClassificationEngine`] from this pipeline's config.
    ///
    /// Why: both [`Self::run`] and [`Self::backfill_complexity`] need an
    /// identically-configured engine; extracting this keeps the rule-merge
    /// and config-mapping logic in one place.
    /// What: loads/merges rules, maps `Config` → `ClassificationEngineConfig`,
    /// and constructs the engine (without the DB-backed override tier).
    /// When the top-level `llm:` section is present it takes precedence over
    /// legacy `classification.*` fields; legacy fields emit a deprecation
    /// warning when `llm:` is absent but those fields are used.
    /// Test: exercised indirectly by the pipeline integration tests.
    ///
    /// # Errors
    ///
    /// Returns an error if rules fail to load/compile or the LLM provider
    /// fails to initialize.
    async fn build_engine(&self) -> Result<ClassificationEngine> {
        let mut engine = self.build_rule_engine()?;
        let engine_cfg = self.engine_config();

        // Determine whether the LLM tier is requested and which source.
        //
        // Precedence (highest first):
        // 1. Top-level `llm:` section (new, preferred): presence of the section
        //    SELF-ENABLES the LLM tier — no `classification.use_llm: true` required.
        //    The intent is: if you wrote `llm:` in config, you mean to use the LLM.
        // 2. Legacy `classification.use_llm: true` — still honored when no `llm:`
        //    section is present.
        //
        // Note: an explicit `use_llm: false` in `classification:` does NOT suppress
        // the `llm:` section; the `llm:` section is always self-enabling. Users who
        // need to temporarily disable the LLM tier while keeping the `llm:` config
        // should remove or comment out the `llm:` block.
        let use_llm = self.config.llm.is_some()
            || self
                .config
                .classification
                .as_ref()
                .map(|c| c.use_llm)
                .unwrap_or(false);

        // Wire the LLM tier when requested, preferring the `llm:` section.
        if use_llm {
            let llm_classifier = if let Some(llm_cfg) = self.config.llm.as_ref() {
                // New path: `llm:` section present.
                //
                // Model resolution order:
                //  1. Explicit `llm.model` in the `llm:` section.
                //  2. Legacy `classification.llm_model` (migration compat).
                //  3. Source-aware default: bedrock → DEFAULT_BEDROCK_MODEL,
                //     anthropic-api → ANTHROPIC_DEFAULT_MODEL,
                //     openrouter → "gpt-4o-mini".
                //
                // Why: using `gpt-4o-mini` as the universal fallback causes
                // invalid-model errors for `bedrock` and `anthropic-api` sources
                // when `llm.model` is unset. Each provider requires a model id
                // from its own namespace.
                let source_default = match llm_cfg.source {
                    LlmSource::Bedrock => DEFAULT_BEDROCK_MODEL,
                    LlmSource::AnthropicApi => ANTHROPIC_DEFAULT_MODEL,
                    LlmSource::Openrouter => "gpt-4o-mini",
                };
                let model = llm_cfg
                    .model
                    .as_deref()
                    .or(self
                        .config
                        .classification
                        .as_ref()
                        .and_then(|c| c.llm_model.as_deref()))
                    .unwrap_or(source_default);
                crate::classify::tiers::llm::LlmClassifier::from_llm_config(llm_cfg, model)
                    .await
                    .map_err(|e| {
                        crate::classify::errors::ClassifyError::Config(format!(
                            "LLM provider init failed (llm: section): {e}"
                        ))
                    })?
            } else {
                // Legacy path: `classification.llm_provider` etc.
                // Emit a single deprecation warning so existing configs
                // get a clear upgrade path.
                if self
                    .config
                    .classification
                    .as_ref()
                    .map(|c| c.openrouter_api_key.is_some() || c.llm_provider != "auto")
                    .unwrap_or(false)
                {
                    warn!(
                        "classification.openrouter_api_key / classification.llm_provider \
                             are deprecated. Migrate to the top-level `llm:` section: \
                             'llm:\\n  source: openrouter\\n  api_key_env: OPENROUTER_API_KEY'"
                    );
                }
                crate::classify::tiers::llm::LlmClassifier::from_provider_async(
                    &engine_cfg.llm_provider,
                    &engine_cfg.llm_model,
                    engine_cfg.openrouter_api_key.clone(),
                )
                .await
                .map_err(|e| {
                    crate::classify::errors::ClassifyError::Config(format!(
                        "LLM provider init failed: {e}"
                    ))
                })?
            };

            // Fail-loudly guard: when LLM is enabled but no credential resolves,
            // error before writing any DB rows. The error names the specific env
            // var (when an `llm:` section is present) so the user knows exactly
            // what to export. No DB writes occur.
            if !llm_classifier.has_api_key() {
                let var_hint = self
                    .config
                    .llm
                    .as_ref()
                    .map(|l| format!(" ('{}')", l.api_key_env))
                    .unwrap_or_default();
                return Err(crate::classify::errors::ClassifyError::Config(format!(
                    "LLM tier is enabled but no API key or credentials could be resolved. \
                     Ensure the environment variable{var_hint} named by llm.api_key_env \
                     is set and non-empty (for openrouter/anthropic-api), or that valid \
                     AWS credentials are present in the credential chain (for bedrock). \
                     No database writes will occur."
                )));
            }

            // #131: a rules file that defines the whole category set
            // (`extend_defaults: false`) is the LLM's category set too.
            let llm_classifier = match self.llm_categories()? {
                Some(categories) => llm_classifier.with_allowed_categories(categories),
                None => llm_classifier,
            };
            engine.attach_llm(llm_classifier);
        }

        Ok(engine)
    }

    /// Map `Config` onto the engine's per-run tier knobs.
    fn engine_config(&self) -> ClassificationEngineConfig {
        match self.config.classification.as_ref() {
            Some(c) => ClassificationEngineConfig {
                use_llm: c.use_llm,
                llm_model: c.llm_model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                llm_provider: c.llm_provider.clone(),
                openrouter_api_key: c.openrouter_api_key.clone(),
                confidence_threshold: c.confidence_threshold,
                weighted_sum: c.weighted_sum.clone(),
            },
            None => ClassificationEngineConfig::default(),
        }
    }

    /// Load and merge `classification.rules_files`, or the built-ins when
    /// none are configured (#445 batch C). The last file's `extend_defaults`
    /// flag wins; with it set, custom rules override defaults by id.
    fn load_ruleset(&self) -> Result<(crate::classify::rules::RuleSet, RuleSources)> {
        use crate::classify::rules::load_rules_multi_with_sources;
        let paths: Vec<&std::path::Path> = self
            .config
            .classification
            .as_ref()
            .map(|c| c.rules_files.iter().map(|p| p.as_path()).collect())
            .unwrap_or_default();
        if paths.is_empty() {
            return Ok((default_rules(), RuleSources::builtin()));
        }
        let (custom, sources) = load_rules_multi_with_sources(&paths)?;
        if !custom.extend_defaults {
            return Ok((custom, sources));
        }
        let mut merged = default_rules();
        let custom_ids: std::collections::HashSet<String> =
            custom.rules.iter().map(|r| r.id.clone()).collect();
        merged.rules.retain(|r| !custom_ids.contains(&r.id));
        merged.rules.extend(custom.rules);
        Ok((merged, sources))
    }

    /// Category names the loaded rules can emit, deduplicated, in rule order.
    ///
    /// Why: `tga eval score --config` accepts a rater label the config names
    /// (#111). The rules file is the source of truth for its category set, and
    /// a rule's category need not be declared in the taxonomy.
    /// What: loads the ruleset [`Self::build_rule_engine`] uses and returns
    /// each rule's `category`.
    /// Test: `tests/eval_harness.rs::score_accepts_labels_named_by_the_rules_file`.
    ///
    /// # Errors
    ///
    /// Returns an error if a rules file fails to load.
    pub fn rule_categories(&self) -> Result<Vec<String>> {
        let (ruleset, _) = self.load_ruleset()?;
        Ok(configured_categories(ruleset)
            .into_iter()
            .map(|c| c.name)
            .collect())
    }

    /// The category set the LLM tier may answer with, or `None` for the
    /// built-in list (#131).
    ///
    /// Why: with `extend_defaults: false` the rules files define the whole
    /// category set; see [`crate::classify::tiers::llm::LlmClassifier::with_allowed_categories`].
    /// What: `None` when no rules file is configured or the last one sets
    /// `extend_defaults: true`; otherwise every rule category (rule order)
    /// then every `categories:` entry not already named, each carrying its
    /// `description` when the rules files give one.
    /// Test: `pipeline_llm_tests::llm_categories_follow_extend_defaults`.
    ///
    /// # Errors
    ///
    /// Returns an error if a rules file fails to load.
    pub fn llm_categories(&self) -> Result<Option<Vec<crate::classify::rules::CategoryDef>>> {
        let (ruleset, _) = self.load_ruleset()?;
        let custom_only = !ruleset.extend_defaults
            && self
                .config
                .classification
                .as_ref()
                .is_some_and(|c| !c.rules_files.is_empty());
        Ok(custom_only.then(|| configured_categories(ruleset)))
    }

    /// Build the synchronous rule engine (tiers 1–3.5) with rule provenance,
    /// without the LLM tier.
    ///
    /// Why: `tga classify` attaches the LLM tier on top of this engine; the
    /// eval harness (#111) re-classifies with exactly the same rules but must
    /// never call an LLM, and needs each rule's source file for its trace.
    /// What: loads and merges `classification.rules_files` (or the built-ins),
    /// maps `Config` onto the engine config with `use_llm = false`, applies
    /// the custom taxonomy and JIRA project mappings, and records which file
    /// defined each rule.
    /// Test: `tests/classify_byte_identical.rs` (verdicts unchanged) and
    /// `classify::rules::multi_loader::tests::multi_load_records_the_last_defining_file` (rule sources).
    ///
    /// # Errors
    ///
    /// Returns an error if a rules file fails to load or compile.
    pub fn build_rule_engine(&self) -> Result<ClassificationEngine> {
        let (ruleset, sources) = self.load_ruleset()?;

        let custom_taxonomy = self
            .config
            .classification
            .as_ref()
            .map(|c| c.custom_categories.clone())
            .unwrap_or_default();

        let jira_mappings = self
            .config
            .jira
            .as_ref()
            .map(|j| j.jira_project_mappings.clone())
            .unwrap_or_default();

        let jira_confidence = self
            .config
            .jira
            .as_ref()
            .and_then(|j| j.jira_project_mapping_confidence);

        // Build the engine without an injected LLM tier; `build_engine`
        // attaches it (which may require async SDK init for Bedrock).
        let engine_cfg_no_llm = ClassificationEngineConfig {
            use_llm: false,
            ..self.engine_config()
        };
        let engine = ClassificationEngine::with_taxonomy_mappings_and_confidence(
            ruleset,
            engine_cfg_no_llm,
            custom_taxonomy,
            jira_mappings,
            jira_confidence,
            // Override-tier DB wiring is deferred: rusqlite::Connection is
            // not Send + Sync, so plumbing the live connection through the
            // Rayon batch would require a redesign. The override tier is
            // still constructible via `with_taxonomy_and_mappings` for
            // single-threaded callers and tests.
            None,
        )?;
        Ok(engine.with_rule_sources(sources))
    }

    /// Build an [`ExternalSourceResolver`] from the pipeline's config, or
    /// return `None` when external sources are disabled or none are configured.
    ///
    /// Why: the resolver is an optional component — teams without JIRA/GitHub
    /// or running in offline CI should not pay any overhead for it. This
    /// method centralises the "should I build a resolver?" decision.
    /// What: returns `None` when `no_external` is `true` OR when the
    /// `sources` list is empty; otherwise constructs a fresh resolver.
    /// Test: exercised by the pipeline integration tests via `run`.
    fn build_resolver(&self) -> Option<ExternalSourceResolver> {
        let no_external = self
            .config
            .classification
            .as_ref()
            .map(|c| c.no_external)
            .unwrap_or(false);
        if no_external {
            return None;
        }
        let sources = self
            .config
            .classification
            .as_ref()
            .map(|c| c.sources.as_slice())
            .unwrap_or(&[]);
        if sources.is_empty() {
            return None;
        }
        Some(ExternalSourceResolver::new(sources))
    }

    /// Execute the pipeline against `db`.
    ///
    /// Workflow:
    /// 1. Build the [`ClassificationEngine`] from config (rules + LLM tier).
    /// 2. Optionally build an [`ExternalSourceResolver`] from `config.sources`.
    /// 3. Query all commits with `classification_id IS NULL`.
    /// 4. Classify in parallel (Rayon) using tiers 0–3.
    /// 5. Optionally invoke the async LLM tier for low-confidence verdicts.
    /// 6. Write `classifications` rows (including `complexity`) and update
    ///    each commit's `classification_id` and `confidence`.
    ///
    /// # Errors
    ///
    /// Returns an error if the DB queries, rule loading, or migrations fail.
    pub async fn run(&self, db: &mut Database) -> Result<ClassificationStats> {
        // 1. Build engine (async to support Bedrock credential init).
        let engine = self.build_engine().await?;
        // 2. Build optional external source resolver.
        let resolver = self.build_resolver();
        self.run_with_engine_and_resolver(db, engine, resolver)
            .await
    }

    /// Run the classification pipeline using a caller-supplied engine.
    ///
    /// Why: tests need to inject an engine wired to a mock LLM endpoint;
    /// [`Self::run`] builds the engine itself and delegates here.
    /// What: identical to [`Self::run`] but skips engine construction.
    /// Test: the complexity-write integration test calls this directly.
    ///
    /// # Errors
    ///
    /// Returns an error if DB queries or write-back fail.
    #[allow(dead_code)]
    pub(crate) async fn run_with_engine(
        &self,
        db: &mut Database,
        engine: ClassificationEngine,
    ) -> Result<ClassificationStats> {
        self.run_with_engine_and_resolver(db, engine, None).await
    }

    /// Run with a caller-supplied engine and optional external resolver.
    ///
    /// Why: tests need to inject both a mock LLM engine and a mock external
    /// resolver independently; this overload allows both injections at once.
    /// What: the innermost execution entry point; all other `run*` variants
    /// delegate here.
    /// Test: used by resolver integration tests.
    ///
    /// # Errors
    ///
    /// Returns an error if DB queries or write-back fail.
    pub(crate) async fn run_with_engine_and_resolver(
        &self,
        db: &mut Database,
        engine: ClassificationEngine,
        resolver: Option<ExternalSourceResolver>,
    ) -> Result<ClassificationStats> {
        // 2. Read candidate commits. The default flow returns only the
        //    rows that lack a verdict; `--force` widens this to every row
        //    (optionally bounded by `--since`/`--until`/`--repos`).
        if let Some(shas) = &self.shas {
            super::pipeline_db::check_shas_exist(db, shas)?;
        }
        let commits = super::pipeline_db::read_candidate_commits(
            db,
            self.force,
            self.since.as_deref(),
            self.until.as_deref(),
            &self.repos,
            self.shas.as_deref(),
        )?;
        // #111 review: with --force every listed SHA is a candidate unless a
        // --repos/--since/--until filter excluded it; never skip one silently.
        if let (true, Some(shas)) = (self.force, &self.shas) {
            let found: std::collections::HashSet<&str> =
                commits.iter().map(|c| c.sha.as_str()).collect();
            let excluded: Vec<&str> = shas
                .iter()
                .map(String::as_str)
                .filter(|s| !found.contains(s))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if !excluded.is_empty() {
                return Err(crate::classify::errors::ClassifyError::Config(format!(
                    "{} SHA(s) from --shas fall outside the --repos/--since/--until \
                     filter (e.g. {}); nothing was written",
                    excluded.len(),
                    excluded
                        .iter()
                        .take(5)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(", ")
                )));
            }
        }
        let total = commits.len();
        info!(
            total,
            force = self.force,
            since = ?self.since,
            until = ?self.until,
            repos = ?self.repos,
            "starting classification"
        );

        if commits.is_empty() {
            return Ok(ClassificationStats::default());
        }

        // 3a. Tier 0 (override) pre-pass — done serially against the live
        //     DB connection. Commits with a hit skip the parallel cascade.
        let overrides = super::pipeline_db::read_overrides(db, &commits)?;

        // 3b. Tiers 1–3 in parallel for commits without an override.
        let pairs: Vec<(&str, bool)> = commits
            .iter()
            .map(|c| (c.message.as_str(), c.is_merge))
            .collect();
        let mut results = engine.classify_batch(&pairs);

        // Apply Tier-0 manual overrides (highest precedence).
        for (idx, commit) in commits.iter().enumerate() {
            if let Some(r) = overrides.get(&commit.id) {
                results[idx] = r.clone();
            }
        }

        // Tier 0.5: external sources (JIRA / GitHub Issues), resolved with
        // bounded concurrency (issue #2719).
        //
        // Why: external ticket-type signals are more authoritative than
        // commit-message heuristics but must still defer to manual overrides
        // (Tier 0). The previous implementation resolved tickets strictly
        // serially — one network round-trip per commit, each up to the 30s
        // JIRA timeout — which produced multi-minute "zero progress" stalls on
        // large corpora. We now warm the resolver's cache from the full
        // ticket-KEY set extracted across every unique commit message (NOT a
        // message-level dedupe — two differently-worded commits referencing
        // the same ticket must still resolve to one fetch; see
        // `pipeline_external`'s module doc for the code-critic HIGH finding
        // this closes), fetching each unique ticket with a bounded
        // `buffer_unordered` (mirroring the LLM fallback tier), then apply the
        // resulting signal to every commit sharing a message that referenced
        // it. Commits with a Tier-0 override are excluded (manual overrides
        // win).
        if let Some(res) = &resolver {
            let signals =
                super::pipeline_external::resolve_external_signals(&commits, &overrides, res).await;
            for (idx, commit) in commits.iter().enumerate() {
                if overrides.contains_key(&commit.id) {
                    continue;
                }
                if let Some(signal) = signals.get(commit.message.as_str()) {
                    let top_level = engine.taxonomy().resolve(&signal.category);
                    results[idx] = ClassificationResult {
                        category: signal.category.clone(),
                        subcategory: None,
                        top_level,
                        confidence: signal.confidence,
                        method: ClassificationMethod::ExternalSource,
                        ticket_id:
                            crate::classify::tiers::regex_tier::RegexMatcher::extract_ticket_id(
                                &commit.message,
                            ),
                        complexity: None,
                    };
                }
            }
        }

        // 4. LLM fallback (async, bounded-concurrency) for the verdicts
        //    `llm_fallback_scope` selects: `low_confidence` (default) sends
        //    every verdict at or below `llm_fallback_threshold` (0.65);
        //    `unanswered` (#111) sends only verdicts the rules left
        //    uncategorized.
        let run_started_at = chrono::Utc::now().to_rfc3339();
        let mut llm_totals = super::pipeline_llm::LlmUsageTotals::default();
        let mut usage_rows = Vec::new();
        if engine.config().use_llm {
            // Single startup-time diagnostic when the LLM tier is on but no
            // credential is reachable.
            if matches!(engine.llm_has_api_key(), Some(false)) {
                warn!(
                    "LLM tier enabled but no API key resolved \
                     (OPENAI_API_KEY / OPENROUTER_API_KEY unset); \
                     fallback will short-circuit silently"
                );
            }
            let cls = self.config.classification.as_ref();
            (llm_totals, usage_rows) = super::pipeline_llm::run_llm_fallback(
                &engine,
                &commits,
                &mut results,
                cls.map(|c| c.llm_fallback_scope).unwrap_or_default(),
                cls.map(|c| c.llm_fallback_threshold).unwrap_or(0.65),
                cls.map(|c| c.llm_fallback_concurrency).unwrap_or(8),
            )
            .await;
        }
        // #111 review: record billed calls before the classification writes,
        // so a failed write-back never loses them.
        if let Some((provider, model)) = engine.llm_identity() {
            super::pipeline_llm::record_usage(
                db,
                &commits,
                &usage_rows,
                (provider, &model),
                &run_started_at,
            )?;
        }

        // 5. Write back + coverage bookkeeping.
        let checkpoint_every = self
            .config
            .classification
            .as_ref()
            .map(|c| c.checkpoint_every)
            .unwrap_or(0);
        let mut stats =
            super::pipeline_db::write_results(db, &commits, &results, checkpoint_every)?;
        super::pipeline_db::compute_coverage(&mut stats);
        if llm_totals.calls > 0 {
            info!(
                calls = llm_totals.calls,
                input_tokens = llm_totals.input_tokens,
                output_tokens = llm_totals.output_tokens,
                "LLM fallback token usage"
            );
        }
        stats.llm_usage = llm_totals;
        super::pipeline_db::persist_repository_status(db, &stats)?;
        super::pipeline_db::report_coverage(&stats, self.min_coverage_pct());
        info!(
            total = stats.total_commits,
            classified = stats.classified,
            coverage_pct = stats.coverage_pct,
            "classification complete"
        );
        Ok(stats)
    }

    /// Effective minimum-coverage warning threshold for this pipeline.
    fn min_coverage_pct(&self) -> f64 {
        self.config
            .classification
            .as_ref()
            .map(|c| c.min_coverage_pct)
            .unwrap_or(DEFAULT_MIN_COVERAGE_PCT)
    }

    /// Backfill missing `complexity` scores for already-classified commits.
    ///
    /// Why: rows classified before this feature (or by non-LLM tiers) have
    /// `complexity IS NULL`. This fills them in without disturbing the
    /// existing category/confidence/method verdict, so a corpus can gain
    /// complexity scores incrementally.
    /// What: selects `classifications` rows where `complexity IS NULL` and
    /// `method != 'exact_rule'`, skipping merge commits (#111), asks the LLM
    /// for a complexity score per commit, and writes only the `complexity`
    /// column back.
    /// Test: see `tests/` — pre-seed a NULL row and a scored row, run this,
    /// assert the NULL row is filled and the scored row is unchanged.
    ///
    /// # Errors
    ///
    /// Returns an error if engine construction or DB access fails.
    pub async fn backfill_complexity(&self, db: &mut Database) -> Result<usize> {
        let engine = self.build_engine().await?;
        Self::backfill_complexity_with_engine(db, &engine).await
    }

    /// Backfill complexity using a caller-supplied engine.
    ///
    /// Why: tests inject an engine wired to a mock LLM endpoint;
    /// [`Self::backfill_complexity`] builds the engine and delegates here.
    /// What: the engine-agnostic core of the backfill.
    /// Test: the backfill integration test calls this directly.
    ///
    /// # Errors
    ///
    /// Returns an error if DB access fails.
    pub(crate) async fn backfill_complexity_with_engine(
        db: &mut Database,
        engine: &ClassificationEngine,
    ) -> Result<usize> {
        // Collect candidate rows. Rows produced by the `exact_rule` tier and
        // merge commits are excluded — neither is LLM-eligible (#111).
        let candidates = super::pipeline_db::read_complexity_backfill_candidates(db)?;
        let total = candidates.len();
        info!(total, "starting complexity backfill");
        if candidates.is_empty() {
            return Ok(0);
        }

        let pb = super::pipeline_db::make_progress(total as u64, "Complexity backfill");
        let mut updated = 0_usize;
        {
            let conn = db.connection_mut();
            let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
            {
                let mut update_stmt = tx
                    .prepare("UPDATE classifications SET complexity = ?1 WHERE id = ?2")
                    .map_err(crate::core::TgaError::from)?;
                for cand in &candidates {
                    let verdict = engine.llm_classify_only(&cand.message).await;
                    let complexity = verdict.and_then(|r| r.complexity);
                    match complexity {
                        Some(score) => {
                            update_stmt
                                .execute(params![score as i64, cand.classification_id])
                                .map_err(crate::core::TgaError::from)?;
                            updated += 1;
                            info!(
                                commit_sha = %cand.commit_sha,
                                score,
                                "backfilled complexity"
                            );
                        }
                        None => {
                            warn!(
                                commit_sha = %cand.commit_sha,
                                "LLM returned no complexity score; leaving NULL"
                            );
                        }
                    }
                    pb.inc(1);
                }
            }
            tx.commit().map_err(crate::core::TgaError::from)?;
        }
        pb.finish_and_clear();
        info!(updated, total, "complexity backfill complete");
        Ok(updated)
    }
}

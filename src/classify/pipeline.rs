//! End-to-end classification pipeline: read DB → classify → write back.

use std::collections::HashMap;

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tracing::{info, warn};

use crate::classify::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::classify::errors::Result;
use crate::classify::rules::{default_rules, load_rules};
use crate::classify::tiers::ClassificationResult;
use crate::core::config::Config;
use crate::core::db::Database;

/// Default minimum coverage threshold (percent) below which the pipeline
/// emits a warning. Used when no config-level override is supplied.
#[allow(dead_code)]
const DEFAULT_MIN_COVERAGE_PCT: f64 = 20.0;

/// Aggregate statistics from a single pipeline run.
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
}

/// Per-repository coverage breakdown.
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
pub struct ClassificationPipeline {
    config: Config,
}

impl ClassificationPipeline {
    /// Construct a new pipeline bound to the given config.
    pub fn new(config: Config) -> Self {
        Self { config }
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
    pub async fn run(&self, db: &mut Database) -> Result<ClassificationStats> {
        // 1. Build engine.
        let ruleset = match self
            .config
            .classification
            .as_ref()
            .and_then(|c| c.rules_file.as_ref())
        {
            Some(path) => {
                let custom = load_rules(path)?;
                if custom.extend_defaults {
                    // Merge: start with defaults, let custom rules override by id
                    let mut merged = default_rules();
                    let custom_ids: std::collections::HashSet<String> =
                        custom.rules.iter().map(|r| r.id.clone()).collect();
                    // Remove any default rules whose id is overridden by custom
                    merged.rules.retain(|r| !custom_ids.contains(&r.id));
                    // Append custom rules (they may have higher priority to win)
                    merged.rules.extend(custom.rules);
                    merged
                } else {
                    custom
                }
            }
            None => default_rules(),
        };

        let engine_cfg = match self.config.classification.as_ref() {
            Some(c) => ClassificationEngineConfig {
                use_llm: c.use_llm,
                llm_model: c.llm_model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                llm_provider: c.llm_provider.clone(),
                openrouter_api_key: c.openrouter_api_key.clone(),
                confidence_threshold: c.confidence_threshold,
            },
            None => ClassificationEngineConfig::default(),
        };

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

        let engine = ClassificationEngine::with_taxonomy_and_mappings(
            ruleset,
            engine_cfg,
            custom_taxonomy,
            jira_mappings,
            // Override-tier DB wiring is deferred: rusqlite::Connection is
            // not Send + Sync, so plumbing the live connection through the
            // Rayon batch would require a redesign. The override tier is
            // still constructible via `with_taxonomy_and_mappings` for
            // single-threaded callers and tests.
            None,
        )?;

        // 2. Read unclassified commits.
        let commits = read_unclassified_commits(db)?;
        let total = commits.len();
        info!(total, "starting classification");

        if commits.is_empty() {
            return Ok(ClassificationStats::default());
        }

        // 3a. Tier 0 (override) pre-pass — done serially against the live
        //     DB connection. Commits with a hit skip the parallel cascade.
        let overrides = read_overrides(db, &commits)?;

        // 3b. Tiers 1–3 in parallel for commits without an override.
        let pairs: Vec<(&str, bool)> = commits
            .iter()
            .map(|c| (c.message.as_str(), c.is_merge))
            .collect();
        let mut results = engine.classify_batch(&pairs);

        for (idx, commit) in commits.iter().enumerate() {
            if let Some(r) = overrides.get(&commit.id) {
                results[idx] = r.clone();
            }
        }

        // 4. LLM fallback (async, serialized) for entries whose verdict
        //    confidence is at or below `llm_fallback_threshold`. The default
        //    threshold is `0.0`, preserving legacy behaviour; raising it to
        //    `~0.35` routes catch-all (`confidence = 0.3`) verdicts through
        //    the LLM.
        if engine.config().use_llm {
            let fallback_threshold = self
                .config
                .classification
                .as_ref()
                .map(|c| c.llm_fallback_threshold)
                .unwrap_or(0.0);
            let pb = make_progress(total as u64, "LLM fallback");
            for (idx, commit) in commits.iter().enumerate() {
                if results[idx].confidence <= fallback_threshold {
                    let original = results[idx].clone();
                    let r = engine.classify(&commit.message, commit.is_merge).await;
                    // Overwrite-guard: if the LLM call effectively failed
                    // (returned a verdict no better than what we had), keep
                    // the original catch-all verdict so we don't regress
                    // confidence to 0.0.
                    if r.confidence > original.confidence {
                        results[idx] = r;
                    } else {
                        results[idx] = original;
                    }
                }
                pb.inc(1);
            }
            pb.finish_and_clear();
        }

        // 5. Write back + coverage bookkeeping.
        let mut stats = write_results(db, &commits, &results)?;
        compute_coverage(&mut stats);
        persist_repository_status(db, &stats)?;
        report_coverage(&stats, self.min_coverage_pct());
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
}

/// Look up manual classification overrides for the given commits.
///
/// Returns a map keyed by commit row id; only commits with a hit are
/// present in the result. Performs one prepared query per commit; for
/// the typical "small N" override list this is cheaper than building an
/// in-memory index.
fn read_overrides(
    db: &Database,
    commits: &[CommitRow],
) -> Result<HashMap<i64, ClassificationResult>> {
    use crate::classify::taxonomy::TaxonomyRegistry;
    use crate::core::models::ClassificationMethod;

    let mut out: HashMap<i64, ClassificationResult> = HashMap::new();
    if commits.is_empty() {
        return Ok(out);
    }
    let taxonomy = TaxonomyRegistry::with_builtins();
    let conn = db.connection();
    let mut stmt = conn
        .prepare(
            "SELECT work_type, change_type FROM classification_overrides \
             WHERE commit_sha = ?1 AND repo_path = ?2",
        )
        .map_err(crate::core::TgaError::from)?;
    for commit in commits {
        let row = stmt.query_row(params![commit.sha, commit.repository], |row| {
            let work_type: String = row.get(0)?;
            let change_type: String = row.get(1)?;
            Ok((work_type, change_type))
        });
        match row {
            Ok((work_type, change_type)) => {
                let top_level = taxonomy
                    .resolve(&change_type)
                    .or_else(|| taxonomy.resolve(&work_type));
                out.insert(
                    commit.id,
                    ClassificationResult {
                        category: work_type,
                        subcategory: Some(change_type),
                        top_level,
                        confidence: 1.0,
                        method: ClassificationMethod::Manual,
                        ticket_id: None,
                    },
                );
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => {}
            Err(e) => return Err(crate::core::TgaError::from(e).into()),
        }
    }
    Ok(out)
}

/// Fill in `coverage_pct` and `coverage_by_repo[_].coverage_pct` from the
/// already-populated totals.
fn compute_coverage(stats: &mut ClassificationStats) {
    stats.coverage_pct = if stats.total_commits == 0 {
        0.0
    } else {
        (stats.classified as f64 / stats.total_commits as f64) * 100.0
    };
    for repo in stats.coverage_by_repo.values_mut() {
        repo.coverage_pct = if repo.total == 0 {
            0.0
        } else {
            (repo.classified as f64 / repo.total as f64) * 100.0
        };
    }
}

/// Upsert one `repository_analysis_status` row per repo seen in `stats`.
fn persist_repository_status(db: &mut Database, stats: &ClassificationStats) -> Result<()> {
    if stats.coverage_by_repo.is_empty() {
        return Ok(());
    }
    let conn = db.connection_mut();
    let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
    {
        let mut upsert = tx
            .prepare(
                "INSERT INTO repository_analysis_status \
                    (repo_name, last_analyzed_at, classification_coverage_pct, \
                     total_commits, classified_commits) \
                 VALUES (?1, datetime('now'), ?2, ?3, ?4) \
                 ON CONFLICT(repo_name) DO UPDATE SET \
                    last_analyzed_at = datetime('now'), \
                    classification_coverage_pct = excluded.classification_coverage_pct, \
                    total_commits = excluded.total_commits, \
                    classified_commits = excluded.classified_commits",
            )
            .map_err(crate::core::TgaError::from)?;
        for (repo, cov) in &stats.coverage_by_repo {
            upsert
                .execute(params![
                    repo,
                    cov.coverage_pct,
                    cov.total as i64,
                    cov.classified as i64,
                ])
                .map_err(crate::core::TgaError::from)?;
        }
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    Ok(())
}

/// Emit `info!` (always) and `warn!` (when below `threshold_pct`) lines
/// summarizing the coverage achieved by this run.
fn report_coverage(stats: &ClassificationStats, threshold_pct: f64) {
    info!(
        "Classification coverage: {:.1}% ({} / {})",
        stats.coverage_pct, stats.classified, stats.total_commits
    );
    if stats.coverage_pct < threshold_pct && stats.total_commits > 0 {
        warn!(
            coverage_pct = stats.coverage_pct,
            threshold_pct, "classification coverage below configured threshold"
        );
    }
}

/// Minimal projection of a commit row needed for classification.
#[allow(dead_code)]
struct CommitRow {
    id: i64,
    sha: String,
    message: String,
    is_merge: bool,
    repository: String,
}

fn read_unclassified_commits(db: &Database) -> Result<Vec<CommitRow>> {
    let mut stmt = db
        .connection()
        .prepare(
            "SELECT id, sha, message, is_merge, repository \
             FROM commits WHERE classification_id IS NULL",
        )
        .map_err(crate::core::TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CommitRow {
                id: row.get(0)?,
                sha: row.get(1)?,
                message: row.get(2)?,
                is_merge: row.get::<_, i64>(3)? != 0,
                repository: row.get(4)?,
            })
        })
        .map_err(crate::core::TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(crate::core::TgaError::from)?);
    }
    Ok(out)
}

fn write_results(
    db: &mut Database,
    commits: &[CommitRow],
    results: &[ClassificationResult],
) -> Result<ClassificationStats> {
    let mut stats = ClassificationStats {
        total_commits: commits.len(),
        ..Default::default()
    };

    let pb = make_progress(commits.len() as u64, "Writing results");
    let conn = db.connection_mut();
    let tx = conn.transaction().map_err(crate::core::TgaError::from)?;
    {
        let mut insert_classification = tx
            .prepare(
                "INSERT INTO classifications (category, subcategory, ticket_id, confidence, method) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(crate::core::TgaError::from)?;
        let mut update_commit = tx
            .prepare("UPDATE commits SET classification_id = ?1, confidence = ?2 WHERE id = ?3")
            .map_err(crate::core::TgaError::from)?;

        for (commit, result) in commits.iter().zip(results.iter()) {
            insert_classification
                .execute(params![
                    result.category,
                    result.subcategory,
                    result.ticket_id,
                    result.confidence,
                    result.method.as_str(),
                ])
                .map_err(crate::core::TgaError::from)?;
            let classification_id = tx.last_insert_rowid();
            update_commit
                .execute(params![classification_id, result.confidence, commit.id])
                .map_err(crate::core::TgaError::from)?;

            // "Classified" means non-null, non-`"uncategorized"` category.
            let is_classified = !result.category.is_empty()
                && result.category != "uncategorized"
                && result.confidence > 0.0;
            if is_classified {
                stats.classified += 1;
            }
            let repo_entry = stats
                .coverage_by_repo
                .entry(commit.repository.clone())
                .or_default();
            repo_entry.total += 1;
            if is_classified {
                repo_entry.classified += 1;
            }
            *stats
                .by_method
                .entry(result.method.as_str().to_string())
                .or_insert(0) += 1;
            *stats
                .by_category
                .entry(result.category.clone())
                .or_insert(0) += 1;
            pb.inc(1);
        }
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    pb.finish_and_clear();

    if stats.classified < stats.total_commits {
        warn!(
            unclassified = stats.total_commits - stats.classified,
            "some commits remained uncategorized"
        );
    }
    Ok(stats)
}

fn make_progress(len: u64, label: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    if let Ok(style) =
        ProgressStyle::with_template("{prefix:.bold} [{bar:40.cyan/blue}] {pos}/{len} ({percent}%)")
    {
        pb.set_style(style.progress_chars("##-"));
    }
    pb.set_prefix(label.to_string());
    pb
}

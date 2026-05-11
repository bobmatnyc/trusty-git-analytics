//! End-to-end classification pipeline: read DB → classify → write back.

use std::collections::HashMap;

use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tga_core::config::Config;
use tga_core::db::Database;
use tracing::{info, warn};

use crate::classifier::{ClassificationEngine, ClassificationEngineConfig};
use crate::errors::Result;
use crate::rules::{default_rules, load_rules};
use crate::tiers::ClassificationResult;

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
            Some(path) => load_rules(path)?,
            None => default_rules(),
        };

        let engine_cfg = match self.config.classification.as_ref() {
            Some(c) => ClassificationEngineConfig {
                use_llm: c.use_llm,
                llm_model: c.llm_model.clone().unwrap_or_else(|| "gpt-4o-mini".into()),
                confidence_threshold: c.confidence_threshold,
            },
            None => ClassificationEngineConfig::default(),
        };

        let engine = ClassificationEngine::new(ruleset, engine_cfg)?;

        // 2. Read unclassified commits.
        let commits = read_unclassified_commits(db)?;
        let total = commits.len();
        info!(total, "starting classification");

        if commits.is_empty() {
            return Ok(ClassificationStats::default());
        }

        // 3. Tiers 1–3 in parallel.
        let pairs: Vec<(&str, bool)> = commits
            .iter()
            .map(|c| (c.message.as_str(), c.is_merge))
            .collect();
        let mut results = engine.classify_batch(&pairs);

        // 4. LLM fallback (async, serialized) for entries that came back as uncategorized.
        if engine.config().use_llm {
            let pb = make_progress(total as u64, "LLM fallback");
            for (idx, commit) in commits.iter().enumerate() {
                if results[idx].confidence <= 0.0 {
                    let r = engine.classify(&commit.message, commit.is_merge).await;
                    results[idx] = r;
                }
                pb.inc(1);
            }
            pb.finish_and_clear();
        }

        // 5. Write back.
        let stats = write_results(db, &commits, &results)?;
        info!(
            total = stats.total_commits,
            classified = stats.classified,
            "classification complete"
        );
        Ok(stats)
    }
}

/// Minimal projection of a commit row needed for classification.
struct CommitRow {
    id: i64,
    message: String,
    is_merge: bool,
}

fn read_unclassified_commits(db: &Database) -> Result<Vec<CommitRow>> {
    let mut stmt = db
        .connection()
        .prepare("SELECT id, message, is_merge FROM commits WHERE classification_id IS NULL")
        .map_err(tga_core::TgaError::from)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CommitRow {
                id: row.get(0)?,
                message: row.get(1)?,
                is_merge: row.get::<_, i64>(2)? != 0,
            })
        })
        .map_err(tga_core::TgaError::from)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(tga_core::TgaError::from)?);
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
    let tx = conn.transaction().map_err(tga_core::TgaError::from)?;
    {
        let mut insert_classification = tx
            .prepare(
                "INSERT INTO classifications (category, subcategory, ticket_id, confidence, method) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(tga_core::TgaError::from)?;
        let mut update_commit = tx
            .prepare("UPDATE commits SET classification_id = ?1, confidence = ?2 WHERE id = ?3")
            .map_err(tga_core::TgaError::from)?;

        for (commit, result) in commits.iter().zip(results.iter()) {
            insert_classification
                .execute(params![
                    result.category,
                    result.subcategory,
                    result.ticket_id,
                    result.confidence,
                    result.method.as_str(),
                ])
                .map_err(tga_core::TgaError::from)?;
            let classification_id = tx.last_insert_rowid();
            update_commit
                .execute(params![classification_id, result.confidence, commit.id])
                .map_err(tga_core::TgaError::from)?;

            if result.confidence > 0.0 {
                stats.classified += 1;
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
    tx.commit().map_err(tga_core::TgaError::from)?;
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

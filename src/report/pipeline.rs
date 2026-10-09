//! Report pipeline — orchestrates aggregation and formatter dispatch.

use std::path::{Path, PathBuf};

use tracing::{info, warn};

use crate::core::config::Config;
use crate::core::db::Database;
use crate::report::aggregator::Aggregator;
use crate::report::errors::{ReportError, Result};
use crate::report::formatters::{csv as csv_fmt, json as json_fmt, markdown as md_fmt};
use crate::report::linear_section::linear_section;

/// Default output directory when [`crate::core::config::OutputConfig::directory`] is unset.
const DEFAULT_OUTPUT_DIR: &str = "./reports";

/// Supported format identifiers.
const FORMAT_CSV: &str = "csv";
const FORMAT_JSON: &str = "json";
const FORMAT_MARKDOWN: &str = "markdown";

/// `fmt` names the Markdown report (`markdown` or `md`).
fn is_markdown(fmt: &str) -> bool {
    fmt == FORMAT_MARKDOWN || fmt == "md"
}

/// Stage 3 orchestrator.
///
/// Why: report generation requires aggregating the DB once and then
/// dispatching to multiple formatters (CSV / JSON / Markdown); a single
/// orchestrator means callers don't pick formatters or build paths.
/// What: holds the validated [`Config`]; `run` does the aggregation and
/// per-format dispatch and returns a [`ReportStats`].
/// Test: covered by `report::tests::pipeline_runs_all_formats_when_unspecified`.
pub struct ReportPipeline {
    config: Config,
}

/// Summary of a [`ReportPipeline::run`] invocation.
///
/// Why: the binary prints what it wrote to the user; tests assert on the
/// list of produced files.
/// What: counters plus the absolute paths of every emitted file.
/// Test: covered by `report::tests::pipeline_runs_all_formats_when_unspecified`
/// (asserts 14 files written: 9 CSV + 4 JSON + 1 Markdown).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ReportStats {
    /// Total commits that appeared in the report.
    pub total_commits: usize,
    /// Total distinct authors that appeared in the report.
    pub total_authors: usize,
    /// Absolute paths of every file written.
    pub files_written: Vec<PathBuf>,
}

impl ReportPipeline {
    /// Construct a new pipeline bound to `config`.
    ///
    /// Why: keeps construction trivial — all behaviour lives on
    /// [`Self::run`].
    /// What: stores the config; no other initialisation.
    /// Test: covered by `report::tests::pipeline_constructs_without_panic`.
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    /// Aggregate the database and write all configured report formats.
    ///
    /// If `config.output.formats` is empty (and no legacy `format` is set),
    /// all three formats are emitted. The output directory is created if it
    /// does not already exist.
    ///
    /// # Errors
    ///
    /// Propagates any aggregator, formatter, or I/O failure.
    pub fn run(&self, db: &Database) -> Result<ReportStats> {
        self.run_with_filter(db, None)
    }

    /// Aggregate the database, optionally scoped to one canonical identity,
    /// and write all configured report formats.
    ///
    /// Why: `tga report --author <email>` needs to scope the report to a
    /// single canonical identity; this method threads the filter through to
    /// the aggregator without duplicating the full run logic.
    /// What: delegates to [`Aggregator::build_filtered`] when `author_email`
    /// is `Some`; falls back to the full aggregate when `None`.  All
    /// formatters receive the (possibly filtered) [`ReportData`](crate::report::ReportData) unchanged.
    /// When Markdown is among the formats and `linear.stats.report` is on,
    /// `report.md` also carries the Linear delivery section (#190); that
    /// section is computed before any file is written, so its failure fails
    /// the run with no partial output.
    /// Test: covered by `report::tests::pipeline_author_filter_single_author`;
    /// the Linear section by `report::pipeline_linear_tests`.
    pub fn run_with_filter(
        &self,
        db: &Database,
        author_email: Option<&str>,
    ) -> Result<ReportStats> {
        let data = Aggregator::build_filtered(db, &self.config, author_email)?;
        let formats = self.resolve_formats();
        // #190: computed before the output directory or any file exists, so a
        // Linear stats failure fails the report instead of dropping the
        // section from a report that claims success.
        let linear = if formats.iter().any(|f| is_markdown(f)) {
            linear_section(db, &self.config, author_email, &data.generated_at)?
        } else {
            None
        };
        let output_dir = self.resolve_output_dir();
        std::fs::create_dir_all(&output_dir)?;
        info!(dir = %output_dir.display(), "writing reports");

        let mut files_written = Vec::new();

        for fmt in &formats {
            match fmt.as_str() {
                FORMAT_CSV => {
                    files_written.push(csv_fmt::write_author_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_weekly_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_weekly_metrics_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_developer_activity_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_summary_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_untracked_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_weekly_categorization_csv(
                        &data,
                        &output_dir,
                    )?);
                    files_written.push(csv_fmt::write_weekly_velocity_csv(&data, &output_dir)?);
                    files_written.push(csv_fmt::write_weekly_dora_csv(&data, &output_dir)?);
                }
                FORMAT_JSON => {
                    files_written.push(json_fmt::write_json(&data, &output_dir)?);
                    files_written.push(json_fmt::write_velocity_json(&data, &output_dir)?);
                    files_written.push(json_fmt::write_quality_json(&data, &output_dir)?);
                    files_written.push(json_fmt::write_dora_json(&data, &output_dir)?);
                }
                f if is_markdown(f) => {
                    files_written.push(md_fmt::write_markdown_with_linear(
                        &data,
                        &output_dir,
                        linear.as_deref(),
                    )?);
                }
                other => {
                    warn!(format = %other, "ignoring unknown output format");
                    return Err(ReportError::Report(format!(
                        "unknown output format: {other}"
                    )));
                }
            }
        }

        Ok(ReportStats {
            total_commits: data.total_commits,
            total_authors: data.total_authors,
            files_written,
        })
    }

    fn resolve_output_dir(&self) -> PathBuf {
        match self
            .config
            .output
            .as_ref()
            .and_then(|o| o.directory.as_ref())
        {
            Some(p) => crate::core::config::expand_path(p),
            None => Path::new(DEFAULT_OUTPUT_DIR).to_path_buf(),
        }
    }

    fn resolve_formats(&self) -> Vec<String> {
        if let Some(out) = &self.config.output {
            if !out.formats.is_empty() {
                return out.formats.iter().map(|s| s.to_lowercase()).collect();
            }
            if let Some(single) = &out.format {
                return vec![single.to_lowercase()];
            }
        }
        vec![
            FORMAT_CSV.to_string(),
            FORMAT_JSON.to_string(),
            FORMAT_MARKDOWN.to_string(),
        ]
    }
}

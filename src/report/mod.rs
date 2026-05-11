//! Stage 3 of the pipeline: read classified commits from a SQLite database
//! and generate CSV, JSON, and Markdown reports.
//!
//! ## Submodules
//!
//! - [`aggregator`] — DB → in-memory [`ReportData`]
//! - [`formatters`] — CSV / JSON / Markdown output
//! - [`templates`] — embedded Tera template strings
//! - [`pipeline`] — [`ReportPipeline`] orchestrator
//! - [`errors`] — [`ReportError`] / [`Result`]
//! - [`models`] — aggregated data structures

pub mod aggregator;
pub mod errors;
pub mod formatters;
pub mod models;
pub mod pipeline;
pub mod templates;

pub use errors::{ReportError, Result};
pub use models::ReportData;
pub use pipeline::{ReportPipeline, ReportStats};

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::core::config::{Config, OutputConfig, RepositoryConfig};
    use crate::core::db::Database;

    use super::aggregator::Aggregator;
    use super::formatters::{csv as csv_fmt, json as json_fmt, markdown as md_fmt};
    use super::pipeline::ReportPipeline;

    /// Seed an in-memory DB with two authors, two commits, and one classification.
    fn seed_db() -> Database {
        let db = Database::open_in_memory().expect("open db");
        let conn = db.connection();
        conn.execute(
            "INSERT INTO classifications (id, category, subcategory, ticket_id, confidence, method) \
             VALUES (1, 'feature', NULL, NULL, 0.9, 'exact_rule')",
            [],
        )
        .expect("insert classification");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, classification_id, confidence, is_merge) \
             VALUES ('aaa111', 'Alice', 'alice@example.com', '2024-01-15T10:00:00+00:00', \
                 'feat: add login', 'repo-a', 3, 50, 5, 1, 0.9, 0)",
            [],
        )
        .expect("insert commit 1");

        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, \
                 files_changed, insertions, deletions, classification_id, confidence, is_merge) \
             VALUES ('bbb222', 'Bob', 'bob@example.com', '2024-01-22T11:00:00+00:00', \
                 'fix: edge case', 'repo-a', 1, 10, 2, NULL, NULL, 0)",
            [],
        )
        .expect("insert commit 2");

        db
    }

    fn baseline_config() -> Config {
        Config {
            repositories: vec![RepositoryConfig {
                path: PathBuf::from("/tmp/repo-a"),
                name: Some("repo-a".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn aggregator_builds_report_data() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        assert_eq!(data.total_commits, 2);
        assert_eq!(data.total_authors, 2);
        assert!(data.period_start.is_some());
        assert!(data.period_end.is_some());
        assert_eq!(data.repositories.len(), 1);
        assert_eq!(data.repositories[0].name, "repo-a");
        assert_eq!(data.repositories[0].author_count, 2);
        assert_eq!(data.category_breakdown.get("feature").copied(), Some(1));
        // Two weekly buckets — one per author (different weeks).
        assert_eq!(data.weekly_activity.len(), 2);
    }

    #[test]
    fn aggregator_handles_empty_db() {
        let db = Database::open_in_memory().expect("open db");
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");
        assert_eq!(data.total_commits, 0);
        assert_eq!(data.total_authors, 0);
        assert!(data.period_start.is_none());
    }

    fn tmp_dir(label: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "tga-report-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("mkdir");
        path
    }

    #[test]
    fn csv_formatter_writes_files_with_headers() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("csv");
        let authors_path = csv_fmt::write_author_csv(&data, &dir).expect("write authors");
        let weekly_path = csv_fmt::write_weekly_csv(&data, &dir).expect("write weekly");

        let authors_text = std::fs::read_to_string(&authors_path).expect("read");
        assert!(authors_text.starts_with("name,email,commit_count"));
        assert!(authors_text.contains("Alice"));
        assert!(authors_text.contains("Bob"));

        let weekly_text = std::fs::read_to_string(&weekly_path).expect("read");
        assert!(weekly_text.starts_with("week,author,repository"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn json_formatter_writes_valid_json() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("json");
        let path = json_fmt::write_json(&data, &dir).expect("write json");
        let text = std::fs::read_to_string(&path).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(parsed["total_commits"], 2);
        assert_eq!(parsed["total_authors"], 2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn markdown_formatter_emits_report_header() {
        let db = seed_db();
        let cfg = baseline_config();
        let data = Aggregator::build(&db, &cfg).expect("aggregate");

        let dir = tmp_dir("md");
        let path = md_fmt::write_markdown(&data, &dir).expect("write md");
        let text = std::fs::read_to_string(&path).expect("read");
        assert!(text.contains("# Git Activity Report"));
        assert!(text.contains("Alice"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn pipeline_constructs_without_panic() {
        let cfg = baseline_config();
        let _pipeline = ReportPipeline::new(cfg);
    }

    #[test]
    fn pipeline_runs_all_formats_when_unspecified() {
        let db = seed_db();
        let dir = tmp_dir("pipeline");
        let cfg = Config {
            repositories: vec![RepositoryConfig {
                path: PathBuf::from("/tmp/repo-a"),
                name: Some("repo-a".into()),
                ..Default::default()
            }],
            output: Some(OutputConfig {
                directory: Some(dir.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let pipeline = ReportPipeline::new(cfg);
        let stats = pipeline.run(&db).expect("run");
        assert_eq!(stats.total_commits, 2);
        assert_eq!(stats.total_authors, 2);
        // CSV (2 files) + JSON + Markdown = 4 files.
        assert_eq!(stats.files_written.len(), 4);
        for f in &stats.files_written {
            assert!(f.exists(), "{} should exist", f.display());
        }

        std::fs::remove_dir_all(&dir).ok();
    }
}

//! CSV formatter — writes `authors.csv` and `weekly_activity.csv`.

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::report::errors::Result;
use crate::report::models::ReportData;

/// Filename for the per-author summary CSV.
pub const AUTHORS_CSV: &str = "authors.csv";

/// Filename for the weekly activity CSV.
pub const WEEKLY_CSV: &str = "weekly_activity.csv";

/// Write the per-author summary as CSV into `output_dir`.
///
/// Returns the full path to the written file.
///
/// # Errors
///
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_author_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(AUTHORS_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "name",
        "email",
        "commit_count",
        "insertions",
        "deletions",
        "files_changed",
        "first_commit",
        "last_commit",
        "categories",
    ])?;
    for a in &data.authors {
        let categories = serialize_categories(&a.categories);
        w.write_record([
            a.name.as_str(),
            a.email.as_str(),
            &a.commit_count.to_string(),
            &a.insertions.to_string(),
            &a.deletions.to_string(),
            &a.files_changed.to_string(),
            a.first_commit.as_str(),
            a.last_commit.as_str(),
            categories.as_str(),
        ])?;
    }
    w.flush()?;
    debug!(path = %path.display(), rows = data.authors.len(), "wrote authors.csv");
    Ok(path)
}

/// Write the weekly activity table as CSV into `output_dir`.
///
/// # Errors
///
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Csv`] on write failure.
pub fn write_weekly_csv(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(WEEKLY_CSV);
    let mut w = ::csv::Writer::from_path(&path)?;
    w.write_record([
        "week",
        "author",
        "repository",
        "commit_count",
        "insertions",
        "deletions",
        "categories",
    ])?;
    for row in &data.weekly_activity {
        let categories = serialize_categories(&row.categories);
        w.write_record([
            row.week.as_str(),
            row.author.as_str(),
            row.repository.as_str(),
            &row.commit_count.to_string(),
            &row.insertions.to_string(),
            &row.deletions.to_string(),
            categories.as_str(),
        ])?;
    }
    w.flush()?;
    debug!(
        path = %path.display(),
        rows = data.weekly_activity.len(),
        "wrote weekly_activity.csv"
    );
    Ok(path)
}

/// Encode a category histogram as a deterministic `key=value;…` string so
/// the CSV cell is stable and machine-parseable.
fn serialize_categories(map: &std::collections::HashMap<String, usize>) -> String {
    let mut entries: Vec<(&String, &usize)> = map.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    entries
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";")
}

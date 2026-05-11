//! Markdown formatter — renders `report.md` via Tera.

use std::path::{Path, PathBuf};

use tera::{Context, Tera};
use tracing::debug;

use crate::report::errors::Result;
use crate::report::models::ReportData;
use crate::report::templates::MARKDOWN_REPORT;

/// Filename for the Markdown output.
pub const REPORT_MD: &str = "report.md";

/// Maximum number of authors rendered in the "Top Authors" section.
const TOP_AUTHOR_LIMIT: usize = 10;

/// Render the Markdown report into `<output_dir>/report.md`.
///
/// # Errors
///
/// - [`crate::report::ReportError::Template`] on Tera failure.
/// - [`crate::report::ReportError::Io`] on write failure.
pub fn write_markdown(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let mut ctx = Context::new();
    ctx.insert("generated_at", &data.generated_at);
    ctx.insert("period_start", &data.period_start);
    ctx.insert("period_end", &data.period_end);
    ctx.insert("total_commits", &data.total_commits);
    ctx.insert("total_authors", &data.total_authors);

    let top_authors: Vec<_> = data.authors.iter().take(TOP_AUTHOR_LIMIT).collect();
    ctx.insert("top_authors", &top_authors);
    ctx.insert("repositories", &data.repositories);

    // Sort category breakdown deterministically by descending count.
    let mut cats: Vec<(String, usize)> = data
        .category_breakdown
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect();
    cats.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    ctx.insert("category_breakdown", &cats);

    let rendered = Tera::one_off(MARKDOWN_REPORT, &ctx, true)?;
    let path = output_dir.join(REPORT_MD);
    std::fs::write(&path, rendered)?;
    debug!(path = %path.display(), "wrote report.md");
    Ok(path)
}

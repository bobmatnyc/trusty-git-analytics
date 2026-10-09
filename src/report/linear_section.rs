//! The Linear delivery section of `report.md` (#190 step 5).
//!
//! Why: `tga linear stats` printed the Linear delivery metrics, but the
//! report a reader opens did not carry them.
//! What: [`linear_section`] decides whether `report.md` gets the block that
//! [`render_markdown`] writes, and computes it. It reads the database only.
//! Test: `report::pipeline_linear_tests`.

use chrono::{DateTime, Utc};
use tracing::warn;

use crate::core::config::Config;
use crate::core::db::Database;
use crate::core::errors::TgaError;
use crate::report::errors::{ReportError, Result};
use crate::report::linear_stats::{compute_linear_stats, render_markdown, StatsOptions};

/// Prefix of every error this module returns, so the report failure names
/// the section that caused it.
const SECTION: &str = "Linear delivery section";

/// The line `report.md` carries instead of the section under `--author`.
const AUTHOR_NOTE: &str = "The Linear delivery section is left out under an author filter \
    because it covers the whole workspace.";

/// What a failed section tells the user to do.
const REMEDY: &str = "fix the named row (re-run `tga linear sync`), or set \
    `linear.stats.report: false` to generate the report without the section";

fn failed(err: impl std::fmt::Display) -> ReportError {
    ReportError::Report(format!("{SECTION}: {err}; {REMEDY}"))
}

/// The Linear delivery block for `report.md` (or the line saying why it is
/// left out), or `None` when the report carries nothing about Linear.
///
/// Why: a report must either carry the Linear figures or say why it failed;
/// a section dropped on a read error would let the report claim success with
/// a part missing.
/// What: `None` when `linear.stats.report` is off or the `linear:` block is
/// absent, or when `linear_issues` holds no row with a Linear id. When
/// `author_email` scopes the report to one author, [`AUTHOR_NOTE`] instead
/// of the figures, which cover the whole workspace. Otherwise computes [`compute_linear_stats`] as of
/// `generated_at` (the report's own timestamp) with the `linear.stats`
/// bounds and returns [`render_markdown`]'s block. Writes nothing.
/// Test: `report_md_carries_the_linear_section_when_enabled_and_synced`,
/// `report_md_is_byte_identical_when_linear_is_off_or_empty`,
/// `report_md_omits_the_linear_section_under_an_author_filter`,
/// `report_fails_naming_linear_stats_when_they_cannot_be_computed`,
/// `report_leaves_every_linear_table_unchanged`.
///
/// # Errors
///
/// [`ReportError::Report`] starting `Linear delivery section:` when the
/// Linear tables cannot be read, a stored value does not parse, or
/// `generated_at` is not RFC 3339.
pub(crate) fn linear_section(
    db: &Database,
    config: &Config,
    author_email: Option<&str>,
    generated_at: &str,
) -> Result<Option<String>> {
    let Some(linear) = config.linear.as_ref().filter(|l| l.stats.report) else {
        return Ok(None);
    };
    let conn = db.connection();
    let synced: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM linear_issues WHERE linear_id IS NOT NULL)",
            [],
            |r| r.get(0),
        )
        .map_err(|e| failed(TgaError::from(e)))?;
    if !synced {
        warn!(
            "linear.stats.report is on but the database holds no synced Linear issues; \
             run `tga linear sync`. The report carries no Linear section"
        );
        return Ok(None);
    }
    // #190: the reader is told in the report itself, not only in a log.
    if author_email.is_some() {
        return Ok(Some(format!("{AUTHOR_NOTE}\n")));
    }
    let as_of = DateTime::parse_from_rfc3339(generated_at)
        .map_err(|e| failed(format!("report timestamp '{generated_at}': {e}")))?
        .with_timezone(&Utc);
    let opts = StatsOptions::from_config(as_of, &linear.stats);
    let stats = compute_linear_stats(conn, &opts).map_err(failed)?;
    Ok(Some(render_markdown(&stats)))
}

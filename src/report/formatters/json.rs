//! JSON formatter — writes `report.json` (pretty-printed).

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::report::errors::Result;
use crate::report::models::ReportData;

/// Filename for the JSON output.
pub const REPORT_JSON: &str = "report.json";

/// Filename for the velocity summary JSON.
pub const VELOCITY_JSON: &str = "velocity_summary.json";

/// Filename for the quality summary JSON.
pub const QUALITY_JSON: &str = "quality_summary.json";

/// Filename for the DORA summary JSON.
pub const DORA_JSON: &str = "dora_summary.json";

/// Write `velocity_summary.json`.
///
/// Why: dashboards consume velocity metrics as structured JSON; we keep this
/// file separate from the omnibus `report.json` so it can be polled cheaply.
/// What: serializes [`crate::report::models::VelocitySummary`] (or `null`).
/// Test: invoke after aggregation, parse the resulting JSON, assert
/// `pr_count` matches the seeded PR rows.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Json`] on failure.
pub fn write_velocity_json(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(VELOCITY_JSON);
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, &data.velocity)?;
    debug!(path = %path.display(), "wrote velocity_summary.json");
    Ok(path)
}

/// Write `quality_summary.json`.
///
/// Why: quality scoring is consumed by retro tooling separately from the
/// activity dashboard, so it gets a dedicated file.
/// What: serializes [`crate::report::models::QualitySummary`] (or `null`).
/// Test: assert `quality_score` is in `[0, 1]` after writing.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Json`] on failure.
pub fn write_quality_json(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(QUALITY_JSON);
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, &data.quality)?;
    debug!(path = %path.display(), "wrote quality_summary.json");
    Ok(path)
}

/// Write `dora_summary.json`.
///
/// Why: keeps the DORA payload available even when the `csv` output format
/// is disabled.
/// What: serializes [`crate::report::models::DoraMetrics`] (or `null`).
/// Test: parse the resulting JSON, assert `performance_level` is one of
/// `"elite" | "high" | "medium" | "low"`.
///
/// # Errors
/// - [`crate::report::ReportError::Io`] / [`crate::report::ReportError::Json`] on failure.
pub fn write_dora_json(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(DORA_JSON);
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, &data.dora)?;
    debug!(path = %path.display(), "wrote dora_summary.json");
    Ok(path)
}

/// Serialize [`ReportData`] as pretty JSON into `<output_dir>/report.json`.
///
/// # Errors
///
/// - [`crate::report::ReportError::Io`] on write failure.
/// - [`crate::report::ReportError::Json`] on serialization failure.
pub fn write_json(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(REPORT_JSON);
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, data)?;
    debug!(path = %path.display(), "wrote report.json");
    Ok(path)
}

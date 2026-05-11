//! JSON formatter — writes `report.json` (pretty-printed).

use std::path::{Path, PathBuf};

use tracing::debug;

use crate::errors::Result;
use crate::models::ReportData;

/// Filename for the JSON output.
pub const REPORT_JSON: &str = "report.json";

/// Serialize [`ReportData`] as pretty JSON into `<output_dir>/report.json`.
///
/// # Errors
///
/// - [`crate::ReportError::Io`] on write failure.
/// - [`crate::ReportError::Json`] on serialization failure.
pub fn write_json(data: &ReportData, output_dir: &Path) -> Result<PathBuf> {
    let path = output_dir.join(REPORT_JSON);
    let file = std::fs::File::create(&path)?;
    serde_json::to_writer_pretty(file, data)?;
    debug!(path = %path.display(), "wrote report.json");
    Ok(path)
}

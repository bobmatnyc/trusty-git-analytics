//! `tga report` — stage 3 (report generation) entry point.

use tga::core::config::{Config, OutputConfig};
use tga::core::db::Database;
use tga::report::ReportPipeline;

use crate::ReportArgs;

/// Generate reports from the classified commits stored in `db`.
///
/// Applies `--output` and `--formats` CLI overrides on top of the
/// [`OutputConfig`] in the loaded YAML config.
pub fn run(config: Config, db: &Database, args: ReportArgs) -> anyhow::Result<()> {
    let mut cfg = config;

    // Materialize an OutputConfig if any override is supplied and none exists yet.
    if cfg.output.is_none() && (args.output.is_some() || !args.formats.is_empty()) {
        cfg.output = Some(OutputConfig::default());
    }
    if let Some(ref mut out) = cfg.output {
        if let Some(dir) = args.output {
            out.directory = Some(dir);
        }
        if !args.formats.is_empty() {
            out.formats = args.formats;
        }
    }

    let pipeline = ReportPipeline::new(cfg);
    let stats = pipeline.run(db)?;

    println!(
        "Generated {} report file(s) ({} commits, {} authors)",
        stats.files_written.len(),
        stats.total_commits,
        stats.total_authors
    );
    for f in &stats.files_written {
        println!("  {}", f.display());
    }
    Ok(())
}

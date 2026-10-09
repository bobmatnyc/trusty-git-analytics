//! The `linear.stats` block: lower bounds for `tga linear stats` (#190 step 4)
//! and the switch that puts the metrics into `tga report` (#190 step 5).
//!
//! Why: two Linear delivery metrics are usually read over a recent slice of
//! history, and the right slice depends on the engagement. A built-in date
//! would be one engagement's choice imposed on every other. A config that
//! only enriches commits with referenced issues should not grow a delivery
//! section, so the report section is opt-in.
//! What: [`LinearStatsConfig`]. Both bounds default to `None`, which measures
//! every issue. A value that is not a `YYYY-MM-DD` date fails the config load.
//! `report` defaults to `false`.
//! Test: `linear_stats_block_parses_both_dates`,
//! `linear_stats_block_defaults_to_no_bounds`,
//! `linear_stats_block_rejects_a_bad_date`,
//! `linear_stats_block_parses_the_report_switch`.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// Lower bounds for `tga linear stats` and the `tga report` switch, under
/// `linear.stats` in YAML.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct LinearStatsConfig {
    /// Field use (C9) counts only issues created on or after this UTC date.
    #[serde(default)]
    pub field_use_created_since: Option<NaiveDate>,
    /// Concentration (C14) counts only issues completed on or after this UTC
    /// date.
    #[serde(default)]
    pub concentration_completed_since: Option<NaiveDate>,
    /// Append the Linear delivery section to `report.md` (`tga report`,
    /// `tga analyze`) when the database holds synced Linear issues.
    // #190: opt-in, so existing configs keep a byte-identical report.
    #[serde(default)]
    pub report: bool,
}

#[cfg(test)]
mod tests {
    use super::super::LinearConfig;
    use super::*;

    fn parse(yaml: &str) -> Result<LinearConfig, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    #[test]
    fn linear_stats_block_parses_both_dates() {
        let cfg = parse(
            "stats:\n  field_use_created_since: 2025-01-01\n  \
             concentration_completed_since: 2026-01-01\n",
        )
        .expect("parses");
        assert_eq!(
            cfg.stats.field_use_created_since,
            NaiveDate::from_ymd_opt(2025, 1, 1)
        );
        assert_eq!(
            cfg.stats.concentration_completed_since,
            NaiveDate::from_ymd_opt(2026, 1, 1)
        );
    }

    #[test]
    fn linear_stats_block_defaults_to_no_bounds() {
        let cfg = parse("team_keys: [ENG]\n").expect("parses");
        assert_eq!(cfg.stats, LinearStatsConfig::default());
        assert!(!cfg.stats.report);
    }

    #[test]
    fn linear_stats_block_parses_the_report_switch() {
        let cfg = parse("stats:\n  report: true\n").expect("parses");
        assert!(cfg.stats.report);
        assert_eq!(cfg.stats.field_use_created_since, None);
    }

    #[test]
    fn linear_stats_block_rejects_a_bad_date() {
        let err = parse("stats:\n  field_use_created_since: last year\n").expect_err("fails");
        assert!(err.to_string().contains("field_use_created_since"), "{err}");
    }
}

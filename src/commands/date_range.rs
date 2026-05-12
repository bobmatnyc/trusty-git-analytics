//! Shared resolution of CLI date-window flags.
//!
//! Both `tga collect` and `tga analyze` accept three ways to specify the
//! collection window:
//!
//! - `--weeks N` (last N weeks; mutually exclusive with `--from`/`--to`)
//! - `--from DATE` / `--to DATE` (explicit ISO8601 bounds)
//! - `analysis.since_date` in the YAML config (fallback)
//!
//! [`resolve_date_range`] reduces these to a single `(since, until)` pair
//! of RFC3339 strings, which is what the per-repo overrides downstream
//! actually want.

use anyhow::{Context, Result};
use chrono::{Duration, NaiveDate, TimeZone, Utc};

/// Resolve CLI date flags into a `(since, until)` pair of RFC3339 strings.
///
/// Priority order (highest first):
/// 1. `weeks` — computes `since = now - N weeks`, leaves `until` open
/// 2. `from` / `to` — used as-is (interpreted as midnight UTC of that day)
/// 3. `config_since` — fallback lower bound from YAML
///
/// `from` and `to` must be `YYYY-MM-DD`. Both bounds are inclusive on the
/// calendar-day grain.
///
/// # Errors
///
/// Returns an error if `from` or `to` cannot be parsed as `YYYY-MM-DD`.
pub fn resolve_date_range(
    weeks: Option<u32>,
    from: Option<&str>,
    to: Option<&str>,
    config_since: Option<&str>,
) -> Result<(Option<String>, Option<String>)> {
    if let Some(n) = weeks {
        let cutoff = Utc::now() - Duration::weeks(i64::from(n));
        return Ok((Some(cutoff.to_rfc3339()), None));
    }

    if from.is_some() || to.is_some() {
        let from_rfc = match from {
            Some(s) => Some(parse_iso_date_to_rfc3339(s, false)?),
            None => config_since.map(str::to_string),
        };
        let to_rfc = match to {
            Some(s) => Some(parse_iso_date_to_rfc3339(s, true)?),
            None => None,
        };
        return Ok((from_rfc, to_rfc));
    }

    Ok((config_since.map(str::to_string), None))
}

/// Parse `YYYY-MM-DD` into an RFC3339 timestamp.
///
/// `end_of_day = true` returns 23:59:59 UTC, otherwise 00:00:00 UTC.
fn parse_iso_date_to_rfc3339(s: &str, end_of_day: bool) -> Result<String> {
    let d: NaiveDate = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .with_context(|| format!("invalid date '{s}' (expected YYYY-MM-DD)"))?;
    let (h, m, sec) = if end_of_day { (23, 59, 59) } else { (0, 0, 0) };
    let ndt = d
        .and_hms_opt(h, m, sec)
        .with_context(|| format!("invalid time-of-day for date '{s}'"))?;
    Ok(Utc.from_utc_datetime(&ndt).to_rfc3339())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weeks_wins_over_from_to() {
        let (since, until) =
            resolve_date_range(Some(2), Some("2024-01-01"), Some("2024-02-01"), None).unwrap();
        assert!(since.is_some());
        assert!(until.is_none(), "--weeks must leave until open");
    }

    #[test]
    fn from_and_to_parsed() {
        let (since, until) =
            resolve_date_range(None, Some("2024-01-01"), Some("2024-02-01"), None).unwrap();
        assert!(since.unwrap().starts_with("2024-01-01T00:00:00"));
        assert!(until.unwrap().starts_with("2024-02-01T23:59:59"));
    }

    #[test]
    fn config_since_fallback() {
        let (since, until) = resolve_date_range(None, None, None, Some("2023-06-01")).unwrap();
        assert_eq!(since.as_deref(), Some("2023-06-01"));
        assert!(until.is_none());
    }

    #[test]
    fn invalid_from_errors() {
        let err = resolve_date_range(None, Some("not-a-date"), None, None);
        assert!(err.is_err());
    }
}

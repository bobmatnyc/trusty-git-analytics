//! Merge status of sample rows for `tga eval score` (#111).
//!
//! Why: a commit with 2+ parents is a merge and is excluded from the eval.
//! A sample written before that rule carries no merge flag, so the scorer
//! must look each such row up in a tga database. A row whose status cannot
//! be determined is an error, never a silent non-merge.
//! What: [`resolve_merges`] returns one flag per sample row, taken from the
//! row's `is_merge` field or, when absent, from `commits.is_merge` by SHA.
//! Test: `tests/eval_harness.rs::score_excludes_merges_resolved_from_the_db`,
//! `tests/eval_harness.rs::score_refuses_rows_with_unknown_merge_status`.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use rusqlite::params_from_iter;

use super::records::{SampleRecord, StrataSummary, Stratum};
use super::{open_eval_db, EvalError, Result};

/// Merge flag per sample row, in sample order.
///
/// Why: see the module doc. What: rows with `is_merge` set keep it; the rest
/// are looked up in `db` (opened read-only), where a SHA stored more than
/// once counts as a merge when any copy is one.
///
/// # Errors
///
/// [`EvalError::Invalid`] when a row has no flag and `db` is `None`, or when
/// its SHA is not in `db`; database errors from the lookup.
pub(crate) fn resolve_merges(sample: &[SampleRecord], db: Option<&Path>) -> Result<Vec<bool>> {
    let unknown: Vec<&str> = sample
        .iter()
        .filter(|r| r.is_merge.is_none())
        .map(|r| r.sha.as_str())
        .collect();
    if unknown.is_empty() {
        return Ok(sample.iter().map(|r| r.is_merge == Some(true)).collect());
    }
    let Some(db) = db else {
        return Err(EvalError::Invalid(format!(
            "{} sample rows carry no merge flag (a sample written before merges were \
             excluded); pass --db with a copy of the tga database so merges can be \
             resolved by SHA and excluded",
            unknown.len()
        )));
    };
    let stored = lookup(db, &unknown)?;
    let flags: Vec<Option<bool>> = sample
        .iter()
        .map(|r| r.is_merge.or_else(|| stored.get(r.sha.as_str()).copied()))
        .collect();
    let missing: Vec<&str> = sample
        .iter()
        .zip(&flags)
        .filter(|(_, f)| f.is_none())
        .map(|(r, _)| r.sha.as_str())
        .collect();
    if let Some(first) = missing.first() {
        return Err(EvalError::Invalid(format!(
            "merge status unknown for {} sample rows: their SHAs are not in {} (first: {first}); \
             pass the database the sample was drawn from",
            missing.len(),
            db.display()
        )));
    }
    Ok(flags.into_iter().map(|f| f == Some(true)).collect())
}

/// Remove each stratum's estimated merge commits from `strata.json` counts.
///
/// Why: a `strata.json` written before merges were excluded counts them in
/// every stratum population, so stratum weights would still weigh merges
/// (#111). The window cannot be re-stratified without re-running the rules,
/// so the merge count is estimated from the sample.
/// What: for each stratum with `m` merge rows among its `n` sample rows, the
/// population drops by `round(population · m / n)`; `population` falls and
/// `merges_excluded` rises by the same total, which is returned. A sample
/// without merges leaves `strata` unchanged. The estimate assumes merges are
/// spread across the sample as in the window; the draw's per-repo and
/// per-author caps can under-sample merge-heavy integrators, so
/// [`count_window_merges`] reports the exact total beside it.
/// Test: `tests/eval_harness.rs::score_weights_strata_without_their_merges`.
pub(crate) fn scale_out_merges(
    strata: &mut StrataSummary,
    sample: &[SampleRecord],
    merges: &[bool],
) -> u64 {
    let mut total = 0;
    let mut rows: BTreeMap<Stratum, (u64, u64)> = BTreeMap::new();
    for (r, &m) in sample.iter().zip(merges) {
        let e = rows.entry(r.stratum).or_default();
        e.0 += 1;
        e.1 += u64::from(m);
    }
    for (stratum, (n, m)) in rows {
        let Some(counts) = strata.strata.get_mut(stratum.as_str()) else {
            continue;
        };
        if m == 0 {
            continue;
        }
        let removed = (counts.population as f64 * m as f64 / n as f64).round() as u64;
        let removed = removed.min(counts.population);
        counts.population -= removed;
        strata.population = strata.population.saturating_sub(removed);
        strata.merges_excluded += removed;
        total += removed;
    }
    total
}

/// Merge commits in the sampling window, counted exactly from the database.
///
/// Why: #111 — the per-stratum merge estimate is biased by the draw's caps;
/// the report shows the exact window total next to it. Strata are not
/// recomputed: they come from the rules at sampling time.
/// What: counts `commits.is_merge = 1` rows whose timestamp parses into
/// `[window_start, window_end]` over every repository, the same scope
/// `tga eval sample` builds its population from.
/// Test: `tests/eval_harness.rs::score_reports_exact_window_merges`.
///
/// # Errors
///
/// [`EvalError::Invalid`] for an unparseable window bound; database errors.
pub(crate) fn count_window_merges(db: &Path, strata: &StrataSummary) -> Result<u64> {
    let bound = |s: &str| {
        super::population::parse_ts(s).ok_or_else(|| {
            EvalError::Invalid(format!("strata.json window bound {s:?} is not a timestamp"))
        })
    };
    let (start, end) = (bound(&strata.window_start)?, bound(&strata.window_end)?);
    let conn = open_eval_db(db)?;
    let mut stmt = conn.prepare("SELECT timestamp FROM commits WHERE is_merge = 1")?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    let mut n = 0;
    for ts in rows {
        if super::population::parse_ts(&ts?).is_some_and(|t| t >= start && t <= end) {
            n += 1;
        }
    }
    Ok(n)
}

/// `commits.is_merge` for the wanted SHAs that the database holds.
fn lookup(db: &Path, shas: &[&str]) -> Result<HashMap<String, bool>> {
    let conn = open_eval_db(db)?;
    let mut out = HashMap::new();
    for chunk in shas.chunks(500) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT sha, MAX(is_merge) FROM commits WHERE sha IN ({placeholders}) GROUP BY sha"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(chunk.iter()), |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
        })?;
        for row in rows {
            let (sha, is_merge) = row?;
            out.insert(sha, is_merge);
        }
    }
    Ok(out)
}

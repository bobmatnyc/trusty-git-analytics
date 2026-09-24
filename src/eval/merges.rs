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

use std::collections::HashMap;
use std::path::Path;

use rusqlite::params_from_iter;

use super::records::SampleRecord;
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

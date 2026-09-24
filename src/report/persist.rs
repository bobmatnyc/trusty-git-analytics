//! Persistence helpers: UPSERT [`ReportData`] weekly slices into SQLite fact tables.
//!
//! Why: `aggregator.rs` was approaching its 500-line budget when the two persist
//! functions for `fact_weekly_quality` (issue #445) and `fact_weekly_engineer`
//! (issue #1113) were added. Extracting them here keeps each file focused and
//! within the project line-cap.
//! What: two public functions — [`persist_weekly_quality`] and
//! [`persist_weekly_engineer`] — each UPSERT one row per
//! [`crate::report::models::WeeklyActivity`] into the corresponding fact table.
//! Test: `report::tests::persist_weekly_quality_upserts_rows_and_is_idempotent` and
//! `report::tests::persist_weekly_engineer_upserts_rows`.

use std::collections::{HashMap, HashSet};

use tracing::warn;

use crate::core::db::Database;
use crate::core::quality::QUALITY_FORMULA_VERSION;
use crate::report::errors::Result;
use crate::report::models::ReportData;

/// Parse an ISO week label `"YYYY-Www"` into `(iso_year, iso_week)`.
///
/// Why: fact tables store year/week as separate INTEGER columns so warehouse
/// tools can filter without string parsing.
/// What: splits on `-W`, parses both halves as i64.
/// Test: exercised indirectly by both persist functions below.
pub(super) fn parse_week_label_to_parts(label: &str) -> Option<(i64, i64)> {
    let (y, w) = label.split_once("-W")?;
    let year: i64 = y.parse().ok()?;
    let week: i64 = w.parse().ok()?;
    Some((year, week))
}

/// Build a display-name → canonical-email lookup from [`ReportData::authors`].
///
/// Why: [`crate::report::models::WeeklyActivity::author`] carries the display
/// name (resolved at materialisation); the fact tables need the canonical email
/// as the grain key so they join correctly with `fact_weekly_quality`.
/// What: returns a `HashMap<display_name, canonical_email>`.
/// Test: exercised by both persist functions.
fn name_to_email_map(data: &ReportData) -> HashMap<String, String> {
    data.authors
        .iter()
        .map(|a| (a.name.clone(), a.email.clone()))
        .collect()
}

/// `fact_weekly_engineer.formula_version` written by this build (#111: `v2` excludes merges).
pub const ENGINEER_FORMULA_VERSION: &str = "v2";

/// Which rows a persist run is allowed to prune (#111).
///
/// The caller states it; it is never inferred from the data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistScope {
    /// Every commit was aggregated: rows of an older formula are removed for
    /// every author, and each written author's rows the run did not produce.
    Full,
    /// An `--author`-scoped report: only the authors the run wrote are
    /// pruned; every other author's rows, of any formula, are kept.
    Authors,
}

/// Grain key shared by both weekly fact tables:
/// `(author_email, iso_year, iso_week, repository)`.
type GrainKey = (String, i64, i64, String);

/// Remove rows a persist run did not produce, so no stale row survives it.
///
/// Why: #111 changed both weekly formulas (merges left the counts), and
/// `INSERT OR REPLACE` only rewrites the grain keys a run produces. A week
/// whose only commits were merges produces no row, so its old
/// merge-inclusive row would otherwise stay forever.
/// What: on a [`PersistScope::Full`] run, deletes every row whose
/// `formula_version` is not `version`. On any run, then deletes, for each
/// author the run wrote, every row whose grain key the run did not produce;
/// the aggregator reads each written author's whole history, so those rows
/// are stale. An [`PersistScope::Authors`] run touches no other author.
/// Test: `report::tests::persist_weekly_engineer_drops_stale_merge_only_rows`,
/// `report::tests::persist_weekly_quality_drops_stale_merge_only_rows`,
/// `report::tests::scoped_persist_keeps_other_authors_old_rows`.
fn prune_stale(
    db: &Database,
    table: &'static str,
    version: &str,
    written: &HashSet<GrainKey>,
    scope: PersistScope,
) -> Result<usize> {
    let conn = db.connection();
    let tx = conn
        .unchecked_transaction()
        .map_err(crate::core::TgaError::from)?;
    let mut removed = 0;
    if scope == PersistScope::Full {
        removed += tx
            .execute(
                &format!("DELETE FROM {table} WHERE formula_version != ?1"),
                [version],
            )
            .map_err(crate::core::TgaError::from)?;
    }
    let authors: HashSet<&str> = written.iter().map(|k| k.0.as_str()).collect();
    let stale: Vec<GrainKey> = {
        let mut stmt = tx
            .prepare(&format!(
                "SELECT author_email, iso_year, iso_week, repository FROM {table}"
            ))
            .map_err(crate::core::TgaError::from)?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .map_err(crate::core::TgaError::from)?;
        let mut out = Vec::new();
        for row in rows {
            let key: GrainKey = row.map_err(crate::core::TgaError::from)?;
            if authors.contains(key.0.as_str()) && !written.contains(&key) {
                out.push(key);
            }
        }
        out
    };
    for (email, year, week, repo) in &stale {
        removed += tx
            .execute(
                &format!(
                    "DELETE FROM {table} WHERE author_email = ?1 AND iso_year = ?2 \
                     AND iso_week = ?3 AND repository = ?4"
                ),
                rusqlite::params![email, year, week, repo],
            )
            .map_err(crate::core::TgaError::from)?;
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    Ok(removed)
}

/// Unix-epoch seconds for "now", used as `computed_at` in fact rows.
fn computed_at_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Persist per-engineer-per-week quality scores to `fact_weekly_quality`.
///
/// Why: downstream warehouses read `tga.db` directly; storing quality rows
/// avoids requiring consumers to re-implement the scoring formula in SQL.
/// This is called immediately after aggregation so stored values always
/// reflect the corrected ticketed logic from migration v17.
/// What: UPSERTs one row per [`crate::report::models::WeeklyActivity`] into
/// `fact_weekly_quality`, batching in chunks of 500, then removes rows the run
/// did not produce and rows of an older formula ([`prune_stale`]); #111
/// excludes merges from `commit_count` (formula `v2`). Rows whose ISO week label
/// cannot be parsed are skipped with a warning. Rows whose author display name
/// cannot be resolved to a canonical email are also skipped with a `warn!` — the
/// same policy as [`persist_weekly_engineer`]. This ensures both fact tables
/// share an identical grain key (author_email, iso_year, iso_week, repository)
/// so downstream joins between them are always consistent. Never write a display
/// name into the email-keyed column: doing so would produce rows that can never
/// join with other tables and would silently corrupt aggregate queries.
/// To fix unmapped identities run `tga aliases list` and add the missing mapping.
/// Test: `report::tests::persist_weekly_quality_upserts_rows_and_is_idempotent`,
/// `report::tests::persist_weekly_quality_drops_stale_merge_only_rows`.
///
/// # Errors
///
/// Returns [`ReportError::Core`](crate::report::ReportError::Core)(crate::report::ReportError::Core) if any SQLite operation fails.
pub fn persist_weekly_quality(
    db: &Database,
    data: &ReportData,
    scope: PersistScope,
) -> Result<usize> {
    if data.weekly_activity.is_empty() {
        return Ok(0);
    }
    let computed_at = computed_at_secs();
    let name_to_email = name_to_email_map(data);

    let rows: Vec<_> =
        data.weekly_activity
            .iter()
            .filter_map(|wa| {
                let (iso_year, iso_week) = match parse_week_label_to_parts(&wa.week) {
                    Some(p) => p,
                    None => {
                        warn!(
                            week = %wa.week,
                            "persist_weekly_quality: cannot parse week label; skipping row"
                        );
                        return None;
                    }
                };
                let quality_tshirt: i64 = wa.quality_tshirt.parse().unwrap_or(
                    crate::core::quality::size_for_quality_score(wa.quality_score) as i64,
                );
                Some((
                    wa.author.clone(),
                    iso_year,
                    iso_week,
                    wa.repository.clone(),
                    wa.quality_score,
                    quality_tshirt,
                    wa.revert_count as i64,
                    wa.bugfix_count as i64,
                    wa.ticketed_count as i64,
                    wa.commit_count as i64,
                    computed_at,
                ))
            })
            .collect();

    let mut written = 0usize;
    let mut keys: HashSet<GrainKey> = HashSet::new();
    for chunk in rows.chunks(500) {
        let conn = db.connection();
        let tx = conn
            .unchecked_transaction()
            .map_err(crate::core::TgaError::from)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO fact_weekly_quality \
                     (author_email, iso_year, iso_week, repository, quality_score, \
                      quality_tshirt, revert_count, bugfix_count, ticketed_count, \
                      commit_count, formula_version, computed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .map_err(crate::core::TgaError::from)?;
            for (author_display, iso_year, iso_week, repo, qs, qt, rc, bc, tc, cc, ca) in chunk {
                // Resolve display name → canonical email.
                // POLICY: skip rows that cannot be resolved rather than
                // falling back to the display name. Both fact tables
                // (fact_weekly_quality and fact_weekly_engineer) share the
                // grain key (author_email, iso_year, iso_week, repository).
                // Writing a display name into the email-keyed column would
                // produce rows that never join with the other table and
                // silently corrupt downstream aggregates. Run `tga aliases
                // list` to review and add unmapped identities.
                let author_email = match name_to_email.get(author_display) {
                    Some(e) => e.clone(),
                    None => {
                        warn!(
                            author = %author_display,
                            iso_year = iso_year,
                            iso_week = iso_week,
                            "persist_weekly_quality: no email mapping for author; \
                             skipping row to avoid corrupting the grain key. \
                             Run `tga aliases list` to review unmapped identities."
                        );
                        continue;
                    }
                };
                stmt.execute(rusqlite::params![
                    author_email,
                    iso_year,
                    iso_week,
                    repo,
                    qs,
                    qt,
                    rc,
                    bc,
                    tc,
                    cc,
                    QUALITY_FORMULA_VERSION,
                    ca,
                ])
                .map_err(crate::core::TgaError::from)?;
                keys.insert((author_email, *iso_year, *iso_week, repo.clone()));
                written += 1;
            }
        }
        tx.commit().map_err(crate::core::TgaError::from)?;
    }
    // #111: merges left the counts; drop rows this run did not rewrite.
    prune_stale(
        db,
        "fact_weekly_quality",
        QUALITY_FORMULA_VERSION,
        &keys,
        scope,
    )?;
    Ok(written)
}

/// Persist per-engineer-per-week agentic counts to `fact_weekly_engineer`.
///
/// Why: downstream warehouses (cto-reports) need agentic % per engineer per
/// ISO week without re-running the aggregator (issue #1113). Mirrors the
/// `fact_weekly_quality` pattern from issue #445 batch B.
/// What: UPSERTs one row per [`crate::report::models::WeeklyActivity`] into
/// `fact_weekly_engineer`. `net_commits` = `commit_count - revert_count`
/// over non-merge commits: #111 excludes merges (2+ parents) from metrics, so
/// they are in neither count (formula `v2`; `v1` counted them). `agentic_pct` = `agentic_count / net *
/// 100` (full-agentic only — excludes `ide_assisted_count`). Rows with
/// unresolvable author emails are skipped with a `warn!` to preserve
/// grain-key integrity. Rows the run does not produce, and every row of an
/// older formula, are then removed ([`prune_stale`]).
/// Test: `report::tests::persist_weekly_engineer_upserts_rows`,
/// `report::tests::persist_weekly_engineer_drops_stale_merge_only_rows`,
/// `report::tests::agentic_pct_keeps_unknown_in_the_denominator`.
///
/// # How `agentic_pct` treats `AgenticMode::Unknown` (#5250)
///
/// `unknown` commits stay in the `net_commits` DENOMINATOR and contribute to
/// neither numerator, exactly as `none` does. `agentic_pct` is therefore a
/// LOWER BOUND on agentic share: a repository whose merge and squash commits
/// were composed by the forge reports a smaller percentage than the work
/// warrants, and the size of that deficit is bounded by the unknown count.
///
/// Excluding them from the denominator was the alternative, and was rejected:
/// `fact_weekly_engineer` has no `unknown_count` column, so a reader of the
/// table could no longer reproduce `agentic_pct` from the columns beside it —
/// `net_commits` would stop being the denominator it is named for. Reporting
/// the unknown share needs that column, which is a migration and belongs with
/// the per-tool attribution work in
/// [#5251](https://github.com/bobmatnyc/trusty-tools/issues/5251).
///
/// # Errors
///
/// Returns [`ReportError::Core`](crate::report::ReportError::Core)(crate::report::ReportError::Core) if any SQLite operation fails.
pub fn persist_weekly_engineer(
    db: &Database,
    data: &ReportData,
    scope: PersistScope,
) -> Result<usize> {
    if data.weekly_activity.is_empty() {
        return Ok(0);
    }
    let computed_at = computed_at_secs();
    let name_to_email = name_to_email_map(data);

    let rows: Vec<_> = data
        .weekly_activity
        .iter()
        .filter_map(|wa| {
            let (iso_year, iso_week) = parse_week_label_to_parts(&wa.week)?;
            // net_commits = commit_count - revert_count, i.e.
            // `wa.commit_count_net` (issue #660).
            // #111: merge commits (2+ parents) are excluded from metrics, so
            // the aggregator never counts them here; this is formula `v2`
            // (`v1`, per #1113, included them).
            let net = wa.commit_count_net as i64;
            // agentic_pct = agentic_count / net * 100 — intentionally
            // EXCLUDES ide_assisted_count (full-agentic only, per #1113 spec).
            // ide_assisted is tracked separately and must NOT inflate the numerator.
            // #5250: `unknown` commits are in `net` and in neither numerator, so
            // this is a lower bound — see the fn doc for why they stay there.
            let agentic_pct = if net > 0 {
                (wa.agentic_count as f64) / (net as f64) * 100.0
            } else {
                0.0
            };
            Some((
                wa.author.clone(),
                iso_year,
                iso_week,
                wa.repository.clone(),
                net,
                wa.agentic_count as i64,
                wa.ide_assisted_count as i64,
                agentic_pct,
                computed_at,
            ))
        })
        .collect();

    let mut written = 0usize;
    let mut keys: HashSet<GrainKey> = HashSet::new();
    for chunk in rows.chunks(500) {
        let conn = db.connection();
        let tx = conn
            .unchecked_transaction()
            .map_err(crate::core::TgaError::from)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO fact_weekly_engineer \
                     (author_email, iso_year, iso_week, repository, \
                      net_commits, agentic_count, ide_assisted_count, agentic_pct, \
                      formula_version, computed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(crate::core::TgaError::from)?;
            for (author_display, iso_year, iso_week, repo, net, ac, ic, pct, ca) in chunk {
                // Resolve display name → canonical email for the grain key.
                // Skip rows that cannot be resolved: persisting a display
                // name as author_email corrupts the grain key and breaks
                // joins with fact_weekly_quality (issue #1113).
                let author_email = match name_to_email.get(author_display) {
                    Some(e) => e.clone(),
                    None => {
                        warn!(
                            author = %author_display,
                            iso_year = iso_year,
                            iso_week = iso_week,
                            "persist_weekly_engineer: no email mapping for author; \
                             skipping row to avoid corrupting the grain key. \
                             Run `tga aliases list` to review unmapped identities."
                        );
                        continue;
                    }
                };
                stmt.execute(rusqlite::params![
                    author_email,
                    iso_year,
                    iso_week,
                    repo,
                    net,
                    ac,
                    ic,
                    pct,
                    ENGINEER_FORMULA_VERSION,
                    ca,
                ])
                .map_err(crate::core::TgaError::from)?;
                keys.insert((author_email, *iso_year, *iso_week, repo.clone()));
                written += 1;
            }
        }
        tx.commit().map_err(crate::core::TgaError::from)?;
    }
    // #111: merges left `net_commits`; drop rows this run did not rewrite.
    prune_stale(
        db,
        "fact_weekly_engineer",
        ENGINEER_FORMULA_VERSION,
        &keys,
        scope,
    )?;
    Ok(written)
}

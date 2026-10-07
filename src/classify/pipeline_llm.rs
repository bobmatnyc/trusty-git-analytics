//! The pipeline's LLM fallback step: selection, calls, and token accounting.
//!
//! Why (#111): the accuracy plan runs the LLM only on commits the rules left
//! unanswered and must price that before any full run. Extracted from
//! `pipeline.rs` to keep it under the size cap.
//! What: [`llm_eligible`] decides which verdicts reach the LLM (shared with
//! `tga eval repredict`); `ClassificationPipeline::llm_enabled` says whether
//! the tier is on (#165); [`load_contexts`] reads the `llm.context` facts
//! (#111); [`run_llm_fallback`] makes the calls and totals the tokens;
//! [`record_usage`] writes one `llm_usage` row per call.
//! Test: `classify::pipeline_llm_tests`.

use std::collections::{BTreeMap, HashMap, HashSet};

use futures::stream::StreamExt;
use rusqlite::params;
use tracing::{info, warn};

use crate::classify::classifier::ClassificationEngine;
use crate::classify::errors::Result;
use crate::classify::tiers::llm_context::CommitContext;
use crate::classify::tiers::llm_prompt::{LlmOutcome, LlmUsage};
use crate::classify::tiers::ClassificationResult;
use crate::core::config::{LlmConfig, LlmContextItem, LlmFallbackScope};
use crate::core::db::Database;

use super::pipeline_db::CommitRow;

/// Whether the rules gave `r` no category.
///
/// True for the `uncategorized` placeholder a full cascade miss produces and
/// for the built-in `catch-all` rule (`maintenance/uncategorized`).
pub(crate) fn is_unanswered(r: &ClassificationResult) -> bool {
    r.category == "uncategorized" || r.subcategory.as_deref() == Some("uncategorized")
}

/// Whether the LLM fallback may revisit verdict `r` (#111).
///
/// Why: one predicate for `tga classify` and `tga eval repredict`, so the
/// eval replays exactly the routing the pipeline uses.
/// What: never a `repo_map` verdict (#158; in floor mode, #167, the map
/// applies after the LLM, so a mapped commit is eligible); otherwise
/// `LowConfidence` → `confidence <= threshold`; `Unanswered` →
/// [`is_unanswered`], threshold ignored.
/// Test: `pipeline_llm_tests::unanswered_scope_sends_only_abstentions`,
/// `pipeline_llm_tests::low_confidence_scope_also_sends_weak_rule_hits`,
/// `pipeline_repo_map_tests::mapped_repo_commits_never_reach_the_llm`,
/// `pipeline_repo_map_floor_tests::floor_overrides_a_non_exception_rule_and_llm_verdict`.
pub(crate) fn llm_eligible(
    scope: LlmFallbackScope,
    r: &ClassificationResult,
    threshold: f64,
) -> bool {
    // #158: the repo map is a hard override; the LLM never revisits it.
    if r.method == crate::core::models::ClassificationMethod::RepoMap {
        return false;
    }
    match scope {
        LlmFallbackScope::LowConfidence => r.confidence <= threshold,
        LlmFallbackScope::Unanswered => is_unanswered(r),
    }
}

impl super::pipeline::ClassificationPipeline {
    /// Whether this config turns the LLM tier on.
    ///
    /// Why (#165): `tga classify` and `tga rules test` must agree on it.
    /// What: true when the top-level `llm:` section is present (it enables
    /// the tier by itself, even with `classification.use_llm: false`), else
    /// `classification.use_llm`.
    /// Test: `tests/rules_test_cli.rs::rules_test_honours_weighted_sum_enabled_false`.
    pub(crate) fn llm_enabled(&self) -> bool {
        self.config.llm.is_some()
            || self
                .config
                .classification
                .as_ref()
                .is_some_and(|c| c.use_llm)
    }
}

/// The `llm.context` facts of every commit the LLM fallback will send.
///
/// Why (#111): the LLM sees the same facts the human rater sees in
/// `tga eval sample`, read by the same joins.
/// What: empty unless `llm.context` names an item. Otherwise, for each
/// commit [`run_llm_fallback`] will send (eligible, not a merge), reads only
/// the requested facts, applies the path caps, and keys the commits that
/// have any fact by their index in `commits`.
/// Test: `classify::llm_context_tests::context_paths_appends_a_paths_block`,
/// `classify::llm_context_tests::missing_context_facts_add_nothing`.
///
/// # Errors
///
/// A database read fails.
pub(super) fn load_contexts(
    db: &Database,
    llm: Option<&LlmConfig>,
    commits: &[CommitRow],
    results: &[ClassificationResult],
    scope: LlmFallbackScope,
    threshold: f64,
) -> Result<BTreeMap<usize, CommitContext>> {
    use crate::core::db::commit_context::{load_issue_types, load_paths, load_pr_titles};
    let Some(cfg) = llm.filter(|c| !c.context.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    let wants = |item| cfg.context.contains(&item);
    let sent: Vec<usize> = (0..commits.len())
        .filter(|&i| !commits[i].is_merge && llm_eligible(scope, &results[i], threshold))
        .collect();
    let conn = db.connection();
    let ids: Vec<i64> = sent.iter().map(|&i| commits[i].id).collect();
    let shas: HashSet<&str> = sent.iter().map(|&i| commits[i].sha.as_str()).collect();
    let db_err = crate::core::TgaError::from;
    let mut paths = HashMap::new();
    let mut prs = HashMap::new();
    let mut issues = HashMap::new();
    if wants(LlmContextItem::Paths) {
        paths = load_paths(conn, &ids).map_err(db_err)?;
    }
    if wants(LlmContextItem::PrTitle) {
        prs = load_pr_titles(conn, &shas).map_err(db_err)?;
    }
    if wants(LlmContextItem::IssueType) {
        issues = load_issue_types(conn, &shas).map_err(db_err)?;
    }
    let contexts: BTreeMap<usize, CommitContext> = sent
        .into_iter()
        .filter_map(|i| {
            let c = &commits[i];
            let ctx = CommitContext::new(
                paths.get(&c.id).map_or(&[][..], Vec::as_slice),
                cfg.context_max_paths,
                cfg.context_max_path_bytes,
                prs.get(&c.sha).map(String::as_str),
                issues.get(&c.sha).map(String::as_str),
            );
            (!ctx.is_empty()).then_some((i, ctx))
        })
        .collect();
    info!(
        with_context = contexts.len(),
        items = ?cfg.context,
        "LLM context loaded"
    );
    Ok(contexts)
}

/// Totals for one run's LLM calls (#111).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct LlmUsageTotals {
    /// LLM-tier calls in the run, one per eligible commit, including
    /// `skipped` ones that sent no request (#111).
    pub calls: usize,
    /// Answers that replaced the rule verdict.
    pub adopted: usize,
    /// Answers at or below the rule verdict's confidence, so kept out by the
    /// overwrite guard (#111 review).
    pub not_adopted: usize,
    /// Calls where the model chose the abstain label.
    pub abstained: usize,
    /// Calls answered outside the configured category set (dropped).
    pub out_of_set: usize,
    /// Calls with no usable reply.
    pub failed: usize,
    /// Calls that sent no request: spend cap reached or payload dump
    /// (#111, `source: jev`).
    pub skipped: usize,
    /// Calls whose provider reported token usage.
    pub calls_with_usage: usize,
    /// Sum of reported input tokens.
    pub input_tokens: u64,
    /// Sum of reported output tokens.
    pub output_tokens: u64,
    /// SHAs of the `skipped` calls, in commit order, so the caller can
    /// re-send exactly those commits with `--force --shas` (#111).
    pub skipped_shas: Vec<String>,
}

impl LlmUsageTotals {
    fn add(&mut self, outcome: &str, usage: Option<LlmUsage>) {
        self.calls += 1;
        match outcome {
            ADOPTED => self.adopted += 1,
            NOT_ADOPTED => self.not_adopted += 1,
            o if o == LlmOutcome::Abstained.as_str() => self.abstained += 1,
            o if o == LlmOutcome::OutOfSet.as_str() => self.out_of_set += 1,
            o if o == LlmOutcome::Skipped.as_str() => self.skipped += 1,
            _ => self.failed += 1,
        }
        if let Some(u) = usage {
            self.calls_with_usage += 1;
            self.input_tokens += u.input_tokens;
            self.output_tokens += u.output_tokens;
        }
    }
}

/// `llm_usage.outcome` for an answer that replaced the rule verdict.
const ADOPTED: &str = "adopted";
/// `llm_usage.outcome` for an answer the overwrite guard kept out.
const NOT_ADOPTED: &str = "not_adopted";

/// One call's accounting record, written to `llm_usage`.
pub(super) struct UsageRow {
    idx: usize,
    /// `adopted`, `not_adopted`, `abstained`, `out_of_set`, `failed` or
    /// `skipped`.
    outcome: &'static str,
    usage: Option<LlmUsage>,
    /// The model the reply named, when it named one (#111).
    model: Option<String>,
    /// The Jev commit-text mode, `real` or `obfuscated` (#111).
    text_mode: Option<&'static str>,
}

/// Run the LLM on every eligible verdict and fold adopted answers back in.
///
/// Why: see the module doc; fan-out is bounded by `concurrency`.
/// What: selects indices with [`llm_eligible`], skips merge commits (#111:
/// they keep their rule verdict; the count is logged), calls
/// [`ClassificationEngine::llm_classify_detailed`] through
/// `buffer_unordered`, and replaces `results[idx]` only when the LLM answered
/// with strictly higher confidence than the rule verdict. Abstentions,
/// out-of-set replies and failures keep the rule verdict.
/// Test: `pipeline_llm_tests::unanswered_scope_sends_only_abstentions`,
/// `pipeline_llm_tests::merge_commits_never_reach_the_llm`.
pub(super) async fn run_llm_fallback(
    engine: &ClassificationEngine,
    commits: &[CommitRow],
    contexts: &BTreeMap<usize, CommitContext>,
    results: &mut [ClassificationResult],
    scope: LlmFallbackScope,
    threshold: f64,
    concurrency: usize,
) -> (LlmUsageTotals, Vec<UsageRow>) {
    let mut skipped_merges = 0_usize;
    let mut pending: Vec<(usize, &str, Option<&CommitContext>)> = Vec::new();
    for (idx, c) in commits.iter().enumerate() {
        if !llm_eligible(scope, &results[idx], threshold) {
            continue;
        }
        // #111: merges are excluded from metrics and the eval, so an LLM call
        // on one is spend with no use. The count goes to the log.
        if c.is_merge {
            skipped_merges += 1;
        } else {
            pending.push((idx, c.message.as_str(), contexts.get(&idx)));
        }
    }
    info!(
        pending = pending.len(),
        skipped_merges,
        scope = ?scope,
        "LLM fallback selection"
    );

    let pb = super::pipeline_db::make_progress(pending.len() as u64, "LLM fallback");
    let pb_ref = &pb;
    let calls: Vec<_> =
        futures::stream::iter(pending.into_iter().map(|(idx, message, ctx)| async move {
            // Direct LLM dispatch — `engine.classify` would re-run the rule tiers
            // and stop at the verdict that triggered the fallback (issue #99).
            let call = engine.llm_classify_with_context(message, ctx).await;
            pb_ref.inc(1);
            (idx, call)
        }))
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;
    pb.finish_and_clear();

    let mut totals = LlmUsageTotals::default();
    let mut rows = Vec::with_capacity(calls.len());
    for (idx, call) in calls {
        let Some(call) = call else { continue };
        // Overwrite-guard: adopt only an answer that beats the rule verdict.
        let outcome = match call.verdict {
            Some(r) if r.confidence > results[idx].confidence => {
                results[idx] = r;
                ADOPTED
            }
            Some(r) => {
                warn!(
                    commit_idx = idx,
                    original_conf = results[idx].confidence,
                    new_conf = r.confidence,
                    "LLM fallback did not improve confidence; keeping original verdict"
                );
                NOT_ADOPTED
            }
            None => call.outcome.as_str(),
        };
        totals.add(outcome, call.usage);
        rows.push(UsageRow {
            idx,
            outcome,
            usage: call.usage,
            model: call.model,
            text_mode: call.text_mode,
        });
    }
    // #111: commit order, whatever order the calls finished in.
    rows.sort_by_key(|r| r.idx);
    totals.skipped_shas = rows
        .iter()
        .filter(|r| r.outcome == LlmOutcome::Skipped.as_str())
        .map(|r| commits[r.idx].sha.clone())
        .collect();
    (totals, rows)
}

/// Write one `llm_usage` row per call made in this run.
///
/// Why (#111): a cost report prices a run from these rows; `run_started_at`
/// groups one run's calls.
/// What: inserts `(commit, provider, model, outcome, tokens, text_mode)`
/// in one transaction.
/// Test: `pipeline_llm_tests::unanswered_scope_sends_only_abstentions`.
pub(super) fn record_usage(
    db: &mut Database,
    commits: &[CommitRow],
    rows: &[UsageRow],
    identity: (&str, &str),
    run_started_at: &str,
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let tx = db
        .connection_mut()
        .transaction()
        .map_err(crate::core::TgaError::from)?;
    {
        let mut stmt = tx
            .prepare(
                "INSERT INTO llm_usage (commit_id, commit_sha, provider, model, outcome, \
                 input_tokens, output_tokens, run_started_at, text_mode) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )
            .map_err(crate::core::TgaError::from)?;
        for row in rows {
            let commit = &commits[row.idx];
            stmt.execute(params![
                commit.id,
                commit.sha,
                identity.0,
                // #111: the model the reply named wins over the configured id.
                row.model.as_deref().unwrap_or(identity.1),
                row.outcome,
                row.usage.map(|u| u.input_tokens as i64),
                row.usage.map(|u| u.output_tokens as i64),
                run_started_at,
                row.text_mode,
            ])
            .map_err(crate::core::TgaError::from)?;
        }
    }
    tx.commit().map_err(crate::core::TgaError::from)?;
    Ok(())
}

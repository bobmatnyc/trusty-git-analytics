//! Per-item GitHub fetch failures, tallied and turned into run faults (#146).
//!
//! Why: the PR-list fetch and the reviewer fetch both caught a per-item error,
//! logged a warning, and carried on, so a field run dropped 20,916 HTTP 403s
//! and 119 HTTP 404s and still reported 0 failures. Both passes now apply the
//! same rule (owner ruling D28), and it lives here so the two cannot drift.
//! What: [`FetchFaults`] collects each failed item with its HTTP status. A 404
//! becomes one [`FaultSeverity::ItemSkipped`] warning (exit 0): the repository
//! or pull request is gone, and a re-run cannot fix that. Every other failure
//! — a 403, a 5xx after retries, a transport or parse error — becomes one
//! [`FaultSeverity::StageFailed`] fault, which makes `tga collect` exit
//! non-zero. Rate limits never reach this type; #6553 handles them upstream.
//! Test: `crate::collect::github_pipeline::tests::a_reviewer_fetch_404_is_a_counted_warning_not_a_stage_failure`,
//! `crate::collect::github_pipeline::tests::a_reviewer_fetch_403_fails_the_stage_once`,
//! `crate::collect::pr_pipeline::tests::a_pr_list_404_is_a_counted_warning_not_a_stage_failure`,
//! `crate::collect::pr_pipeline::tests::a_pr_list_403_fails_the_stage_once`.
//!
//! [`FaultSeverity::ItemSkipped`]: crate::collect::FaultSeverity::ItemSkipped
//! [`FaultSeverity::StageFailed`]: crate::collect::FaultSeverity::StageFailed

use std::collections::BTreeMap;

use crate::collect::errors::CollectError;
use crate::collect::fault::CollectionFault;

/// Most failed items one fault message lists by name; the rest are counted.
const MAX_LISTED: usize = 20;

/// The HTTP status a fetch error carries, if GitHub answered at all.
///
/// A non-success response reaches the caller as [`CollectError::Http`] via
/// `error_for_status`; every other variant has no status.
pub(crate) fn http_status(err: &CollectError) -> Option<u16> {
    match err {
        CollectError::Http(e) => e.status().map(|s| s.as_u16()),
        _ => None,
    }
}

/// Wording for one pass's fault messages.
pub(crate) struct FaultWording<'a> {
    /// Message prefix naming the pass, e.g. `github reviewers`.
    pub(crate) label: &'a str,
    /// What one item is, plural form, e.g. `pull request(s)`.
    pub(crate) noun: &'a str,
    /// The fetch that failed, e.g. `reviewer fetch`.
    pub(crate) fetch: &'a str,
    /// What the failure cost, e.g. `got no reviewer rows`.
    pub(crate) lost: &'a str,
}

/// Failed items of one fetch pass, split by how the run must treat them.
#[derive(Debug, Default)]
pub(crate) struct FetchFaults {
    /// Items that answered HTTP 404.
    missing: Vec<String>,
    /// Every other failure, with its status when GitHub answered.
    failed: Vec<(String, Option<u16>)>,
}

impl FetchFaults {
    /// Record one failed item (`owner/repo` or `owner/repo#N`).
    pub(crate) fn record(&mut self, item: String, status: Option<u16>) {
        if status == Some(404) {
            self.missing.push(item);
        } else {
            self.failed.push((item, status));
        }
    }

    /// Turn the tally into at most two faults: one warning for the 404s, one
    /// stage failure for everything else. `total` is how many items the pass
    /// attempted. Items are sorted so the text does not depend on completion
    /// order.
    pub(crate) fn into_faults(
        mut self,
        w: &FaultWording<'_>,
        total: usize,
    ) -> Vec<CollectionFault> {
        let mut out = Vec::new();
        if !self.missing.is_empty() {
            self.missing.sort();
            out.push(CollectionFault::item_skipped(format!(
                "{}: {} of {total} {} answered HTTP 404 on the {} and {}: {}. The repository \
                 or pull request is gone, renamed, or not visible to the token (see #146)",
                w.label,
                self.missing.len(),
                w.noun,
                w.fetch,
                w.lost,
                listed(self.missing.iter().map(String::clone)),
            )));
        }
        if !self.failed.is_empty() {
            self.failed.sort();
            let mut counts = BTreeMap::<String, usize>::new();
            for (_, status) in &self.failed {
                *counts.entry(status_label(*status)).or_default() += 1;
            }
            let counts = counts
                .iter()
                .map(|(l, n)| format!("{l}: {n}"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push(CollectionFault::stage_failed(format!(
                "{}: {} of {total} {} failed the {} and {} ({counts}): {}. An HTTP 403 usually \
                 means the token lacks the scope or SAML SSO authorization for that repository \
                 (see #146)",
                w.label,
                self.failed.len(),
                w.noun,
                w.fetch,
                w.lost,
                listed(
                    self.failed
                        .iter()
                        .map(|(item, s)| format!("{item} ({})", status_label(*s)))
                ),
            )));
        }
        out
    }
}

/// `HTTP 403`, or `no HTTP status` for a failure GitHub never answered.
fn status_label(status: Option<u16>) -> String {
    match status {
        Some(code) => format!("HTTP {code}"),
        None => "no HTTP status".to_string(),
    }
}

/// Join the first [`MAX_LISTED`] items and count the rest.
fn listed(items: impl ExactSizeIterator<Item = String>) -> String {
    let n = items.len();
    let mut s = items.take(MAX_LISTED).collect::<Vec<_>>().join(", ");
    if n > MAX_LISTED {
        s.push_str(&format!(", and {} more", n - MAX_LISTED));
    }
    s
}

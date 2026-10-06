//! Two-level scoring for `tga eval score` (#111): primary (bucket) and
//! secondary (fine category within the bucket).
//!
//! Why: the owner reads accuracy at two levels. Predicting `bug_fix` for an
//! `upkeep` commit is the right bucket and the wrong fine category; the fine
//! figure alone counts it only as wrong.
//! What: [`score_buckets`] derives each scored row's bucket pair from the
//! [`BucketMap`] — the same derivation for every arm, since it reads only the
//! predicted category — and computes stratum-weighted primary and secondary
//! accuracy with the estimator the fine figure uses, Wilson intervals on the
//! raw counts, and per-bucket rows.
//! Test: `tests/eval_harness.rs::score_reports_primary_and_secondary_accuracy`,
//! `tests/eval_harness.rs::score_moves_with_a_bucket_override`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::records::{SampleRecord, StrataSummary};
use super::score::{precision_rows, weighted_accuracy, PrecisionRow, WeightedAccuracy};
use crate::core::config::BucketMap;

/// One bucket's rows in [`BucketScores::per_bucket`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BucketRow {
    /// Bucket name.
    pub bucket: String,
    /// Scored rows whose label is in this bucket.
    pub labelled: u64,
    /// Scored rows whose prediction is in this bucket.
    pub predicted: u64,
    /// Primary accuracy over the rows labelled in this bucket (Wilson CI).
    pub primary: PrecisionRow,
    /// Secondary accuracy over the same rows; `None` for a bucket with one
    /// fine category.
    pub secondary: Option<PrecisionRow>,
}

/// Primary and secondary accuracy of one scored sample.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BucketScores {
    /// The map scored against.
    pub map: BucketMap,
    /// Stratum-weighted primary accuracy over every scored row with a
    /// bucketed label; `None` without one.
    pub primary: Option<WeightedAccuracy>,
    /// Stratum-weighted secondary accuracy over the rows whose label's
    /// bucket has more than one fine category.
    pub secondary: Option<WeightedAccuracy>,
    /// Unweighted primary counts with a Wilson interval (key `primary`).
    pub primary_counts: PrecisionRow,
    /// Unweighted secondary counts with a Wilson interval (key `secondary`).
    pub secondary_counts: PrecisionRow,
    /// Buckets scored at the secondary level, in map order.
    pub secondary_buckets: Vec<String>,
    /// One row per bucket, in map order.
    pub per_bucket: Vec<BucketRow>,
    /// Scored rows whose label the map does not name; left out of primary
    /// and secondary.
    pub unmapped_labels: u64,
    /// Scored rows whose prediction has no bucket (`uncategorized`, or a
    /// category the map does not name); wrong at both levels.
    pub unmapped_predictions: u64,
}

/// One scored row at both levels.
struct Scored<'a> {
    stratum: String,
    label_bucket: &'a str,
    primary: bool,
    /// `None` when the label's bucket has one fine category.
    secondary: Option<bool>,
}

/// Score `rows` — `(record, final label)` for every row whose label is not
/// a no-answer label — at both levels.
///
/// What: primary is correct when the prediction's bucket equals the label's;
/// secondary, scored only for a label in a bucket with more than one fine
/// category, is correct when the fine categories match. Weighted figures use
/// [`weighted_accuracy`] over per-stratum rows, as the fine figure does.
pub(super) fn score_buckets<'a>(
    map: &BucketMap,
    rows: impl Iterator<Item = (&'a SampleRecord, &'a str)>,
    strata: &StrataSummary,
) -> BucketScores {
    let (mut scored, mut unmapped_labels, mut unmapped_predictions) = (Vec::new(), 0, 0);
    let mut predicted_in: BTreeMap<&str, u64> = BTreeMap::new();
    for (r, label) in rows {
        let predicted = map.bucket_of(&r.predicted_category);
        match predicted {
            Some(b) => *predicted_in.entry(b).or_default() += 1,
            None => unmapped_predictions += 1,
        }
        let Some(label_bucket) = map.bucket_of(label) else {
            unmapped_labels += 1;
            continue;
        };
        scored.push(Scored {
            stratum: r.stratum.as_str().to_string(),
            label_bucket,
            primary: predicted == Some(label_bucket),
            secondary: map
                .has_secondary(label_bucket)
                .then(|| label.eq_ignore_ascii_case(r.predicted_category.trim())),
        });
    }
    let primary_strata =
        precision_rows(scored.iter().map(|s| (s.stratum.clone(), Some(s.primary))));
    let secondary_strata = precision_rows(
        scored
            .iter()
            .filter_map(|s| s.secondary.map(|ok| (s.stratum.clone(), Some(ok)))),
    );
    let overall = |key: &str, outcomes: Vec<bool>| {
        precision_rows(outcomes.into_iter().map(|ok| (key.to_string(), Some(ok))))
            .pop()
            .unwrap_or_else(|| PrecisionRow {
                key: key.to_string(),
                ..PrecisionRow::default()
            })
    };
    let per_bucket = map
        .buckets()
        .iter()
        .map(|b| {
            let mine: Vec<&Scored> = scored.iter().filter(|s| s.label_bucket == b.name).collect();
            let secondary: Vec<bool> = mine.iter().filter_map(|s| s.secondary).collect();
            BucketRow {
                bucket: b.name.clone(),
                labelled: mine.len() as u64,
                predicted: predicted_in.get(b.name.as_str()).copied().unwrap_or(0),
                primary: overall(&b.name, mine.iter().map(|s| s.primary).collect()),
                secondary: map
                    .has_secondary(&b.name)
                    .then(|| overall(&b.name, secondary)),
            }
        })
        .collect();
    BucketScores {
        map: map.clone(),
        primary: weighted_accuracy(&primary_strata, strata),
        secondary: weighted_accuracy(&secondary_strata, strata),
        primary_counts: overall("primary", scored.iter().map(|s| s.primary).collect()),
        secondary_counts: overall(
            "secondary",
            scored.iter().filter_map(|s| s.secondary).collect(),
        ),
        secondary_buckets: map
            .buckets()
            .iter()
            .filter(|b| map.has_secondary(&b.name))
            .map(|b| b.name.clone())
            .collect(),
        per_bucket,
        unmapped_labels,
        unmapped_predictions,
    }
}

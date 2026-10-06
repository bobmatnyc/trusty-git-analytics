//! The pipeline's view of `classification.buckets` (#111).
//!
//! Why: every arm (rules, Bedrock, Jev) reports primary and secondary from
//! one derivation: the bucket map applied to the fine category it chose. The
//! map is checked against the categories this config knows once, here.
//! What: [`ClassificationPipeline::known_categories`],
//! [`ClassificationPipeline::bucket_map`] and [`bucket_counts`].
//! Test: `classify::pipeline_buckets::tests`.

use std::collections::BTreeMap;

use crate::classify::errors::{ClassifyError, Result};
use crate::classify::pipeline::ClassificationPipeline;
use crate::core::config::{BucketMap, BucketSource};

/// Bucket name used for categories the map does not name.
pub const NO_BUCKET: &str = "(no bucket)";

impl ClassificationPipeline {
    /// Category names this config knows: its taxonomy plus every rule's
    /// category.
    ///
    /// # Errors
    ///
    /// A rules file fails to load or compile.
    pub fn known_categories(&self) -> Result<Vec<String>> {
        let engine = self.build_rule_engine()?;
        let taxonomy = engine.taxonomy().all().iter().map(|d| d.name.clone());
        Ok(taxonomy.chain(self.rule_categories()?).collect())
    }

    /// The bucket map in effect and where it came from, with structural
    /// checks only.
    ///
    /// Why (#111, owner ruling 2026-10-06): the consumer owns the bucket
    /// map; tga's built-in map is only a fallback.
    /// What: `classification.buckets` in the main config, else the
    /// top-level `buckets:` map of the rules files (a later file's map
    /// replaces an earlier one), else [`BucketMap::default`].
    /// Test: `tests::each_level_reports_its_source`,
    /// `tests::rules_file_buckets_apply_below_the_config_map`.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, or its map breaks a structural rule.
    pub fn effective_bucket_map(&self) -> Result<(BucketMap, BucketSource)> {
        let configured = self.config.classification.as_ref();
        if let Some(map) = configured.and_then(|c| c.buckets.clone()) {
            return Ok((map, BucketSource::Config));
        }
        let (ruleset, _) = self.load_ruleset()?;
        Ok(match ruleset.buckets {
            Some(map) => (map, BucketSource::RulesFile),
            None => (BucketMap::default(), BucketSource::Fallback),
        })
    }

    /// [`Self::effective_bucket_map`], checked against
    /// [`Self::known_categories`].
    /// Test: `tests::a_broken_rules_file_map_is_rejected`.
    ///
    /// # Errors
    ///
    /// A rules file fails to load, or a map names a category this config
    /// does not know ([`BucketMap::check_known`]).
    pub fn bucket_map_with_source(&self) -> Result<(BucketMap, BucketSource)> {
        let (map, source) = self.effective_bucket_map()?;
        map.check_known(&self.known_categories()?)
            .map_err(|e| ClassifyError::Config(e.to_string()))?;
        Ok((map, source))
    }

    /// The checked bucket map in effect; see [`Self::bucket_map_with_source`].
    ///
    /// # Errors
    ///
    /// As [`Self::bucket_map_with_source`].
    pub fn bucket_map(&self) -> Result<BucketMap> {
        self.bucket_map_with_source().map(|(map, _)| map)
    }
}

/// One bucket's share of a run's per-category counts.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BucketCount {
    /// Bucket name, or [`NO_BUCKET`].
    pub bucket: String,
    /// Commits in the bucket.
    pub total: u64,
    /// `(category, commits)`, sorted by category.
    pub categories: Vec<(String, u64)>,
}

/// Per-category counts rolled up to buckets.
///
/// What: buckets in map order, categories sorted within each; categories the
/// map does not name fall under [`NO_BUCKET`], last; empty buckets are left
/// out. A derived view only; nothing is stored.
/// Test: `tests::counts_roll_up_by_bucket`.
pub fn bucket_counts<'a>(
    map: &BucketMap,
    by_category: impl IntoIterator<Item = (&'a str, u64)>,
) -> Vec<BucketCount> {
    let by_category: BTreeMap<&str, u64> = by_category.into_iter().collect();
    let names = map.buckets().iter().map(|b| b.name.clone());
    let mut out: Vec<BucketCount> = names
        .chain([NO_BUCKET.to_string()])
        .map(|bucket| BucketCount {
            bucket,
            total: 0,
            categories: Vec::new(),
        })
        .collect();
    for (category, n) in by_category {
        let i = map
            .bucket_of(category)
            .and_then(|name| out.iter().position(|o| o.bucket == name))
            .unwrap_or(out.len() - 1);
        out[i].total += n;
        out[i].categories.push((category.to_string(), n));
    }
    out.retain(|o| o.total > 0);
    out
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::core::config::{ClassificationConfig, Config};

    /// Why: `tga classify` prints the bucket view as a derived field.
    /// What: `upkeep` and `bug_fix` sum under Maintenance; `uncategorized`
    /// and an unmapped category fall under `(no bucket)`; empty buckets drop.
    /// Test: this function.
    #[test]
    fn counts_roll_up_by_bucket() {
        let by = [
            ("upkeep", 2),
            ("bug_fix", 3),
            ("uncategorized", 4),
            ("other", 1),
        ];
        let rows = bucket_counts(&BucketMap::default(), by);
        assert_eq!(rows.len(), 2);
        assert_eq!((rows[0].bucket.as_str(), rows[0].total), ("Maintenance", 5));
        assert_eq!((rows[1].bucket.as_str(), rows[1].total), (NO_BUCKET, 5));
    }

    /// Why: an override naming a category the rules never emit is an error
    /// at the pipeline too, not only in the scorer.
    /// What: a rules file emitting `upkeep` and `qa`; an override naming
    /// `upkep` fails, a correct one loads.
    /// Test: this function.
    #[test]
    fn pipeline_rejects_an_override_with_an_unknown_category() {
        let mut rules = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .expect("tempfile");
        rules
            .write_all(
                b"extend_defaults: false\nrules:\n  - id: u\n    category: upkeep\n    \
                  keywords: [\"bump\"]\n  - id: q\n    category: qa\n    keywords: [\"test\"]\n",
            )
            .expect("write");
        let config = |buckets: &str| Config {
            classification: Some(ClassificationConfig {
                rules_files: vec![rules.path().to_path_buf()],
                buckets: Some(serde_yaml::from_str(buckets).expect("map")),
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        };
        let bad = ClassificationPipeline::new(config("M: [upkep, qa]"));
        let e = bad.bucket_map().expect_err("typo accepted").to_string();
        assert!(e.contains("upkep"), "{e}");
        let good = ClassificationPipeline::new(config("M: [upkeep]\nQ: [qa]"));
        assert_eq!(good.bucket_map().expect("ok").bucket_of("qa"), Some("Q"));
    }

    /// A rules file emitting `upkeep` and `qa`, with `extra` appended.
    fn rules_with(extra: &str) -> tempfile::NamedTempFile {
        let mut rules = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .expect("tempfile");
        let body = format!(
            "extend_defaults: false\nrules:\n  - id: u\n    category: upkeep\n    \
             keywords: [\"bump\"]\n  - id: q\n    category: qa\n    keywords: [\"test\"]\n{extra}"
        );
        rules.write_all(body.as_bytes()).expect("write");
        rules
    }

    fn pipeline(rules: &tempfile::NamedTempFile, buckets: Option<&str>) -> ClassificationPipeline {
        ClassificationPipeline::new(Config {
            classification: Some(ClassificationConfig {
                rules_files: vec![rules.path().to_path_buf()],
                buckets: buckets.map(|b| serde_yaml::from_str(b).expect("map")),
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        })
    }

    /// Why (#111, owner ruling 2026-10-06): the consumer owns the bucket
    /// map and may ship it in its rules file.
    /// What: a rules file's `buckets:` applies when the config has none; a
    /// `classification.buckets` map wins over it.
    /// Test: this function.
    #[test]
    fn rules_file_buckets_apply_below_the_config_map() {
        let rules = rules_with("buckets:\n  Keep: [upkeep]\n  Check: [qa]\n");
        let map = pipeline(&rules, None).bucket_map().expect("rules-file map");
        assert_eq!(map.bucket_of("qa"), Some("Check"));
        assert_eq!(map.buckets()[0].name, "Keep");
        let map = pipeline(&rules, Some("Run: [upkeep, qa]"))
            .bucket_map()
            .expect("config map");
        assert_eq!(map.bucket_of("qa"), Some("Run"));
    }

    /// Why (#111): callers (`tga rules list`, `tga classify`, Jev) must
    /// tell a consumer-supplied map from tga's built-in fallback.
    /// What: each precedence level reports its source; the fallback is the
    /// default map and is the only one not consumer-supplied.
    /// Test: this function.
    #[test]
    fn each_level_reports_its_source() {
        let plain = rules_with("");
        let mapped = rules_with("buckets:\n  Keep: [upkeep, qa]\n");
        for (rules, config, want) in [
            (&plain, None, BucketSource::Fallback),
            (&mapped, None, BucketSource::RulesFile),
            (&mapped, Some("Run: [upkeep]"), BucketSource::Config),
        ] {
            let (map, source) = pipeline(rules, config)
                .bucket_map_with_source()
                .expect("map");
            assert_eq!(source, want);
            assert_eq!(
                source.is_consumer_supplied(),
                want != BucketSource::Fallback
            );
            assert_eq!(map == BucketMap::default(), want == BucketSource::Fallback);
        }
    }

    /// Why (#111): a rules-file map gets the same checks as
    /// `classification.buckets`; a broken one must never be dropped.
    /// What: a category in two buckets, a no-answer label, and a category
    /// no rule knows are each a load error naming the problem.
    /// Test: this function.
    #[test]
    fn a_broken_rules_file_map_is_rejected() {
        for (extra, needle) in [
            ("buckets:\n  A: [qa]\n  B: [qa]\n", "more than one bucket"),
            ("buckets:\n  A: [unclear]\n", "no-answer"),
            ("buckets:\n  A: [upkep]\n", "upkep"),
        ] {
            let rules = rules_with(extra);
            let e = pipeline(&rules, None)
                .bucket_map()
                .expect_err(extra)
                .to_string();
            assert!(e.contains(needle), "{extra}: {e}");
        }
    }
}

//! `classification.buckets`: the two-level classification map (#111).
//!
//! Why: the owner reads a classification at two levels. The primary is a
//! bucket; the secondary is the fine category within it. Which bucket a fine
//! category belongs to is configuration, so a deployment can move one without
//! a code change. tga's `TopLevelCategory` is an older, different scheme and
//! is not reused.
//! What: [`BucketMap`], its default (owner ruling 2026-10-06), the structural
//! checks run at config load, and [`BucketMap::check_known`], the category
//! check against a config's vocabulary.
//! Test: `core::config::buckets::tests`,
//! `tests/eval_harness.rs::score_rejects_an_unknown_category_in_the_bucket_map`.

use std::collections::BTreeSet;
use std::fmt;

use serde::de::{self, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::core::errors::{Result, TgaError};

/// The default map: bucket name, then its fine categories, in report order.
pub const DEFAULT_BUCKETS: [(&str, &[&str]); 4] = [
    (
        "Maintenance",
        &["bug_fix", "devops", "security", "qa", "upkeep"],
    ),
    (
        "Value Creation",
        &["new_feature", "integration", "content_design"],
    ),
    (
        "Foundational Investment",
        &["platform_infrastructure", "data_science"],
    ),
    ("Internal Tooling", &["internal_tooling"]),
];

/// Fine categories a map may name although no rules file emits them yet.
pub const MAP_ONLY_CATEGORIES: [&str; 1] = ["content_design"];

/// Labels that never carry a bucket; a map naming one is rejected.
pub const NO_BUCKET_LABELS: [&str; 4] = ["unclear", "mixed", "release_merge", "uncategorized"];

/// One bucket and its fine categories (lower-case).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Bucket {
    /// Bucket name as configured, e.g. `Maintenance`.
    pub name: String,
    /// Fine categories in this bucket, lower-cased, in configured order.
    pub categories: Vec<String>,
}

/// Fine category → bucket, in configured order.
///
/// Invariant: at least one bucket; every bucket has a unique, non-blank name
/// and at least one category; every category is non-blank, lower-case,
/// appears in exactly one bucket and is not a [`NO_BUCKET_LABELS`] entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketMap {
    buckets: Vec<Bucket>,
}

impl Default for BucketMap {
    fn default() -> Self {
        Self {
            buckets: DEFAULT_BUCKETS
                .iter()
                .map(|(name, cats)| Bucket {
                    name: (*name).to_string(),
                    categories: cats.iter().map(|c| (*c).to_string()).collect(),
                })
                .collect(),
        }
    }
}

impl BucketMap {
    /// Build a map from `(bucket, categories)` pairs.
    ///
    /// What: trims names, lower-cases categories, and enforces the type's
    /// invariant.
    /// Test: `tests::structural_errors_are_rejected`.
    ///
    /// # Errors
    ///
    /// [`TgaError::ConfigError`] naming the first broken invariant.
    pub fn new(entries: Vec<(String, Vec<String>)>) -> Result<Self> {
        let err = |m: String| Err(TgaError::ConfigError(format!("bucket map: {m}")));
        if entries.is_empty() {
            return err("names no bucket".into());
        }
        let (mut names, mut seen) = (BTreeSet::new(), BTreeSet::new());
        let mut buckets = Vec::with_capacity(entries.len());
        for (name, cats) in entries {
            let name = name.trim().to_string();
            if name.is_empty() {
                return err("a bucket name is blank".into());
            }
            if !names.insert(name.to_lowercase()) {
                return err(format!("bucket {name:?} is listed twice"));
            }
            if cats.is_empty() {
                return err(format!("bucket {name:?} has no categories"));
            }
            let mut categories = Vec::with_capacity(cats.len());
            for c in cats {
                let c = c.trim().to_lowercase();
                if c.is_empty() {
                    return err(format!("bucket {name:?} has a blank category"));
                }
                if NO_BUCKET_LABELS.contains(&c.as_str()) {
                    return err(format!("{c:?} is a no-answer label and has no bucket"));
                }
                if !seen.insert(c.clone()) {
                    return err(format!("category {c:?} is in more than one bucket"));
                }
                categories.push(c);
            }
            buckets.push(Bucket { name, categories });
        }
        Ok(Self { buckets })
    }

    /// The buckets, in configured order.
    pub fn buckets(&self) -> &[Bucket] {
        &self.buckets
    }

    /// The bucket (primary) of a fine category, case-insensitively; `None`
    /// for a category the map does not name, `uncategorized` included.
    pub fn bucket_of(&self, category: &str) -> Option<&str> {
        let c = category.trim();
        self.buckets
            .iter()
            .find(|b| b.categories.iter().any(|x| x.eq_ignore_ascii_case(c)))
            .map(|b| b.name.as_str())
    }

    /// Whether a bucket's rows are scored at the secondary level: true when
    /// it holds more than one fine category (a one-category bucket's
    /// secondary is always its primary).
    pub fn has_secondary(&self, bucket: &str) -> bool {
        self.buckets
            .iter()
            .any(|b| b.name == bucket && b.categories.len() > 1)
    }

    /// Every fine category, in configured order.
    pub fn fine_categories(&self) -> impl Iterator<Item = &str> {
        self.buckets
            .iter()
            .flat_map(|b| b.categories.iter().map(String::as_str))
    }

    /// Check that an overriding map names only categories the config knows.
    ///
    /// Why: a misspelt category in an override would silently leave its
    /// rows without a bucket. The default map is the owner's table and is
    /// accepted as is, so a config with another scheme still loads.
    /// What: for a map other than the default, every fine category must be
    /// in `known` (case-insensitive) or in [`MAP_ONLY_CATEGORIES`].
    /// Test: `tests::an_override_naming_an_unknown_category_is_rejected`.
    ///
    /// # Errors
    ///
    /// [`TgaError::ConfigError`] listing every unknown category.
    pub fn check_known(&self, known: &[String]) -> Result<()> {
        if *self == Self::default() {
            return Ok(());
        }
        let known: BTreeSet<String> = known.iter().map(|k| k.to_lowercase()).collect();
        let unknown: Vec<&str> = self
            .fine_categories()
            .filter(|c| !known.contains(*c) && !MAP_ONLY_CATEGORIES.contains(c))
            .collect();
        if unknown.is_empty() {
            Ok(())
        } else {
            Err(TgaError::ConfigError(format!(
                "the bucket map names categories this config does not know: {}",
                unknown.join(", ")
            )))
        }
    }
}

/// Where the bucket map in effect came from (#111).
///
/// Why (owner ruling 2026-10-06): downstream consumers own the bucket map
/// and supply it as config; tga's built-in map is only a fallback for a
/// consumer that supplies none. Callers need to tell the levels apart.
/// What: the three levels, highest precedence first. Serialized as
/// `config`, `rules_file` and `fallback`.
/// Test: `classify::pipeline_buckets::tests::each_level_reports_its_source`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BucketSource {
    /// `classification.buckets` in the main config.
    Config,
    /// A top-level `buckets:` map in a `classification.rules_file`.
    RulesFile,
    /// [`BucketMap::default`]: neither of the above supplies a map.
    Fallback,
}

impl BucketSource {
    /// Whether a consumer supplied the map (config or rules file).
    pub fn is_consumer_supplied(self) -> bool {
        !matches!(self, Self::Fallback)
    }

    /// Where the map came from, for console output.
    pub fn describe(self) -> &'static str {
        match self {
            Self::Config => "classification.buckets",
            Self::RulesFile => "the rules file",
            Self::Fallback => "tga's built-in fallback",
        }
    }
}

impl super::Config {
    /// `classification.buckets`, else the built-in fallback. Structural
    /// checks only; see [`BucketMap::check_known`].
    ///
    /// This ignores a rules file's `buckets:` map, which only the
    /// classifier loads: the map in effect is
    /// `crate::classify::ClassificationPipeline::bucket_map_with_source`.
    pub fn bucket_map(&self) -> BucketMap {
        self.classification
            .as_ref()
            .and_then(|c| c.buckets.clone())
            .unwrap_or_default()
    }
}

impl Serialize for BucketMap {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.buckets.len()))?;
        for b in &self.buckets {
            map.serialize_entry(&b.name, &b.categories)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for BucketMap {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct Ordered;
        impl<'de> Visitor<'de> for Ordered {
            type Value = Vec<(String, Vec<String>)>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of bucket name to a list of categories")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut access: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut out = Vec::new();
                while let Some(entry) = access.next_entry()? {
                    out.push(entry);
                }
                Ok(out)
            }
        }
        let entries = deserializer.deserialize_map(Ordered)?;
        Self::new(entries).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(yaml: &str) -> std::result::Result<BucketMap, serde_yaml::Error> {
        serde_yaml::from_str(yaml)
    }

    /// Why: the default is the owner's table, and the order is report order.
    /// What: `upkeep` is Maintenance, `content_design` Value Creation,
    /// `internal_tooling` has no secondary; no-answer labels have no bucket.
    /// Test: this function.
    #[test]
    fn default_map_matches_the_ruling() {
        let m = BucketMap::default();
        let names: Vec<&str> = m.buckets().iter().map(|b| b.name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Maintenance",
                "Value Creation",
                "Foundational Investment",
                "Internal Tooling"
            ]
        );
        assert_eq!(m.bucket_of("Upkeep"), Some("Maintenance"));
        assert_eq!(m.bucket_of("content_design"), Some("Value Creation"));
        assert_eq!(m.bucket_of("data_science"), Some("Foundational Investment"));
        for label in NO_BUCKET_LABELS {
            assert_eq!(m.bucket_of(label), None, "{label}");
        }
        assert!(m.has_secondary("Maintenance"));
        assert!(!m.has_secondary("Internal Tooling"));
        assert_eq!(m.fine_categories().count(), 11);
    }

    /// Why: a broken map must fail at load, never drop rows later.
    /// What: empty map, empty bucket, a category in two buckets, a
    /// no-answer label, a duplicate bucket; YAML order is kept.
    /// Test: this function.
    #[test]
    fn structural_errors_are_rejected() {
        for (yaml, needle) in [
            ("{}", "no bucket"),
            ("A: []", "no categories"),
            ("A: [qa]\nB: [QA]", "more than one bucket"),
            ("A: [unclear]", "no-answer"),
            ("A: [qa]\na: [devops]", "listed twice"),
        ] {
            let e = parse(yaml).expect_err(yaml).to_string();
            assert!(e.contains(needle), "{yaml}: {e}");
        }
        let m = parse("Zeta: [qa]\nAlpha: [devops, upkeep]").expect("ordered");
        assert_eq!(m.buckets()[0].name, "Zeta");
        let back = serde_yaml::to_string(&m).expect("serialize");
        assert_eq!(parse(&back).expect("round trip"), m);
    }

    /// Why: a misspelt category in an override is an error, not a silent
    /// drop (#111); `content_design` is accepted though no rule emits it.
    /// What: an override naming `upkep` is rejected against the known set;
    /// one moving `qa` and naming `content_design` passes; the default map
    /// passes against any set.
    /// Test: this function.
    #[test]
    fn an_override_naming_an_unknown_category_is_rejected() {
        let known: Vec<String> = ["qa", "upkeep", "devops"].map(String::from).to_vec();
        let bad = parse("Maintenance: [upkep]\nOther: [qa]").expect("parse");
        let e = bad
            .check_known(&known)
            .expect_err("typo accepted")
            .to_string();
        assert!(e.contains("upkep") && !e.contains("qa"), "{e}");
        let good = parse("Maintenance: [upkeep, devops]\nOther: [qa, content_design]").expect("p");
        good.check_known(&known).expect("known");
        BucketMap::default().check_known(&[]).expect("default");
    }
}

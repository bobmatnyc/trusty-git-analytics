//! File formats shared by `tga eval sample` and `tga eval score`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::classify::TraceTier;

/// Sampling stratum of a verdict.
///
/// The first six are the strata the design names; `Fuzzy` and `RegexOther`
/// split out the remaining rule tiers, and `Other` holds verdicts decided
/// outside the rule engine (manual override, external source, LLM, issue
/// type, JIRA project, repo fallback) so every commit belongs to one stratum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stratum {
    /// Exact-keyword rules.
    Exact,
    /// Regex rules with confidence ≥ 0.9.
    RegexHigh,
    /// Regex rules with confidence in [0.55, 0.7].
    RegexMid,
    /// Regex rules outside both bands (other than the catch-all).
    RegexOther,
    /// The weighted-sum signal model.
    WeightedSum,
    /// Fuzzy heuristics.
    Fuzzy,
    /// The built-in catch-all regex rule.
    CatchAll,
    /// No tier matched, or the verdict is `uncategorized` / Unknown.
    Unknown,
    /// Verdicts decided outside the rule engine.
    Other,
}

impl Stratum {
    /// Every stratum, in report order.
    pub const ALL: [Stratum; 9] = [
        Stratum::Exact,
        Stratum::RegexHigh,
        Stratum::RegexMid,
        Stratum::RegexOther,
        Stratum::WeightedSum,
        Stratum::Fuzzy,
        Stratum::CatchAll,
        Stratum::Unknown,
        Stratum::Other,
    ];

    /// Stable snake_case label, identical to the serde form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::RegexHigh => "regex_high",
            Self::RegexMid => "regex_mid",
            Self::RegexOther => "regex_other",
            Self::WeightedSum => "weighted_sum",
            Self::Fuzzy => "fuzzy",
            Self::CatchAll => "catch_all",
            Self::Unknown => "unknown",
            Self::Other => "other",
        }
    }

    /// Assign a verdict to its stratum.
    ///
    /// Why: strata must partition the population so stratum weights sum to
    /// it. What: catch-all wins over the category test (its category is a
    /// real rule output); an `uncategorized`/`unknown` category otherwise
    /// lands in `Unknown` whatever tier produced it.
    /// Test: `eval::draw::tests::strata_partition_tiers`.
    pub fn classify(tier: TraceTier, category: &str, confidence: f64) -> Self {
        if tier == TraceTier::CatchAll {
            return Self::CatchAll;
        }
        if tier == TraceTier::Unclassified
            || category.eq_ignore_ascii_case("uncategorized")
            || category.eq_ignore_ascii_case("unknown")
        {
            return Self::Unknown;
        }
        match tier {
            TraceTier::Exact => Self::Exact,
            TraceTier::Regex if confidence >= 0.9 => Self::RegexHigh,
            TraceTier::Regex if (0.55..=0.7).contains(&confidence) => Self::RegexMid,
            TraceTier::Regex => Self::RegexOther,
            TraceTier::WeightedSum => Self::WeightedSum,
            TraceTier::Fuzzy => Self::Fuzzy,
            _ => Self::Other,
        }
    }
}

/// Lines changed by a commit.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diffstat {
    /// Files touched.
    pub files: i64,
    /// Lines added.
    pub insertions: i64,
    /// Lines removed.
    pub deletions: i64,
}

/// One sampled commit, one line of `sample.jsonl`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleRecord {
    /// Commit SHA.
    pub sha: String,
    /// Repository as stored in the database.
    pub repo: String,
    /// Commit timestamp as stored.
    pub date: String,
    /// Salted BLAKE3 of the lower-cased author e-mail (16 hex chars).
    pub author_hash: String,
    /// First line of the message.
    pub subject: String,
    /// Remainder of the message.
    pub body: String,
    /// Changed file paths.
    pub paths: Vec<String>,
    /// Files and lines changed.
    pub diffstat: Diffstat,
    /// Title of a pull request containing the commit.
    pub pr_title: Option<String>,
    /// Ticket id extracted for the commit.
    pub ticket_id: Option<String>,
    /// PM issue type of a linked work item.
    pub issue_type: Option<String>,
    /// Sampling stratum.
    pub stratum: Stratum,
    /// Tier that produced the verdict (`TraceTier` label).
    pub method: String,
    /// Stable rule id (see `RuleTrace`).
    pub rule_id: String,
    /// Predicted category.
    pub predicted_category: String,
    /// Verdict confidence.
    pub confidence: f64,
    /// Stratum population ÷ stratum sample size.
    pub weight: f64,
}

/// Population and sample counts of one stratum.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StratumCounts {
    /// Commits in the window assigned to this stratum.
    pub population: u64,
    /// Commits sampled from it.
    pub sampled: u64,
}

/// Contents of `strata.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StrataSummary {
    /// Seed the sample was drawn with.
    pub seed: u64,
    /// Window length in weeks.
    pub weeks: u32,
    /// Inclusive window start (RFC 3339).
    pub window_start: String,
    /// Window end: the newest commit timestamp in the database (RFC 3339).
    pub window_end: String,
    /// Requested sample size.
    pub requested_size: u64,
    /// Per-repo and per-author cap within a stratum.
    pub cap: u64,
    /// Commits in the window.
    pub population: u64,
    /// Per-stratum counts, keyed by stratum label.
    pub strata: BTreeMap<String, StratumCounts>,
    /// Valid rater labels other than `unclear` and `mixed`.
    pub categories: Vec<String>,
    /// Set when this file describes a `tga eval subsample` subset; `sampled`
    /// then counts the subset's rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subsample: Option<SubsampleOrigin>,
}

/// How a subset was drawn from a source sample (#111).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubsampleOrigin {
    /// Seed the subset was drawn with.
    pub seed: u64,
    /// Rows in the subset.
    pub size: u64,
    /// Rows in the source `sample.jsonl`.
    pub source_size: u64,
}

impl StrataSummary {
    /// Population of one stratum (0 when absent).
    pub fn population_of(&self, stratum: Stratum) -> u64 {
        self.strata
            .get(stratum.as_str())
            .map(|c| c.population)
            .unwrap_or(0)
    }
}

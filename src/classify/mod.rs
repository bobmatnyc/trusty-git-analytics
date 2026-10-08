//! Stage 2 of the pipeline: classify each collected commit using a four-tier
//! cascade.
//!
//! ## Tiers
//!
//! 1. **Exact** — Aho-Corasick multi-keyword match (case-insensitive).
//! 2. **Regex** — pre-compiled regex patterns.
//! 3. **Fuzzy** — structural heuristics (merge/revert/ticket-prefix).
//! 4. **LLM** — optional async fallback via an OpenAI-compatible API.
//!
//! Tiers 1–3 are synchronous and run in parallel across commits via Rayon.
//! Tier 4 is async and serialized.

pub mod classifier;
pub mod errors;
pub mod pipeline;
// #111: the two-level bucket map as the pipeline applies it.
pub mod pipeline_buckets;
pub(super) mod pipeline_db;
pub(super) mod pipeline_external;
// #111: the LLM fallback step and its token accounting.
pub(crate) mod pipeline_llm;
// #111: Jev category set and names to pseudonymize.
mod pipeline_jev;
// #158: the repo -> category map (hard override; #167 floor mode).
pub(crate) mod pipeline_repo_map;
// #167: `<repo>` / `<repo>:<prefix>` keys and path resolution.
pub(crate) mod repo_map_keys;
pub mod rules;
pub mod sources;
pub mod taxonomy;
pub mod tiers;
// #111: in-memory rule tracing for the eval harness; never persisted.
pub mod trace;

pub use classifier::{ClassificationEngine, ClassificationEngineConfig};
pub use errors::{ClassifyError, Result};
pub use pipeline::{ClassificationPipeline, ClassificationStats};
pub use pipeline_llm::LlmUsageTotals;
pub use rules::{Rule, RuleSet};
pub use taxonomy::{SubcategoryDef, TaxonomyRegistry, TopLevelCategory};
pub use tiers::ClassificationResult;
pub use trace::{RuleSources, RuleTrace, TraceTier, TracedVerdict};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod pipeline_tests;

#[cfg(test)]
mod pipeline_llm_tests;

#[cfg(test)]
mod llm_context_tests;

#[cfg(test)]
mod trace_tests;

// #158: the repo → category hard override, end to end.
#[cfg(test)]
mod pipeline_repo_map_tests;

// #167: repo map v2 — floor mode, path-prefix keys, unmatched-key warning.
#[cfg(test)]
mod pipeline_repo_map_floor_tests;

// #165: the weighted-sum tier stays inside the active category set.
#[cfg(test)]
mod weighted_sum_taxonomy_tests;

// #175: `use_llm: false` builds no LLM client and sends nothing.
#[cfg(test)]
mod use_llm_off_tests;

// #182: `is_revert` is the verdict OR the commit-message revert match.
#[cfg(test)]
mod revert_flag_tests;

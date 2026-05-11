//! # tga-classify
//!
//! Stage 2 of the `trusty-git-analytics` pipeline: classify each collected
//! commit using a four-tier cascade.
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
//!
//! ## Example
//!
//! ```no_run
//! use tga_classify::{ClassificationEngine, ClassificationEngineConfig};
//! use tga_classify::rules::default_rules;
//!
//! # async fn run() -> tga_classify::Result<()> {
//! let engine = ClassificationEngine::new(default_rules(), ClassificationEngineConfig::default())?;
//! let verdict = engine.classify("feat: add login", false).await;
//! assert_eq!(verdict.category, "feature");
//! # Ok(())
//! # }
//! ```

#![warn(missing_docs)]

pub mod classifier;
pub mod errors;
pub mod pipeline;
pub mod rules;
pub mod tiers;

pub use classifier::{ClassificationEngine, ClassificationEngineConfig};
pub use errors::{ClassifyError, Result};
pub use pipeline::{ClassificationPipeline, ClassificationStats};
pub use rules::{Rule, RuleSet};
pub use tiers::ClassificationResult;

#[cfg(test)]
mod tests {
    use super::*;
    use tga_core::models::ClassificationMethod;

    #[test]
    fn default_rules_is_non_empty() {
        let rs = rules::default_rules();
        assert!(!rs.rules.is_empty());
        let ids: Vec<&str> = rs.rules.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"cc-feat"));
        assert!(ids.contains(&"cc-fix"));
        assert!(ids.contains(&"jira-ticket"));
    }

    #[test]
    fn exact_matcher_classifies_feat() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        let r = m.classify("feat: add login flow").expect("match");
        assert_eq!(r.category, "feature");
    }

    #[test]
    fn exact_matcher_classifies_fix() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        let r = m
            .classify("fix: null pointer in user lookup")
            .expect("match");
        assert_eq!(r.category, "bugfix");
    }

    #[test]
    fn exact_matcher_returns_none_for_unknown() {
        let rs = rules::default_rules();
        let m = tiers::exact::ExactMatcher::new(&rs.rules).expect("build");
        assert!(m
            .classify("the rain in spain falls mainly on the plain")
            .is_none());
    }

    #[test]
    fn regex_matcher_classifies_jira_ticket() {
        let rs = rules::default_rules();
        let m = tiers::regex_tier::RegexMatcher::new(&rs.rules).expect("build");
        let r = m
            .classify("PROJ-123 implement payment flow")
            .expect("match");
        assert_eq!(r.category, "feature");
    }

    #[test]
    fn regex_matcher_extracts_ticket_id() {
        let id =
            tiers::regex_tier::RegexMatcher::extract_ticket_id("Implement PROJ-456 with new logic");
        assert_eq!(id.as_deref(), Some("PROJ-456"));
    }

    #[test]
    fn fuzzy_detects_merge_via_flag() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f.classify("arbitrary message", true).expect("match");
        assert_eq!(r.category, "merge");
        assert_eq!(r.method, ClassificationMethod::FuzzyMatch);
    }

    #[test]
    fn fuzzy_detects_merge_via_text() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f
            .classify("Merge pull request #42 from feature/x", false)
            .expect("match");
        assert_eq!(r.category, "merge");
    }

    #[test]
    fn fuzzy_detects_revert() {
        let f = tiers::fuzzy::FuzzyClassifier;
        let r = f
            .classify("Revert \"feat: add buggy feature\"", false)
            .expect("match");
        assert_eq!(r.category, "revert");
    }

    #[test]
    fn engine_classify_batch_does_not_panic() {
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig::default(),
        )
        .expect("engine");
        let pairs: Vec<(&str, bool)> = vec![
            ("feat: add login", false),
            ("fix: null deref", false),
            ("docs: update readme", false),
            ("Merge branch 'main' into x", true),
            ("PROJ-1 minor update", false),
            ("totally random text", false),
        ];
        let results = engine.classify_batch(&pairs);
        assert_eq!(results.len(), pairs.len());
        assert_eq!(results[0].category, "feature");
        assert_eq!(results[1].category, "bugfix");
        assert_eq!(results[2].category, "documentation");
        assert_eq!(results[3].category, "merge");
    }

    #[tokio::test]
    async fn engine_classify_full_cascade_returns_uncategorized_when_no_match() {
        let engine = ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: false,
                ..Default::default()
            },
        )
        .expect("engine");
        let r = engine
            .classify("xyzzy plugh frobnicate quux nonsense", false)
            .await;
        assert_eq!(r.category, "uncategorized");
    }

    #[tokio::test]
    async fn pipeline_runs_against_in_memory_db() {
        use rusqlite::params;
        use tga_core::config::Config;
        use tga_core::db::Database;

        let mut db = Database::open_in_memory().expect("open");
        {
            let conn = db.connection();
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, is_merge) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["aaa", "x", "x@example.com", "2024-01-01T00:00:00Z", "feat: add x", "r", 0],
            )
            .expect("insert");
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, repository, is_merge) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params!["bbb", "x", "x@example.com", "2024-01-01T00:00:00Z", "Merge branch foo", "r", 1],
            )
            .expect("insert");
        }

        let pipeline = ClassificationPipeline::new(Config::default());
        let stats = pipeline.run(&mut db).await.expect("run");
        assert_eq!(stats.total_commits, 2);
        assert_eq!(stats.classified, 2);
    }
}

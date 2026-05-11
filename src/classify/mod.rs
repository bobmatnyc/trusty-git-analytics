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
    use crate::core::models::ClassificationMethod;

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

    /// Build a sync engine from default rules for quick rule-coverage tests.
    fn engine() -> ClassificationEngine {
        ClassificationEngine::new(
            rules::default_rules(),
            ClassificationEngineConfig {
                use_llm: false,
                ..Default::default()
            },
        )
        .expect("engine")
    }

    fn classify_sync(msg: &str) -> ClassificationResult {
        engine()
            .classify_sync(msg, false)
            .unwrap_or_else(ClassificationResult::unclassified)
    }

    // ---------- Conventional-commit prefix coverage ----------

    #[test]
    fn cc_prefix_variants_with_scope_and_bang() {
        assert_eq!(classify_sync("feat(api)!: drop v1").category, "breaking");
        assert_eq!(classify_sync("fix(ui): broken modal").category, "bugfix");
        assert_eq!(
            classify_sync("perf(db): faster query").category,
            "performance"
        );
        assert_eq!(
            classify_sync("docs(readme): typo").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("test(auth): cover edge case").category,
            "test"
        );
        assert_eq!(classify_sync("ci(release): publish step").category, "ci");
        assert_eq!(classify_sync("build(deps): upgrade").category, "build");
        assert_eq!(classify_sync("style(lint): tabs").category, "style");
        assert_eq!(
            classify_sync("refactor(core): extract helper").category,
            "refactor"
        );
        assert_eq!(classify_sync("chore(deps): bump axios").category, "chore");
    }

    #[test]
    fn cc_additional_prefixes() {
        assert_eq!(
            classify_sync("security: patch CVE-2024-1234").category,
            "security"
        );
        assert_eq!(
            classify_sync("deps: bump tokio to 1.40").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("i18n: add Spanish translations").category,
            "localization"
        );
        assert_eq!(classify_sync("release: v1.2.0").category, "release");
        assert_eq!(classify_sync("wip: still thinking").category, "wip");
    }

    // ---------- Merge / revert ----------

    #[test]
    fn merge_patterns_classify_to_merge() {
        assert_eq!(
            classify_sync("Merge pull request #42 from foo/bar").category,
            "merge"
        );
        assert_eq!(
            classify_sync("Merge branch 'main' into dev").category,
            "merge"
        );
        assert_eq!(classify_sync("Merge tag 'v1.0.0'").category, "merge");
    }

    #[test]
    fn revert_patterns_classify_to_revert() {
        assert_eq!(
            classify_sync(r#"Revert "feat: add login""#).category,
            "revert"
        );
        assert_eq!(
            classify_sync("This reverts commit abcdef1234567890.").category,
            "revert"
        );
    }

    // ---------- Initial commit / version bump ----------

    #[test]
    fn initial_commit_classifies_to_chore() {
        let r = classify_sync("Initial commit");
        assert_eq!(r.category, "chore");
        assert_eq!(r.subcategory.as_deref(), Some("initial"));
    }

    #[test]
    fn version_bump_classifies_to_release() {
        assert_eq!(classify_sync("Bump version to 1.2.3").category, "release");
        assert_eq!(classify_sync("Prepare release 2.0").category, "release");
        assert_eq!(classify_sync("Release v3.4.0").category, "release");
    }

    // ---------- Dependency updates ----------

    #[test]
    fn dependency_updates_classify_to_maintenance() {
        assert_eq!(
            classify_sync("Update dependencies for security").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Bump axios from 0.21.0 to 1.0.0").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Dependabot bumps lodash").category,
            "maintenance"
        );
        assert_eq!(
            classify_sync("Update yarn.lock after install").category,
            "maintenance"
        );
    }

    // ---------- Lint / format / cleanup ----------

    #[test]
    fn lint_and_format_classify_to_style() {
        assert_eq!(classify_sync("Fix lint warnings").category, "style");
        assert_eq!(classify_sync("Run prettier on src/").category, "style");
        assert_eq!(classify_sync("Reformat with rustfmt").category, "style");
        assert_eq!(
            classify_sync("Trailing whitespace removal").category,
            "style"
        );
    }

    #[test]
    fn cleanup_classifies_to_refactor() {
        assert_eq!(classify_sync("Clean up old helpers").category, "refactor");
        assert_eq!(classify_sync("Remove unused imports").category, "refactor");
        assert_eq!(classify_sync("Delete dead code").category, "refactor");
    }

    // ---------- Review / PR feedback ----------

    #[test]
    fn review_feedback_classifies_to_refactor() {
        let r = classify_sync("Address review comments");
        assert_eq!(r.category, "refactor");
        assert_eq!(r.subcategory.as_deref(), Some("review"));

        assert_eq!(
            classify_sync("Apply suggestions from code review").category,
            "refactor"
        );
        assert_eq!(
            classify_sync("Incorporate review feedback").category,
            "refactor"
        );
    }

    // ---------- Infrastructure ----------

    #[test]
    fn infra_keywords_classify_appropriately() {
        assert_eq!(
            classify_sync("Update Dockerfile base image").category,
            "build"
        );
        assert_eq!(
            classify_sync("Add Helm chart for staging").category,
            "build"
        );
        assert_eq!(
            classify_sync("Switch k8s to nginx ingress").category,
            "build"
        );
        assert_eq!(
            classify_sync("Refactor Terraform modules").category,
            "build"
        );
        assert_eq!(
            classify_sync("Update github workflow for release").category,
            "ci"
        );
    }

    // ---------- Bug / fix prose ----------

    #[test]
    fn bug_fix_prose_classifies_to_bugfix() {
        for msg in [
            "Fix crash on empty input",
            "Fixes #123: bad redirect",
            "Resolve race condition in worker",
            "Closes #456",
        ] {
            let r = classify_sync(msg);
            assert_eq!(r.category, "bugfix", "{msg:?} => {r:?}");
        }
    }

    // ---------- Security / perf / docs / tests ----------

    #[test]
    fn security_prose_classifies_to_security() {
        assert_eq!(
            classify_sync("Patch XSS in comment renderer").category,
            "security"
        );
        assert_eq!(
            classify_sync("Fix SQL injection in search").category,
            "security"
        );
        assert_eq!(classify_sync("Address CVE-2023-0001").category, "security");
    }

    #[test]
    fn performance_prose_classifies_to_performance() {
        assert_eq!(
            classify_sync("Speed up query parser").category,
            "performance"
        );
        assert_eq!(
            classify_sync("Reduce memory usage in cache").category,
            "performance"
        );
        assert_eq!(
            classify_sync("Fix memory leak in worker").category,
            "performance"
        );
    }

    #[test]
    fn docs_prose_classifies_to_documentation() {
        assert_eq!(
            classify_sync("Update README with install instructions").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("Update changelog for 1.0").category,
            "documentation"
        );
        assert_eq!(
            classify_sync("Add docstring to extractor").category,
            "documentation"
        );
    }

    #[test]
    fn test_prose_classifies_to_test() {
        assert_eq!(classify_sync("Add unit tests for parser").category, "test");
        assert_eq!(classify_sync("Fix flaky integration test").category, "test");
        assert_eq!(classify_sync("Improve test coverage").category, "test");
    }

    // ---------- WIP / database / config ----------

    #[test]
    fn wip_prose_classifies_to_wip() {
        assert_eq!(classify_sync("[WIP] still hacking").category, "wip");
        assert_eq!(classify_sync("WIP refactor of auth").category, "wip");
    }

    #[test]
    fn database_migration_classifies_to_feature() {
        let r = classify_sync("Add migration for users table");
        assert_eq!(r.category, "feature");
        assert_eq!(r.subcategory.as_deref(), Some("database"));
    }

    // ---------- Coverage smoke test: a representative corpus ----------

    /// Smoke test: confirm < 15% of a representative corpus ends up
    /// uncategorized. Each entry is a real-world commit-message shape.
    #[test]
    fn corpus_uncategorized_below_15_percent() {
        let corpus: &[(&str, bool)] = &[
            ("feat: add login flow", false),
            ("feat(api)!: drop deprecated /v1 routes", false),
            ("fix: null deref in user lookup", false),
            ("fix(ui): modal close button", false),
            ("chore: tidy imports", false),
            ("chore(deps): bump axios", false),
            ("docs: clarify install steps", false),
            ("docs(readme): typo", false),
            ("test: add cases for parser", false),
            ("ci: enable rust beta", false),
            ("perf: cache hot path", false),
            ("style: rustfmt run", false),
            ("build: upgrade webpack", false),
            ("refactor: extract auth helper", false),
            ("revert: revert bad commit", false),
            ("Revert \"feat: add buggy thing\"", false),
            ("Merge pull request #42 from foo/bar", true),
            ("Merge branch 'main' into dev", true),
            ("Initial commit", false),
            ("First commit", false),
            ("Bump version to 1.2.3", false),
            ("Release v2.0", false),
            ("Update dependencies", false),
            ("Bump tokio from 1.30 to 1.40", false),
            ("Dependabot weekly update", false),
            ("Fix lint warnings", false),
            ("Run prettier", false),
            ("Address review comments", false),
            ("Code review feedback", false),
            ("Clean up dead code", false),
            ("Remove unused imports", false),
            ("Update Dockerfile", false),
            ("Add Helm chart", false),
            ("Update github workflow", false),
            ("Fix crash on empty input", false),
            ("Resolves #123", false),
            ("Closes #456 properly", false),
            ("Patch XSS vulnerability", false),
            ("Reduce memory usage", false),
            ("Update README", false),
            ("Add unit tests for parser", false),
            ("Fix flaky test", false),
            ("WIP: experimenting", false),
            ("[WIP] refactor", false),
            ("Add migration for users table", false),
            ("PROJ-123 implement payment flow", false),
            ("security: patch CVE-2024-1234", false),
            ("i18n: add Spanish translations", false),
            ("implement new feature for export", false),
            ("introduce caching layer", false),
            ("This reverts commit abcd1234.", false),
        ];

        let results = engine().classify_batch(corpus);
        let total = results.len();
        let uncategorized = results
            .iter()
            .filter(|r| r.category == "uncategorized")
            .count();
        let pct = (uncategorized as f64 / total as f64) * 100.0;
        assert!(
            pct < 15.0,
            "uncategorized {uncategorized}/{total} = {pct:.1}% (>= 15%)"
        );
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
        use crate::core::config::Config;
        use crate::core::db::Database;
        use rusqlite::params;

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

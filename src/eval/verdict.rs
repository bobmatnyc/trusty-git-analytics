//! One commit's verdict as the eval harness evaluates it (#111).
//!
//! Why: `tga eval sample` and `tga eval repredict` must reach the same verdict
//! for the same commit under the same config, or a re-predicted sample would
//! measure a different classifier than a freshly drawn one. Both aim at what
//! `tga classify` would store under that config, so they share one policy.
//! What: [`resolve_verdicts`] runs the traced rule engine on each commit's
//! message and merge flag — the inputs `tga classify` gives the cascade. A
//! stored verdict from a tier that is never re-run offline (manual, LLM,
//! external source, repo fallback) is carried only when the config's cascade
//! would still reach that tier ([`CarryPolicy`]); otherwise the re-derived
//! verdict wins and the stored one counts as superseded.
//! Test: `tests/eval_harness.rs::sample_then_score_end_to_end`,
//! `tests/eval_harness.rs::repredict_keeps_rows_and_follows_the_new_rules`,
//! `tests/eval_harness.rs::repredict_carries_a_stored_verdict_only_when_its_tier_is_reached`,
//! `tests/eval_harness.rs::repredict_never_carries_an_llm_verdict_for_a_merge`.

use super::population::CommitRow;
use crate::classify::{ClassificationEngine, TraceTier, TracedVerdict};
use crate::core::config::Config;

/// A commit's verdict as the harness evaluates it.
pub(crate) struct Resolved {
    pub tier: TraceTier,
    pub rule_id: String,
    pub category: String,
    pub confidence: f64,
    /// The verdict was carried from the database, not re-derived.
    pub carried: bool,
    /// A stored non-rule verdict existed but the config no longer reaches
    /// its tier, so the re-derived verdict replaced it.
    pub superseded: bool,
}

/// Which non-rule tiers the config's `tga classify` cascade would reach.
///
/// Why: #111 review — a stored LLM or external verdict must not survive a
/// config that no longer runs that tier. What: mirrors
/// `ClassificationPipeline`: the LLM tier runs when `Config::llm_tier_enabled`
/// says so (#175: `classification.use_llm: false` turns it off whatever the
/// `llm:` section says), on the verdicts `llm_fallback_scope`
/// selects (#111: at or below `llm_fallback_threshold`, or only unanswered
/// ones), never for a merge commit (#111), and only for a stored category
/// inside a custom-only rules set (#131); external sources run when any is
/// configured and
/// `no_external` is off. The stored verdict does not say which source produced
/// it, so any configured source keeps it. A stored repo fallback is never
/// carried: `tga classify` never applies one. #158: a non-merge commit of a
/// repository in `classification.repo_categories` resolves to the repo map,
/// whatever is stored, as `tga classify` would store it. #167: in floor mode
/// the map applies after the carried or re-derived verdict instead, keeping
/// it only when it is an exception at or above the threshold.
/// Test: `tests/eval_harness.rs::repredict_carries_a_stored_verdict_only_when_its_tier_is_reached`,
/// `tests/eval_harness.rs::repredict_never_carries_an_llm_verdict_for_a_merge`.
pub(crate) struct CarryPolicy {
    use_llm: bool,
    llm_threshold: f64,
    llm_scope: crate::core::config::LlmFallbackScope,
    /// #131: the LLM's category set when the rules restrict it.
    llm_categories: Option<Vec<String>>,
    external: bool,
    /// #158: the checked `classification.repo_categories` map.
    repo_map: crate::classify::pipeline_repo_map::RepoCategoryMap,
    /// #167: changed paths of the commits a `<repo>:<prefix>` key needs,
    /// read by [`Self::prepare`].
    paths: std::collections::HashMap<i64, Vec<String>>,
}

impl CarryPolicy {
    /// Read the policy from `config`.
    ///
    /// # Errors
    ///
    /// Returns an error if a configured rules file fails to load.
    pub(crate) fn from_config(config: &Config) -> crate::classify::Result<Self> {
        let c = config.classification.as_ref();
        let pipeline = crate::classify::ClassificationPipeline::new(config.clone());
        let llm_categories = pipeline
            .llm_categories()?
            .map(|cats| cats.into_iter().map(|c| c.name).collect());
        Ok(Self {
            repo_map: pipeline.repo_category_map()?,
            paths: std::collections::HashMap::new(),
            llm_categories,
            // #175: the predicate `tga classify` reads; `use_llm: false` wins.
            use_llm: config.llm_tier_enabled(),
            llm_threshold: c.map_or(0.65, |c| c.llm_fallback_threshold),
            llm_scope: c.map(|c| c.llm_fallback_scope).unwrap_or_default(),
            external: c.is_some_and(|c| !c.no_external && !c.sources.is_empty()),
        })
    }

    /// Read what the repo map needs from the eval database (#167).
    ///
    /// What: warns once per map key that matches nothing stored, and reads
    /// the changed paths of the `commits` a prefix key needs. Call once,
    /// before [`resolve_verdicts`].
    ///
    /// # Errors
    ///
    /// A database read fails.
    pub(crate) fn prepare(
        &mut self,
        conn: &rusqlite::Connection,
        commits: &[&CommitRow],
    ) -> crate::classify::Result<()> {
        self.repo_map.warn_unmatched_keys(conn)?;
        self.paths = self
            .repo_map
            .load_paths(conn, commits.iter().map(|c| (c.id, c.repo.as_str())))?;
        Ok(())
    }

    /// Whether the cascade reaches `stored` given the re-derived verdict `t`
    /// for a commit whose merge flag is `is_merge`.
    fn reaches(
        &self,
        stored: TraceTier,
        stored_category: &str,
        t: &TracedVerdict,
        is_merge: bool,
    ) -> bool {
        // #158: the repo map outranks every stored verdict.
        if t.trace.tier == TraceTier::RepoMap {
            return false;
        }
        match stored {
            TraceTier::Manual => true,
            // #111: `tga classify` never sends a merge to the LLM.
            TraceTier::Llm if is_merge => false,
            // #111: the same predicate `tga classify` routes with.
            TraceTier::Llm => {
                // #131: `tga classify` now drops an LLM answer outside the
                // configured set, so a stored one is superseded.
                let in_set = self
                    .llm_categories
                    .as_ref()
                    .is_none_or(|cats| cats.iter().any(|n| n == stored_category));
                self.use_llm
                    && in_set
                    && crate::classify::pipeline_llm::llm_eligible(
                        self.llm_scope,
                        &t.verdict,
                        self.llm_threshold,
                    )
            }
            // #111 review: `tga classify` never applies a repo fallback
            // (`apply_repo_category_fallback` has no production caller), so a
            // stored one is never reproduced.
            TraceTier::RepoCategory => false,
            TraceTier::ExternalSource => self.external,
            _ => false,
        }
    }
}

/// Map a stored `method` decided outside the rule engine to its trace.
fn stored_override(method: &str, traced: TraceTier) -> Option<(TraceTier, &'static str)> {
    match method {
        "manual" => Some((TraceTier::Manual, "manual_override")),
        "llm_fallback" => Some((TraceTier::Llm, "llm")),
        "repo_category_fallback" => Some((TraceTier::RepoCategory, "repo_category")),
        // The engine's own JIRA-project and issue-type tiers also store
        // `external_source`; only a verdict the engine did not reproduce came
        // from the pipeline's external resolver.
        "external_source" if !matches!(traced, TraceTier::JiraProject | TraceTier::IssueType) => {
            Some((TraceTier::ExternalSource, "external_source"))
        }
        _ => None,
    }
}

/// Resolve each commit's verdict, in input order, and count drift.
///
/// Why: see the module doc. What: classifies `(message, is_merge)` with
/// [`ClassificationEngine::classify_batch_traced`]; a commit whose stored
/// `method` names a tier outside the rule engine keeps its stored verdict when
/// `policy` says the cascade reaches that tier. The repo map then applies as
/// [`CarryPolicy`] describes (#158, #167). The second value counts
/// rule-engine commits whose stored category differs from the resolved one.
/// Test: see the module doc,
/// `tests::floor_mode_keeps_an_exception_and_floors_the_rest`.
pub(crate) fn resolve_verdicts(
    engine: &ClassificationEngine,
    policy: &CarryPolicy,
    commits: &[&CommitRow],
) -> (Vec<Resolved>, u64) {
    let pairs: Vec<(&str, bool)> = commits
        .iter()
        .map(|c| (c.message.as_str(), c.is_merge))
        .collect();
    let traced = engine.classify_batch_traced(&pairs);
    let mut drifted = 0u64;
    let resolved = commits
        .iter()
        .zip(traced)
        .map(|(c, t)| {
            let paths = policy.paths.get(&c.id).map_or(&[][..], Vec::as_slice);
            let mapped =
                policy
                    .repo_map
                    .traced(&c.repo, c.is_merge, paths, &c.message, engine.taxonomy());
            // #158: in override mode a mapped commit takes the repo map's
            // verdict up front. #167: in floor mode it applies last, below.
            let (t, floor) = match mapped {
                Some(m) if !policy.repo_map.is_floor() => (m, None),
                m => (t, m),
            };
            let mut superseded = false;
            let mut carried = None;
            let mut stored_rule_category = None;
            if let Some((cat, conf, method)) = &c.stored {
                match stored_override(method, t.trace.tier) {
                    // #111: carry only a tier the config's cascade reaches.
                    Some((tier, rule)) if policy.reaches(tier, cat, &t, c.is_merge) => {
                        carried = Some(Resolved {
                            tier,
                            rule_id: rule.to_string(),
                            category: cat.clone(),
                            confidence: *conf,
                            carried: true,
                            superseded: false,
                        });
                    }
                    Some(_) => superseded = true,
                    None => stored_rule_category = Some(cat),
                }
            }
            let mut r = carried.unwrap_or(Resolved {
                tier: t.trace.tier,
                rule_id: t.trace.rule_id,
                category: t.verdict.category,
                confidence: t.verdict.confidence,
                carried: false,
                superseded,
            });
            // #167: the floor replaces any verdict it does not keep, a
            // carried one included (which then counts as superseded).
            if let Some(m) = floor.filter(|_| !policy.repo_map.keeps(&r.category, r.confidence)) {
                r = Resolved {
                    tier: m.trace.tier,
                    rule_id: m.trace.rule_id,
                    category: m.verdict.category,
                    confidence: m.verdict.confidence,
                    carried: false,
                    superseded: r.carried || r.superseded,
                };
            }
            if stored_rule_category.is_some_and(|cat| cat != &r.category) {
                drifted += 1;
            }
            r
        })
        .collect();
    (resolved, drifted)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;
    use crate::classify::ClassificationPipeline;
    use crate::core::config::ClassificationConfig;

    fn commit(repo: &str, stored: Option<(&str, &str)>) -> CommitRow {
        CommitRow {
            id: 1,
            sha: "sha-a".into(),
            repo: repo.into(),
            author_email: "a@x".into(),
            timestamp: "2024-01-01T00:00:00Z".into(),
            ts: None,
            message: "fix: close the security hole".into(),
            is_merge: false,
            files: 1,
            insertions: 1,
            deletions: 0,
            ticket_id: None,
            stored: stored.map(|(c, m)| (c.to_string(), 0.9, m.to_string())),
        }
    }

    /// Why (#158 criterion 9): `tga eval sample` and `repredict` must reach
    /// the verdict `tga classify` stores, so a mapped repo's commit resolves
    /// to the repo map and shows as `repo_map` in the per-method breakdown,
    /// ahead of a rule match and of a stored manual verdict.
    /// What: a `fix: security` message, which the rules call `security`, in
    /// a mapped repo (with and without a stored manual verdict) and in an
    /// unmapped one.
    /// Test: this test.
    #[test]
    fn a_mapped_repo_resolves_to_the_repo_map_tier() {
        let mut rules = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .expect("tempfile");
        rules
            .write_all(
                b"extend_defaults: false\nrules:\n  - id: sec\n    category: security\n    \
                  keywords: [\"security\"]\ncategories:\n  - name: qa\n",
            )
            .expect("write");
        let config = Config {
            classification: Some(ClassificationConfig {
                rules_files: vec![rules.path().to_path_buf()],
                repo_categories: [("e2e".to_string(), "qa".to_string())].into(),
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        };
        let engine = ClassificationPipeline::new(config.clone())
            .build_rule_engine()
            .expect("engine");
        let policy = CarryPolicy::from_config(&config).expect("policy");
        let rows = [
            commit("e2e", None),
            commit("e2e", Some(("security", "manual"))),
            commit("api", None),
        ];
        let refs: Vec<&CommitRow> = rows.iter().collect();
        let (resolved, _) = resolve_verdicts(&engine, &policy, &refs);
        for r in &resolved[..2] {
            assert_eq!((r.tier.as_str(), r.category.as_str()), ("repo_map", "qa"));
            assert_eq!(r.rule_id, "repo_map:e2e");
            assert!(!r.carried);
        }
        assert_eq!(
            (resolved[2].tier.as_str(), resolved[2].category.as_str()),
            ("exact", "security")
        );
    }

    /// Why (#167): `tga eval` must reach the verdict a floor-mode
    /// `tga classify` stores: an exception verdict at or above the threshold
    /// survives, everything else takes the mapped category.
    /// What: floor mode, repo `e2e` mapped to `qa`. A `security` rule hit at
    /// 0.9 keeps its tier; an unanswered commit and a stored manual
    /// `new_feature` verdict (not an exception) resolve to the repo map.
    /// Test: this test.
    #[test]
    fn floor_mode_keeps_an_exception_and_floors_the_rest() {
        let mut rules = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .expect("tempfile");
        rules
            .write_all(
                b"extend_defaults: false\nrules:\n  - id: sec\n    category: security\n    \
                  keywords: [\"security\"]\n    confidence: 0.9\ncategories:\n  - name: qa\n  \
                  - name: new_feature\n",
            )
            .expect("write");
        let config = Config {
            classification: Some(ClassificationConfig {
                rules_files: vec![rules.path().to_path_buf()],
                repo_categories: [("e2e".to_string(), "qa".to_string())].into(),
                repo_map: crate::core::config::RepoMapConfig {
                    mode: crate::core::config::RepoMapMode::Floor,
                    ..Default::default()
                },
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        };
        let engine = ClassificationPipeline::new(config.clone())
            .build_rule_engine()
            .expect("engine");
        let policy = CarryPolicy::from_config(&config).expect("policy");
        let mut quiet = commit("e2e", None);
        quiet.message = "zzz qqq vvv".into();
        let mut manual = commit("e2e", Some(("new_feature", "manual")));
        manual.message = "zzz qqq vvv".into();
        let rows = [commit("e2e", None), quiet, manual];
        let refs: Vec<&CommitRow> = rows.iter().collect();
        let (resolved, _) = resolve_verdicts(&engine, &policy, &refs);
        assert_eq!(
            (resolved[0].tier.as_str(), resolved[0].category.as_str()),
            ("exact", "security")
        );
        for r in &resolved[1..] {
            assert_eq!((r.tier.as_str(), r.category.as_str()), ("repo_map", "qa"));
            assert_eq!(r.rule_id, "repo_map:e2e");
            assert!(!r.carried);
        }
        assert!(resolved[2].superseded, "the manual verdict was floored");
    }

    /// Why (#171): `tga eval` must keep the built-in rules' `test` and `ci`
    /// verdicts in a floor-mode repo, as `tga classify` does, because the
    /// default exceptions' `qa` and `devops` stand for them.
    /// What: built-in rules, floor mode with the default exceptions, repo
    /// `e2e` mapped to `tooling`. A `test:` and a `ci:` commit resolve to
    /// their own category on the exact tier; a `feat:` commit to the map.
    /// Test: this test.
    #[test]
    fn floor_mode_keeps_built_in_test_and_ci_verdicts() {
        let config = Config {
            classification: Some(ClassificationConfig {
                repo_categories: [("e2e".to_string(), "tooling".to_string())].into(),
                repo_map: crate::core::config::RepoMapConfig {
                    mode: crate::core::config::RepoMapMode::Floor,
                    ..Default::default()
                },
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        };
        let engine = ClassificationPipeline::new(config.clone())
            .build_rule_engine()
            .expect("engine");
        let policy = CarryPolicy::from_config(&config).expect("policy");
        let messages = [
            "test: cover the parser edge cases",
            "ci: pin the runner image",
            "feat: add the export button",
        ];
        let rows: Vec<CommitRow> = messages
            .iter()
            .map(|m| {
                let mut c = commit("e2e", None);
                c.message = (*m).into();
                c
            })
            .collect();
        let refs: Vec<&CommitRow> = rows.iter().collect();
        let (resolved, _) = resolve_verdicts(&engine, &policy, &refs);
        let got: Vec<(&str, &str)> = resolved
            .iter()
            .map(|r| (r.tier.as_str(), r.category.as_str()))
            .collect();
        assert_eq!(
            got,
            [("exact", "test"), ("exact", "ci"), ("repo_map", "tooling")]
        );
    }

    /// Why (#167 review): `tga eval` reads the changed paths a prefix key
    /// needs through [`CarryPolicy::prepare`], so it resolves a monorepo
    /// commit as `tga classify` does.
    /// What: keys `mono` (qa) and `mono:services` (new_feature); a commit
    /// under `services/` resolves to `repo_map:mono:services`, one under
    /// `docs/` to `repo_map:mono`.
    /// Test: this test.
    #[test]
    fn prepare_reads_the_paths_a_prefix_key_needs() {
        let mut rules = tempfile::Builder::new()
            .suffix(".yaml")
            .tempfile()
            .expect("tempfile");
        rules
            .write_all(
                b"extend_defaults: false\nrules:\n  - id: sec\n    category: security\n    \
                  keywords: [\"security\"]\ncategories:\n  - name: qa\n  - name: new_feature\n",
            )
            .expect("write");
        let config = Config {
            classification: Some(ClassificationConfig {
                rules_files: vec![rules.path().to_path_buf()],
                repo_categories: [
                    ("mono".to_string(), "qa".to_string()),
                    ("mono:services".to_string(), "new_feature".to_string()),
                ]
                .into(),
                ..ClassificationConfig::default()
            }),
            ..Config::default()
        };
        let db = crate::core::db::Database::open_in_memory().expect("db");
        let conn = db.connection();
        let mut rows = Vec::new();
        for (sha, path) in [("sha-svc", "services/a.rs"), ("sha-docs", "docs/x.md")] {
            conn.execute(
                "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                 repository, is_merge) VALUES (?1, 'a', 'a@x', '2024-01-01T00:00:00Z', \
                 'zzz qqq vvv', 'mono', 0)",
                [sha],
            )
            .expect("commit");
            let id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO files (commit_id, path, change_type) VALUES (?1, ?2, 'M')",
                rusqlite::params![id, path],
            )
            .expect("file");
            let mut row = commit("mono", None);
            row.id = id;
            row.sha = sha.into();
            row.message = "zzz qqq vvv".into();
            rows.push(row);
        }
        let engine = ClassificationPipeline::new(config.clone())
            .build_rule_engine()
            .expect("engine");
        let mut policy = CarryPolicy::from_config(&config).expect("policy");
        let refs: Vec<&CommitRow> = rows.iter().collect();
        policy.prepare(conn, &refs).expect("prepare");
        let (resolved, _) = resolve_verdicts(&engine, &policy, &refs);
        assert_eq!(
            (resolved[0].rule_id.as_str(), resolved[0].category.as_str()),
            ("repo_map:mono:services", "new_feature")
        );
        assert_eq!(
            (resolved[1].rule_id.as_str(), resolved[1].category.as_str()),
            ("repo_map:mono", "qa")
        );
    }

    /// Why (#175): `tga eval repredict` carries a stored LLM verdict only
    /// when `tga classify` would run the LLM, and `use_llm: false` turns it
    /// off whatever the `llm:` section says.
    /// What: the carry policy's LLM flag for an explicit false, an absent
    /// key, and an explicit true, each beside a Bedrock `llm:` section.
    /// Test: this test.
    #[test]
    fn carry_policy_honours_an_explicit_use_llm_false() {
        for (line, want) in [
            ("  use_llm: false\n", false),
            ("", true),
            ("  use_llm: true\n", true),
        ] {
            let yaml = format!(
                "classification:\n  confidence_threshold: 0.7\n{line}\
                 llm:\n  source: bedrock\n  region: us-east-1\n"
            );
            let config: Config = serde_yaml::from_str(&yaml).expect("config");
            let policy = CarryPolicy::from_config(&config).expect("policy");
            assert_eq!(policy.use_llm, want, "{line:?}");
        }
    }
}

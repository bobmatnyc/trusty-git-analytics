Added

- Rule tracing for the classification cascade (#111):
  `ClassificationEngine::classify_sync_traced` and `classify_batch_traced`
  return each verdict with a `RuleTrace` naming the tier and a stable rule id
  (`<rules file or builtin>#<rule id>`, `catch_all`,
  `weighted_sum:<category>/<signal>`, `fuzzy:<heuristic>`,
  `jira_project:<KEY>`, ...). The trace is held in memory only; `tga classify`
  writes byte-identical `classifications` rows, pinned by a golden test over
  the recorded corpus. `ClassificationPipeline::build_rule_engine` exposes the
  rule engine `tga classify` uses, without the LLM tier, and
  `load_rules_multi_with_sources` records which rules file defined each rule.

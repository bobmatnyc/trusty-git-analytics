Changed
- The LLM tier never sends a merge commit to the LLM, under either
  `llm_fallback_scope` (#111). Merges keep their rule verdict and write no
  `llm_usage` row; the number skipped is logged at `info` level.
  Under a custom-only ruleset (`extend_defaults: false`), a merge the rules
  leave uncategorized now stays uncategorized instead of getting an LLM
  label. Those merges count against the classify summary's `coverage_pct`,
  `repository_analysis_status.classification_coverage_pct`, and the
  `min_coverage_pct` warning, so coverage can drop after upgrading.
- `tga backfill complexity` and `tga classify --backfill-complexity` skip
  merge commits too (#111). A merge's `complexity` stays NULL, and the
  `--dry-run` candidate count no longer includes merges.
- `tga eval repredict` no longer carries a stored LLM verdict on a merge
  commit (#111), because `tga classify` would not produce one. The row is
  re-derived and counted as superseded.

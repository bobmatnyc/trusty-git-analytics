Changed
- The LLM tier never sends a merge commit to the LLM, under either
  `llm_fallback_scope` (#111). Merges keep their rule verdict and write no
  `llm_usage` row; the number skipped is logged at `info` level.
- `tga backfill complexity` and `tga classify --backfill-complexity` skip
  merge commits too (#111). A merge's `complexity` stays NULL, and the
  `--dry-run` candidate count no longer includes merges.
- `tga eval repredict` no longer carries a stored LLM verdict on a merge
  commit (#111), because `tga classify` would not produce one. The row is
  re-derived and counted as superseded.

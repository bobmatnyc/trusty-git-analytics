Changed
- The LLM tier never sends a merge commit to the LLM, under either
  `llm_fallback_scope` (#111). Merges keep their rule verdict and write no
  `llm_usage` row; the number skipped is logged at `info` level.

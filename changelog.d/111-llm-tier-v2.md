Added

- `classification.llm_fallback_scope: unanswered` sends only the commits the
  rules left uncategorized to the LLM tier; the default `low_confidence`
  keeps the `llm_fallback_threshold` routing (#111). `tga eval repredict`
  carries a stored LLM verdict under the same rule.
- LLM token accounting: every LLM-tier call records its input/output tokens
  from the provider reply in the new `llm_usage` table (migration v31), and
  `tga classify` prints call counts by outcome, token totals and tokens per
  call (#111).
- `tga classify --shas <file>` classifies only the listed commit SHAs and
  fails before any write on an empty list or an unknown SHA. It writes, so
  use it on a scratch database copy (#111).
- Rules files accept a top-level `categories:` list with an optional
  `description` per category, and the `llm:` section accepts
  `effort: low|medium|high|xhigh|max` for `anthropic-api` (#131).

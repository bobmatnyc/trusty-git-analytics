Changed

- `tga eval sample` keeps a commit's stored verdict from a tier it does not
  re-run only when the sampling config still reaches that tier, as
  `tga classify` would (#111). A stored LLM verdict is kept only when the
  config enables the LLM tier (an `llm:` section or
  `classification.use_llm`) and the re-derived confidence is at or below
  `llm_fallback_threshold`. A stored external-source verdict is kept only
  when an external source is configured. A stored repo fallback is never
  kept. Manual overrides are always kept. Any other commit is stratified by
  its re-derived verdict, so on the same database and seed a config without
  `llm:` can place formerly-LLM commits in other strata and draw a different
  sample than earlier releases did.

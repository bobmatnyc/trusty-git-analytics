Fixed
- `classification.use_llm: false` now turns the LLM tier off even when an `llm:` section is present. Before, the `llm:` section enabled the tier on its own, so `tga classify` sent every eligible commit to the configured provider (Bedrock, OpenRouter, the Anthropic API or Jev) despite the opt-out. With `use_llm: false`, `tga classify`, `tga analyze`, `tga collect`'s classify step and the complexity backfill build no provider client and make no LLM call, and the run logs that the `llm:` section is ignored (#175).
  - An absent `use_llm` keeps the old behaviour: an `llm:` section turns the tier on. `--use-llm` still turns it on for one run.
  - `use_llm` must now be `true` or `false`; an empty value or `null` is a config error instead of reading as absent.
  - `tga backfill complexity` with the LLM tier off now logs one warning and scores nothing, instead of one warning per commit.

Breaking
- The public `ClassificationConfig.use_llm` field changes from `bool` to `Option<bool>`; `None` means the key is absent from the config (#175).
- `classification.use_llm: null`, or the key with an empty value, is now a config error instead of reading as `false` (#175).
- A config with `classification.use_llm: false` and an `llm:` section no longer runs the LLM tier. Before, the `llm:` section turned the tier on regardless (#175).

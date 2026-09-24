Changed

- With `extend_defaults: false`, the LLM prompt offers only the configured
  categories plus an `unclear` abstain option, and a reply outside that set
  is dropped instead of stored (#131). Other configs keep the built-in list.
- The `anthropic-api` default model is now `claude-haiku-4-5`; the previous
  default, Claude Haiku 3.5, is retired. The Anthropic request allows 2048
  output tokens so adaptive thinking on Claude Sonnet 5 fits (#131).

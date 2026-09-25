Fixed
- The Bedrock LLM tier no longer sends `temperature` (#111). Claude Sonnet 5
  on Bedrock rejected every classification call because tga sent
  `temperature: 0.0`. No Bedrock request carries a temperature now, for any
  model, so Sonnet 5 works by setting `llm.model` to its `us.anthropic.`
  inference-profile id. Other models run at the provider's default
  temperature.
- The default Bedrock model is now `us.anthropic.claude-haiku-4-5-20251001-v1:0`
  (#111). The old default, `anthropic.claude-3-haiku-20240307-v1:0`, was not
  invocable in us-east-1; current Claude models on Bedrock need a
  cross-region inference-profile id.

Fixed
- `taudit render` no longer requires an OpenRouter key when the selected provider is not `openrouter`. A Bedrock engagement with a blank or absent `openrouter_key` re-renders, and no blank key is set on the `trusty-review` child; an `openrouter` engagement with no key is still refused ([#220](https://github.com/bobmatnyc/trusty-git-analytics/issues/220)).

Breaking
- `LlmClassifier::new`, `LlmClassifier::build_anthropic` and `ExternalSourceResolver::new` now return `Result<_, ClassifyError>` instead of `Self`. Callers must handle or propagate the error, which is returned when the timed HTTP client cannot be built (for example, TLS backend initialisation failure). Previously a build failure silently fell back to an untimed client (#255).

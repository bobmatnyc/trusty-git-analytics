# Project Instructions — trusty-git-analytics

## Context

This is a Rust port of gitflow-analytics (Python).
Python predecessor: /Users/masa/Projects/gitflow-analytics
GitHub: https://github.com/bobmatnyc/gitflow-analytics

## Shipping Checklist (MANDATORY for every feature/fix release)

1. **Implement** — write code and tests, verify all pass (`cargo test`)
2. **Lint** — `cargo clippy -- -D warnings` must pass
3. **Format** — `cargo fmt --check` must pass
4. **Commit** — staged files only, passing pre-commit hooks
5. **Update docs** — update CHANGELOG.md, README.md if needed
6. **Bump version** — Cargo.toml workspace version, commit + tag
7. **Push** — `git push origin main && git push origin vX.Y.Z`

## Engineering Standards

- Use workspace dependencies (no version duplication)
- Every public function must have doc comments
- Errors must use thiserror (libraries) or anyhow (CLI)
- No unwrap() in library code — propagate with ?
- All async code uses tokio
- Parallelism uses rayon for CPU-bound, tokio for I/O-bound
- SQLite operations use WAL mode
- Config structs must implement serde::Deserialize
- Test coverage required for: all parsers, all classifiers, all DB operations

## Reference Implementation

When implementing any feature, first check the equivalent Python implementation:
`/Users/masa/Projects/gitflow-analytics/src/gitflow_analytics/`

The Rust port should be API-compatible (same config, same DB schema, same CLI flags).

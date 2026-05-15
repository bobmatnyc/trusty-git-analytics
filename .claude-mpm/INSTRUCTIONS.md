# Project Instructions — trusty-git-analytics

## Context

This is a Rust port of gitflow-analytics (Python).
Python predecessor: /Users/masa/Projects/gitflow-analytics
GitHub: https://github.com/bobmatnyc/gitflow-analytics

## Session Start Workflow (MANDATORY order)

When asked to "check issues", "run workflow", or start a work session:

1. **Check open PRs first** — `gh pr list --repo bobmatnyc/trusty-git-analytics --state open`
   - Review each open PR (diff, CI status, comments)
   - Address PRs before touching issues: review, request changes, or merge
2. **Then check open issues** — only after all actionable PRs are handled

## Shipping Checklist (MANDATORY for every feature/fix release)

1. **Implement** — write code and tests, verify all pass (`cargo test`)
2. **Lint** — `cargo clippy -- -D warnings` must pass
3. **Format** — `cargo fmt --check` must pass
4. **Commit** — staged files only, passing pre-commit hooks. When a commit resolves a GitHub issue, include `closes #N` in the commit message body (not the subject line) so GitHub auto-closes the issue on push.
5. **Update docs** — update CHANGELOG.md, README.md if needed
6. **Bump version** — Cargo.toml workspace version, commit + tag
7. **Push** — `git push origin main && git push origin vX.Y.Z`
8. **Monitor CI/CD** — after every push, delegate to version-control agent to check `gh run list` status; do NOT declare release complete until all CI gates are green

## CI/CD Monitoring (MANDATORY)

After every `git push` to main or a version tag:

1. **Delegate to version-control agent**: `gh run list --repo bobmatnyc/trusty-git-analytics --limit 5`
2. **Wait for runs to complete** — if any run is `in_progress` or `queued`, poll until settled
3. **If any run fails**: immediately triage the failure (clippy / test / rustdoc / release), fix it, and push a follow-up commit before closing the task
4. **Release is only complete** when: all CI matrix jobs green AND release workflow has published binaries

Failure categories to watch for:
- `cargo clippy -- -D warnings` — lint regressions from new code
- Test failures — especially `duetto_contractors_config_resolves` (historically flaky)
- Rustdoc broken links — stale module paths after refactors
- Release workflow — binary build / upload failures

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

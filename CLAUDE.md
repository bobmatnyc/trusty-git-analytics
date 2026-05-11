# trusty-git-analytics — AI Assistant Instructions

## Project Purpose

This is a **Rust port** of `gitflow-analytics` — a developer productivity analytics tool.
- Python predecessor: `/Users/masa/Projects/gitflow-analytics`
- Python predecessor GitHub: https://github.com/bobmatnyc/gitflow-analytics

The goal is full API compatibility with the Python tool, using Rust best practices for
superior performance, parallelism, and correctness.

## Implementation State

> Last updated: 2026-05-11

| Component | Status | Notes |
|-----------|--------|-------|
| Cargo workspace | DONE | All 5 crates scaffolded with correct dependencies |
| `tga-core/src/lib.rs` | PLACEHOLDER | Empty — needs types, config, DB, errors |
| `tga-collect/src/lib.rs` | PLACEHOLDER | Empty — needs git2 extraction, HTTP clients |
| `tga-classify/src/lib.rs` | PLACEHOLDER | Empty — needs classification cascade |
| `tga-report/src/lib.rs` | PLACEHOLDER | Empty — needs CSV/JSON/Markdown output |
| `tga-cli/src/main.rs` | PLACEHOLDER | Only prints project name — needs clap CLI |
| Database migrations | NOT STARTED | Schema defined in docs, no SQL files yet |
| Configuration structs | NOT STARTED | YAML schema defined in docs |
| Tests | NOT STARTED | No test files exist |
| CI/CD | NOT STARTED | No GitHub Actions workflows |

**Start here**: Implement `tga-core` first — all other crates depend on it.

## Architecture Overview

Three-stage pipeline implemented as a Cargo workspace under `crates/`:

| Crate | Path | Purpose |
|-------|------|---------|
| `tga-core` | `crates/tga-core` | Shared types, config (serde), DB schema (rusqlite), error types |
| `tga-collect` | `crates/tga-collect` | Stage 1: git extraction (git2), GitHub/JIRA HTTP clients (reqwest+tokio) |
| `tga-classify` | `crates/tga-classify` | Stage 2: four-tier classification cascade (rules + LLM) |
| `tga-report` | `crates/tga-report` | Stage 3: CSV/JSON/Markdown generation |
| `tga-cli` | `crates/tga-cli` | Binary entry point (`tga`), clap CLI |

### Crate Dependency Order

```
tga-core  <──  tga-collect  <──┐
          <──  tga-classify <──┤  tga-cli (binary)
          <──  tga-report   <──┘
```

Implement in this order: `tga-core` → `tga-collect` → `tga-classify` → `tga-report` → `tga-cli`

## Key Rust Decisions

- **git2**: libgit2 bindings for git operations (replaces GitPython + subprocess)
- **rusqlite**: SQLite with `bundled` feature — no system SQLite required
- **tokio**: async runtime for all HTTP clients
- **rayon**: data parallelism for batch commit processing
- **clap**: CLI with derive macros (same subcommand structure as Python)
- **serde + serde_yaml**: config deserialization (same YAML schema as Python)
- **aho-corasick**: multi-pattern commit message matching
- **strsim**: fuzzy string matching for identity resolution
- **chrono**: date/time with ISO week support
- **tera**: Jinja2-style templates for Markdown reports
- **blake3**: config file hashing
- **anyhow + thiserror**: error handling (anyhow in bins, thiserror in libs)

## Database

SQLite, same schema as `gitflow-analytics`. Schema defined in `tga-core/src/db/`.
Migration runner applies versioned SQL migrations on startup (v1–v18 from Python port, +future).

🔴 **Critical**: Always use WAL journal mode: `PRAGMA journal_mode=WAL`.

Reference: `docs/requirements/database-schema.md`

## Configuration

YAML file, same structure as Python version. Deserialized via `serde_yaml` into structs in
`tga-core/src/config/`. Support `~` expansion for paths.

Reference: `docs/requirements/configuration.md`

## CLI Structure

Binary: `tga` (produced by `tga-cli` crate)

Subcommands: `analyze`, `collect`, `classify`, `report`, `fetch`, `aliases`, `identities`,
             `pr-metrics`, `override`, `install`

Reference: `docs/requirements/cli-commands.md`

## Development Commands

The ONE canonical way to perform each task:

```bash
# Build everything
cargo build

# Build release binary
cargo build --release          # output: target/release/tga

# Run all tests
cargo test

# Lint (must pass with zero warnings)
cargo clippy -- -D warnings

# Format check (CI gate)
cargo fmt --check

# Format (auto-fix)
cargo fmt

# Generate and open API docs
cargo doc --open

# Run the CLI (dev)
cargo run --bin tga -- <subcommand>

# Check a single crate
cargo check -p tga-core
```

🔴 **CI requirements**: `cargo clippy -- -D warnings` and `cargo fmt --check` must both pass before merging.

## Priority Rankings

### 🔴 Critical (implement first)
- `tga-core`: error types, config structs, DB schema, migration runner
- WAL mode pragma on every DB open
- `anyhow` in binaries, `thiserror` in library crates (never mix)

### 🟡 Important (implement second)
- `tga-collect`: git2 commit extraction, identity resolution, GitHub/JIRA clients
- `tga-classify`: four-tier cascade (exact rules → regex → fuzzy → LLM fallback)
- `tga-cli`: clap subcommand wiring

### 🟢 Nice-to-have (implement third)
- `tga-report`: CSV/JSON output first, Markdown templates later
- Progress bars (indicatif) in long-running operations
- `--dry-run` flags on mutating commands

### ⚪ Informational
- `docs/requirements/` contains full specification — read before implementing any module
- Python predecessor at `/Users/masa/Projects/gitflow-analytics` for reference behavior
- KuzuMemory MCP tools (`kuzu_recall`, `kuzu_learn`, `kuzu_enhance`) available for context

## Requirements Reference

All specification documents are in `docs/requirements/`:

| File | Covers |
|------|--------|
| `overview.md` | System overview and pipeline |
| `configuration.md` | Full YAML config schema |
| `database-schema.md` | All SQLite tables and columns |
| `cli-commands.md` | All subcommands and flags |
| `classification.md` | Four-tier classification cascade |
| `collection.md` | Git extraction and API fetching |
| `reporting.md` | Report formats and metrics |
| `rust-architecture.md` | Rust-specific design decisions |
| `index.md` | Requirements index |

## Coding Standards

- Use `anyhow::Result` in binary crates (`tga-cli`), `thiserror` enums in library crates
- Prefer `tracing::{info, warn, error, debug}` over `println!` / `eprintln!`
- All public API items must have doc comments (`///`)
- No `unwrap()` or `expect()` in library code — propagate errors with `?`
- Use `rayon::par_iter()` for CPU-bound batch operations (commit classification)
- All async functions use `tokio` — no mixing of async runtimes

## Claude MPM Configuration

See `.claude-mpm/` for claude-mpm project configuration.

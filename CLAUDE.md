# trusty-git-analytics — AI Assistant Instructions

## Project Purpose

This is a **Rust port** of `gitflow-analytics` — a developer productivity analytics tool.
- Python predecessor: `/Users/masa/Projects/gitflow-analytics`
- Python predecessor GitHub: https://github.com/bobmatnyc/gitflow-analytics

The goal is full API compatibility with the Python tool, using Rust best practices for
superior performance, parallelism, and correctness.

## Architecture Overview

Three-stage pipeline implemented as a Cargo workspace:

| Crate | Purpose |
|-------|---------|
| `tga-core` | Shared types, config (serde), DB schema (rusqlite), error types |
| `tga-collect` | Stage 1: git extraction (git2), GitHub/JIRA HTTP clients (reqwest+tokio) |
| `tga-classify` | Stage 2: four-tier classification cascade (rules + LLM) |
| `tga-report` | Stage 3: CSV/JSON/Markdown generation |
| `tga-cli` | Binary entry point, clap CLI |

## Key Rust Decisions

- **git2**: libgit2 bindings for git operations (replaces GitPython + subprocess)
- **rusqlite**: SQLite with bundled feature (replaces SQLAlchemy)
- **tokio**: async runtime for HTTP clients
- **rayon**: data parallelism for batch processing
- **clap**: CLI with derive macros (same subcommand structure as Python)
- **serde + serde_yaml**: config deserialization (same YAML schema)
- **aho-corasick**: multi-pattern commit message matching
- **strsim**: fuzzy string matching for identity resolution
- **chrono**: date/time with ISO week support

## Database

SQLite, same schema as `gitflow-analytics`. Schema defined in `tga-core/src/db/`.
Migration runner applies versioned SQL migrations on startup (v1–v18 from Python port, +future).
Use WAL journal mode: `PRAGMA journal_mode=WAL`.

## Configuration

YAML file, same structure as Python version. Deserialized via `serde_yaml` into structs in
`tga-core/src/config/`. Support `~` expansion for paths.

## CLI Structure

Binary: `tga` (from `tga-cli`)
Subcommands: `analyze`, `collect`, `classify`, `report`, `fetch`, `aliases`, `identities`,
             `pr-metrics`, `override`, `install`

## Development Commands

```bash
cargo build                          # Build all crates
cargo test                           # Run all tests
cargo clippy -- -D warnings          # Lint
cargo fmt --check                    # Format check
cargo doc --open                     # API docs
```

## Requirements Reference

See `docs/requirements/` for full specification:
- `overview.md` — system overview and pipeline
- `configuration.md` — full YAML config schema
- `database-schema.md` — all SQLite tables and columns
- `cli-commands.md` — all subcommands and flags
- `classification.md` — four-tier classification cascade
- `collection.md` — git extraction and API fetching
- `reporting.md` — report formats and metrics
- `rust-architecture.md` — Rust-specific design decisions

## Memory Integration

KuzuMemory MCP tools available for context:
- `kuzu_recall` — query project memories
- `kuzu_learn` — store decisions
- `kuzu_enhance` — enhance prompts with context

## Claude MPM Configuration

See `.claude-mpm/` for claude-mpm project configuration.

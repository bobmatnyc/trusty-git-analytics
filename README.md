# trusty-git-analytics

A high-performance Rust port of [gitflow-analytics](https://github.com/bobmatnyc/gitflow-analytics) — a developer productivity analytics tool that analyzes Git repositories to generate insights about commit patterns, classification of work types, and engineering metrics.

## Why Rust?

The Python predecessor (`gitflow-analytics`) proved the value of the analytics pipeline. This Rust port delivers:

- **True parallelism**: No GIL — all repositories and branches analyzed concurrently via `rayon` + `tokio`
- **libgit2 integration**: Native git access via `git2` crate — no subprocess overhead per commit
- **Async HTTP**: Concurrent GitHub/JIRA API calls via `reqwest` + `tokio`
- **Compiled regex FSMs**: `aho-corasick` for O(n) simultaneous pattern matching across all classification rules
- **SQLite WAL mode**: Concurrent reads during write-heavy collection phases

## Python Predecessor

This project is a Rust port of `gitflow-analytics`, located at `/Users/masa/Projects/gitflow-analytics`.
See also: https://github.com/bobmatnyc/gitflow-analytics

The core functionality is API-compatible: same YAML configuration schema, same three-stage pipeline, same SQLite database schema, same CLI subcommand structure.

## Pipeline

```
tga collect  →  tga classify  →  tga report
(git + APIs)    (rules + LLM)    (CSV/JSON/Markdown)
     ↓               ↓                  ↓
 SQLite DB      qualitative data    output files
```

## Quick Start

```bash
# Install
cargo install --path crates/tga-cli

# Run full pipeline
tga analyze --config config.yaml --weeks 4

# Or run stages independently
tga collect  --config config.yaml --weeks 4
tga classify --config config.yaml --weeks 4
tga report   --config config.yaml --weeks 4 --output ./reports
```

## Configuration

Same YAML schema as `gitflow-analytics`. See `docs/requirements/configuration.md` for full reference.

## Architecture

```
crates/
├── tga-core/      # Shared types, config schema, DB models, migrations
├── tga-collect/   # Git extraction, GitHub/JIRA API clients
├── tga-classify/  # Four-tier classification cascade
├── tga-report/    # Report generation (CSV, JSON, Markdown)
└── tga-cli/       # CLI entry point (clap)
```

## Development Status

Pre-alpha — Porting in progress. See [migration plan tickets](https://github.com/bobmatnyc/trusty-git-analytics/issues) for current status.

## Requirements

- Rust 1.75+
- libgit2 (bundled via `git2` crate)
- OpenSSL or rustls

## License

MIT

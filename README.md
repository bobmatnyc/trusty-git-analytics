# trusty-git-analytics

Analyze git repositories to measure developer productivity — classify commit work types, track weekly velocity, and export CSV/JSON/Markdown reports.

## What It Does

`tga` walks one or more local git repositories, collects every commit into a SQLite database, classifies each commit into a work category (feature, bugfix, refactor, etc.) using a four-tier rule cascade, then aggregates the results into per-author and per-week reports. It is a Rust port of [gitflow-analytics](https://github.com/bobmatnyc/gitflow-analytics) with the same YAML config schema and the same SQLite schema — existing config files work without modification.

## Installation

### From crates.io (recommended)

```bash
cargo install trusty-git-analytics
```

This installs the `tga` binary to `~/.cargo/bin/`. Ensure `~/.cargo/bin` is in your `PATH`.

### From source

```bash
git clone https://github.com/bobmatnyc/trusty-git-analytics
cd trusty-git-analytics
cargo install --path .
```

### Verify installation

```bash
tga --version
tga --help
```

## Quick Start

### Run Your First Analysis

**Step 1** — Create a `config.yaml`:

```yaml
repositories:
  - path: ~/code/my-project
    name: my-project

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

**Step 2** — Run the full pipeline:

```bash
tga analyze --config config.yaml
```

**Step 3** — Find reports in `./reports/`:

```
reports/
├── authors.csv         # Per-author commit summary
├── weekly_activity.csv # Week-by-week breakdown
├── report.json         # Full structured payload
└── report.md           # Narrative Markdown report
```

## Configuration

### Minimal config.yaml

```yaml
repositories:
  - path: ~/code/my-repo
    name: my-repo
```

All other sections are optional. When `output.formats` is omitted, all three formats (CSV, JSON, Markdown) are written.

### Full config reference

| Key | Type | Default | Description |
|-----|------|---------|-------------|
| `repositories` | list | required | Repos to analyze |
| `developer_aliases` | map | `{}` | Canonical name → list of emails/aliases |
| `team` | object | — | Alternative to `developer_aliases`; roster with email |
| `output.directory` | path | `./reports` | Where reports are written |
| `output.formats` | list | `[csv, json, markdown]` | `csv`, `json`, and/or `markdown` |
| `output.include_unclassified` | bool | `false` | Include commits with no category |
| `output.include_merges` | bool | `false` | Include merge commits |
| `output.include_files` | bool | `false` | Include file-level change detail |
| `classification.rules_file` | path | — | Path to custom rules YAML/JSON |
| `classification.use_llm` | bool | `false` | Enable LLM fallback tier |
| `classification.llm_model` | string | `gpt-4o-mini` | LLM model identifier |
| `classification.confidence_threshold` | float | `0.7` | Minimum acceptance confidence |
| `github.token` | string | `$GITHUB_TOKEN` | GitHub PAT for PR fetch |
| `github.org` | string | — | Org slug for org-wide PR queries |
| `github.repo` | string | — | Single repo slug (`owner/name`) |
| `github.fetch_prs` | bool | `false` | Fetch pull request metadata |
| `jira.url` | string | — | JIRA base URL |
| `jira.username` | string | — | JIRA API username (email for Cloud) |
| `jira.token` | string | — | JIRA API token |
| `jira.project_key` | string | — | Project key filter (e.g. `API`) |
| `pm.azure_devops.organization_url` | string | — | ADO org URL (e.g. `https://dev.azure.com/myorg`) |
| `pm.azure_devops.pat` | string | — | Azure DevOps Personal Access Token |
| `pm.azure_devops.project` | string | — | Default ADO project name |
| `pm.azure_devops.fetch_on_reference` | bool | `false` | Fetch work items when `AB#N` refs appear in commits |
| `cache.directory` | path | — | Cache directory (supports `~`) |
| `version` | string | — | Schema version; stored for compatibility |
| `profile` | string | — | Named profile; stored for compatibility |

Paths support `~` expansion. Config files from the Python `gitflow-analytics` tool load without changes — unknown keys are silently ignored.

### developer_aliases vs team.members

**`developer_aliases`** (Python-compatible flat map):

```yaml
developer_aliases:
  "Alice Smith":
    - "alice@company.com"
    - "asmith@company.com"
    - "alice@personal.dev"
  "Bob Jones":
    - "bob@company.com"
    - "129991831+bobgithub@users.noreply.github.com"
```

**`team.members`** (structured roster with canonical email):

```yaml
team:
  members:
    - name: Alice Smith
      email: alice@company.com
      aliases:
        - asmith@company.com
        - alice@personal.dev
```

When `developer_aliases` is non-empty it takes precedence over `team.members`. Use `developer_aliases` when migrating an existing Python config file; use `team.members` for new setups where canonical email matters for downstream tooling.

### Example: multi-repo config with GitHub

See [`configs/example-config.yaml`](configs/example-config.yaml) for a working example that covers multiple repositories, developer aliases, and CSV+Markdown output.

## CLI Reference

All subcommands accept these global flags:

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `config.yaml` | Path to config YAML |
| `--database <PATH>` | `tga.db` | Path to SQLite database |
| `-v / -vv / -vvv` | warnings only | Increase log verbosity |

### tga analyze

Run the full pipeline: collect → classify → report.

```bash
tga analyze [--config <PATH>] [--database <PATH>] [--output <DIR>]
            [--skip-collect] [--skip-classify] [--weeks <N>]
```

| Flag | Description |
|------|-------------|
| `--skip-collect` | Skip Stage 1; use commits already in the database |
| `--skip-classify` | Skip Stage 2; use existing classifications |
| `--output <DIR>` | Override `output.directory` from config |
| `--weeks <N>` | Limit collection to the last N weeks (overrides config `start_date`) |

```bash
# Full pipeline
tga analyze --config config.yaml

# Re-run reports only (commits already collected and classified)
tga analyze --skip-collect --skip-classify --output ./reports-v2
```

### tga collect

Stage 1: extract commits from git repositories into the database.

```bash
tga collect [--config <PATH>] [--database <PATH>]
            [--repos <NAME,...>] [--since <DATE>] [--until <DATE>] [--weeks <N>]
```

| Flag | Description |
|------|-------------|
| `--repos <NAME,...>` | Comma-separated list of repository names to collect; others are skipped |
| `--since <DATE>` | Collect commits on or after this ISO 8601 date (overrides config and `--weeks`) |
| `--until <DATE>` | Collect commits on or before this ISO 8601 date (overrides config) |
| `--weeks <N>` | Limit collection to the last N weeks; `--since` takes precedence if both supplied |

```bash
tga collect --repos my-project --since 2024-01-01 --until 2024-03-31
tga collect --weeks 4   # collect last 4 weeks across all repos
```

### tga classify

Stage 2: run the classification cascade over collected commits.

```bash
tga classify [--config <PATH>] [--database <PATH>]
             [--rules <PATH>] [--use-llm]
```

| Flag | Description |
|------|-------------|
| `--rules <PATH>` | Override `classification.rules_file` from config |
| `--use-llm` | Enable LLM fallback regardless of config setting |

```bash
tga classify --rules ./custom-rules.yaml --use-llm
```

### tga report

Stage 3: generate reports from classified commits.

```bash
tga report [--config <PATH>] [--database <PATH>]
           [--output <DIR>] [--formats <FMT,...>]
```

| Flag | Description |
|------|-------------|
| `--output <DIR>` | Override `output.directory` from config |
| `--formats <FMT,...>` | Comma-separated: `csv`, `json`, `markdown` |

```bash
tga report --output ./q1-reports --formats csv,json
```

## Pipeline Architecture

```
git repos ──┐
             │  tga-collect   SQLite (tga.db)  tga-classify   SQLite   tga-report
GitHub API ──┼─────────────► [commits]        ──────────────► [classif]─────────► CSV
JIRA API ────┤  (libgit2,     [authors]          (rules +              ► JSON
Linear API ──┤  reqwest)      [pull_requests]    LLM fallback)        ► Markdown
ADO API  ───┘
```

**Stage 1 — collect** (`tga-collect`): opens each repository with libgit2, walks the configured branch, extracts commit metadata and diff stats, resolves author identities, optionally fetches GitHub PR metadata via the REST API, and writes everything to SQLite.

**Stage 2 — classify** (`tga-classify`): reads unclassified commits from the database, runs each message through the four-tier cascade (see below), and writes a classification verdict back. Tiers 1–3 execute in parallel via Rayon.

**Stage 3 — report** (`tga-report`): reads the classified database, aggregates per-author and per-week statistics, and writes the configured output formats to the output directory.

## Classification

### Four-Tier Cascade

Each commit message is tested against tiers in order. The first match wins.

**Tier 1 — Exact (Aho-Corasick)**: builds a single finite-state machine from all keyword lists across all rules, then scans the message in O(n) time. Matches `feat:`, `fix:`, `chore:`, etc. Confidence: 0.85–0.95.

**Tier 2 — Regex**: applies pre-compiled regex patterns from rules. Handles anchored conventional-commit patterns (`^feat(\([^)]*\))?!?:`) and JIRA ticket IDs (`\b[A-Z][A-Z0-9]+-\d+\b`).

**Tier 3 — Fuzzy heuristics**: detects merge commits (via `is_merge` flag or "Merge pull request" prefix) and reverts (via "Revert" prefix). No external dependencies.

**Tier 4 — LLM fallback** (optional, async): calls an OpenAI-compatible API when tiers 1–3 all fail. Reads `OPENAI_API_KEY` from the environment. Disabled by default; enable with `classification.use_llm: true` or `--use-llm`.

### Default Rules

| ID | Category | Keywords / Patterns |
|----|----------|---------------------|
| `cc-feat` | `feature` | `feat:`, `feature:`, `^feat(...)?!?:` |
| `cc-fix` | `bugfix` | `fix:`, `bugfix:`, `hotfix`, `^fix(...)?!?:` |
| `cc-chore` | `chore` | `chore:`, `^chore(...)?!?:` |
| `cc-docs` | `documentation` | `docs:`, `doc:`, `^docs?(...)?!?:` |
| `cc-refactor` | `refactor` | `refactor:`, `^refactor(...)?!?:` |
| `cc-test` | `test` | `test:`, `tests:`, `^tests?(...)?!?:` |
| `cc-ci` | `ci` | `ci:`, `^ci(...)?!?:` |
| `cc-perf` | `performance` | `perf:`, `^perf(...)?!?:` |
| `cc-style` | `style` | `style:`, `^style(...)?!?:` |
| `cc-build` | `build` | `build:`, `^build(...)?!?:` |
| `cc-revert` | `revert` | `revert:`, `^revert(...)?!?:` |
| `breaking-change` | `breaking` | `breaking change`, `breaking-change` |
| `jira-ticket` | `feature` (ticketed) | `\b[A-Z][A-Z0-9]+-\d+\b` |
| `kw-bug` | `bugfix` | `bug`, `defect` |
| `kw-security` | `bugfix` (security) | `security`, `cve-`, `vulnerability` |

Commits that match no rule are assigned category `uncategorized` with confidence 0.0.

### Custom Rules File

Supply your own rules alongside the defaults:

```yaml
# my-rules.yaml
version: "1.0"
rules:
  - id: my-deploy
    category: deployment
    keywords:
      - "deploy:"
      - "release:"
    patterns:
      - "(?i)^deploy(ment)?:"
    priority: 80
    confidence: 0.9
```

```bash
tga classify --rules ./my-rules.yaml
# or in config.yaml:
# classification:
#   rules_file: ./my-rules.yaml
```

## Output Formats

### CSV

Two files are written when `csv` is in the format list:

**`authors.csv`** — one row per author:

| Column | Description |
|--------|-------------|
| `name` | Canonical author name |
| `email` | Canonical author email |
| `commit_count` | Total commits |
| `insertions` | Total lines added |
| `deletions` | Total lines deleted |
| `files_changed` | Total files changed |
| `first_commit` | ISO 8601 timestamp of earliest commit |
| `last_commit` | ISO 8601 timestamp of most recent commit |

**`weekly_activity.csv`** — one row per week/author/repository bucket:

| Column | Description |
|--------|-------------|
| `week` | ISO week label, e.g. `2024-W03` |
| `author` | Author name |
| `repository` | Repository name |
| `commit_count` | Commits in this bucket |
| `insertions` | Lines added in this bucket |
| `deletions` | Lines deleted in this bucket |

### JSON

**`report.json`** — full structured payload:

```json
{
  "generated_at": "2024-03-15T10:00:00Z",
  "period_start": "2024-01-01T00:00:00Z",
  "period_end":   "2024-03-14T23:59:59Z",
  "total_commits": 347,
  "total_authors": 8,
  "category_breakdown": { "feature": 120, "bugfix": 45, ... },
  "authors": [
    {
      "name": "Alice Smith",
      "email": "alice@company.com",
      "commit_count": 87,
      "insertions": 4200,
      "deletions": 1100,
      "files_changed": 310,
      "categories": { "feature": 50, "bugfix": 20, ... },
      "first_commit": "...",
      "last_commit": "..."
    }
  ],
  "repositories": [
    {
      "name": "my-project",
      "commit_count": 347,
      "author_count": 8,
      "insertions": 18000,
      "deletions": 6000,
      "top_categories": [["feature", 120], ["bugfix", 45]]
    }
  ],
  "weekly_activity": [
    {
      "week": "2024-W03",
      "author": "Alice Smith",
      "repository": "my-project",
      "commit_count": 12,
      "insertions": 500,
      "deletions": 120,
      "categories": { "feature": 8, "bugfix": 4 }
    }
  ]
}
```

### Markdown

**`report.md`** — a narrative report containing a summary header, per-author commit table, category breakdown, and weekly activity section. Suitable for pasting into Confluence or a PR description.

## Development

### Build and Test

```bash
# Build everything
cargo build

# Build release binary
cargo build --release

# Run all tests
cargo test

# Lint (zero warnings required)
cargo clippy -- -D warnings

# Format check (CI gate)
cargo fmt --check

# Auto-format
cargo fmt

# Generate rustdoc
cargo doc --open
```

### Running Against Real Repos

`configs/example-config.yaml` is a working example that analyzes repositories using `developer_aliases`. Copy it, adjust paths and names to match your setup, then run:

```bash
tga analyze --config configs/example-config.yaml --database tga.db
```

### CI Gates

The GitHub Actions workflow (`ci.yml`) requires:
- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS="-D warnings"`

## Crate Structure

Single `tga` crate (consolidated from the original 5-crate workspace):

| Module | Path | Purpose |
|--------|------|---------|
| `tga::core` | `src/core/` | Shared types, config, DB schema, migrations, error types |
| `tga::collect` | `src/collect/` | Stage 1: git extraction (libgit2), GitHub/JIRA/Linear/ADO clients, `PmAdapter` trait |
| `tga::classify` | `src/classify/` | Stage 2: four-tier classification cascade |
| `tga::report` | `src/report/` | Stage 3: CSV/JSON/Markdown output |
| `commands` (binary-private) | `src/commands/` | Subcommand handlers wired into `src/main.rs` |

## License

**Non-commercial use only.** See [LICENSE](LICENSE) for terms.

Elastic License 2.0

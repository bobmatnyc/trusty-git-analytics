# tga — Agent Reference Guide

`tga` (trusty-git-analytics) is a Rust-based developer productivity analytics tool. This
document is a dense technical reference for AI agents operating autonomously.

---

## 1. Overview

**What tga does**: Three-stage pipeline over git repositories:

1. **Collect** — extract commits via `git2`, correlate with GitHub PRs / JIRA tickets /
   Linear issues / ADO work items, resolve developer identities, persist to SQLite
2. **Classify** — assign each commit a `change_type` via a seven-tier cascade (override →
   issue type → JIRA mapping → exact → regex → fuzzy → LLM fallback)
3. **Report** — aggregate into CSV (×9) / JSON / Markdown outputs, including DORA,
   velocity, and quality summaries

**Binary**: `tga` — single Rust binary, also ships as `tga::*` library crate  
**Build output**: `target/release/tga`  
**Crates.io**: `tga` v1.0.0  
**DB**: SQLite (`gitflow_cache.db` + `identities.db`); WAL mode enabled automatically on open  
**Python predecessor**: `gitflow-analytics` at `/Users/masa/Projects/gitflow-analytics`
(GitHub: `bobmatnyc/gitflow-analytics`)

Config files, alias files, and DB column names are backward-compatible with the Python version.

---

## 2. Quick Start

### Minimum working config (`config.yaml`)

```yaml
repositories:
  - name: my-app
    path: ~/code/my-app
    branch: main

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

### Invocation patterns

**Pattern A — full pipeline in one command**

```bash
tga analyze --config config.yaml --weeks 4
```

**Pattern B — staged (full control)**

```bash
tga collect --config config.yaml --from 2026-03-09 --to 2026-05-10
tga classify --config config.yaml --from 2026-03-09 --to 2026-05-10
tga report   --config config.yaml --from 2026-03-09 --to 2026-05-10
```

**Pattern C — point at a single local repo, quick 2-week window**

```bash
tga analyze \
  --config /path/to/config.yaml \
  --from 2026-04-28 --to 2026-05-10 \
  --output /tmp/reports
```

### Backfill behavior

`tga` records completed `(repo_path, iso_year, iso_week)` tuples in `weekly_fetch_status`.
Re-running the same date range is idempotent — already-collected weeks are skipped. Use
`--force` to override. Repos without explicit date bounds always re-collect.

---

## 3. CLI Reference

### Global flags (all subcommands)

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` / `-c` | `./config.yaml` | YAML config path |
| `--database <PATH>` / `-d` | `./tga.db` | SQLite database path |
| `--log <LEVEL>` | `warn` | `error` / `warn` / `info` / `debug` / `trace` (overrides `-v`) |
| `-v` / `-vv` / `-vvv` | — | Shortcut: `info` / `debug` / `trace` |
| `--help` | — | Print help |
| `--version` | — | Print version |

Set `RUST_LOG` to override both flags (e.g. `RUST_LOG=tga::collect=debug,warn`).

### ISO week targeting (mutually exclusive across subcommands)

| Flag | Repeatable | Example |
|------|-----------|---------|
| `--weeks <N>` | no | `--weeks 4` — look back N ISO weeks from today |
| `--week <YYYY-Www>` | yes | `--week 2026-W11 --week 2026-W12` |
| `--from <DATE> --to <DATE>` | no | `--from 2026-03-09 --to 2026-05-10` |

`--week` is mutually exclusive with `--weeks` and `--from`/`--to`. `--from`/`--to` must
be used together.

### `tga analyze` — full pipeline

| Flag | Default | Description |
|------|---------|-------------|
| `--weeks <N>` | 4 | Lookback |
| `--week <YYYY-Www>` | — | Specific weeks (repeatable) |
| `--from / --to` | — | ISO 8601 date range |
| `--output <PATH>` / `-o` | from config | Output directory |
| `--force` / `-f` | false | Bypass `weekly_fetch_status` immutability |
| `--anonymize` | from config | Replace identities with `dev_N` |
| `--generate-csv` | true | Emit CSV |
| `--reclassify` | false | Re-run classification on cached commits |

### `tga collect` — stage 1 only

| Flag | Default | Description |
|------|---------|-------------|
| `--weeks / --week / --from / --to` | 4 weeks | Time range |
| `--force` / `-f` | false | Override immutability |

### `tga classify` — stage 2 only

| Flag | Default | Description |
|------|---------|-------------|
| `--weeks / --week / --from / --to` | 4 weeks | Time range |
| `--reclassify` | false | Re-classify previously classified commits |
| `--show-jira-signals` | false | Per-commit tier diagnostics to stdout |
| `--validate-coverage` | false | Exit non-zero if coverage below threshold |
| `--coverage-threshold <PCT>` | 20.0 | Minimum classification coverage % |

### `tga report` — stage 3 only

| Flag | Default | Description |
|------|---------|-------------|
| `--weeks / --week / --from / --to` | 4 weeks | Time range |
| `--output <PATH>` / `-o` | from config | Output directory |
| `--generate-csv` | true | CSV output |
| `--anonymize` | from config | Identity masking |

### `tga fetch` — external APIs only (no git extraction)

| Flag | Default | Description |
|------|---------|-------------|
| `--source <SOURCE>` | `all` | `github` / `jira` / `confluence` / `all` |
| `--weeks <N>` | 4 | Lookback |

### `tga aliases` — LLM-based alias suggestion

| Flag | Default | Description |
|------|---------|-------------|
| `--apply` | false | Apply suggestions to `identities.db` |
| `--dry-run` | true | Print suggestions only |

### `tga identities` — identity management

```bash
tga identities list  [--include-aliases]
tga identities merge --source <CANONICAL_ID> --target <CANONICAL_ID>
```

### `tga pr-metrics` — PR metrics aggregation

| Flag | Default | Description |
|------|---------|-------------|
| `--weeks <N>` | 4 | |
| `--rebuild` | false | Drop existing rows for range before inserting |

### `tga override` — manual Tier 0 classification

| Flag | Description |
|------|-------------|
| `--commit <HASH>` | Commit hash |
| `--repo <PATH>` | Repository path |
| `--change-type <TYPE>` | One of 19 `change_type` values |
| `--work-type <TYPE>` | Optional override |
| `--reason <STRING>` | Required justification |
| `--remove` | Remove existing override |

### `tga install` — interactive setup wizard

```bash
tga install [--output ./config.yaml] [--force]
```

### `tga help` — extended help topics

```bash
tga help config
tga help classification
tga help iso-weeks
```

---

## 4. Configuration Reference

### Top-level structure

```yaml
repositories: []          # required
aliases_file: ""          # path to external developer alias file
developer_aliases: {}     # inline aliases (per-config overrides)
github: {}
analysis: {}
output: {}
cache: {}
jira: {}
jira_integration: {}
jira_project_mappings: {} # dict[str,str] — JIRA key → change_type
taxonomy_mapping: {}      # dict[str,str] — change_type → work_type
teams: {}
velocity: {}
activity_scoring: {}
boilerplate_filter: {}
quality_report: {}
ai_detection: {}
github_issues: {}
confluence: {}
linear: {}
```

### `repositories[]`

| Field | Type | Req | Description |
|-------|------|-----|-------------|
| `name` | string | yes | Display name |
| `path` | path | yes | Local path; supports `~` expansion |
| `github_repo` | string | no | `owner/name` for GitHub API correlation |
| `project_key` | string | no | JIRA project key prefix |
| `branch` | string | no | Override branch detection |

### `aliases_file` — external developer alias file

**Prominent field**: For organizations with many developers, maintain a single canonical
alias file shared across configs.

```yaml
aliases_file: "/path/to/aliases.yaml"
```

The aliases file format (Python-compatible, generated by `gitflow-analytics`):

```yaml
developers:
  - name: "Canonical Name"
    primary_email: "email@company.com"
    aliases:
      - "alt-email@personal.com"
      - "github-username"
    github_username: "ghslug"   # optional
    confidence: 1.0             # optional (0–1)
    reasoning: ""               # optional, for audit trail
```

Entries from `aliases_file` are merged with inline `developer_aliases`. For overlapping
canonical names, the external file wins. Paths resolve relative to the config file directory.

### `developer_aliases` — inline aliases

```yaml
developer_aliases:
  "Alice Smith":
    - "alice@example.com"
    - "alice.smith@work.com"
```

### `github`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token` | string | `$GITHUB_TOKEN` | PAT |
| `owner` | string | — | Repo owner / org |
| `organization` | string | — | Enables org-wide repo discovery |
| `base_url` | URL | `https://api.github.com` | GHE support |
| `max_retries` | u32 | 3 | |
| `backoff_factor` | f64 | 2.0 | Exponential backoff multiplier |
| `fetch_pr_reviews` | bool | true | |
| `open_pr_refresh_ttl_hours` | u32 | 1 | Refresh open PR snapshots |

### `analysis`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `exclude_authors` | list[string] | [] | Email patterns to exclude |
| `exclude_paths` | list[glob] | [] | Glob file paths to exclude from diff stats |
| `exclude_merge_commits` | bool | false | Skip merge commits entirely |
| `similarity_threshold` | f64 | 0.85 | Identity fuzzy match threshold |

#### `analysis.branch_analysis`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `strategy` | enum | `smart` | `smart` / `all` / `main_only` |
| `branch_commit_limit` | u32 | 1000 | Max commits per branch |
| `max_branches` | u32 | 50 | Max branches per repo |
| `active_days` | u32 | 90 | Smart: only branches active within N days |
| `include_patterns` | list[regex] | `release/*, hotfix/*` | Always include |
| `exclude_patterns` | list[regex] | `dependabot/*, renovate/*` | Always exclude |

#### `analysis.ticket_detection`

| Field | Type | Default |
|-------|------|---------|
| `jira_pattern` | regex | `[A-Z]{2,10}-\d+` |
| `github_pattern` | regex | `(?:closes\|fixes\|resolves)\s+#(\d+)` |
| `exclude_patterns` | list[regex] | `CVE-\d+`, `CWE-\d+`, `\d{8,}` |
| `commit_filter` | enum | `all` | `all` / `squash_merges_only` / `merge_commits` |

#### `analysis.llm_classification`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | true | |
| `provider` | enum | `openrouter` | `openrouter` / `bedrock` / `auto` |
| `model` | string | `mistralai/mistral-7b-instruct` | |
| `api_key` | string | `$OPENROUTER_API_KEY` | |
| `confidence_threshold` | f64 | 0.7 | Minimum confidence to accept LLM result |
| `batch_size` | u32 | 50 | Commits per LLM request |
| `max_tokens` | u32 | 50 | |
| `temperature` | f64 | 0.1 | |
| `timeout_seconds` | u32 | 30 | |
| `cache_ttl_days` | u32 | 90 | LLM response cache lifetime |

#### `analysis.identity`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `strip_suffixes` | list[string] | [] | Email suffixes to strip before matching |
| `manual_mappings` | list[ManualMapping] | [] | Forced canonical mappings |
| `fuzzy_threshold` | f64 | 0.85 | Override global similarity threshold |

### `output`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `directory` | path | `./reports` | Report output path |
| `formats` | list[enum] | all three | `csv` / `json` / `markdown` |
| `csv_delimiter` | string | `,` | |
| `anonymize_enabled` | bool | false | Mask identities as `dev_N` |

### `cache`

| Field | Type | Default |
|-------|------|---------|
| `directory` | path | `~/.tga-cache` |
| `ttl_hours` | u32 | 168 (7 days) |
| `max_size_mb` | u32 | 1024 |

### `jira`

| Field | Type | Default |
|-------|------|---------|
| `base_url` | URL | required if JIRA used |
| `access_user` | string | `$JIRA_USER` |
| `access_token` | string | `$JIRA_TOKEN` |

### `jira_integration`

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `fetch_story_points` | bool | true |
| `project_keys` | list[string] | [] |
| `story_point_fields` | list[string] | `["customfield_10016"]` |

### `jira_project_mappings`

JIRA project key (uppercase) → `change_type`. Used in classification Tier 3.

```yaml
jira_project_mappings:
  PLAT: platform
  SEC: security
  DOC: documentation
```

### `taxonomy_mapping`

Post-classification remap: `change_type` → `work_type`. Applied as a SQL UPDATE pass.

```yaml
taxonomy_mapping:
  feature: product_work
  bugfix: maintenance_work
  platform: platform_work
```

### `activity_scoring` — weights must sum to 1.0

| Field | Default |
|-------|---------|
| `commits_weight` | 0.22 |
| `prs_weight` | 0.26 |
| `code_impact_weight` | 0.26 |
| `complexity_weight` | 0.11 |
| `ticketing_weight` | 0.15 |

### `linear`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string | `$LINEAR_API_KEY` | Linear GraphQL API key |
| `team_keys` | list[string] | [] | e.g. `["ENG", "FE"]` |
| `fetch_on_reference` | bool | true | Enrich commits referencing Linear IDs |

### Annotated working example

```yaml
repositories:
  - name: backend-api
    path: ~/code/backend-api
    github_repo: acme/backend-api
    project_key: API
  - name: frontend-app
    path: ~/code/frontend-app
    github_repo: acme/frontend-app

aliases_file: ~/shared/developers.yaml   # shared canonical list

github:
  token: ${GITHUB_TOKEN}           # ⚠️ NOT interpolated — substitute externally
  organization: acme
  fetch_pr_reviews: true

jira:
  base_url: https://acme.atlassian.net
  access_user: ${JIRA_USER}        # ⚠️ NOT interpolated
  access_token: ${JIRA_TOKEN}      # ⚠️ NOT interpolated

jira_integration:
  enabled: true
  project_keys: [API, WEB, PLAT]

jira_project_mappings:
  PLAT: platform
  SEC: security

taxonomy_mapping:
  feature: product_work
  platform: platform_work

analysis:
  exclude_authors:
    - "dependabot[bot]@users.noreply.github.com"
  exclude_paths:
    - "**/node_modules/**"
    - "**/__generated__/**"
  branch_analysis:
    strategy: smart
    active_days: 90
  llm_classification:
    enabled: true
    provider: openrouter
    model: mistralai/mistral-7b-instruct

output:
  directory: ./reports
  formats: [csv, json, markdown]
```

> ⚠️ `${ENV_VAR}` syntax in `api_key`, `token`, and `access_token` fields is **not
> interpolated by tga**. Substitute values before writing the file, or use a config
> generation step (e.g., `envsubst < config.template.yaml > config.yaml`).

---

## 5. Classification System

### Seven-tier cascade

The first tier to produce a result wins. Every commit receives a classification — there is
no unclassified state.

| Tier | Name | Confidence | Fires when |
|------|------|-----------|-----------|
| 0 | **Manual Override** | 1.0 | `classification_overrides` table has entry for `(commit_hash, repo_path)` |
| 1.5 | **Issue Type** | 0.90 | Commit has `ticket_references` resolving to `issue_cache` rows |
| 3 | **JIRA Project Mapping** | 0.95 | `jira_project_mappings` configured AND commit refs match `[A-Z]+-\d+` |
| 4 | **Exact (Aho-Corasick)** | 0.85–0.95 | Multi-pattern keyword match across all rules |
| 5 | **Regex** | rule-defined | Pre-compiled `Regex` patterns from rule set |
| 6 | **Fuzzy heuristics** | 0.70–0.85 | Merge / revert detection, prefix heuristics |
| 7 | **LLM** | configurable (default 0.7) | LLM enabled; result accepted only when `confidence >= confidence_threshold`. Providers: **OpenRouter** (default), **AWS Bedrock** (with `--features bedrock`) |
| — | **Rule-Based Fallback** | 0.30–0.50 | Always available catch-all; defaults to `maintenance` |

**Tier 1.5 — Issue type → change_type map**

| Issue type (case-insensitive) | change_type |
|-------------------------------|-------------|
| `bug` / `defect` / `error` | `bugfix` |
| `story` / `feature` / `new feature` / `epic` / `improvement` | `feature` |
| `task` / `sub-task` / `subtask` | label-disambiguated (see below) |
| `technical task` | `maintenance` |
| `tech debt` / `infrastructure` / `platform` | `platform` |
| `spike` | `research` |
| `documentation` | `documentation` |
| `test` | `test` |

Task label disambiguation (when issue_type is task/sub-task/subtask):

| Label substring | change_type |
|-----------------|-------------|
| `platform` / `infra` / `tooling` | `platform` |
| `refactor` / `tech-debt` | `refactor` |
| `maintenance` / `chore` | `maintenance` |
| (default) | `maintenance` |

**Rule-based fallback — built-in priority order**

| Priority | change_type | Trigger patterns |
|----------|-------------|-----------------|
| 1 | `maintenance` | merge commit patterns, `chore:`, `update deps`, `bump version` |
| 2 | `bugfix` | `revert`, `fix:`, `bug:`, `resolve`, `repair`, `correct` |
| 3 | `platform` | `platform:`, `infra:`, `devops:`, `tooling:`, `architect` |
| 4 | `feature` | `feat:`, `add feature`, `implement`, `introduce` |
| 5 | `refactor` | `refactor:`, `restructure`, `optimize`, `improve`, `clean up` |
| 6 | `documentation` | `docs:`, `documentation:`, `readme` |
| 7 | `test` | `test:`, `spec:`, `add test` |
| 8 | `style` | `style:`, `format:`, `lint`, `prettier`, `whitespace` |
| (default) | `maintenance` | anything not matched above |

### Change type taxonomy (19 values)

| `change_type` | Description |
|---------------|-------------|
| `feature` | New user-facing functionality |
| `bugfix` | Defect repair |
| `platform` | Infrastructure / platform engineering |
| `refactor` | Code restructuring, no behavior change |
| `documentation` | Docs-only changes |
| `test` | Test additions or fixes |
| `maintenance` | Routine upkeep (deps, config, chores) |
| `style` | Formatting, lint fixes |
| `build` | Build system changes |
| `security` | Security patches |
| `hotfix` | Urgent production fix |
| `revert` | Code reversion |
| `integration` | Third-party integration work |
| `content` | Content updates (non-code) |
| `localization` | i18n / translation |
| `research` | Spike / investigation |
| `ktlo` | Keep-the-lights-on operational work |
| `other` | Uncategorized |
| `unknown` | Classifier could not decide |

**Fallthrough categories** (treated as "unclassified" for coverage metrics):
`maintenance`, `ktlo`, `other`, `unknown`

Coverage: `coverage_pct = 100 * (commits NOT IN fallthrough) / total_commits`  
Warning emitted when `coverage_pct < 20%`. Use `--validate-coverage` to make this a
non-zero exit.

### Catch-all safety net

The rule-based fallback defaults to `maintenance` for anything not matched by earlier rules.
Zero commits are ever truly unclassified. Any result with `confidence < 0.5` is a candidate
for LLM review.

Use `--show-jira-signals` to see per-commit tier diagnostics:

```
abc1234 → ticket_refs=[API-123] | issue_types=[bug] | tier=issue_type | result=bugfix(0.90)
```

### Custom rules

Create a rules YAML file and reference it via `analysis.llm_classification` or pass
directly. The `extend_defaults: true` flag merges your rules with the built-in set;
`extend_defaults: false` (or omitted) replaces the built-in rules entirely.

```yaml
version: "1.0"
extend_defaults: true   # true = ADD to built-in rules; false = REPLACE them

rules:
  - id: my-rule-id            # unique string, required
    category: feature         # top-level category
    subcategory: api-work     # subcategory label
    keywords:                 # fast Aho-Corasick substring match
      - "api endpoint"
      - "graphql mutation"
    patterns:                 # regex patterns (case-insensitive by default)
      - '(?i)^feat\(api\)'
      - '(?i)add.*endpoint'
    priority: 80              # higher = evaluated first (0–100)
    confidence: 0.88          # confidence assigned when this rule matches
```

Rules are evaluated in descending `priority` order. First match wins. When two rules share
the same priority, definition order in the file determines precedence.

### ⚠️ Field naming gotcha

`ClassificationResult.category` is the **subcategory name** (e.g., `"bugfix"`), NOT the
top-level category. The `top_level` field holds the `TopLevelCategory` enum value.

This naming was preserved for backward compatibility with the Python predecessor's DB schema.
The DB column `qualitative_commits.change_type` is also the subcategory/leaf value.

**When doing executive roll-up reporting**: use `top_level` (from JSON reports) or
`work_type` (after `taxonomy_mapping` is applied), not raw `change_type`.

---

## 6. Database Schema

Two SQLite files:
- `gitflow_cache.db` — commits, PRs, issues, classifications, metrics
- `identities.db` — canonical developer records and aliases

WAL mode: `PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; PRAGMA foreign_keys = ON;`

### `gitflow_cache.db` — key tables

**`cached_commits`** — primary commit records

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | autoincrement |
| `commit_hash` | TEXT UNIQUE | full OID hex |
| `repo_path` | TEXT | repository identifier |
| `author_name` | TEXT | |
| `author_email` | TEXT | |
| `canonical_id` | TEXT | FK → `identities.db.developer_identities.id` (nullable) |
| `timestamp` | DATETIME | UTC |
| `iso_week` | TEXT | `YYYY-Www` |
| `branch` | TEXT | first observed on |
| `is_merge` | BOOLEAN | parent_count > 1 |
| `message` | TEXT | |
| `files_changed` | JSON | array of paths |
| `lines_added` | INTEGER | |
| `lines_deleted` | INTEGER | |
| `filtered_insertions` | INTEGER | after `exclude_paths` |
| `filtered_deletions` | INTEGER | after `exclude_paths` |
| `story_points` | INTEGER | from message regex; nullable |
| `ticket_references` | JSON | array of ticket refs |
| `ai_confidence_score` | REAL | nullable |
| `ai_detection_method` | TEXT | nullable |
| `created_at` | DATETIME | |

Indexes: UNIQUE(`commit_hash`, `repo_path`), `iso_week`, `canonical_id`, `timestamp`

**`qualitative_commits`** — classification results

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `commit_id` | INTEGER | FK → `cached_commits.id` CASCADE DELETE |
| `change_type` | TEXT | one of 19 taxonomy values |
| `work_type` | TEXT | after `taxonomy_mapping` remap; fallback to `change_type` |
| `confidence` | REAL | 0–1 |
| `tier` | TEXT | `override` / `issue_type` / `jira_mapping` / `llm` / `rule_based` |
| `risk_level` | TEXT | `low` / `medium` / `high` |
| `domain` | TEXT | |
| `complexity_score` | REAL | |
| `model_used` | TEXT | LLM model identifier |
| `classified_at` | DATETIME | |

Indexes: UNIQUE(`commit_id`), `change_type`, `work_type`

**`daily_commit_batches`** — daily aggregation for batch tracking

| Column | Type | Notes |
|--------|------|-------|
| `repo_path` | TEXT | |
| `date` | DATE | |
| `iso_week` | TEXT | |
| `commit_count` | INTEGER | |
| `classification_status` | TEXT | `pending` / `running` / `complete` / `failed` |
| `classified_count` | INTEGER | |

Constraint: UNIQUE(`repo_path`, `date`)

**`pull_request_cache`** — GitHub PR records

Key columns: `github_repo`, `pr_number`, `author`, `pr_state`, `created_at`, `merged_at`,
`additions`, `deletions`, `approvals`, `change_requests`, `time_to_first_review_seconds`,
`revision_count`  
Constraint: UNIQUE(`github_repo`, `pr_number`)

**`issue_cache`** — JIRA / GitHub issue records

Key columns: `platform`, `external_id`, `project_key`, `issue_type`, `status`, `assignee`,
`story_points`, `labels`  
Constraint: UNIQUE(`platform`, `external_id`)

**`repository_analysis_status`** — per-repo pipeline state

Key columns: `repo_path` (PK), `last_collected_at`, `last_classified_at`, `status`,
`total_commits`, `classified_commits`, `classification_coverage_pct`, `config_hash`

**`weekly_fetch_status`** — per-repo per-ISO-week immutability flag  
**`classification_overrides`** — Tier 0 manual overrides  
**`daily_metrics`** — per-developer per-day aggregations; PK (`canonical_id`, `date`)  
**`weekly_trends`** — per-developer per-ISO-week; PK (`canonical_id`, `iso_week`)  
**`weekly_pr_metrics`** — PR review cycle-time; PK (`engineer_identifier`, `iso_week`)  
**`llm_usage_stats`** — provider/model/token usage logging  
**`schema_version`** — migration tracking  

Additional tables: `detailed_tickets`, `commit_ticket_correlations`, `classification_batches`,
`training_data`, `training_sessions`, `classification_models`, `ticketing_activity_cache`,
`confluence_page_cache`

### `identities.db`

**`developer_identities`** — canonical records; PK: UUID v4 `id`

| Column | Type |
|--------|------|
| `id` | TEXT PK (UUID v4) |
| `canonical_name` | TEXT |
| `canonical_email` | TEXT |
| `github_login` | TEXT |
| `created_at` | DATETIME |
| `last_seen_at` | DATETIME |

**`developer_aliases`** — observed identity → canonical record

| Column | Type |
|--------|------|
| `canonical_id` | TEXT (FK) |
| `name` | TEXT |
| `email` | TEXT |
| `source` | TEXT (`git` / `github` / `jira` / `manual`) |
| `confidence` | REAL |

Indexes: `email`, `name`

### Migration versions

Schema auto-migrates on DB open (v1–v18 from Python port; v19+ for Rust additions).

| Version | Description |
|---------|-------------|
| v1 | Initial schema (cached_commits, identities) |
| v2 | Add `qualitative_commits` |
| v3 | Add `pull_request_cache` |
| v4 | Add `issue_cache` |
| v5 | Add `daily_commit_batches`, classification_status |
| v6 | Add `repository_analysis_status` with config_hash |
| v7 | Add `daily_metrics`, `weekly_trends` |
| v8 | Add `weekly_pr_metrics` |
| v9 | Add `classification_overrides` (Tier 0) |
| v10 | Add `commit_ticket_correlations` |
| v11 | Add `detailed_tickets` |
| v12–v18 | LLM stats, training data, fetch immutability, cache tables |

### Useful SQL queries

```sql
-- Category breakdown for a date range
SELECT qc.change_type, COUNT(*) AS n,
       ROUND(100.0*COUNT(*)/SUM(COUNT(*)) OVER (), 1) AS pct
FROM cached_commits cc
JOIN qualitative_commits qc ON cc.id = qc.commit_id
WHERE cc.timestamp BETWEEN '2026-03-09' AND '2026-05-10'
GROUP BY qc.change_type ORDER BY n DESC;

-- Ticketed vs unticketed by week
SELECT cc.iso_week,
       SUM(CASE WHEN json_array_length(cc.ticket_references) > 0 THEN 1 ELSE 0 END) AS ticketed,
       SUM(CASE WHEN json_array_length(cc.ticket_references) = 0 THEN 1 ELSE 0 END) AS unticketed,
       COUNT(*) AS total
FROM cached_commits cc
GROUP BY cc.iso_week ORDER BY cc.iso_week;

-- Per-author canonical summary
SELECT di.canonical_name, COUNT(*) AS commits,
       SUM(cc.lines_added) AS ins, SUM(cc.lines_deleted) AS del
FROM cached_commits cc
LEFT JOIN developer_identities di ON cc.canonical_id = di.id
GROUP BY COALESCE(di.canonical_name, cc.author_name)
ORDER BY commits DESC;

-- Low-confidence commits (candidates for LLM review)
SELECT cc.commit_hash, cc.message, qc.change_type, qc.confidence, qc.tier
FROM cached_commits cc
JOIN qualitative_commits qc ON cc.id = qc.commit_id
WHERE qc.confidence < 0.5
ORDER BY qc.confidence ASC;

-- Which weeks have been collected for a repo
SELECT repo_path, iso_week, commit_count
FROM daily_commit_batches
WHERE repo_path LIKE '%duetto-monolith%'
  AND classification_status = 'complete'
ORDER BY iso_week;

-- Work type distribution after taxonomy_mapping
SELECT qc.work_type, COUNT(*) AS n,
       ROUND(100.0*COUNT(*)/SUM(COUNT(*)) OVER (), 1) AS pct
FROM qualitative_commits qc
GROUP BY qc.work_type ORDER BY n DESC;

-- Coverage check per repo
SELECT repo_path, total_commits, classified_commits,
       ROUND(classification_coverage_pct, 1) AS coverage_pct
FROM repository_analysis_status;

-- LLM usage by model
SELECT model_used, COUNT(*) AS classifications,
       AVG(confidence) AS avg_confidence
FROM qualitative_commits
WHERE tier = 'llm'
GROUP BY model_used;
```

---

## 7. Identity Resolution

Four-step process applied during collection:

1. **Exact alias match** — lowercase `(author_name, author_email)` looked up against the
   alias map loaded from `aliases_file` + inline `developer_aliases`
2. **Jaro-Winkler fuzzy match** — applied to raw name and email; threshold 0.85 (configurable
   via `analysis.similarity_threshold`)
3. **Email local-part normalized fuzzy match** — `bob.matsuoka@co.com` →
   `bob matsuoka`, threshold 0.82; catches dot-vs-space name variants
4. **Pass-through** — unknown identity stored as-is; `canonical_id` is NULL in
   `cached_commits`

Report aggregation joins on `canonical_id → developer_identities.canonical_name`. When
`canonical_id` is NULL (pre-resolver or unresolved commit), raw git `author_name`/
`author_email` are used as fallback. This means unresolved authors appear as separate rows
in per-author summaries.

To investigate resolution gaps: `tga identities list --include-aliases` then
`tga aliases --dry-run` for LLM suggestions.

---

## 8. Known Bugs and Caveats

| Issue | Impact | Workaround |
|-------|--------|-----------|
| `qualitative_commits.change_type` is subcategory name, not top-level | Roll-up queries break if you GROUP BY `change_type` expecting top-level | Always check `work_type` (after taxonomy_mapping) for executive summaries; use JSON report `top_level` field |
| `${ENV_VAR}` not interpolated in config | API keys written literally land in config file | Use `envsubst` or a generation step before invoking tga |
| `ticketed` signal requires ticket_references | Old data collected before JIRA/GitHub integration shows empty `ticket_references` | Re-collect with APIs enabled to backfill |
| No `--weeks N` → no shorthand for "last N weeks" without math | Must compute dates manually for ad hoc runs | `--weeks N` IS supported — see CLI reference above |
| `weekly_fetch_status` only guards repos with explicit date bounds | Repos without `--from`/`--to` always re-collect | Always use explicit date bounds for reproducibility |
| LLM provider is OpenRouter by default; `OPENROUTER_API_KEY` env var | `OPENAI_API_KEY` alone is insufficient | Set `OPENROUTER_API_KEY` or configure `provider: bedrock` |
| LLM circuit breaker: 3 consecutive batch failures disables LLM for run | Silent fallback to rule-based; coverage drops | Check `--log debug` output for LLM error details |
| `analysis.branch_analysis` `smart` strategy may miss feature branches inactive > 90 days | Historical analysis may be incomplete | Set `strategy: all` or lower `active_days` for historical runs |
| Identity resolution is applied at collect time | Re-identifying requires re-collect | After updating `aliases_file`, re-run `tga collect --force` |
| `work_type` falls back to `change_type` when no `taxonomy_mapping` entry exists | Mixed vocabulary in roll-up reports | Ensure all 19 `change_type` values are mapped in `taxonomy_mapping` |

---

## 9. tga vs gfa — Differences

### Compatible (same in both)

- `developer_aliases` YAML format and `aliases_file` external alias format
- Config fields: `repositories`, `github`, `jira`, `output`, `analysis.exclude_*`
- DB column names (`change_type`, `work_type`, `commit_hash`, etc.)
- Classification rule YAML schema (rules files are portable)
- JIRA ticket reference extraction pattern
- Story point extraction from commit messages

### tga only (not in gfa)

- Per-week incremental backfill via `weekly_fetch_status` immutability table
- `--force` flag backed by SQLite, not filesystem cache
- `linear` integration (Linear GraphQL API enrichment)
- `TaxonomyRegistry` for `TopLevelCategory` roll-up
- `analysis.llm_classification` supporting OpenRouter and Bedrock providers
- `classify --show-jira-signals` per-commit diagnostic output
- `classify --validate-coverage` / `--coverage-threshold` CI gate
- Bundled SQLite via `rusqlite` — no system dependency
- Single binary via `cargo install tga`
- `RuleSet.extend_defaults` merge mode (true/false) in rule files
- Email local-part normalized fuzzy tier in identity resolution
- WAL journal mode by default (Python used DELETE mode)
- `mmap_size = 256 MB` for large DB reads
- `blake3` config hashing in `repository_analysis_status.config_hash`

### gfa only (not yet in tga)

- **DORA metrics** (deployment frequency, lead time, change failure rate, MTTR)
- **ML categorization** (spaCy, scikit-learn, AWS Bedrock)
- **Story point extraction and JIRA velocity** analytics
- **PR lifecycle metrics** and rejection rate
- **`ticketing_score`** composite metric
- **Override management UI** — interactive wizard for manual classification edits
- **Interactive identity consolidation wizard**
- **Data anonymization** pipeline
- **GitHub org-wide repository discovery** (without explicit repo list)
- **ClickUp ticket references**
- **Confluence activity integration** (partially stubbed in tga schema)
- **`--weeks N` convenience flag** (⚠️ tga does support `--weeks N` per CLI spec above;
  verify against your installed binary version)
- **Backfill subcommands** (`backfill-tickets`, `backfill-prs`, etc.)

---

## 10. Self-Running Guide for Agents

### Step 1: Verify binary exists

```bash
ls -la /Users/masa/Projects/trusty-git-analytics/target/release/tga
```

If missing:

```bash
cargo build --release \
  --manifest-path /Users/masa/Projects/trusty-git-analytics/Cargo.toml
```

### Step 2: Known data locations (Duetto context)

| Resource | Path |
|----------|------|
| CTO DB (W11–W19, 384 commits, canonical names) | `/Users/masa/Duetto/repos/reports/cto-w11-w19.db` |
| CTO reports directory | `/Users/masa/Duetto/repos/reports/cto-w11-w19/` |
| Duetto alias file (95 canonical developers) | `/Users/masa/Duetto/repos/duetto-aliases.yaml` |
| Duetto monolith config | `/Users/masa/Projects/trusty-git-analytics/configs/duetto-monolith.yaml` |
| CTO-specific config | `/Users/masa/Projects/trusty-git-analytics/configs/duetto-monolith-cto-w11-w19.yaml` |
| Classification rules | `/Users/masa/Projects/trusty-git-analytics/configs/duetto-rules.yaml` |

### Step 3: Run a fresh analysis

```bash
/Users/masa/Projects/trusty-git-analytics/target/release/tga analyze \
  --config /Users/masa/Projects/trusty-git-analytics/configs/duetto-monolith.yaml \
  --from 2026-03-09 --to 2026-05-10 \
  --output /tmp/tga-fresh
```

### Step 4: Query the existing CTO DB directly (no re-run needed)

```bash
# Category breakdown
sqlite3 /Users/masa/Duetto/repos/reports/cto-w11-w19.db \
  "SELECT qc.change_type, COUNT(*) AS n
   FROM cached_commits cc
   JOIN qualitative_commits qc ON cc.id = qc.commit_id
   GROUP BY qc.change_type ORDER BY n DESC;"

# Per-developer commit counts
sqlite3 /Users/masa/Duetto/repos/reports/cto-w11-w19.db \
  "SELECT COALESCE(di.canonical_name, cc.author_name) AS developer, COUNT(*) AS commits
   FROM cached_commits cc
   LEFT JOIN developer_identities di ON cc.canonical_id = di.id
   GROUP BY developer ORDER BY commits DESC;"
```

Note: `identities.db` is a separate file. Attach it for cross-DB queries:

```bash
sqlite3 /Users/masa/Duetto/repos/reports/cto-w11-w19.db \
  "ATTACH '/path/to/identities.db' AS ids;
   SELECT ids.developer_identities.canonical_name, COUNT(*) AS n
   FROM cached_commits cc
   JOIN ids.developer_identities ON cc.canonical_id = ids.developer_identities.id
   GROUP BY 1 ORDER BY 2 DESC;"
```

### Step 5: Interpret results

| Signal | Meaning |
|--------|---------|
| `confidence < 0.5` | Catch-all / rule-based match; treat as noisy unless `--use-llm` was active |
| `tier = rule_based` AND `change_type = maintenance` | Default fallthrough; may be mis-classified |
| `ticket_references = '[]'` | Commit has no ticket reference; ticketing hygiene issue |
| `classification_coverage_pct < 20` | Most commits are in fallthrough categories; add custom rules |
| `canonical_id IS NULL` | Author not resolved; check `aliases_file` coverage |

### Step 6: Add a classification rule when catch-all fires incorrectly

Edit `/Users/masa/Projects/trusty-git-analytics/configs/duetto-rules.yaml`, add an entry
under `rules:` with a `patterns:` list matching the commit message pattern, appropriate
`category`/`subcategory`, and `priority` above 50. Set `extend_defaults: true` to keep
built-in rules.

After editing:

```bash
cd /Users/masa/Projects/trusty-git-analytics
cargo test -- corpus   # run corpus tests
```

Then re-classify:

```bash
tga classify \
  --config configs/duetto-monolith.yaml \
  --from 2026-03-09 --to 2026-05-10 \
  --reclassify
```

### Step 7: Build and run tests

```bash
cd /Users/masa/Projects/trusty-git-analytics
cargo build                         # debug build
cargo build --release               # release binary
cargo test                          # all tests (247 total: unit + integration + doctests)
cargo clippy -- -D warnings         # must be zero warnings for CI
cargo fmt --check                   # formatting gate
```

### Step 8: Debug classification issues

```bash
tga classify \
  --config configs/duetto-monolith.yaml \
  --from 2026-03-09 --to 2026-05-10 \
  --show-jira-signals \
  --log debug \
  --reclassify
```

Look for commits where `tier=rule_based` and `confidence=0.3`–`0.5` — these are the
catch-all matches most likely to benefit from a custom rule.

# Database Schema

`trusty-git-analytics` uses two SQLite databases, matching the schema of the Python
predecessor. WAL journal mode is enabled to support concurrent reads during write-heavy
collection phases.

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA foreign_keys = ON;
```

## Files

- `gitflow_cache.db` — primary cache of commits, PRs, issues, metrics, classifications
- `identities.db` — developer identity resolution database (canonical IDs and aliases)

---

## gitflow_cache.db Tables

### `cached_commits`

Primary commit records extracted from git.

| Column | Type | Null | Notes |
|--------|------|------|-------|
| `id` | INTEGER PK | no | Autoincrement |
| `commit_hash` | TEXT | no | Full OID hex |
| `repo_path` | TEXT | no | Repository identifier |
| `author_name` | TEXT | no | |
| `author_email` | TEXT | no | |
| `canonical_id` | TEXT | yes | FK to identities.db.developer_identities.id |
| `timestamp` | DATETIME | no | UTC |
| `iso_week` | TEXT | no | `YYYY-Www` |
| `branch` | TEXT | yes | |
| `is_merge` | BOOLEAN | no | parents > 1 |
| `message` | TEXT | no | |
| `files_changed` | JSON | no | Array of paths |
| `lines_added` | INTEGER | no | |
| `lines_deleted` | INTEGER | no | |
| `filtered_insertions` | INTEGER | no | After exclude_paths |
| `filtered_deletions` | INTEGER | no | After exclude_paths |
| `story_points` | INTEGER | yes | From message regex |
| `ticket_references` | JSON | no | Array of ticket refs |
| `ai_confidence_score` | REAL | yes | 0–1 |
| `ai_detection_method` | TEXT | yes | |
| `created_at` | DATETIME | no | |

**Indexes**: UNIQUE(`commit_hash`, `repo_path`), INDEX(`iso_week`), INDEX(`canonical_id`),
INDEX(`timestamp`).

### `qualitative_commits`

Classification results per commit. FK to `cached_commits.id`.

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `commit_id` | INTEGER | FK → `cached_commits.id` ON DELETE CASCADE |
| `change_type` | TEXT | One of 19 taxonomy values |
| `work_type` | TEXT | After taxonomy_mapping remap |
| `confidence` | REAL | 0–1 |
| `tier` | TEXT | `override` / `issue_type` / `jira_mapping` / `llm` / `rule_based` |
| `risk_level` | TEXT | `low` / `medium` / `high` |
| `domain` | TEXT | |
| `complexity_score` | REAL | |
| `model_used` | TEXT | LLM model identifier |
| `classified_at` | DATETIME | |

**Indexes**: UNIQUE(`commit_id`), INDEX(`change_type`), INDEX(`work_type`).

### `daily_commit_batches`

Daily aggregation per repo for classification batch tracking.

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `repo_path` | TEXT | |
| `date` | DATE | |
| `iso_week` | TEXT | |
| `commit_count` | INTEGER | |
| `classification_status` | TEXT | `pending` / `running` / `complete` / `failed` |
| `classified_count` | INTEGER | |
| `last_attempted_at` | DATETIME | |

**Constraint**: UNIQUE(`repo_path`, `date`).

### `pull_request_cache`

GitHub PR records.

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `repo_path` | TEXT | |
| `github_repo` | TEXT | `owner/name` |
| `pr_number` | INTEGER | |
| `title` | TEXT | |
| `description` | TEXT | |
| `author` | TEXT | GitHub login |
| `author_canonical_id` | TEXT | |
| `pr_state` | TEXT | `open` / `closed` / `merged` |
| `created_at` | DATETIME | |
| `updated_at` | DATETIME | |
| `merged_at` | DATETIME | |
| `closed_at` | DATETIME | |
| `labels` | JSON | |
| `commit_hashes` | JSON | |
| `additions` | INTEGER | |
| `deletions` | INTEGER | |
| `changed_files` | INTEGER | |
| `approvals` | INTEGER | |
| `change_requests` | INTEGER | |
| `time_to_first_review_seconds` | INTEGER | |
| `revision_count` | INTEGER | |
| `cached_at` | DATETIME | |

**Constraint**: UNIQUE(`github_repo`, `pr_number`).

### `issue_cache`

External ticket records (JIRA, GitHub issues).

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `platform` | TEXT | `jira` / `github` |
| `external_id` | TEXT | e.g. `API-123`, `42` |
| `project_key` | TEXT | |
| `issue_type` | TEXT | bug / story / task / etc. |
| `title` | TEXT | |
| `description` | TEXT | |
| `status` | TEXT | |
| `assignee` | TEXT | |
| `story_points` | REAL | |
| `labels` | JSON | |
| `created_at` | DATETIME | |
| `updated_at` | DATETIME | |
| `resolved_at` | DATETIME | |
| `cached_at` | DATETIME | |

**Constraint**: UNIQUE(`platform`, `external_id`).

### `repository_analysis_status`

Per-repo tracking of analysis state and coverage.

| Column | Type | Notes |
|--------|------|-------|
| `repo_path` | TEXT PK | |
| `last_collected_at` | DATETIME | |
| `last_classified_at` | DATETIME | |
| `last_reported_at` | DATETIME | |
| `status` | TEXT | `idle` / `collecting` / `classifying` / `reporting` / `failed` |
| `total_commits` | INTEGER | |
| `classified_commits` | INTEGER | |
| `classification_coverage_pct` | REAL | |
| `config_hash` | TEXT | blake3 hash of config inputs |

### `daily_metrics`

Per-developer per-day aggregations.

| Column | Type | Notes |
|--------|------|-------|
| `canonical_id` | TEXT | |
| `date` | DATE | |
| `commits` | INTEGER | |
| `lines_added` | INTEGER | |
| `lines_deleted` | INTEGER | |
| `prs_opened` | INTEGER | |
| `prs_merged` | INTEGER | |
| `story_points` | REAL | |

**PK**: (`canonical_id`, `date`).

### `weekly_trends`

Per-developer per-ISO-week aggregations.

| Column | Type | Notes |
|--------|------|-------|
| `canonical_id` | TEXT | |
| `iso_week` | TEXT | |
| `commits` | INTEGER | |
| `lines_changed` | INTEGER | |
| `prs_merged` | INTEGER | |
| `activity_score` | REAL | |

**PK**: (`canonical_id`, `iso_week`).

### `weekly_pr_metrics`

Per-engineer per-ISO-week PR review/cycle-time metrics.

| Column | Type | Notes |
|--------|------|-------|
| `engineer_identifier` | TEXT | |
| `iso_week` | TEXT | |
| `prs_opened` | INTEGER | |
| `prs_merged` | INTEGER | |
| `avg_cycle_time_hours` | REAL | |
| `median_cycle_time_hours` | REAL | |
| `avg_revision_count` | REAL | |
| `total_approvals_given` | INTEGER | |
| `total_change_requests_given` | INTEGER | |

**PK**: (`engineer_identifier`, `iso_week`).

### `classification_overrides`

Manual override entries (Tier 0).

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `commit_hash` | TEXT | |
| `repo_path` | TEXT | |
| `change_type` | TEXT | |
| `work_type` | TEXT | |
| `reason` | TEXT | |
| `created_by` | TEXT | |
| `created_at` | DATETIME | |

**Constraint**: UNIQUE(`commit_hash`, `repo_path`).

### `schema_version`

Migration tracking.

| Column | Type | Notes |
|--------|------|-------|
| `version` | INTEGER PK | |
| `applied_at` | DATETIME | |
| `description` | TEXT | |

### Additional Tables

- `detailed_tickets` — full JIRA ticket detail snapshots
- `commit_ticket_correlations` — many-to-many commits ↔ tickets
- `classification_batches` — LLM batch dispatch tracking
- `llm_usage_stats` — provider/model/token usage logging
- `training_data` — manual labels for fine-tuning
- `training_sessions` — training run history
- `classification_models` — local model registry
- `weekly_fetch_status` — per-repo per-ISO-week fetch immutability flag
- `ticketing_activity_cache` — non-commit-linked ticket events
- `confluence_page_cache` — Confluence document cache

---

## identities.db Tables

### `developer_identities`

Canonical developer records.

| Column | Type | Notes |
|--------|------|-------|
| `id` | TEXT PK | UUID v4 |
| `canonical_name` | TEXT | |
| `canonical_email` | TEXT | |
| `github_login` | TEXT | |
| `created_at` | DATETIME | |
| `last_seen_at` | DATETIME | |

### `developer_aliases`

Mapping from observed identity to canonical record.

| Column | Type | Notes |
|--------|------|-------|
| `id` | INTEGER PK | |
| `canonical_id` | TEXT | FK → `developer_identities.id` |
| `name` | TEXT | |
| `email` | TEXT | |
| `source` | TEXT | `git` / `github` / `jira` / `manual` |
| `confidence` | REAL | |

**Indexes**: INDEX(`email`), INDEX(`name`).

### `pattern_cache`

NLP feature cache keyed by canonical content hash.

---

## Migration Versions (v1–v18)

The Python predecessor introduced 18 schema migrations. The Rust port replays these as a
sequence of versioned SQL migrations at startup. Summary:

| Version | Description |
|---------|-------------|
| v1 | Initial schema (cached_commits, developer_identities, developer_aliases) |
| v2 | Add `qualitative_commits` |
| v3 | Add `pull_request_cache` |
| v4 | Add `issue_cache` |
| v5 | Add `daily_commit_batches`, classification_status enum |
| v6 | Add `repository_analysis_status` with config_hash |
| v7 | Add `daily_metrics`, `weekly_trends` |
| v8 | Add `weekly_pr_metrics` with PR review fields |
| v9 | Add `classification_overrides` (Tier 0) |
| v10 | Add `commit_ticket_correlations` join table |
| v11 | Add `detailed_tickets` for JIRA snapshots |
| v12 | Add `llm_usage_stats` |
| v13 | Add `classification_batches` |
| v14 | Add `training_data`, `training_sessions` |
| v15 | Add `classification_models` registry |
| v16 | Add `weekly_fetch_status` immutability |
| v17 | Add `ticketing_activity_cache` |
| v18 | Add `confluence_page_cache` |

Future migrations (v19+) will be added for Rust-specific improvements.

## Rust Improvements Over Python

- **WAL mode** enabled by default (Python predecessor used DELETE journal mode)
- Better composite indexes for collection/classification join queries
- Single connection per crate with explicit transactions for batch writes (no SQLAlchemy session overhead)
- `PRAGMA mmap_size = 268435456;` (256 MB) for memory-mapped reads on large caches

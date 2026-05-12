# Configuration Reference

The configuration file is YAML, matching the schema used by the Python predecessor
`gitflow-analytics`. All keys are deserialized via `serde_yaml` into typed structs in
`tga-core::config`. Paths support `~` expansion via the `shellexpand` crate.

## Top-Level Structure

```yaml
repositories: []          # list[RepositoryConfig], required
github: {}                # GitHubConfig
bitbucket: {}             # BitbucketConfig (Cloud only)
analysis: {}              # AnalysisConfig
output: {}                # OutputConfig
cache: {}                 # CacheConfig
jira: {}                  # JIRAConfig
jira_integration: {}      # JIRAIntegrationConfig
jira_project_mappings: {} # dict[str,str]
taxonomy_mapping: {}      # dict[str,str]
teams: {}                 # TeamsConfig
velocity: {}              # VelocityConfig
activity_scoring: {}      # ActivityScoringConfig
boilerplate_filter: {}    # BoilerplateFilterConfig
quality_report: {}        # QualityReportConfig
ai_detection: {}          # AIDetectionConfig
github_issues: {}         # GitHubIssuesConfig
confluence: {}            # ConfluenceConfig
```

## Sections

### `repositories[]` — RepositoryConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `name` | string | required | Display name for the repository |
| `path` | path | required | Local filesystem path (supports `~`) |
| `github_repo` | string | None | `owner/name` for GitHub API correlation |
| `project_key` | string | None | JIRA project key prefix |
| `branch` | string | None | Override default branch detection |

### `github` — GitHubConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `token` | string | env `GITHUB_TOKEN` | GitHub Personal Access Token |
| `owner` | string | None | Repository owner / org |
| `organization` | string | None | If set, discover all repos from this org |
| `base_url` | url | `https://api.github.com` | API base URL (GHE support) |
| `max_retries` | u32 | 3 | Retry count on transient failures |
| `backoff_factor` | f64 | 2.0 | Exponential backoff multiplier |
| `fetch_pr_reviews` | bool | true | Fetch review summaries with PRs |
| `open_pr_refresh_ttl_hours` | u32 | 1 | TTL for refreshing open PR snapshots |

### `bitbucket` — BitbucketConfig

Bitbucket Cloud only. Bitbucket Server / Data Center is not supported.

Authentication accepts either an access token (Bearer) or an App Password
(Basic auth). Token takes precedence when both are populated.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `username` | string | None | Bitbucket account / workspace member username (required for Basic auth) |
| `app_password` | string | env `BITBUCKET_APP_PASSWORD` | Bitbucket App Password (Basic auth secret) |
| `token` | string | env `BITBUCKET_TOKEN` | Workspace / repository access token (Bearer auth) |
| `workspace` | string | required when `fetch_prs: true` | Workspace slug (`myteam` in `bitbucket.org/myteam/myrepo`) |
| `repo_slug` | string | required when `fetch_prs: true` | Repository slug (`myrepo` in `bitbucket.org/myteam/myrepo`) |
| `fetch_prs` | bool | `false` | Fetch pull request metadata |
| `api_base_url` | url | `https://api.bitbucket.org/2.0` | API base URL override (test seam) |

State mapping into the shared `pull_requests` table:

| Bitbucket state | Stored as |
|-----------------|-----------|
| `OPEN` | `open` |
| `MERGED` | `merged` |
| `DECLINED` | `closed` |
| `SUPERSEDED` | `closed` |

`DECLINED` and `SUPERSEDED` collapse onto `closed` because the shared schema
has no richer variants. Reports that need to distinguish them must consult
the raw Bitbucket payload, which is currently not persisted.

### `analysis` — AnalysisConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `exclude_authors` | list[string] | [] | Email patterns to exclude |
| `exclude_paths` | list[glob] | [] | File path globs to exclude from diff stats |
| `exclude_merge_commits` | bool | false | Skip merge commits entirely |
| `similarity_threshold` | f64 | 0.85 | Identity fuzzy match threshold (0–1) |
| `branch_analysis` | BranchAnalysisConfig | smart | Branch selection strategy |
| `ticket_detection` | TicketDetectionConfig | {} | Ticket regex configuration |
| `llm_classification` | LlmClassificationConfig | {} | LLM provider settings |
| `identity` | IdentityConfig | {} | Identity resolution settings |

#### `analysis.branch_analysis`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `strategy` | enum | `smart` | `smart` / `all` / `main_only` |
| `branch_commit_limit` | u32 | 1000 | Max commits per branch |
| `max_branches` | u32 | 50 | Max branches per repo |
| `active_days` | u32 | 90 | Only branches with commits in last N days (smart) |
| `include_patterns` | list[regex] | release/*, hotfix/* | Always-include patterns |
| `exclude_patterns` | list[regex] | dependabot/*, renovate/* | Always-exclude patterns |

#### `analysis.ticket_detection`

| Field | Type | Default |
|-------|------|---------|
| `jira_pattern` | regex | `[A-Z]{2,10}-\d+` |
| `github_pattern` | regex | `(?:closes\|fixes\|resolves)\s+#(\d+)` |
| `exclude_patterns` | list[regex] | `CVE-\d+`, `CWE-\d+`, `\d{8,}` |
| `commit_filter` | enum | `all` | `all` / `squash_merges_only` / `merge_commits` |

#### `analysis.llm_classification`

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | true |
| `provider` | enum | `openrouter` (`openrouter` / `bedrock` / `auto`) |
| `model` | string | `mistralai/mistral-7b-instruct` |
| `api_key` | string | env `OPENROUTER_API_KEY` |
| `confidence_threshold` | f64 | 0.7 |
| `batch_size` | u32 | 50 |
| `max_tokens` | u32 | 50 |
| `temperature` | f64 | 0.1 |
| `timeout_seconds` | u32 | 30 |
| `cache_ttl_days` | u32 | 90 |

#### `analysis.identity`

| Field | Type | Default |
|-------|------|---------|
| `strip_suffixes` | list[string] | [] | Email suffixes to strip before matching |
| `manual_mappings` | list[ManualMapping] | [] | Forced canonical mappings |
| `fuzzy_threshold` | f64 | 0.85 |

### `output` — OutputConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `directory` | path | `./reports` | Where reports are written |
| `formats` | list[enum] | `[csv, json, markdown]` | Output formats |
| `csv_delimiter` | string | `","` | CSV delimiter |
| `csv_encoding` | string | `utf-8` | CSV encoding |
| `anonymize_enabled` | bool | false | Replace identities with `dev_N` IDs |

### `cache` — CacheConfig

| Field | Type | Default |
|-------|------|---------|
| `directory` | path | `~/.tga-cache` |
| `ttl_hours` | u32 | 168 (7 days) |
| `max_size_mb` | u32 | 1024 |

### `jira` — JIRAConfig

| Field | Type | Default |
|-------|------|---------|
| `access_user` | string | env `JIRA_USER` |
| `access_token` | string | env `JIRA_TOKEN` |
| `base_url` | url | required if JIRA used |

### `jira_integration` — JIRAIntegrationConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `fetch_story_points` | bool | true |
| `project_keys` | list[string] | [] |
| `story_point_fields` | list[string] | `["customfield_10016"]` |

### `jira_project_mappings`

`dict<string,string>` — JIRA project key (uppercase) → change_type. Used in classification
Tier 3. Example:

```yaml
jira_project_mappings:
  PLAT: platform
  SEC: security
  DOC: documentation
```

### `taxonomy_mapping`

`dict<string,string>` — change_type → work_type custom remap. Applied as a SQL UPDATE pass
after classification. Example:

```yaml
taxonomy_mapping:
  feature: product_work
  bugfix: maintenance_work
  platform: platform_work
```

### `teams` — TeamsConfig

| Field | Type | Description |
|-------|------|-------------|
| `definitions` | dict[string, list[string]] | Team name → list of canonical IDs / emails |

### `velocity` — VelocityConfig

| Field | Type | Default |
|-------|------|---------|
| `cycle_time_min_hours` | f64 | 0.5 |
| `cycle_time_max_hours` | f64 | 720.0 |

### `activity_scoring` — ActivityScoringConfig

Weights must sum to 1.0:

| Field | Type | Default |
|-------|------|---------|
| `commits_weight` | f64 | 0.22 |
| `prs_weight` | f64 | 0.26 |
| `code_impact_weight` | f64 | 0.26 |
| `complexity_weight` | f64 | 0.11 |
| `ticketing_weight` | f64 | 0.15 |

### `boilerplate_filter` — BoilerplateFilterConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `avg_lines_per_commit_threshold` | u32 | 500 |
| `total_lines_threshold` | u32 | 10000 |
| `action` | enum | `flag` | `flag` / `exclude_from_averages` / `exclude` |

### `quality_report` — QualityReportConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | true |
| `revert_patterns` | list[regex] | `["^revert", "rollback", "hotfix"]` |
| `min_revision_warning` | u32 | 3 |

### `ai_detection` — AIDetectionConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `confidence_threshold` | f64 | 0.7 |
| `signals` | list[enum] | all |

### `github_issues` — GitHubIssuesConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | true |
| `fetch_closed` | bool | true |
| `lookback_days` | u32 | 365 |

### `confluence` — ConfluenceConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `base_url` | url | None |
| `access_user` | string | env `CONFLUENCE_USER` |
| `access_token` | string | env `CONFLUENCE_TOKEN` |
| `space_keys` | list[string] | [] |

## Complete Example

```yaml
repositories:
  - name: backend-api
    path: ~/code/backend-api
    github_repo: acme/backend-api
    project_key: API
  - name: frontend-app
    path: ~/code/frontend-app
    github_repo: acme/frontend-app
    project_key: WEB

github:
  token: ${GITHUB_TOKEN}
  organization: acme
  fetch_pr_reviews: true

jira:
  base_url: https://acme.atlassian.net
  access_user: ${JIRA_USER}
  access_token: ${JIRA_TOKEN}

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

cache:
  directory: ~/.tga-cache
  ttl_hours: 168
```

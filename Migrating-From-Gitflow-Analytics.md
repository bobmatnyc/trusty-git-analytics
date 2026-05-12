# Migrating from gitflow-analytics to trusty-git-analytics

This guide is for teams running `gitflow-analytics` (`gfa`) in production who want to migrate to
`trusty-git-analytics` (`tga`). It covers installation, config field-by-field changes, CLI command
mapping, database differences, and a step-by-step checklist.

---

## Table of Contents

1. [Why Migrate](#1-why-migrate)
2. [Installation](#2-installation)
3. [Config Migration — Field-by-Field](#3-config-migration--field-by-field)
4. [CLI Command Mapping](#4-cli-command-mapping)
5. [Database Migration](#5-database-migration)
6. [Classification Rules Migration](#6-classification-rules-migration)
7. [Step-by-Step Migration Checklist](#7-step-by-step-migration-checklist)
8. [Running Both in Parallel](#8-running-both-in-parallel)

---

## 1. Why Migrate

### Advantages of tga

| Area | gfa (Python) | tga (Rust) |
|------|-------------|-----------|
| **Installation** | `pip install`, `venv`, optional spaCy model download | Single static binary — `cargo install tga` or copy binary |
| **Runtime dependency** | Python 3.10+, system spaCy | None |
| **Collection speed** | Baseline | ~10–50× faster via Rayon parallelism; no GIL |
| **SQLite** | Requires system library | Bundled — no system SQLite required |
| **Incremental backfill** | Re-runs must be managed manually | Per-week (repo, year, week) state tracked in DB — re-runs always safe |
| **Taxonomy** | Single-level category | Two-level: subcategory + 7 canonical top-level rollups |
| **Ticket detection** | Configurable patterns, per-repo exclusions | Automatic on every commit; `ticketed: bool` column in DB |
| **Linear support** | Not supported | Native — `linear_issues` table, ref detection built in |

### Current Limitations of tga (honest)

As of v1.0.0, `tga` reproduces every analytical output that `gfa` produces. The following
gfa-era gaps have been **closed in v1.0.0**:

- DORA metrics (`lead_time`, `deployment_frequency`, `change_failure_rate`, `mttr`) — now
  emitted as `weekly_dora_metrics.csv` + `dora_summary.json`
- Story point extraction — `jira_integration.fetch_story_points` + `story_point_fields`
  supported; `qualitative_commits.story_points` populated
- Override management — `tga override add|list|remove` (Tier 0 manual overrides)
- AWS Bedrock LLM provider — available behind the `bedrock` cargo feature; OpenRouter is
  the default
- `--weeks N` flag — supported on `analyze` / `collect` / `classify` / `report` /
  `fetch` / `pr-metrics`
- `backfill-*` subcommands — `tga backfill ai-detection|revert-flags|ticket-ids`
- Identity merge — `tga aliases merge` and `tga identities merge`

The following gfa features remain not yet implemented in tga (run gfa alongside tga or open
an issue if you depend on them):

- ML categorization via spaCy / scikit-learn (tga uses rule-based + LLM only)
- Interactive identity consolidation wizard (tga has CLI commands, not an interactive UI)
- Data anonymization / pseudonymization pipeline at report time (tga has `--anonymize`
  identity masking; full pseudonymization pipeline is not yet ported)
- Org-wide repo discovery without an explicit repo list
- ClickUp ticket pattern detection
- Full Confluence integration (schema stubbed, fetcher not implemented)

---

## 2. Installation

### tga

```bash
# From crates.io (requires Rust toolchain)
cargo install tga

# Or build from source
git clone https://github.com/bobmatnyc/trusty-git-analytics
cd trusty-git-analytics
cargo build --release
# Binary at: target/release/tga
cp target/release/tga /usr/local/bin/tga
```

Verify:

```bash
tga --version
```

### gfa (for reference)

```bash
pip install gitflow-analytics   # requires Python 3.10+
python -m spacy download en_core_web_sm   # optional, for ML categorization
```

tga produces a single self-contained binary with no runtime dependencies. There is no virtual
environment to activate and no model to download.

---

## 3. Config Migration — Field-by-Field

tga ignores unknown config fields at load time. You can leave gfa-only keys in your config during
a phased migration — they will not cause errors.

### 3.1 Repositories — identical

No changes required.

```yaml
# gfa — no changes needed for tga
repositories:
  - name: "my-app"
    path: "/path/to/my-app"
    branch: "main"
    since_date: "2026-01-01"
    until_date: "2026-05-10"
```

✅ This section is fully compatible. Copy as-is.

### 3.2 Developer Aliases

Both formats work in tga:

```yaml
# gfa — inline map (also valid in tga)
developer_aliases:
  "John Doe":
    - "john.doe@company.com"
    - "jdoe@gmail.com"
```

```yaml
# tga preferred — external file reference
aliases_file: "/path/to/aliases.yaml"
```

External file format (compatible with both tools):

```yaml
developers:
  - name: "John Doe"
    primary_email: "john.doe@company.com"
    aliases:
      - "jdoe@gmail.com"
      - "john-doe-github"
  - name: "Jane Smith"
    primary_email: "jane.smith@company.com"
    aliases:
      - "jsmith@gmail.com"
```

If you already have an aliases file from gfa (`gfa aliases export`), it uses the same
`primary_email` / `aliases` structure. Copy the file path directly into `aliases_file:`.

For teams with more than 20 developers, prefer the external file — it keeps the main config
readable and lets you manage identities in a dedicated file under version control.

### 3.3 Identity Settings

```yaml
# gfa
analysis:
  identity:
    strip_suffixes:
      - "-duetto"
    auto_analysis: true
    similarity_threshold: 0.85
```

```yaml
# tga — these knobs do not exist
# Similarity thresholds are code constants:
#   DEFAULT_SIMILARITY_THRESHOLD    = 0.85  (raw name/email comparison)
#   NORMALIZED_SIMILARITY_THRESHOLD = 0.82  (email local-part fuzzy match)
# strip_suffixes: not implemented
# auto_analysis: not applicable
```

⚠️ **Migration note — strip_suffixes**: if your organization uses GitHub username suffixes (e.g.
`-duetto`, `-contractor`) that differ from email local-parts, add those variants explicitly to
the `aliases` list in your external aliases file. tga does not strip suffixes automatically.

Before (relying on gfa `strip_suffixes`):

```yaml
# gfa config
analysis:
  identity:
    strip_suffixes:
      - "-duetto"
```

After (explicit aliases in tga):

```yaml
# aliases.yaml
developers:
  - name: "Alice Park"
    primary_email: "alice.park@company.com"
    aliases:
      - "alice.park-duetto"      # GitHub username with suffix
      - "aparks@gmail.com"
```

### 3.4 Output — identical

No changes required.

```yaml
# gfa and tga: same format
output:
  directory: "./reports"
  formats:
    - csv
    - json
    - markdown
```

✅ This section is fully compatible. Copy as-is.

### 3.5 Classification

```yaml
# gfa
analysis:
  ml_categorization:
    enabled: true
  llm_classification:
    enabled: true
    provider: bedrock
    bedrock_model_id: "anthropic.claude-3-haiku-20240307-v1:0"
    aws_region: "us-east-1"
```

```yaml
# tga
classification:
  rules_file: "./my-rules.yaml"   # replaces ML categorization
  use_llm: true                   # OpenAI only; reads OPENAI_API_KEY env var
  # AWS Bedrock not supported — omit provider/bedrock_model_id/aws_region
```

⚠️ **Migration note — Bedrock**: tga's LLM tier is OpenAI-only. If you rely on AWS Bedrock for
classification, either stay on gfa for the classify stage or switch to `use_llm: true` with an
`OPENAI_API_KEY`. If LLM classification is not critical, the built-in rules (80+ patterns) cover
conventional commits and most common patterns without it.

⚠️ **Migration note — ML categorization**: tga has no spaCy-based ML tier. The rules cascade
(exact keyword → regex → fuzzy → LLM) replaces it. For most organizations the built-in ruleset
plus a small custom rules file covers the same ground. See [section 6](#6-classification-rules-migration).

### 3.6 Ticket Detection

```yaml
# gfa — fine-grained control
analysis:
  ticket_detection:
    commit_filter: "squash_merges_only"
    target_branches: ["develop", "main"]
    position: "start"
    patterns:
      jira: "([A-Z]{2,10}-\\d+)"
    exclude_patterns:
      - "CVE-\\d+"
    exclude_repos:
      - "APEX"
    exclude_authors:
      - "Bob Matsuoka"
```

```yaml
# tga — automatic, not configurable
# Detects on all commits, all repos, all branches:
#   JIRA / Linear:  \b[A-Z][A-Z0-9]*-\d+\b
#   GitHub:         fixes/closes/resolves #N, bare #N
# No per-repo or per-author exclusions, no position filter, no branch filter
# ticketed: bool column written for every commit row in the DB
```

⚠️ **Migration note**: tga's ticket detection is always-on and not configurable. If you need to
exclude CVE references or bot commits from compliance counts, filter at the query layer:

```sql
-- Example: ticketed commits, excluding bot authors and CVE refs
SELECT *
FROM commits
WHERE ticketed = 1
  AND author_name NOT IN ('dependabot[bot]', 'renovate[bot]')
  AND message NOT GLOB '*CVE-[0-9]*';
```

### 3.7 JIRA

```yaml
# gfa
jira:
  access_user: "user@company.com"
  access_token: "${JIRA_TOKEN}"
  base_url: "https://company.atlassian.net"
  fetch_story_points: true
  story_point_fields:
    - "customfield_10004"
```

```yaml
# tga — rename three fields, drop story point fields
jira:
  url: "https://company.atlassian.net"       # was: base_url
  username: "user@company.com"               # was: access_user
  token: "your-token-here"                   # was: access_token
  project_key: "ENG"                         # optional — filter issues by project
  # fetch_story_points: not supported
  # story_point_fields: not supported
```

⚠️ **Migration note — env var interpolation**: tga does not interpolate `${VAR}` syntax in config
values. Set the token directly or use a secrets management tool to write the config. Alternatively,
pass the token via a dedicated env var if your deployment tooling supports config templating.

⚠️ **Migration note — story points**: `fetch_story_points` and `story_point_fields` are not
implemented in tga. Story point data will not be available in tga reports.

Field rename summary:

| gfa field | tga field |
|-----------|-----------|
| `base_url` | `url` |
| `access_user` | `username` |
| `access_token` | `token` |
| `fetch_story_points` | _(not supported — remove)_ |
| `story_point_fields` | _(not supported — remove)_ |

### 3.8 GitHub

```yaml
# gfa
github:
  token: "${GITHUB_TOKEN}"
  organization: "myorg"
  fetch_pr_reviews: true
```

```yaml
# tga
github:
  token: "your-token-here"    # was: ${GITHUB_TOKEN} — no interpolation in tga
  org: "myorg"                # was: organization
  fetch_prs: true             # was: fetch_pr_reviews — simplified; reviews included
```

Field rename summary:

| gfa field | tga field |
|-----------|-----------|
| `organization` | `org` |
| `fetch_pr_reviews` | `fetch_prs` |

### 3.9 Linear — tga only

Linear is not in gfa. If you use Linear, add this section to your tga config:

```yaml
# tga only — not in gfa
linear:
  api_key: "your-linear-api-key"  # pragma: allowlist secret
  team_keys: ["ENG", "FE"]
  fetch_on_reference: true
```

### 3.10 Fields to Drop

The following gfa fields are not parsed by tga. They are safe to leave in (tga ignores them), or
remove them for a clean config:

| Field | Reason |
|-------|--------|
| `analysis.ml_categorization` | No ML tier in tga |
| `analysis.llm_classification.provider` | Use `classification.use_llm: true` (OpenAI only) |
| `analysis.llm_classification.bedrock_model_id` | Bedrock not supported |
| `analysis.llm_classification.aws_region` | Bedrock not supported |
| `analysis.ticket_detection.*` | tga auto-detects; no config |
| `jira.fetch_story_points` | Not supported |
| `jira.story_point_fields` | Not supported |
| `analysis.identity.strip_suffixes` | Not implemented |
| `analysis.identity.auto_analysis` | Not applicable |
| `confluence.*` | Confluence integration not in tga |
| `profile` | Parsed but ignored |
| `analysis.auto_analysis` | Not applicable |

---

## 4. CLI Command Mapping

tga requires an explicit subcommand. There is no default "run everything" invocation — use
`tga analyze` for the full pipeline.

| gfa command | tga equivalent | Notes |
|-------------|---------------|-------|
| `gfa` | `tga analyze` | tga requires explicit subcommand |
| `gfa -c config.yaml` | `tga analyze --config config.yaml` | Flag name identical |
| `gfa collect -w 16` | `tga collect --since 2025-01-14 --until 2026-05-10` | No `--weeks` flag; use ISO dates |
| `gfa classify` | `tga classify` | tga adds `--rules <file>` and `--use-llm` flags |
| `gfa report` | `tga report` | Same flags |
| `gfa fetch` | _(built into `tga collect`)_ | GitHub fetch runs automatically during collect |
| `gfa aliases` | _(not implemented)_ | Manage `aliases_file` directly |
| `gfa identities` | _(not implemented)_ | Edit `aliases_file` or `developer_aliases` in config |
| `gfa pr-metrics` | _(not implemented)_ | DORA metrics not in tga |
| `gfa override set` | _(not implemented)_ | No override management |
| `gfa override list` | _(not implemented)_ | No override management |
| `gfa backfill-ticket-ids` | Re-run `tga collect` | tga detects tickets at extraction time; re-run is idempotent |
| `gfa -f` / `gfa --fresh` | `tga analyze --force` | Both re-collect; tga tracks per-week state in DB |

### Date range — replacing `--weeks`

gfa's `--weeks N` flag computes a rolling window relative to today. tga uses explicit ISO dates:

```bash
# gfa: collect the last 16 weeks
gfa collect -w 16

# tga equivalent: compute the date manually
tga collect \
  --config config.yaml \
  --database tga.db \
  --since 2026-01-12 \
  --until 2026-05-10
```

For automation scripts, compute the since date in your shell:

```bash
SINCE=$(date -v-16w +%Y-%m-%d)   # macOS
# SINCE=$(date -d "16 weeks ago" +%Y-%m-%d)  # Linux

tga collect --config config.yaml --database tga.db --since "$SINCE"
```

---

## 5. Database Migration

tga uses a different SQLite schema from gfa. You cannot import a gfa database into tga — start
with a new database file.

To backfill historical data, run `tga collect` with `--since` set to your desired history start
date. The per-week backfill tracking means you can safely re-run the same command — already
collected (repo, year, week) tuples are skipped automatically.

```bash
# Collect the last year — safe to run multiple times
tga collect \
  --config config.yaml \
  --database tga.db \
  --since 2025-05-01 \
  --until 2026-05-10
```

### Schema differences

**Tables in tga that have no gfa equivalent:**

| Table | Purpose |
|-------|---------|
| `collection_runs` | Tracks `(repo, year, week)` backfill state — enables safe re-runs |
| `linear_issues` | Linear ticket data fetched on reference |
| `commits.ticketed` | Boolean column — automatic ticket detection on every commit |

**Tables in gfa that are not in tga:**

| Table | gfa purpose |
|-------|------------|
| `story_points` | JIRA story point values |
| `dora_metrics` | Lead time, deployment frequency, change failure rate, MTTR |
| `pr_reviews` | Pull request review data |
| `ticketing_scores` | Per-commit compliance scoring |
| `overrides` | Manual category overrides |

If your reporting queries reference any of these tables, they will need to be rewritten or deferred
until those features land in tga.

---

## 6. Classification Rules Migration

If you have custom gfa rules, the format is largely compatible. The key semantic change is in how
`category` is interpreted.

### Format comparison

```yaml
# gfa custom rules
rules:
  - id: "my-rule"
    category: "feature"
    keywords: ["PROJ-", "feat"]
    patterns: ["^PROJ-\\d+"]
    priority: 80
```

```yaml
# tga equivalent — same structure, new fields available
rules:
  - id: "my-rule"
    category: "feature"       # now = subcategory name; maps to TopLevelCategory::Feature
    subcategory: "payments"   # optional leaf label for granular reporting
    keywords: ["PROJ-", "feat"]
    patterns: ["^PROJ-\\d+"]
    priority: 80
    confidence: 0.9           # optional; default 0.85
```

### Taxonomy change

In tga, `category` is the **subcategory name** that maps through `TaxonomyRegistry` to one of
seven canonical top-level categories. The field name is preserved for database compatibility, but
its role has changed.

The seven top-level categories and example subcategories:

| TopLevelCategory | Example subcategory values |
|-----------------|---------------------------|
| `Feature` | `feature`, `enhancement`, `new-feature` |
| `Bugfix` | `bugfix`, `hotfix`, `fix` |
| `KTLO` | `ktlo`, `keep-the-lights-on`, `maintenance` |
| `Integrations` | `integration`, `api-integration`, `third-party` |
| `PlatformWork` | `platform`, `infrastructure`, `devops`, `ci` |
| `Content` | `content`, `copy`, `localization` |
| `Maintenance` | `refactor`, `cleanup`, `chore`, `deps` |

Any subcategory value you define in your rules file maps to the nearest top-level via the registry.
If you use a value that does not match any built-in mapping, it defaults to `Maintenance`.

### Extending built-in rules

To add rules on top of the 80+ built-in patterns (rather than replacing them):

```yaml
# tga rules file — extend_defaults keeps all built-in rules active
version: "1.0"
extend_defaults: true
rules:
  - id: "my-org-proj-rule"
    category: "feature"
    subcategory: "payments"
    keywords: ["MYPROJ-", "PAYMENT-"]
    patterns: ["^MYPROJ-\\d+"]
    priority: 95            # higher than built-in defaults (1–90 range)
    confidence: 0.95
```

To replace built-in rules entirely, set `extend_defaults: false` (or omit it — the default is
`true`; check your tga version).

---

## 7. Step-by-Step Migration Checklist

```
[ ] 1. Install tga binary (cargo install tga or copy release binary)
[ ] 2. Copy gfa config to tga-config.yaml; rename fields per section 3:
       - jira: base_url→url, access_user→username, access_token→token
       - github: organization→org, fetch_pr_reviews→fetch_prs
[ ] 3. Remove or leave gfa-only fields (tga ignores unknowns — no errors)
[ ] 4. If using strip_suffixes: add those username variants explicitly to aliases file
[ ] 5. Extract developer_aliases to external aliases.yaml if team has >20 developers
[ ] 6. Run dry-run collect on a short date range (last 90 days):
       tga collect --config tga-config.yaml --database tga-test.db \
         --since 2026-02-10 --until 2026-05-10
[ ] 7. Verify author count in output matches expected canonical developer count
[ ] 8. Run classify and report stages:
       tga classify --config tga-config.yaml --database tga-test.db
       tga report --config tga-config.yaml --database tga-test.db
[ ] 9. Spot-check classifications against gfa output for the same period
[ ] 10. Tune rules file for org-specific patterns not covered by defaults
[ ] 11. If using OpenAI LLM classification: set OPENAI_API_KEY and add
        classification: { use_llm: true } to config
[ ] 12. Run full historical backfill with desired --since date
[ ] 13. Point output.directory to existing gfa reports path for continuity
[ ] 14. Update automation scripts using gfa CLI (see command mapping table in section 4)
[ ] 15. Note features requiring gfa fallback: story points, DORA, Bedrock LLM, overrides
```

---

## 8. Running Both in Parallel

If you depend on gfa features not yet in tga (story points, DORA metrics, overrides, Bedrock),
you can run both tools against the same repositories simultaneously.

- Both tools read from the same git repos on disk (read-only access)
- They use separate database files — no conflict
- Config files are largely compatible — tga ignores gfa-only fields, so a single config file
  works for both tools after the field renames in section 3
- Reports from each tool can be written to separate subdirectories

Suggested split:

| Use tga for | Keep using gfa for |
|-------------|-------------------|
| Fast incremental collection | Story point extraction |
| Two-level taxonomy reporting | DORA metrics |
| Linear ticket integration | Override management |
| `ticketed: bool` compliance | Bedrock LLM classification |
| CSV/JSON/Markdown output | Confluence publishing |

```bash
# Run tga for taxonomy and ticket compliance
tga analyze --config shared-config.yaml --database tga.db

# Run gfa for story points and DORA
gfa --config gfa-config.yaml --database gfa.db
```

The two database files are independent. Merge reporting at the query layer if needed, joining
on `commit_hash` which both tools store.

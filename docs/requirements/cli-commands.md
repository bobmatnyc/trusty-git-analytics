# CLI Commands

The binary is `tga` (built from the `tga-cli` crate). A `gitflow-analytics` symlink or alias
is recommended for compatibility with scripts targeting the Python predecessor.

```
tga <SUBCOMMAND> [FLAGS]
```

Global flags:

| Flag | Type | Description |
|------|------|-------------|
| `--config <PATH>` / `-c` | path | Path to config YAML (default: `./config.yaml`) |
| `--database <PATH>` / `-d` | path | Path to SQLite database (default: `./tga.db`) |
| `--log <LEVEL>` | enum | `error` / `warn` / `info` / `debug` / `trace` (default: `warn`). Overrides `-v`. |
| `-v` / `-vv` / `-vvv` | count | Verbosity shortcut: `info` / `debug` / `trace` |
| `--help` | | Print help |
| `--version` | | Print version |

The `RUST_LOG` environment variable, when set, takes precedence over both
`--log` and `-v` (supports the standard `tracing-subscriber` `EnvFilter`
syntax, e.g. `RUST_LOG=tga::collect=debug,warn`).

## ISO Week Targeting

Three mutually-exclusive selectors for time range:

| Flag | Repeatable | Description |
|------|------------|-------------|
| `--weeks <N>` | no | Look back N ISO weeks from today |
| `--week <YYYY-Www>` | yes | Target one or more specific ISO weeks (e.g. `--week 2025-W12 --week 2025-W14`) |
| `--from <DATE> --to <DATE>` | no | Date range, both required if either used |

Validation: `--week` is mutually exclusive with both `--weeks` and `--from`/`--to`.
`--from`/`--to` must be used together.

---

## Subcommands

### `tga analyze`

Run the full pipeline: collect → classify → report.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | Config file |
| `--weeks <N>` | 4 | Lookback weeks |
| `--week <YYYY-Www>` | — | Specific ISO week (repeatable) |
| `--from <DATE>` | — | Range start (YYYY-MM-DD) |
| `--to <DATE>` | — | Range end (YYYY-MM-DD) |
| `--output <PATH>` | from config | Output directory |
| `--force` | false | Bypass week-level cache immutability |
| `--anonymize` | from config | Override anonymization |
| `--generate-csv` | true | Emit CSV reports |
| `--reclassify` | false | Re-run classification on cached commits |

### `tga collect`

Stage 1 only — extract git data and external APIs into SQLite cache.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | Specific week (repeatable) |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--force` | false | Override `weekly_fetch_status` immutability |
| `--log <LEVEL>` | warn | (global) |

### `tga classify`

Stage 2 only — run classification cascade against cached commits.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--reclassify` | false | Re-classify previously-classified commits |
| `--log <LEVEL>` | warn | (global) |
| `--show-jira-signals` | false | Emit JIRA signal diagnostics per commit |
| `--validate-coverage` | false | Exit non-zero if coverage below threshold |
| `--coverage-threshold <PCT>` | 20.0 | Minimum classification coverage % |
| `--rules <PATH>` | — | Rules file for this run, loaded before any `classification.rules_file` (a later file wins on rule ids and on `buckets:`); its `buckets:` map applies when the config has no `classification.buckets` (#111) |

The "By bucket" breakdown names the bucket map's source: `classification.buckets`,
the rules file, or tga's built-in fallback (#111).

### `tga rules list`

| Flag | Default | Description |
|------|---------|-------------|
| `--rules <PATH>` | — | Rules file in place of `classification.rules_file` |
| `--format <text\|json>` | `text` | `json` prints `subcategory_to_top_level`, `top_level_categories` and `bucket_map` |

`bucket_map` is the checked bucket map in effect and its source (#111):
`{"source": "config" | "rules_file" | "fallback", "buckets": {"<bucket>": ["<category>", …]}}`,
buckets in map order.

### `tga eval score` / `tga eval repredict`

Both take `--rules <PATH>`: the rules file for this run, in place of
`classification.rules_file`, as `tga rules list --rules` (#111). On `score` its
categories are valid labels and its `buckets:` map applies when the config has
no `classification.buckets`. See `docs/eval-harness.md` for every other flag.

### `tga report`

Stage 3 only — generate reports from cache.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--week <YYYY-Www>` | — | |
| `--from <DATE>` | — | |
| `--to <DATE>` | — | |
| `--output <PATH>` | from config | Output directory |
| `--generate-csv` | true | |
| `--anonymize` | from config | |
| `--log <LEVEL>` | warn | (global) |

### `tga fetch`

Fetch external data only (GitHub PRs/issues, JIRA tickets) — no git extraction.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--source <SOURCE>` | all | `github` / `jira` / `confluence` / `all` |
| `--weeks <N>` | 4 | |

### `tga aliases`

LLM-based identity alias suggestion / generation.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--apply` | false | Apply suggestions to identities.db |
| `--dry-run` | true | Print suggestions only |

### `tga identities`

Identity management subcommands.

#### `tga identities list`

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--include-aliases` | Show all aliases per canonical |

#### `tga identities merge`

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--source <ID>` | Canonical ID to merge from |
| `--target <ID>` | Canonical ID to merge into (kept) |

### `tga pr-metrics`

Weekly PR metrics aggregation into `weekly_pr_metrics`.

| Flag | Default | Description |
|------|---------|-------------|
| `--config <PATH>` | `./config.yaml` | |
| `--weeks <N>` | 4 | |
| `--rebuild` | false | Drop existing rows for range before inserting |

### `tga linear sync`

Bulk-sync Linear issues into `linear_issues` and `work_items` (#7139, #190).
One team per run (`--team`, or the single `linear.team_keys` entry), or every
team the API key can see (`--all-teams`). Each team keeps its own
`updatedAt` cursor in `linear_sync_cursor`; the first sync of a team is a full
pull.

| Flag | Default | Description |
|------|---------|-------------|
| `--team <KEY>` | from `linear.team_keys` | Sync one team. Conflicts with `--all-teams` |
| `--all-teams` | false | List every team through the API and sync each in turn. Archived issues are always included |
| `--exclude-archived` | false | Leave archived issues out (single-team mode only) |
| `--since <YYYY-MM-DD>` | stored cursor | Only issues updated on or after this date |
| `--backfill` | false | Ignore the stored cursor; with no `--since`, sync the whole history |
| `--max-issues <N>` | no cap | Fail when a team holds more than N issues in the window. The error names the team and the cap, and nothing is written for that team |
| `--dry-run` | false | Open the database read-only, fetch, and print what would be written. No row and no cursor is written |
| `--entities` | false | After the issues, also sync the workspace's teams, users, labels, projects, project milestones and cycles (see below) |

Rows are keyed by Linear's issue `id` (UUID); `identifier` (ENG-123) is a
separate indexed column. An issue that moves team gets a new identifier and
keeps its one row. Each row stores the state name and type, priority,
estimate, project, cycle and parent ids, label ids and names, due date,
assignee id/name/email, creator id, the created/updated/started/completed/
canceled/archived timestamps, an `archived` flag, url, team id and key, and
`raw_json` — the GraphQL node as Linear returned it, including the
description and `previousIdentifiers`. An issue whose stored node is
identical is not rewritten.

A row written before Linear's id was stored, under an identifier the issue
has since left, is found through the issue's `previousIdentifiers` and
updated in place (or deleted, when the issue already has a row). The
`work_items` row follows the same rule: it is keyed by the issue id in
`work_items.stable_id`, renamed when the issue moves, and its commit links
and `fact_pm_work` / `fact_pm_effort` rows move with it. `work_items.tags`
holds the label names, `work_items.project` the Linear project id (the name
is in `linear_projects`), and `item_type` is `Issue`.

Each team's cursor records the issue field set it was last read in full
under. When a new tga adds fields to the issue query or changes how an issue
is projected into `work_items`, the next sync of each team ignores the cursor
once, reads the whole history and rewrites every issue's `work_items` row, so
issues that did not change also get the new fields; later runs resume from
the cursor. The full read is the same paged walk as any sync: the same
`--max-issues` cap, and the same retry with backoff on HTTP 429/503 and on
Linear's rate-limit error (HTTP 400 with `RATELIMITED`), which waits for the
exhausted window's `X-RateLimit-*-Reset` time when Linear sends one. A rate
limit that outlasts the retry budget fails the team and writes nothing. A
`--since` or `--exclude-archived` run does not count as that full read.

Output is one line per team, for example:

```text
Linear sync (ENG): 3 issue(s) fetched, 1 archived; wrote 2 (1 new, 1 changed, 0 moved, 1 unchanged).
```

The first team that fails stops the run with a non-zero exit naming that
team; teams synced earlier in the run keep their rows and cursors.

With `--entities`, the sync then walks six workspace-wide entity sets, each
with `includeArchived: true` (users also with `includeDisabled: true`), and
stores them in migration v34's tables, keyed by Linear's `id` with the
GraphQL node in `raw_json`:

| Set | Table | Columns beyond the timestamps and `archived` |
|-----|-------|------|
| teams | `linear_teams` | key, name, estimation type/allow-zero/extended/default, cycle settings |
| users | `linear_users` | name, display name, email, active, admin, guest |
| labels | `linear_labels` | name, color, group flag, parent id, team id (NULL = workspace-level) |
| projects | `linear_projects` | state, status, progress, start/target date, started/completed/canceled at, lead id, team ids |
| milestones | `linear_milestones` | project id, name, target date, sort order |
| cycles | `linear_cycles` | team id, number, starts/ends/completed at, scope and completed counts, `is_empty` |

Each set is a full refresh: every page is fetched first, then the set's rows
and its `linear_entity_sync_state` row commit in one transaction. A failed
page, or a page whose `pageInfo` does not say whether more pages follow,
writes nothing for that set and leaves its state row as it was; the run
exits non-zero naming the set, and sets synced earlier keep their rows.

A stored row that the complete fetch no longer returns (Linear purged or
deleted it, or the key lost access to its team) gets `removed_at` set in the
same transaction. The row is kept so older issues still resolve its name;
count current entities with `WHERE removed_at IS NULL`. A row that comes
back has `removed_at` cleared. A cycle whose latest issue count is 0 has
`is_empty = 1`; a cycle with no history yet has `is_empty` NULL. The pass
prints one line, with the rows newly marked removed per set:

```text
Linear entities: teams 3 (1 archived, 0 removed), users 3 (1 archived, 0 removed), labels 4 (1 archived, 0 removed), projects 2 (1 archived, 1 removed), milestones 2 (1 archived, 0 removed), cycles 4 (1 archived, 0 removed); wrote 6 set(s).
```

A `--dry-run` line omits the removed counts: it reads and writes nothing.

`tga linear freshness` reads the same cursor table and fails when a team has
never synced or is older than `--max-age-days` (default 2).

### `tga override`

Manage manual classification overrides (Tier 0).

| Flag | Description |
|------|-------------|
| `--config <PATH>` | |
| `--commit <HASH>` | Commit hash |
| `--repo <PATH>` | Repository path |
| `--change-type <TYPE>` | One of 19 change_type values |
| `--work-type <TYPE>` | Optional override |
| `--reason <STRING>` | Required justification |
| `--remove` | Remove existing override |

### `tga install`

Setup wizard. Creates `config.yaml`. On a terminal with no flags it prompts;
given `--host` / `--pm`, or with stdin not a terminal, it runs from flags and
environment variables and never prompts ([#5216](https://github.com/bobmatnyc/trusty-tools/issues/5216)).
With `--host github --org <ORG>` it pages the GitHub API for the org's
repositories and writes them into the generated config.

| Flag | Description |
|------|-------------|
| `--output <PATH>` | Output config path (default `./config.yaml`) |
| `--force` | Overwrite existing config |
| `--non-interactive` | Never prompt; implied when stdin is not a terminal |
| `--host <local\|github\|bitbucket>` | Where repositories come from (required non-interactively) |
| `--org <ORG>` | GitHub org to discover repositories from |
| `--workspace <WORKSPACE>` | Bitbucket Cloud workspace |
| `--repo <OWNER/NAME>` | Explicit remote repository; repeatable |
| `--repo-path <PATH>` | Already-cloned repository; repeatable |
| `--repo-cache <DIR>` | Where remote repositories are expected on disk (default `./repos`) |
| `--host-token <TOKEN>` | Host API token; falls back to `$GITHUB_TOKEN` / `$BITBUCKET_TOKEN` |
| `--pm <none\|github\|jira\|linear>` | PM system supplying work items (required non-interactively) |
| `--jira-url`, `--jira-user`, `--jira-token` | JIRA credentials; fall back to `$JIRA_URL`, `$JIRA_EMAIL`, `$JIRA_API_TOKEN` |
| `--linear-api-key <KEY>` | Linear API key; falls back to `$LINEAR_API_KEY` |
| `--linear-team <TEAM>` | Linear team key to scope issue fetches to; repeatable |
| `--output-dir <DIR>` | Report output directory (default `./tga-output`) |
| `--llm-provider <PROVIDER>` | `none`, `openai` or `openrouter` (default `none`) |
| `--llm-api-key <KEY>` | LLM API key; falls back to the provider's conventional env var |

**Precedence:** a flag value wins over its environment variable. A credential
taken from the environment is written to the config as a `${VAR}` reference,
not as the secret itself.

Run non-interactively with a required flag missing, install names every missing
flag at once rather than blocking on a prompt:

```bash
$ tga install --host github --pm none < /dev/null
Error: `tga install` has no terminal to prompt on and these required flags are missing:
  --org <ORG> (or --repo <OWNER/NAME>, repeatable)
  --host-token <TOKEN> (or set $GITHUB_TOKEN)
```

Bitbucket Cloud workspace-to-repo discovery is not implemented yet
([#5220](https://github.com/bobmatnyc/trusty-tools/issues/5220)), so
`--host bitbucket` requires an explicit `--repo <workspace/slug>` list and
records that limitation as a comment in the generated config.

### `tga help`

Extended help with topic deep-dives.

```
tga help config           # YAML schema cheatsheet
tga help classification   # Cascade explanation
tga help iso-weeks        # ISO week targeting examples
```

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

Linear bulk sync (#190): with `linear.sync_on_collect: true`, `collect` (and
`analyze`) also runs the work of `tga linear sync --entities`: every
`linear.team_keys` entry, or all teams when the list is empty, then the
reference entities. Off (the default), `collect` makes no bulk Linear request.
A Linear HTTP 404 is a counted warning (exit 0). A 401, 403, 5xx after
retries, or any other sync failure is a stage failure: `collect` prints it as
an `error:` line and exits non-zero after its remaining stages. See
[collection.md](collection.md#linear-bulk-sync-in-collect-190).

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

Linear delivery section (#190): with `linear.stats.report: true` and synced
Linear issues in the database, `report.md` ends with the block
`tga linear stats` prints, measured as of the report's `Generated` time. The
section is left out when Markdown is not among the formats and when the
database holds no Linear issue with a Linear id. Under `--author` it is
replaced by one line saying it is left out because it covers the whole
workspace. When the switch is off or `linear:` is absent,
`report.md` is unchanged. If the figures cannot be computed (a damaged row),
the command fails with an error starting `Linear delivery section:` and
writes no report file. It reads the Linear tables and never writes them.
`tga analyze` writes the same section. See
[reporting.md](reporting.md#linear-delivery-section).

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
| `--history` | false | After each team's issues, read the workflow-state history of each due issue into `fact_linear_transitions` (see below) |
| `--comments` | false | After each team's issues, read the team's comments updated since its comment cursor (every comment on the first run), and every comment of issues that moved into the team, into `fact_linear_comment_detail`; with `--backfill`, walk each issue's comments instead. Bodies are never stored (see below) |

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

With `--history` and/or `--comments` (#190 step 6), each team's issue pass
is followed by a pass for each flag. `--history`, and `--comments` under
`--backfill`, are per-issue passes: Linear has no workspace-wide history
list, so each issue's `history` or `comments` connection is walked in full (50 per page, `includeArchived: true`, retried on 429/503 and
RATELIMITED). There is no cap; a page that fails, a page without a `nodes`
array, or a page without a boolean `pageInfo.hasNextPage` fails the walk.

An issue is due when its stored `updated_at` differs from the `updated_at`
its activity was last read at (`linear_issue_activity_state`, one marker per
flag). `--backfill` makes every issue of the team due. Each issue's rows are
replaced as a set and its marker written in one transaction, so a comment
deleted in Linear leaves no row. A history entry becomes a
`fact_linear_transitions` row only when it changed the workflow state
(from/to state name and type, actor id/name, time). A comment becomes a
`fact_linear_comment_detail` row with author id/name, parent comment id,
created/updated time and `body_len` (characters); the body is not stored.

An issue Linear no longer has is a counted warning, not a failure. Only
Linear's explicit answer counts: `issue: null`, or GraphQL errors that all
read `Entity not found: Issue` (HTTP 200 or 400). The pass logs a warning,
tombstones the issue (`linear_issue_activity_state.missing_for` /
`missing_at`), and goes on to the next issue, the next team and
`--entities`; the run exits 0 when nothing else failed. The tombstone
deletes none of the issue's stored history or comment rows and moves
neither marker. A tombstoned issue is not requested again while its stored
`updated_at` is unchanged, by `--history` (an incremental `--comments`
run does not walk issues; see below). It is due again when its stored
row changes, and `--backfill` requests it again; a successful read clears
the tombstone, and another not-found answer renews it.

Any other failure (5xx, timeout, spent rate-limit retries, auth error,
another GraphQL error, a malformed page) stops the run with a non-zero exit
naming the issue and the connection. The team's issue rows and cursor, and
issues written before it, are kept; the failed issue and the rest stay due,
so the next run reads them even though the issue cursor has moved past
them. Each pass prints one line, with a warning appended when issues were
missing:

```text
Linear history (ENG): read 2 of 2 due issue(s); wrote 4 state transition row(s).
Linear comments (ENG): read 2 of 3 due issue(s); wrote 2 comment row(s). Warning: 1 issue(s) no longer in Linear, tombstoned and skipped until they change or --backfill: ENG-2.
```

The warning lists up to 10 identifiers, then how many more.

Under `--dry-run` the line reports how many issues would be read; the pass
sends no per-issue request.

#### Incremental `--comments` (comment cursor)

Without `--backfill`, `--comments` does not use the per-issue marker
(#190). Linear's `Issue.updatedAt` does not reliably move when a comment is
created or edited: on one live workspace, 1,212 of 60,055 comments were newer
than their issue. So the pass reads each team's comments by the comment's own
`updatedAt`, through Linear's top-level `comments` query:

- Filter: `issue.team.key = <team>` and, once the team has a cursor,
  `updatedAt > cursor − 10 minutes`. `orderBy: createdAt`,
  `includeArchived: true`, 250 comments per page, retried on 429/503 and
  RATELIMITED, no cap.
- Each comment is upserted by its Linear id, so the overlap re-reads write no
  second row and an edit updates the row. `identifier` and `team_key` are the
  issue's values from Linear; after the write, the rows of every issue now in
  the team take that issue's current `linear_issues` values, so a moved
  issue's comments follow it.
- The cursor (`linear_comment_cursor`, one row per team) becomes the newest
  comment `updatedAt` stored, never later than the moment the walk started,
  and never moves backward. It is written in the same transaction as the
  rows, after every page has arrived. A failed page, a transport error, spent
  rate-limit retries, a page without a `nodes` array or a boolean
  `pageInfo.hasNextPage`, or a comment without an id, `createdAt` or
  `updatedAt` fails the run non-zero; no comment from that walk is written
  and the cursor does not move.
- **First run** (no cursor for the team): the same query with no
  `updatedAt` bound reads every comment of the team, one request per 250
  comments, instead of one per-issue walk per issue. A walk that stores no
  comment sets no cursor, so the next run sweeps again.
- **Deleted comments**: this walk cannot see them, because a deleted comment
  is no longer in the `comments` connection. Their rows stay until a
  `--backfill --comments` run, whose per-issue walk replaces each issue's
  rows with what Linear returns.
- **Comments on an issue not in `linear_issues`** (created after the issue
  pass, or on an issue the issue pass has not stored): stored, keyed by the
  issue's Linear id, and counted in the summary line. A comment with no issue
  (Linear also has comments on project updates and documents) is not stored
  and is counted.
- **Issues moved into the team** (#190): moving an issue does not change its
  comments' `updatedAt`, so a comment written while the issue sat in another
  team, older than the cursor minus 10 minutes, matches no bounded walk.
  Every write of `linear_issues` (this issue pass and `tga collect`)
  therefore queues, in `linear_comment_due` and in the same transaction as
  the issue rows, each issue it classifies as moved into a team, and each
  issue new to a team whose `createdAt` is at or before that team's comment
  cursor minus 10 minutes. The cursor read is the issue's own team's. It queues them whether or not the
  run passes `--comments`, so a later `--comments` run, whose issue pass sees
  the issue unchanged, still reads them. A team with no comment cursor queues
  nothing: its next walk reads every comment. The next incremental comments
  pass reads every comment of the queued issues, with no `updatedAt` bound,
  through the same `comments` query filtered by `issue.id in [...]` (100 ids
  per request, 250 comments per page), and deletes the queue rows in the same
  transaction as the team's comment rows and cursor. These comments do not
  move the cursor. A failed page fails the run as above and keeps the queue.
  An issue created after that bound needs no extra read. A `--backfill`
  per-issue comments write also removes the issue from the queue.
- `--backfill --comments` keeps the per-issue walk above and leaves the
  comment cursor as it is.
- **Clocks**: the cursor is a Linear server time capped at the local clock's
  walk start. The 10-minute overlap also absorbs skew between the two clocks;
  a local clock more than 10 minutes behind Linear's can skip a comment edited
  during the previous walk until `--backfill`.

Requests: one per 250 comments updated since the cursor minus 10 minutes,
at least one per team. A team with up to 250 changed comments costs one
request per run; `--all-teams` costs at least one per team. The first run
costs one request per 250 comments of the team (about 241 for a 60,000-comment
workspace across all teams). Issues moved into the team add one request per
100 queued issues (plus one per further 250 of their comments), only on the
run after the move; a typical incremental run stays at a few requests per
team.

```text
Linear comments (ENG): read 12 comments updated after 2026-10-09T08:50:00.000Z; wrote 12 comment row(s), 1 on issue(s) not yet in linear_issues; cursor 2026-10-09T18:59:12.000Z.
```

Under `--dry-run` the line reports the bound it would read from and sends no
request.

`tga linear freshness` reads the same cursor table and fails when a team has
never synced or is older than `--max-age-days` (default 2).

### `tga linear stats`

Print the Linear delivery metrics (#190) computed by
`report::linear_stats` from the tables `tga linear sync` (with `--entities`)
already wrote. The command makes no network call and opens the database
read-only: it never creates or migrates it. A missing file fails with the
path named; a database with pending migrations fails and asks for a
migrating command (such as `tga linear sync`) first.

| Flag | Default | Description |
|------|---------|-------------|
| `--json` | false | Print the full metrics as JSON instead of the Markdown block |
| `--as-of <YYYY-MM-DD>` | now | Measure at midnight UTC at the start of this date |

The as-of instant fixes the completion windows (the four whole quarters
before the as-of quarter, and the four before those), project ages and the
overdue figures. `linear.stats` in config.yaml sets the lower bounds for
field use and concentration (see
[configuration.md](configuration.md#linear--linearconfig)).

Every figure is given for the whole workspace and per current team key,
archived issues included:

| Id | Figure |
|----|--------|
| C0 | Population: issues, archived share, state types, entity counts |
| C1, C1b, C1c | Completions per UTC quarter; prior vs recent four-quarter trend; the same over teams that complete in every quarter of both windows |
| C2 | Lead time (created to completed) and cycle time (started to completed) by completion year |
| C3 | Cycle completion rate and carry-over by year of the cycle's end |
| C5 | Issues by the number of bracketed tags their title starts with |
| C9 | Share of issues with an estimate, label, assignee, project, due date, cycle or parent |
| C11 | Projects by state, missing targets, overdue and late projects, milestones |
| C12 | Cancel rate by closure year |
| C14 | Contributors per year and the share of completed work held by the top 1, 3 and 5 assignees |

The JSON carries `schema_version` (1), `as_of` (RFC 3339), both bounds, one
key per metric (`population`, `completions`, `lead_cycle`, `cycles`,
`title_tags`, `field_use`, `projects`, `cancel_rate`, `people`) and
`tolerances`, which names where the published metric text and the
reference implementation differ. A per-group figure sits under `all` (the
workspace) and `teams.<KEY>`. A percentage is `null` when its denominator
is 0. A stored timestamp, date or label list that does not parse fails the
command with the row named.

```text
tga linear stats --json --as-of 2026-10-01
```

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

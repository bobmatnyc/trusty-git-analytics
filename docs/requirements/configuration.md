# Configuration Reference

The configuration file is YAML, matching the schema used by the Python predecessor
`gitflow-analytics`. All keys are deserialized via `serde_yaml` into typed structs in
`tga::core::config`. Paths support `~` expansion via the `shellexpand` crate.

## Top-Level Structure

```yaml
repositories: []          # list[RepositoryConfig], required
database: ~               # path  — SQLite DB override (added v2.2.2, issue #406)
llm: {}                   # LlmConfig — top-level LLM section (added v2.2.2, issue #407)
classification: {}        # ClassificationConfig — rules, LLM tier knobs, `buckets` (#111)
github: {}                # GitHubConfig
bitbucket: {}             # BitbucketConfig (Cloud only)
developer_aliases: {}     # dict[str, list[str]] — inline identity alias map
aliases_file: ~           # path — external YAML alias file
fuzzy_identity_fallback: ~ # bool — Tier-3/4 fuzzy identity fallback (issue #4251)
analysis: {}              # AnalysisConfig
audit: {}                 # AuditConfig — `tga audit` settings (issue #5482)
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

### `database` — SQLite database path (added v2.2.2, issue #406)

The path to the SQLite database file. Supports `~` home-directory expansion.

**Precedence** (highest first):

1. `--database` CLI flag — always wins.
2. `database:` field in this config file.
3. Hardcoded default `tga.db` in the current working directory.

```yaml
# Example: use a team-shared path
database: ~/data/team-analytics.db
```

This field is at the top level of `config.yaml` and is **not** inside any
nested section. Adding it here eliminates the need to pass `--database` on
every `tga` invocation.

---

### `llm` — Top-level LLM configuration (added v2.2.2, issue #407)

The `llm:` section controls how the LLM fallback tier reaches an inference
provider. **When `llm:` is present, the LLM tier is automatically enabled** —
no `classification.use_llm: true` is required (added v2.3.0), and an explicit
`classification.use_llm: false` turns it off (#175; see "Self-enabling
behavior" below). It takes
precedence over the legacy `classification.llm_provider` /
`classification.openrouter_api_key` fields; using those legacy fields without
an `llm:` section emits a `tracing::warn!` deprecation message.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `source` | enum | `openrouter` | LLM provider: `openrouter`, `bedrock`, `anthropic-api`, or `jev` |
| `api_key_env` | string | `OPENROUTER_API_KEY` (`TYPESAFE_API_KEY` for `jev`) | **Name** of the env var holding the API key (never the key itself). Ignored for `bedrock`. |
| `region` | string | None | AWS region (Bedrock only). When absent, the AWS SDK resolves the region from the environment (`AWS_DEFAULT_REGION`, profile, etc.). |
| `model` | string | provider-appropriate default | Provider-specific model id (see below). |
| `jev` | map | see below | `source: jev` settings: `budget_usd`, `sensitive_terms`, `payload_dump_dir`, `id_patterns`, `name_matcher_bytes`. Ignored by the other sources. |
| `context` | list | `[]` | Commit facts appended to the user prompt after the message: any of `paths`, `pr_title`, `issue_type` (#111). See [Commit context](#commit-context-llmcontext-111). Any other item is a load error. |
| `context_max_paths` | integer | `30` | Most changed paths `context: [paths]` lists per commit. |
| `context_max_path_bytes` | integer | `2048` | Most bytes of path text `context: [paths]` lists per commit. |
| `max_input_tokens` | integer | `190000` for `bedrock` and `anthropic-api`, else `100000` | Most estimated input tokens one LLM prompt may carry, system prompt included; minimum `8192` (#178). See [Input budget](#input-budget-llmmax_input_tokens-178). Ignored by `jev`. |

Any other key under `llm:` is a load error (#111), as it is under `llm.jev:`.
A `jev` option written one level too high (`llm.payload_dump_dir`) therefore
stops the run instead of being ignored.

#### Source variants

`llm.source: bedrock | openrouter | anthropic-api | jev`. All four are in the
default build (`cargo install tga`, the release binaries); the configuration
alone selects one. Credentials each source needs:

| Source | Credentials |
|--------|-------------|
| `bedrock` | AWS credentials from the default chain (env vars, `~/.aws` profile, SSO, instance or task role) with Bedrock model access in the region. No API key. |
| `openrouter` | OpenRouter API key in the variable named by `api_key_env` (default `OPENROUTER_API_KEY`). |
| `anthropic-api` | Anthropic API key in the variable named by `api_key_env`. |
| `jev` | TypeSafe API key in the variable named by `api_key_env` (default `TYPESAFE_API_KEY`). |

A binary built with `--no-default-features` leaves out the AWS SDK. In that
build `source: bedrock` stops `tga classify` with "bedrock feature not
compiled in"; rebuild with the default features. `--features bedrock` is
accepted and changes nothing.

**`openrouter`** (default)

Uses the OpenRouter API (OpenAI-compatible schema). The API key is read from
the environment variable named by `api_key_env` (default:
`OPENROUTER_API_KEY`). The variable is never stored in the config. If the
variable is unset or empty when LLM is enabled, `tga classify` exits
non-zero with an actionable error before writing any DB rows.

```yaml
llm:
  source: openrouter
  api_key_env: OPENROUTER_API_KEY   # or any custom var name
  model: gpt-4o-mini
```

**`bedrock`**

Uses AWS Bedrock with IAM credential-chain auth — no secret is stored in
the config file. Requires:

- Nothing at build time: the default build includes Bedrock. Only a
  `--no-default-features` build rejects this source (see above).
- Valid AWS credentials in the default chain: `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` env vars, `~/.aws/credentials` profile, EC2
  instance metadata, ECS task role, or AWS SSO.
- `api_key_env` is ignored for this source.
- `model` is a cross-region inference-profile id (`us.anthropic.…`). Current
  Claude models are not invocable on demand by their bare `anthropic.…` id.
  To use Claude Sonnet 5, set `model` to its `us.anthropic.` inference-profile
  id as listed in the Bedrock console for your account and region.
- The default `model` is the `us.` Haiku 4.5 profile, which works only when
  `region` is a US source region. Outside the US, set `model` to the `eu.`,
  `apac.` or `global.` Haiku 4.5 inference-profile id, as listed in the
  Bedrock console for your account and region.
- Bedrock requests never carry `temperature`, for any model (#111). Claude
  Sonnet 5 rejects a request that sets it; other models run at the
  provider's default.

```yaml
llm:
  source: bedrock
  region: us-east-1                  # optional; falls back to AWS SDK defaults
  model: us.anthropic.claude-haiku-4-5-20251001-v1:0   # the default when omitted
```

**`anthropic-api`**

Calls the Anthropic Messages API directly (`POST https://api.anthropic.com/v1/messages`).
No OpenRouter account or AWS credentials required — only a direct Anthropic API key.

- The API key is read from the environment variable named by `api_key_env`. If the
  variable is unset or empty, `tga classify` exits non-zero with an actionable error
  naming the missing variable before writing any DB rows.
- The request sends `x-api-key: <key>` and `anthropic-version: 2023-06-01` headers.
- The same `SYSTEM_PROMPT` and `LlmVerdict` JSON shape is used as the other providers
  (`category`, `subcategory`, `confidence`, `complexity`).
- When `model` is absent (or the caller supplies the OpenRouter default `gpt-4o-mini`),
  the default model is `claude-3-5-haiku-latest` — a cost-efficient model well-suited
  to short classification tasks.

```yaml
llm:
  source: anthropic-api
  api_key_env: ANTHROPIC_ANALYTICS_API_KEY   # export ANTHROPIC_ANALYTICS_API_KEY=sk-ant-...
  model: claude-3-5-haiku-latest             # optional; defaults to claude-3-5-haiku-latest
```

This source is available in the default `cargo install tga` build — no feature flags required.

**`jev`** (added v10.1.0, #111)

Calls TypeSafe's Jev decision model (`POST https://api.typesafe.ai/v1/systemone`)
with bearer auth. Each commit becomes one `choice` question: the criteria are
the configured categories (with their `description` when the rules file gives
one) plus the reserved abstain codes `NO_MATCH` and `INSUFFICIENT_INFORMATION`.
When the rules extend the built-ins, the criteria are every category the loaded
rules can emit. A category may not be named `NO_MATCH` or
`INSUFFICIENT_INFORMATION`.

- **Key:** read from the variable named by `api_key_env`. Left at its default,
  that is `TYPESAFE_API_KEY`. Unset or empty → `tga classify` exits non-zero
  before writing any DB rows (unless `payload_dump_dir` is set).
- **Verdicts:** an abstain code → `abstained`; a code outside the configured
  set → `out_of_set`; a configured code → the verdict, with confidence equal
  to that code's probability. A transport error, an HTTP error after retries
  (429 and 5xx are retried twice with backoff), or a reply that fails
  validation → `failed`. None of these replace the rule verdict. Each call's
  usage is written to `llm_usage` with provider `jev`. Merge commits are never
  sent, as for every source.
- **Commit text (`jev.obfuscate`, default `false`):** by default Jev
  receives each commit message exactly as stored in the database, names,
  addresses, ticket keys and paths included (owner ruling 2026-10-06,
  #111). The pseudonymizer, the name, trailer and repository learning, the
  database scans behind it and the name-matcher build are all skipped, so
  `jev.sensitive_terms`, `jev.id_patterns` and `jev.name_matcher_bytes`
  have no effect. Set `jev.obfuscate: true` to send pseudonymized text
  instead; every rule below marked "obfuscation only" then applies. The run
  log names the mode (`Jev: sending real commit text` or `Jev: sending
  obfuscated text`), and each `llm_usage` row records it in `text_mode`
  (`real` or `obfuscated`; `NULL` for other sources).
- **Pseudonymization (obfuscation only):** before a message is sent, tga replaces e-mail
  addresses (`EMAIL_n`), people named in git trailers (`Co-authored-by`,
  `Signed-off-by`, `Reviewed-by`, `Acked-by`, `Tested-by`, `Reported-by`,
  `Helped-by`, `cc`), `@` mentions and roster names from `team:` and
  `developer_aliases` (`PERSON_n`), ticket keys such as `ABC-123`
  (`TICKET_n`), URLs (`URL_n`), host names, domains and IPv4 addresses
  (`HOST_n`), file paths and source file names (`PATH_n`), repository, org
  and workspace names (`REPO_n`, see below), and every
  `jev.sensitive_terms` entry (`TERM_n`). Numbers are assigned in first-seen
  order and are stable for the run; other text is sent unchanged. The
  pseudonym → original map stays in memory and is never sent or logged.
  Every person in the database is a known person, not only the run's
  authors: `commits` author names and address local-parts, `authors`
  canonical names, addresses and aliases, `pull_requests.author`,
  `pr_reviewers.reviewer_id` and `display_name`, `linear_issues.assignee`
  Jira changelog and comment authors (`fact_ticket_transitions.author`,
  `fact_jira_comment_detail.author`), reporters (`fact_pm_effort.pm_name`)
  and the `author_email` local-parts of the weekly fact tables, and every
  name in an identity trailer of every commit message the database stores,
  classified or not (#111). `--since`, `--repos` and `--shas` narrow the
  commits sent, never the names learned. A known
  name is matched whole; a multi-word display name is also matched by its
  parts, including hyphen parts and either apostrophe (`O'Brien`,
  `O’Brien`). A single-token login is matched whole only, and bot accounts
  (`x[bot]`, `x-bot`) are not people. Classification vocabulary is never a
  name: the category names and descriptions, the keywords and pattern
  words of the loaded rules, and a built-in list of generic and technical
  words (`test`, `dev`, `admin`, `ci`, `build`, `deploy`, `role`, …). A
  single-word identity made of such words (`test`, `deploy-bot`) is not
  learned, and a name part that is one is never matched alone; the person
  is still hidden by their full name and address. A part that is a common
  given name (`Frank`, `Will`) is matched only in Titlecase and not at the
  start of a sentence or list item, so `frank` and `Mark as done` stay.
  Values of
  `-with`, `-to` and role trailers (`Tested with:`, `Owner:`) are replaced
  on their own line, but teach the run a name only when they look like one
  (`Jane Roe`, `jroe-acme`); `Tested with: chrome and firefox` teaches
  nothing.
- **Repository, org and workspace names** (`REPO_n`, #111; obfuscation
  only): from the
  config, `repositories[].name`, `.org` and the path basename,
  `github.org`, `github.orgs` and `github.repo`, `bitbucket.workspace`,
  `bitbucket.workspaces` and `bitbucket.repo_slug`, the Azure DevOps
  organisation (from `pm.azure_devops.organization_url`) and its `project`
  and `projects`, the Jira site name (`acme` in
  `jira.url: https://acme.atlassian.net`), and every
  `classification.repo_categories` key that is not a glob (for a
  `<repo>:<prefix>` key, only the repository before the `:`, #167); from the
  database, every distinct `repository` in `commits` and `pull_requests`.
  An `owner/name` slug also gives each part. Names are matched whole, in
  any case, hyphens included (`port acme-fin invoicing-api client` →
  `port REPO_1 REPO_2 client`). A database name that is the column default
  `unknown` or classification vocabulary (`platform`, `docs`) is not
  learned. Database names share the name matcher, its size cap and its
  fail-closed rebuild with people.
- **Stored trailer scan cost (obfuscation only):** the trailer names of stored commits are
  read in one pass over `commits.message`, skipping messages with no `:`
  in SQL. Measured on a synthetic 300,000-commit database (a quarter of
  the messages one-line, a quarter multi-line with no trailer, half with
  one or two trailers; 2,250 distinct names): the scan takes 0.25 s in a
  release build (3.5 s in a debug build), and learning the names and
  rebuilding the matcher 15 ms more. It runs once per Jev run, before the
  first request.
- **Categories:** option keys are pseudonym codes (`CAT_1`…). Each code's
  criterion text is the category's description, or — when it has none — the
  category name itself, sent verbatim: it is operator configuration, not
  commit text, and is never tokenised. Do not put customer or people names
  in category descriptions.
- **Paths (obfuscation only):** every slash-joined token is a path (`PATH_n`) — two-segment
  directories (`billing/invoices`), branch names (`feature/foo-bar`,
  `release/2026-09`), `.github/workflows/…` and deeper paths, all-caps
  pairs (`CI/CD`, `I/O`) included — unless it is prose: all segments digits
  (`1/2`, `2026/09/25`), two segments where one is a single lowercase
  character (`w/o`, `n/a`), or one of the fixed pairs `and/or`,
  `read/write`, `client/server`, `input/output`, `true/false`, `yes/no`,
  `on/off`, `pass/fail` (either order, any case).
- **File names (obfuscation only):** a bare `name.ext` with a known source, config, data or doc
  extension becomes `PATH_n.<ext>` (`invoice_sync.py`, `PriceTable.tsx`,
  `README.md`, `Cargo.toml`); build files (`Makefile`, `Dockerfile`,
  `Dockerfile.prod`, `CODEOWNERS`) become `PATH_n`. The run also learns
  every basename in `files.path` (the files each collected commit touched)
  and, when identifier-like (`invoice_sync`, `PriceTable`), its stem, so a
  file with no or an unusual extension is caught too; an extension-less
  basename that is classification vocabulary (`build`) is skipped.
- **Ticket keys (obfuscation only):** uppercase keys always; lowercase keys with any number of
  digits (`abc-1`, `[abc-1]`, `abc-1_fix`, `build_abc-7`) unless the prefix
  is a version or ordinal word (`python-3`, `step-2`) or classification
  vocabulary (`fix-1`).
- **Spend cap:** `jev.budget_usd` (default `0.25`) caps one run's running
  total. Input costs $0.042 per million tokens; output tokens cost $0 and
  never count against the cap. Before each call tga reserves the request's
  byte length plus 1024 input tokens, times the three attempts. After the
  call it charges the reported input plus that per-attempt bound for every
  earlier attempt sent (a timed-out or failed attempt may still be billed),
  so retries never push the run past the cap. Once a reservation would pass
  the cap, that call and every later call in the run are recorded as
  `skipped` and nothing more is sent.
- **Model:** tga pins `jev-1.13.0`. Any other `llm.model` is a
  configuration error, and a reply that names another model is recorded as
  `failed` with the served model in `llm_usage.model`. Each commit is one
  `category` question; the second "mixed" question is not sent.
- **Fail closed (obfuscation only):** a message the pseudonymizer cannot process is recorded as
  `failed` and not sent; after a failed name-matcher rebuild every later
  message in the run fails the same way.
- **More redaction (obfuscation only):** trailers in any case and spacing whose token ends in
  `by`, `with` or `to` (`Approved by:`, `Paired-with:`, `Thanks-to:`) or is
  `Reviewer`, `Author`, `Owner`, `Assignee` or `Approver` (singular or
  plural), with or without an address; Phabricator `Reviewers:`,
  `Reviewed By:`, `Subscribers:` and `Auditors:` username lists, including
  a list wrapped onto an indented next line; trailers behind list or quote
  markers; bracketed and any-case ticket keys (`[abc-123]`,
  `PROJ-12_fix`, `feature/proj-12`); record ids of 1–4 letters and 4+
  digits (`H1234` → `ID_n`); commit and content hashes and UUIDs (`ID_n`,
  gate B 2: a hex run of 7 to 31 characters holding both a digit and a
  letter, any hex run of 32 or more such as a 64-character hash, and a
  UUID; inside a version string, after `-g`, `@` or `+`, never inside a
  longer word; words spelled in hex letters such as `decade` or `facade`,
  and pure-digit numbers under 32 digits such as `20261006`, are kept);
  `jev.id_patterns` regexes (`ID_n`); IPv6
  addresses; host names in any case when the last label is a known TLD
  (`DB1.CORP.ACME.COM`).
- **Hosts on word suffixes:** (obfuscation only) a dotted name of two or more labels, in any
  case, ending in `local`, `prod`, `staging`, `stage`, `qa`, `uat`, `int`,
  `private`, `office`, `home`, `cloud`, `dev`, `app`, `in`, `it`, `at`,
  `be`, `me`, `us`, `no`, `so`, `to`, `info`, `tech`, `site`, `online`,
  `global` or `test` is a host (`acme.dev`, `ledger.prod`, `ACME.LOCAL`)
  unless another label is a code or member word (`config`, `env`,
  `window`, `this`, `process`, `self`, …): `config.dev`, `window.app` and
  `process.env.dev` stay. A name followed by `(` or with a camelCase label
  (`fooBar.baz`) is code and stays, as does `f64::MAX`.
- **Name matcher size (obfuscation only):** `jev.name_matcher_bytes` (default `67108864`,
  64 MiB) caps the heap bytes of the compiled matcher of learned names and
  file names. A run whose names do not fit fails before anything is sent.
  The matcher is one Aho-Corasick automaton over every name (gate B 2; it
  was one regex alternation, which needed about 1 GiB for 225,000 names).
  Measured on a synthetic set shaped like a production database (230,000
  file paths with their basenames and stems, 80,000 logins, 40,000
  two-word names with their parts, 20,000 hyphenated `org/repo` names;
  672,780 names in all): 44,453,564 bytes (42 MiB), built in 1.1 s in a
  release build and 7.9 s in a debug build; the whole test process peaked
  at about 400 MB resident. At about 66 bytes per name the default holds
  roughly a million names.
- **Payload dump:** with `jev.payload_dump_dir` set, tga writes each exact
  outbound request body to `<dir>/jev-request-<hash>.json`, sends nothing, and
  records the calls as `skipped`; no key is needed. It works in both text
  modes. With obfuscation on, next to each body it
  writes `<dir>/jev-request-<hash>.tokens.json`, `{"tokens": {"PERSON_1":
  "<original>", …}}` for every pseudonym in that message, so a scanner can
  measure which words each token replaced. That map holds the originals:
  keep the dump directory on the host. In real-text mode no token map is
  written, and the body itself holds the original message. `tga classify` still writes
  the rule verdicts, so point it at a scratch copy of the database. A relative
  path resolves against the config file's directory.

```yaml
llm:
  source: jev
  # api_key_env: TYPESAFE_API_KEY     # the default for this source
  # model: jev-1.13.0                 # pinned; any other value is an error
  jev:
    budget_usd: 0.25                  # per-run spend cap (default)
    # obfuscate: true                 # pseudonymize first (default: false, real text)
    # sensitive_terms: [ledgerd, paygate]   # extra names to hide; obfuscation only
    # payload_dump_dir: ./jev-payloads   # write bodies, send nothing
    # name_matcher_bytes: 134217728      # raise past ~1M names (default 64 MiB)
```

#### Commit context (`llm.context`, #111)

By default the LLM sees only the commit message. `llm.context` adds facts
the database stores about the commit, read by the same joins `tga eval
sample` uses for the label sheet:

- `paths`: the changed file paths (`files.path`), in path order. The list
  stops before the first path that would pass `context_max_paths` entries
  or `context_max_path_bytes` bytes; a cut list's header says how many of
  how many are shown.
- `pr_title`: the title of the lowest-numbered stored pull request whose
  `commit_shas` contains the commit.
- `issue_type`: the `item_type` of the first work item linked to the
  commit (`commit_work_items` → `work_items`), such as `Bug` or `Story`.

The facts go in one block after the message, each on one line:

```text
<commit message>

--- commit context (from the repository, not the commit message) ---
Changed paths (2 of 41 shown):
- src/ledger/retry.rs
- tests/retry_tests.rs
PR title: Retry ledger writes
Issue type: Bug
--- end commit context ---
```

A fact the database does not hold for a commit is left out; a commit with
none gets no block. With `context: []`, the default, every provider sends
exactly the pre-#111 prompt. All four sources honour the key. For `jev` the
block is part of `state.commit.message`; with `jev.obfuscate: true` it is
pseudonymized as well: every path becomes a whole `PATH_n` (plus a known
extension), whatever its shape, and the PR title and issue type pass
through the same pseudonymizer as the message. Only the block's fixed
header lines are sent unchanged.

Caching: no LLM verdict is cached by prompt. `tga classify` sends only
commits that have no stored verdict, so commits classified before
`context` was set keep their verdict until a `--force` run re-sends them
with the block. `llm_usage` gains one row per call and is not a cache. The
Jev payload dump names each file by a hash of the body, so a context body
never shares a file name with a no-context body.

```yaml
llm:
  source: anthropic-api
  api_key_env: ANTHROPIC_API_KEY
  context: [paths, pr_title, issue_type]
  context_max_paths: 30          # default
  context_max_path_bytes: 2048   # default
```

#### Input budget (`llm.max_input_tokens`, #178)

A commit message can be hundreds of kilobytes long, for example when it
embeds a diff or a generated file. Sent whole, such a prompt passes the
model's context window and the provider rejects every call for that commit.
`max_input_tokens` caps every prompt that `openrouter`, `bedrock` and
`anthropic-api` send, both for the `tga classify` LLM fallback and for the
complexity backfill. When the key is unset, the default depends on the
source:

| Source | Default | Why |
|--------|---------|-----|
| `bedrock`, `anthropic-api` | `190000` | Claude models have a 200k-token window; 10k is left for the reply (at most 2,048 tokens) and request framing. |
| `openrouter` (and the legacy OpenAI-compatible endpoints) | `100000` | Fits a 128k-token window such as `gpt-4o-mini`, the OpenRouter default. |

A set `max_input_tokens` overrides the default for every source. A value
below `8192` fails the config load with an error naming the key and the
minimum: 4,096 tokens of room for the commit plus as much again for the
system prompt and framing. If a configured system prompt is long enough
to leave less than 4,096 tokens of room, tga sends nothing for that commit
and records the call as failed, so no verdict on a marker-only prompt is
stored.

tga has no tokenizer. It counts one token per byte of the system prompt,
the fixed `Classify this commit message:` prefix, the message and the
context block, plus 64 tokens for request framing. One byte per token is
an upper bound: every token of a byte-level BPE vocabulary (Claude,
GPT-4o) is at least one byte long. Ordinary commit text runs near 3.3
bytes per token, so the estimate counts about three times the real
number. A prompt at the cap is therefore at most that many real tokens,
whatever it contains.

A prompt within the budget is sent byte-identical to tga 10.3.2. A
larger one is cut:

- The context block keeps the bytes the message leaves free, and at least
  a quarter of the room. The message gets the rest.
- Each cut part ends at a UTF-8 character boundary, followed by a
  `[truncated N bytes]` line, where N is the number of bytes left out.
- The cut is deterministic: the same commit always sends the same prompt.
- tga logs a warning with the prompt size before and after the cut.

`jev` ignores the key and sends its pseudonymized text uncut;
`jev.budget_usd` caps its spend, not the size of one request.

```yaml
llm:
  source: openrouter
  model: some/small-context-model
  max_input_tokens: 24000   # unset: 190000 for bedrock/anthropic-api, 100000 otherwise
```

#### Self-enabling behavior (added v2.3.0)

When a valid `llm:` section is present in the config, the LLM classification tier
is **automatically enabled** — no `classification.use_llm: true` is required. The
intent is: if you wrote `llm:`, you mean to use it.

Precedence (#175, tga 10.3.2):
1. `classification.use_llm: false` → LLM tier **off**, whatever the `llm:`
   section says. tga builds no provider client and makes no LLM call in
   `tga classify`, `tga analyze`, the `tga audit` sweep's classify stage or the
   complexity backfill. The run logs at info that the `llm:` section is ignored.
2. `classification.use_llm: true` → LLM tier on, through the `llm:` section when
   present, else the legacy `classification.llm_provider` fields.
3. `classification.use_llm` absent, `llm:` section present → LLM tier on.
4. Neither → LLM tier off.

`use_llm` must be `true` or `false`; an empty value or `null` is a config error,
so a half-written off switch never reads as absent. The `--use-llm` flag on
`tga classify` and `tga backfill complexity` sets `use_llm: true` for that run,
overriding a `use_llm: false` in the file.

To disable the LLM tier while keeping the `llm:` config, set
`classification.use_llm: false`. Before 10.3.2 that setting was ignored when an
`llm:` section was present, and every eligible commit went to the provider.

#### Default models by source

| Source | Default model |
|--------|---------------|
| `openrouter` | `gpt-4o-mini` |
| `bedrock` | `us.anthropic.claude-haiku-4-5-20251001-v1:0` |
| `anthropic-api` | `claude-3-5-haiku-latest` |
| `jev` | `jev-1.13.0` |

#### Security note

`api_key_env` stores the **variable name** (e.g. `OPENROUTER_API_KEY`), never
the key value. The actual secret is read from the environment at runtime. Never
commit API keys to the config file.

---

### `classification.buckets` — two-level bucket map (#111)

A classification has two levels. The **primary** is a bucket; the
**secondary** is the fine category within it. A bucket map maps each bucket
name to its fine categories, in report order.

The consumer owns the map (owner ruling 2026-10-06): the downstream
consumer defines the secondary categories, their rules and the
secondary → primary map, and supplies them as config. tga classifies; its
built-in map is only a fallback for a consumer that supplies none. The map
in effect is the first of:

1. `classification.buckets` in the main config;
2. a top-level `buckets:` map in the rules file (`classification.rules_file`,
   or the file `--rules` names on `tga classify`, `tga eval score`,
   `tga eval repredict` and `tga rules list`). With several rules files, a later
   file's map replaces an earlier one whole;
3. tga's built-in fallback:

```yaml
classification:
  buckets:
    Maintenance: [bug_fix, devops, security, qa, upkeep]
    Value Creation: [new_feature, integration, content_design]
    Foundational Investment: [platform_infrastructure, data_science]
    Internal Tooling: [internal_tooling]
```

A rules file carrying its own map:

```yaml
extend_defaults: false
rules:
  - id: defect
    category: bug_fix
    keywords: ["fix:"]
  - id: deps
    category: upkeep
    keywords: ["chore(deps)"]
  - id: feat
    category: new_feature
    keywords: ["feat:"]
categories:
  - name: bug_fix
    description: Corrects behaviour that was wrong.
buckets:
  Maintenance: [bug_fix, upkeep]
  Value Creation: [new_feature]
```

`tga rules list --format json` prints the map in effect and its source
under `bucket_map` (`{"source": "config" | "rules_file" | "fallback",
"buckets": {...}}`); `tga classify` and `tga eval score` name the source on
the console.

- A present map replaces the lower levels whole; to move one category, copy
  the table and edit it. A rules-file map has the same shape and the same
  checks as `classification.buckets`.
- Bucket names are free text. Category names are matched case-insensitively.
- Load-time errors: no bucket, a blank or duplicate bucket name, a bucket
  with no categories, a category in two buckets, or a no-answer label
  (`unclear`, `mixed`, `release_merge`, `uncategorized`) in the map.
- A map other than the default must name only categories the config knows
  (its taxonomy and rules files). `content_design` is always accepted. An
  unknown category is an error at `tga classify`, at `tga eval score`, and
  when a Jev tier starts; it is never dropped silently. The default map is
  accepted with any config, so a config with another category scheme still
  loads; its categories then have no bucket.
- `unclear`, `mixed` and `release_merge` have no bucket. A predicted
  `uncategorized`, or any category the map does not name, has no bucket and
  is wrong at both levels.
- The pair is derived from the fine category, the same way for every arm
  (rules, Bedrock, Jev). Nothing is stored and there is no migration:
  `tga classify` prints a derived "By bucket" breakdown, and `tga eval score`
  reports primary and secondary accuracy (see `docs/eval-harness.md`).
- With `llm.source: jev`, the one category question per commit offers the
  rules' categories and, when the consumer supplies the map (level 1 or 2),
  every fine category in it. The built-in fallback adds no category to the
  question: its categories reach Jev only through the rules, as they reach
  Bedrock and the other sources.

### `classification.repo_categories` — repo → category map (#158, #167)

Some repositories map to one category for every commit (owner ruling
2026-10-07). `classification.repo_categories` maps a repository name, or a
repository plus a path prefix, to a category. How the mapped category
combines with the cascade is set by `classification.repo_map` (below): a
hard override (the default) or a floor.

```yaml
classification:
  rules_files: [./rules.yaml]
  repo_categories:
    mcp-services: internal_tooling
    duetto-dashboard: internal_tooling
    cs-system-audit: internal_tooling
    duetto-pa-agents: internal_tooling
    hr-skills: internal_tooling
    duetto-playwright-e2e: qa
```

- **A hard override** (`repo_map.mode: override`, the default). A mapped
  commit gets the category at confidence 1.0 ahead of every other tier: the manual override
  (`classification_overrides`), the exact and regex rules (a security fix
  included), the issue-type and JIRA project tiers, external sources,
  weighted sum, fuzzy and the LLM.
- **No LLM spend** in override mode. A mapped commit is never sent to the
  LLM fallback, nor
  by `tga classify --backfill-complexity`; no `llm_usage` row is written
  for it.
- **Method `repo_map`.** The `classifications.method` column, the
  `tga classify` "by method" counts, and `tga eval`'s per-method precision
  all show `repo_map`. The eval rule id is `repo_map:<key>`, naming the
  key that decided the commit (`repo_map:<repository>` or
  `repo_map:<repository>:<prefix>`).
- **Matching.** A key is the name tga stores in `commits.repository`:
  `repositories[].name`, else the basename of `repositories[].path`. It
  matches the whole name, case-sensitively: `mcp-services` does not match
  `MCP-Services` or `mcp-services-v2`. A key holding `*` is an error; there
  are no globs. A key with a `:` is a path-prefix key (below). A
  repository part may hold a `/` only when it equals a configured
  `repositories[].name` (`acme-org/widget`); otherwise, as in
  `acme-mono/api`, it is an error naming the key and saying no configured
  name matches. An empty repository or prefix around the `:` is an error.
  Two keys naming the same repository and prefix (`r:api` and `r:api/`)
  are an error.
- **Merges.** A merge commit is not mapped. It keeps its existing handling:
  the rules decide it and it never reaches the LLM.
- **Checks.** Each category must be one the config knows: under
  `extend_defaults: false`, the rules files' categories and `categories:`
  entries (the set the LLM tier is restricted to); otherwise the taxonomy
  plus every rule category. Category names match case-insensitively and are
  stored in the config's spelling. An unknown category is an error naming
  the repository and the category, raised when `tga classify`,
  `tga eval sample` or `tga eval repredict` starts, before any write.
- **Unmatched keys warn.** When `tga classify`, `tga eval sample` or
  `tga eval repredict` starts, each key whose repository is not in
  `commits.repository`, and each prefix key whose repository holds no
  stored path under the prefix, gets one warning naming the key.
- **Unmapped repositories** classify exactly as before.
- Before #158 this key was documented as a last-resort per-repo fallback
  with glob keys, but `tga classify` never applied it.

#### Path-prefix keys: `<repo>:<prefix>` (#167)

A monorepo maps by subdirectory. A key `<repo>:<prefix>` applies to the
commits of `<repo>` whose changed paths (the `files` table) lie under
`<prefix>`. The prefix loses a leading `./` and leading or trailing `/`,
and matches whole path segments: `services` holds `services/api/main.rs`,
not `services-legacy/x.rs`. Paths match case-sensitively.

```yaml
classification:
  repo_categories:
    acme-mono: internal_tooling            # bare key: every other path
    acme-mono:services: platform_infrastructure
    acme-mono:services/qa-harness: qa      # longest prefix wins
```

Each changed path resolves to one key: the longest prefix key it lies
under, else the bare `<repo>` key, else no key. A commit whose paths span
several keys is resolved by a vote, so the result never depends on path
order:

1. Each path votes for its key's category; a path with no key votes
   "unmapped".
2. The category with the most votes wins. Two keys with the same category
   pool their votes.
3. A tie goes to the category whose most specific voting key is longest (a
   bare key is the least specific), then to the alphabetically first
   category.
4. "Unmapped" wins only with strictly more votes than every category; the
   commit is then not mapped.

A commit with no stored paths, or a repository with no prefix keys, uses
the bare key alone. The rule id names the winning category's most specific
voting key; between equal-length keys, the alphabetically first prefix.

#### `classification.repo_map` — override or floor (#167)

```yaml
classification:
  repo_map:
    mode: floor                                 # override (default) | floor
    exceptions: [qa, security, devops, bug_fix] # default
    min_confidence: 0.8                         # default
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `mode` | `override` \| `floor` | `override` | `override` is the #158 hard override above. `floor` makes the mapped category the default and lets the cascade infer exceptions. |
| `exceptions` | list of categories | `[qa, security, devops, bug_fix]` | Categories a cascade verdict may keep in floor mode. Names compare through the taxonomy's canonical names, case-insensitively, so `bug_fix` matches the built-in rules' `bugfix` (aliases: `bug_fix` = `bugfix`, `new_feature` = `feature`, `tech_debt_refactoring` = `refactor`). `qa` matches `test` and `devops` matches `ci`, unless the config's rules or `custom_categories` define `qa` or `devops`; then the name matches exactly (#171). In a written list, a name the config's category set lacks is an error. In the default list it is a warning. |
| `min_confidence` | float in `[0, 1]` | `0.8` | Lowest confidence at which an exception verdict is kept. A value outside `[0, 1]` is an error. |

Without the block, or with `mode: override`, behaviour is exactly that of
10.2.0. Unknown keys in the block fail the load, as elsewhere in
`classification:`.

In floor mode:

- The whole cascade runs for a mapped commit: the manual override, rules,
  external sources, weighted sum, fuzzy and the LLM. A mapped commit is
  LLM-eligible again under the usual `llm_fallback_scope` and
  `llm_fallback_threshold` routing.
- After the cascade, a mapped commit keeps the cascade's verdict only when
  its category is in `exceptions` and its confidence is at or above
  `min_confidence`. Every other mapped commit gets the mapped category at
  confidence 1.0 with method `repo_map`, keeping the replaced verdict's
  complexity score (the complexity backfill skips `repo_map` rows). The
  rule applies to every tier alike, the manual override included.
- Under the built-in rules, `security`, `bugfix`, `test` and `ci` verdicts
  match the default exceptions: the built-in rules emit no `qa` or
  `devops`, so those names stand for `test` and `ci` (#171). `build`
  verdicts do not match; to keep them, write the list, e.g.
  `exceptions: [qa, security, devops, bug_fix, build]`.
- A config whose rules or `custom_categories` define `qa` or `devops`
  (such as the eval scheme v2 rules file) matches those names exactly:
  there, `test` and `ci` verdicts are not exceptions unless listed.
- A merge commit is never mapped, as in override mode.
- `tga eval sample` and `tga eval repredict` apply the same rule to the
  carried or re-derived verdict; a carried verdict the floor replaces counts
  as superseded.

---

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
| `fetch_prs` | bool | false | Fetch pull request metadata from GitHub |
| `fetch_pr_reviews` | bool | true | Fetch review summaries with PRs |
| `open_pr_refresh_ttl_hours` | u32 | 1 | TTL for refreshing open PR snapshots |
| `ticket_regex` | string | None | Override regex for detecting GitHub ticket refs (e.g. `#(\d+)`) in commit messages. Added in v1.0.6 (#75). |
| `work_items_unavailable` | string | None | Declares the GitHub work-item leg ABSENT for this run and says why. Set it and no GitHub adapter is built, the reason is logged, and `tga audit` names it in the report's Gaps section as a leg that was not attempted. Leave it unset and nothing changes — a configured `repo` whose fetch fails still fails the `collect` stage. A reason, never a boolean: an empty string declares nothing. Added in #6130. |

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

### Identity resolution — alias table and fuzzy fallback

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `developer_aliases` | dict[string, list[string]] | {} | Inline alias map: canonical name → emails / login handles |
| `aliases_file` | path | None | External YAML alias file, merged over `developer_aliases` (supports `~`) |
| `fuzzy_identity_fallback` | bool | see below | Override for the Tier-3/4 Jaro-Winkler fallback (added v2.11.0, issue #4251) |

`tga` resolves each observed `(author_name, author_email)` pair in four tiers:

1. exact alias match on the email,
2. exact alias match on the display name,
3. Jaro-Winkler similarity ≥ 0.85 against canonical names, and against email
   local-parts **when both addresses share the same domain** (issue #2253),
4. Jaro-Winkler ≥ 0.82 against punctuation-normalized names / local-parts.

Tiers 3 and 4 exist to *guess* identities that were never declared. Once a
project supplies a comprehensive `aliases_file`, every additional roster entry
enlarges the fuzzy haystack, and the guesser starts collapsing distinct people
onto similarly-spelled colleagues (`Crispian Alvarenga` → `Crisandra Fonseca`,
`Marci Dalton` → `Marcel Sutton`). So:

- **`fuzzy_identity_fallback` unset (default)** — Tiers 3/4 run unless a
  declared `aliases_file` **successfully loaded a non-empty alias table**. With
  such a table present, an author that matches no declared alias is reported
  under its own raw name. A declared `aliases_file` that is missing, unreadable,
  malformed, or empty leaves Tiers 3/4 **on** and logs at `error!` — a broken
  config degrades to the previous behaviour rather than to silent
  fragmentation. An inline `developer_aliases` map does not disable the tiers;
  use `fuzzy_identity_fallback: false` for that.
- **`fuzzy_identity_fallback: true`** — force Tiers 3/4 on even with an alias
  file (pre-2.11.0 behaviour).
- **`fuzzy_identity_fallback: false`** — force Tiers 3/4 off even without an
  alias file (useful for exhaustive inline `developer_aliases` maps).

An undeclared author showing up under its own name is a visible, fixable gap —
add an alias. A silently misattributed one corrupts every per-developer metric
downstream with no signal that it happened.

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

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | bool | true | Enable the LLM fallback tier |
| `provider` | enum | `openrouter` | `openrouter` / `bedrock` / `auto` |
| `model` | string | `mistralai/mistral-7b-instruct` | LLM model identifier |
| `api_key` | string | env `OPENROUTER_API_KEY` | API key (not used for `bedrock`) |
| `confidence_threshold` | f64 | 0.7 | Minimum confidence to accept an LLM result |
| `llm_fallback_threshold` | f64 | 0.0 | Commits with rule-based confidence **above** this value skip the LLM tier entirely. Setting to e.g. `0.5` avoids sending already-confident results to the LLM. Added in v1.0.6 (#78). |
| `llm_fallback_concurrency` | usize | 4 | Maximum concurrent LLM requests during the fallback pass (`buffer_unordered` cap). Increase to reduce wall-clock time when API latency is the bottleneck. Added in v1.0.6 (#83). |
| `batch_size` | u32 | 50 | Commits per LLM batch |
| `max_tokens` | u32 | 50 | Maximum tokens per LLM response |
| `temperature` | f64 | 0.1 | Sampling temperature |
| `timeout_seconds` | u32 | 30 | Per-request timeout |
| `cache_ttl_days` | u32 | 90 | Cache TTL for LLM results |

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

### `audit` — AuditConfig (added issue #5482)

Settings for `tga audit`, the one-shot due-diligence sweep.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `window_weeks` | u32 | 52 (1 year) | Lookback window in ISO weeks, applied to the sweep's collect, classify, and pr-metrics stages |

**Precedence** (highest first):

1. `tga audit --weeks` — always wins.
2. `audit.window_weeks:` in this config file.
3. The 52-week default.

```yaml
# Example: a two-year engagement
audit:
  window_weeks: 104
```

The window is a real collection bound, not a report filter — a narrower window
collects less. `trusty-audit` spawns `tga audit` with no `--weeks`, so this field
is the only way an engagement states its own window.

### `jira` — JIRAConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `access_user` | string | env `JIRA_USER` | JIRA API username (email for Cloud) |
| `access_token` | string | env `JIRA_TOKEN` | JIRA API token |
| `base_url` | url | required if JIRA used | JIRA instance base URL |
| `ticket_regex` | string | None | Override regex for detecting JIRA ticket refs (e.g. `([A-Z]+-\d+)`) in commit messages. Added in v1.0.6 (#75). |

### `jira_integration` — JIRAIntegrationConfig

| Field | Type | Default |
|-------|------|---------|
| `enabled` | bool | false |
| `fetch_story_points` | bool | true |
| `project_keys` | list[string] | [] |
| `story_point_fields` | list[string] | `["customfield_10016"]` |

### `linear` — LinearConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `api_key` | string | env `LINEAR_API_KEY` | Linear API key. Supports `${LINEAR_API_KEY}`; when unset, the shared credential resolver is asked for the `linear` key. |
| `team_keys` | list[string] | [] | Team keys (e.g. `["ENG"]`). The per-commit lookup fetches only issues whose identifier prefix is listed (empty = every team). `tga linear sync` with no `--team` and no `--all-teams` needs exactly one entry. |
| `fetch_on_reference` | bool | true | During `collect`, fetch the Linear issues that commit messages name. |
| `ticket_regex` | string | None | Override regex for detecting Linear ticket refs (e.g. `([A-Z]+-\d+)`) in commit messages. Added in v1.0.6 (#75). |
| `stats.field_use_created_since` | date (`YYYY-MM-DD`) | None | `tga linear stats` field use (C9) counts only issues created on or after this UTC date. None = every issue. |
| `stats.concentration_completed_since` | date (`YYYY-MM-DD`) | None | `tga linear stats` concentration (C14) counts only issues completed on or after this UTC date. None = every completed issue. |
| `stats.report` | bool | false | Append the Linear delivery section to `report.md` in `tga report` and `tga analyze`, when the database holds synced Linear issues. The bounds above apply. See [reporting.md](reporting.md#linear-delivery-section). |

The `stats` block is optional (#190). A value that is not a `YYYY-MM-DD`
date fails the config load. See
[cli-commands.md](cli-commands.md#tga-linear-stats).

```yaml
linear:
  stats:
    field_use_created_since: 2025-01-01
    concentration_completed_since: 2026-01-01
    report: true          # #190: Linear delivery section in report.md
```

There is no `team_id` key; the team scope is `team_keys`. The bulk sync
(`tga linear sync`, see [cli-commands.md](cli-commands.md#tga-linear-sync))
takes its scope from `--team`, `--all-teams` or `team_keys`.

### `pm.azure_devops` — AzureDevOpsConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `organization_url` | string | required | ADO org URL (e.g. `https://dev.azure.com/myorg`) |
| `pat` | string | required | Azure DevOps Personal Access Token |
| `project` | string | None | Default ADO project name |
| `fetch_on_reference` | bool | false | Fetch work items when `AB#N` refs appear in commits |
| `fetch_prs` | bool | false | Fetch ADO pull requests and reviewer data into `pull_requests` + `pr_reviewers` tables. Added in v1.0.6 (#84). |
| `ticket_regex` | string | `AB#(\d+)` | Override regex for detecting ADO work item refs in commit messages. Must contain a capture group. Added in v1.0.6 (#75). |

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

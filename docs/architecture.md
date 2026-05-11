# Architecture

Technical deep-dive into the trusty-git-analytics Rust workspace.

## Workspace Structure

```
trusty-git-analytics/
├── Cargo.toml              # Workspace manifest; all dep versions pinned here
├── crates/
│   ├── tga-core/           # Foundation crate — no internal dependencies
│   │   └── src/
│   │       ├── config/     # YAML deserialization
│   │       ├── db/         # SQLite wrapper + migrations
│   │       │   └── sql/    # Embedded SQL files (include_str!)
│   │       ├── errors.rs   # TgaError (thiserror)
│   │       └── models/     # Domain structs
│   ├── tga-collect/        # Stage 1: depends on tga-core
│   │   └── src/
│   │       ├── git/        # libgit2 extractor + diff
│   │       ├── github/     # REST client (reqwest)
│   │       ├── identity/   # IdentityResolver
│   │       ├── jira/       # JIRA REST client
│   │       ├── collector.rs
│   │       └── errors.rs
│   ├── tga-classify/       # Stage 2: depends on tga-core
│   │   └── src/
│   │       ├── tiers/      # exact, regex_tier, fuzzy, llm
│   │       ├── rules/      # types, loader, default_rules
│   │       ├── classifier.rs
│   │       ├── pipeline.rs
│   │       └── errors.rs
│   ├── tga-report/         # Stage 3: depends on tga-core
│   │   └── src/
│   │       ├── formatters/ # csv, json, markdown
│   │       ├── aggregator.rs
│   │       ├── models.rs
│   │       ├── pipeline.rs
│   │       ├── templates.rs
│   │       └── errors.rs
│   └── tga-cli/            # Binary: depends on all four crates
│       └── src/
│           ├── commands/   # analyze, collect, classify, report
│           └── main.rs
├── configs/                # Example configuration files
└── docs/
    └── requirements/       # Full specification documents
```

### Crate Dependency Graph

```
tga-core
    │
    ├──► tga-collect ──────────────────────────────┐
    │                                               │
    ├──► tga-classify ─────────────────────────────┤
    │                                               │
    └──► tga-report ──────────────────────────────►tga-cli (binary)
```

Publish order on crates.io must follow this graph: `tga-core` first, then `tga-collect` / `tga-classify` / `tga-report` in parallel, then `tga-cli`. See `docs/publishing.md`.

## Database Schema

SQLite with WAL mode. One database file (`tga.db` by default) holds all three pipeline stages.

### Tables

**`authors`** — canonical developer identities:

```sql
CREATE TABLE authors (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    canonical_name  TEXT NOT NULL,
    canonical_email TEXT NOT NULL UNIQUE,
    aliases         TEXT NOT NULL DEFAULT '[]'  -- JSON array of strings
);
```

**`commits`** — raw git commit records:

```sql
CREATE TABLE commits (
    id                INTEGER PRIMARY KEY AUTOINCREMENT,
    sha               TEXT NOT NULL UNIQUE,
    author_id         INTEGER REFERENCES authors(id) ON DELETE SET NULL,
    author_name       TEXT NOT NULL,    -- raw name from git
    author_email      TEXT NOT NULL,    -- raw email from git
    timestamp         TEXT NOT NULL,    -- ISO 8601
    message           TEXT NOT NULL,
    repository        TEXT NOT NULL,
    files_changed     INTEGER NOT NULL DEFAULT 0,
    insertions        INTEGER NOT NULL DEFAULT 0,
    deletions         INTEGER NOT NULL DEFAULT 0,
    classification_id INTEGER REFERENCES classifications(id) ON DELETE SET NULL,
    confidence        REAL,
    is_merge          INTEGER NOT NULL DEFAULT 0  -- boolean
);
```

**`classifications`** — one row per distinct verdict:

```sql
CREATE TABLE classifications (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    category    TEXT NOT NULL,     -- e.g. "feature", "bugfix"
    subcategory TEXT,              -- e.g. "security", "ticketed"
    ticket_id   TEXT,              -- e.g. "PROJ-123"
    confidence  REAL NOT NULL DEFAULT 0.0,
    method      TEXT NOT NULL      -- exact_rule | regex_rule | fuzzy_match | llm_fallback | manual
);
```

**`files`** — file-level change records:

```sql
CREATE TABLE files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    commit_id   INTEGER NOT NULL REFERENCES commits(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    change_type TEXT NOT NULL,  -- added | modified | deleted | renamed
    insertions  INTEGER NOT NULL DEFAULT 0,
    deletions   INTEGER NOT NULL DEFAULT 0
);
```

**`pull_requests`** — GitHub PR metadata:

```sql
CREATE TABLE pull_requests (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_number    INTEGER NOT NULL,
    title        TEXT NOT NULL,
    author       TEXT NOT NULL,
    state        TEXT NOT NULL,   -- open | closed | merged
    created_at   TEXT NOT NULL,
    merged_at    TEXT,
    commit_shas  TEXT NOT NULL DEFAULT '[]'  -- JSON array
);
```

**`schema_migrations`** — migration bookkeeping:

```sql
CREATE TABLE schema_migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL
);
```

### Pragmas Applied on Every Open

```sql
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
PRAGMA foreign_keys=ON;
```

WAL mode is non-negotiable: it allows concurrent reads during write-heavy collection while staying safe for single-writer use.

## Classification Cascade

The cascade is implemented in `tga-classify`. Each tier either returns a `ClassificationResult` or `None`. The first non-`None` result wins.

### Tier 1 — Exact (Aho-Corasick)

`ExactMatcher` collects all `keywords` from every `Rule` and feeds them into a single Aho-Corasick automaton at construction time. Classification is a single O(n) scan of the message string, where n is the message length regardless of how many keywords exist. When a keyword fires, the associated `Rule` is returned.

Rules are sorted by `priority` (highest first) before automaton construction; Aho-Corasick returns the first match in input order, so higher-priority keywords placed earlier in the pattern set win ties.

### Tier 2 — Regex

`RegexMatcher` compiles each `pattern` string from every `Rule` into a `regex::Regex` at construction. Classification iterates rules by priority and applies each compiled regex; the first match returns the associated rule. Patterns are anchored by the rule author (`(?i)^\s*feat...`); no implicit anchoring is applied by the matcher.

`RegexMatcher::extract_ticket_id()` is a standalone helper that applies a JIRA-pattern regex (`\b[A-Z][A-Z0-9]+-\d+\b`) to extract ticket references. It is called by tiers 1 and 2 to populate `ticket_id` on their results.

### Tier 3 — Fuzzy Heuristics

`FuzzyClassifier` applies structural rules that require no external data:

- If `is_merge` is `true`, return category `merge`.
- If the message starts with `"Merge pull request"` or `"Merge branch"` (case-insensitive prefix match), return category `merge`.
- If the message starts with `"Revert "` (case-insensitive), return category `revert`.

All tier-3 results carry `ClassificationMethod::FuzzyMatch` and confidence 0.8.

### Tier 4 — LLM Fallback

`LlmClassifier` is constructed with a model name and optional API key. When enabled (key is present), it formats the commit message into a prompt requesting a single JSON object `{"category": "...", "confidence": 0.0}` and posts it to the OpenAI chat completions endpoint. Responses are parsed leniently; malformed JSON causes the tier to return `None`, falling through to `uncategorized`.

The LLM tier is async and not included in `classify_batch()` — batch processing uses only tiers 1–3 via Rayon parallel iteration.

### Batch vs. Single Classification

```
ClassificationEngine::classify_batch(&[(msg, is_merge)])
    → Rayon par_iter → classify_sync() for each
    → returns Vec<ClassificationResult> (no LLM)

ClassificationEngine::classify(msg, is_merge).await
    → classify_sync() → if None, try LLM → fallback to unclassified
```

`ClassificationPipeline` uses `classify_batch()` for the bulk of commits and calls the async `classify()` only for commits that tiers 1–3 failed to classify when LLM is enabled.

## Identity Resolution

`IdentityResolver` maps observed `(author_name, author_email)` pairs from git commits to canonical identities.

### Resolution Order

1. **Exact alias on email**: look up `email.to_lowercase()` in the aliases map. If found, return the canonical `(name, email)` pair.
2. **Exact alias on name**: look up `name.to_lowercase()` in the aliases map. If found, return the canonical pair.
3. **Jaro-Winkler fuzzy match**: compute `jaro_winkler(input, canonical)` for both the name and email of every team member. Accept the best match whose score exceeds the threshold (default 0.85). The `strsim` crate provides the implementation.
4. **Passthrough**: return the input unchanged.

### Alias Map Construction

When `developer_aliases` is used (Python-compat format), each entry maps canonical name → list of aliases. The resolver pre-processes this into a flat `HashMap<String, String>` (alias-lowercase → canonical-name) and a `Vec<(canonical_name, canonical_email)>` for fuzzy matching. The first email-shaped entry in each alias list is treated as the canonical email.

When `team.members` is used, the same flat map is built from `member.email`, `member.aliases`, and the free-form `team.aliases` map.

### Database Upsert

`upsert_author()` resolves, then executes:

```sql
INSERT INTO authors (canonical_name, canonical_email, aliases)
VALUES (?, ?, '[]')
ON CONFLICT(canonical_email) DO UPDATE SET canonical_name = excluded.canonical_name
```

The `commits.author_id` column is then backfilled in a second pass after all commits for a repository are inserted.

## Configuration Loading

`Config::load(path)` applies tilde-expansion to the path itself (via `expand_path()`), reads the file as a string, and deserializes with `serde_yaml`. Unknown YAML keys are silently ignored (`#[serde(default)]` on every field), so config files written for newer versions of the tool — or for the Python predecessor — load cleanly in older binaries.

Path fields within the config (`RepositoryConfig::path`, `OutputConfig::directory`, `CacheConfig::directory`) are stored as `PathBuf`. Callers that use these paths should apply `expand_path()` before I/O. The `Database::open()` and `ReportPipeline` implementations do this automatically.

Environment variable interpolation in YAML values (e.g. `token: "${GITHUB_TOKEN}"`) is not handled by `serde_yaml` natively. `GithubConfig::token` stores the literal string; callers are expected to perform substitution or read the env var directly. The CLI reads `OPENAI_API_KEY` and `GITHUB_TOKEN` directly from the environment.

## DB Migration System

All migrations live as embedded SQL strings via `include_str!()` in `tga-core/src/db/migrations.rs`. The `MIGRATIONS` constant is a `&[Migration]` slice; each entry has `version: i64`, `name: &'static str`, and `sql: &'static str`.

On every `Database::open()`:

1. `ensure_migrations_table()` creates `schema_migrations` if it does not exist.
2. `current_version()` queries `MAX(version)` from that table.
3. Each migration with `version > current` is applied inside a transaction that also inserts the version record. Partial application is impossible.

To add a migration: append a new `Migration` to `MIGRATIONS` with a strictly increasing version number. Never edit an existing migration — write a follow-up instead.

## Error Handling Strategy

Library crates (`tga-core`, `tga-collect`, `tga-classify`, `tga-report`) define errors with `thiserror`. Each crate exposes its own error enum and a `Result<T>` alias:

- `tga_core::TgaError` / `tga_core::Result`
- `tga_collect::CollectError` / `tga_collect::Result`
- `tga_classify::ClassifyError` / `tga_classify::Result`
- `tga_report::ReportError` / `tga_report::Result`

The binary crate (`tga-cli`) uses `anyhow::Result` in `main` and the command handlers, letting `?` convert from any of the library error types via their `std::error::Error` implementations.

`unwrap()` and `expect()` are forbidden in library code. Tests use `expect("message")` with descriptive messages.

## Async / Sync Split

| Concern | Runtime |
|---------|---------|
| HTTP (GitHub API, JIRA, LLM) | `tokio` async |
| git extraction (libgit2) | synchronous |
| classification batch (tiers 1–3) | `rayon` parallel |
| LLM fallback classification | `tokio` async |
| CSV/JSON/Markdown write | synchronous |

`tga-cli` runs a `#[tokio::main]` entry point. The `CollectionPipeline::run()` and `ClassificationPipeline::run()` methods are `async` to accommodate the HTTP and LLM calls. The git extraction and Rayon batch classification inside those pipelines are synchronous blocking calls that complete before the async methods return.

The `git2` crate is not `Send + Sync`; repository handles are opened and dropped within the per-repo processing block and are never shared across threads.

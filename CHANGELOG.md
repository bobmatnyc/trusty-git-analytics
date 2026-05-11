# Changelog

All notable changes to trusty-git-analytics will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

#### tga-core
- `Config` struct with full YAML deserialization via `serde_yaml`; compatible with the Python `gitflow-analytics` config schema
- `Config::load()` with tilde-expansion on all path fields
- `Config::validate()` enforcing at minimum one configured repository
- `Config::resolved_aliases()` unifying `developer_aliases` (Python-compat flat map) and `team.members` (structured roster)
- `RepositoryConfig`, `TeamConfig`, `TeamMember`, `OutputConfig`, `ClassificationConfig`, `GithubConfig`, `JiraConfig`, `AnalysisConfig`, `CacheConfig` structs
- `Database` wrapper with mandatory WAL journal mode, `synchronous=NORMAL`, and `foreign_keys=ON` applied on every open
- `Database::open()` and `Database::open_in_memory()` with automatic migration execution
- `Database::journal_mode()` and `Database::schema_version()` introspection helpers
- Versioned SQL migration runner (`db::migrations`): transactional application, idempotent, records `(version, name, applied_at)` in `schema_migrations`
- Migration v1 (`0001_initial_schema.sql`): creates `authors`, `classifications`, `commits`, `files`, `pull_requests`, and `schema_migrations` tables with appropriate indexes and foreign keys
- Domain models: `Commit`, `Author`, `Classification`, `FileChange`, `PullRequest`
- Enums: `ClassificationMethod` (`exact_rule`, `regex_rule`, `fuzzy_match`, `llm_fallback`, `manual`), `ChangeType` (`added`, `modified`, `deleted`, `renamed`), `PrState` (`open`, `closed`, `merged`)
- `TgaError` enum with `thiserror` covering I/O, SQLite, YAML, validation, and migration errors
- `tga_core::Result<T>` type alias
- 100% `#[warn(missing_docs)]` coverage; `cargo doc` passes with `RUSTDOCFLAGS="-D warnings"`

#### tga-collect
- `CollectionPipeline` orchestrator: sequential per-repo git extraction, author backfill, optional GitHub PR fetch
- `CollectionStats` reporting commits collected, authors resolved, PRs fetched, and per-repo non-fatal warnings
- `GitCollector`: opens a local repository via libgit2, validates existence and git-ness on construction, walks the default branch, extracts commit SHA/author/timestamp/message/diff-stats
- `IdentityResolver` with three-tier resolution: (1) exact alias match on email, (2) exact alias match on name, (3) Jaro-Winkler fuzzy match (default threshold 0.85), (4) raw passthrough
- `IdentityResolver::from_config()` preferring `developer_aliases` over `team.members`
- `IdentityResolver::upsert_author()` writing canonical rows to the `authors` table with `ON CONFLICT` upsert
- `GitHubClient`: REST client for fetching pull requests via `reqwest` + `tokio`; stores PR rows via `store_pull_requests()`
- JIRA client stub (`JiraClient`) for future issue fetch integration
- `CollectError` enum with `thiserror`

#### tga-classify
- `ClassificationEngine` combining all four tiers behind a single `classify()` async entry point and a `classify_batch()` Rayon-parallel sync entry point
- `ClassificationEngineConfig` with `use_llm`, `llm_model`, and `confidence_threshold` fields
- `ExactMatcher`: builds a single Aho-Corasick automaton from all rule keyword lists; O(n) scan per message
- `RegexMatcher`: compiles one `Regex` per pattern string; first match wins; extracts JIRA ticket IDs via `extract_ticket_id()`
- `FuzzyClassifier`: heuristic detection of merge commits (via `is_merge` flag or "Merge pull request" prefix) and revert commits (via "Revert" prefix)
- `LlmClassifier`: optional async OpenAI-compatible fallback; reads `OPENAI_API_KEY` from environment; silently no-ops if key is absent
- `ClassificationResult` struct carrying `category`, `subcategory`, `confidence`, `method`, `ticket_id`
- Built-in default ruleset (`default_rules()`): 15 rules covering conventional commit prefixes (`feat`, `fix`, `chore`, `docs`, `refactor`, `test`, `ci`, `perf`, `style`, `build`, `revert`), breaking-change marker, JIRA-style ticket pattern, and keyword fallbacks (`bug`, `security`)
- `load_rules()`: YAML or JSON rules file loader (format detected by extension)
- `Rule` and `RuleSet` types with `id`, `category`, `subcategory`, `keywords`, `patterns`, `priority`, `confidence`
- `ClassificationPipeline` orchestrator: reads unclassified commits from DB, runs cascade, writes `classifications` rows, links `commits.classification_id`
- `ClassificationStats` with `total_commits`, `classified`, `by_method`, `by_category` breakdowns

#### tga-report
- `Aggregator`: reads `commits` + `classifications` + `authors` from DB and produces an in-memory `ReportData`
- `ReportData` with `generated_at`, `period_start`, `period_end`, `total_commits`, `total_authors`, `category_breakdown`, `authors`, `repositories`, `weekly_activity`
- `AuthorSummary` per-author rollup: name, email, commit count, insertions, deletions, files changed, category map, first/last commit timestamps
- `RepositorySummary` per-repo rollup: name, commit/author counts, insertions, deletions, top categories
- `WeeklyActivity` per-week/author/repo bucket: ISO week label, counts, insertions, deletions, category map
- CSV formatter: `write_author_csv()` → `authors.csv`, `write_weekly_csv()` → `weekly_activity.csv`
- JSON formatter: `write_json()` → `report.json` (serializes `ReportData` directly)
- Markdown formatter: `write_markdown()` → `report.md` via embedded Tera template
- `ReportPipeline` orchestrator: resolves output directory, dispatches to all configured formatters, returns `ReportStats` with files written
- Default output directory `./reports` when `output.directory` is unset
- All three formats emitted when `output.formats` is empty

#### tga-cli
- `tga` binary with four subcommands wired to the pipeline crates
- `tga analyze`: runs collect → classify → report in sequence; `--skip-collect` and `--skip-classify` flags for partial re-runs; `--output` override
- `tga collect`: `--repos` filter, `--since` / `--until` date overrides
- `tga classify`: `--rules` file override, `--use-llm` flag
- `tga report`: `--output` directory override, `--formats` comma-separated list
- Global `--config` (default `config.yaml`), `--database` (default `tga.db`), and `-v`/`-vv`/`-vvv` verbosity flags
- `tracing-subscriber` initialization from verbosity count: `WARN` / `INFO` / `DEBUG` / `TRACE`
- Graceful config-not-found fallback to `Config::default()`
- `anyhow::Result` error propagation to `main`

#### CI / CD
- GitHub Actions CI workflow (`ci.yml`): runs on push and PR to `main`; matrix over `stable` and `beta` toolchains; jobs: format check, Clippy with `-D warnings`, tests (skipping the integration test that requires a local git repo configured via `INTEGRATION_REPO_PATH`), rustdoc build with `RUSTDOCFLAGS="-D warnings"`, release binary build
- Concurrent-run cancellation via `concurrency.cancel-in-progress`
- Rust artifact caching via `Swatinem/rust-cache@v2`
- GitHub Actions publish workflow (`publish-tga-core.yml`): triggered by `tga-core-v*` tags or `workflow_dispatch`; dry-run gate, Clippy gate, then `cargo publish`; supports `dry_run` input to skip actual upload
- `CARGO_REGISTRY_TOKEN` secret required for actual publish

#### Integration
- `configs/example-config.yaml`: example config for analyzing multiple repositories with developer aliases, CSV + Markdown output

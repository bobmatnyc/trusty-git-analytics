# Collection Stage

Stage 1 of the pipeline: extract commits from Git repositories and correlate them with
external systems (GitHub PRs, JIRA tickets, identity resolution). All data is persisted to
`gitflow_cache.db` and `identities.db`.

## Git Extraction

### Library

- **`git2` crate** (libgit2 bindings) — replaces `GitPython` + subprocess model of the
  Python predecessor.
- Direct OID traversal, in-process diff computation, no subprocess overhead per commit.

### Repository Discovery

1. **Explicit**: `repositories[].path` in config (always used)
2. **Org discovery**: when `github.organization` is set, the GitHub Search API enumerates
   all matching repos and they are added to the working set (cloned to cache dir if missing)

### Branch Selection Strategies

Controlled by `analysis.branch_analysis.strategy`:

| Strategy | Behavior |
|----------|----------|
| `smart` (default) | `main`/`master`/`develop`/`dev` always included; matches `release/*` / `hotfix/*`; excludes `dependabot/*` / `renovate/*`; only branches with commits in last `active_days` (default 90) |
| `all` | All local + remote branches (subject to `max_branches`) |
| `main_only` | Default branch only |

Limits:
- `branch_commit_limit`: 1000 commits per branch
- `max_branches`: 50 per repo

### Per-Commit Data Extracted

| Field | Source |
|-------|--------|
| `commit_hash` | OID hex |
| `author_name`, `author_email` | `commit.author()` |
| `timestamp` | `commit.time()` (converted to UTC chrono::DateTime) |
| `iso_week` | `chrono::IsoWeek::format("%G-W%V")` |
| `branch` | Branch ref where commit was first observed |
| `is_merge` | `commit.parent_count() > 1` |
| `message` | `commit.message()` |
| `files_changed` | From `Diff::deltas()` |
| `lines_added` / `lines_deleted` | `DiffStats` |
| `filtered_insertions` / `filtered_deletions` | After applying `exclude_paths` glob match |
| `story_points` | Regex extraction from message |
| `ticket_references` | Platform-specific regex extraction |
| `ai_confidence_score`, `ai_detection_method` | Optional, from AI detection module |

### Path Exclusions

Glob patterns from `analysis.exclude_paths` are matched against each diff file path. Matched
files are excluded from `filtered_insertions`/`filtered_deletions` and from `files_changed`.

### Git Remote Fetch

- Library: `git2::Remote::fetch` with 30s timeout
- SSH: relies on system OpenSSH agent; `BatchMode=yes` for non-interactive
- HTTPS: token from `github.token` or env

### Ticket Reference Extraction

| Pattern type | Regex | Notes |
|--------------|-------|-------|
| JIRA | `[A-Z]{2,10}-\d+` | Excludes `CVE-\d+`, `CWE-\d+` |
| GitHub | `(?:closes\|fixes\|resolves)\s+#(\d+)` | Case-insensitive |
| Numeric exclusion | `\d{8,}` | Skip 8+ digit numbers (likely timestamps/hashes) |

`analysis.ticket_detection.commit_filter`:
- `all`: extract from every commit
- `squash_merges_only`: only commits with parent_count >= 2 and matching squash patterns
- `merge_commits`: only merge commits

---

## PR / MR Data Collection (GitHub)

- **Library**: `reqwest` (async, JSON, rustls-tls) + `tokio` runtime
- **Auth**: `Authorization: token <github.token>` header
- **GitLab support**: not yet implemented (parity gap with Python predecessor)
- **Pagination**: GitHub link-header driven, batched via `tokio::JoinSet`

### Incremental Fetch

For each repo, the most recent `pull_request_cache.cached_at` is queried and used as the
`since` parameter to `/repos/{owner}/{repo}/pulls?state=all&sort=updated&direction=desc`.

### Open PR TTL Refresh

Open PRs are re-fetched if `cached_at` is older than
`github.open_pr_refresh_ttl_hours` (default 1h). This keeps cycle-time and revision-count
metrics fresh without re-fetching closed PRs.

### Per-PR Data

| Field | Notes |
|-------|-------|
| `pr_number`, `title`, `description` | |
| `author` | GitHub login |
| `created_at`, `updated_at`, `merged_at`, `closed_at` | UTC datetimes |
| `pr_state` | `open` / `closed` / `merged` |
| `labels` | JSON array |
| `commit_hashes` | From `/pulls/{number}/commits` |
| `additions`, `deletions`, `changed_files` | From PR object |
| `approvals`, `change_requests` | From `/pulls/{number}/reviews` (only when `fetch_pr_reviews: true`) |
| `time_to_first_review_seconds` | First review timestamp − `created_at` |
| `revision_count` | Count of force-pushes / new commits after first review |

---

## JIRA Data Collection

- **Auth**: HTTP Basic Auth with `access_user` + `access_token`
- **Endpoint**: `/rest/api/3/search` with JQL
- **Batch size**: 50 tickets per request
- **JQL**: `project IN (KEY1, KEY2) AND updated >= "YYYY-MM-DD"`
- **Story point field discovery**: GET `/rest/api/3/field`, match by name patterns
  (default: `customfield_10016`)
- `issue_type` (e.g. `Bug`, `Story`, `Task`) cached for Tier 1.5 classifier

---

## Developer Identity Resolution

### Resolution Order

For each `(author_name, author_email)` observed in commits:

1. **Manual mapping** → `analysis.identity.manual_mappings`
2. **Exact email match** → `developer_aliases.email = ?`
3. **GitHub username match** (when `github_login` present in PR/issue data)
4. **Fuzzy match**:
   - Strip configured suffixes from email local-part
   - Compute `strsim::jaro_winkler` between names
   - Match if combined score ≥ `similarity_threshold` (default 0.85)
5. **Create new** canonical record (UUID v4)

### Storage

- `developer_identities`: canonical records
- `developer_aliases`: every observed `(name, email, source)` tuple
- Separate DB file (`identities.db`) for portability across cache rebuilds

---

## Caching

| Cache Type | Key | TTL | Invalidation |
|------------|-----|-----|--------------|
| Week-level fetch status | `(repo_path, iso_week)` | immutable after fetch | `--force` flag |
| Repository config hash | `repo_path` | until config changes | blake3(config inputs) |
| LLM classification results | `(commit_hash, model)` | 90 days (configurable) | reclassify flag |
| GitHub PR (open) | `pr_number` | 1h (configurable) | auto-refresh on stale read |
| GitHub PR (closed/merged) | `pr_number` | indefinite | manual cache clear |

### Concurrency

SQLite **WAL mode** (`PRAGMA journal_mode=WAL`) allows the classifier to read commits while
the collector continues writing — enabling pipelined `analyze` runs.

---

## Rust Parallelism Opportunities

| Operation | Mechanism |
|-----------|-----------|
| Multiple repositories | `rayon::par_iter` over `repositories[]` |
| Multiple branches per repo | `rayon::par_iter` over discovered branches |
| GitHub / JIRA HTTP calls | `tokio::JoinSet` with bounded concurrency (default 8 in-flight) |
| Commit batch writes | rusqlite transactions with `INSERT … ON CONFLICT` (50–200 commits per tx) |
| Ticket pattern matching | Single `aho_corasick::AhoCorasick` over message (one pass for all patterns) |
| Identity fuzzy match | `rayon::par_iter` over alias candidates, parallel strsim scoring |

### Bounded Concurrency

Use `tokio::sync::Semaphore` to cap concurrent HTTP requests at 8 per host to avoid
GitHub rate-limit triggering. Respect `X-RateLimit-Remaining` and back off when < 100.

---

## Error Handling

- `thiserror` enum `CollectError` per failure mode (`GitError`, `HttpError`, `DbError`, `ConfigError`)
- Per-repo errors are isolated: one failing repo does not abort the whole `tga collect` run
- Failed repos marked `status = "failed"` in `repository_analysis_status`
- Final exit code: non-zero only if **all** repos failed

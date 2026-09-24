# Classification Engine

The classification engine assigns a `change_type` (and optional remapped `work_type`) to
every commit. It runs as a four-tier cascade: the first tier to produce a result wins, with
a rule-based fallback ensuring every commit receives a classification.

## Four-Tier Cascade

### Tier 0: Manual Overrides (confidence: 1.0)

- Source: `classification_overrides` table
- Key: `(commit_hash, repo_path)`
- Set via `tga override --commit <HASH> --repo <PATH> --change-type <TYPE> --reason <...>`
- Always wins when present

### Tier 1.5: Issue Type Classifier (confidence: 0.90)

Applies when the commit has at least one `ticket_references` entry that resolves to an
`issue_cache` row. The cached `issue_type` is mapped via `ISSUETYPE_CHANGE_TYPE_MAP`:

| Issue Type (case-insensitive) | change_type |
|-------------------------------|-------------|
| `bug` / `defect` / `error` | `bugfix` |
| `story` / `feature` / `new feature` / `epic` / `improvement` / `enhancement` | `feature` |
| `task` / `sub-task` / `subtask` | `None` → check labels |
| `technical task` | `maintenance` |
| `tech debt` / `infrastructure` / `platform` | `platform` |
| `spike` | `research` |
| `documentation` | `documentation` |
| `test` | `test` |

**Task label disambiguation** (only when issue_type ∈ {task, sub-task, subtask}):

| Label substring | change_type |
|------------------|-------------|
| `platform` / `infra` / `tooling` | `platform` |
| `refactor` / `tech-debt` | `refactor` |
| `maintenance` / `chore` | `maintenance` |
| (default) | `maintenance` |

If the commit has multiple ticket references mapped to different change_types, the highest
confidence (alphabetical tiebreak) wins.

### Tier 3: JIRA Project Key Mapping (confidence: 0.95)

Applies when `jira_project_mappings` is set in config and the commit has ticket references
matching the JIRA pattern `[A-Z]+-\d+`.

- Project key extracted from regex, normalized uppercase
- Looked up in `jira_project_mappings: dict<string,string>`
- Multiple matches → first config-order match wins

### LLM Classification (confidence threshold: 0.7 configurable)

- **Providers**: `openrouter` (default), `bedrock`, `auto`
- **Default model**: `mistralai/mistral-7b-instruct`
- **Batch size**: 50 commits per request
- **Parameters**: `max_tokens=50`, `temperature=0.1`, `timeout=30s`
- **Circuit breaker**: after 3 consecutive batch failures, LLM disabled for run
- **Response cache**: 90-day TTL keyed by commit hash + model name
- Result accepted only when `confidence >= confidence_threshold`

### Rule-Based Fallback (always available)

Ordered first-match list. Patterns compiled into a single `aho-corasick` automaton at
startup for O(message_length) matching.

| Priority | change_type | Patterns (case-insensitive substrings / regex) |
|----------|-------------|------------------------------------------------|
| 1 | `maintenance` | `chore:`, `update deps`, `bump version` |
| 2 | `bugfix` | `revert`, `fix:`, `bug:`, `resolve`, `repair`, `correct` |
| 3 | `platform` | `platform:`, `infra:`, `devops:`, `tooling:`, `architect` |
| 4 | `feature` | `feat:`, `add feature`, `implement`, `introduce` |
| 5 | `refactor` | `refactor:`, `restructure`, `optimize`, `improve`, `clean up` |
| 6 | `documentation` | `docs:`, `documentation:`, `readme` |
| 7 | `test` | `test:`, `spec:`, `add test` |
| 8 | `style` | `style:`, `format:`, `lint`, `prettier`, `whitespace` |

Default: `maintenance`.

---

## Categories

**Merge rule (#111):** a commit with 2+ parents is a merge, and it is excluded from metrics and from the eval. Squash and rebase commits (1 parent) are normal commits, classified by content.

tga records `commits.is_merge` (`parent_count() > 1`) at collection and keeps
merges in the database. The classifier may still label a merge `merge`, but
reports, per-author drill-downs, period trends and `tga eval` leave merges out.

### Built-in default categories

Without a rules file, the built-in ruleset and taxonomy
(`src/classify/taxonomy.rs::built_in_defs`) map each subcategory to one of
eight top-level categories:

| Top level | Built-in subcategories |
|-----------|------------------------|
| `feature` | `feature`, `enhancement`, `new-feature`, `new_feature`, `breaking`, `experiment`, `spike`, `prototype` |
| `bugfix` | `bugfix`, `bug`, `bug_fix`, `hotfix`, `security` |
| `ktlo` | `ci`, `build`, `ops`, `release` |
| `integrations` | `integration`, `integrations`, `api`, `webhook` |
| `platform_work` | `infra`, `platform`, `performance`, `perf`, `architecture`, `devops`, `cloud`, `monitoring`, `observability`, `database`, `messaging`, `networking`, `storage` |
| `content` | `docs`, `documentation`, `content`, `localization`, `content-docs`, `translation`, `assets` |
| `maintenance` | `refactor`, `test`, `tests`, `style`, `cleanup`, `maintenance`, `deps`, `dependencies`, `revert`, `merge`, `chore`, `tech_debt_refactoring`, `rollback`, `config`, `tooling` |
| `unknown` | `wip`, `uncategorized` |

### Category sets are config-driven

A rules file (`classification.rules_files`, alias `rules_file`) defines the
categories its rules emit; with `extend_defaults: false` it replaces the
built-in ruleset. `classification.custom_categories` adds or overrides
taxonomy entries. tga hardcodes no deployment's category list.

### Eval scheme v2 (#111)

The #111 precision eval uses scheme v2 through the cto-reports rules file:
`security`, `devops`, `qa`, `bug_fix`, `new_feature`, `internal_tooling`,
`integration`, `platform_infrastructure`, `maintenance`, `data_science`, plus
the rater labels `release_merge`, `unclear` and `mixed`. `tga eval score
--config` accepts every category the config's rules or taxonomy name.
`unclear`, `mixed` and `release_merge` are always valid, are counted per
label, and score as no answer. See `docs/eval-harness.md`.

---

## Work Type Taxonomy Mapping

When `taxonomy_mapping` is configured, a post-classification SQL UPDATE pass remaps
`change_type` → `work_type`:

```sql
UPDATE qualitative_commits
SET work_type = COALESCE(
    (SELECT mapped_value FROM taxonomy_mapping WHERE source_change_type = change_type),
    change_type
);
```

When no mapping exists for a given `change_type`, `work_type` falls back to `change_type`.

---

## Coverage Metrics

```
coverage_pct = 100 * (commits with a category other than uncategorized) / total_commits
```

- Computed per-repo, stored in `repository_analysis_status.classification_coverage_pct`
- Warning emitted at `coverage < 20%` (configurable via `--coverage-threshold`)
- `--validate-coverage` flag causes `tga classify` to exit non-zero when below threshold

---

## Rust Implementation Notes

| Concern | Approach |
|---------|----------|
| Tier 0 lookup | `SELECT FROM classification_overrides WHERE commit_hash = ? AND repo_path = ?` (rusqlite prepared statement, single connection per worker) |
| Tier 1.5 map | `HashMap<String, Option<ChangeType>>` compiled once at startup from `ISSUETYPE_CHANGE_TYPE_MAP` constant |
| Tier 3 map | `HashMap<String, WorkType>` deserialized from config |
| Rule patterns | Single `aho_corasick::AhoCorasick` automaton per tier; lookup tier → change_type via match index |
| LLM dispatch | `async fn` returning `Result<Vec<LlmResult>>`; `tokio::select!` for concurrent batches with timeout |
| Cascade dispatcher | `rayon::par_iter` over commit batches; each batch invokes async LLM via `tokio::runtime::Handle::block_on` from a dedicated executor pool |
| Result type | `struct ClassificationResult { change_type: ChangeType, work_type: WorkType, confidence: f32, tier: ClassificationTier }` |
| Tier enum | `enum ClassificationTier { Override, IssueType, JiraMapping, Llm, RuleBased }` |
| ChangeType enum | 19 variants, `#[derive(Serialize, Deserialize, sqlx::Type)]` (or rusqlite `ToSql`/`FromSql`) |

---

## Diagnostics

`tga classify --show-jira-signals` emits per-commit diagnostics:

```
abc1234 → ticket_refs=[API-123, PLAT-456] | issue_types=[bug, task] | tier=issue_type | result=bugfix(0.90)
```

This is the canonical diagnostic for understanding why a given commit landed in a given
tier and helps validate `jira_project_mappings` / `ISSUETYPE_CHANGE_TYPE_MAP` correctness.

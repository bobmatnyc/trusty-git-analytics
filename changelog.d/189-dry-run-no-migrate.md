Fixed
- `--dry-run` no longer changes the database file (#189). Before, every command opened the database before it read `--dry-run`, and that open created a missing file, switched it to WAL and applied every pending migration: `tga backfill revert-flags --dry-run` moved a schema-30 database to 32. Now:
  - `tga backfill <any> --dry-run`, `tga linear sync --dry-run`, `tga jira sync --dry-run` and `tga profile --dry-run` open the database read-only. A missing file is an error and is not created. A schema older than the binary exits non-zero and names the pending migrations, for example `pending migrations: v31 (llm_usage), v32 (llm_usage_text_mode)`; run the command without `--dry-run` to apply them.
  - `tga collect --dry-run` and `tga analyze --dry-run` no longer open the database file at all; they already ran on an in-memory copy.
  - A run without `--dry-run` is unchanged: it still opens in WAL mode and applies pending migrations.

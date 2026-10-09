-- Migration v33: full Linear issue fields, archived state, and a stable key
-- (issue #190, step 1).
--
-- `linear_issues` was keyed by `identifier` (ENG-123), which Linear changes
-- when an issue moves to another team, so a moved issue arrived as a second
-- row. Linear's stable key is the issue `id` (a UUID). This migration adds it
-- as `linear_id` with a UNIQUE index; `identifier` keeps its existing UNIQUE
-- index and stays queryable.
--
-- Upgrade strategy for existing rows, additive only:
--   * No table rebuild, no column change, no row removed. Every existing row
--     keeps its integer `id`, `identifier` and every v2/v30 column.
--   * Existing rows get `linear_id = NULL` and `archived = 0`. SQLite allows
--     any number of NULLs under a UNIQUE index, so the index builds over
--     them.
--   * The next sync that returns such an issue fills the row in place: the
--     writer (`collect::linear::store`) looks a row up by `linear_id` first,
--     then by `identifier` among rows whose `linear_id` is still NULL, and
--     updates that row. The row is never deleted and re-inserted.
--   * A legacy row whose issue moved team BEFORE its first sync under v33
--     cannot be matched (its stored identifier is the old one, and the row
--     holds no UUID). It stays as written, identifiable by
--     `linear_id IS NULL`.
--
-- New columns mirror the Linear GraphQL `Issue` fields the bulk sync reads.
-- `label_ids` and `label_names` hold JSON arrays. `raw_json` holds the issue
-- node exactly as the API returned it.

ALTER TABLE linear_issues ADD COLUMN linear_id TEXT;
ALTER TABLE linear_issues ADD COLUMN state_type TEXT;
ALTER TABLE linear_issues ADD COLUMN team_id TEXT;
ALTER TABLE linear_issues ADD COLUMN estimate REAL;
ALTER TABLE linear_issues ADD COLUMN project_id TEXT;
ALTER TABLE linear_issues ADD COLUMN cycle_id TEXT;
ALTER TABLE linear_issues ADD COLUMN parent_id TEXT;
ALTER TABLE linear_issues ADD COLUMN label_ids TEXT;
ALTER TABLE linear_issues ADD COLUMN label_names TEXT;
ALTER TABLE linear_issues ADD COLUMN due_date TEXT;
ALTER TABLE linear_issues ADD COLUMN assignee_id TEXT;
ALTER TABLE linear_issues ADD COLUMN assignee_name TEXT;
ALTER TABLE linear_issues ADD COLUMN assignee_email TEXT;
ALTER TABLE linear_issues ADD COLUMN creator_id TEXT;
ALTER TABLE linear_issues ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;
ALTER TABLE linear_issues ADD COLUMN archived_at TEXT;
ALTER TABLE linear_issues ADD COLUMN raw_json TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_linear_issues_linear_id
    ON linear_issues(linear_id);
CREATE INDEX IF NOT EXISTS idx_linear_issues_state_type
    ON linear_issues(state_type);

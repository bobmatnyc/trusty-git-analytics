-- Migration v35: a stable key on `work_items`, and the field-set version on
-- `linear_sync_cursor` (issue #190, step 3).
--
-- `work_items.id` for a Linear row is the issue identifier (ENG-123), which
-- Linear changes when an issue moves team, so a moved issue arrived as a
-- second row and its commit links stayed on the stale one. `id` stays the
-- identifier, because commit messages name it and `commit_work_items`,
-- `collect::correlate` and the effort tier's parent count all join on it.
-- `stable_id` holds the id that does not change (Linear's issue UUID); the
-- writer (`collect::linear::projection`) finds a moved issue's row by it and
-- renames that row, moving its commit links and fact rows with it.
--
-- Upgrade strategy for existing rows, additive only:
--   * No table rebuild, no column change, no row removed.
--   * A Linear row whose `id` is the identifier of a `linear_issues` row that
--     carries a `linear_id` gets that `linear_id` as its `stable_id`. Both
--     `linear_issues.identifier` and `linear_issues.linear_id` are UNIQUE, so
--     no two rows can receive the same key.
--   * Every other Linear row — written before v33, or a stale copy an issue
--     moved away from under v33/v34 — keeps `stable_id = NULL`. The next sync
--     that returns its issue merges it: the issue's `previousIdentifiers`
--     name the old identifier.
--   * Rows of every other source keep `stable_id = NULL`; nothing reads it
--     for them.
--
-- `linear_sync_cursor.fields_version` is the issue field set a team was last
-- read in full under (`collect::linear::issue::ISSUE_FIELDS_VERSION`).
-- Existing cursors get 0, so the next `tga linear sync` of each team reads
-- its whole history once and every stored issue gets the fields this
-- version added; later runs resume from the cursor as before.

ALTER TABLE work_items ADD COLUMN stable_id TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS idx_work_items_source_stable_id
    ON work_items(source, stable_id) WHERE stable_id IS NOT NULL;

UPDATE work_items
   SET stable_id = (
       SELECT li.linear_id FROM linear_issues li
        WHERE li.identifier = work_items.id AND li.linear_id IS NOT NULL
   )
 WHERE source = 'linear';

ALTER TABLE linear_sync_cursor ADD COLUMN fields_version INTEGER NOT NULL DEFAULT 0;

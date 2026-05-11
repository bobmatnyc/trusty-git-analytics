-- Add `ticketed` flag to commits.
--
-- A commit is ticketed when its message references a known ticket system
-- (JIRA/Linear-style `PROJ-123`, GitHub `fixes #123`, or bare `#123`). The
-- flag is computed at extraction time and stored as 0/1.
--
-- Existing rows default to 0 (unticketed); a re-collection will overwrite
-- with the correct value because `INSERT OR IGNORE` preserves the existing
-- row only when the `sha` already exists. To backfill in place without a
-- re-collection, run the `tga reclassify` / re-collect path against the
-- same repository set.

ALTER TABLE commits ADD COLUMN ticketed INTEGER NOT NULL DEFAULT 0;

CREATE INDEX IF NOT EXISTS idx_commits_ticketed ON commits (ticketed);

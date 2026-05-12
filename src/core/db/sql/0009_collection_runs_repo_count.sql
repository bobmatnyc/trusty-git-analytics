-- Issue #69: record how many repositories were in scope at the time of each
-- per-(repo, ISO-week) collection. Without this, week-over-week deltas can
-- be silently wrong when the prior week was collected with a different
-- repository roster than the current week.
--
-- `repo_count` is the size of `repositories[]` at the moment the row was
-- written. Existing rows default to 0, which downstream comparisons treat
-- as "unknown coverage" (no warning fires).

ALTER TABLE collection_runs ADD COLUMN repo_count INTEGER NOT NULL DEFAULT 0;

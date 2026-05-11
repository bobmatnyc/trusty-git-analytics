-- Per-(repo, ISO-week) collection bookkeeping.
--
-- One row per successfully collected (repository, iso_year, iso_week) tuple.
-- The presence of a row is the signal used by the per-week backfill
-- iterator to skip already-collected weeks unless `--force` is supplied.

CREATE TABLE IF NOT EXISTS collection_runs (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    repo_name      TEXT    NOT NULL,
    iso_year       INTEGER NOT NULL,
    iso_week       INTEGER NOT NULL,
    collected_at   TEXT    NOT NULL,
    commit_count   INTEGER NOT NULL DEFAULT 0,
    UNIQUE (repo_name, iso_year, iso_week)
);

CREATE INDEX IF NOT EXISTS idx_collection_runs_repo_week
    ON collection_runs (repo_name, iso_year, iso_week);

-- Azure DevOps iterations (sprints) — Phase 4.
--
-- One row per iteration returned by
-- `GET {org}/{project}/_apis/work/teamsettings/iterations`. Iteration IDs
-- are GUIDs and globally unique, so `id` alone serves as the primary key;
-- `project` is denormalized for fast per-project listing.

CREATE TABLE IF NOT EXISTS azdo_iterations (
    id           TEXT PRIMARY KEY,
    project      TEXT NOT NULL,
    name         TEXT NOT NULL,
    path         TEXT,
    start_date   TEXT,
    finish_date  TEXT,
    time_frame   TEXT,
    fetched_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_azdo_iterations_project
    ON azdo_iterations(project);

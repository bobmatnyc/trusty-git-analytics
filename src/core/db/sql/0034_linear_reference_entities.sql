-- Migration v34: Linear reference entities (issue #190, step 2).
--
-- `tga linear sync --entities` stores the workspace objects an issue points
-- at: teams, users, labels, projects, project milestones and cycles. Each
-- table is keyed by Linear's own `id` (a UUID), so a re-sync upserts in place
-- and the row count stays stable. `raw_json` holds the GraphQL node exactly
-- as the API returned it; the named columns are the fields reports read.
-- `archived` is 1 when the node carries an `archivedAt`.
--
-- `removed_at` is set when a complete fetch of the set no longer returns the
-- row: Linear purged it, deleted it, or the key lost access to its team. The
-- row stays, so older issue rows still resolve its name; current-entity
-- queries add `WHERE removed_at IS NULL`. It is cleared when the row returns.
--
-- Additive only: six new tables and one bookkeeping table. No existing table
-- or row changes.
--
-- Field selections live in `collect::linear::entities::queries`. No
-- selection asks for a `description`, so `raw_json` carries no long free text.

CREATE TABLE IF NOT EXISTS linear_teams (
    id                          TEXT PRIMARY KEY,
    key                         TEXT,
    name                        TEXT,
    private                     INTEGER,
    timezone                    TEXT,
    issue_estimation_type       TEXT,     -- notUsed | exponential | fibonacci | linear | tShirt
    issue_estimation_allow_zero INTEGER,
    issue_estimation_extended   INTEGER,
    default_issue_estimate      REAL,
    cycles_enabled              INTEGER,
    cycle_duration              REAL,     -- weeks
    cycle_cooldown_time         REAL,     -- weeks
    cycle_start_day             REAL,     -- 0 = Sunday
    upcoming_cycle_count        REAL,
    created_at                  TEXT,
    updated_at                  TEXT,
    archived_at                 TEXT,
    archived                    INTEGER NOT NULL DEFAULT 0,
    raw_json                    TEXT NOT NULL,
    fetched_at                  TEXT NOT NULL,
    removed_at                  TEXT
);
CREATE INDEX IF NOT EXISTS idx_linear_teams_key ON linear_teams(key);

CREATE TABLE IF NOT EXISTS linear_users (
    id           TEXT PRIMARY KEY,
    name         TEXT,
    display_name TEXT,
    email        TEXT,
    active       INTEGER,
    admin        INTEGER,
    guest        INTEGER,
    created_at   TEXT,
    updated_at   TEXT,
    archived_at  TEXT,
    archived     INTEGER NOT NULL DEFAULT 0,
    raw_json     TEXT NOT NULL,
    fetched_at   TEXT NOT NULL,
    removed_at   TEXT
);
CREATE INDEX IF NOT EXISTS idx_linear_users_email ON linear_users(email);

-- `team_id` NULL is a workspace-level label. `is_group` 1 is a label group;
-- its children carry its id in `parent_id`.
CREATE TABLE IF NOT EXISTS linear_labels (
    id          TEXT PRIMARY KEY,
    name        TEXT,
    color       TEXT,
    is_group    INTEGER,
    parent_id   TEXT,
    team_id     TEXT,
    created_at  TEXT,
    updated_at  TEXT,
    archived_at TEXT,
    archived    INTEGER NOT NULL DEFAULT 0,
    raw_json    TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    removed_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_linear_labels_team ON linear_labels(team_id);

-- `state` is Linear's legacy project state string; `status_name` and
-- `status_type` are the custom project status that replaces it. `team_ids`
-- is a JSON array of team ids.
CREATE TABLE IF NOT EXISTS linear_projects (
    id           TEXT PRIMARY KEY,
    name         TEXT,
    slug_id      TEXT,
    url          TEXT,
    state        TEXT,
    status_name  TEXT,
    status_type  TEXT,
    progress     REAL,
    start_date   TEXT,     -- YYYY-MM-DD
    target_date  TEXT,     -- YYYY-MM-DD
    started_at   TEXT,
    completed_at TEXT,
    canceled_at  TEXT,
    lead_id      TEXT,
    team_ids     TEXT,
    created_at   TEXT,
    updated_at   TEXT,
    archived_at  TEXT,
    archived     INTEGER NOT NULL DEFAULT 0,
    raw_json     TEXT NOT NULL,
    fetched_at   TEXT NOT NULL,
    removed_at   TEXT
);

CREATE TABLE IF NOT EXISTS linear_milestones (
    id          TEXT PRIMARY KEY,
    project_id  TEXT,
    name        TEXT,
    target_date TEXT,      -- YYYY-MM-DD
    sort_order  REAL,
    created_at  TEXT,
    updated_at  TEXT,
    archived_at TEXT,
    archived    INTEGER NOT NULL DEFAULT 0,
    raw_json    TEXT NOT NULL,
    fetched_at  TEXT NOT NULL,
    removed_at  TEXT
);
CREATE INDEX IF NOT EXISTS idx_linear_milestones_project ON linear_milestones(project_id);

-- The four counts are the last element of Linear's daily history arrays
-- (`issueCountHistory`, `completedIssueCountHistory`, `scopeHistory`,
-- `completedScopeHistory`); the full arrays stay in `raw_json`. `is_empty` is
-- 1 when the latest issue count is 0, 0 when it is above 0, and NULL when the
-- cycle has no history yet (it has not started).
CREATE TABLE IF NOT EXISTS linear_cycles (
    id                 TEXT PRIMARY KEY,
    team_id            TEXT,
    number             INTEGER,
    name               TEXT,
    starts_at          TEXT,
    ends_at            TEXT,
    completed_at       TEXT,
    progress           REAL,
    scope_count        INTEGER,
    completed_count    INTEGER,
    scope_estimate     REAL,
    completed_estimate REAL,
    is_empty           INTEGER,
    created_at         TEXT,
    updated_at         TEXT,
    archived_at        TEXT,
    archived           INTEGER NOT NULL DEFAULT 0,
    raw_json           TEXT NOT NULL,
    fetched_at         TEXT NOT NULL,
    removed_at         TEXT
);
CREATE INDEX IF NOT EXISTS idx_linear_cycles_team ON linear_cycles(team_id);

-- One row per entity set. Every entity sync is a full refresh; this row
-- records the last successful one, written in the same transaction as the
-- entity's rows. `max_updated_at` is the newest `updatedAt` stored so far and
-- never moves backward.
CREATE TABLE IF NOT EXISTS linear_entity_sync_state (
    entity         TEXT PRIMARY KEY,
    max_updated_at TEXT,
    last_run_at    TEXT NOT NULL,
    rows_synced    INTEGER NOT NULL
);

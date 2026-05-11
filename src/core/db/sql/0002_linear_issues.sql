-- Linear issue persistence.
--
-- Stores the issues fetched from the Linear GraphQL API during the
-- collection stage so they can be joined against commits and used in
-- reporting/analysis without re-querying the Linear API.
--
-- The `identifier` column (e.g. "ENG-123") is the natural key. Re-running
-- collection upserts on `identifier` via `INSERT OR REPLACE`.

CREATE TABLE IF NOT EXISTS linear_issues (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    identifier      TEXT NOT NULL UNIQUE,
    title           TEXT NOT NULL,
    state           TEXT NOT NULL,
    team            TEXT NOT NULL,
    team_key        TEXT NOT NULL,
    assignee        TEXT,
    priority        INTEGER NOT NULL DEFAULT 0,
    url             TEXT NOT NULL DEFAULT '',
    fetched_at      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_linear_issues_team_key
    ON linear_issues(team_key);
CREATE INDEX IF NOT EXISTS idx_linear_issues_state
    ON linear_issues(state);
CREATE INDEX IF NOT EXISTS idx_linear_issues_assignee
    ON linear_issues(assignee);

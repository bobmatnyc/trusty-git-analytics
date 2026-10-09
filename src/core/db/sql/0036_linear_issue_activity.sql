-- Migration v36: Linear issue history and comments (issue #190, step 6).
--
-- `tga linear sync --history --comments` stores the Linear counterparts of
-- the JIRA fact tables from migration v23:
--
--   fact_linear_transitions    -- one row per workflow-state change
--                                 (`Issue.history` entry with a `toState`)
--   fact_linear_comment_detail -- one row per comment, metadata only
--
-- Design decisions:
--   * Both tables are keyed by Linear's own id (a UUID), so a re-read
--     rewrites a row in place. `issue_id` is the issue's `linear_id`, the key
--     that survives a team move; `identifier` and `team_key` are the issue's
--     values when its activity was last read, denormalized for filtering.
--   * Each issue's rows are replaced as a set: the writer deletes them and
--     inserts the complete list Linear returned, so a deleted comment
--     leaves no row behind. The per-issue walk fails rather than return a
--     cut list.
--   * `from_state` is NULL for the entry that set the issue's first state.
--   * `body_len` is the comment body's length in characters. The body
--     itself is never stored.
--   * `synced_at` (unix seconds) mirrors the JIRA tables.
--   * `linear_issue_activity_state` records, per issue and connection, the
--     issue `updated_at` its activity was last read at. An issue is read
--     again when its stored `updated_at` differs. The marker is written in
--     the same transaction as the rows, so a failed read leaves the issue
--     due no matter how far the team's issue cursor has moved.
--
-- Additive migration: three new tables. No existing table or row changes.

CREATE TABLE IF NOT EXISTS fact_linear_transitions (
    history_id      TEXT PRIMARY KEY,
    issue_id        TEXT NOT NULL,
    identifier      TEXT NOT NULL,
    team_key        TEXT NOT NULL,
    from_state      TEXT,
    from_state_type TEXT,
    to_state        TEXT NOT NULL,
    to_state_type   TEXT,
    actor_id        TEXT,             -- NULL for a change made by an integration
    actor_name      TEXT,
    transitioned_at TEXT NOT NULL,    -- RFC3339: the history entry's createdAt
    synced_at       INTEGER NOT NULL  -- unix seconds: when tga wrote this row
);

CREATE INDEX IF NOT EXISTS idx_fact_linear_transitions_issue
    ON fact_linear_transitions(issue_id);
CREATE INDEX IF NOT EXISTS idx_fact_linear_transitions_team
    ON fact_linear_transitions(team_key);

CREATE TABLE IF NOT EXISTS fact_linear_comment_detail (
    comment_id  TEXT PRIMARY KEY,
    issue_id    TEXT NOT NULL,
    identifier  TEXT NOT NULL,
    team_key    TEXT NOT NULL,
    parent_id   TEXT,                 -- the comment this one replies to
    author_id   TEXT,                 -- NULL for a comment posted by an integration
    author_name TEXT,
    created_at  TEXT NOT NULL,        -- RFC3339
    updated_at  TEXT,                 -- RFC3339
    body_len    INTEGER NOT NULL,
    synced_at   INTEGER NOT NULL      -- unix seconds: when tga wrote this row
);

CREATE INDEX IF NOT EXISTS idx_fact_linear_comment_detail_issue
    ON fact_linear_comment_detail(issue_id);
CREATE INDEX IF NOT EXISTS idx_fact_linear_comment_detail_team
    ON fact_linear_comment_detail(team_key);

CREATE TABLE IF NOT EXISTS linear_issue_activity_state (
    issue_id           TEXT PRIMARY KEY,
    history_for        TEXT,          -- issue updated_at the history was read at
    history_synced_at  TEXT,          -- RFC3339 wall clock of that read
    comments_for       TEXT,          -- issue updated_at the comments were read at
    comments_synced_at TEXT           -- RFC3339 wall clock of that read
);

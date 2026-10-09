-- Migration v37: the per-team cursor of the incremental Linear comments
-- walk (issue #190).
--
-- Why: v36 re-read an issue's comments only when the issue's `updatedAt`
-- moved. Live Linear data shows `Issue.updatedAt` does not reliably move
-- when a comment is created or edited, so an incremental
-- `tga linear sync --comments` missed new and edited comments. The
-- incremental pass now reads Linear's `comments` connection filtered by the
-- comment's own `updatedAt`, one walk per team, from this cursor.
--
-- Design decisions:
--   * One row per team key, mirroring `linear_sync_cursor`. A team-scoped
--     `--team` run and an `--all-teams` run share the row of each team.
--   * `cursor_updated_at` is the newest comment `updatedAt` a completed walk
--     stored, never later than the walk's start. The next walk asks for
--     comments updated after it minus a 10-minute overlap; the writer upserts
--     by comment id, so the overlap re-reads write no second row.
--   * The row is written in the same transaction as the walk's comment rows,
--     after every page has arrived. A failed page, transport error, rate
--     limit or malformed page leaves it unchanged. It never moves backward.
--   * No row means the next walk reads the team's whole comment set.
--   * `fact_linear_comment_detail` and `linear_issue_activity_state` are
--     unchanged; `--backfill` still walks each issue and writes the
--     `comments_for` marker.
--
-- Additive migration: one new table. No existing table or row changes.

CREATE TABLE IF NOT EXISTS linear_comment_cursor (
    team_key          TEXT PRIMARY KEY,
    cursor_updated_at TEXT NOT NULL,    -- RFC3339: newest comment updatedAt stored
    last_run_at       TEXT NOT NULL,    -- RFC3339 wall clock of the walk's commit
    comments_synced   INTEGER NOT NULL  -- comment rows that walk wrote
);

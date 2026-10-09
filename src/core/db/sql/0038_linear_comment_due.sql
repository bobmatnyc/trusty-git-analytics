-- Migration v38: issues whose whole comment set the next incremental Linear
-- comments walk must read (issue #190).
--
-- Why: the incremental `--comments` walk (v37) reads a team's comments by
-- the comment's own `updatedAt`, filtered on the issue's current team. Moving
-- an issue into a team does not change its comments' `updatedAt`, so a
-- comment written while the issue sat in another team, older than the
-- team's cursor minus the overlap, matches no incremental walk.
--
-- Design decisions:
--   * The issue sync writes a row, in the same transaction as the issue
--     rows, for each issue that moved into the team and each issue new to
--     the team whose `createdAt` is at or before the team's comment cursor
--     minus the overlap. A team with no comment cursor gets no row: its
--     next comments walk reads every comment of the team.
--   * The row is written whether or not the run passes `--comments`, so a
--     later `--comments` run, whose issue pass sees the issue unchanged,
--     still reads it.
--   * The next incremental comments pass for `team_key` reads each listed
--     issue's comments with no `updatedAt` bound and deletes the rows in the
--     same transaction as the team's comment rows and cursor. A failed walk
--     leaves them. A `--backfill` per-issue comments write deletes the
--     issue's row too.
--
-- Additive migration: one new table. No existing table or row changes.

CREATE TABLE IF NOT EXISTS linear_comment_due (
    issue_id  TEXT PRIMARY KEY,   -- Linear issue id (UUID)
    team_key  TEXT NOT NULL,      -- the team the issue moved into
    reason    TEXT NOT NULL,      -- 'moved' or 'new'
    queued_at TEXT NOT NULL       -- RFC3339 wall clock of the issue sync
);

CREATE INDEX IF NOT EXISTS idx_linear_comment_due_team
    ON linear_comment_due (team_key);

-- PR metrics and backfill support.
--
-- Adds columns to `commits` consumed by the `tga backfill` subcommands:
--   * `is_revert`  — flagged when the message starts with `Revert ` (or
--                    matches `git revert`-style auto-generated subjects).
--   * `ticket_id`  — first ticket reference extracted from the message
--                    (JIRA/Linear `PROJ-123`, GitHub `#123`, ADO `AB#123`).
--
-- Both default to NULL/0 for existing rows; `tga backfill revert-flags` and
-- `tga backfill ticket-ids` populate them after the fact.

ALTER TABLE commits ADD COLUMN is_revert INTEGER NOT NULL DEFAULT 0;
ALTER TABLE commits ADD COLUMN ticket_id TEXT;

CREATE INDEX IF NOT EXISTS idx_commits_is_revert ON commits(is_revert);
CREATE INDEX IF NOT EXISTS idx_commits_ticket_id ON commits(ticket_id);

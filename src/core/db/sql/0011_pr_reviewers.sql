-- Migration v11: add pr_reviewers table for Azure DevOps PR reviewer tracking
--
-- Issue #84: ADO PR fetcher records reviewer votes for review-pattern analytics.
-- The provider column allows the same table to record reviewers from other
-- PR providers (e.g. GitHub review-requests) in the future; default is 'azdo'
-- because this is the first integration to populate it.
--
-- ADO vote values:
--   10 = approved
--    5 = approved-with-suggestions
--    0 = no-vote
--   -5 = waiting-for-author
--  -10 = rejected
CREATE TABLE IF NOT EXISTS pr_reviewers (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    pr_id         INTEGER NOT NULL,
    provider      TEXT    NOT NULL DEFAULT 'azdo',
    reviewer_id   TEXT    NOT NULL,
    display_name  TEXT,
    vote          INTEGER NOT NULL DEFAULT 0,
    is_required   BOOLEAN NOT NULL DEFAULT 0,
    is_container  BOOLEAN NOT NULL DEFAULT 0,
    FOREIGN KEY (pr_id) REFERENCES pull_requests(id) ON DELETE CASCADE,
    UNIQUE(pr_id, provider, reviewer_id)
);

CREATE INDEX IF NOT EXISTS idx_pr_reviewers_pr_id ON pr_reviewers(pr_id);

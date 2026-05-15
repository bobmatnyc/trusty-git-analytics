-- Migration v10: add provider column and unique constraint to pull_requests
--
-- Bug #71: idx_pull_requests_pr_number was a non-UNIQUE index, so SQLite's
-- `INSERT OR REPLACE` never fired on `(pr_number)` and PRs silently
-- accumulated on every re-run of `tga collect`. We add a `provider` column
-- (default 'github') and a UNIQUE index on `(provider, pr_number)` so that
-- OR REPLACE correctly deduplicates per provider.
ALTER TABLE pull_requests ADD COLUMN provider TEXT NOT NULL DEFAULT 'github';
CREATE UNIQUE INDEX idx_pull_requests_provider_pr_number
    ON pull_requests(provider, pr_number);

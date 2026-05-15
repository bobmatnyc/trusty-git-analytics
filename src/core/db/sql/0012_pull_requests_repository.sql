-- Migration v12: add `repository` column to pull_requests and fix UNIQUE constraint.
--
-- Bug #88: After v1.0.8 added org-wide / multi-repo PR fetching (#87), the
-- UNIQUE(provider, pr_number) index from migration v10 became a data-loss
-- hazard. GitHub assigns pr_number per-repository (so two different repos
-- both have a PR #1), and `INSERT OR REPLACE INTO pull_requests` was
-- silently overwriting cross-repo collisions. In a five-repo collection
-- run, 1,737 fetched PRs collapsed into 663 stored rows (~62% loss).
--
-- Fix: add a `repository` column (NOT NULL, default 'unknown' so existing
-- rows are preserved) and rebuild the unique index as
-- UNIQUE(provider, repository, pr_number). Also add a non-unique index on
-- (provider, repository) for per-repo aggregates.
--
-- SQLite supports `ALTER TABLE ADD COLUMN` with a NOT NULL + DEFAULT
-- clause, and DROP/CREATE INDEX, so this migration does not require a
-- table rewrite.
ALTER TABLE pull_requests ADD COLUMN repository TEXT NOT NULL DEFAULT 'unknown';

DROP INDEX IF EXISTS idx_pull_requests_provider_pr_number;

CREATE UNIQUE INDEX idx_pull_requests_provider_repo_pr_number
    ON pull_requests(provider, repository, pr_number);

CREATE INDEX IF NOT EXISTS idx_pull_requests_provider_repository
    ON pull_requests(provider, repository);

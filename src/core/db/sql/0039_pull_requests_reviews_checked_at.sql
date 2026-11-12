-- Migration v39: per-PR "reviews checked at" marker for the GitHub reviewer
-- pass (issue #245).
--
-- Why: the reviewer pass selected every GitHub PR with no `pr_reviewers` row.
-- A PR with zero reviews never gets a row, so it was re-fetched on every run,
-- oldest first, until the rate budget ran out and newer PRs never got their
-- reviews. A PR that already had a row was never re-read, so reviews added
-- later were lost.
--
-- Design decisions:
--   * `updated_at` is the provider's own PR `updated_at` (RFC3339), written by
--     the GitHub PR listing. GitHub bumps it when a review is submitted.
--     NULL for rows written before v39 and for providers that do not set it.
--   * `reviews_checked_at` is written by the reviewer pass only for a PR whose
--     reviews it actually fetched and stored: the PR's `updated_at` at that
--     moment, or the wall clock when `updated_at` is NULL. A rate-limited or
--     failed fetch leaves it unchanged, so the PR stays selectable.
--   * The pass selects a PR when `reviews_checked_at` is NULL or `updated_at`
--     is later than it, newest first, bounded by `github.review_fetch_max_prs`.
--   * Backfill: a GitHub PR that already has reviewer rows was handled by the
--     old forward-only pass, so its marker starts at its `fetched_at`. That
--     keeps the upgrade from re-fetching every reviewed PR at once; the next
--     listing writes `updated_at`, and a PR updated after that fetch is
--     re-read. Rows with no reviewer rows keep a NULL marker and are selected.
--
-- Additive migration: two new nullable columns, one index, and a backfill of
-- the new marker column only. No existing column or row value changes.

ALTER TABLE pull_requests ADD COLUMN updated_at TEXT;
ALTER TABLE pull_requests ADD COLUMN reviews_checked_at TEXT;

UPDATE pull_requests
   SET reviews_checked_at = fetched_at
 WHERE provider = 'github'
   AND fetched_at != ''
   AND EXISTS (
       SELECT 1 FROM pr_reviewers r
        WHERE r.pr_id = pull_requests.id AND r.provider = 'github'
   );

CREATE INDEX IF NOT EXISTS idx_pull_requests_reviews_checked
    ON pull_requests (provider, reviews_checked_at);

-- Migration v41: Linear issue attachments and PR-to-work-item links (#272).
--
-- Why: Linear's GitHub integration attaches the pull requests that implement
-- an issue. Those attachments link a PR to its issue even when no key appears
-- in the branch, title, body or commits.
--
-- Design decisions:
--   * `linear_issue_attachments` holds one row per (issue, url). `issue_id`
--     is Linear's stable issue id (`linear_issues.linear_id`), so a team move
--     keeps its rows. A store replaces an issue's rows in one transaction, so
--     a re-sync is idempotent and a removed attachment disappears.
--   * `pr_work_items` holds one row per piece of evidence: the same PR and
--     work item can appear once per `source`. `repository` uses the spelling
--     of the stored `pull_requests.repository` row (#266), never the spelling
--     in an attachment URL. Rows are written with INSERT OR IGNORE, so the
--     correlation pass can re-run without duplicates.
--   * `source` is one of 'attachment', 'branch', 'title', 'body', 'commit'.
--   * Additive only: no existing table changes.
CREATE TABLE IF NOT EXISTS linear_issue_attachments (
    issue_id    TEXT NOT NULL,
    url         TEXT NOT NULL,
    source_type TEXT,
    title       TEXT,
    PRIMARY KEY (issue_id, url)
);

CREATE TABLE IF NOT EXISTS pr_work_items (
    provider         TEXT NOT NULL,
    repository       TEXT NOT NULL,
    pr_number        INTEGER NOT NULL,
    work_item_id     TEXT NOT NULL,
    work_item_source TEXT NOT NULL,
    source           TEXT NOT NULL
        CHECK (source IN ('attachment', 'branch', 'title', 'body', 'commit')),
    PRIMARY KEY (provider, repository, pr_number, work_item_id, work_item_source, source)
);

CREATE INDEX IF NOT EXISTS idx_pr_work_items_item
    ON pr_work_items(work_item_id, work_item_source);

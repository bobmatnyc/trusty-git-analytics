-- Migration v40: Notion activity as a per-person weekly signal (issue #252).
--
-- Why: Notion pages and comments are engineering activity beside git, PRs
-- and tickets. `tga collect` reads them when the opt-in `notion:` block is
-- set and stores one row per event, keyed so readers can count per person
-- per ISO week.
--
-- Design decisions:
--   * One row per event. `event_id` is deterministic, so a re-sync upserts
--     the same row instead of adding one:
--       page_created:<page_id>
--       page_edited:<page_id>:<last_edited_time>
--       comment_created:<comment_id>
--   * `kind` is one of 'page_created', 'page_edited', 'comment_created'.
--   * `page_edited` is a LOWER BOUND: the public API exposes only the last
--     editor and last edit time of a page, so a sync records at most one
--     edit per page. Earlier edits and non-last editors are not visible.
--   * `actor_email` is the canonical email from the identity resolver.
--     NULL for bots, for users with no email, and for users the token cannot
--     list. `actor_id` (Notion user id) is always kept.
--   * `body_len` is the comment body length in characters, for
--     'comment_created' only. The body itself is never stored.
--   * `iso_year` / `iso_week` bucket `occurred_at` (UTC).
--   * SCALE SEPARATION: Notion activity is its own signal. It is never
--     summed into `fact_commit_effort`, `fact_pm_work`, `fact_pm_effort` or
--     any effort/volume metric (same rule as migration v26).
--   * `notion_sync_cursor` holds, per workspace, the newest page
--     `last_edited_time` a completed sync stored. It is written in the same
--     transaction as that sync's event rows; a failed fetch or write leaves
--     it unchanged.
--
-- Additive migration: two new tables. No existing table or row changes.

CREATE TABLE IF NOT EXISTS fact_notion_activity (
    event_id     TEXT PRIMARY KEY,
    kind         TEXT NOT NULL,
    page_id      TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    actor_id     TEXT,
    actor_email  TEXT,
    actor_name   TEXT,
    occurred_at  TEXT NOT NULL,     -- RFC3339 UTC
    iso_year     INTEGER NOT NULL,
    iso_week     INTEGER NOT NULL,
    body_len     INTEGER,           -- comment_created only
    synced_at    TEXT NOT NULL      -- RFC3339 wall clock of the write
);

CREATE INDEX IF NOT EXISTS idx_fact_notion_activity_actor_week
    ON fact_notion_activity (actor_email, iso_year, iso_week);

CREATE INDEX IF NOT EXISTS idx_fact_notion_activity_page
    ON fact_notion_activity (page_id);

CREATE TABLE IF NOT EXISTS notion_sync_cursor (
    workspace_id          TEXT PRIMARY KEY,
    last_edited_watermark TEXT NOT NULL,  -- RFC3339: newest page last_edited_time stored
    synced_at             TEXT NOT NULL   -- RFC3339 wall clock of that sync's commit
);

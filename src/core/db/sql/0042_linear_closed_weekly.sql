-- Migration v42: closed Linear issues per developer per ISO week (#272).
--
-- Why: cto-reports reads FLYR ticket activity per engineer. It needs one row
-- per developer, team and ISO week, keyed on tga's canonical developer
-- (`authors.id`), not on Linear's user record.
--
-- Design decisions:
--   * A closed issue has `state_type = 'completed'` and a non-NULL
--     `completed_at`. Canceled issues (`state_type = 'canceled'`) are not
--     closed work and are excluded. Archived completed issues are included.
--   * The week is the ISO 8601 week of `completed_at` in UTC, written as
--     `YYYY-Www` (`2026-W01`); `week_start` is that week's Monday.
--   * The issue is credited to its stored assignee. tga keeps no assignee
--     history, so a reassignment after completion moves the credit. An issue
--     with no assignee is not written.
--   * `canonical_developer_id` is `authors.id` when the assignee matches a
--     tga developer (email first, then the identity aliases), else NULL. An
--     unmatched assignee keeps a row under its Linear name, so no closed
--     issue drops out of the totals.
--   * `closed_issue_keys` is a JSON array of issue identifiers, sorted.
--   * The table is derived. `tga linear sync` rebuilds it at the end of every
--     non-dry run, in one transaction (DELETE then INSERT), so a re-run gives
--     the same rows.
CREATE TABLE IF NOT EXISTS fact_linear_closed_weekly (
    canonical_developer_id INTEGER,
    developer_name         TEXT NOT NULL,
    team_key               TEXT NOT NULL,
    iso_week               TEXT NOT NULL,
    week_start             TEXT NOT NULL,
    closed_count           INTEGER NOT NULL,
    closed_issue_keys      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_fact_linear_closed_weekly_dev
    ON fact_linear_closed_weekly(canonical_developer_id, iso_week);
CREATE INDEX IF NOT EXISTS idx_fact_linear_closed_weekly_week
    ON fact_linear_closed_weekly(iso_week);

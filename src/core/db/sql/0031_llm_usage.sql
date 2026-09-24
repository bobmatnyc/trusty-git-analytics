-- #111: one row per LLM-tier call, so a cost report can price a run.
-- Rows accumulate across runs; group by run_started_at for one run.
CREATE TABLE IF NOT EXISTS llm_usage (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    commit_id      INTEGER NOT NULL,
    commit_sha     TEXT    NOT NULL,
    provider       TEXT    NOT NULL,
    model          TEXT    NOT NULL,
    outcome        TEXT    NOT NULL,
    input_tokens   INTEGER,
    output_tokens  INTEGER,
    run_started_at TEXT    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_llm_usage_run ON llm_usage(run_started_at);
CREATE INDEX IF NOT EXISTS idx_llm_usage_commit ON llm_usage(commit_id);

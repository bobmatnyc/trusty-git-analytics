-- Work items from PM systems (JIRA, GitHub, Linear, ADO).
--
-- This table stores a flattened, source-agnostic projection of work items
-- referenced by commits. The `(id, source)` composite PK lets us scope IDs by
-- source (e.g. ADO "42" is distinct from JIRA "42").
CREATE TABLE IF NOT EXISTS work_items (
    id          TEXT NOT NULL,
    source      TEXT NOT NULL,  -- 'azdo', 'jira', 'github', 'linear'
    title       TEXT NOT NULL,
    status      TEXT NOT NULL,
    item_type   TEXT NOT NULL,  -- 'Bug', 'User Story', 'Task', 'Epic', etc.
    tags        TEXT,           -- comma-separated
    project     TEXT,
    url         TEXT,
    raw_json    TEXT,           -- full JSON payload
    fetched_at  TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (id, source)
);

-- Join table: which commits reference which work items.
CREATE TABLE IF NOT EXISTS commit_work_items (
    commit_sha       TEXT NOT NULL,
    work_item_id     TEXT NOT NULL,
    work_item_source TEXT NOT NULL,
    PRIMARY KEY (commit_sha, work_item_id, work_item_source),
    FOREIGN KEY (work_item_id, work_item_source) REFERENCES work_items(id, source)
);

CREATE INDEX IF NOT EXISTS idx_commit_work_items_sha
    ON commit_work_items(commit_sha);
CREATE INDEX IF NOT EXISTS idx_commit_work_items_item
    ON commit_work_items(work_item_id, work_item_source);

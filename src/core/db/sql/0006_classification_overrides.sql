-- Tier 0: manual classification overrides.
--
-- Highest-priority signal in the cascade. A row here means a human (or
-- upstream tooling) has hand-labeled the commit, so the classifier should
-- skip all other tiers and use this verdict with confidence 1.0.
--
-- The (commit_sha, repo_path) composite PK lets the same SHA appear in
-- multiple repos (forks, mirrors) with potentially different overrides.
CREATE TABLE IF NOT EXISTS classification_overrides (
    commit_sha   TEXT NOT NULL,
    repo_path    TEXT NOT NULL,
    work_type    TEXT NOT NULL,    -- subcategory (e.g. "feature", "bugfix")
    change_type  TEXT NOT NULL,    -- top-level category (e.g. "feature", "maintenance")
    notes        TEXT,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (commit_sha, repo_path)
);

CREATE INDEX IF NOT EXISTS idx_classification_overrides_sha
    ON classification_overrides(commit_sha);

-- Per-repository analysis status / coverage bookkeeping.
--
-- One row per repository. Updated at the end of every classification run
-- with the percentage of commits that received a non-null, non-"uncategorized"
-- verdict. Downstream reports can surface low-coverage repos so operators
-- can tune rules or invoke the LLM tier.
CREATE TABLE IF NOT EXISTS repository_analysis_status (
    repo_name                    TEXT NOT NULL PRIMARY KEY,
    last_analyzed_at             TEXT NOT NULL DEFAULT (datetime('now')),
    classification_coverage_pct  REAL,
    total_commits                INTEGER NOT NULL DEFAULT 0,
    classified_commits           INTEGER NOT NULL DEFAULT 0
);

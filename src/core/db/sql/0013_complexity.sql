-- Add complexity score (1-5) to classifications.
-- NULL means not yet scored (non-LLM tiers, or pre-migration rows).
ALTER TABLE classifications ADD COLUMN complexity INTEGER;

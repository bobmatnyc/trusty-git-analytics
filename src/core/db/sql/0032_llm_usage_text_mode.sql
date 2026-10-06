-- #111: which commit text a Jev call carried: 'real' (the message as
-- stored, the default) or 'obfuscated' (llm.jev.obfuscate). NULL for every
-- other provider and for rows written before this migration.
ALTER TABLE llm_usage ADD COLUMN text_mode TEXT;

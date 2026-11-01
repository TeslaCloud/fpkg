-- Docs are current only when docs_status = 'ready' AND docs_version = FDOC_VERSION;
-- docs_error holds the error of a failed generation.
ALTER TABLE versions ADD COLUMN docs_version TEXT;
ALTER TABLE versions ADD COLUMN docs_error TEXT;

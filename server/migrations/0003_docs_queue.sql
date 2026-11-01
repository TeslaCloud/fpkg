-- When the docs job was last enqueued; requests don't re-enqueue it until this ages out.
ALTER TABLE versions ADD COLUMN docs_queued_at TEXT;

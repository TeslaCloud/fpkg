-- A package published by a non-admin is pending until an admin approves it; until then it is
-- visible only to its owners and admins. Packages that exist already stay public.
ALTER TABLE packages ADD COLUMN approved_at TEXT;
ALTER TABLE packages ADD COLUMN approved_by INTEGER REFERENCES users(id);
-- Who first published the package; the cap on pending packages counts by this, not by ownership.
ALTER TABLE packages ADD COLUMN created_by INTEGER REFERENCES users(id);
UPDATE packages SET approved_at = created_at WHERE approved_at IS NULL;
UPDATE packages SET created_by = (SELECT v.published_by FROM versions v WHERE v.package_id = packages.id ORDER BY v.published_at, v.sort_key LIMIT 1);
-- What admins did through the admin pages, newest last.
CREATE TABLE admin_actions (
  id            INTEGER PRIMARY KEY,
  actor_id      INTEGER NOT NULL REFERENCES users(id),
  action        TEXT    NOT NULL,               -- approve | reject | yank | unyank | owner_add | owner_remove
  package_id    TEXT    NOT NULL,               -- not a reference: a rejected package is gone
  version       TEXT,
  detail        TEXT,
  at            TEXT    NOT NULL
);
CREATE INDEX admin_actions_at ON admin_actions(at DESC);

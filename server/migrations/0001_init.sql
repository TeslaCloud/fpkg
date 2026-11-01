CREATE TABLE users (
  id            INTEGER PRIMARY KEY,
  github_id     INTEGER NOT NULL UNIQUE,
  login         TEXT    NOT NULL,
  created_at    TEXT    NOT NULL
);
CREATE TABLE tokens (
  id            INTEGER PRIMARY KEY,
  user_id       INTEGER NOT NULL REFERENCES users(id),
  name          TEXT    NOT NULL,
  hash          TEXT    NOT NULL UNIQUE,        -- sha256 hex of the token
  created_at    TEXT    NOT NULL,
  last_used_at  TEXT,
  revoked_at    TEXT
);
CREATE TABLE packages (
  id            TEXT    PRIMARY KEY,            -- the package id
  revision      INTEGER NOT NULL DEFAULT 1,     -- bumped on every publish/yank, feeds the ETag
  created_at    TEXT    NOT NULL,
  updated_at    TEXT    NOT NULL
);
CREATE TABLE owners (
  package_id    TEXT    NOT NULL REFERENCES packages(id),
  user_id       INTEGER NOT NULL REFERENCES users(id),
  added_at      TEXT    NOT NULL,
  PRIMARY KEY (package_id, user_id)
);
CREATE TABLE versions (
  package_id    TEXT    NOT NULL REFERENCES packages(id),
  version       TEXT    NOT NULL,               -- canonical X.Y.Z[-suffix]
  sort_key      TEXT    NOT NULL,               -- zero-padded components for ORDER BY
  metadata      TEXT    NOT NULL,               -- version metadata JSON minus dist
  sha256        TEXT    NOT NULL,
  size          INTEGER NOT NULL,
  yanked        INTEGER NOT NULL DEFAULT 0,
  published_at  TEXT    NOT NULL,
  published_by  INTEGER NOT NULL REFERENCES users(id),
  docs_status   TEXT    NOT NULL DEFAULT 'pending',  -- pending | ready | failed
  PRIMARY KEY (package_id, version)
);
CREATE INDEX versions_sort ON versions(package_id, sort_key DESC);
CREATE TABLE downloads (
  package_id    TEXT    NOT NULL,
  version       TEXT    NOT NULL,
  day           TEXT    NOT NULL,               -- YYYY-MM-DD
  count         INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (package_id, version, day)
);

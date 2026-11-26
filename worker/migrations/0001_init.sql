-- ReqHarbor Worker schema (PRODUCT.md §8 "D1 schema")

-- One row per remote workspace. Only SHA-256(token) is stored, hex-encoded.
-- `rev` is the per-workspace revision counter; every accepted change gets the next value.
CREATE TABLE workspaces (
  id          TEXT    PRIMARY KEY,
  name        TEXT    NOT NULL,
  token_hash  TEXT    NOT NULL UNIQUE,
  rev         INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
);

-- Synced entities (project, collection, folder, request, environment).
-- Deletes are kept as tombstones (deleted = 1, data = NULL) so they propagate.
CREATE TABLE entities (
  workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  type         TEXT    NOT NULL,
  id           TEXT    NOT NULL,
  rev          INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  deleted      INTEGER NOT NULL DEFAULT 0,
  data         TEXT,
  PRIMARY KEY (workspace_id, type, id)
);
CREATE UNIQUE INDEX entities_workspace_rev ON entities (workspace_id, rev);

-- Share links. `snapshot` is cleared (NULL) when a share is revoked.
CREATE TABLE shares (
  id           TEXT    PRIMARY KEY,
  workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  kind         TEXT    NOT NULL CHECK (kind IN ('collection', 'request')),
  entity_id    TEXT    NOT NULL,
  snapshot     TEXT,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  revoked_at   INTEGER
);
CREATE INDEX shares_workspace ON shares (workspace_id, created_at);

-- Fixed-window rate limiting (POST /v1/workspaces: 10/hour/IP). `key` embeds a hash of the IP.
CREATE TABLE rate_limits (
  key          TEXT    PRIMARY KEY,
  window_start INTEGER NOT NULL,
  count        INTEGER NOT NULL
);

-- index entities_workspace_rev on entities
CREATE UNIQUE INDEX entities_workspace_rev ON entities (workspace_id, rev);

-- index entities_workspace_size on entities
CREATE INDEX entities_workspace_size ON entities (workspace_id, size);

-- index shares_workspace on shares
CREATE INDEX shares_workspace ON shares (workspace_id, created_at);

-- index shares_workspace_size on shares
CREATE INDEX shares_workspace_size ON shares (workspace_id, revoked_at, size);

-- table entities on entities
CREATE TABLE entities (
  workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  type         TEXT    NOT NULL,
  id           TEXT    NOT NULL,
  rev          INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  deleted      INTEGER NOT NULL DEFAULT 0,
  data         TEXT, device_id TEXT NOT NULL DEFAULT '', size INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (workspace_id, type, id)
);

-- table schema_migrations on schema_migrations
CREATE TABLE schema_migrations (
  version    INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  applied_at INTEGER NOT NULL
);

-- table server_state on server_state
CREATE TABLE server_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- table shares on shares
CREATE TABLE shares (
  id           TEXT    PRIMARY KEY,
  workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  kind         TEXT    NOT NULL CHECK (kind IN ('collection', 'request')),
  entity_id    TEXT    NOT NULL,
  snapshot     TEXT,
  created_at   INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  revoked_at   INTEGER
, size INTEGER NOT NULL DEFAULT 0);

-- table workspaces on workspaces
CREATE TABLE workspaces (
  id          TEXT    PRIMARY KEY,
  name        TEXT    NOT NULL,
  token_hash  TEXT    NOT NULL UNIQUE,
  rev         INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
);


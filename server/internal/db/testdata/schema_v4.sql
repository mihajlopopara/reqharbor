-- index auth_requests_expiry on auth_requests
CREATE INDEX auth_requests_expiry ON auth_requests (expires_at);

-- index entities_workspace_rev on entities
CREATE UNIQUE INDEX entities_workspace_rev ON entities (workspace_id, rev);

-- index entities_workspace_size on entities
CREATE INDEX entities_workspace_size ON entities (workspace_id, size);

-- index identities_one_per_user on identities
CREATE UNIQUE INDEX identities_one_per_user ON identities (user_id);

-- index login_throttle_email on login_throttle
CREATE INDEX login_throttle_email ON login_throttle (email_key);

-- index login_throttle_gc on login_throttle
CREATE INDEX login_throttle_gc ON login_throttle (last_failed_at);

-- index one_time_tokens_expiry on one_time_tokens
CREATE INDEX one_time_tokens_expiry ON one_time_tokens (expires_at);

-- index sessions_prev_refresh on sessions
CREATE INDEX sessions_prev_refresh ON sessions (prev_refresh_hash);

-- index sessions_user on sessions
CREATE INDEX sessions_user ON sessions (user_id, revoked_at);

-- index shares_workspace on shares
CREATE INDEX shares_workspace ON shares (workspace_id, created_at);

-- index shares_workspace_size on shares
CREATE INDEX shares_workspace_size ON shares (workspace_id, revoked_at, size);

-- index team_members_user on team_members
CREATE INDEX team_members_user ON team_members (user_id);

-- index teams_one_personal on teams
CREATE UNIQUE INDEX teams_one_personal ON teams (owner_user_id) WHERE personal = 1;

-- index teams_owner on teams
CREATE INDEX teams_owner ON teams (owner_user_id, personal);

-- index users_open_signup on users
CREATE INDEX users_open_signup ON users (signup_via) WHERE signup_via = 'open';

-- index workspaces_one_per_team on workspaces
CREATE UNIQUE INDEX workspaces_one_per_team ON workspaces (team_id) WHERE team_id IS NOT NULL;

-- table auth_requests on auth_requests
CREATE TABLE auth_requests (
  id                    TEXT PRIMARY KEY,             -- 128-bit random, the ?ar= value
  browser_hash          TEXT NOT NULL,                -- sha256(cookie)
  client_id             TEXT NOT NULL,
  redirect_uri          TEXT NOT NULL,
  code_challenge        TEXT NOT NULL,
  client_state          TEXT NOT NULL,
  device_name           TEXT NOT NULL DEFAULT '',
  method_hint           TEXT,
  grant_token_hash      TEXT,                         -- applied invite/setup code (one_time_tokens.token_hash)
  provider              TEXT,
  upstream_state_hash   TEXT UNIQUE,
  upstream_verifier     TEXT,
  upstream_nonce        TEXT,
  pending_email         TEXT,                         -- password signup awaiting the SMTP code
  pending_name          TEXT,
  pending_phc           TEXT,
  email_code_hash       TEXT,
  email_code_expires_at INTEGER,
  email_code_attempts   INTEGER NOT NULL DEFAULT 0,
  email_codes_sent      INTEGER NOT NULL DEFAULT 0,
  password_attempts     INTEGER NOT NULL DEFAULT 0,
  code_attempts         INTEGER NOT NULL DEFAULT 0,
  user_id               TEXT REFERENCES users(id) ON DELETE CASCADE,
  code_hash             TEXT UNIQUE,
  code_expires_at       INTEGER,
  created_at            INTEGER NOT NULL,
  expires_at            INTEGER NOT NULL,
  consumed_at           INTEGER
);

-- table entities on entities
CREATE TABLE entities (
  workspace_id TEXT    NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  type         TEXT    NOT NULL,
  id           TEXT    NOT NULL,
  rev          INTEGER NOT NULL,
  updated_at   INTEGER NOT NULL,
  deleted      INTEGER NOT NULL DEFAULT 0,
  data         TEXT, device_id TEXT NOT NULL DEFAULT '', size INTEGER NOT NULL DEFAULT 0, updated_by TEXT NOT NULL DEFAULT '',
  PRIMARY KEY (workspace_id, type, id)
);

-- table identities on identities
CREATE TABLE identities (
  provider      TEXT NOT NULL CHECK (provider IN ('github','google','oidc','dev')),
  issuer        TEXT NOT NULL DEFAULT '',             -- OIDC issuer URL; '' for github/google/dev
  subject       TEXT NOT NULL,
  user_id       TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  email         TEXT NOT NULL DEFAULT '',             -- provider's current email (edge case 9)
  created_at    INTEGER NOT NULL,
  last_login_at INTEGER NOT NULL,
  PRIMARY KEY (provider, issuer, subject)
);

-- table login_throttle on login_throttle
CREATE TABLE login_throttle (
  key_hash       TEXT PRIMARY KEY,
  email_key      TEXT NOT NULL,                       -- HMAC(throttle_key, email): "all rows for this email"
  failed_count   INTEGER NOT NULL DEFAULT 0,
  locked_until   INTEGER NOT NULL DEFAULT 0,
  last_failed_at INTEGER NOT NULL
);

-- table one_time_tokens on one_time_tokens
CREATE TABLE one_time_tokens (
  token_hash TEXT PRIMARY KEY,
  id         TEXT NOT NULL UNIQUE,                    -- short public id for CLI list/revoke
  purpose    TEXT NOT NULL CHECK (purpose IN ('setup','signup_invite','reset_password')),
  user_id    TEXT REFERENCES users(id) ON DELETE CASCADE,   -- reset_password only
  email      TEXT,                                    -- optional: invite restricted to this email
  max_uses   INTEGER NOT NULL DEFAULT 1 CHECK (max_uses >= 1),
  uses       INTEGER NOT NULL DEFAULT 0,
  created_by TEXT,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  revoked_at INTEGER
);

-- table password_credentials on password_credentials
CREATE TABLE password_credentials (
  user_id      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  phc          TEXT NOT NULL,                         -- $argon2id$v=19$m=…,t=…,p=…$salt$hash
  updated_at   INTEGER NOT NULL
);

-- table schema_migrations on schema_migrations
CREATE TABLE schema_migrations (
  version    INTEGER PRIMARY KEY,
  name       TEXT    NOT NULL,
  applied_at INTEGER NOT NULL
);

-- table server_state on server_state
CREATE TABLE server_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);

-- table sessions on sessions
CREATE TABLE sessions (
  id                TEXT PRIMARY KEY,
  user_id           TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  client_id         TEXT NOT NULL,
  device_name       TEXT NOT NULL DEFAULT '',
  access_hash       TEXT NOT NULL UNIQUE,
  access_expires_at INTEGER NOT NULL,
  refresh_hash      TEXT NOT NULL UNIQUE,
  prev_refresh_hash TEXT,
  pending_pair_ct   TEXT,
  pending_until     INTEGER,
  created_at        INTEGER NOT NULL,
  last_used_day     INTEGER NOT NULL,
  idle_expires_at   INTEGER NOT NULL,
  expires_at        INTEGER NOT NULL,
  revoked_at        INTEGER
);

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
, size INTEGER NOT NULL DEFAULT 0, created_by TEXT, options    TEXT);

-- table team_members on team_members
CREATE TABLE team_members (
  team_id   TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role      TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
);

-- table teams on teams
CREATE TABLE teams (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  personal      INTEGER NOT NULL DEFAULT 0,
  owner_user_id TEXT NOT NULL REFERENCES users(id),   -- billing owner; NO ACTION on purpose (04 §5.5)
  created_at    INTEGER NOT NULL
);

-- table users on users
CREATE TABLE users (
  id             TEXT PRIMARY KEY,                    -- ULID
  email          TEXT NOT NULL UNIQUE,                -- trimmed + lowercased by accounts.NormalizeEmail
  email_trusted  INTEGER NOT NULL DEFAULT 0,          -- §5.4: may satisfy ADMIN_EMAIL / change users.email
  signup_via     TEXT NOT NULL CHECK (signup_via IN ('setup','invite','open','cli','admin_email')),
  name           TEXT NOT NULL DEFAULT '',
  avatar_url     TEXT,
  is_admin       INTEGER NOT NULL DEFAULT 0,
  disabled_at    INTEGER,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

-- table workspaces on workspaces
CREATE TABLE workspaces (
  id          TEXT    PRIMARY KEY,
  name        TEXT    NOT NULL,
  token_hash  TEXT    NOT NULL UNIQUE,
  rev         INTEGER NOT NULL DEFAULT 0,
  created_at  INTEGER NOT NULL
, team_id TEXT REFERENCES teams(id) ON DELETE CASCADE);

-- trigger one_method_identity on identities
CREATE TRIGGER one_method_identity BEFORE INSERT ON identities
  WHEN EXISTS (SELECT 1 FROM password_credentials WHERE user_id = NEW.user_id)
  BEGIN SELECT RAISE(ABORT, 'account already uses a password'); END;

-- trigger one_method_password on password_credentials
CREATE TRIGGER one_method_password BEFORE INSERT ON password_credentials
  WHEN EXISTS (SELECT 1 FROM identities WHERE user_id = NEW.user_id)
  BEGIN SELECT RAISE(ABORT, 'account already uses an external identity'); END;


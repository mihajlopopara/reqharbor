-- M2 teams (06-go-server.md §10 "M2 outline", 04-tech-design.md §4 0004_teams.sql).

-- Invite links, addressed by id; only SHA-256 of the token is stored (PRIV-4). max_uses NULL means
-- unlimited. Admin links must be single-use: `IS 1` (not `= 1`) so an admin link with an unlimited
-- (NULL) max_uses fails the CHECK too, since a CHECK that evaluates to NULL passes in SQLite.
CREATE TABLE team_invites (
  id         TEXT PRIMARY KEY,
  team_id    TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  role       TEXT NOT NULL CHECK (role IN ('admin','editor','viewer')),
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  max_uses   INTEGER CHECK (max_uses IS NULL OR max_uses >= 1),
  uses       INTEGER NOT NULL DEFAULT 0,
  revoked_at INTEGER,
  CHECK (role <> 'admin' OR max_uses IS 1)
);
CREATE INDEX team_invites_team ON team_invites (team_id, revoked_at, expires_at);
CREATE INDEX team_invites_creator ON team_invites (team_id, created_by);

-- Removal, leave and team-delete notices, and rejoin blocking (AC-TEAM-4/5/7). No FK to teams: rows
-- outlive a deleted team so its members learn "Acme was deleted". GC deletes them after 31 days
-- (invites live at most 30 days, so an older removal can no longer be bypassed).
CREATE TABLE team_departures (
  team_id    TEXT NOT NULL,
  user_id    TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  space_id   TEXT NOT NULL,
  team_name  TEXT NOT NULL,
  reason     TEXT NOT NULL CHECK (reason IN ('removed','left','deleted')),
  at         INTEGER NOT NULL,
  by_user_id TEXT,
  PRIMARY KEY (team_id, user_id, at)
);
CREATE INDEX team_departures_user ON team_departures (user_id, at);
CREATE INDEX team_departures_gc ON team_departures (at);

-- A 10-minute ticket that lets a not-yet-registered invitee sign up while REQHARBOR_SIGNUP=invite.
-- The app gets it from POST /v1/invites/ticket {token} and adds &ticket= to the authorize URL, so
-- the invite token itself never appears in a URL (PRIV-4).
CREATE TABLE signup_tickets (
  ticket_hash TEXT PRIMARY KEY,
  invite_id   TEXT NOT NULL REFERENCES team_invites(id) ON DELETE CASCADE,
  expires_at  INTEGER NOT NULL,
  used_at     INTEGER
);
CREATE INDEX signup_tickets_expiry ON signup_tickets (expires_at);

-- The ticket an auth request was started with (SHA-256 of it); consumed by account creation.
ALTER TABLE auth_requests ADD COLUMN ticket_hash TEXT;

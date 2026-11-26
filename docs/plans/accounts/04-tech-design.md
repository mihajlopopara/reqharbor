# ReqHarbor accounts, login, sync and team projects: technical design, revision 3 (lead developer)

**Status:** this is the design for review. It follows product spec revision 2 (including rulings R1–R8) and UX revision 2, and it resolves every item in the QA feedback (§0). Deviations from the specs are marked **[DEV-n]** and collected in §15 for sign-off. I read every file cited here and changed nothing in the repo. Build and test commands follow the house rule and run through `docker compose run --rm dev …` (`compose.yaml`).

---

## 0. QA resolution summary

| QA item | Resolution | Where |
|---|---|---|
| **[blocker]** Moving data between spaces relies on ids, so a pulled id can rebind a project | Bindings change **only by a local user action**. A known id that arrives from an unexpected space is ignored and counted. Parents are checked against the binding before any child upsert. This also closes a hole that exists today: `upsert_remote` removes a matching id from **every** project. Moves use **new ids** [DEV-5]; only the legacy-to-personal merge keeps ids. When any space's pull fails, the run defers all deletes. Moves run online, with a persisted plan that can be resumed. Tests cover the malicious re-push, the stale resurrection and the partial run. | §6.3, §6.5, §6.6 |
| **[major]** Role hierarchy and endpoint scoping are not enforced | Every role change or removal is one guarded SQL statement. Admins can act only on viewers and editors, and can grant at most admin. Only owners grant or revoke owner. The billing owner must transfer before being demoted or removed. Transfer is a 3-statement batch that never leaves the team without an owner. Every mutation carries explicit `team_id`/`user_id` predicates. Tests cover every actor×target role pair and cross-team id substitution. | §5.4, §12 |
| **[major]** The `oob` paste code enables phishing | v1 is **loopback-only**, and no page ever shows a code. Paste-code moves to M4 behind an interstitial that names the device [DEV-3]. The authorize request carries `device_name`, which the hosted page shows. | §2 |
| **[major]** Deleting an account orphans the personal space | `teams.owner_user_id NOT NULL REFERENCES users(id)` with no cascade. `DELETE /v1/me` runs an explicit batch. If the user still billing-owns a team that has other members, the foreign key makes the batch fail, which is a deliberate safety net. A test asserts that no workspaces, entities or shares remain and that share URLs return 404. | §4, §5.5 |
| **[major]** A single 404 deletes team data | A space is marked Removed only when `GET /v1/me/spaces` returns 200 for the same server and user and the space is absent from it. Any other 403 or 404 is a transient `unavailable` issue. The focused default is always **Keep a personal copy** [DEV-6]. 403 `not_a_member` and 404 `space_not_found` now follow the product spec wording. | §6.7 |
| **[minor]** The claim batch cannot run | Spec revision 2 dropped the server-side claim, so `claim.ts`, `claimed_tokens` and `workspace_claimed` are removed. The merge is client-side only and the legacy token is never invalidated. | §7 |
| **[minor]** A `__Host-` cookie cannot be scoped to `/auth` | The cookie becomes `__Secure-rh_ar; Path=/auth; HttpOnly; Secure; SameSite=Lax; Max-Age=600`. Tests check that `Set-Cookie` appears only under `/auth/*` and that `/v1` handlers never read cookies. | §2.3 |
| **[minor]** The loopback listener is fragile, and refresh has no grace window | The listener keeps accepting connections until a `GET /cb` with a matching `state` arrives. Refresh gets a 60 s idempotent window that returns the identical pair, stored encrypted under the old refresh token [replaces my earlier "no grace"]. | §2.2, §3.1 |
| **[minor]** Viewer read-only is enforced only in the UI | `Store` write guard `read_only_role` / `read_only_newer`. Viewer spaces never create conflict copies; the remote always wins, except for frozen unpushed edits. | §6.8 |
| **[minor]** Downgrades and upgrades corrupt bindings | M0 adds `extra` passthrough to `Workspace` and `SyncState`. Account state goes into **new files** (`account.json`, `sync-spaces.json`). Signing in removes `sync` from `workspace.json`, so a downgraded client stops syncing and cannot leak team projects. Pure-legacy users keep today's files unchanged. The Worker records each pushing client's version so gRPC and GraphQL work can warn about old clients. | §7, §9 |
| **[minor]** OTP brute force; skewed timestamps never recover | M4 adds a per-email failed-verify cap across codes (10/day, then a 24 h lock). Push responses carry `serverTime`. Once the clock is correct, the client restamps entities rejected for skew. | §6.9, §11 M4 |
| **[minor]** macOS keychain prompts with ad-hoc signing | Ad-hoc-signed macOS builds use the 0600 file store, with the warning line. The keychain is enabled only when CI signs with a Developer ID (`REQHARBOR_MACOS_KEYCHAIN=1`) [DEV-7]. Upgrading from build N to N+1 is added to the manual matrix. | §3.3 |

---

## 1. Code facts that drive the design (each checked against the file)

1. **A `workspaces` rebuild would cascade-delete everything.** `entities.workspace_id` and `shares.workspace_id` are `REFERENCES workspaces(id) ON DELETE CASCADE` (`worker/migrations/0001_init.sql`), and D1 enforces foreign keys. Therefore every schema change here is `ADD COLUMN` or a new table. Account spaces keep `token_hash NOT NULL UNIQUE` by storing the sentinel `'space:'||id`. `authenticate()` (`worker/src/workspaces.ts`) only looks up `sha256Hex(token)` after `TOKEN_RE` passes, so it can never match a sentinel.
2. **The Worker rejects data on tombstones.** `validateChange` (`worker/src/sync.ts`) throws "`data` must be null for a tombstone". A "moved to" marker on a tombstone would need a protocol change; new-id moves need none.
3. **`upsert_remote` relocates entities across projects today.** In `crates/core/src/sync.rs` L1009–1135, collection, folder and request upserts run `for t in store.projects.values_mut() { t.….retain(|x| x.id != …) }` and then insert under the parent named in the incoming data. With several spaces feeding one store, a teammate could move or inject an entity into a personal project just by knowing ids. The binding checks in §6.3 fix this.
4. **One unknown entity type fails the whole pull.** `PullPage.changes: Vec<Change>` and `Change.entity_type: EntityType` is a closed enum (`sync.rs` L88–110; `model.rs` L565).
5. **Push results ignore `reason`.** `apply_push_results_detailed` (`sync.rs` L726) drops tombstone bookkeeping for any status (`(true, _)`) and marks every `Rejected` entity as `too_large`.
6. **Only `Settings` passes unknown fields through** (`model.rs` L650 `#[serde(flatten)] extra`). `Workspace` (L41), `SyncState` (`storage.rs` L77) and all synced structs drop unknown keys. `BodyType` (`model.rs` L291) is a closed enum.
7. **Push is at D1's 100-parameter limit.** Today it binds 1 shared parameter plus 7 per row × 14 rows (`ROWS_PER_STATEMENT = 14`). `updated_by` must be a **second shared** parameter: 2 + 98 = 100.
8. **`withSecurityHeaders` deletes `Set-Cookie` on every response,** and the API CSP is `form-action 'none'` (`worker/src/http.ts`).
9. **The legacy client splits its state across files.** The cursor is in `workspace.json` (`SyncConfig.cursor`, `model.rs` L54). Bookkeeping is in `sync-state.json`, with a backup used on corruption (`storage.rs` ~L302). `connect()` calls `reset_for_new_remote()` (`sync.rs` L1341). `Store::tombstone` (`storage.rs` L532) records a tombstone only when `synced` has the key.
10. **Runtime behaviour.** A 401 maps to `SyncError::Unauthorized`, which leads to `FailureAction::Disconnect` (`sync_rt.rs`). A 5xx maps to `SyncError::Server`, which is retryable, so old clients already back off on a 507. `schedule_push` returns early when `workspace.sync` is `None`.
11. **Tauri setup.** Plugins are dialog, clipboard-manager and single-instance; the single-instance handler already does `unminimize()` and `set_focus()` (`src-tauri/src/main.rs` L19–24). Core tokio has no `net` feature outside dev-dependencies. `move_node` (`storage.rs` L1087) works within one project only, so no cross-project drag exists to block (AC-PROJ-7 is UI-only).
12. **Signing and mirrors.** macOS releases are ad-hoc signed (`.github/workflows/desktop-build.yml` L7). `ui/src/lib/api/types.ts` is byte-identical to `docs/types.ts` (the diff is empty). `wrangler.toml` has a placeholder `database_id`, and the Worker has no runtime dependencies (`worker/package.json`).

---

## 2. Auth mechanism and flow

**Choice:** the Worker acts as a minimal OAuth 2.1-style authorization server for a single public client, `reqharbor-desktop`. Towards GitHub and Google it is a confidential client.
- The desktop opens the **system browser** with PKCE S256 and a **loopback** redirect (RFC 8252).
- Tokens are opaque and only their hashes are stored. There are no JWTs and no new npm dependencies.
- v1 offers GitHub and Google; email OTP comes in M4 (D2b).

### 2.1 Route namespaces [DEV-1]

| Prefix | Content | Cookies |
|---|---|---|
| `/auth/*` | Server-rendered sign-in HTML | Only `__Secure-rh_ar`, with `Path=/auth` |
| `/i` | Static invite page; reads the URL fragment | none |
| `/v1/auth/*`, `/v1/me*`, `/v1/teams*`, `/v1/invites/*`, `/v1/spaces/*` | JSON for the app | never |
| `/v1/workspace*`, `/v1/workspaces`, `/v1/sync`, `/v1/shares*`, `/s/*` | Legacy API and share pages | unchanged byte for byte, except the 507 breaker (AC-QUOTA-4) |

The spec's `/v1/s/:id` would sit next to the public `/s/:shareId` pages, so space routes use `/v1/spaces/:id` instead.

### 2.2 Desktop sign-in sequence (v1, loopback only)

```
App (Rust core)                          Browser                         Worker                         Provider
1 GET /v1/auth/providers ─────────────────────────────────────────────▶ {providers:["github","google"], serverName}
2 bind 127.0.0.1:0; verifier=b64url(32B); challenge=b64url(SHA256(verifier)); state=b64url(16B)
  bind fails ⇒ error "couldn't open a local port", link to Sync › Advanced (no oob in v1)
3 opener.open_url(/auth/authorize?client_id=reqharbor-desktop&redirect_uri=http://127.0.0.1:P/cb
                  &code_challenge&code_challenge_method=S256&state&device_name=MacBook%20Pro%20(macOS))
                                         ─GET /auth/authorize──────────▶ validate; INSERT auth_requests (10 min);
                                                                          Set-Cookie __Secure-rh_ar; page shows
                                                                          "Sign in to ReqHarbor on MacBook Pro (macOS) · 14:02"
                                         ─GET /auth/start/github?ar=ID─▶ sha256(cookie)==browser_hash; upstream state+PKCE
                                                                          302 ──────────────────────────────▶ authorize
                                         ◀──────────────────────────────────────── 302 /auth/callback/github?code&state
                                         ─GET /auth/callback/github────▶ ar by upstream_state_hash + cookie;
                                                                          token exchange (secret + upstream verifier);
                                                                          profile; AC-AUTH-6 account resolution;
                                                                          mint code (32B, 60 s, hash stored)
                                         ◀── 302 http://127.0.0.1:P/cb?code&state
4 loopback loop: GET /cb with matching state ⇒ 302 → https://<server>/auth/done, stop listening,
  unminimize + set_focus main window. Anything else ⇒ 404 / 400 and keep listening (≤64 conns, 5 s read timeout each)
5 POST /v1/auth/token {grant_type:"authorization_code", code, code_verifier, redirect_uri}
    ◀── {accessToken:"rha_…", refreshToken:"rhr_…", expiresIn:3600, sessionId, user}
6 secret store write first (§3.3), then account.json (non-secret), then GET /v1/me/spaces
```

**Validation**
- **`redirect_uri`:** must match `^http://127\.0\.0\.1:(102[4-9]|10[3-9]\d|1[1-9]\d\d|[2-9]\d{3}|[1-5]\d{4}|6[0-4]\d{3}|65[0-4]\d\d|655[0-2]\d|6553[0-5])/cb$`. `localhost`, `https` and `oob` are rejected in v1. The token exchange requires an exact match.
- **`device_name`:** at most 64 characters, from the class `[\p{L}\p{N} ()._-]`. It is HTML-escaped on the page and stored on the session for AC-AUTH-10.
- **Code consumption:** one statement,
  `UPDATE auth_requests SET consumed_at=?now WHERE code_hash=?h AND consumed_at IS NULL AND code_expires_at>?now RETURNING …`.
  The Worker then checks the PKCE challenge and the redirect. A failed exchange still burns the code.
- **Provider errors:** on `?error=access_denied` the Worker redirects to `redirect_uri?error=access_denied&state=…`, and the app shows "Sign-in was cancelled in the browser."
- **Profiles:**
  - GitHub: `GET /user` and `GET /user/emails` (primary and verified). These need a `User-Agent` header.
  - Google: `openidconnect.googleapis.com/v1/userinfo` with `email_verified`.
  - Upstream tokens are never stored or logged.
- **Account resolution (AC-AUTH-6, D15c):**
  - Look up `identities(provider, subject)`. A hit means a returning user. If the provider email changed and no other account uses it, update `users.email`; otherwise keep the old email and expose `providerEmail` for edge case 9.
  - On a miss with a verified email: if `users.email` already exists, render the hosted `email_in_use` page with a masked email and the existing method. **Nothing is created.** Otherwise create the user, the identity, the personal team, the owner membership and the personal space in **one batch**.
  - With no verified email, render the hosted "no verified email" page.
  - Account creation is limited to 10 per hour per IP bucket and is refused while the breaker is open.

**UX impact of [DEV-3]:**
- The waiting view keeps **Open browser again** and **Cancel**, and drops **Paste code instead**.
- The hosted done page shows no code.
- M4 adds paste-code with the mitigations listed in §11.

### 2.3 Cookie and CSP

- **Cookie:** `Set-Cookie: __Secure-rh_ar=<b64url 16B>; Path=/auth; HttpOnly; Secure; SameSite=Lax; Max-Age=600`. Lax is still sent on the top-level GET back from the provider. Only `sha256(cookie)` is stored, as `auth_requests.browser_hash`.
- **Header handling:** `withSecurityHeaders(res, { allowCookie: pathname.startsWith("/auth/") })` keeps today's deletion of `Set-Cookie` everywhere else.
- **CSP for auth pages:** `default-src 'none'; style-src 'sha256-<PAGE_CSS>'; img-src data:; form-action 'none'; frame-ancestors 'none'; base-uri 'none'`.
  - v1 has no forms, because provider buttons are plain `<a>` links to `/auth/start/:p?ar=`.
  - M4 email OTP adds `form-action 'self'` on `/auth/email/*` only.
- **CSP for the `/i` page:** adds `script-src 'sha256-<INVITE_JS>'; connect-src 'self'`.
- **Reuse:** `share-page.ts` exports `PAGE_CSS` and `LOGO_SVG`, and the auth pages reuse them.

### 2.4 Tauri 2 pieces

| Need | Choice |
|---|---|
| Open the browser | `tauri-plugin-opener = "2"`, called from Rust as `app.opener().open_url(url, None::<&str>)`. Rust calls need no capability entry, so `capabilities/default.json` is unchanged. |
| Loopback | tokio `TcpListener::bind("127.0.0.1:0")` in core (add tokio `net`). The loop is described in §2.2. It has a 10-minute timeout plus a `tokio_util::sync::CancellationToken`. No `tauri-plugin-oauth`. |
| Focus | On the `/cb` hit, `get_webview_window("main")` then `unminimize()` and `set_focus()`, as in `main.rs` L19–24. |
| Deep links | Not in v1; they stay in PRODUCT §2.2. |
| Webview CSP | Unchanged. Rust makes every Worker call. |

---

## 3. Sessions and tokens

### 3.1 Server

- **Access token:** `rha_` + b64url(32 B), valid 1 h. **Refresh token:** `rhr_` + b64url(32 B), 60-day sliding idle expiry, 1-year absolute expiry, rotated on every use (D3a). Only SHA-256 hex hashes are stored. `crypto.ts` gains `newOpaqueToken(prefix, bytes)`, `sha256B64url()` (for PKCE) and `hkdfAesGcm{Seal,Open}()`.
- **Rotation (one compare-and-swap, same pattern as `rotateToken`):**
  ```sql
  UPDATE sessions SET prev_refresh_hash=refresh_hash, refresh_hash=?new, access_hash=?acc,
         access_expires_at=?now+3600000, idle_expires_at=?now+5184000000,
         pending_pair_ct=?ct, pending_until=?now+60000
   WHERE refresh_hash=?old AND revoked_at IS NULL AND idle_expires_at>?now AND expires_at>?now
  RETURNING id, user_id
  ```
  `pending_pair_ct` is AES-GCM(key = HKDF-SHA256(old refresh token, info `"rh-refresh-grace"`), `{accessToken, refreshToken, expiresAt}`). Only the holder of the old token can decrypt it.
- **When no row matches:** `SELECT … WHERE prev_refresh_hash=?old`.
  - If `now < pending_until`: decrypt and return **the same pair**. This is idempotent and writes nothing, and covers "the response was lost on the network".
  - Otherwise it is reuse: set `revoked_at` and return 401 `session_revoked`.
- **Per-request auth:** one indexed lookup on `access_hash`. An expired token gives 401 `session_expired`; an unknown one gives 401 `auth_required`. `last_used_day` is written only when the day changes (PRIV-1).
- **Session limits:** at most 20 live sessions per user. On insert:
  `UPDATE sessions SET revoked_at=?now WHERE id IN (SELECT id FROM sessions WHERE user_id=?u AND revoked_at IS NULL ORDER BY created_at DESC LIMIT -1 OFFSET 20)`.
- **Logout and revoke:** `POST /v1/auth/logout` and `DELETE /v1/me/sessions/:id … WHERE id=? AND user_id=?me` set `revoked_at`, which takes effect immediately.

### 3.2 Client: `crates/core/src/auth.rs` (new)

- **Helpers:** PKCE and random values through `ring` (already compiled via rustls; add `ring = "0.17"` as a direct dependency). The loopback listener. An `AuthClient` with `providers`, `exchange_code`, `refresh`, `logout`, `me`, `sessions`, `spaces`, teams and invites.
- **`Session` struct in core** (no async trait needed): `{ server, user_id, secrets: Arc<dyn SecretStore>, tokens: tokio::sync::Mutex<Tokens> }`.
  - `access_token()` refreshes when `now > expiresAt − 60 s`.
  - `on_unauthorized(used)` refreshes only if `used` is still the current token (single-flight), then the caller retries once.
  - When refresh fails, the state becomes `SignInRequired` (AC-AUTH-9). Bindings, `sync-spaces.json` and unpushed edits are untouched.
- **Persist before use:** new tokens are written to the secret store (in `spawn_blocking`) and only then swapped into memory.

### 3.3 Secret storage (AC-AUTH-7)

- **Core:** `crates/core/src/secrets.rs` defines `trait SecretStore { get / set / delete }` with `FileSecretStore` (`secrets.json` in the app data dir, 0600 on Unix, profile ACL on Windows) and `MemorySecretStore` for tests.
- **Tauri:** `src-tauri/src/secret_store.rs` provides `KeyringStore` on `keyring = "3"`.
  - Service `dev.reqharbor.desktop`.
  - Entry `session:<host>:<userId>` holds one JSON value `{accessToken, accessExpiresAt, refreshToken, sessionId}`, so each write is atomic.
  - Entry `legacy:<remoteWorkspaceId>` holds the `rh_` token, written only on sign-in or merge (§7).
- **Backends:**
  - **Windows:** `windows-native`.
  - **Linux:** the zbus-based `async-secret-service` + `tokio` + `crypto-rust`, so the Docker image does not need `libdbus-1-dev`. Verify the exact feature names when implementing.
  - **macOS [DEV-7]:** `FileSecretStore` unless built with `REQHARBOR_MACOS_KEYCHAIN=1`, which CI sets only when Developer ID signing secrets exist. Ad-hoc signatures change on every release, so the keychain ACL would prompt or deny after each update. Settings shows: "Your sign-in is stored in a protected file because this build isn't signed for the macOS keychain."
- **Startup probe:** a set/get/delete round trip.
  - If it fails, use the file store and set `SyncStatus.keychain = "file"`.
  - If a keychain *read* is denied, retry once. If it is denied again, treat the user as signed out with the banner (edge case 7). Bindings stay Paused, not removed.
- **Dev builds:** `REQHARBOR_SECRET_STORE=file`.

---

## 4. D1 schema (new numbered migrations)

### `worker/migrations/0003_accounts.sql` (Worker M1)

```sql
CREATE TABLE users (
  id TEXT PRIMARY KEY, email TEXT COLLATE NOCASE, name TEXT NOT NULL DEFAULT '',
  avatar_url TEXT, created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX users_email ON users (email) WHERE email IS NOT NULL;

CREATE TABLE identities (
  provider TEXT NOT NULL CHECK (provider IN ('github','google','email')),
  subject  TEXT NOT NULL,                      -- GitHub numeric id | Google sub | (M4) lowercased email
  user_id  TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  email    TEXT,                               -- provider's current email (edge case 9)
  created_at INTEGER NOT NULL,
  PRIMARY KEY (provider, subject)
);
CREATE UNIQUE INDEX identities_one_per_user ON identities (user_id);   -- v1: one method per account (D15c); dropped in M4

-- Every account owns exactly one hidden personal team; its workspace is the personal space.
-- owner_user_id = billing owner (quota aggregate). NO ACTION on delete on purpose: DELETE /v1/me must
-- delete or reassign owned teams first, otherwise the FK aborts the batch (QA: orphaned spaces).
CREATE TABLE teams (
  id TEXT PRIMARY KEY, name TEXT NOT NULL, personal INTEGER NOT NULL DEFAULT 0,
  owner_user_id TEXT NOT NULL REFERENCES users(id),
  created_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX teams_one_personal ON teams (owner_user_id) WHERE personal = 1;
CREATE INDEX teams_owner ON teams (owner_user_id);

CREATE TABLE team_members (
  team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);

CREATE TABLE sessions (
  id TEXT PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  device_name TEXT NOT NULL DEFAULT '',
  access_hash TEXT NOT NULL UNIQUE, access_expires_at INTEGER NOT NULL,
  refresh_hash TEXT NOT NULL UNIQUE, prev_refresh_hash TEXT,
  pending_pair_ct TEXT, pending_until INTEGER,          -- 60 s idempotent refresh grace (§3.1)
  created_at INTEGER NOT NULL, last_used_day INTEGER NOT NULL,
  idle_expires_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, revoked_at INTEGER
);
CREATE INDEX sessions_user ON sessions (user_id, revoked_at);
CREATE INDEX sessions_prev_refresh ON sessions (prev_refresh_hash);

CREATE TABLE auth_requests (
  id TEXT PRIMARY KEY,                       -- 128-bit random; also the ?ar= value
  browser_hash TEXT NOT NULL,                -- sha256(__Secure-rh_ar)
  redirect_uri TEXT NOT NULL, code_challenge TEXT NOT NULL, client_state TEXT NOT NULL,
  device_name TEXT NOT NULL DEFAULT '',
  provider TEXT, upstream_state_hash TEXT UNIQUE, upstream_verifier TEXT,
  user_id TEXT REFERENCES users(id) ON DELETE CASCADE,
  code_hash TEXT UNIQUE, code_expires_at INTEGER,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, consumed_at INTEGER
);
CREATE INDEX auth_requests_expiry ON auth_requests (expires_at);

-- Ownership. NULL = legacy anonymous workspace. No table rebuild (§1.1): account spaces store
-- token_hash = 'space:'||id, which can never equal a SHA-256 hex digest.
ALTER TABLE workspaces ADD COLUMN team_id TEXT REFERENCES teams(id) ON DELETE CASCADE;
CREATE UNIQUE INDEX workspaces_one_per_team ON workspaces (team_id) WHERE team_id IS NOT NULL;  -- D13a

ALTER TABLE entities ADD COLUMN updated_by TEXT NOT NULL DEFAULT '';   -- user id; '' legacy (AC-TEAM-8)
ALTER TABLE shares   ADD COLUMN created_by TEXT;   -- no FK: deleted user renders "Former member"
ALTER TABLE shares   ADD COLUMN options    TEXT;   -- {"includeCredentials":bool,"environmentId":string|null}

-- Which client versions push into a space (forward-compat warning for gRPC/GraphQL, §9).
CREATE TABLE client_versions (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
  device_id TEXT NOT NULL, version TEXT NOT NULL,      -- '' = pre-M0 (no header)
  last_seen_day INTEGER NOT NULL,
  PRIMARY KEY (workspace_id, device_id)
);
```

### `worker/migrations/0004_teams.sql` (Worker M2)

```sql
CREATE TABLE team_invites (
  id TEXT PRIMARY KEY,
  team_id TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  token_hash TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL CHECK (role IN ('admin','editor','viewer')),
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL,
  max_uses INTEGER CHECK (max_uses IS NULL OR max_uses >= 1),          -- NULL = unlimited
  uses INTEGER NOT NULL DEFAULT 0, revoked_at INTEGER,
  CHECK (role <> 'admin' OR max_uses = 1)                              -- AC-INV-1
);
CREATE INDEX team_invites_team ON team_invites (team_id, revoked_at, expires_at);

-- Removal / leave / team-delete notices + rejoin blocking (AC-TEAM-4/5/7, D21). No FK to teams:
-- rows must survive team deletion so members learn "Acme was deleted". GC after 31 days
-- (invites expire after ≤ 30 days, so an older removal can no longer be bypassed).
CREATE TABLE team_departures (
  team_id TEXT NOT NULL, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  space_id TEXT NOT NULL, team_name TEXT NOT NULL,
  reason TEXT NOT NULL CHECK (reason IN ('removed','left','deleted')),
  at INTEGER NOT NULL, by_user_id TEXT,
  PRIMARY KEY (team_id, user_id, at)
);
CREATE INDEX team_departures_user ON team_departures (user_id, at);
```

### `0005_email.sql` (M4)
`email_codes(auth_request_id PK → auth_requests ON DELETE CASCADE, email, code_hmac, attempts, sent_at, expires_at)`. The per-email failure cap reuses `rate_limits` with the key `otp_fail:` + sha256(email), so it needs no table.

### Ownership model and GC
- **Ownership:**
  - A legacy space has `workspaces.team_id IS NULL`.
  - A personal space belongs to a team with `personal = 1`; its owner is the only member.
  - A team space belongs to a team with `personal = 0`.
- **Project ownership** is not a column. It follows from which space a project's entities live in; the device-local binding is described in §6.1.
- **GC:** add `[triggers] crons = ["17 3 * * *"]` with a `scheduled()` handler in `index.ts` that deletes:
  - expired `auth_requests`;
  - sessions that were revoked or expired more than 7 days ago;
  - invites that expired more than 30 days ago;
  - `team_departures` older than 31 days;
  - `rate_limits` windows older than 2 days;
  - `client_versions` older than 90 days.

---

## 5. Worker API

### 5.1 Modules (`worker/src/`)

| File | Responsibility |
|---|---|
| `authz.ts` | `authSession()` returns `{sessionId, userId}`. `authSpace(req, env, spaceId)` returns `{ws, actor{userId, role, personal, ownerUserId}}`. Also `RANK` and `requireRole()`. |
| `oauth.ts` | GitHub and Google adapters. A provider is enabled when its secret pair exists. A `dev` provider is enabled only when `DEV_AUTH="1"` **and** the host is `localhost`, `127.0.0.1` or `worker.test`. |
| `auth.ts`, `auth-pages.ts` | `/auth/*` pages, `/v1/auth/providers`, `/v1/auth/token`, `/v1/auth/logout`, and account resolution |
| `me.ts` | `/v1/me` (GET, DELETE), sessions, `/v1/me/spaces` |
| `teams.ts`, `invites.ts`, `invite-page.ts` | M2 |
| `ratelimit.ts` | `enforceCreateRateLimit` moved out of `workspaces.ts` as `fixedWindow(env, prefix, bucket, limit, windowMs)`, plus `coarse(env.AUTH_LIMITER?, key)`, which is skipped when the binding is absent |
| `capacity.ts` | Breaker (§5.6) |

### 5.2 Space authorisation (one round trip; it also loads `rev` for `pull`)

```sql
SELECT s.id AS session_id, s.user_id, s.access_expires_at, s.last_used_day,
       w.id, w.name, w.rev, w.created_at, w.token_hash, w.team_id,
       t.personal, t.owner_user_id, m.role
  FROM sessions s
  LEFT JOIN workspaces w   ON w.id = ?2
  LEFT JOIN teams t        ON t.id = w.team_id
  LEFT JOIN team_members m ON m.team_id = w.team_id AND m.user_id = s.user_id
 WHERE s.access_hash = ?1 AND s.revoked_at IS NULL
```

| Result | Response |
|---|---|
| No row | 401 `auth_required` |
| Token expired | 401 `session_expired` |
| `w.id IS NULL`, or `w.team_id IS NULL` (legacy rows are never addressable via `/v1/spaces`) | 404 `space_not_found` |
| `m.role IS NULL` | 403 `not_a_member` (follows the spec; my earlier deviation 8 is withdrawn) |

The client never acts on these codes alone (§6.7).

### 5.3 Endpoints

The role ranks are viewer 1, editor 2, admin 3, owner 4.

| Method & path | Auth | Rule |
|---|---|---|
| `GET /v1/auth/providers` | – | `{providers, serverName, allowAnonymousWorkspaces}` |
| `GET /auth/authorize`, `/auth/start/:p`, `/auth/callback/:p`, `/auth/done` | cookie | HTML (§2) |
| `POST /v1/auth/token` | PKCE or refresh | `grant_type` is `authorization_code` or `refresh_token`; failures return 400 `invalid_grant` |
| `POST /v1/auth/logout` | access | Revokes this session |
| `GET /v1/me` | access | `{user, method, providerEmail?}` |
| `DELETE /v1/me` | access | §5.5 (M3) |
| `GET /v1/me/sessions`; `DELETE /v1/me/sessions/:id` | access | Scoped by `user_id = me` |
| `GET /v1/me/spaces` | access | `{spaces:[{spaceId, kind, teamId, teamName, role, bytesUsed, quotaBytes, memberCount}], owner:{bytesUsed, quotaBytes}, departed:[{spaceId, teamId, teamName, reason, at}]}`. This is the **authoritative membership source** (§6.7). |
| `GET /v1/spaces/:id` | viewer | Shape of `getWorkspace` plus `oldestClientVersion` |
| `GET /v1/spaces/:id/sync` | viewer | `pull`, plus `updatedBy:{userId, name}` per change |
| `POST /v1/spaces/:id/sync` | **editor** | Push with `actor` (§6.2). A viewer gets 403 `forbidden_role` for the whole push. |
| `GET /v1/spaces/:id/shares` | viewer | Adds `createdBy` and `options` |
| `POST /v1/spaces/:id/shares` | editor | Stores `created_by` and `options`; 30 per hour per space |
| `PUT`/`DELETE /v1/spaces/:id/shares/:sid` | editor for own shares, admin for others | `WHERE id=?sid AND workspace_id=?space` |
| `GET`/`POST /v1/teams` | access | List with `personal = 0`. Create: at most `MAX_OWNED_TEAMS` (3) where `owner_user_id = me`, otherwise 409 `limit_teams`. Team, membership and workspace are one batch. `TEAMS_ENABLED` var gate (§11). |
| `GET /v1/teams/:id` | viewer | Detail |
| `PATCH /v1/teams/:id` | admin | Rename |
| `DELETE /v1/teams/:id` | billing owner | §5.4 |
| `GET /v1/teams/:id/members` | viewer | List |
| `PATCH`/`DELETE /v1/teams/:id/members/:uid` | §5.4 | `DELETE` body `{revokeInvites: bool}` |
| `POST /v1/teams/:id/transfer {userId}` | billing owner | §5.4 |
| `POST /v1/teams/:id/leave` | member | Not allowed for the billing owner or the last owner |
| `GET`/`POST /v1/teams/:id/invites`; `DELETE /v1/teams/:id/invites/:iid` | admin | Admin-role links can be created only by an owner and must be single-use. Limits: 20 active, 30 per day per team. Delete is `WHERE id=?iid AND team_id=?id`. |
| `POST /v1/invites/preview {token}` | – | `{teamName, inviterName, role, expiresAt}`, or a generic 410 `invite_invalid`. Coarse rate limit. |
| `POST /v1/invites/accept {token}` | access | §5.4 |
| `GET /i` | – | Static invite page (AC-INV-2/3) |
| Legacy routes | `rh_` | Unchanged, plus the breaker. `POST /v1/workspaces` obeys `ALLOW_ANON_WORKSPACES` (default `"true"`, D4a). |

Personal teams are hidden: every `/v1/teams/:id*` mutation on `personal = 1` returns 404.

### 5.4 Atomic invariants

Let `R(x)` be `(CASE x WHEN 'owner' THEN 4 WHEN 'admin' THEN 3 WHEN 'editor' THEN 2 WHEN 'viewer' THEN 1 END)`.

**Role change** (`PATCH members/:uid {role: ?4}`; `?1`=team, `?2`=target, `?3`=actor):
```sql
UPDATE team_members AS tm SET role = ?4
 WHERE tm.team_id = ?1 AND tm.user_id = ?2
   AND EXISTS (SELECT 1 FROM teams t WHERE t.id = ?1 AND t.personal = 0 AND t.owner_user_id <> ?2)  -- billing owner: transfer first
   AND EXISTS (SELECT 1 FROM team_members a WHERE a.team_id = ?1 AND a.user_id = ?3 AND (
            a.role = 'owner'                                           -- owners act on anyone, any role
         OR (a.role = 'admin' AND R(tm.role) < 3 AND R(?4) <= 3)      -- admins: only below them, grant ≤ admin
         OR (?2 = ?3 AND R(?4) < R(tm.role))))                        -- anyone may lower themselves
   AND (tm.role <> 'owner' OR ?4 = 'owner'
        OR (SELECT COUNT(*) FROM team_members WHERE team_id = ?1 AND role = 'owner') > 1)
```

**Remove** (`DELETE members/:uid`): one batch whose statements share the predicate above, with the self-branch removed (leaving is a separate endpoint):
1. `INSERT INTO team_departures (…, reason) SELECT …, 'removed', ?now, ?3 FROM team_members tm JOIN teams t … WHERE <predicate>`
2. `DELETE FROM team_members WHERE <predicate>`
3. If `revokeInvites`: `UPDATE team_invites SET revoked_at=?now WHERE team_id=?1 AND revoked_at IS NULL AND EXISTS (SELECT 1 FROM team_departures WHERE team_id=?1 AND user_id=?2 AND at=?now AND reason='removed')`

If statement 2 changed 0 rows, a diagnostic `SELECT` maps the outcome to:
- 404 `not_a_member`;
- 403 `forbidden_role`;
- 409 `last_owner`;
- 409 `transfer_required` [DEV-8].

**Transfer** (`?2`=target, `?3`=caller). This is one batch, and the team can never end up with zero owners:
1. `UPDATE team_members SET role='owner' WHERE team_id=?1 AND user_id=?2 AND ?2<>?3 AND EXISTS (SELECT 1 FROM teams WHERE id=?1 AND owner_user_id=?3 AND personal=0)`
2. `UPDATE teams SET owner_user_id=?2 WHERE id=?1 AND owner_user_id=?3 AND EXISTS (SELECT 1 FROM team_members WHERE team_id=?1 AND user_id=?2 AND role='owner')`
3. `UPDATE team_members SET role='admin' WHERE team_id=?1 AND user_id=?3 AND EXISTS (SELECT 1 FROM teams WHERE id=?1 AND owner_user_id=?2)`

Before the batch, the target's aggregate is checked: (target aggregate + team usage) must not exceed `QUOTA_OWNER_BYTES`, otherwise 413 `quota_exceeded` with the shortfall. If the target was removed at the same moment, all three statements are no-ops and the call returns 404 `not_a_member`.

**Invite accept.** A pre-check finds an existing membership and returns 200 `{alreadyMember: true, role}`. Otherwise one batch:
1. `INSERT INTO team_members (team_id,user_id,role,joined_at) SELECT i.team_id, ?u, i.role, ?now FROM team_invites i WHERE i.token_hash=?h AND i.revoked_at IS NULL AND i.expires_at>?now AND (i.max_uses IS NULL OR i.uses<i.max_uses) AND NOT EXISTS (SELECT 1 FROM team_departures d WHERE d.team_id=i.team_id AND d.user_id=?u AND d.reason='removed' AND d.at>=i.created_at) AND (SELECT COUNT(*) FROM team_members WHERE team_id=i.team_id) < ?maxMembers ON CONFLICT DO NOTHING`
2. `UPDATE team_invites SET uses=uses+1 WHERE token_hash=?h AND EXISTS (SELECT 1 FROM team_members m WHERE m.team_id=team_invites.team_id AND m.user_id=?u AND m.joined_at=?now)`

D1 batches are serialised transactions, so two parallel accepts of a single-use link cannot both pass statement 1. When nothing is inserted, the response is 410 `invite_invalid` (the single generic message of AC-INV-5) or 409 `limit_members`.

**Delete team** (billing owner):
1. `INSERT INTO team_departures SELECT team_id, user_id, …, 'deleted' FROM team_members WHERE team_id=?1`
2. `DELETE FROM teams WHERE id=?1 AND personal=0 AND owner_user_id=?me`

The delete cascades through members, invites, the workspace, entities and shares. Share pages then return 404, because the rows are gone.

### 5.5 Delete account (`DELETE /v1/me`, AC-ACCT-2)

- **Pre-check:** if the user is the sole owner of any team that has other members, return 409 `last_owner` with the list of those teams.
- **Batch:**
  1. Reassign billing: `UPDATE teams SET owner_user_id=(SELECT user_id FROM team_members m WHERE m.team_id=teams.id AND m.role='owner' AND m.user_id<>?u ORDER BY joined_at LIMIT 1) WHERE owner_user_id=?u AND personal=0 AND EXISTS (SELECT 1 FROM team_members m WHERE m.team_id=teams.id AND m.role='owner' AND m.user_id<>?u)`
  2. Delete sole-member teams, which covers the personal team: `DELETE FROM teams WHERE id IN (SELECT team_id FROM team_members WHERE user_id=?u) AND NOT EXISTS (SELECT 1 FROM team_members m WHERE m.team_id=teams.id AND m.user_id<>?u)`. This cascades through the workspaces, entities and shares.
  3. `DELETE FROM users WHERE id=?u`. This cascades through identities, sessions, memberships and auth requests.
- **Safety net:** if any `teams.owner_user_id` still points at `?u` (a race), the foreign key fails statement 3 and the whole batch rolls back with 409.
- **What remains:** `entities.updated_by` and `shares.created_by` keep opaque ids, which render as "Former member".

### 5.6 Quotas, breaker and rate limits

- **`quota.ts`:** `enforceQuota(used, delta, limit, label)`.
  - Personal and team spaces have `QUOTA_PERSONAL_BYTES` and `QUOTA_TEAM_BYTES` (25 MB each).
  - The **owner aggregate** is `SUM(size)` over entities and active shares of workspaces whose `teams.owner_user_id = owner` (covered by the existing size indexes), checked against `QUOTA_OWNER_BYTES` (100 MB).
  - Legacy spaces keep `LIMITS.workspaceQuotaBytes` (50 MB).
- **`capacity.ts` (AC-QUOTA-4):**
  - A module-level `{bytes, at}` is updated from `meta.size_after` of every D1 result the push and share handlers already hold.
  - On a usage-growing write, when `bytes ≥ DB_SOFT_LIMIT_BYTES` (default 400 MB; set 8 GB on Paid) and the value is fresher than 60 s, return 507 `server_full`. That applies to legacy writes too, and to account creation.
  - A stale cache is refreshed by a `SELECT 1` and its meta. Verify in miniflare that read results carry `size_after`.

| Limit | Mechanism | Default |
|---|---|---|
| `/auth/*`, `/v1/auth/*`, `/v1/invites/preview` | `[[ratelimits]] AUTH_LIMITER` per IP bucket | 20 / 60 s |
| `/v1/spaces/*/sync` | `SYNC_LIMITER` per session | 120 / 60 s |
| Token-exchange failures | D1 `fixedWindow("tok_fail")` | 30 / h per IP |
| New accounts | D1, same bucket as workspace creation | 10 / h per IP |
| Share create | D1 | 30 / h per space |
| Invite create | D1 | 30 / day per team |
| Legacy workspace create | unchanged | 10 / h |

Every limit returns 429 `rate_limited` with `Retry-After`. IP buckets reuse `rateLimitBucket()`, which groups IPv6 by /64.

### 5.7 Error codes

| Status | Codes |
|---|---|
| 401 | `auth_required`, `session_expired`, `session_revoked` |
| 403 | `forbidden_role`, `not_a_member` |
| 404 | `space_not_found` |
| 409 | `last_owner`, `transfer_required`, `limit_teams`, `limit_members`, `limit_invites` |
| 410 | `invite_invalid` |
| 426 | `client_too_old` |
| 400 | `invalid_grant` |
| 429 | `rate_limited` |
| 507 | `server_full` |
| per change | `clock_skew`, `forbidden_role`, `global_env_not_allowed` |
| hosted page only | `email_in_use`, `no_verified_email` |

---

## 6. Extending the sync protocol to personal and team spaces

The wire protocol is **unchanged**: LWW on `(updatedAt, deviceId)`, a rev cursor, and batches of 500 changes / 4 MB. The client goes from one remote to N spaces, under strict binding rules.

### 6.1 Client files and model

| File | Content | Written by |
|---|---|---|
| `workspace.json` | Unchanged, plus `#[serde(flatten)] extra` (M0). `sync` is used **only in pure-legacy mode**. | all |
| `sync-state.json` | Unchanged, plus `extra` (M0). Holds `device_id`; in pure-legacy mode also the legacy bookkeeping. | all |
| `account.json` (new) | `{schemaVersion:1, account?:{serverUrl,userId,email,name,method,sessionId}, spaces:[SpaceBinding], projectSpaces:{projectId: spaceId}, globalSpace?: spaceId, legacy?:{workerUrl, remoteWorkspaceId, mode:"sync"\|"sharesOnly"}}` | M1+ only |
| `sync-spaces.json` (+ `.backup`) (new) | `{schemaVersion:1, spaces:{spaceId:{cursor, state: SyncState /* no deviceId */, attribution:{key:{userId,name}}, frozen:[key], issue?, unknownTypes, deferredRuns, pendingMoves:[…]}}}` | M1+ only |

```rust
pub struct SpaceBinding { space_id: String, kind: SpaceKind /* Personal|Team|Legacy */, team_id: Option<String>,
                          team_name: Option<String>, role: Option<Role>,
                          state: BindingState /* Active|Paused|Unavailable{since}|Removed{at, reason} */ }
```

- **Modes:**
  - **Pure-legacy mode** means `account.json` is absent. The engine treats every project as bound to a synthetic `legacy` space, backed by today's `sync_state` and `workspace.sync.cursor`. Files and behaviour are byte-identical to today.
  - **Account mode** means `account.json` is present. `projectSpaces` is **authoritative**: a project with no entry is "This device only". This mapping is never re-derived.
- **Store:** gains `account: Option<AccountFile>` and `spaces_state: SpacesFile`, and `Store::space_state_mut(space)` hides the two backends.
- **Tombstones:** `Store::tombstone(t, id)` resolves the entity's space **before** removal, through `space_of_project`. The call sites at `storage.rs` L684–700, 824–831, 954–960, 1077 and 1330 all know the project or can look it up. A project with no binding records no tombstone.
- **New projects:** signed in, they go to Personal (D11). With only legacy connected, they go to legacy (today). Otherwise they have no binding.

### 6.2 Worker push and pull with an `actor` (`worker/src/sync.ts`)

`push(req, ws, env, actor?)` and `pull(req, ws, env, actor?)`. Legacy calls pass no actor, so their output is byte-identical. When an actor is present:

- **Role:** a viewer gets 403 `forbidden_role` for the whole push. For `type === "project" && deleted && R(role) < admin`, the change is rejected with `forbidden_role` and the rest of the batch applies (AC-ROLE-2).
- **Team spaces only:**
  - an `environment` whose `data.projectId` is null is rejected with `global_env_not_allowed` (AC-TSYNC-6);
  - `stripDeviceLocalPaths()` (new, in `sanitize.ts`) blanks `body.binaryPath` and the value of each `body.form[].kind === "file"` entry before the size is computed (AC-TSYNC-8).
- **Clock skew (all account spaces):** `updatedAt > Date.now() + 300000` is rejected with `clock_skew` (AC-TSYNC-5).
- **Attribution:** `updated_by` is bound as shared `?2` (2 + 7 × 14 = 100; leave a comment next to `ROWS_PER_STATEMENT`). It is written as `CASE WHEN ${WIN} THEN ?2 ELSE entities.updated_by END` and in `VALUES (…, ?2)`.
- **Response:** gains `serverTime`, used for the clock-skew restamp (§6.9).
- **Version record:** one more statement in the same batch:
  `INSERT INTO client_versions … ON CONFLICT DO UPDATE SET version=excluded.version, last_seen_day=excluded.last_seen_day WHERE version<>excluded.version OR last_seen_day<>excluded.last_seen_day`
  This runs on legacy pushes too; the response is unchanged. `version` comes from `X-ReqHarbor-Version`, or `''` when the header is absent.
- **Pull:** gains `LEFT JOIN users u ON u.id = e.updated_by`, which yields `updatedBy` only when an actor is present.
- **`SYNC_TYPES`:** stays a strict allowlist. Adding a type later (for example `protoFile`) is a server-first deploy.

### 6.3 Binding invariants (the blocker fix; enforced in `apply_pull`, `upsert_remote` and `collect_push`)

- **B1. Bindings change only through an explicit local action on this device:**
  - first-sync confirm;
  - Sync location…;
  - Move to team / move out;
  - Merge legacy;
  - a project that is new to this device arriving in a space;
  - resolving a space notice.

  Pulled data never changes a binding.
- **B2. A pulled project upsert in space S:**
  - **Unknown locally**, and not in S's tombstones: create it and bind it to S.
  - **Bound to S:** normal LWW.
  - **Bound elsewhere or local-only:** ignore it. It is not marked synced, and `foreign` is incremented. This is a diagnostic only, and it is suppressed while a legacy merge is pending (AC-ONB-4). A personal project is therefore never pulled into a team, and a team copy never takes over a personal one.
- **B3. A pulled child (collection, folder, request, project environment) in S** is applied only when:
  1. the parent named in the *incoming* data (`projectId`, or `collectionId` resolved to its project) is bound to S; and
  2. if the id already exists locally, its current project is also bound to S.

  `upsert_remote` must stop the `retain` that runs across all projects (§1.3) and replace the entity only in its own project.
- **B4. The Global environment** (`project_id == None`) is accepted only from `account.globalSpace` (personal, or legacy before the merge) and pushed only there. It is never accepted from or pushed to a team space.
- **B5. A pulled tombstone in S** applies only to entities whose project is bound to S.
- **B6. Moves between spaces use new ids** [DEV-5] (§6.5). The only same-id transfer is Legacy → Personal merge (AC-ONB-3/4). It is safe because the legacy space switches to shares-only on this device in the same local transaction, so no further legacy pulls can disagree.
- **B7. Partial-run safety.** If any *active* space's pull failed transiently in a run (offline, 5xx or timeout; not Removed and not 426):
  - every space that pulled successfully applies **upserts only**;
  - spaces whose pages contained deletes keep their old cursor, so the deletes are re-pulled and the re-applied upserts are equal-version no-ops;
  - after 3 consecutive deferred runs (`deferredRuns`), deletes apply anyway.

  This stops "the tombstone landed, the destination hasn't yet" from flipping or briefly losing a project.
- **Other rules:**
  - The fresh-device "drop the untouched Default" rule (`sync.rs` ~L1261) runs only on the first pull of the personal space or a legacy space.
  - Team spaces keep the local `binaryPath` and form file values on upsert, the same way secrets are kept today.

### 6.4 Engine changes (`crates/core/src/sync.rs`)

- **`SyncClient`** gains:
  - `scope: Scope::{Legacy, Space(String)}` (the path helper maps `/sync` to `/v1/sync` or `/v1/spaces/{id}/sync`, and does the same for shares);
  - `auth: Auth::{Token(String), Session(Arc<auth::Session>)}`;
  - the `X-ReqHarbor-Version` header on every call (M0).
- **`send()` mapping:**
  - **401:** a session client refreshes once and retries; a legacy client keeps today's `Unauthorized`.
  - **New error mappings:** `SignInRequired`, `Forbidden{code}`, `SpaceGone{status}`, `RateLimited`, `ClientTooOld` and `ServerFull`.
  - **507 `server_full`:** set an issue on the space and retry with backoff, as with other 5xx errors.
- **Lenient `PullPage` (M0):** `changes: Vec<Value>`, each converted with `Change::try_from`. Unknown `type` values increment `unknownTypes` and are skipped (AC-FWD-5). `Change.updated_by` goes into the space's `attribution` map, so entity JSON is not polluted.
- **`collect_push(store, space)`:** filters by binding. For team spaces it blanks paths. Requests that are not editable (AC-FWD-3) are never pushed modified; deletes are allowed.
- **`apply_push_results_detailed`** matches on `(deleted, status, reason)`:

  | Result | Effect |
  |---|---|
  | `entity_too_large` | Marked skipped, as today |
  | `clock_skew` | The entity stays dirty; `issue = ClockSkew{aheadMs}` |
  | `forbidden_role` | The entity stays dirty, or the tombstone is **retained**; `issue = RoleChanged` |
  | `global_env_not_allowed` | Dropped silently. This only happens with a buggy client. |
  | other `rejected` | The entity stays dirty; generic warning |
  | `(true, applied \| stale)` | The tombstone is dropped, as today |

### 6.5 Moves, copies and duplicates (new ids)

- **`move_project_to_space(pid, target)`** (Move to team, AC-PROJ-4; admin move-out, AC-PROJ-5) is an **online, resumable** command:
  1. Build a plan: copy the project, its collections, folders, requests and project environments with **new ids** (reuse the re-id path of `import_snapshot_with_warnings`). Persist it as `pendingMoves[{moveId, from, to, idMap}]` in `sync-spaces.json`.
  2. Push the copy to `target` with the normal batching. Retrying reuses the same ids, so it converges.
  3. When every result is `applied`: in one local step, insert the copy bound to `target` and marked synced there, delete the source (tombstones in the source space), and clear the plan. Then `schedule_push`.
  4. On failure (offline, quota, `server_full`), the local project is unchanged and the UI shows "Nothing was moved". **Cancel** pushes tombstones for any ids already applied, then clears the plan.
- **On the user's other devices:** they pull a source tombstone (project removed) and a target upsert with new ids (a new project bound to the team). There is no conflict copy and no rebind. An unpushed local edit that is newer than the tombstone survives in the source as today's delete-versus-edit rule, so the user may see an honest duplicate but never a leak.
- **Shares and moves:** existing shares stay in the source space. Their URLs keep working (AC-TSHARE-4), and they stay listable and revocable under that space's group, but they cannot be updated from the moved copy [DEV-5].
- **Copy to project…** and **Duplicate to personal…** create a local copy with new ids into the target project, which then syncs normally. Neither touches the source.

### 6.6 Run algorithm (`src-tauri/src/sync_rt.rs`, account mode)

1. **Reconcile.** `GET /v1/me/spaces` through `Session`:
   - add bindings for new team spaces (D16, AC-TSYNC-9);
   - update roles (AC-ROLE-4);
   - process `departed[]` (§6.7).

   A 401 after refresh sets `SignInRequired`, and account spaces are skipped while legacy keeps syncing.
2. **Pull.** Every Active space in sequence (personal → legacy → teams), each wrapped in `tokio::time::timeout`, within a 30 s overall budget per run (edge case 8). Pages are held in memory.
3. **Apply.** Under the store lock, with no `await`: apply each space in the same order, using B2–B7. Then `save_all`, and `sync-spaces.json` is written atomically, with its backup.
4. **Push.** Each space, with batches and reason-aware results.
5. **Status.** Fold per-space states into `SyncStatus.spaces` plus the worst state.

**Runtime and policy changes:**
- One global `sync_lock` stays, and so does one global retry timer.
- `schedule_push` triggers when any Active binding exists (today it returns early when `workspace.sync` is `None`).
- `sync_policy::on_failure` returns `Disconnect` **only** for a legacy 401 (AC-SYNC-7 is preserved).
- `SyncState` gains `SignInRequired` and `Paused`.

### 6.7 Removal, deletion and unavailable spaces

- **A bound space is marked `Removed` only when** a `GET /v1/me/spaces` returns **200** for the same `serverUrl` and `userId` and the space is absent from it. The `reason` comes from `departed[]`, defaulting to `removed`.
- **Anything else is `Unavailable{since}`:**
  - a 403 `not_a_member` or 404 `space_not_found` on a space call;
  - a failed `/v1/me/spaces` call;
  - a different user.

  `Unavailable` means "Acme is temporarily unavailable". It backs off and **never touches local data**.
- **`SpaceNoticeDialog`:** the focused default is **Keep a personal copy** in every case [DEV-6], and **Remove from this device** is a deliberate second button.
- **Core functions:**
  - `detach_space(space, keep)`: keep turns the projects into local-only copies named "X (from Team)"; remove calls `forget_project`.
  - `save_dirty_as_personal(space)` for AC-ROLE-3 and edge case 3.
- **Closing the dialog with ×** means "decide later", which shows a 7-day "Decide" row (UX-D4). When the 7 days run out, the default applies, which is **keep**.

### 6.8 Viewer guard (AC-ROLE-1/3)

- **`Store::write_guard(project_id)`** fails with:
  - `StoreError::ReadOnly{reason: "read_only_role"}` when the project's space role is viewer;
  - `read_only_newer` for an entity that is not editable (§9).

  Every project-tree mutator calls it: `create_*`, `rename_*`, `delete_*`, `move_*`, `save_request` and project-environment edits. UI scratch edits are never persisted, so they pass.
- **In viewer spaces,** pulled upserts always win, and `conflict_copy` is never called.
- **Exception: role lowered with unpushed edits.** When a role drops to viewer (detected in step 1 of the run) and the space has dirty keys, they go into `frozen`. Remote upserts for frozen keys are skipped until the banner is resolved:
  - **Save as personal copy** runs `duplicate_to_space` with new ids, then unfreezes.
  - **Discard** unfreezes and resets the space cursor to 0, so a re-pull restores the server versions; equal versions are no-ops.
- **Rejected project tombstones** use the same Discard path.

### 6.9 Clock-skew recovery

- Each push response for an account space carries `serverTime`.
- When `|now_ms() − serverTime| < 60 s` (the clock has been fixed), the next run restamps each entity rejected for skew to `updatedAt = max(now_ms(), synced_at + 1)`.
- The issue text then becomes: "Your clock was corrected; 3 changes will sync now."

---

## 7. Migration and backward compatibility

| Who | Behaviour |
|---|---|
| Old client on a legacy workspace | Unchanged. Routes, responses and 401 behaviour are byte-identical. The only change is the 507 breaker, which old clients already retry as a 5xx. The new columns have defaults. |
| New client, legacy only, never signs in | Pure-legacy mode: the same files and the same behaviour. The token stays in `workspace.json` (OS keychain remains PRODUCT §2.2 Later for this mode). Settings shows the connection under Advanced. |
| Signs in on an install that has legacy sync | A one-time local migration writes `account.json` with `legacy:{…, mode:"sync"}` and binds every existing project to `legacy`. The legacy token moves to the secret store, the cursor and bookkeeping move into `sync-spaces.json["legacy"]`, and `sync` is **removed from `workspace.json`**. A downgraded client then sees "not connected" and cannot push team projects into the legacy workspace (QA downgrade item). The first-sync checklist lists nothing, because every project is legacy-bound, and the user is offered **Merge into my personal space**. |
| Merge (AC-ONB-3) | One local transaction:<br>1. Rebind legacy projects and `globalSpace` to personal (B6; same ids) and `reset_space(personal)` for those keys.<br>2. Set `legacy.mode = "sharesOnly"`; legacy pulls and pushes stop and legacy share calls keep working.<br>3. `schedule_push`.<br>A partial failure leaves the unpushed items dirty; **Merge again** is idempotent. Nothing is invalidated on the server. |
| A second device still on the token | Keeps syncing the legacy space. While it is signed in but not yet merged, personal upserts for legacy-bound ids are ignored (B2, warning suppressed). After it merges, LWW converges (AC-ONB-4). |
| Upgrade after a downgrade | `account.json` is authoritative and never re-derived. A project created by an old client has no binding, so it is local-only, which is the safe direction. |
| First sign-in with local projects | `FirstSyncDialog`; nothing is bound until the user confirms (AC-ONB-5). |
| Fresh device | Pull personal plus all teams, then drop the untouched Default (AC-ONB-6). |

There is no server claim endpoint and no `claimed_tokens`, which follows spec revision 2 (D20a).

---

## 8. Secrets handling

**Server**
- Stored only as SHA-256: access, refresh, invite and auth-code tokens, and the auth cookie.
- The refresh grace pair is stored only as AES-GCM ciphertext under a key derived from the old refresh token.
- OTP (M4): HMAC-SHA256 with `OTP_PEPPER`.
- Upstream provider tokens are used once and never stored.
- `stripSecretValuesForSync` runs for every space. Team spaces also run `stripDeviceLocalPaths`.
- Logs carry stacks only (`index.ts`). A test asserts that error bodies never echo a token.

**Client**
- No token appears in `workspace.json` (in account mode), `account.json`, logs, `SyncStatus` or error strings.
- Share options for account spaces live on the server (`shares.options`, AC-TSHARE-3). Legacy shares keep `settings.shareOptions`.
- The Move/Copy pre-flight reuses the `share.rs` credential scanner (`share_preview`) and only warns (D8a). The convert-to-secret action is Later.

---

## 9. Forward compatibility for gRPC and GraphQL (client M0, before any team sync)

1. **Unknown-field passthrough.** Add `#[serde(flatten)] pub extra: serde_json::Map<String, Value>` to:
   - `Workspace` and `SyncState`;
   - `Project`, `Collection`, `Folder`, `Request` and `Environment`;
   - `Variable`, `KeyValue`, `Auth`, `ApiKeyAuth`, `Body`, `GraphqlBody`, `FormField` and `RequestSettings`.

   `Default` impls get an empty map. `upsert_remote`, `conflict_copy`, duplicate and import carry `extra` along. **`Store::save_request` merges `extra` from the stored version for every key the update lacks**, because the UI may rebuild objects without unknown keys.
2. **`Request.kind: RequestKind`** with values `Http | Graphql | Grpc | Other(String)` and string serde through a small macro.
   - When the value is `Http` it is omitted (`skip_serializing_if`), so old readers see no change.
   - Future shapes: `kind:"graphql"` plus `graphql:{…}`; `kind:"grpc"` plus `grpc:{target, service, method, protoRef, metadata[], message}`; a future `protoFile` sync type added server first; R2 later for anything over 512 KB.
3. **`BodyType::Other(String)`**, which round-trips the original string. All 17 `BodyType::` matches in `send.rs`, `prepare.rs` and `share.rs` handle it. Sending a non-HTTP kind or an `Other` body returns "This request needs a newer version of ReqHarbor".
4. **`Request::is_editable()`** is false for a non-HTTP kind, a `BodyType::Other`, or `minClientVersion > CARGO_PKG_VERSION` (AC-FWD-3/4). The store guard returns `read_only_newer`.
5. **Share page (Worker M1, AC-FWD-6).** `normalizeSnapshot` (`snapshot.ts`) reads `request.kind`. Any kind other than `http` (or absent) renders "This request type can't be previewed on the web. Import it into ReqHarbor." instead of the `method ?? "GET"` fallback.
6. **Legacy caveat.** Pre-M0 clients still erase unknown fields in legacy workspaces. `GET /v1/spaces/:id` and the new legacy `GET /v1/workspace/clients` return `oldestClientVersion` (from `client_versions`, last 30 days), so future gRPC and GraphQL editors can warn: "A device syncing this space runs an older ReqHarbor and may erase this request's details."

---

## 10. `docs/types.ts`, the UI mirror and the mock backend

**Types.** These go into both files, which must stay byte-identical:
- `Account`, `AuthProviders`, `SignInStatus` (`idle | checking | waiting | exchanging | success | error`, plus `error?: CommandErrorCode`; v1 has no `paste`), `SessionInfo`;
- `SpaceKind`, `SpaceRole`, `SpaceInfo`, `SpaceStatus{spaceId, kind, name, state, issue?, lastSyncAt, pendingChanges}`, `SpaceIssue` (`quota | ownerQuota | serverFull | clockSkew | roleChanged | unavailable | clientTooOld | removed`), `ProjectLocation`;
- `TeamSummary`, `TeamDetail`, `TeamMember`, `InviteInfo`, `InvitePreview`, `MovePreview` (the share-preview shape plus `unresolvedVars`), `SpaceNotice`;
- `ShareInfo` gains `createdBy`, `options` and `spaceId`. `SyncStatus` gains `spaces`, `account` and `keychain: "system" | "file" | "unavailable"`. `SyncStateName` gains `"signInRequired" | "paused"`;
- `Request.kind?: "http" | "graphql" | "grpc" | (string & {})`, `minClientVersion?: string`, and `BodyType | (string & {})`;
- events `auth://status`, `sync://space-notice`;
- the §5.7 codes plus `sign_in_cancelled`, `sign_in_timeout`, `loopback_unavailable`, `keychain_unavailable`, `read_only_role` and `read_only_newer`.

**Tauri commands** (`commands.rs` and the `Commands` map):

| Group | Commands |
|---|---|
| Auth | `auth_providers(serverUrl)`, `auth_sign_in_start(serverUrl)`, `auth_sign_in_cancel`, `auth_sign_out`, `auth_me`, `auth_sessions`, `auth_session_revoke(id)`, `auth_delete_account` |
| Spaces | `spaces_list`, `project_locations`, `project_set_location(projectId, spaceId\|null)`, `first_sync_apply(projectIds)`, `move_preview(projectId, target)`, `move_project(projectId, target)`, `move_cancel(moveId)`, `copy_to_project(kind, id, targetProjectId)`, `space_notice_resolve(spaceId, keep)`, `role_change_resolve(spaceId, "savePersonal"\|"discard")`, `legacy_merge`, `legacy_disconnect` |
| Teams | `teams_list`, `team_create`, `team_get`, `team_rename`, `team_delete`, `team_set_role`, `team_remove_member(uid, revokeInvites)`, `team_leave`, `team_transfer`, `invite_create`, `invite_list`, `invite_revoke`, `invite_preview(url)`, `invite_accept(url)` |

`share_*` keep their signatures; the space comes from the project binding. `API_CONTRACT.md` §3.9 and §3.10 get these commands, the refresh and 401 semantics, and the B1–B7 invariants.

**Mock.** `ui/src/lib/api/mock/index.ts` is 1,389 lines, so split it into `mock/account.ts` and `mock/teams.ts`. They seed the team "Acme" (Priya owner, Sam editor, Olu viewer), invites and per-space status. The mock mirrors the §5.3/§5.4 role rules, including `forbidden_role`, `transfer_required` and B2 ignoring foreign ids. Switches:
- `?mockAuth=ok|timeout|offline|denied|email_in_use|no_port`
- `?mockRole=viewer|editor|admin|owner`
- `?mockSpace=removed|unavailable|quota|server_full|skew|client_too_old`

Add `ui/e2e/accounts.spec.ts` and `ui/e2e/teams.spec.ts`, using the UX §14 test ids.

---

## 11. Configuration and deploy

**`worker/wrangler.toml` additions**
```toml
[vars]
PUBLIC_BASE_URL = "https://<host>"        # REQUIRED with any OAuth provider (callback URLs, invite links)
ALLOW_ANON_WORKSPACES = "true"             # D4a
TEAMS_ENABLED = "false"                    # flip only after client M0 is widely installed (§13 release order)
QUOTA_PERSONAL_BYTES = "26214400"
QUOTA_TEAM_BYTES = "26214400"
QUOTA_OWNER_BYTES = "104857600"
DB_SOFT_LIMIT_BYTES = "419430400"          # 8589934592 on D1 Paid (D10)
MAX_OWNED_TEAMS = "3"
MAX_TEAM_MEMBERS = "50"
MAX_TEAM_INVITES = "20"
# MIN_CLIENT_VERSION = ""                  # optional 426 gate on /v1/spaces/*
# DEV_AUTH = "1"                           # local/tests only, host-guarded

[[ratelimits]]
name = "AUTH_LIMITER"
namespace_id = "1001"
simple = { limit = 20, period = 60 }

[[ratelimits]]
name = "SYNC_LIMITER"
namespace_id = "1002"
simple = { limit = 120, period = 60 }

[triggers]
crons = ["17 3 * * *"]
```

- **D1:** replace the placeholder `database_id` with the id from `npx wrangler d1 create reqharbor`, then run `npm run db:migrate:remote`.
- **Types:** run `npm run cf-typegen`, or extend `AppEnv` in `http.ts`.
- **Tests:** `vitest.config.ts` adds `DEV_AUTH: "1"` to `miniflare.bindings`.

**Secrets** (`wrangler secret put`):
- `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`
- `GOOGLE_CLIENT_ID`, `GOOGLE_CLIENT_SECRET`
- M4: `OTP_PEPPER`, plus `RESEND_API_KEY` or a Cloudflare Email Service binding

**OAuth apps**
- **GitHub:** Developer settings → **OAuth App** (not a GitHub App).
  - Callback `https://<host>/auth/callback/github`.
  - Scopes `read:user user:email`.
  - Device flow off.
- **Google Cloud:**
  1. Consent screen: External, scopes `openid email profile`, then publish to Production.
  2. Client type **Web application**, because the Worker is the confidential client.
  3. Redirect URI `https://<host>/auth/callback/google`.

**Desktop**
- Build-time `REQHARBOR_DEFAULT_SERVER` (D14a) and `REQHARBOR_MACOS_KEYCHAIN` (set by CI only with Developer ID signing).
- Crates:
  - `crates/core`: `ring = "0.17"` and tokio `net`;
  - `src-tauri`: `tauri-plugin-opener = "2"` and `keyring = "3"`.
- Verify they build with `docker compose run --rm dev cargo build --workspace`.

**`worker/README.md` (self-hosters):** the steps above, the D1 plan limits and the breaker, and the privacy note (PRIV-6: D1 Time Travel keeps up to 30 days).

---

## 12. Phased implementation plan

Releases follow product §13 exactly: client M0 → Worker M1 → client M1 → Worker M2 → client M2. Each row below can ship on its own.

**How tests run:**
- Rust: `docker compose run --rm dev cargo test --workspace`
- Worker: `docker compose run --rm -w /app/worker dev npm test`
- UI: Playwright inside the same container

| Phase | Files | Tests |
|---|---|---|
| **M0 client: forward-compat** | `crates/core/src/{model.rs, storage.rs, sync.rs, send.rs, prepare.rs, share.rs}`; `docs/types.ts` + `ui/src/lib/api/types.ts`; `ui/src/lib/components/{CollectionsTree, RequestPane, Icon}.svelte`; mock | Proptest: unknown fields at every level survive deserialise → edit → serialise, including `Workspace` and `SyncState`. `save_request` keeps `extra`. An unknown `body.type` round-trips byte-equal. A pull page with an unknown `type` is skipped and the rest applies. Rejected tombstones are retained, and `clock_skew` and `forbidden_role` results are handled per reason. `X-ReqHarbor-Version` is sent (wiremock). Playwright: a "newer version" request is read-only. |
| **M1a Worker: auth core** | `migrations/0003_accounts.sql`; `src/{crypto, oauth, auth, auth-pages, me, ratelimit}.ts`; `http.ts` (`allowCookie`); `index.ts`; `wrangler.toml`; `vitest.config.ts`; `test/helpers.ts` (`signIn()`, `spaceCall()`) | `auth.test.ts`: the PKCE happy path; a wrong verifier, code reuse and an expired code; redirect validation (rejects `localhost`, `https`, `oob`, port < 1024, an extra path); refresh rotation; the **grace window returns an identical pair within 60 s and revokes after**; reuse revocation; logout; the 20-session cap; `email_in_use` refused with no rows created; returning-user email update; **`Set-Cookie` only under `/auth/*`**; **a `/v1` handler ignores a forged cookie**; no token echo in errors. |
| **M1b Worker: spaces** | `src/{authz, capacity}.ts`; `sync.ts`, `quota.ts`, `shares.ts`, `sanitize.ts`, `snapshot.ts`, `share-page.ts`, `workspaces.ts`; `scheduled()` | `spaces.test.ts`: a personal space is created at sign-in; a pull/push round trip with `updatedBy` and `serverTime`; another user's space returns 403 and an unknown one 404; a legacy id via `/v1/spaces` returns 404; expired access returns 401 `session_expired`; `clock_skew` per change; the personal quota and the owner aggregate; the breaker returns 507 on growth and allows deletes; `client_versions` recorded and `''` for pre-M0; a gRPC-kind share page shows the fallback card. **Regression:** `sync.test.ts`, `shares.test.ts`, `http.test.ts` and `qa_parity_security.test.ts` pass unchanged. |
| **M1c client: secrets and sign-in** (behind the `accounts` flag until M1d) | `core/src/{auth, secrets}.rs`; `src-tauri/src/{secret_store, auth_rt}.rs`, `commands.rs`, `main.rs` (opener plugin), `state.rs`, `Cargo.toml`; UI `SignInFlow`, `Avatar`, `AccountSection` (split out of `SettingsDialog.svelte`) including Devices and Sign out; `mock/account.ts` | Core: **the loopback ignores stray requests and a wrong `state` and completes on the matching `/cb`**; timeout and cancel; a single-flight refresh under concurrent calls makes exactly one HTTP refresh; a 401 refreshes and retries; a failed refresh gives `SignInRequired` with data intact; persist-before-use ordering; the file store is 0600. Playwright: sign-in states including `no_port`. |
| **M1d client: personal sync and bindings** | `core/src/{storage.rs (account.json, sync-spaces.json, space_state_mut, binding-aware tombstones, write_guard), sync.rs (Scope, B1–B7, move engine, merge, restamp)}`; `src-tauri/src/{sync_rt, sync_policy}.rs`; UI `FirstSyncDialog`, `NewProjectDialog`, `SyncLocationDialog`, the Sync section rework (Advanced holds legacy), `MergeLegacyDialog`, the grouped project menu (`MenuItem.heading`), the footer tooltip | Core:<br>• the sign-in migration removes `sync` from `workspace.json` and preserves the cursor and bookkeeping;<br>• a local-only project produces no traffic;<br>• Global env only to `globalSpace`;<br>• **B2: a known id from another space is ignored and the binding is unchanged**;<br>• **B3: a child with a foreign parent, or a foreign existing id, is not relocated** (regression for today's `retain`);<br>• **B7: a partial run defers deletes and does not flip**;<br>• merge convergence across two devices with no duplicates;<br>• **removal only on an authoritative `/v1/me/spaces`; a stray 404 leads to `Unavailable` with no data change**;<br>• clock-skew restamp;<br>• `sync_policy`: Disconnect only for a legacy 401. |
| **M2a Worker: teams** | `migrations/0004_teams.sql`; `src/{teams, invites, invite-page}.ts`; role enforcement in `sync.ts` and `shares.ts`; `stripDeviceLocalPaths`; `departed[]` in `/v1/me/spaces`; `TEAMS_ENABLED`, `MIN_CLIENT_VERSION` | `teams.test.ts`:<br>• **a table-driven matrix of actor role × target role × requested role** for PATCH and DELETE members;<br>• billing owner returns `transfer_required`; last owner;<br>• **transfer racing a removal leaves ≥ 1 owner**;<br>• **cross-team id substitution** (invite `:iid` from team B, `:uid` not in team, another user's session id) returns 404;<br>• parallel accepts of a single-use link: exactly one wins;<br>• a removed user with a pre-removal link returns 410, and a post-removal link works;<br>• member and invite limits; admin links single-use and owner-only;<br>• a viewer push returns 403; an editor's project tombstone gets a per-change `forbidden_role`;<br>• `global_env_not_allowed`;<br>• `binaryPath` stripped in team and kept in personal;<br>• team delete cascades and share pages return 404;<br>• `/i` CSP. |
| **M2b client: team UI** | Core `move_project_to_space` (online and resumable), `copy_to_project`, `duplicate_to_space`, `detach_space`, `save_dirty_as_personal`, viewer `frozen` handling; Tauri team, invite and move commands; UI `TeamsSection`, `TeamDetail`, `InviteDialog`, `JoinTeamDialog`, `CreateTeamDialog`, `TransferOwnershipDialog`, `MoveToTeamDialog`, `CopyToProjectDialog`, `SpaceNoticeDialog` (Keep focused), viewer affordances, "Edited by", conflict badges, Shares grouped with the creator; `ConfirmState.{typeToConfirm, details, checkbox}`; `mock/teams.ts` | Core:<br>• **a malicious editor re-pushing a personal project's old ids into the team: the personal copy and its binding are untouched**;<br>• **a stale offline edit beats the team tombstone after a move-out: the team gets the resurrection, the personal copy is unaffected**;<br>• **partial run during a move (personal OK, team fails): no flip, it converges next run**;<br>• a move resumes after a crash with the same ids;<br>• the viewer guard blocks mutators and creates no conflict copies;<br>• a role downgrade freezes dirty keys.<br>Playwright: join, viewer, move pre-flight, removal notice. |
| **M3: polish and v1 QA** | `DELETE /v1/me` and the delete-account UI; role-change and removal flows; mock parity audit; docs (PRODUCT §2.1, §2.2, §5.10, §8, §9; API_CONTRACT; DESIGN §5.12/§5.14; worker README) | Worker: **after account deletion, zero `workspaces`, `entities` and `shares` rows remain for the personal space and sole-member teams, and personal share URLs return 404**; co-owned teams survive with billing reassigned; deletion is blocked as last owner, including the forced-FK race. Manual matrix on macOS, Windows and Linux: browser hand-off, keychain or file store, **macOS upgrade from build N to N+1 stays signed in**. |
| **M4** | Email OTP (`0005`, pluggable sender); explicit "Link another method" (drop `identities_one_per_user`); **paste-code**; device rename; "Keep this version"; collapsible groups | OTP: 5 attempts per code, **10 failed verifies per day per email across codes, then a 24 h lock with a notice email**, HMAC storage. Paste-code is single-use and shown only after the interstitial. |

**M4 paste-code requirements**, from the QA mitigation:
- The hosted interstitial before the code names the device and the start time, and requires clicking "I started this on my own computer".
- One code per auth request.
- An `oob` session is flagged "New sign-in with a code", both in Devices and on the app's next start.

---

## 13. Deferred (not blocked by this design)

- Passkeys on the hosted page
- `reqharbor://` deep links
- Emailed invites
- Several spaces per team (drop `workspaces_one_per_team`)
- Per-collection access control
- Server-assigned timestamps (D7c)
- Field-level merge
- End-to-end encryption
- Audit log
- Turnstile
- SSO through a broker provider
- Cloudflare Access mode
- R2 for proto bundles
- Tombstone GC
- A server-side legacy claim (D20b)
- Moving shares along with a moved project
- A "recovered edits" project for unpushed edits under a deleted project (today's delete-wins-over-older-edit rule stays)

---

## 14. Effects on the UX spec (for the designer)

- **Sign-in waiting view:** drop **Paste code instead** in v1. Add an error for a failed loopback bind: "ReqHarbor couldn't start sign-in on this computer. You can still sync with a token under Sync › Advanced." The done page shows no code.
- **`SpaceNoticeDialog`:** the focused primary is always **Keep a personal copy**. A new `Unavailable` row reads "Acme is temporarily unavailable · Retrying", with no dialog.
- **Move pre-flight:** add the line "Existing share links stay with your personal space and keep working." Errors keep "Nothing was moved".
- **Team detail:** the billing owner's role select and Remove are disabled with "Transfer ownership first". Admins see the owner and admin rows read-only.
- **macOS file store:** use the warning copy from §3.3.

---

## 15. Deviations from the PO and UX specs (for owner sign-off)

1. **[DEV-1]** Hosted HTML lives under `/auth/*` and space routes under `/v1/spaces/:id/*`, not `/v1/auth/authorize` and `/v1/s/:id`.
2. **[DEV-2]** No `workspaces` rebuild. Sentinel `token_hash` values are used, because a rebuild would cascade-delete data.
3. **[DEV-3]** AC-AUTH-4 paste-code is deferred to M4, with an interstitial. v1 is loopback-only because of the `oob` phishing risk.
4. **[DEV-4]** The personal space is the workspace of a hidden personal team. `teams.owner_user_id` is the billing owner.
5. **[DEV-5]** Moving a project to or from a team uses **new ids**. "Same entity ids" holds only for the legacy merge. As a result, existing shares stay in the source space: they keep working and can be revoked, but cannot be updated from the moved copy.
6. **[DEV-6]** The removed-from-team notice defaults to **Keep a personal copy** in every case (D5 had Remove for removals by an admin).
7. **[DEV-7]** Ad-hoc-signed macOS builds store tokens in a 0600 file rather than the keychain until Developer ID signing exists.
8. **[DEV-8]** The billing owner cannot be demoted or removed by a co-owner without a transfer first (409 `transfer_required`). Admins can act only on viewers and editors.

**Withdrawn from my previous revision:**
- The server claim endpoint (follows spec D20a).
- "No refresh grace" (replaced by the 60 s idempotent window).
- Using 404 for non-members (now 403 `not_a_member`, as in the spec).

Files read (none modified):
- `worker/migrations/0001_init.sql`
- `worker/migrations/0002_device_id_and_quota.sql`
- `worker/wrangler.toml`
- `worker/package.json`
- `worker/vitest.config.ts`
- `worker/src/{index,workspaces,sync,http,quota,sanitize,crypto,shares,snapshot,share-page}.ts`
- `worker/test/{helpers,apply-migrations,env.d}.ts`
- `crates/core/Cargo.toml`
- `crates/core/src/{model,sync,storage}.rs`
- `src-tauri/Cargo.toml`
- `src-tauri/src/{main,sync_rt,sync_policy,state}.rs`
- `Cargo.toml`
- `compose.yaml`
- `docker/dev.Dockerfile`
- `.github/workflows/desktop-build.yml`
- `docs/types.ts` (identical to `ui/src/lib/api/types.ts`)
# ReqHarbor sync, sharing and workspaces: current-state map

I only read files for this. Nothing in the repo was changed.

## 1. Mental model in one paragraph
A remote **workspace** is a bag of opaque JSON entities, keyed by `(workspace_id, type, id)`. Whoever holds its single bearer token (`rh_…`) can use it. There are no users, sessions, roles or membership. Every device or person with the token has full read/write/rotate/share rights. One desktop install (one local `Store`) can be connected to **at most one** remote workspace, and **everything** in it syncs: all projects, collections, folders, requests and environments. Share links hang off the workspace and are public read-only snapshots that anyone with the URL can open.

## 2. D1 data model (`worker/migrations/0001_init.sql`, `0002_device_id_and_quota.sql`)
| Table | Columns | Notes |
|---|---|---|
| `workspaces` | `id` TEXT PK (ULID), `name` TEXT, `token_hash` TEXT UNIQUE (hex SHA-256 of token), `rev` INT (per-workspace counter), `created_at` INT ms | The token is the identity. There is one token per workspace. |
| `entities` | `workspace_id` FK→workspaces ON DELETE CASCADE, `type` TEXT, `id` TEXT, `rev` INT, `updated_at` INT, `deleted` INT (0/1), `data` TEXT (JSON, NULL for tombstone), `device_id` TEXT default '' (0002), `size` INT (0002) | PK `(workspace_id,type,id)`. UNIQUE index `(workspace_id, rev)`. Index `(workspace_id,size)`. Tombstones are kept **forever** with no GC. The server has **no parent/collection column**: hierarchy exists only inside `data` JSON. |
| `shares` | `id` TEXT PK (22-char base62, 128-bit), `workspace_id` FK CASCADE, `kind` CHECK('collection','request'), `entity_id`, `snapshot` TEXT (NULL after revoke), `created_at`, `updated_at`, `revoked_at`, `size` (0002) | Index `(workspace_id, created_at)` and `(workspace_id, revoked_at, size)`. `entity_id` is never checked against `entities`. |
| `rate_limits` | `key` PK (`"ws_create:"+sha256(ip bucket)`), `window_start`, `count` | Generic fixed-window table that can be reused for login and OTP limits. |

## 3. Worker endpoints (`worker/src/index.ts` router)
Auth is `authed()`, which wraps `authenticate()` in `workspaces.ts`. It parses `Authorization: Bearer`, checks `TOKEN_RE /^rh_[A-Za-z0-9_-]{43}$/`, looks the token up by `sha256Hex(token)` and does a constant-time compare. A failure returns 401 with `WWW-Authenticate: Bearer realm="reqharbor"`.

| Method & path | Auth | Handler | Purpose |
|---|---|---|---|
| POST `/v1/workspaces` `{name≤200}` | none, 10/h per IPv4 or IPv6 /64 | `workspaces.createWorkspace` | Creates the workspace and returns `201 {workspaceId, token}`. The token is shown once and only its hash is stored. |
| GET `/v1/workspace` | bearer | `getWorkspace` | `{workspaceId,name,createdAt,entityCount,bytesUsed,quotaBytes}` |
| POST `/v1/workspace/token/rotate` | bearer | `rotateToken` | Compare-and-swap on `token_hash` (contract C4). The old token dies immediately and a concurrent loser gets 401. |
| GET `/v1/sync?since=&limit=` | bearer | `sync.pull` | Returns rows with `rev > since` ordered by rev, limit 1..500, as `{changes,cursor,hasMore}`. On an empty page the cursor is the workspace head. |
| POST `/v1/sync` `{changes[≤500]}` | bearer | `sync.push` | LWW upsert in one D1 batch. Returns `{results:[{type,id,status:applied\|stale\|rejected,rev\|reason}],cursor}`. |
| POST `/v1/shares` `{kind,entityId,snapshot}` | bearer | `shares.createShare` | `201 {shareId,url,createdAt,updatedAt}` |
| GET `/v1/shares` | bearer | `listShares` | Active shares only. The name comes from `json_extract` on the snapshot. |
| PUT `/v1/shares/:id` `{snapshot}` | bearer | `updateShare` | Replaces the snapshot behind the same URL. Returns 410 if revoked. |
| DELETE `/v1/shares/:id` | bearer | `revokeShare` | Idempotent. The snapshot is nulled. |
| GET `/s/:id` | public | `publicSharePage` | Server-rendered HTML (`share-page.ts`) with a strict hash-based CSP, `noindex` and `max-age=60`. |
| GET/OPTIONS `/s/:id.json` | public | `publicShareJson` | Snapshot JSON with CORS `*`. Used by "Import from link". |

Cross-cutting behaviour in `http.ts`:
- JSON-only bodies are enforced with a 415 on any other content type. There is a 5 MB streaming cap.
- Errors use one shape: `{error:{code,message}}`.
- `withSecurityHeaders` adds HSTS, `X-Frame-Options: DENY` and API CSP `default-src 'none'; form-action 'none'`. It also **deletes every `Set-Cookie`**.
- There is no CORS on `/v1/*`, because the Rust core makes all `/v1` calls.
- `PUBLIC_BASE_URL` is an optional var for share URLs.
- The `wrangler.toml` D1 `database_id` is still a placeholder (all zeros).

## 4. Limits and quotas (`http.ts` `LIMITS`, `quota.ts`)
- Request body ≤ 5 MB, entity `data` ≤ 512 KB (per-change `rejected: entity_too_large`), snapshot ≤ 1 MB, ≤ 500 changes per push, pull page ≤ 500.
- Quota is **50 MB per workspace**: `SUM(entities.size) + SUM(active shares.size)`. Only writes that grow usage are blocked, with 413 `quota_exceeded`.
- The only rate limit is workspace creation (10/h per IP bucket). **Sync, shares and auth failures are not rate-limited at all.**

## 5. Sync protocol
**Server (`worker/src/sync.ts`)**
- Synced types are a whitelist, `SYNC_TYPES = project, collection, folder, request, environment`. Anything else gets 400 `unsupported_type`, and history and settings never sync.
- Ids match `/^[A-Za-z0-9_.:-]{1,128}$/`. They are client-generated ULIDs.
- Push reserves `n` revs with `UPDATE workspaces SET rev=rev+n`, then runs multi-row `INSERT … ON CONFLICT DO UPDATE`, 14 rows per statement because of D1's 100-parameter limit.
- The LWW rule is decided inside SQL: incoming wins iff `updatedAt >` stored, or `updatedAt` is equal and `deviceId >=` stored.
- **A stale push still bumps the row's `rev`**, so every client re-pulls the winner.
- The result is `applied` iff the stored `(updated_at, device_id)` equals the incoming values.
- For environment data, `stripSecretValuesForSync` blanks secret values server-side as defense in depth.
- The workspace `rev` row is a single hot counter that every push writes.

**Client (`crates/core/src/sync.rs`, `storage.rs`, `model.rs`)**
- Config lives in `Workspace.sync: Option<SyncConfig{worker_url, remote_workspace_id, token, cursor}>`. It is persisted in **`workspace.json` in plaintext**. The OS keychain is listed as "Later" in PRODUCT §2.2.
- Bookkeeping is in `sync-state.json`, with a backup at `sync-state.backup.json`. `SyncState` holds:
  - `device_id`: stable ULID, the tie-breaker.
  - `tombstones[]`: created only for entities already in `synced`.
  - `synced`: map from `"type:id"` to the `updatedAt` known to the server.
  - `origins`: map from key to the remote deviceId.
  - `sync_skipped`: too-large items.
  - `last_sync_at`
  - Keys are **not namespaced by remote**.
- `collect_push` sends every local entity whose `updatedAt` ≠ `synced[key]`, ordered parents first, then tombstones. `plan_push`/`batch_changes` cap batches at 500 changes and 4 MB.
- `apply_pull` does the following:
  - Keeps the latest change per entity by rev, then applies upserts parents-first and deletes children-first.
  - Uses the LWW key `(updatedAt, deviceId)`.
  - When a dirty local edit loses, it keeps the local version as a `"<name> (conflict)"` sibling with a new id (`conflict_copy`).
  - Lets a local tombstone that is newer than the remote change win.
  - Cascades deletes locally through `remove_local`.
  - Keeps local secret variable values on pull.
  - Merges the Global env (`project_id == None`, one per workspace), and the merged copy gets a fresh `updatedAt` and is re-pushed.
  - On a fresh device, drops the empty never-synced "Default" project.
- `upsert_remote` overwrites `workspace_id` inside data with the local `store.workspace.id`, so `workspaceId` in the JSON is meaningless on the server.
- `connect()` / `commands.rs::connect` validates the token with `GET /v1/workspace`, sets `cursor=0`, and calls `reset_for_new_remote()`. That means **everything local is treated as unsynced and pushed into the remote**: connecting merges the local tree into the workspace.
- `disconnect` clears the config and keeps local data.
- Clock: `updatedAt` is the client wall clock (`now_ms`), so a skewed clock wins LWW.

**Runtime (`src-tauri/src/sync_rt.rs`, `sync_policy.rs`, `state.rs`, `main.rs`)**
- A sync runs pull then push.
- Triggers:
  - on startup when connected (`main.rs` setup);
  - on window focus, at most once per 60 s (`pull_if_stale`);
  - 2 s after any mutation (`schedule_push` debounce);
  - when the user clicks "Sync now".
- `sync_lock` serialises runs with token rotation.
- Failure handling:
  - Offline/5xx: backoff from 5 s up to 5 min with a single retry timer.
  - **401: auto-disconnect** (AC-SYNC-7), and the status becomes `unauthorized`.
- Events: `sync://status` and `sync://changed`.

**Tauri commands (`src-tauri/src/commands.rs` ~L620–870):**
- Sync: `get_sync_status`, `sync_create_workspace(workerUrl,name?)`, `sync_connect(workerUrl,token)`, `sync_disconnect`, `sync_rotate_token`, `sync_workspace_info`, `sync_now`.
- Shares: `share_preview`, `share_create`, `share_update`, `share_revoke`, `share_list`, `preview_share(url)`, `import_share(url,projectId)`.
- The contract is in `docs/API_CONTRACT.md` §3.9/§3.10 and the types in `docs/types.ts` (`SyncStatus`, `RemoteWorkspace`, `CreatedWorkspace`, `ShareInfo`, error codes `sync_*`, `share_gone`).

## 6. Share links
- `crates/core/src/share.rs::build_snapshot` builds a client-side sanitised snapshot: `{schemaVersion:1, collection{…,folders,requests} | request, environment?}`.
- Sanitising rules:
  - Secret variables are always dropped.
  - Unless "Include credentials" is set, `Authorization`, `Proxy-Authorization` and `Cookie` headers and auth fields become `{{placeholders}}`.
  - A request share inlines its collection's auth when the request uses Inherit.
- The Worker re-validates in `snapshot.ts::prepareSnapshot` and removes secret variables again.
- Rendering uses `normalizeSnapshot`, which assumes the HTTP request shape. Its body types are `none|json|xml|text|form|formEncoded|graphql|binary`, and anything unknown becomes `none`.
- Import (`import_snapshot_with_warnings`) copies with new ids and clears file paths.
- Possession of the URL equals read access.
- Ownership is the workspace, so anyone with the workspace token can update or revoke any share.
- Options for "Update share" are kept **per device** in `settings.json → shareOptions`, which never syncs (QA-R2-002). Other devices and teammates fall back to re-asking in the dialog.

## 7. UI surfaces
- **`ui/src/lib/dialogs/SettingsDialog.svelte`**
  - Sync section: Worker URL, "Create workspace" or "Connect with token", the one-time token box, the status line, the storage meter (`sync_workspace_info`), and Sync now / Rotate token / Disconnect, plus the unauthorized banner and error mapping.
  - Shares section: table with update and revoke.
  - Section enum is `general|sync|shares|shortcuts|about`, mirrored in `ui.svelte.ts`.
- **`ui/src/lib/dialogs/ShareDialog.svelte`**: preview of stripped items, include credentials, attach environment, create or update, revoke, and a "Sharing needs sync" state.
- **`ui/src/lib/components/Sidebar.svelte`** (~L130–275): footer sync indicator for synced / syncing / offline / "Sync off" / warning.
- **`ui/src/lib/components/CollectionsTree.svelte`**: "Share…" menu items (L119, L143) and the "too large" not-synced badge (L384).
- **`ui/src/lib/dialogs/ImportDialog.svelte`**: Link tab (`preview_share` / `import_share`).
- **`ui/src/lib/stores/app.svelte.ts`**: `sync` state, listeners for `sync://status` and `sync://changed` (conflict toasts), `shareOptions` / `rememberShareOptions`.
- **`ui/src/lib/api/mock/index.ts`** (~L480–600, L1172–1270): a mock sync/share backend that must keep parity with the real one.
- Design spec is in `docs/DESIGN.md` §5.12 Settings and §5.14 Sync indicator.

## 8. What changes to add user accounts and team-owned collections
**Worker / D1**
- New migration `0003_…`. Draft tables:
  - `users`
  - `identities` (provider and subject, or email)
  - `sessions` (refresh-token hash, device label, expiry, revoked)
  - `teams`
  - `memberships(team_id,user_id,role)`
  - `invites`
- `workspaces` also needs new columns:
  - owner, either `owner_user_id` or `team_id`, plus `kind personal|team`;
  - `token_hash` relaxed from NOT NULL/UNIQUE to nullable, or kept for legacy anonymous workspaces.
- Recommendation: model **team collections as team-owned workspaces**. This reuses `entities`, `rev`, the quota and shares unchanged. The alternative is adding a `collection_id`/`scope` column to `entities`. That is needed if access control must be per collection inside one workspace, because the server cannot see hierarchy today.
- `workspaces.ts::authenticate` must resolve either a legacy `rh_` token or a user session token. It then needs a membership/role check per `workspaceId`, which means routes must carry `/v1/workspaces/:id/sync` etc. rather than an implicit workspace. `index.ts` routes and the `authed` wrapper change accordingly.
- New endpoints:
  - auth start/callback (OAuth device flow or loopback, or email OTP), refresh and logout;
  - `GET /v1/me`;
  - list my workspaces;
  - team CRUD, invites and members;
  - claim/attach a legacy token workspace to an account.
- Rate-limit login, OTP and refresh with `rate_limits`, and add limits for sync and shares.
- `withSecurityHeaders` strips `Set-Cookie`, and the API CSP has `form-action 'none'`. Any browser login page needs its own headers, as `share-page.ts` already does. For the desktop client, prefer bearer access and refresh tokens over cookies.
- Shares need `created_by` (user) and role checks so that, for example, only an editor can update or revoke. `listShares` is scoped per workspace.
- Quotas need to be per team or per user rather than per workspace only.
- Workers cannot send email freely, so magic links or OTP need a provider. OAuth (GitHub/Google) avoids that.
- Tests: `worker/test/sync.test.ts`, `shares.test.ts`, `http.test.ts`, and `helpers.ts`, which creates workspaces by token.

**Rust core**
- `model.rs`: `Workspace.sync: Option<SyncConfig>` has to become a list of remotes, or an account plus several workspace bindings. `SyncConfig.token` becomes a session reference, not a workspace token.
- `storage.rs::SyncState`: `synced`, `origins`, `tombstones`, `sync_skipped` and `cursor` must be **per remote workspace**. Today they are global, and `reset_for_new_remote` wipes them.
- There is currently no way to say "this project/collection belongs to team X". `Project` (or `Collection`) needs a `remote`/`owner` binding so `collect_push` routes entities to the right workspace. Moving a collection between personal and team then becomes delete-in-A plus create-in-B with children.
- `sync.rs`:
  - `SyncClient` needs auth headers with refresh handling and workspace-scoped URLs.
  - `connect`, `client_for`, `pull`, `push_all`, `sync_now` and `apply_pull` need to run per remote.
  - The fresh-device "Default" project removal and the Global env merge ("one Global per workspace") need rethinking once several remotes feed one store.
- `share.rs`: no protocol change, but share ownership and author fields are needed.
- Token storage: move secrets to the OS keychain (keyring crate). Today they are plaintext in `workspace.json`.

**Tauri**
- `commands.rs`: new auth commands (login/logout/me/teams/invites/link workspace). `sync_create_workspace`, `sync_connect` and `sync_rotate_token` get legacy and anonymous semantics.
- `sync_rt.rs` / `sync_policy.rs`:
  - Today a 401 means disconnect. With accounts it should mean try refresh, then sign-in required.
  - Status needs to be per remote.
  - `sync_lock` per remote, or kept global.
- `state.rs`
- `main.rs` handler list.
- For OAuth, a deep link or loopback redirect needs a Tauri plugin. `reqharbor://` deep links are also in §2.2 Later.

**UI**
- `SettingsDialog.svelte`: new Account section, Sync reworked around accounts and teams, and Shares showing ownership.
- `Sidebar.svelte`: footer indicator plus a team or workspace switcher.
- `CollectionsTree.svelte`: team badges and a "Move to team…" action.
- `ShareDialog.svelte`
- `stores/app.svelte.ts` / `ui.svelte.ts`: section enum, account state.
- `api/types.ts`
- The mock backend `api/mock/index.ts`
- `docs/types.ts`, `docs/API_CONTRACT.md` §3.9/3.10
- `docs/DESIGN.md` §5.12/§5.14
- `docs/PRODUCT.md`: §9 currently lists "team accounts, roles" as **out of scope**, §2.2 lists "multi-user permissions" and "OS keychain" as Later, and §8 says "One token per workspace". All of these need rewriting.

## 9. Fragile spots and migration risks
1. **Unknown fields are dropped on round-trip.** `Request`, `Body` and the other model structs have no `#[serde(flatten)] extra`; only `Settings` has one (`model.rs` ~L650). Suppose a newer client adds a field, for example gRPC or GraphQL settings. An older teammate's client then deserialises the entity, drops the field, edits and pushes, and LWW silently erases it. An unknown `BodyType` variant, or a future `kind`, fails to deserialise, and `upsert_remote` reports it as skipped. Before team sync goes live, add an `extra` passthrough and/or a `kind` discriminator with a schema or min-client version on entities. This is the main blocker for adding gRPC and GraphQL later.
2. **Collaborators trust each other's clocks.** LWW uses the client `updatedAt`. One teammate with a fast clock wins every conflict, and there is no server-assigned timestamp or base-rev check.
3. **Whole-entity LWW.** Two teammates editing different fields of one request will produce conflict copies or lose edits. That is fine for single-user multi-device, and noticeable for teams.
4. **The server is hierarchy-blind.** `data` is opaque, so per-collection permissions or partial sync need either a schema change (`parent_id`/`collection_id` columns) or a workspace per team scope.
5. **Connecting merges everything.** `reset_for_new_remote` plus a full push means pointing an existing install at a team would upload all personal projects. Accounts need selective, per-project binding.
6. **Plaintext tokens** in `workspace.json`. A token gives full rights, including rotation and revoking all shares. There is no audit trail of who changed what (`device_id` is the only attribution).
7. **401 means hard disconnect** (AC-SYNC-7). With expiring access tokens this would disconnect constantly, so a refresh path is needed first.
8. **Tombstones are never garbage-collected** on the server, and they count against the per-workspace `rev` space. There is also a single hot `workspaces.rev` row per workspace. That is fine for small teams, and contention grows with team size.
9. **Share options are per device** (`settings.shareOptions`, not synced), so teammates cannot faithfully update someone else's share. Shares have no `created_by`.
10. **Local file paths sync.** `body.binaryPath` and form file fields sync as data. That is harmless for one user, and with a team it leaks local paths and breaks on other machines. Shares already strip them on import.
11. `withSecurityHeaders` deletes `Set-Cookie`, and the API CSP blocks forms. A web login flow must opt out deliberately.
12. Legacy anonymous workspaces need a claim path (prove the `rh_` token, then attach the workspace to a user or team) so existing users are not stranded. Keep `token_hash` working during the transition.
13. `wrangler.toml` has a placeholder `database_id`, so nothing is deployed yet. Migration risk is low today, and this is the cheapest moment to change the schema.
14. The mock backend (`ui/src/lib/api/mock`) duplicates sync and share behaviour and must be extended in lockstep or e2e tests drift.
15. Server-side `normalizeSnapshot` and `share-page.ts` assume HTTP requests. Shares of future gRPC or GraphQL request kinds would render as plain HTTP with body type `none`.

Key files:
- worker/src/index.ts
- worker/src/workspaces.ts
- worker/src/sync.ts
- worker/src/shares.ts
- worker/src/snapshot.ts
- worker/src/http.ts
- worker/src/quota.ts
- worker/migrations/0001_init.sql
- worker/migrations/0002_device_id_and_quota.sql
- crates/core/src/sync.rs
- crates/core/src/share.rs
- crates/core/src/model.rs
- crates/core/src/storage.rs
- src-tauri/src/commands.rs
- src-tauri/src/sync_rt.rs
- src-tauri/src/sync_policy.rs
- ui/src/lib/dialogs/SettingsDialog.svelte
- ui/src/lib/dialogs/ShareDialog.svelte
- ui/src/lib/components/Sidebar.svelte
- ui/src/lib/stores/app.svelte.ts
- ui/src/lib/api/mock/index.ts
- docs/PRODUCT.md
- docs/API_CONTRACT.md
- docs/types.ts
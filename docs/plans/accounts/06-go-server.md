# ReqHarbor server in Go: design (lead backend developer)

**Status:** implementation design. Where this document conflicts with `04-tech-design.md`, **this document wins**. It applies the owner decisions of 2026-11-27:

1. The backend becomes a small **Go service** in this repo (`server/`), shipped as a **public Docker image** that anyone can run anywhere. It replaces the Cloudflare Worker (`worker/`) going forward.
2. There is **no default server URL**. Users type their server's URL in the app, so sign-in must work against any self-hosted server.
3. Build order: **M0** client forward-compat → **M1** Go server (legacy API, then auth and personal spaces), then client sign-in and personal sync → **M2** teams → **M3** polish.
4. WebSocket (Socket.IO events first), gRPC and GraphQL request kinds come later. The data model must stay open for them.

Everything in `04-tech-design.md` §6–§10 that is about the **client** (bindings B1–B7, `account.json`, `sync-spaces.json`, the secret store, M0 forward compat, the move engine) still holds, except for the changes in §9 of this document. Everything in 04 that is about the **Worker** (D1, `wrangler.toml`, the breaker, Workers rate limiting, cron, `__Secure-` cookies on every deployment) is replaced by this document.

House rules: every build and test runs in Docker (`docker compose run --rm …`). Nobody runs `git commit` or `git push` as part of this plan's automation.

## QA resolution (review of the first draft of this document)

Each QA issue raised against the first draft, and the change in this revision that resolves it. Section numbers refer to this revision.

| # | Sev. | Issue | Resolution |
|---|---|---|---|
| Q1 | major | The dev sign-in guard keyed on the PUBLIC_URL host; the dev compose published 8080 on every interface with dev auth on | The dev provider is compiled only with `-tags devauth`. The published image and release binaries never contain it, and CI checks this with `go version -m`. A devauth build also requires a loopback `REQHARBOR_LISTEN` and a loopback `RemoteAddr` on every dev-provider request. Dev identities are never `email_trusted`. The `go` compose service has no dev auth and binds `127.0.0.1:8080:8080`. §5.4, §6, §7.2, §7.4, §7.5, S5. |
| Q2 | major | nOAuth: an unverified OIDC email could match `ADMIN_EMAIL`, and `ADMIN_EMAIL` re-granted admin on every sign-in | `users.email_trusted` replaces `email_verified`. Emails accepted under `REQUIRE_VERIFIED_EMAIL=false`, dev identities and password signups without SMTP are never trusted. They never satisfy `ADMIN_EMAIL` and never update `users.email`. `ADMIN_EMAIL` applies once, in `accounts.Create`, and only while no admin exists. Tests added. §5.4, §5.6, §8.3, S4, S5, S10. |
| Q3 | major | `Canonicalize` kept number lexemes and lone surrogates that `serde_json` rejects, so one entity could stop every pull | `Canonicalize` accepts only what the client's `serde_json` accepts: finite f64 range, no unpaired surrogate escapes, and a depth limit that leaves room for the pull envelope. A push gets a per-change `rejected` with `invalid_json_value` and the rest applies; shares get 400. A Rust test reads a parity fixture generated from the Go side, and a live test pushes poison values. §1.2, §1.3 (W1, W4, W9), §1.4, §1.6. |
| Q4 | major | Restoring a backup silently split server and clients | New `reqharbor-server restore FILE`: it shifts every rev above the pre-restore head and rotates `sync_epoch`, which `/v1/server` and every pull response report. Client rule (M0): an epoch change or a cursor regression resets the cursor to 0 and marks every entity dirty. Restoring by copying the file is unsupported. Both paths are tested. §5.1, §6, §8.3, §9, S1, S7. |
| Q5 | major | The strict `SYNC_TYPES` allowlist breaks newer clients against older self-hosted servers | `/v1/server` lists `sync.types` and `sync.requestKinds`. Clients push only listed types and keep the rest local-only, with a notice. `/v1/spaces/*/sync` rejects an unknown type per change; legacy `/v1/sync` keeps the 400. §1.2, §5.1, §9, §11, S6. |
| Q6 | major | Legacy workspaces defaulted to `open`, so every install was free anonymous storage and a share-page host | `REQHARBOR_LEGACY_WORKSPACES` defaults to `off`. New global caps: `REQHARBOR_MAX_USERS`, `REQHARBOR_LEGACY_TOTAL_BYTES`, `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES`. The startup warning names the abuse risk. §0, §1.2, §6, §12 item 2. |
| Q7 | major | The persisted account lockout allowed indefinite lockout and revealed which accounts exist | The lock is keyed by (email, IP bucket) and stored as a keyed hash. The account-wide limit is a proof-of-work slow-down that never refuses the right password. Known and unknown emails take the identical path. New `user unlock`. Cross-IP test added. §5.5, §5.7, §5.9, §8.3, S4. |
| Q8 | major | Sign-in broke when the address the user typed differed from PUBLIC_URL | Every `/auth/*` GET answers 302 to the same path on the PUBLIC_URL origin when the host, or a known scheme, differs; off-origin POSTs are refused. The client compares discovery's `publicUrl` with the entered origin and offers to switch. PUBLIC_URL is required unless the listener is loopback. §1.3 W2, §5.3, §6, §9, S4. |
| Q9 | major | The live gate could not pass: wrong variable name, and `CF-Connecting-IP` was ignored without a trusted proxy | The script trusts `127.0.0.1/32` with `CF-Connecting-IP` and keeps the default 10/h. Startup now fails on an unknown `REQHARBOR_RATE_*`, `REQHARBOR_QUOTA_*` or `REQHARBOR_MAX_*` name. §6, §7.5. |
| Q10 | minor | Token-endpoint field names differed between 04 and 06 | RFC 6749 snake_case in requests and responses: `grant_type`, `code`, `code_verifier`, `redirect_uri`, `client_id`, `refresh_token`; `access_token`, `refresh_token`, `token_type`, `expires_in`, plus `session_id` and `user`. 04 §2.2 step 5 and §5.3 need the matching correction (add `client_id`, rename the response fields). This revision may edit only 06, so that is a follow-up for 04's owner; until then this document wins, as the status line says. §5.2, §5.8, §9. |
| Q11 | minor | Signup and reset enumeration, pre-hijack squatting, no per-recipient email cap | With SMTP, signup always shows "Check your email", and an address that already has an account gets an "You already have an account" email instead of a code. Reset by email works for untrusted accounts and sets `email_trusted=1`. Method hints appear only in emails. At most 5 emails per recipient per hour, plus a global cap. §5.4, §5.5, §5.7, §5.9. |
| Q12 | minor | OIDC: only the issuer had to be https; `aud` arrays were not handled | Every endpoint taken from discovery must be https unless its host is loopback, redirects included. `aud` may be a string or an array, and `azp` must equal `client_id` when present or when there are several audiences. Tests in S10. §5.4. |
| Q13 | minor | The compose example used a nonexistent `:1` tag; tag patterns were unanchored | The example uses `:0.2`. Patterns are anchored; there is no major-only tag while the major is 0; `latest` is only for tags without a pre-release suffix. §0, §7.2, §7.3. |
| Q14 | minor | The `private` trusted-proxy keyword allowed `X-Forwarded-For` spoofing | `private` is removed; only explicit CIDRs are accepted. The example pins Caddy's address on a fixed-subnet network and trusts only that /32. One-time warnings for an untrusted peer that sends the header and for traffic that all comes from one peer. The README documents both failure modes. §5.9, §6, §7.3. |
| Q15 | minor | HSTS with `includeSubDomains` was imposed on self-hosters' domains | `REQHARBOR_HSTS`, default `max-age=31536000` without `includeSubDomains`, sent only for an https PUBLIC_URL, and `off` is allowed. §1.2, W3, §6. |
| Q16 | minor | Backups on the writer connection; listener started after migrations; fixed healthcheck port | Backups run on a dedicated connection that is not the writer. The listener starts before the pre-migration backup and the migrations: `/healthz` answers at once, while `/readyz` and every other route (503 `starting`) wait for migrations. `healthcheck` derives its URL from `REQHARBOR_LISTEN`. §2.1, §4, §6, §7.1. |
| Q17 | minor | The discovery table missed `features.accounts:false` and a higher `apiVersion` | `accounts:false` maps to `token_sync_only`. A higher `apiVersion` is compatible unless `minClientVersion` blocks the client. New mock states `legacyOnly` and `futureApi`. §9. |
| Q18 | minor | The check order for share update and revoke was not stated | Explicit order: id, ownership (404), revoked (410), then the body. Revoke never reads a body. Parity tests added. §1.2, §1.6. |
| Q19 | minor | M1 scope was too heavy for "auth + personal spaces" | M1 ships password sign-in, CLI reset links, invites, setup codes and GitHub. Google, generic OIDC, SMTP flows and scheduled backups move to M3, and may be pulled forward as a server-only M1.5 once C2 passes `qa_auth_live`. §0, §1.1, §5, §10. |

---

## 0. Decisions at a glance

| Topic | Decision |
|---|---|
| Language and runtime | Go 1.27 (current stable line; `go 1.27.0` in `go.mod`). Static binary, `CGO_ENABLED=0`. |
| HTTP | stdlib `net/http`, with `ServeMux` path patterns (`/v1/spaces/{id}/sync`). Method dispatch goes through a 20-line helper, so 405 responses keep the Worker's JSON shape and `Allow` header. No framework. |
| Database | SQLite through `modernc.org/sqlite` (pure Go). WAL, `synchronous=FULL`, foreign keys on. One file, `/data/reqharbor.db`. One writer connection plus a reader pool. Migrations are embedded SQL files with a 50-line runner. |
| Postgres | Not built. Every SQL statement lives behind `internal/store` interfaces. `REQHARBOR_DATABASE_URL` is reserved and refused at startup (§3.4). |
| Direct dependencies | `modernc.org/sqlite`, `golang.org/x/crypto` (argon2id), `golang.org/x/oauth2`. That is all (§3.2). |
| Wire compatibility | All `/v1/*` legacy routes and `/s/*` pages behave like the Worker: status codes, error codes and messages, JSON shapes, LWW, revs, limits, headers. The intentional differences are listed in §1.3. Today's desktop release works unchanged **against servers whose admin enabled token sync** (`REQHARBOR_LEGACY_WORKSPACES=open` or `existing`); the default is `off` (§1, §6). |
| Auth | The server hosts its own sign-in pages. **M1:** email and password (argon2id), on by default, and GitHub when the admin sets its credentials. **M3** (or an earlier server-only M1.5): Google, one generic OIDC provider, and SMTP email verification and reset. The desktop app always signs in through the system browser with PKCE S256 and a `127.0.0.1` loopback redirect, for every method and every server (§5). |
| Discovery | `GET /v1/server` returns the name, version, `instanceId`, `syncEpoch`, `publicUrl`, auth methods, signup mode, the sync types the server accepts and `minClientVersion` (§5.1). |
| Bootstrap | The first account becomes server admin. Creating it needs the setup code printed in the server log, or a **trusted** email that matches `REQHARBOR_ADMIN_EMAIL` while no admin exists, or the CLI (§5.6). |
| Signup modes | `open`, `invite` (default) or `closed` (§5.6). |
| Sessions | 1 h opaque access token, plus a rotating refresh token with reuse detection and a 60 s grace window. Only hashes are stored. Devices can be listed and revoked (§5.8). |
| Rate limits | In-process (single instance), with IPv6 grouped per /64. The client IP comes from an explicit trusted-proxy CIDR list (§5.9). Password guessing is throttled per (email, IP bucket), with an account-wide proof-of-work slow-down that never locks the owner out. |
| Image | `ghcr.io/mihajlopopara/reqharbor-server`, multi-arch (amd64 and arm64), distroless static, non-root, with a `/data` volume and a built-in healthcheck. It never contains the dev sign-in provider. Tags on `server-vX.Y.Z` git tags: `X.Y.Z`; `X.Y` and `latest` only without a pre-release suffix; `X` only once X ≥ 1. `edge` on `main` (§7). |
| `worker/` | Frozen as soon as the Go server reaches legacy parity (S3). Deleted in a single PR once one Go release is public and the live client suite passes against it (S8). Nothing was ever deployed from it: `wrangler.toml` still has the placeholder `database_id`, so there is no D1 data to migrate. |

---

## 1. Scope and compatibility

### 1.1 What the Go server implements

| Milestone | Routes |
|---|---|
| **M1 legacy parity** (S0–S3) | `POST /v1/workspaces`, `GET /v1/workspace`, `POST /v1/workspace/token/rotate`, `GET`/`POST /v1/sync`, `GET`/`POST /v1/shares`, `PUT`/`DELETE /v1/shares/{id}`, `GET /s/{id}`, `GET`/`OPTIONS /s/{id}.json`, plus the new `GET /v1/server`, `/healthz` and `/readyz` |
| **M1 accounts** (S4–S7) | `/auth/*` hosted pages for password sign-in, signup with invite and setup codes, and GitHub; `POST /v1/auth/token`, `POST /v1/auth/logout`, `GET /v1/me`, `GET /v1/me/sessions`, `DELETE /v1/me/sessions/{id}`, `GET /v1/me/spaces`, and `/v1/spaces/{id}` with `/sync` and `/shares`. Password reset in M1 is by CLI link only. Manual `backup` and `restore`. |
| **M2 teams** | `/v1/teams*`, `/v1/invites/*`, `/i`, role enforcement in space sync and shares (§10) |
| **M3** (S9–S11; may be pulled forward as a server-only **M1.5** once C2 passes `qa_auth_live`) | SMTP: signup email codes, `/auth/forgot` by email, the per-recipient email cap. Google and generic OIDC. Scheduled backups. Then `DELETE /v1/me`, the admin JSON API for an in-app admin view, and polish. The discovery `methods` list lets the client cope with whichever subset a server offers. |

### 1.2 The wire contract we must keep (legacy)

Each item below is behaviour that `worker/src` implements today and that the Rust client (`crates/core/src/sync.rs`, `share.rs`) or the worker tests depend on. Port the error **messages** verbatim from the TypeScript as well, because the client shows them as `code: message` (`SyncClient::send`, `sync.rs` L272–305).

**Envelope and routing (`index.ts`, `http.ts`)**
- Every error is `{"error":{"code","message"}}` with `Content-Type: application/json; charset=utf-8`.
- An unknown path returns 404 `not_found` with the message `No route for <METHOD> <path>`.
- A known path with the wrong method returns 405 `method_not_allowed`, the message `Method X not allowed; use A, B`, and `Allow: A, B`. The method list is in registration order and does **not** add `HEAD` automatically. (`HEAD /s/{id}` returns 405, which `qa_parity_security.test.ts` accepts.)
- Go's `ServeMux` would answer a method mismatch with plain text and add `HEAD`. So every path is registered **without** a method (`mux.Handle("/v1/sync", methods{{"GET", pull}, {"POST", push}})`), and `methods.ServeHTTP` performs the dispatch and the 405. `mux.Handle("/", notFound)` is the catch-all.
- Share ids in paths must match `^[A-Za-z0-9]{1,64}$` (`SHARE_PATH`). A segment that does not match is a **route** 404, not a handler 404.
- `/s/{id}` and `/s/{id}.json` share the mux pattern `/s/{file}`. The handler strips a `.json` suffix and validates the id. `.json` allows `GET` and `OPTIONS`; the HTML page allows only `GET`.
- On authenticated routes the order is: route, then method, then auth, then any **resource checks the route has before its body** (share update and revoke, see Shares below), then body. A 5 MB body without a token therefore gets 401.
- An uncaught error or panic becomes 500 `internal_error` with the message `Internal server error`. Details are never included; the stack goes to the log. A recover middleware handles this.

**Request bodies (`readJson`)**, checked in this order:
1. 415 `unsupported_media_type` unless `Content-Type` matches `^application/json\s*(;|$)` (case-insensitive).
2. 413 `payload_too_large` when `Content-Length` exceeds 5 MiB.
3. A missing body returns 400 `invalid_json` with "Request body is required".
4. The body is read through `http.MaxBytesReader`. Going over 5 MiB while streaming returns 413.
5. Invalid UTF-8 returns 400 `invalid_json`. A leading UTF-8 BOM is stripped, as `TextDecoder` does.
6. A body that is only whitespace (JS whitespace, see `jsTrim` below) returns 400 `invalid_json`.
7. A JSON syntax error returns 400 `invalid_json`.
8. A value that is not an object (`[]`, `null`, a number) returns 400 `validation_failed` with "Request body must be a JSON object". Check the first non-space byte is `{`; `json.Unmarshal("null", &map)` would succeed silently.

**Headers (`withSecurityHeaders`)**
- Every response sets `X-Content-Type-Options: nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, `Cross-Origin-Opener-Policy: same-origin`, `Cross-Origin-Resource-Policy: same-origin` (unless already set), `Permissions-Policy: camera=(), microphone=(), geolocation=(), interest-cohort=()`, and `Strict-Transport-Security` with the value of `REQHARBOR_HSTS` (default `max-age=31536000`, **without** `includeSubDomains`). HSTS is sent **only when `REQHARBOR_PUBLIC_URL` is https**, and never when `REQHARBOR_HSTS=off`. A self-hoster on an apex domain must not have every sibling subdomain forced to HTTPS by default, and the TLS proxy usually owns HSTS anyway (W3). An admin who wants `includeSubDomains` or `preload` sets the full value; startup validates it against `^max-age=\d+(; ?includeSubDomains)?(; ?preload)?$`.
- API responses default to `Content-Security-Policy: default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'` and `Cache-Control: no-store` when the handler has not set them.
- `Set-Cookie` is deleted from every response whose path does not start with `/auth/`.
- `/v1/*` never sends CORS headers, and `OPTIONS /v1/sync` returns 405.
- Implement this as a `ResponseWriter` wrapper that fills in the defaults just before `WriteHeader`.

**Legacy token auth (`workspaces.ts`)**
- The header must match `^Bearer\s+(\S+)\s*$` (case-insensitive) and the token `^rh_[A-Za-z0-9_-]{43}$`.
- The lookup is `sha256` hex of the token, then a constant-time compare (`subtle.ConstantTimeCompare`).
- Failure returns 401 `unauthorized`, "Missing or invalid workspace token", and `WWW-Authenticate: Bearer realm="reqharbor"`.
- Rotation is the compare-and-swap `UPDATE workspaces SET token_hash=? WHERE id=? AND token_hash=?old`. Zero changed rows returns 401 "Token was already rotated".
- **Availability:** every legacy route obeys `REQHARBOR_LEGACY_WORKSPACES` (§6), which defaults to **`off`**. A route that the setting disables returns 403 `legacy_disabled` before auth and before the body. The parity tests and the live gate run with `open`.
- `POST /v1/workspaces`:
  1. Validate `name`: a string, 1–200 characters after `jsTrim`, counted in **UTF-16 code units** as JavaScript counts them.
  2. Then apply the rate limit: 429 `rate_limited` with `Retry-After`, 10 per hour per IP bucket.
  3. Then apply the global caps: 507 `server_full` when `REQHARBOR_LEGACY_TOTAL_BYTES` is reached, or when the breaker is open.
  4. Then insert and return 201 `{workspaceId, token}`.
- `GET /v1/workspace` returns `{workspaceId, name, createdAt, entityCount, bytesUsed, quotaBytes}`.

**Sync (`sync.ts`), the core of compatibility**
- **Pull:** `since` and `limit` must match `^\d+$`, be at most 2^53−1 and be ≥ min (0 and 1). `limit` is capped at 500 and defaults to 500.
  - The response is `{changes:[{type,id,updatedAt,deleted,deviceId,data,rev}], cursor, hasMore, syncEpoch}`. `syncEpoch` is new (W10, §6 Restore).
  - `cursor` is the last rev of the page, or the workspace head when the page is empty.
  - The page and the head are read in **one read transaction**, so they are consistent.
- **Push validation**, in this order, with the first failure ending the request with 400:
  - `changes` must be an array; more than 500 changes returns 413 `too_many_changes`.
  - Each change must be an object. `type` must be in `project, collection, folder, request, environment`, otherwise 400 `unsupported_type` **on legacy `/v1/sync` only**, which keeps it byte-compatible. On `/v1/spaces/{id}/sync` every other check below still applies to the change, and then an unknown type becomes the per-change result `{status:"rejected", reason:"unsupported_type"}` with no rev; it is excluded from the quota delta, and the rest of the push applies (§11). `id` must match `^[A-Za-z0-9_.:-]{1,128}$`.
  - `updatedAt` must be a JSON number that JavaScript's `Number.isSafeInteger` accepts: parse it with `strconv.ParseFloat`, then require it to be integral, ≥ 0 and ≤ 2^53−1. This means `1e3` is accepted, as it is today.
  - `deleted` must be a boolean. `deviceId` that is absent or `null` becomes `""`; otherwise it must match `^[A-Za-z0-9_.:-]{0,128}$`.
  - For a tombstone, `data` must be absent or null. For an upsert, `data` must be an object.
  - A duplicate `(type, id)` in one push returns 400 `duplicate_change`.
- **Entity data:**
  - Environment data has secret values blanked (`stripSecretValuesForSync`).
  - Data is then canonicalised (§1.4) and measured in bytes. Over 512 KiB, the change gets the per-change result `{status:"rejected", reason:"entity_too_large"}` with no rev, and the rest of the push applies.
  - Data that the client's `serde_json` could not parse back (a number outside the finite f64 range, an unpaired surrogate escape, nesting deeper than the limit; §1.4) gets `{status:"rejected", reason:"invalid_json_value"}` with no rev, on both legacy and space sync, and the rest of the push applies. Such data is never stored, so it can never stop another device's pull (W9).
- **LWW, contract C3:** the incoming change wins when `updatedAt` is greater, or when `updatedAt` is equal and `deviceId >= stored deviceId` (byte-wise comparison).
  - Every non-rejected change takes the next rev **in push order**: `oldHead + 1 + i`.
  - A losing change still bumps the **stored** row to that new rev, so it is re-delivered. Its result is `stale` with that rev. A winning change is `applied`.
- **Quota:** the push is refused with 413 `quota_exceeded` when net growth (the sum of new size minus old size over the non-rejected changes) is greater than 0 **and** `used + delta > quota`. Use the Worker's message text. Deletes and shrinking writes always pass.
- **Response:** `{results:[{type,id,status,rev}|{type,id,status:"rejected",reason}], cursor:newHead}` in input order.
- **Go implementation:** one write transaction, which is serialised by the single writer connection.
  1. Re-read `workspaces.rev` inside the transaction. Do not trust the value read at auth time.
  2. Compute usage and the earlier sizes, then enforce the quota and the breaker.
  3. For each change: `SELECT updated_at, device_id, size`, decide the winner in Go, then either upsert or run `UPDATE … SET rev=?`.
  4. Finally `UPDATE workspaces SET rev = head + n`.

  The D1 tricks (100-parameter batches, `ROWS_PER_STATEMENT = 14`, `IDS_PER_LOOKUP = 90`, LWW inside `CASE WHEN`) are **not ported** (§8.4).

**Shares (`shares.ts`, `snapshot.ts`)**
- **Create:** the order is body, then `kind` (`collection` or `request`), then `entityId` (the id regex), then `prepareSnapshot`, then quota, then insert.
  - `prepareSnapshot` requires `schemaVersion` to be a safe integer ≥ 1 and `snapshot[kind]` to be an object. For collections, `folders` and `requests` must be arrays if present. `environment` must be an object, null or absent.
  - Secret variables are **removed** from the environment. A snapshot over 1 MiB returns 413 `snapshot_too_large`.
  - A snapshot that `Canonicalize` refuses (§1.4) returns 400 `validation_failed` with "Snapshot contains a value ReqHarbor cannot read". The same applies to update.
  - The response is 201 `{shareId, url, createdAt, updatedAt}`.
  - The share id is 128 random bits, written as 22 base62 characters. `url` is `REQHARBOR_PUBLIC_URL + "/s/" + id`.
- **List:** active shares only, ordered by `created_at DESC, id`. `name` comes from `COALESCE(json_extract(snapshot,'$.collection.name'), json_extract(snapshot,'$.request.name'))` and is returned only when it is a string. SQLite's JSON functions are built into modernc.
- **Update** (`PUT`), checked in exactly this order, as `updateShare` does:
  1. The path segment matches `^[A-Za-z0-9]{1,64}$`, otherwise a route 404 (above).
  2. Auth (401).
  3. An id that is not 22 base62 characters, or a share of another workspace or space, returns 404 "Share not found".
  4. A revoked share returns 410 `gone`.
  5. **Only now** `readJSON` (415, 413, 400), then `prepareSnapshot`, the quota and the breaker.
  6. `updated_at` becomes `max(now, row.updated_at)`. If the share is revoked while the update runs, the result is 410.
- **Revoke** (`DELETE`): the same steps 1–3, and it **never reads a body**, whatever its `Content-Type` or size. Idempotent: it sets `revoked_at`, `snapshot = NULL` and `size = 0`, and returns `{shareId, revokedAt}`.
- **`GET /s/{id}.json`:**
  - 200 `{shareId, kind, createdAt, updatedAt, snapshot}` with `X-Robots-Tag: noindex, nofollow`, `Cache-Control: public, max-age=60`, `Access-Control-Allow-Origin: *` and `Cross-Origin-Resource-Policy: cross-origin`.
  - 404 and 410 errors return JSON with the robots, CORS and CORP headers and the default `no-store`.
  - `OPTIONS` returns 204 with `Access-Control-Allow-Origin: *`, `Access-Control-Allow-Methods: GET, OPTIONS` and `Access-Control-Max-Age: 86400`.
- **`GET /s/{id}`:** the HTML page with the strict CSP (§1.5), `X-Robots-Tag` and `public, max-age=60`. 404 and 410 return the error page with `no-store`.

**JS semantics helpers (`internal/jsx`)**
- `jsTrim`: trims `unicode.IsSpace` plus U+FEFF.
- `utf16Len`.
- `isSafeInteger(json.Number)`.

These are used wherever the Worker relied on JavaScript behaviour.

### 1.3 Intentional differences from the Worker (documented, tested)

| # | Difference | Why it is safe |
|---|---|---|
| W1 | Stored `data` and `snapshot` are the client's JSON **canonicalised** (§1.4), not `JSON.stringify(JSON.parse(x))`. Number spellings within the finite f64 range are kept verbatim, so integers above 2^53 no longer lose precision. A number outside that range (`1e400`), which the Worker silently turned into `null`, is refused instead (W9). Sizes can differ by a few bytes from the Worker's. | The client parses the values; they are semantically equal. Size-limit tests use ASCII payloads. |
| W2 | `REQHARBOR_PUBLIC_URL` always builds share URLs, OAuth callbacks, cookies and the canonical sign-in origin. The Worker fell back to the request origin. | `serve` refuses to start without it unless `REQHARBOR_LISTEN` is loopback, in which case it defaults to `http://<listen address>` (§6). |
| W3 | HSTS comes from `REQHARBOR_HSTS` (default `max-age=31536000`, no `includeSubDomains`, `off` allowed) and only on https deployments. The Worker sent `max-age=31536000; includeSubDomains` everywhere. | Browsers ignore HSTS over http anyway. `includeSubDomains` on a third party's apex domain would force every sibling subdomain to HTTPS for a year, and the TLS proxy usually owns HSTS. |
| W4 | `data` and `snapshot` nested deeper than 100 levels are refused: per change `invalid_json_value`, or 400 for shares. | `serde_json` stops at 128 levels counted from the outermost value it parses. The pull page adds 3 levels around `data`, and a share snapshot sits inside the client's own structures, so 100 leaves headroom (§1.4). |
| W5 | Rate-limit windows live in memory and reset on restart. | Single instance. The persisted (email, IP bucket) password throttle (§5.9) is the brute-force control. |
| W6 | Folder and request order on the share page breaks `sortOrder` ties with `strings.ToLower` and then a byte comparison, not ICU `localeCompare`. | Cosmetic. It avoids `golang.org/x/text`. |
| W7 | Go's `ServeMux` cleans paths (`/v1//sync` becomes a 301 to `/v1/sync`), and a percent-encoded `.` in `/s/` ids decodes. | No client produces such paths. |
| W8 | The per-workspace 50 MB quota is a configuration value (`REQHARBOR_QUOTA_LEGACY_BYTES`, default 52428800). | The default equals today's value, so `GET /v1/workspace` reports the same `quotaBytes`. |
| W9 | A change whose `data` holds a value `serde_json` rejects (out-of-range number, unpaired surrogate escape, depth over 100) gets per-change `rejected` / `invalid_json_value` and is not stored. The Worker stored lone surrogates, after which every pull of that workspace failed in the client with `Protocol` and `pull_all` stayed stuck on the same cursor. | It turns a permanent, all-device sync failure into one rejected change on the device that wrote it. The client already handles per-change `rejected` (`entity_too_large`). |
| W10 | Pull responses carry an extra `syncEpoch` field, and legacy routes can be disabled (`REQHARBOR_LEGACY_WORKSPACES`, default `off`, 403 `legacy_disabled`). | `PullPage` ignores unknown fields, so today's client is unaffected by `syncEpoch`. M0 clients use it to detect a restore (§6). With legacy `off`, today's client gets a clean `Rejected` error instead of syncing. |
| W11 | Variable resolution on the share page is budgeted. Each export may substitute at most 1 MiB (`curlexport.MaxExpansionBytes`: every value substituted, at every nesting level, plus the reference it replaces), and one page view at most 2 MiB across all its requests (`sharepage.MaxPageExpansionBytes`). Past the budget, that request shows "Can't export as cURL: Resolved request is too large to export" instead of a Copy button. `findReference` jumps over `{{` candidates whose name would contain a `{`, which returns the same reference in linear time. The Worker and the Rust core have no budget and scan candidate by candidate. | Only pathological input diverges: nested references such as `a={{b}}×N, b={{c}}×N, c={{d}}×N` grow as N³ (about 8 GB per reference from a 30 KB environment), and `{{ {{ {{ … }}` scans quadratically. The Worker was contained by per-request isolate CPU and memory limits; the Go server is one long-lived process, and the page renders on every unauthenticated view. All 39 cases of `curl_parity.json` still match, run through a shared page budget. |

### 1.4 Canonical JSON (`internal/jsonx`)

There are two functions, both tested by fuzzing.

- **`Canonicalize(raw []byte) ([]byte, error)`** is used for every stored `data` and `snapshot`. Its rule is: **accept only what the client's `serde_json::Value` parser accepts** (default features, no `arbitrary_precision`), because the client parses every pull page whole and one unreadable entity would fail every pull of that workspace or space for every device, with `pull_all` retrying the same cursor forever (W9).
  - Parsing keeps object key order. A duplicate key keeps its first position and takes the last value, as `JSON.parse` does. This also stops a malicious client from storing duplicate keys that would make `serde` struct deserialisation fail on teammates' devices.
  - **Numbers:** the lexeme is copied verbatim when `strconv.ParseFloat(lexeme, 64)` yields a finite value (the check is `math.IsInf` on the result, so an underflow is never refused whatever `err` says). A lexeme that overflows to ±Inf (`1e400`, or an integer literal beyond about 1.8e308) is refused, because `serde_json` reports "number out of range". Underflow to zero or a subnormal (`1e-400`) is accepted, as `serde_json` accepts it. Integers beyond u64 but within f64 range are accepted (serde reads them as f64) and kept verbatim.
  - **Strings:** re-escaped minimally: `\"`, `\\`, `\b\f\n\r\t`, other control characters as lowercase `\u00xx`, and everything else as raw UTF-8. A `\uD800`–`\uDBFF` escape followed by a `\uDC00`–`\uDFFF` escape is decoded to the 4-byte UTF-8 character. **Any unpaired surrogate escape is refused**, as `serde_json` refuses it ("lone leading surrogate", which `worker/src/serde-json.ts` also ports). Raw bytes are already valid UTF-8 (`readJSON` step 5), so a raw surrogate cannot occur.
  - **Depth:** at most 100 levels, counting the `data` (or `snapshot`) value itself as level 1. `serde_json` stops at 128 levels counted from the document root; the pull page puts `data` at level 4, and the client nests snapshots inside its own structures, so 100 leaves headroom (W4).
  - Output has no insignificant whitespace.
  - **Errors** are typed (`ErrNumberRange`, `ErrLoneSurrogate`, `ErrTooDeep`, `ErrSyntax`). Push maps the first three to the per-change `invalid_json_value` (§1.2); share create and update map them to 400 `validation_failed`.
- **`SerdeReserialize(text string) (string, error)`** is a line-by-line port of `worker/src/serde-json.ts`. It reproduces `serde_json` number formatting (`formatF64`, `f64FromParts`). Only the cURL exporter uses it, to re-serialise GraphQL variables.

All JSON responses are written with `json.Encoder.SetEscapeHTML(false)` and without a trailing newline, so `<`, `>` and `&` stay unescaped, as `JSON.stringify` leaves them. Stored data is embedded as `json.RawMessage`.

### 1.5 Share page and cURL export

- **`internal/sharepage`** ports `snapshot.ts` (`normalizeSnapshot`, `normalizeAuth`, `normalizeBody`, `normalizeRequest`) and `share-page.ts` (tree, crumbs, badges, the kv tables, auth and body blocks, the environment block with the "Keep {{variables}}" toggle, and the error pages).
  - **Templates** are `server/internal/web/templates/share.html` and `share_error.html`, rendered with `html/template` from an `embed.FS`.
  - **Variable highlighting** (`withVars`) is a template function that returns `[]Segment{Text, IsVar}`, and the template wraps each variable in `<span class="var">`. Escaping is left to `html/template`. The HTML is therefore not byte-identical to the Worker's (it writes `&#34;` instead of `&quot;`), and nothing depends on that.
  - **Inline assets:** `PAGE_CSS` and `PAGE_JS` are copied byte for byte into `web/static/share.css` and `web/static/share.js`. The page receives them as one `template.HTML` value, `<style>…</style>`, and one `<script>…</script>`, so the template engine never rewrites them.
  - **CSP:** computed once at startup over the embedded bytes, exactly as `sharePageCsp()` builds it: `default-src 'none'; script-src 'sha256-…'; style-src 'sha256-…'; img-src 'none'; connect-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`.
- **AC-FWD-6, new on the server in M1:** `normalizeRequest` reads `kind`. Any value other than an absent kind or `"http"` (for example `graphql`, `grpc`, `websocket`, `socketio`) renders a card instead of a GET: "This request type can't be previewed on the web. Import it into ReqHarbor." That request has no Copy button and no cURL block.
- **`internal/curlexport`** is a line-by-line port of `curl.ts` (`toCurl`, `shellQuote`, percent-encoding, `--globoff`, `--head`, `--form-string`, quoted form paths, form-encoded keys) and `variables.ts` (`resolveVariables` with Rust `str::trim` semantics, depth 3, cycles left literal; `variableMap`).

### 1.6 How the worker tests become the Go test spec

The worker tests are the **acceptance spec** for legacy parity.
- Each `it(...)` becomes one Go test, or one row of a table-driven test, whose name keeps the AC or QA id, for example `TestSync_AC_SYNC_4_LWWOlderPushIsStale`.
- A test may be dropped only by listing it in the "W" table above.

| Worker file | Go file | Notes |
|---|---|---|
| `test/helpers.ts` | `server/internal/testutil/harness.go` | `New(t, ...Option)` returns `{URL, Store, Clock, Config}`, using a temporary data directory, `httptest.NewServer`, `REQHARBOR_PUBLIC_URL=https://reqharbor.test`, `REQHARBOR_LEGACY_WORKSPACES=open`, trusted proxy `127.0.0.1/32` and `ClientIPHeader=CF-Connecting-IP`. The worker tests' random `CF-Connecting-IP` therefore ports unchanged. By default `Call` sends `Host: reqharbor.test` and `X-Forwarded-Proto: https` (trusted, from 127.0.0.1), so requests are on the canonical origin (§5.3); origin-mismatch tests override both. It also provides `Call`, `CallJSON`, `NewWorkspace`, `Change`, `SampleRequest`, `SampleCollectionSnapshot` and a fake `Clock`. |
| `test/http.test.ts` | `internal/app/http_contract_test.go` | 413 declared and streamed (an `io.Pipe` body without `Content-Length`); the 415/400 matrix; 404 and 405 with `Allow: GET`; security headers; no CORS; 500 with no leak. The 500 case uses a `store.Store` stub whose `Read` returns an error containing "secret internal detail". |
| `test/sync.test.ts` | `internal/app/legacy_sync_test.go` | All 22 cases. "Only SHA-256(token) is stored" queries the DB through the harness. The concurrent rotation case runs 5 goroutines and expects exactly one 200. |
| `test/shares.test.ts` | `internal/app/shares_test.go` | All 17 cases. The CSP-hash test hashes `web/static/share.{css,js}` and asserts that `<style>`+CSS+`</style>` and `<script>`+JS+`</script>` each appear exactly once. Added order cases (§1.2 Shares): `PUT` to a revoked share with `Content-Type: text/plain` returns 410; `PUT` to another workspace's share with an oversized body returns 404, not 413; `DELETE` with a garbage body and no `Content-Type` returns 200. |
| `test/curl.test.ts` | `internal/curlexport/export_test.go` | All cases. |
| `test/curl_parity.test.ts` + `test/fixtures/curl_parity.json` | `internal/curlexport/parity_test.go` | Reads the same fixture (39 cases, expected output from the real Rust exporter). Each case goes through `sharepage.NormalizeSnapshot("request", …)`, then `curlexport.ToCurl`, and must match byte for byte. The error cases must return `*curlexport.ExportError` containing `expected.error`. Until S8 the fixture path is `../../../worker/test/fixtures/curl_parity.json`; S8 moves it (§11). |
| `test/qa_parity_security.test.ts` | `internal/curlexport/qa_parity_test.go` (006a–e, 009, 009b, parity OK) and `internal/app/security_test.go` (escaping of hostile content, cross-workspace 404, inert SQL-ish payloads, quota, IPv6 /64 buckets, HEAD → 405) | The `BUG QA-R1-024/025` cases assert the fixed behaviour, as the Worker tests do today. |
| `qa/worker_smoke.sh` | unchanged | Runs against the Go server with `BASE=http://127.0.0.1:8080`. |
| `crates/core/tests/qa_sync_live.rs` | unchanged | The real Rust client against the real Go server with token sync enabled. This is the "today's desktop client works unchanged against a server whose admin enabled token sync" gate (§7.5). |
| – | `crates/core/tests/qa_server_live.rs` (new, ignored, run by §7.5) | Go-server-only live cases that need raw HTTP: (a) a raw `reqwest` push mixing a valid change with one holding `1e400` and one holding `"\uD800"` returns `applied` and two `invalid_json_value`, after which the real `SyncClient::pull_all` from cursor 0 succeeds; (b) the restore paths (§6). Its raw workspace creates send a random `CF-Connecting-IP`, so they never use the 127.0.0.1 bucket that `qa_sync_live` needs. |

New Go-only tests:
- `FuzzCanonicalize`: round-trips through `encoding/json` to equal values, and every accepted output re-canonicalises to itself.
- **Serde parity (Go fixture, Rust check).** `internal/jsonx/serde_parity_test.go` builds `server/internal/jsonx/testdata/serde_parity.json` from a hand-written edge list (`1e308`, `1.8e308`, `1e400`, `-1e400`, `1e-400`, `18446744073709551616`, a 400-digit integer, paired and unpaired `\uD83D`/`\uDE00` escapes, `􏿿`, depth 100 and 101) plus the saved `FuzzCanonicalize` corpus. Each case is `{input, accepted, output?, reason?}`. `go test ./internal/jsonx -run SerdeParity -update` rewrites the file; without `-update` the test fails when the file is stale. The Rust test `crates/core/tests/serde_parity.rs` (a normal, not ignored, test run by `docker compose run --rm dev cargo test -p reqharbor-core --test serde_parity`) reads the same file and asserts:
  - every accepted `output`, wrapped as `{"changes":[{"type":"request","id":"x","updatedAt":1,"deleted":false,"deviceId":"","data":<output>,"rev":1}],"cursor":1,"hasMore":false,"syncEpoch":"e"}`, parses as the real `PullPage`;
  - every input refused with `number_range` or `lone_surrogate` is also refused by `serde_json::from_str::<Value>`, which proves the server does not over-refuse.
- `FuzzPushValidation`: never panics and only returns 4xx codes.
- `FuzzPushValidation`: never panics and only returns 4xx codes.
- `FuzzSharePage`: random snapshots never panic, and no `<script` appears except the hashed one.
- Migration tests (§8.6).

---

## 2. Stack

### 2.1 Runtime choices

| Concern | Choice |
|---|---|
| Go | 1.27.x. Use the stdlib: `net/http` patterns, `log/slog`, `crypto/hkdf` and `crypto/sha256`, `crypto/rand.Text`, `http.CrossOriginProtection` (Go ≥ 1.25) for the hosted forms, and `testing/synctest` where it helps. |
| Router | `ServeMux` plus the `methods` dispatch helper (§1.2). Middleware are plain `func(http.Handler) http.Handler`: recover, request id, security headers, access log, body limit. |
| JSON | `encoding/json` for envelopes; `internal/jsonx` for stored data. |
| SQL | `database/sql` with modernc. Three `*sql.DB` handles on the same file:<br>• **writer:** `SetMaxOpenConns(1)`, DSN `_txlock=immediate`;<br>• **reader:** `SetMaxOpenConns(8)`, `_pragma=query_only(1)`;<br>• **backup:** `SetMaxOpenConns(1)`, used only for `VACUUM INTO` (scheduled backups and the pre-migration backup). It is not `query_only`, because `VACUUM INTO` writes the target file, but it never writes the live database. In WAL mode its long read transaction does not block the writer, so pushes keep flowing during a backup; the WAL only grows until the backup ends and the next checkpoint runs. The `backup` CLI runs in its own process with its own connection, which is equivalent.<br>All three use `_pragma=foreign_keys(1)&_pragma=busy_timeout(5000)&_pragma=journal_mode(WAL)&_pragma=synchronous(FULL)&_pragma=temp_store(MEMORY)`. |
| Single instance | `serve` takes an exclusive `flock` on `/data/reqharbor.lock` and refuses to start when it is held ("another reqharbor-server is using /data"). Running two replicas on one SQLite file, or on a network filesystem, is unsupported and documented as such. CLI subcommands do not take the lock; they are short transactions with `busy_timeout`. |
| Time | `clock.Clock` interface (`Now() time.Time`), injected everywhere. Tests use a fake. All database timestamps are integer milliseconds since the Unix epoch, as in the legacy schema. |
| IDs | `internal/ids`: `ULID()` (port of `crypto.ts`), `ShareID()` (22 base62 characters), `Token(prefix)` (prefix + base64url of 32 bytes), `SHA256Hex`, and `Code(groups)` (Crockford base32 codes, for example `K7Q4-ZX2P-8M3D-1R6T`). |

### 2.2 Dependencies (direct)

| Module | Why | Alternatives rejected |
|---|---|---|
| `modernc.org/sqlite` (latest v1.x) | SQLite without cgo, so the binary is static and `FROM scratch`-compatible. It has JSON1 built in and supports `VACUUM INTO`. | `mattn/go-sqlite3` needs cgo. `ncruces/go-sqlite3` (wasm) is fine too, but modernc is the more widely deployed. |
| `golang.org/x/crypto` | `argon2.IDKey` | None in the stdlib. |
| `golang.org/x/oauth2` | Auth-code exchange with PKCE (`GenerateVerifier`, `S256ChallengeOption`), the GitHub and Google endpoints, and token-response parsing (including GitHub's form-encoded quirk) | Hand-rolling it is about 150 lines with edge cases. |

Not used: web frameworks, ORMs, migration libraries, `cobra`, `viper`, a JWT library or `go-oidc` (§5.4 explains why ID tokens are not signature-checked), Prometheus or OpenTelemetry clients, `testify`.

Tooling runs through `go run pkg@version` in CI, so it is not a module dependency: `govulncheck` and `staticcheck`.

### 2.3 Postgres later (boundary only)

- **Boundary:** handlers and services depend on `store.Store`, which exposes only two operations:
  ```go
  type Store interface {
      Read(ctx context.Context, fn func(ReadTx) error) error
      Write(ctx context.Context, fn func(WriteTx) error) error
  }
  ```
  `ReadTx` and `WriteTx` are interfaces of domain methods, for example `GetWorkspaceByTokenHash`, `EntityMeta`, `UpsertEntity` and `BumpEntityRev`. `internal/store/sqlite` is the only implementation.
- **What stays outside the store:** business rules such as LWW, validation, quota arithmetic and role checks live in services (`syncsvc`, `auth`, `spaces`). A Postgres store would only re-implement the queries.
- **What a Postgres store would change:**
  - `WriteTx.LockWorkspace(id)`, a no-op on SQLite, becomes `SELECT … FOR UPDATE`.
  - `json_extract` becomes `#>>`.
  - The SQLite triggers in 0004 become `plpgsql` triggers.
  - Migrations go in their own directory, `migrations/postgres/`.
- **Startup:** if `REQHARBOR_DATABASE_URL` is set, the server exits with "Postgres is not supported in this version; unset REQHARBOR_DATABASE_URL".

---

## 3. Repository layout

```
server/
  go.mod                         module github.com/mihajlopopara/reqharbor/server ; go 1.27.0
  go.sum
  Dockerfile
  docker-compose.example.yml     self-hoster compose (server + optional Caddy profile)
  Caddyfile.example
  README.md                      self-hosting guide (config table, OAuth app setup, backups, upgrades)
  cmd/reqharbor-server/
    main.go                      subcommand dispatch (flag package), version vars set by -ldflags
    serve.go  migrate.go  user.go  invite.go  backup.go  restore.go  healthcheck.go  setupcode.go
  internal/
    app/                         wiring: New(cfg, store, clock) http.Handler; Run(ctx) lifecycle; HTTP-level tests
    config/                      env parsing, *_FILE secrets, validation, unknown REQHARBOR_* warnings
                                 (unknown REQHARBOR_RATE_*/QUOTA_*/MAX_* names are errors)
    db/                          Open (writer/reader/backup pools, pragmas, flock), Migrate (embedded runner),
                                 Backup (VACUUM INTO), Restore (rev shift + epoch rotation)
      migrations/                0001_init.sql 0002_device_id_and_quota.sql 0003_server_state.sql (S1)
                                 0004_accounts.sql (S4) (M2: 0005_teams.sql)
    store/                       Store / ReadTx / WriteTx interfaces and row types
      sqlite/                    the implementation (all SQL lives here)
    httpx/                       methods router, JSON read/write, ApiError, security headers, recover, access log
    clientip/                    trusted-proxy resolution and rate-limit bucket (/64)
    ratelimit/                   in-process GCRA limiter, sharded map, sweeper
    ids/  jsx/  jsonx/  clock/
    syncsvc/                     validation + LWW + quota over WriteTx (shared by legacy and spaces)
    legacy/                      /v1/workspaces, /v1/workspace, rotate, /v1/sync, /v1/shares* (rh_ auth)
    shares/                      share create/list/update/revoke + public /s/* (scope-agnostic)
    sharepage/                   snapshot normalise + render
    curlexport/                  cURL exporter port (+ testdata/ from S8)
    auth/                        authorize flow, hosted pages, password, sessions, token endpoint
      providers/                 github.go (M1), google.go oidc.go (M3), dev.go (//go:build devauth only)
      throttle/                  (email, IP bucket) password throttle + proof-of-work challenge
    accounts/                    CreateAccount tx, email rules, admin operations shared by CLI
    spaces/                      /v1/me*, /v1/spaces/* (session auth + space authz)
    discovery/                   GET /v1/server
    mail/                        (M3) SMTP sender, text templates, per-recipient and global send caps
    jobs/                        GC ticker, disk/db-size and total-storage breaker sampler; scheduled backups (M3)
    web/                         embed.FS
      templates/                 layout.html authorize.html signup.html code.html done.html error.html
                                 forgot.html reset.html share.html share_error.html (M3: verify.html)
      static/                    share.css share.js auth.css reset.js pow.js logo.svg
    teams/                       M2
    testutil/
```

- **Embedded assets:** `web/embed.go` declares `//go:embed templates static` and exposes parsed templates and the CSP hashes computed at init.
- **No runtime filesystem reads:** nothing is read from disk at runtime except `/data`.
- **Git hygiene:** `.gitignore` gains `server/bin/` and `server/.data/`.

---

## 4. HTTP surface (complete for M1)

| Method & path | Auth | Notes |
|---|---|---|
| `GET /healthz` | – | 200 `{"status":"ok"}`. Liveness only. It answers as soon as the listener is up, **before** the pre-migration backup and the migrations, so a long migration on a large database never makes the container unhealthy (§6 Startup order). |
| `GET /readyz` | – | 200 or 503 `{"status","checks":{"db","migrations","disk"}}`. `migrations` is `pending`, `running` or `ok`. The DB is checked with `SELECT 1` on both pools (2 s timeout). The disk check compares free space against `REQHARBOR_MIN_FREE_DISK_BYTES`. |
| Every other route, until migrations finish | – | 503 `starting`, "Server is starting; try again shortly", with `Retry-After: 5`. Today's client maps any 5xx to `SyncError::Server` (`send()`, `sync.rs`), shows the message, and tries again on its next sync. |
| `GET /v1/server` | – | §5.1 |
| Legacy `/v1/workspaces`, `/v1/workspace`, `/v1/workspace/token/rotate`, `/v1/sync`, `/v1/shares`, `/v1/shares/{id}` | `rh_` | §1.2. `POST /v1/workspaces` obeys `REQHARBOR_LEGACY_WORKSPACES` (§6). |
| `GET /s/{id}`, `GET`/`OPTIONS /s/{id}.json` | – | §1.2, §1.5 |
| Any `GET /auth/*` on a host or scheme other than `REQHARBOR_PUBLIC_URL`'s | – | 302 to the same path and query on the PUBLIC_URL origin, with no cookie set (§5.3). An off-origin `POST /auth/*` renders the error page "Open this page from https://rh.acme.dev". |
| `GET /auth/authorize` | – | Starts a sign-in (§5.3) and sets the auth cookie |
| `GET /auth/signup?ar=` | cookie | Signup form |
| `POST /auth/code` | cookie | Applies an invite or setup code to the auth request |
| `POST /auth/password/login`, `POST /auth/password/signup` | cookie | Email and password (§5.5). M3 adds `POST /auth/password/verify` for SMTP codes. |
| `GET /auth/start/{provider}?ar=` | cookie | Redirects to GitHub (M1), Google or OIDC (M3) |
| `GET /auth/callback/{provider}` | cookie | Provider callback |
| `GET /auth/done` | – | "You're signed in" page; the app's loopback redirects here |
| `GET`/`POST /auth/forgot`, `GET`/`POST /auth/reset` | – | Password reset (§5.7). In M1 `/auth/forgot` only says to ask the admin; `/auth/reset` consumes CLI-issued links. |
| `POST /v1/auth/token` | PKCE or refresh | §5.8 (RFC 6749 field names) |
| `POST /v1/auth/logout` | access | Revokes this session |
| `GET /v1/me` | access | `{user:{id,email,name,avatarUrl,isAdmin}, method:"password"\|"github"\|"google"\|"oidc", sessionId}` |
| `GET /v1/me/sessions` | access | `{sessions:[{id, deviceName, createdAt, lastUsedDay, current}]}` |
| `DELETE /v1/me/sessions/{id}` | access | `WHERE id=? AND user_id=?me`; another user's id returns 404 |
| `GET /v1/me/spaces` | access | `{spaces:[{spaceId, kind:"personal", teamId:null, teamName:null, role:"owner", bytesUsed, quotaBytes}], departed:[]}`. In M1 the list holds only the personal space; the shape is final. |
| `GET /v1/spaces/{id}` | member | `{spaceId, kind, name, createdAt, entityCount, bytesUsed, quotaBytes, role}` |
| `GET`/`POST /v1/spaces/{id}/sync` | member | Same as legacy (including `syncEpoch` on pulls and per-change `invalid_json_value`), plus `updatedBy` on pulled changes, `serverTime` on push responses, the per-change `clock_skew` rejection, and an unknown `type` rejected per change as `unsupported_type` instead of a 400 (§1.2, §11) |
| `GET`/`POST /v1/spaces/{id}/shares`, `PUT`/`DELETE /v1/spaces/{id}/shares/{sid}` | member | Legacy shapes plus `createdBy` and `options` |

**Space authorisation** (`spaces.authSpace`): one read query joining `sessions`, `users`, `workspaces`, `teams` and `team_members`.

| Condition | Response |
|---|---|
| No session row, a revoked session or a disabled user | 401 `auth_required` |
| Access token expired | 401 `session_expired` |
| Unknown workspace, or a legacy workspace (`team_id IS NULL`) | 404 `space_not_found` |
| No membership row | 403 `not_a_member` |

`WWW-Authenticate: Bearer realm="reqharbor", error="invalid_token"` is sent on every 401. The client treats none of these codes as removal on its own (04 §6.7 stands).

**Version gate:** if `REQHARBOR_MIN_CLIENT_VERSION` is set, `/v1/spaces/*` and `/v1/auth/token` return 426 `client_too_old` when `X-ReqHarbor-Version` is missing or lower (semver compare). Legacy routes are **never** gated.

**Error codes added in M1**

| Status | Codes |
|---|---|
| 401 | `auth_required`, `session_expired`, `session_revoked` |
| 400 | `invalid_grant` |
| 403 | `not_a_member`, `legacy_disabled` |
| 404 | `space_not_found` |
| 426 | `client_too_old` |
| 429 | `rate_limited` (existing) |
| 503 | `starting` |
| 507 | `server_full` |
| per change | `clock_skew`, `invalid_json_value` (also legacy), `unsupported_type` (spaces only) |

The hosted pages use their own codes: `email_in_use`, `no_verified_email`, `signup_closed`, `server_user_limit`, `invite_invalid`, `setup_required`, `request_expired`, `wrong_origin` and `provider_error`.

---

## 5. Auth for a user-entered, self-hosted server

### 5.1 Discovery: `GET /v1/server`

```json
{
  "software": "reqharbor-server",
  "version": "0.2.0",
  "apiVersion": 1,
  "instanceId": "01JD…",
  "syncEpoch": "01JF…",
  "name": "Acme ReqHarbor",
  "publicUrl": "https://rh.acme.dev",
  "minClientVersion": null,
  "auth": {
    "methods": [
      {"id": "password", "label": "Email and password"},
      {"id": "github",   "label": "GitHub"}
    ],
    "signup": "invite",
    "passwordReset": "admin",
    "setupRequired": false
  },
  "features": { "accounts": true, "teams": false, "legacyWorkspaces": "off" },
  "sync": {
    "types": ["project", "collection", "folder", "request", "environment"],
    "requestKinds": ["http"]
  },
  "limits": { "entityBytes": 524288, "pushChanges": 500, "requestBytes": 5242880, "snapshotBytes": 1048576, "jsonDepth": 100 }
}
```

**Fields**
- **`instanceId`:** a ULID generated at the first start and stored in `server_state`. The client uses it to notice "same URL, different server" after the admin resets the database (§9). `restore` does **not** change it: a restored server is the same server.
- **`syncEpoch`:** a ULID in `server_state`, generated at the first start and rotated by `reqharbor-server restore` (§6). Every pull response carries it too. A client that sees it change resets its cursor and re-pushes (§9).
- **`publicUrl`:** `REQHARBOR_PUBLIC_URL`. The client compares it with the origin the user typed (§9).
- **`sync.types`:** the entity types this server stores. A client pushes only these and keeps any other type local-only (§11). A server that does not answer this route (the Worker) is assumed to accept exactly the five types above.
- **`sync.requestKinds`:** the request `kind` values the share page can preview. Every other kind is still stored and synced unchanged; the share page shows the import card for it (AC-FWD-6). The client uses it only to word the share dialog ("Viewers will see an import card for this request").
- **`name`:** `REQHARBOR_SERVER_NAME`, defaulting to the public URL's host.
- **`passwordReset`:** `email` when SMTP is configured (M3), otherwise `admin`. Always `admin` in M1.
- **`setupRequired`:** true while no user exists.
- **`methods`:** listed in a fixed order (`password`, `github`, then in M3 `google`, `oidc`). `password` is present unless `REQHARBOR_PASSWORD_AUTH=false`. A provider is present only when its credentials are configured and, for OIDC, discovery has succeeded. The dev provider is never listed.
- **`apiVersion`:** 1 for everything in this document. It increases only for a breaking change to `/v1/*`; additive fields never bump it.

**Behaviour**
- The JSON is built once at startup. It is rebuilt only when `setupRequired` or the OIDC availability changes.
- The response is `Cache-Control: no-store`, with no CORS.
- **Old servers:** the Worker returns 404 `not_found` for this path, and `server-v0.1.0` (S1–S3) returns 200 with `features.accounts:false` and no methods. The client treats both as "token sync only" (§9).

### 5.2 Desktop sign-in sequence (every method, every server)

```
App (Rust core)                         Browser                             Go server                      Provider
1 GET {S}/v1/server  ──────────────────────────────────────────────────▶  methods, signup, instanceId, minClientVersion
2 bind 127.0.0.1:0; verifier=b64url(32B); challenge=b64url(SHA256(verifier)); state=b64url(16B)
3 open {S}/auth/authorize?client_id=reqharbor-desktop&redirect_uri=http://127.0.0.1:P/cb
        &code_challenge=…&code_challenge_method=S256&state=…&device_name=MacBook%20Pro%20(macOS)&method=github
                                        ─GET /auth/authorize──────────────▶ validate; INSERT auth_requests (10 min);
                                                                             Set-Cookie rh_ar; render chooser:
                                                                             "Sign in to Acme ReqHarbor on MacBook Pro (macOS),
                                                                              started just now. Only continue if you just
                                                                              clicked Sign in in ReqHarbor."
   (a) password:                        ─POST /auth/password/login───────▶ cookie+ar match, CrossOriginProtection,
                                                                             throttle, argon2id verify ─┐
   (b) provider:                        ─GET /auth/start/github?ar=──────▶ upstream state+PKCE(+nonce) ─302─▶ authorize
                                        ◀──────────────────────── 302 {S}/auth/callback/github?code&state
                                        ─GET /auth/callback/github───────▶ exchange, profile, account resolution ─┤
                                                                             mint code (32B, 60 s, hash stored)  ◀┘
                                        ◀── 303 http://127.0.0.1:P/cb?code=…&state=…
4 loopback: GET /cb with matching state ⇒ 302 {S}/auth/done; stop listening; focus window
5 POST {S}/v1/auth/token grant_type=authorization_code&client_id=reqharbor-desktop&code=…&code_verifier=…&redirect_uri=…
     ◀── {access_token:"rha_…", refresh_token:"rhr_…", token_type:"Bearer", expires_in:3600, session_id, user}
6 secret store write (keyed by server origin + user id), then account.json, then GET /v1/me/spaces
```

**Parameter validation**
- **`client_id`:** must be `reqharbor-desktop`.
- **`redirect_uri`:** must match `^http://127\.0\.0\.1:(\d{4,5})/cb$` with a port of 1024–65535. `localhost`, `[::1]`, `https` and `urn:ietf:wg:oauth:2.0:oob` are rejected; paste-code is M4 (DEV-3). Any validation failure renders the error page. The server **never redirects to an unvalidated URI**.
- **`code_challenge_method`:** must be `S256`. The challenge is 43 base64url characters, and `state` is 1–128 characters.
- **`device_name`:** at most 64 characters, from `[\p{L}\p{N} ()._-]`.
- **`method`:** optional. It only preselects a method (focuses the password form, or makes that provider's button primary). The page **never auto-redirects**; QA open issue 9a.
- **Origin:** before any of this, a request whose host or known scheme differs from `REQHARBOR_PUBLIC_URL` is redirected there with the query unchanged (§5.3). `redirect_uri` is a query parameter, so it stays the loopback URI.

**Code issue and exchange**
- The authorization code is stored as a hash on the auth request, which is bound to the challenge and the redirect URI.
- The exchange consumes it in one statement: `UPDATE auth_requests SET consumed_at=? WHERE code_hash=? AND consumed_at IS NULL AND code_expires_at>? RETURNING …`. Only then are the PKCE verifier, `redirect_uri` and `client_id` checked. A failed check still burns the code.
- Every failure returns 400 `invalid_grant`.

**Provider errors:** `?error=access_denied` redirects to `redirect_uri?error=access_denied&state=…`, which is safe because the URI was validated at authorize time.

### 5.3 Hosted pages: canonical origin, cookie, CSRF, CSP

- **One canonical origin.** Cookies, the `Secure` flag, the http banner, OAuth callbacks, reset links and share URLs all come from `REQHARBOR_PUBLIC_URL`, so the browser part of every flow must run on that origin, whatever address the user typed in the app:
  - **Effective origin of a request:** the scheme is **known** when `r.TLS != nil` (https) or when the TCP peer is a trusted proxy that sends `X-Forwarded-Proto` (its value). Otherwise the scheme is **unknown**. The host is the `Host` header, port included; `X-Forwarded-Host` from a trusted proxy wins.
  - **Rule:** every `GET /auth/*` whose host differs from PUBLIC_URL's, or whose scheme is **known** (TLS or a trusted proxy header) and differs, answers 302 to `{PUBLIC_URL}{path}?{query}`, sets no cookie and creates no auth request. An off-origin `POST /auth/*` renders `wrong_origin` ("Open this page from https://rh.acme.dev") and changes nothing.
  - **No redirect loop:** when the host matches but the scheme is unknown (plain http from an untrusted peer, typical of a TLS proxy that was not added to `REQHARBOR_TRUSTED_PROXIES`), the request is served and a one-time warning is logged: "PUBLIC_URL is https but requests for its host arrive over plain http from an untrusted peer; add the proxy to REQHARBOR_TRUSTED_PROXIES". The cookie is still `Secure`, because it follows PUBLIC_URL, and the browser is on https at the proxy, so the cookie works.
  - Cases that now work: PUBLIC_URL https with the app pointed at `http://127.0.0.1:8080` or a LAN IP (the browser lands on the https origin before the cookie is set); provider sign-in started on host A (redirected to B before `/auth/start`, so the callback on B finds the cookie).
- **Cookie:**
  - When PUBLIC_URL is https: `__Secure-rh_ar=<b64url 16B>; Path=/auth; HttpOnly; Secure; SameSite=Lax; Max-Age=600`.
  - When PUBLIC_URL is http (LAN or dev): `rh_ar=…`, the same without `Secure`. The pages then show a banner: "This server doesn't use HTTPS. Others on your network may see your password." Because PUBLIC_URL is required on any non-loopback listener (§6), this banner is shown only when the server really is http.
  - Only `sha256(cookie)` is stored, in `auth_requests.browser_hash`. Every `/auth/*` step after authorize requires that the cookie matches `ar`; this is the login-CSRF fix.
- **POST protection:** every POST handler is wrapped in `http.CrossOriginProtection`, which checks `Sec-Fetch-Site` and `Origin`. It also requires the `ar` form field and the cookie match.
- **CSP for `/auth/*`:** `default-src 'none'; style-src 'sha256-<auth.css>'; script-src 'sha256-<reset.js>'; img-src data:; form-action 'self' http://127.0.0.1:*; frame-ancestors 'none'; base-uri 'none'`.
  - `http://127.0.0.1:*` is needed because browsers apply `form-action` to the redirect that follows a successful password POST (the 303 to the loopback).
  - Provider buttons are plain `<a>` links, never forms, so the redirect to github.com is not a form navigation.
- **Other headers:** `X-Robots-Tag: noindex`, `Cache-Control: no-store`, and the usual security headers.
- **Templates:** `auth.css` reuses the share page's design tokens and `logo.svg`. All templates are `html/template`. No third-party resources.

### 5.4 External providers

| Provider | Milestone | Enabled when | Flow details |
|---|---|---|---|
| GitHub | M1 | `REQHARBOR_GITHUB_CLIENT_ID` and `REQHARBOR_GITHUB_CLIENT_SECRET` | OAuth App. Callback `{PUBLIC_URL}/auth/callback/github`. Scopes `read:user user:email`, sent with PKCE. Profile from `GET https://api.github.com/user` and `/user/emails`, with a `User-Agent` header. Uses the primary verified email, which is **trusted**. Subject is the numeric `id`. |
| Google | M3 | `REQHARBOR_GOOGLE_CLIENT_ID` and `REQHARBOR_GOOGLE_CLIENT_SECRET` | Web-application client. Scopes `openid email profile`, `prompt=select_account` (QA 9a), plus PKCE and `nonce`. Claims come from the `id_token` in the token response: `iss ∈ {https://accounts.google.com, accounts.google.com}`, the audience rules below, `exp`, `nonce`, `email_verified == true`. The email is trusted. |
| Generic OIDC | M3 | `REQHARBOR_OIDC_ISSUER`, `REQHARBOR_OIDC_CLIENT_ID` and `REQHARBOR_OIDC_CLIENT_SECRET` | Discovery from `{issuer}/.well-known/openid-configuration` at startup, retried with backoff up to 5 min. The provider is hidden from `/v1/server` until discovery succeeds. The issuer must equal `issuer` in the discovery document. **Every URL used from configuration or discovery** (the issuer, `authorization_endpoint`, `token_endpoint`, `userinfo_endpoint`) must be https unless its host is loopback; otherwise discovery counts as failed with a config error in the log, the provider stays hidden, and there is no retry. The server's HTTP client for the token and userinfo calls refuses any redirect to a non-https URL. Scopes `REQHARBOR_OIDC_SCOPES` (default `openid email profile`), PKCE, `nonce`. Claims are validated as for Google. If the `id_token` lacks `email`, the userinfo endpoint is called and its `sub` must equal the ID token's `sub`. `REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL=false` accepts IdPs that never send `email_verified` (for example Entra ID), but such emails are **never trusted** (below). Label: `REQHARBOR_OIDC_NAME` (default "Single sign-on"). |
| Dev | tests only | Compiled in only with `-tags devauth`, **and** `REQHARBOR_DEV_AUTH=true` | A form taking any email and name, for the Go harness, e2e tests and live client tests. Guards, all of them enforced: (1) the provider's code exists only in `providers/dev.go` behind `//go:build devauth`; the release workflow and the Dockerfile never pass that tag, and CI fails if `go version -m` on the built binary shows `-tags` containing `devauth`. (2) A binary without the tag exits 2 at startup when `REQHARBOR_DEV_AUTH=true`: "dev sign-in is not compiled into this build". (3) A devauth binary refuses to start unless `REQHARBOR_LISTEN` is a loopback address (`127.0.0.1:P` or `[::1]:P`; `:8080` and `0.0.0.0` are refused). (4) Every dev-provider request must come from a loopback `RemoteAddr` (the TCP peer, never a forwarded header), otherwise 404. (5) Dev identities are never trusted, so they never satisfy `ADMIN_EMAIL`. The guard no longer looks at the PUBLIC_URL host. |

- **Audience rules** (Google and OIDC): `aud` may be a JSON string or an array of strings, and must contain `client_id`. When `aud` has more than one entry, `azp` must be present and equal `client_id`. When `azp` is present at all, it must equal `client_id` (OIDC Core §3.1.3.7 steps 3–5).
- **Why ID-token signatures are not verified:** the ID token is read only from the token-endpoint response, over a direct TLS call from the server to the IdP's `token_endpoint`, which OIDC Core §3.1.3.7 step 6 allows in place of signature validation. That premise holds only because every endpoint is required to be https (above); a discovery document naming an http `token_endpoint` disables the provider. This removes the need for JOSE or JWKS code and dependencies.
- **Redirect URL:** for every provider it is the server's own `{REQHARBOR_PUBLIC_URL}/auth/callback/{id}`. The README tells the admin the exact URL to register.
- **Credentials:** a client id without a secret (or the reverse) is a startup error.
- **Upstream tokens** are used once to read the profile, then dropped. They are never stored or logged.

**Trusted emails.** `users.email_trusted` (in `0004_accounts.sql`; it replaces the first draft's `email_verified`) records whether this server has reason to believe the user controls `users.email`. Only a trusted email may satisfy `REQHARBOR_ADMIN_EMAIL` (§5.6) or change `users.email`. This is the defence against nOAuth-style takeover, where an IdP lets users set their own `email` claim.

| Source of the email | Trusted? |
|---|---|
| GitHub primary verified email; Google with `email_verified == true`; OIDC with `email_verified == true` while `REQUIRE_VERIFIED_EMAIL=true` | Yes |
| OIDC under `REQUIRE_VERIFIED_EMAIL=false` (whatever `email_verified` says) | **No, never** |
| Dev provider | **No, never** |
| Password signup confirmed by an SMTP code (M3); a completed email reset (§5.7) | Yes |
| Password signup without SMTP | No, until a completed email reset |
| `user create` by the admin on the CLI | Yes (the admin vouches for it) |

**Account resolution** (AC-AUTH-6, with QA 9c):
- **Returning user:** a hit on `identities(provider, issuer, subject)`. Accounts are never matched by email.
  - `users.email` is updated **only** when the incoming email is trusted and no other user owns it; `email_trusted` is then 1. An untrusted incoming email is recorded in `identities.email` for display and changes nothing else.
  - `is_admin` is **never** changed on sign-in, so `user set-admin --off` sticks.
  - `name` and `avatar_url` are refreshed.
- **New identity, no email at all, or (with `REQUIRE_VERIFIED_EMAIL=true`) no verified email:** the `no_verified_email` page. Nothing is created.
- **New identity, email already used by another account:** the `email_in_use` page. Nothing is created. When the incoming email is **trusted**, the page masks it and names the existing method ("j***@acme.dev already signs in with Email and password"); the visitor has just proved control of that address, so this tells them about their own account. When it is **untrusted**, the page says only "This email is already used by another account. Ask your server admin." In the trusted case only, when the existing account's own email is untrusted (a possible squatter), the page adds "If this is your address, use Forgot password to claim the account" (§5.7).
- **Otherwise:** signup rules apply (§5.6). If signup is allowed, `accounts.Create` runs in one write transaction, with `email_trusted` from the table above.

### 5.5 Email and password

**Policy and hashing**
- **Password rules:** 10 to 256 characters (counted as runes), and the password must not equal the email. There is no composition rule and no breach list in M1.
- **Hash:** argon2id with `m=REQHARBOR_ARGON2_MEMORY_KIB` (default 65536), `t=3`, `p=2`, a 16-byte salt and a 32-byte key, stored as a PHC string (`$argon2id$v=19$m=65536,t=3,p=2$…`).
  - A semaphore of `max(1, min(4, NumCPU/2))` bounds concurrent hashes, so a login burst cannot run a small VPS out of memory.
  - When the parameters have changed, the password is rehashed on the next successful login.
- **Unknown email:** on the normal (not throttled) path a hash is still verified against a fixed dummy PHC, so timing does not reveal whether the account exists. Throttling (§5.9) is keyed by the normalised email, not the user id, so a known and an unknown email meet the same thresholds, the same messages and the same timing.

**Login** (`POST /auth/password/login`)
- Fields: `ar`, `email`, `password`, and `pow` when the page asked for proof of work (§5.9).
- Order: origin and CSRF checks; the per-IP and per-auth-request limits; the (email, IP bucket) throttle; the account-wide proof-of-work check; then argon2 (real or dummy).
- Every failure shows the same message: "Email or password is incorrect." A throttled (email, IP bucket) shows "Email or password is incorrect. Try again in N minutes." for known and unknown emails alike, and runs no argon2 in either case.
- On success: delete this (email, IP bucket) throttle row, set `auth_requests.user_id`, mint the code, and 303 to the loopback. Rows for other IP buckets are left alone (an attacker's lock is not lifted by the owner's success, and the owner was never blocked by it).
- A password account can sign in even when its email is untrusted. Trust matters only for the admin-email rule (§5.6) and for changing `users.email`.

**Signup** (`POST /auth/password/signup`)
- Fields: `ar`, `name`, `email`, `password`, and `code` when signup is `invite`.
- Order: origin and CSRF; limits; the invite code when signup is `invite` (so probing for existing emails first needs a valid invite); `REQHARBOR_MAX_USERS`; then the email.
- **Without SMTP (M1):** the account is created immediately with `email_trusted=0`. An email that already has an account shows "An account with this email already exists. Sign in, or ask your server admin." without naming the sign-in method. This still reveals that the account exists, which cannot be avoided without an email channel; it is bounded by the 30 / h per-IP signup bucket and, in `invite` mode, by the code check above. With `REQHARBOR_SIGNUP=open` and no SMTP, the startup warning says: "open signup without SMTP: emails are not verified, and anyone can test which emails have accounts".
- **With SMTP configured (M3):** the page always moves to "Check your email for a 6-digit code", whether or not the address has an account.
  - **New address:** nothing is created yet. The pending signup is stored on the auth request as `pending_email`, `pending_name` and `pending_phc`, and a 6-digit code is emailed. The code is valid for 10 minutes and allows 5 attempts. At most 3 codes are sent per auth request, and every send counts against the per-recipient cap (§5.9). `POST /auth/password/verify` creates the account with `email_trusted=1`.
  - **Address that already has an account:** no code is sent. The address gets an email instead: "You already have a ReqHarbor account on Acme ReqHarbor. It signs in with GitHub. If you didn't just try to sign up, you can ignore this." Any code entered on the verify page fails with the usual wrong-code message, so the page behaves exactly as for a new address. The sign-in method appears only in this email.

### 5.6 Signup modes and bootstrap

`REQHARBOR_SIGNUP` takes one of three modes; the default is `invite`.

| Mode | Who may create an account |
|---|---|
| `open` | Anyone, with any enabled method |
| `invite` | Holders of a valid server invite code (`one_time_tokens.purpose='signup_invite'`, created with the CLI), and in M2 holders of a team-invite signup ticket |
| `closed` | Nobody through the pages. Only the bootstrap paths and the CLI. |

**Global caps** apply in every mode: once `REQHARBOR_MAX_USERS` accounts exist (default 500; `0` means no cap), the pages show `server_user_limit` ("This server isn't accepting new accounts. Ask its admin.") and create nothing. Personal spaces of accounts created through **open** signup (`users.signup_via = 'open'`) also share `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` (§6). The CLI may exceed `MAX_USERS` with `--force`, and the first account (bootstrap) is never refused.

**Bootstrap paths** work in every mode:
1. **Setup code.**
   - While the `users` table is empty, `serve` generates a setup code at each start, invalidating the previous one, and stores its hash as `purpose='setup'` with a 24 h expiry.
   - It logs the code at WARN: `First-run setup: in the ReqHarbor app, sign in to https://rh.acme.dev and enter setup code K7Q4-ZX2P-8M3D-1R6T`.
   - `REQHARBOR_SETUP_CODE` fixes the code instead, which suits automation.
   - The authorize page shows "Finish setting up this server" and a code field. An account created with a valid setup code becomes **server admin**.
   - The code works only while no user exists, which closes the race on a freshly exposed server.
2. **`REQHARBOR_ADMIN_EMAIL`.** Applied **once, at account creation only, and only while no admin exists**: when `accounts.Create` inserts a user whose email equals this value, whose email is **trusted** (§5.4 table), and `SELECT 1 FROM users WHERE is_admin = 1` finds nothing in the same write transaction, that user becomes admin, and signup-mode restrictions are bypassed for that one creation.
   - It is never applied on sign-in, so demoting the admin (`user set-admin --off`) or a later email change cannot be undone or exploited by signing in again.
   - Once any admin exists, the variable has no effect; startup logs "REQHARBOR_ADMIN_EMAIL is ignored: an admin already exists" at info.
   - Untrusted emails never qualify: OIDC under `REQUIRE_VERIFIED_EMAIL=false`, dev identities, and password signups without SMTP. Nobody can claim the admin email by typing it into an IdP profile or a signup form. Admins of IdPs such as Entra ID bootstrap with the setup code or the CLI instead; the README says so next to `REQUIRE_VERIFIED_EMAIL`.
3. **CLI.** `reqharbor-server user create --email … [--admin]`. The first user the CLI creates is always admin.

**Codes:**
- **Entry:** a code (invite or setup) is entered on the authorize or signup page (`POST /auth/code`) and stored on the auth request as `grant_token_hash`.
- **Consumption:** the code is consumed inside the account-creation transaction, which checks `uses < max_uses`, `expires_at > now`, `revoked_at IS NULL` and, when set, that the token's `email` matches. That makes it safe under concurrency, because the single writer serialises creation.
- **Wrong codes:** entering a wrong code shows the generic `invite_invalid` message.
- **Brute force:** code entry is limited to 10 tries per auth request and 30 per hour per IP.

**What "admin" means in M1:** a stored flag (`users.is_admin`) that `/v1/me` reports. Administration in M1 happens through the CLI, which needs shell access to the container. M3 adds `/v1/admin/*` for an in-app admin view.

### 5.7 Password reset

- **Without SMTP** (`passwordReset: "admin"`, the only mode in M1):
  - `/auth/forgot` says "Ask your server admin for a reset link." It takes no email, so it reveals nothing.
  - The admin runs `reqharbor-server user reset-link --email …`, which prints `{PUBLIC_URL}/auth/reset#t=<token>`, valid for 24 h. The token is 32 bytes, single-use, and stored as a hash with `purpose='reset_password'`.
  - `user set-password --email … --password-stdin` is the alternative.
- **With SMTP** (`passwordReset: "email"`, M3):
  - `/auth/forgot` takes an email and **always** answers "If an account exists for that email, we sent a link." The page never names a sign-in method.
  - **Password account, trusted or not:** the email contains `{PUBLIC_URL}/auth/reset#t=<token>`, valid for 30 minutes. Reset by email deliberately works for accounts whose email is **untrusted**: proving control of the inbox is exactly what such an account lacks. This is how the real owner reclaims an address that someone registered before SMTP was configured (pre-hijack squatting).
  - **Provider-only account:** the email says "Your account on Acme ReqHarbor signs in with GitHub, so there is no password to reset." No token is issued.
  - **No account:** nothing is sent.
  - Limits: 5 per hour per email and 20 per hour per IP, plus the per-recipient cap (§5.9).
- **The reset page** (both modes): the token travels in the **fragment**, so proxies and logs never see it. `reset.js`, an inline script allowed by its CSP hash, copies it into the form's hidden field. A `<noscript>` block says "Enable JavaScript or ask your server admin for help."
- **After a successful reset:** every throttle row for that email is deleted, **every session of that user is revoked**, and when the token came by email, `email_trusted` becomes 1. The page says "Password changed. Return to ReqHarbor and sign in." A CLI link does not change `email_trusted`, because the admin may have sent it anywhere.
- **Accounts without a password:** a CLI `reset-link` for a provider-only account is refused on the CLI ("this account signs in with GitHub"). The web pages never say which method an account uses.

### 5.8 Sessions and tokens

This keeps 04 §3.1's token lifetimes, rotation and grace semantics. The **field names** follow RFC 6749 (snake_case), because generic OAuth tooling expects them; this document is the reference for the wire format, and 04 §2.2 step 5 and §5.3 are to be corrected to match (Q10). The implementation details:

- **Token formats:**
  - Access token: `rha_` + base64url of 32 bytes, valid for 1 h.
  - Refresh token: `rhr_` + base64url of 32 bytes. Its idle expiry is 60 days, sliding on every refresh; its absolute expiry is 365 days.
  - Only SHA-256 hex hashes are stored.
  - Neither token matches the legacy `rh_` regex (`rha_` has `a` where `_` is expected), so the two auth schemes cannot be confused.
- **Token endpoint:** `POST /v1/auth/token`. The body is `application/x-www-form-urlencoded` (RFC 6749 §4.1.3, what the desktop client sends) or `application/json` with the **same** names; any other `Content-Type` returns 415.
  - `grant_type=authorization_code`, `client_id`, `code`, `code_verifier`, `redirect_uri`, all required;
  - or `grant_type=refresh_token`, `client_id`, `refresh_token`.
  - `client_id` must be `reqharbor-desktop`. It is required, as RFC 6749 §4.1.3 requires for public clients.
- **Response** (RFC 6749 §5.1 plus two extension members): `{access_token, refresh_token, token_type:"Bearer", expires_in:3600, session_id, user}`, with `Cache-Control: no-store`.
- **Errors** keep the ReqHarbor envelope, `{"error":{"code":"invalid_grant","message":…}}`, rather than RFC 6749 §5.2's flat `{"error":"invalid_grant"}`, so the client's single error parser works. This is the one deliberate departure from the RFC and is documented in `docs/API_CONTRACT.md` when it is updated in C1.
- **Rotation** is one compare-and-swap `UPDATE … WHERE refresh_hash=?old AND revoked_at IS NULL AND idle_expires_at>? AND expires_at>? RETURNING …`, which stores the new hashes, sets `prev_refresh_hash=old`, and stores `pending_pair_ct` with `pending_until=now+60s`.
- **Grace window:** `pending_pair_ct` is AES-256-GCM over `{accessToken, refreshToken, accessExpiresAt}`.
  - The key is `hkdf.Key(sha256.New, oldRefreshToken, sessionID, "rh-refresh-grace", 32)` (stdlib `crypto/hkdf`), so only the holder of the old token can decrypt it.
  - If the CAS misses and `prev_refresh_hash` matches within `pending_until`, the server decrypts and returns the **identical** pair and writes nothing.
  - If it matches after the window, it is reuse: the server revokes the session and returns 401 `session_revoked`.
- **Per-request auth:** one indexed read on `access_hash`, joined to `users`, which also rejects a `disabled_at` user. `last_used_day` is written only when the day changes.
- **Session cap:** `REQHARBOR_SESSION_MAX` (default 20) live sessions per user. Inserting one more revokes the oldest.
- **Revocation:**
  - `POST /v1/auth/logout` revokes the session that made the call.
  - `DELETE /v1/me/sessions/{id}` revokes another of the user's sessions; that device gets 401 on its next call.
  - Disabling a user, resetting their password or running `user sessions revoke` revokes all of that user's sessions.
  - `restore` keeps the sessions of the backup. Devices that refreshed after the backup was taken hold a refresh token the restored database does not know; they get 400 `invalid_grant` and must sign in again. That is not treated as reuse, because `prev_refresh_hash` does not match either.

### 5.9 Rate limits, brute force, client IP

**Client IP** (`internal/clientip`)
- `REQHARBOR_TRUSTED_PROXIES` is a comma-separated list of explicit CIDRs. It is empty by default. There is **no** `private` keyword: trusting every RFC 1918, ULA and link-local peer lets any LAN client, and on Docker Desktop, rootless Docker or Podman any client at all (every peer appears as a private gateway address), spoof the header and pick its own rate-limit bucket. A CIDR wider than /16 (IPv4) or /48 (IPv6) logs a startup warning.
- If the TCP peer is trusted, the client IP comes from `REQHARBOR_CLIENT_IP_HEADER` (default `X-Forwarded-For`).
  - For `X-Forwarded-For`, the server takes the **right-most address that is not a trusted proxy**.
  - For single-value headers (`X-Real-IP`, `CF-Connecting-IP`), the server takes the value as is.
- Otherwise the client IP is `RemoteAddr`.
- **Misconfiguration warnings** (each logged once per process, at WARN):
  - *Untrusted peer sends the header:* "a request from an untrusted peer carried X-Forwarded-For; it was ignored. If a reverse proxy is in front of this server, add its address to REQHARBOR_TRUSTED_PROXIES". This is the opposite mistake, a proxy with no trusted-proxy configuration, which puts every user in the proxy's bucket: 10 workspace creates per hour and 30 logins per minute for the whole server.
  - *All traffic from one peer:* when the first 200 requests after start all came from the same untrusted, non-loopback peer, "every request so far came from one address; if that is a reverse proxy, set REQHARBOR_TRUSTED_PROXIES, otherwise all users share one rate-limit bucket".
  - The peer address appears in these lines only when `REQHARBOR_LOG_CLIENT_IP=true`.
  - `server/README.md` documents both failure modes, spoofing and the shared bucket, with the compose example's fixed proxy address as the recommended setup (§7.3).
- **Buckets:** IPv4 per address, IPv6 per /64, and IPv4-mapped IPv6 as IPv4 (a port of `rateLimitBucket`).
- **Storage:** IPs are never written to the database. The limiter keys hold `sha256(bucket)` in memory only. Logs carry no IP unless `REQHARBOR_LOG_CLIENT_IP=true` (PRIV-1).

**Limits:** a GCRA limiter in a sharded map, swept every minute. Every limit returns 429 `rate_limited` with `Retry-After`; hosted pages render the same as HTML.

| Key | Default | Env var |
|---|---|---|
| Legacy workspace create, per IP | 10 / h | `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` |
| `/auth/*` page and POST requests, per IP | 120 / min | `REQHARBOR_RATE_AUTH_PER_MIN` |
| Password login, per IP | 30 / min | `REQHARBOR_RATE_LOGIN_PER_MIN` |
| Password attempts, per auth request | 10 total | – |
| Password failures, per (email, IP bucket), persisted | throttle after 5 (below) | – |
| Password failures, per email across all IPs | proof-of-work slow-down after 20 / h (below) | – |
| Account creation, per IP (separate bucket, QA 7) | 30 / h | `REQHARBOR_RATE_SIGNUP_PER_HOUR` |
| `POST /v1/auth/token`, per IP | 60 / min | `REQHARBOR_RATE_TOKEN_PER_MIN` |
| Failed token exchanges, per IP | 30 / h | – |
| Code entry (invite or setup) | 10 per auth request, 30 / h per IP | – |
| Reset requests (M3) | 5 / h per email, 20 / h per IP | – |
| Emails sent, per recipient address, all purposes together (signup code, "you already have an account", reset) (M3) | 5 / h | `REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR` |
| Emails sent, whole server (M3) | 200 / h | `REQHARBOR_RATE_EMAIL_PER_HOUR` |
| `/v1/spaces/*/sync`, per session | 240 / min | `REQHARBOR_RATE_SYNC_PER_MIN` |
| Share create, per space or legacy workspace | 30 / h | `REQHARBOR_RATE_SHARE_CREATE_PER_HOUR` |

The share-create limit is new for legacy workspaces. Today's limit is unbounded within the quota, and the client handles a 429 as `Rejected`.

An email over its cap is silently not sent: the page shows exactly what it would have shown, and the server logs `email_suppressed` with the purpose (never the address). The cap stops a victim's inbox from being flooded through unlimited auth requests and protects the admin's SMTP reputation.

**Password throttle** (replaces the first draft's account-global lockout; survives restarts)
- **Key:** `HMAC-SHA256(throttle_key, normalisedEmail || "\x00" || ipBucket)`, where `throttle_key` is 32 random bytes in `server_state`. It is keyed by the **email**, not the user id, so known and unknown emails behave identically. No IP is stored in clear (PRIV-1 is amended in the README: the database holds these keyed hashes for at most 24 h after the last failure, then GC deletes them).
- **Table** `login_throttle(key_hash PK, email_key, failed_count, locked_until, last_failed_at)` (0004), where `email_key = HMAC-SHA256(throttle_key, normalisedEmail)` is indexed so that "every row for this email" can be deleted without knowing the IPs.
- **Per (email, IP bucket):** after 5 consecutive failures, each further failure from **that bucket** throttles that pair for `min(2^(failed−5), 60)` minutes. While throttled, that pair's attempts are answered without running argon2, for known and unknown emails alike. Attempts for the same email from any other IP bucket are unaffected, so an attacker cannot lock the owner (or the only admin) out.
- **Account-wide, across all IPs: slow-down only, never a refusal.** Kept in memory per normalised email: after 20 failures in the last hour, the login form for that email (known or unknown) requires a proof of work. `pow.js` (an inline script allowed by its CSP hash) finds a nonce such that `SHA-256(challenge || nonce)` has `d` leading zero bits, using WebCrypto, and submits it. `challenge` is 16 random bytes bound to the auth request and the email, single-use, valid 5 minutes. `d` starts at 18 (about a second in a browser) and grows by one for every further 20 failures, up to 22. A correct password with a valid proof always signs in. Verification costs the server one hash. `<noscript>` says "Enable JavaScript to continue signing in."
- **Responses:** identical for known and unknown emails at every stage: the same thresholds, the same text, the same proof-of-work step, and argon2 either run (normal path, real or dummy hash) or not run (throttled pair) regardless of whether the account exists.
- **Clearing:** a successful login deletes that pair's row. A password reset, `user set-password` or **`user unlock --email E`** deletes every row for the email and its in-memory counter.

### 5.10 QA open issues (05-qa-review.md) as they apply to M1

| QA issue | Resolution in this design |
|---|---|
| Doc disagreements (paste-code, route names, rebuild, macOS keychain) | Binding for M1:<br>• v1 is loopback-only, with no paste-code and no `oob` (DEV-3);<br>• hosted pages under `/auth/*` and spaces under `/v1/spaces/{id}` (DEV-1);<br>• no `workspaces` rebuild, with the sentinel `token_hash` (DEV-2, §8.2);<br>• ad-hoc-signed macOS builds use the 0600 file store (DEV-7).<br>Same-id moves and the removal default are M2 topics and follow DEV-5 and DEV-6. |
| IP limits and shared office NAT | Account creation has its own bucket (30 / h). `/auth/*` is 120 / min per IP plus a per-auth-request budget. The persisted (email, IP bucket) throttle plus the account-wide proof of work is the brute-force control, and neither can lock the owner out from another network. Every limit is an env var documented in `server/README.md`. |
| Loopback (a): crafted authorize URL | The hosted page shows the device name, the start time and "Only continue if you just clicked Sign in in ReqHarbor". Google uses `prompt=select_account`. `method=` never auto-redirects. |
| Loopback (b): connection exhaustion | Client: cap *concurrent* connections (8), keep accepting until the matching `state` or the timeout, with a 5 s read timeout per connection. Covered by a core test (§9). |
| Loopback (c): email update on a returning sign-in | `users.email` is updated only from a **trusted** provider email that is unused (§5.4), never from OIDC under `REQUIRE_VERIFIED_EMAIL=false` or from dev. Server test included. |
| Different-account sign-in | Client: on a change of user id, server origin or `instanceId`, every existing binding becomes `Paused{foreignAccount}`. Such bindings are excluded from the first-sync checklist and from push until the SpaceNoticeDialog choice is made, and the old keychain entry is deleted. Core test: A's project never reaches B's personal space (§9). |
| Clock-skew restamp overwrite (personal spaces have `clock_skew` in M1 too) | Client: before restamping, compare with the last pulled remote version. If the remote changed since `synced_at`, run the conflict-copy path instead (§9). |
| `client_versions` scope creep | Dropped. Only the `X-ReqHarbor-Version` header and the optional 426 gate remain. |
| Breaker depends on per-isolate D1 metadata | Gone. The breaker reads `PRAGMA page_count * page_size` and `statfs` free space, sampled every 10 s by one goroutine. It is deterministic and tested with tiny thresholds (§6). |
| Invite abuse, team cap, move/delete of teammates' work, admin-only project delete | M2 (§10 lists the fixes). |

---

## 6. Configuration and operations

Every setting is an environment variable. Any `*_SECRET` or `*_PASSWORD` variable can instead be supplied as `<NAME>_FILE`, which suits Docker secrets. Startup validates everything and exits with code 2 and a one-line reason on any error. An unknown variable in a **limit family** (`REQHARBOR_RATE_*`, `REQHARBOR_QUOTA_*`, `REQHARBOR_MAX_*`) is an error, because a typo there silently keeps a default that an admin or a test relies on: "unknown variable REQHARBOR_LEGACY_CREATE_PER_HOUR (did you mean REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR?)". Other unknown `REQHARBOR_*` variables are logged as warnings.

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_PUBLIC_URL` | **required**, unless `REQHARBOR_LISTEN` is loopback; then `http://<listen address>` | The origin users type in the app and the only origin of the sign-in pages (§5.3): share URLs, OAuth callbacks, reset links, the cookie's `Secure` flag, the http banner. Must be an absolute http(s) origin with no path, query or fragment. Without it on a non-loopback listener, `serve` exits: "REQHARBOR_PUBLIC_URL is required: the address users enter in ReqHarbor, for example https://rh.example.com". In the image the listener is `:8080`, so the image always needs it. |
| `REQHARBOR_LISTEN` | `:8080` | Listen address. The `healthcheck` command derives its URL from it (§7.1). |
| `REQHARBOR_HSTS` | `max-age=31536000` | `Strict-Transport-Security` value, sent only when PUBLIC_URL is https. `off` disables it. No `includeSubDomains` unless the admin adds it (W3). |
| `REQHARBOR_DATA_DIR` | `/data` in the image, `./data` otherwise | Holds `reqharbor.db`, the `-wal` and `-shm` files, `reqharbor.lock` and `backups/` |
| `REQHARBOR_DATABASE_URL` | unset | Reserved for Postgres; refused when set |
| `REQHARBOR_SERVER_NAME` | host of the public URL | Shown in the app and on hosted pages |
| `REQHARBOR_LOG_LEVEL` | `info` | `debug`, `info`, `warn` or `error` |
| `REQHARBOR_LOG_FORMAT` | `json` | `json` or `text` |
| `REQHARBOR_LOG_CLIENT_IP` | `false` | Include the client IP in access logs |
| `REQHARBOR_TRUSTED_PROXIES` | empty | Explicit CIDRs only (§5.9). The compose example trusts only Caddy's fixed address. |
| `REQHARBOR_CLIENT_IP_HEADER` | `X-Forwarded-For` | Or `X-Real-IP`, `CF-Connecting-IP`, … |
| `REQHARBOR_SIGNUP` | `invite` | `open`, `invite` or `closed` |
| `REQHARBOR_MAX_USERS` | `500` | Total accounts; `0` means no cap (§5.6) |
| `REQHARBOR_ADMIN_EMAIL` | empty | A **trusted** email that becomes admin at account creation, only while no admin exists (§5.6) |
| `REQHARBOR_SETUP_CODE` | random | Fixed first-run setup code |
| `REQHARBOR_PASSWORD_AUTH` | `true` | Email and password login and signup |
| `REQHARBOR_ARGON2_MEMORY_KIB` | `65536` | Memory per argon2id hash |
| `REQHARBOR_GITHUB_CLIENT_ID`, `REQHARBOR_GITHUB_CLIENT_SECRET` | empty | Enables GitHub |
| `REQHARBOR_GOOGLE_CLIENT_ID`, `REQHARBOR_GOOGLE_CLIENT_SECRET` | empty | M3: enables Google |
| `REQHARBOR_OIDC_ISSUER`, `REQHARBOR_OIDC_CLIENT_ID`, `REQHARBOR_OIDC_CLIENT_SECRET` | empty | M3: enables generic OIDC |
| `REQHARBOR_OIDC_NAME` | `Single sign-on` | M3: button label |
| `REQHARBOR_OIDC_SCOPES` | `openid email profile` | M3 |
| `REQHARBOR_OIDC_REQUIRE_VERIFIED_EMAIL` | `true` | M3. `false` accepts IdPs without `email_verified`; their emails are never trusted (§5.4). |
| `REQHARBOR_DEV_AUTH` | `false` | Dev sign-in form. Only in binaries built with `-tags devauth`, never in the image; also needs a loopback listener and loopback peers (§5.4). |
| `REQHARBOR_SMTP_HOST`, `REQHARBOR_SMTP_PORT` | empty, `587` | M3: SMTP enables email codes and reset links |
| `REQHARBOR_SMTP_USERNAME`, `REQHARBOR_SMTP_PASSWORD` | empty | M3: PLAIN auth, sent only over TLS |
| `REQHARBOR_SMTP_FROM` | – | M3: required with `SMTP_HOST`, for example `ReqHarbor <rh@acme.dev>` |
| `REQHARBOR_SMTP_TLS` | `starttls` | M3: `starttls`, `tls` (implicit, port 465) or `none`, which is allowed only for a loopback host |
| `REQHARBOR_LEGACY_WORKSPACES` | **`off`** | `off`: every legacy route returns 403 `legacy_disabled`, while `/s/*` pages of existing shares keep working.<br>`existing`: create returns 403 `legacy_disabled` and existing tokens keep working.<br>`open`: anonymous `rh_` workspaces can be created and used by anyone who can reach the server.<br>`open` logs at WARN on every start: "REQHARBOR_LEGACY_WORKSPACES=open: anyone who can reach this server can store up to 50 MB per anonymous workspace and publish public share pages under your domain (/s/…), which can be abused for spam or phishing. Use it only on a private network, or set the caps below." |
| `REQHARBOR_LEGACY_TOTAL_BYTES` | `1073741824` (1 GiB) | Total bytes across all legacy workspaces. When reached, legacy create and growing legacy writes return 507 `server_full`. `0` means no cap. |
| `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` | `5368709120` (5 GiB) | Total bytes across personal spaces of accounts created through open signup. When reached, their growing writes return 507 and open signup shows `server_user_limit`. `0` means no cap. Invited and CLI-created accounts are not counted. |
| `REQHARBOR_TEAMS` | `false` | M2 gate |
| `REQHARBOR_MIN_CLIENT_VERSION` | empty | 426 gate for `/v1/spaces/*` and `/v1/auth/token` |
| `REQHARBOR_QUOTA_LEGACY_BYTES` | `52428800` | Per legacy workspace |
| `REQHARBOR_QUOTA_PERSONAL_BYTES` | `104857600` | Per personal space. `0` means unlimited. |
| `REQHARBOR_QUOTA_TEAM_BYTES`, `REQHARBOR_QUOTA_OWNER_BYTES` | `262144000`, `0` | M2 |
| `REQHARBOR_MAX_OWNED_TEAMS`, `REQHARBOR_MAX_TEAM_MEMBERS`, `REQHARBOR_MAX_TEAM_INVITES` | `10`, `100`, `20` | M2 |
| `REQHARBOR_MAX_DB_BYTES` | `0` (off) | Breaker: growing writes return 507 `server_full` once the DB file reaches this size |
| `REQHARBOR_MIN_FREE_DISK_BYTES` | `536870912` | Breaker: growing writes return 507 when free space in the data dir falls below this |
| `REQHARBOR_SESSION_MAX` | `20` | Live sessions per user |
| `REQHARBOR_RATE_*` | §5.9 | Rate limits |
| `REQHARBOR_BACKUP_INTERVAL` | `0` (off) | M3: for example `24h`, scheduled `VACUUM INTO` on the backup connection |
| `REQHARBOR_BACKUP_KEEP` | `7` | M3: number of scheduled backups to keep |
| `REQHARBOR_SHUTDOWN_TIMEOUT` | `20s` | Graceful-shutdown budget |

**Quotas and the breaker** replace the D1 constraints. The protocol constants stay fixed in code, because the client hard-codes them: 512 KiB per entity, 1 MiB per snapshot, 5 MiB per request body, and 500 changes per push or pull. "Growing write" has the Worker's meaning: push delta > 0, share create, share update with a larger snapshot, account creation, and legacy workspace creation. Deletes, shrinking writes and every read keep working while the breaker is open. The two total-storage caps (`LEGACY_TOTAL_BYTES`, `OPEN_SIGNUP_TOTAL_BYTES`) are breakers of the same kind, but scoped to their class of space: their sums are computed by the breaker sampler every 10 s, so the abuse classes fill up without blocking writes for invited users.

**Health and logging**
- **Endpoints:** `/healthz` and `/readyz`, as described in §4.
- **Format:** `slog` JSON to stdout.
- **Access-log line:** `method`, `route`, `status`, `dur_ms`, `bytes` and `req_id`.
  - `route` is the **route pattern** (`/s/{file}`, `/v1/spaces/{id}/sync`), never the raw path, because share ids and space ids are capabilities or identifiers.
  - Query strings, `Authorization`, cookies and form bodies are never logged.
- **Request id:** every response carries `X-Request-Id`, and 500s log the stack with it.
- **Startup:** one info line listing the enabled methods, the signup mode and the warnings (http public URL, open signup without SMTP, legacy `open` with its abuse text, `MAX_USERS=0`).

**Startup order** (`serve`):
1. Parse and validate the configuration (exit 2 on error).
2. Take the `flock` on `/data/reqharbor.lock`; open the three pools.
3. **Bind the listener and start serving.** `/healthz` answers 200 at once; `/readyz` answers 503 with `migrations: "pending"`; every other route answers 503 `starting`.
4. In the background: the pre-migration backup when migrations are pending and the DB is not empty, on the backup connection; then the migrations (`migrations: "running"`).
5. Generate `instance_id`, `sync_epoch` and `throttle_key` if absent; the setup code if no user exists.
6. Mark ready: `/readyz` returns 200 and the routes open. A migration failure keeps `/readyz` at 503 with the error in the log; `/healthz` stays 200, so an orchestrator does not restart the container in a loop while the admin reads the log.

**Graceful shutdown.** On SIGTERM or SIGINT:
1. `http.Server.Shutdown` within `REQHARBOR_SHUTDOWN_TIMEOUT`.
2. Cancel the background jobs and wait for them.
3. `PRAGMA wal_checkpoint(TRUNCATE)`.
4. Close the pools and release the lock.

Server timeouts: `ReadHeaderTimeout 10s`, `ReadTimeout 120s` (a 5 MB push over a slow link), `WriteTimeout 120s`, `IdleTimeout 120s`, `MaxHeaderBytes 64 KiB`.

**Background jobs** (`internal/jobs`, one goroutine each):
- **GC, every 10 minutes**, deletes:
  - expired auth requests;
  - one-time tokens and sessions that expired or were revoked more than 7 days ago;
  - `login_throttle` rows whose last failure is more than 24 h old;
  - in-memory limiter entries.
- **Daily maintenance:** `PRAGMA optimize` and `PRAGMA incremental_vacuum`. New databases are created with `auto_vacuum=INCREMENTAL`, set by the migration runner before `0001` runs.
- **Breaker sampler, every 10 s:** DB size, free disk, and the two total-storage sums.
- **Scheduled backups** (M3), when enabled, on the backup connection.

**Backups**
- **Online backup:** `reqharbor-server backup [--out /data/backups/manual.db]` runs `VACUUM INTO` in its own process, which gives a consistent snapshot while the server runs and does not block pushes (WAL readers never block the writer). Run it with `docker compose exec reqharbor /reqharbor-server backup`. Scheduled backups (M3) write `backups/reqharbor-YYYYMMDD-HHMMSS.db` and prune down to `REQHARBOR_BACKUP_KEEP`.
- **Before migrations:** when pending migrations exist and the DB is not empty, `serve` takes an automatic `pre-migrate-v<from>-v<to>-<ts>.db` backup on the backup connection, after the listener is up (Startup order above).
- **Off-site:** copy `/data/backups` elsewhere, or run Litestream as a sidecar on `/data/reqharbor.db`. The README gives an example; we do not ship Litestream.
- **Privacy note:** this replaces PRIV-6's D1 Time Travel. Deletion is immediate in the live database, and backups keep deleted data for as long as the admin keeps them.

**Restore: `reqharbor-server restore FILE [--rev-base N]`**

Restoring a backup rolls `workspaces.rev` back below the cursors that clients hold. Clients never lower their cursor (`out.cursor.max(page.cursor)` and `sync.cursor.max(cursor)` in `sync.rs`), so without help they would skip every new rev up to their old cursor, and the changes they made after the backup, already marked synced, would never be pushed again: silent data loss. **Replacing `reqharbor.db` by hand is therefore unsupported**, and the README says so in a warning box. The command does this instead, with the server stopped (it takes the `flock` and refuses while `serve` holds it). With compose: `docker compose stop reqharbor && docker compose run --rm reqharbor restore /data/backups/<file>.db && docker compose start reqharbor`.

1. **Record the pre-restore head.** Open the current `reqharbor.db` read-only, if it exists, and read `rev` for every workspace (legacy and space). If there is no current database (disk loss), `--rev-base N` must be given, or the default `max(restored revs) + 2^32` is used, which is far above any real head and still leaves room for about two million restores below 2^53.
2. **Prepare the restored copy.** Copy `FILE` to `reqharbor.db.restore-tmp`, run `PRAGMA integrity_check`, apply the downgrade guard, and migrate it forward if it is older (taking no pre-migration backup of a temp file).
3. **Shift the revs**, in one write transaction on the copy, for each workspace `w`: `base_w = max(preHead_w, restoredHead_w) + 1` (or the `--rev-base` value), then `UPDATE entities SET rev = rev + base_w WHERE workspace_id = w` and `UPDATE workspaces SET rev = rev + base_w WHERE id = w`. Every restored entity now has a rev above any cursor a client could hold, so clients that never noticed the restore still receive every entity again; order is preserved. If `entities` has a unique index on `(workspace_id, rev)`, the shift runs in two passes (`rev = -rev`, then `rev = -rev + base_w`) to avoid transient collisions.
4. **Rotate `sync_epoch`** in `server_state` (new ULID), and record `restored_at` and `restored_from` (the file name). `instance_id` stays: it is still the same server, and changing it would pause every binding as a foreign server (§9).
5. **Swap.** Rename the current database to `reqharbor.db.pre-restore-<ts>` (kept, never deleted by the command), delete `-wal` and `-shm`, and rename the temp copy into place. Print a summary: workspaces shifted, the new epoch, and "devices that refreshed their sign-in after this backup was taken must sign in again".

What clients then do (the client rule, §9): an M0-or-newer client sees the new `syncEpoch` on its next pull, resets its cursor to 0, marks every entity dirty and re-pushes; LWW makes that idempotent, and its post-backup changes reach the server again. A pre-M0 client (today's release) still receives every entity because of the shift, but its own post-backup changes are not re-pushed; the README tells admins to have such users upgrade to an M0-or-newer release, which detects the new epoch on its first pull and re-pushes. Until then those changes exist only on that device. Workspaces and accounts created after the backup no longer exist: their devices get 401 or 404 `space_not_found`, which the client already handles.

**Tests** (S7): Go: the command refuses while `serve` holds the lock; shifts every rev above the recorded head; rotates `sync_epoch` and keeps `instance_id`; keeps the pre-restore file; migrates an older backup and refuses a newer one; works with `--rev-base` and no current DB. Live (`qa_server_live`, both paths): (a) **with the command:** push A, `backup`, push B from device 1, `restore`, then device 1 pulls, sees the new epoch, re-pushes B, and device 2 ends with A and B; (b) **by hand (unsupported):** the same with the file copied over; the client's cursor-regression rule (§9) recovers device 1, because its first pull after the swap returns a cursor below its stored one. The by-hand path stays unsupported because that detection fails once other devices have pushed the head past device 1's cursor before device 1 pulls; the command has no such window.

**CLI** (`flag` package with manual subcommands). In the image, run it with `docker compose exec reqharbor /reqharbor-server <cmd>`.

```
serve                                   (default)
migrate [--status] [--to N]
user list
user create --email E [--name N] [--admin] [--force] [--password-stdin | --reset-link]
user set-password --email E --password-stdin
user reset-link --email E [--ttl 24h]
user set-admin --email E (--on|--off)
user disable --email E      (revokes all sessions) ; user enable --email E
user unlock --email E       (clears the password throttle for that email, all IP buckets)
user sessions revoke --email E
invite create [--email E] [--uses 1] [--ttl 168h]   → prints code XXXX-XXXX-XXXX-XXXX
invite list ; invite revoke ID
setup-code                              (prints a fresh code; only while no users exist)
backup [--out PATH]
restore FILE [--rev-base N]             (server must be stopped; see Restore above)
healthcheck [--url URL]                 (exit 0/1; used by Docker HEALTHCHECK; default URL from REQHARBOR_LISTEN)
version
```

`healthcheck` without `--url` builds `http://<host>:<port>/healthz` from `REQHARBOR_LISTEN`: an empty host, `0.0.0.0` or `::` becomes `127.0.0.1` or `[::1]`, and the port is the listener's. So `REQHARBOR_LISTEN=:9000` needs no change to the image's `HEALTHCHECK`.

`user delete` comes in M3, together with `DELETE /v1/me` (04 §5.5 semantics).

---

## 7. Docker, release and developer workflow

### 7.1 `server/Dockerfile`

```dockerfile
# syntax=docker/dockerfile:1
FROM --platform=$BUILDPLATFORM golang:1.27-bookworm AS build      # pin @sha256 digest
ARG TARGETOS TARGETARCH VERSION=dev COMMIT=unknown
WORKDIR /src
COPY go.mod go.sum ./
RUN --mount=type=cache,target=/go/pkg/mod go mod download
COPY . .
RUN --mount=type=cache,target=/go/pkg/mod --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 GOOS=$TARGETOS GOARCH=$TARGETARCH \
    go build -trimpath -ldflags="-s -w -X main.version=${VERSION} -X main.commit=${COMMIT}" \
      -o /out/reqharbor-server ./cmd/reqharbor-server \
 && mkdir -p /out/data

FROM gcr.io/distroless/static-debian12:nonroot                     # pin @sha256 digest; CA certs + tzdata included
COPY --from=build /out/reqharbor-server /reqharbor-server
COPY --from=build --chown=65532:65532 /out/data /data
ENV REQHARBOR_DATA_DIR=/data REQHARBOR_LISTEN=:8080
VOLUME ["/data"]
EXPOSE 8080
USER 65532:65532
HEALTHCHECK --interval=30s --timeout=3s --start-period=10s --retries=3 CMD ["/reqharbor-server","healthcheck"]
ENTRYPOINT ["/reqharbor-server"]
CMD ["serve"]
LABEL org.opencontainers.image.source="https://github.com/mihajlopopara/reqharbor" \
      org.opencontainers.image.title="reqharbor-server" org.opencontainers.image.licenses="MIT"
```

- **Build context:** `server/`. Run `docker build -f server/Dockerfile server`.
- **No build tags:** the `go build` line passes no `-tags`, so the dev provider (`//go:build devauth`) is never in the image. There is deliberately no build arg to add tags.
- **Healthcheck:** `healthcheck` reads `REQHARBOR_LISTEN` (§6), and `/healthz` answers before migrations (§6 Startup order), so `--start-period=10s` holds even for a long migration on a large database.
- **Cross-compilation:** the build runs on `$BUILDPLATFORM`, so amd64 and arm64 need no QEMU; the final stage only copies files.
- **Volume permissions:** a host bind mount must be writable by uid 65532. The README shows `chown 65532:65532 ./data`, or using a named volume.

### 7.2 GitHub Actions: `.github/workflows/server.yml`

**Triggers**
- `push` to `main` with paths `server/**` or the workflow file;
- `push` of tags `server-v*`;
- `pull_request` with paths `server/**`;
- `workflow_dispatch`.

Server releases use their own tag namespace (`server-v0.1.0`), because `desktop-build.yml` requires `v*` tags to equal the Tauri version.

**Jobs**
1. **`test`** (`ubuntu-latest`, `working-directory: server`):
   - `actions/checkout@v7`, `actions/setup-go` with `go-version-file: server/go.mod`;
   - `gofmt -l .` must print nothing;
   - `go vet ./...`;
   - `go run honnef.co/go/tools/cmd/staticcheck@<pinned> ./...`;
   - `go run golang.org/x/vuln/cmd/govulncheck@<pinned> ./...`;
   - `CGO_ENABLED=1 go test -race -count=1 ./...`;
   - `CGO_ENABLED=1 go test -race -count=1 -tags devauth ./...` (the dev-provider tests and the harness cases that sign in through it);
   - `go build -o /tmp/rh ./cmd/reqharbor-server && ! go version -m /tmp/rh | grep -q 'devauth'`, which proves the default build has no dev provider.
2. **`image`** (needs `test`; permissions `contents: read`, `packages: write`, `id-token: write`, `attestations: write`):
   - `docker/setup-buildx-action`;
   - `docker/login-action` to `ghcr.io` with `GITHUB_TOKEN`, skipped on pull requests;
   - a `tag` step (on `server-v*` refs only) that classifies the tag with bash: `[[ $GITHUB_REF_NAME =~ ^server-v([0-9]+)\.([0-9]+)\.([0-9]+)(-[0-9A-Za-z.-]+)?$ ]] || exit 1`, and outputs `stable=true` when there is no pre-release suffix (`BASH_REMATCH[4]` empty) and `major_zero=true` when the major is 0;
   - `docker/metadata-action` with `flavor: latest=false` and these tags, all patterns anchored:
     - `type=match,pattern=^server-v(\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?)$,group=1` (every release, pre-releases included, gets its full version)
     - `type=match,pattern=^server-v(\d+\.\d+)\.\d+$,group=1` (`X.Y`, stable only: the pattern cannot match a suffix)
     - `type=match,pattern=^server-v([1-9]\d*)\.\d+\.\d+$,group=1` (`X`, stable and X ≥ 1 only; no `0` tag, because 0.y releases may break each other)
     - `type=raw,value=latest,enable=${{ steps.tag.outputs.stable == 'true' }}`
     - `type=raw,value=edge,enable=${{ github.ref == 'refs/heads/main' }}`
     - `type=sha,prefix=sha-`
   - So `server-v0.2.0` publishes `0.2.0`, `0.2` and `latest`; `server-v0.2.0-rc1` publishes only `0.2.0-rc1`; `server-v1.3.0` adds `1`.
   - `docker/build-push-action` with `context: server`, `platforms: linux/amd64,linux/arm64`, `push: ${{ github.event_name != 'pull_request' }}`, `provenance: mode=max`, `sbom: true`, build-args `VERSION` (the tag without its prefix, or `edge-<sha>`) and `COMMIT`, and `cache-from`/`cache-to: type=gha`.
   - On tags, `actions/attest-build-provenance` for the pushed digest.
3. **Tag check:** on `server-v*`, the tag must match the anchored pattern above, and its version (suffix included) must equal `server/internal/app/version.go`'s `DefaultVersion`. This mirrors the desktop tag check.

**Action pinning:** pin every action to a full commit SHA with a version comment, and let Dependabot (`.github/dependabot.yml`, ecosystems `github-actions`, `gomod` under `/server`, and `docker` under `/server`) keep them current.

**Making the package public:** after the first push, set the GHCR package `reqharbor-server` to **Public** under Package settings, and link it to the repository. GHCR creates packages private by default. This is a one-time manual step, listed in the S3 checklist.

### 7.3 `server/docker-compose.example.yml` (for self-hosters)

```yaml
services:
  reqharbor:
    image: ghcr.io/mihajlopopara/reqharbor-server:0.2   # 0.y: pin the minor; there is no major-only tag before 1.0
    restart: unless-stopped
    environment:
      REQHARBOR_PUBLIC_URL: https://rh.example.com    # required: the address users type in the app
      REQHARBOR_SIGNUP: invite
      REQHARBOR_TRUSTED_PROXIES: 172.30.53.10/32      # Caddy's fixed address below, and nothing else
      # REQHARBOR_LEGACY_WORKSPACES: existing         # default off; see the README before enabling open
      # REQHARBOR_GITHUB_CLIENT_ID: …
      # REQHARBOR_GITHUB_CLIENT_SECRET_FILE: /run/secrets/github_secret
    volumes:
      - reqharbor-data:/data
    networks:
      reqharbor-net:
        ipv4_address: 172.30.53.20
    ports:
      - "127.0.0.1:8080:8080"     # direct access from this host for testing; remove when using Caddy

  caddy:                          # optional: automatic HTTPS. Start with: docker compose --profile tls up -d
    profiles: ["tls"]
    image: caddy:2
    restart: unless-stopped
    ports: ["80:80", "443:443"]
    volumes:
      - ./Caddyfile:/etc/caddy/Caddyfile:ro
      - caddy-data:/data
    networks:
      reqharbor-net:
        ipv4_address: 172.30.53.10
    depends_on: [reqharbor]

networks:
  reqharbor-net:
    ipam:
      config:
        - subnet: 172.30.53.0/24  # fixed, so the trusted proxy address above is stable

volumes:
  reqharbor-data:
  caddy-data:
```

Only Caddy's /32 is trusted. Requests through the `127.0.0.1:8080` test port arrive from the bridge gateway (`172.30.53.1`), which is not trusted, so a forged `X-Forwarded-For` on that port is ignored (and logged once, §5.9). Requests there for `/auth/*` are redirected to `https://rh.example.com` (§5.3). If the chosen subnet collides with an existing network, pick another and change both addresses.

`Caddyfile.example`:

```
rh.example.com {
    encode zstd gzip
    reverse_proxy reqharbor:8080
}
```

The README covers:
- quick start;
- first-run setup (read the code with `docker compose logs reqharbor | grep setup`);
- registering OAuth and OIDC apps with the exact callback URLs, and why `ADMIN_EMAIL` does not work with `REQUIRE_VERIFIED_EMAIL=false`;
- SMTP (M3);
- trusted proxies: the two failure modes (a too-wide trust list lets clients spoof their bucket; a missing one puts everyone in the proxy's bucket) and the warnings that point at each;
- token sync (`REQHARBOR_LEGACY_WORKSPACES`): off by default, the abuse risk of `open`, and the total-storage caps;
- backups and restore: **only `reqharbor-server restore` is supported**, never copying the file over;
- upgrades: pull the new image, restart, and migrations run automatically with a pre-migration backup;
- the full configuration table (§6);
- the single-instance rule;
- the privacy note.

### 7.4 Developers: Go inside Docker in this repo

Add a service to `compose.yaml`; the dev image stays Rust and Node only:

```yaml
  go:
    image: golang:1.27-bookworm           # pin the exact patch + @sha256 digest, same as server/Dockerfile
    working_dir: /app/server
    volumes:
      - .:/app
      - go-mod:/go/pkg/mod
      - go-build:/root/.cache/go-build
      - server-data:/data
    environment:
      CGO_ENABLED: "0"
      GOTOOLCHAIN: local
      GOFLAGS: -mod=readonly
      REQHARBOR_DATA_DIR: /data
      REQHARBOR_PUBLIC_URL: http://localhost:8080
      REQHARBOR_LEGACY_WORKSPACES: open
      # No REQHARBOR_DEV_AUTH here: the dev provider needs a devauth build and a loopback listener (§5.4).
    ports:
      - "127.0.0.1:8080:8080"             # never on all interfaces: other people on the LAN or café Wi-Fi must not reach it
# volumes: add go-mod, go-build, server-data
```

For manual work with the desktop app against this server, sign in with a password: the setup code is in the `serve` output on first start, or run `docker compose run --rm go go run ./cmd/reqharbor-server user create --email me@example.test --admin --password-stdin`. The dev provider is for automated tests only, which run the server and the client in the same container (§7.5) or in-process (the Go harness).

Commands, all from the repo root:

```sh
docker compose run --rm go go test ./...                                  # unit + HTTP tests
docker compose run --rm go go test -tags devauth ./...                    # plus dev-provider tests
docker compose run --rm -e CGO_ENABLED=1 go go test -race ./...           # race detector (gcc is in the image)
docker compose run --rm go go vet ./...
docker compose run --rm go gofmt -l .
docker compose run --rm --service-ports go go run ./cmd/reqharbor-server serve   # local server on :8080 for the app
docker compose run --rm go go build -o bin/reqharbor-server ./cmd/reqharbor-server
docker build -f server/Dockerfile -t reqharbor-server:dev server          # image build (host Docker)
```

The first time a module is added (S0), run `docker compose run --rm -e GOFLAGS= go go mod tidy`, because `-mod=readonly` blocks `go.sum` writes.

### 7.5 Live client suite against the Go server

`qa/run_live_server_tests.sh` is the sibling of `run_live_worker_tests.sh`. It runs inside the **dev** container:
1. Requires `server/bin/reqharbor-server-devauth`, built in the previous step by the `go` service with `-tags devauth`. The distinct name keeps it from ever being mistaken for a release binary. It is static, so it runs in the dev image.
2. Starts it with a temporary data dir and exactly these settings, then waits for `/readyz`:
   - `REQHARBOR_LISTEN=127.0.0.1:8080` and `REQHARBOR_PUBLIC_URL=http://127.0.0.1:8080` (loopback, as the dev provider requires; the client and the server share the container's network namespace);
   - `REQHARBOR_DEV_AUTH=true`;
   - `REQHARBOR_LEGACY_WORKSPACES=open`;
   - `REQHARBOR_TRUSTED_PROXIES=127.0.0.1/32` and `REQHARBOR_CLIENT_IP_HEADER=CF-Connecting-IP`, matching the Go harness;
   - **no** rate-limit override: the legacy-create limit stays at the default 10 / h.

   Why this passes: `worker_smoke.sh` sends `CF-Connecting-IP`, now honoured because 127.0.0.1 is a trusted proxy. Its two normal creates use one random `203.0.113.x`, and its 11-create rate-limit loop uses a separate random `192.0.2.x`, which reaches 429 at the 11th create under the default 10 / h, as its check expects. Its last create sends no header and lands in the 127.0.0.1 bucket. The Rust client sends no such header either, so `qa_sync_live`'s 5 `new_workspace()` calls join that bucket: 6 creates in all, under 10. `qa_server_live` sends random fake IPs on its raw creates (§1.6). Because unknown `REQHARBOR_RATE_*` names now fail startup (§6), a misspelt override cannot silently keep a default again.
3. Runs `BASE=http://127.0.0.1:8080 bash qa/worker_smoke.sh`.
4. Runs `REQHARBOR_WORKER_URL=http://127.0.0.1:8080 cargo test -p reqharbor-core --test qa_sync_live -- --ignored --test-threads=1`.
5. Runs `--test qa_server_live` the same way. Its restore case stops the server, runs `reqharbor-server-devauth restore`, and starts it again.
6. From M1-client onward, also runs `--test qa_auth_live` (§9).
7. Prints the server's error-level log lines.

```sh
docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
docker compose run --rm dev bash qa/run_live_server_tests.sh
```

---

## 8. Data model (SQLite, numbered migrations)

### 8.1 Migration runner (`internal/db/migrate.go`)

- **Embedding:** `//go:embed migrations/*.sql`. Each file name is `NNNN_name.sql`, applied in ascending order.
- **Bookkeeping:** `schema_migrations(version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at INTEGER NOT NULL)`.
- **New databases:** `PRAGMA auto_vacuum=INCREMENTAL` runs before anything else.
- **Applying a migration:**
  - Each migration runs in its own `BEGIN IMMEDIATE` transaction as one multi-statement `Exec` (modernc executes every statement in the string; verify this in S0, and fall back to splitting on `;` at the end of a line if needed).
  - The runner inserts the `schema_migrations` row and runs `PRAGMA foreign_key_check` before commit. Any result row aborts the migration.
- **Migrations that rebuild a table:** a file whose first line is `-- reqharbor:foreign-keys-off` runs with `PRAGMA foreign_keys=OFF` set **outside** the transaction, followed by the same check. No M1 migration needs this. It exists so that a rebuild can never cascade-delete data (04 §1.1).
- **Downgrade protection:** if the DB's maximum version is greater than the binary's newest migration, startup fails with "database schema vN is newer than this reqharbor-server (max vM); use a newer image".
- **Startup and CLI:** `serve` migrates automatically after the pre-migration backup. `migrate --to N` exists for tests and for the D1 import note (§8.5).

### 8.2 `0001_init.sql` and `0002_device_id_and_quota.sql`

Both are copied **byte for byte** from `worker/migrations/`, because D1 SQL is SQLite SQL. Their `UPDATE` statements are no-ops on an empty database. The resulting schema is identical to D1's, which keeps the D1 import path (§8.5) trivial.

### 8.3 `0003_server_state.sql` (S1) and `0004_accounts.sql` (S4)

`0003_server_state.sql` ships with the legacy-parity release, because `/v1/server` (`instanceId`, `syncEpoch`) and the in-process limiter exist from S1:

```sql
-- Rate limits are in-process now (06 §5.9).
DROP TABLE rate_limits;

-- Keys: 'instance_id' and 'sync_epoch' (ULIDs, generated at first start; restore rotates sync_epoch),
-- 'restored_at', 'restored_from' (set by restore), 'throttle_key' (S4: 32 random bytes, base64).
CREATE TABLE server_state (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

`0004_accounts.sql`:

```sql
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
CREATE INDEX users_open_signup ON users (signup_via) WHERE signup_via = 'open';   -- OPEN_SIGNUP_TOTAL_BYTES sum

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
CREATE UNIQUE INDEX identities_one_per_user ON identities (user_id);   -- v1: one method per account (D15c)

CREATE TABLE password_credentials (
  user_id      TEXT PRIMARY KEY REFERENCES users(id) ON DELETE CASCADE,
  phc          TEXT NOT NULL,                         -- $argon2id$v=19$m=…,t=…,p=…$salt$hash
  updated_at   INTEGER NOT NULL
);

-- §5.9: keyed by HMAC(throttle_key, email || ip bucket), so unknown emails get rows too and no IP is stored.
CREATE TABLE login_throttle (
  key_hash       TEXT PRIMARY KEY,
  email_key      TEXT NOT NULL,                       -- HMAC(throttle_key, email): "all rows for this email"
  failed_count   INTEGER NOT NULL DEFAULT 0,
  locked_until   INTEGER NOT NULL DEFAULT 0,
  last_failed_at INTEGER NOT NULL
);
CREATE INDEX login_throttle_email ON login_throttle (email_key);
CREATE INDEX login_throttle_gc ON login_throttle (last_failed_at);
-- One sign-in method per account in v1 (dropped in M4 "link another method").
CREATE TRIGGER one_method_identity BEFORE INSERT ON identities
  WHEN EXISTS (SELECT 1 FROM password_credentials WHERE user_id = NEW.user_id)
  BEGIN SELECT RAISE(ABORT, 'account already uses a password'); END;
CREATE TRIGGER one_method_password BEFORE INSERT ON password_credentials
  WHEN EXISTS (SELECT 1 FROM identities WHERE user_id = NEW.user_id)
  BEGIN SELECT RAISE(ABORT, 'account already uses an external identity'); END;

-- Every account owns exactly one hidden personal team; its workspace is the personal space (DEV-4).
CREATE TABLE teams (
  id            TEXT PRIMARY KEY,
  name          TEXT NOT NULL,
  personal      INTEGER NOT NULL DEFAULT 0,
  owner_user_id TEXT NOT NULL REFERENCES users(id),   -- billing owner; NO ACTION on purpose (04 §5.5)
  created_at    INTEGER NOT NULL
);
CREATE UNIQUE INDEX teams_one_personal ON teams (owner_user_id) WHERE personal = 1;
CREATE INDEX teams_owner ON teams (owner_user_id, personal);

CREATE TABLE team_members (
  team_id   TEXT NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
  user_id   TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  role      TEXT NOT NULL CHECK (role IN ('owner','admin','editor','viewer')),
  joined_at INTEGER NOT NULL,
  PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);

-- Account spaces store token_hash = 'space:'||id, which is never a SHA-256 hex digest, so legacy
-- authenticate() can never match one (no table rebuild: entities/shares cascade from workspaces).
ALTER TABLE workspaces ADD COLUMN team_id TEXT REFERENCES teams(id) ON DELETE CASCADE;
CREATE UNIQUE INDEX workspaces_one_per_team ON workspaces (team_id) WHERE team_id IS NOT NULL;

ALTER TABLE entities ADD COLUMN updated_by TEXT NOT NULL DEFAULT '';   -- user id; '' = legacy/anonymous
ALTER TABLE shares   ADD COLUMN created_by TEXT;                        -- no FK: "Former member"
ALTER TABLE shares   ADD COLUMN options    TEXT;                        -- {"includeCredentials":bool,"environmentId":string|null}

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
CREATE INDEX sessions_user ON sessions (user_id, revoked_at);
CREATE INDEX sessions_prev_refresh ON sessions (prev_refresh_hash);

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
CREATE INDEX auth_requests_expiry ON auth_requests (expires_at);

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
CREATE INDEX one_time_tokens_expiry ON one_time_tokens (expires_at);
```

**`accounts.Create`** runs in one write transaction:
1. Check whether `SELECT COUNT(*) FROM users` is 0, enforce `REQHARBOR_MAX_USERS` (except for the first user and `--force`), and consume the grant token (setup or invite) when one is needed.
2. `INSERT users` with `email_trusted` from the §5.4 table, `signup_via`, and `is_admin` = first user, **or** a setup code was used, **or** (`REQHARBOR_ADMIN_EMAIL` equals the email **and** `email_trusted = 1` **and** `NOT EXISTS (SELECT 1 FROM users WHERE is_admin = 1)`). This is the only place `ADMIN_EMAIL` is ever evaluated.
3. `INSERT identities` **or** `password_credentials`.
4. `INSERT teams(personal=1, name='Personal')`.
5. `INSERT team_members(role='owner')`.
6. `INSERT workspaces(id=ULID, name='Personal', token_hash='space:'||id, rev=0, team_id)`.

Steps 4–6 set up **personal space ownership**: the owner is the only member of the personal team, and `teams.owner_user_id` is the billing owner for M2 aggregates.

**`email_in_use`** is checked inside the same transaction. The `UNIQUE` constraint on `users.email` is the backstop.

### 8.4 D1-specific constraints that no longer apply

| D1 constraint in the plan | Replacement |
|---|---|
| 100 bound parameters per statement (`ROWS_PER_STATEMENT = 14`, `IDS_PER_LOOKUP = 90`, the `updated_by` shared-parameter trick, the "14 rows = 100 params" regression test) | Gone. Per-row prepared statements inside one write transaction. The test is dropped. |
| LWW computed in SQL `CASE WHEN` inside one D1 batch, because there were no interactive transactions | LWW in Go (`syncsvc`) inside a serialised write transaction. It is easier to read, and the same truth table is unit-tested. |
| Breaker on `meta.size_after` with a per-isolate cache (AC-QUOTA-4, QA cold-isolate issue) | `REQHARBOR_MAX_DB_BYTES` and `REQHARBOR_MIN_FREE_DISK_BYTES`, sampled every 10 s, returning 507 `server_full` on growing writes. |
| D1 Free/Paid sizing (D10: 25 MB/25 MB/100 MB caps to fit 500 MB) | Admin-configured quotas per space (§6). The defaults are larger because the admin owns the disk. The owner aggregate is off by default. |
| `[[ratelimits]]` Workers bindings and the D1 `rate_limits` table | In-process limiter (§5.9). `rate_limits` is dropped in 0003 (S1). |
| `[triggers] crons` for GC | `internal/jobs` tickers |
| `wrangler secret put`, `PUBLIC_BASE_URL` var | Env vars and `*_FILE` |
| `CF-Connecting-IP` always present | Trusted-proxy configuration (§5.9) |
| `__Secure-` cookie everywhere | `__Secure-` only on https |
| D1 Time Travel as the deletion backstop | Admin backups (§6) |
| `TEAMS_ENABLED` Worker var | `REQHARBOR_TEAMS` |

### 8.5 Importing a D1 database (documentation only, not built)

No D1 database was ever deployed. Anyone who deployed a fork can move their data:
1. `npx wrangler d1 export <db> --remote --no-schema --output d1.sql`.
2. `reqharbor-server migrate --to 2` on an empty data dir.
3. Load the file with any `sqlite3` binary on the host (`sqlite3 data/reqharbor.db < d1.sql`). It cannot be done inside the distroless image.
4. Start `serve`, which migrates the rest.

### 8.6 Migration tests (`internal/db/migrate_test.go`)

- **Fresh database:** after 0001–0003 the schema equals `testdata/schema_v3.sql`, and after 0001–0004 `testdata/schema_v4.sql`, the golden outputs of `sqlite_master`.
- **Populated database:** apply 0001 and 0002, load `testdata/legacy_seed.sql` (3 workspaces, 1,000 entities, shares, revoked shares), then apply 0003 and 0004.
  - No rows are lost, and `PRAGMA foreign_key_check` is clean.
  - Every legacy HTTP route returns the same JSON before and after each migration, apart from the added `syncEpoch` on pulls (W10).
- **Downgrade guard:** a database at version 99 makes `Open` fail.
- **Pre-migration backup:** the file exists and opens; `/healthz` answers 200 while it and the migrations run, and `/readyz` and `/v1/sync` answer 503 until they finish (a blocking-store test).
- **Backup does not block writes:** a push completes while a `VACUUM INTO` on the backup connection is held open.

---

## 9. Client changes for M1 that differ from 04-tech-design.md

These come from self-hosting and the absence of a default server. Everything else in 04 §2.4, §3.2–§3.3 and §6–§10 stands.

**Server entry and discovery**
- **No default server:** `REQHARBOR_DEFAULT_SERVER` (04 §11) is **dropped**.
- **Settings › Account, signed out:**
  - The server field is always visible and starts empty, with the placeholder `https://reqharbor.example.com`.
  - **Sign in** stays disabled until discovery succeeds.
  - Earlier servers are offered as suggestions, stored in `settings.json` as `knownServers` (not secret).
  - This is the "no default server" branch of UX §3.2.
- **`server_discover(serverUrl) -> ServerInfo`** replaces `auth_providers`.
  - **URL normalisation:** trim, add `https://` when no scheme is given, lowercase scheme and host, drop the default port and any trailing `/`. A path, query or fragment is rejected with "Enter the server's address without a path".
  - **Request:** `GET {url}/v1/server` with a 10 s timeout.

  Rows are evaluated in this order; the first match wins.

  | Result | Status | Message |
  |---|---|---|
  | Network error | `unreachable` | "Can't reach rh.acme.dev. Check the address and your connection." |
  | 404 with `error.code == "not_found"` (the Worker) | `token_sync_only` | "This server only supports token sync. Use Sync › Advanced." |
  | 200 without `software == "reqharbor-server"`, unparsable, or `apiVersion` missing or < 1 | `not_reqharbor` | "This doesn't look like a ReqHarbor server." |
  | `minClientVersion` above the running version | `client_too_old` | "This server needs ReqHarbor X or newer." Sign-in is blocked. |
  | `features.accounts == false` (`server-v0.1.0`, or accounts disabled) | `token_sync_only` | Same as the 404 row when `features.legacyWorkspaces != "off"`; otherwise "This server has no sync enabled yet. Ask its admin." |
  | `apiVersion` higher than the client knows | – | **Compatible**: the client continues with the fields it knows. `apiVersion` bumps only for breaking `/v1/*` changes, and a server that drops support for an old client says so through `minClientVersion` (row above), never through `apiVersion` alone. |
  | `setupRequired` | – | "This server hasn't been set up yet. Its admin will find the setup code in the server log." Sign-in is still allowed, because the hosted page asks for the code. |
  | `auth.methods` empty | – | "This server doesn't have sign-in set up. Ask its admin, or sync with a token under Sync › Advanced." |

- **Canonical address:** after discovery, the client normalises `publicUrl` and compares it with the origin the user entered. When they differ (for example the user typed `http://192.168.1.5:8080` and `publicUrl` is `https://rh.acme.dev`), Settings shows "This server's address is https://rh.acme.dev. Use that address?" with **Use https://rh.acme.dev** (the default) and **Keep what I typed**. Using it re-runs discovery on the new origin and stores that origin. Keeping the typed one still works: API calls go to the typed origin, and the browser part of sign-in is redirected to `publicUrl` by the server (§5.3). The choice is remembered per origin in `knownServers`.

- **Plain-http warning:** when the scheme is `http` and the host is not loopback (`localhost`, `127.0.0.0/8`, `::1`), the app shows the warning "This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network." It has a **Continue anyway** checkbox, and the acknowledgement is stored per origin in `knownServers`. The same warning applies to the legacy Sync › Advanced connect.

**Sign-in**
- **Methods:** the signed-out view shows one button per entry in `ServerInfo.auth.methods`, with the server's label: "Continue with GitHub", "Continue with Acme SSO", "Sign in with email". Unknown method ids get a generic "Continue with {label}" button, so an M1 client works with an M3 server's Google or OIDC.
  - Every button runs the same loopback flow, adding `&method=<id>`.
  - With no methods, the view shows the empty-methods message from the table above.
- **Signup and codes:** signup, invite codes and setup codes are handled entirely on the hosted page. The app never asks for a password.
- **Loopback listener** (QA 9b): at most 8 concurrent connections, a 5 s read timeout each, accepting until the matching `state` arrives or the 10-minute timeout passes. A `/cb` with the wrong state returns 400 and the listener keeps going.
- **Token exchange:** the client sends a form-encoded body with the RFC 6749 names (`grant_type`, `client_id=reqharbor-desktop`, `code`, `code_verifier`, `redirect_uri`; or `grant_type=refresh_token`, `client_id`, `refresh_token`), reads `access_token`, `refresh_token`, `expires_in`, `session_id` and `user`, and sends the `X-ReqHarbor-Version` header (M0) (§5.8).

**Per-server identity and storage**
- **`account.json` `account`** = `{serverUrl /* normalised origin */, serverInstanceId, serverName, userId, email, name, method, sessionId}`.
- **Keychain entry:** `session:<serverOrigin>:<userId>`. `serverOrigin` is the normalised origin, for example `https://rh.acme.dev` or `http://192.168.1.5:8080`, so sessions on different servers never collide.
- **One signed-in account at a time in M1.**
- **`SpaceBinding`** gains `server_origin` and `server_instance_id`. A binding whose origin or instance id differs from the current account becomes `Paused{foreignAccount}` (QA "different account", extended to different servers):
  - it is excluded from push and from the first-sync checklist;
  - SpaceNoticeDialog offers Keep (local-only copies, the focused default) or Remove;
  - the old keychain entry is deleted on sign-out or when that choice is made.
- **Rediscovery:** at app start and before each sign-in, the app fetches `GET /v1/server` again, with a 24 h cached copy for offline use. A changed `instanceId` for the same origin counts as a different server.

**Sync types the server accepts** (Q5; applies to legacy token sync and to spaces)
- The client reads `sync.types` from discovery (cached with the rest of `ServerInfo`; the five base types when the server is the Worker or discovery fails).
- It pushes only changes whose type is listed. A dirty entity of an unlisted type (for example a future `protoFile` against an older self-hosted server) stays dirty and local-only, and the sync status shows one notice per binding: "3 items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server." The notice clears when a rediscovery lists the type.
- A per-change `rejected` with `unsupported_type` (a server that changed between discovery and push) is handled the same way: the entity stays dirty and local-only, and the next rediscovery decides.
- Pulled changes of unknown types are kept raw, as today (AC-FWD-5).

**Restore detection** (Q4; an M0 change in `crates/core/src/sync.rs`, applied to legacy sync now and to space bindings in C2)
- The stored sync state gains `sync_epoch: Option<String>` next to `cursor`.
- **Rule**, checked on every pull page **before** the cursor is advanced: if the response's `syncEpoch` is present and differs from the stored non-empty epoch, **or** the page's `cursor` is lower than the `since` the client sent, the server was restored. The client then sets the cursor to 0, stores the new epoch, marks **every** entity bound to that workspace or space dirty, and starts the pull again from 0. Its next push re-sends everything; LWW (contract C3) makes that idempotent, because an equal `updatedAt` and `deviceId` is simply applied again.
- The unconditional `cursor.max(page.cursor)` and `sync.cursor.max(cursor)` are replaced by this check followed by a plain assignment.
- A first pull (no stored epoch) only stores the epoch. A server without `syncEpoch` (the Worker) never triggers the epoch half of the rule.

**Legacy and errors**
- **Legacy URL wording:** the Sync › Advanced field becomes "Server URL". The Tauri argument names stay `workerUrl`, so `types.ts`, the mock and commands need no churn.
- **`SyncClient::new` error:** changes to "Server URL must start with https:// or http://".
- **Mixed servers:** the legacy server and the account server may differ.
- **Clock-skew restamp** (QA): restamp only entities whose remote version is unchanged since `synced_at`; otherwise make a conflict copy.
- **New error mappings in `send()`:** 426 `client_too_old` gives `ClientTooOld`, 507 `server_full` gives `ServerFull` (retryable, with backoff), and 403 `legacy_disabled` gives `Rejected` with the copy "This server no longer accepts token sync. Sign in instead."

**Types and mock**
- **Types** (`docs/types.ts` and `ui/src/lib/api/types.ts`, byte-identical):
  - `ServerInfo { software, version, apiVersion, instanceId, syncEpoch, name, publicUrl, minClientVersion: string | null, auth: { methods: AuthMethod[]; signup: "open" | "invite" | "closed"; passwordReset: "email" | "admin"; setupRequired: boolean }, features: { accounts: boolean; teams: boolean; legacyWorkspaces: "open" | "existing" | "off" }, sync: { types: string[]; requestKinds: string[] } }`
  - `AuthMethod { id: "password" | "github" | "google" | "oidc" | (string & {}); label: string }`
  - `SignInStatus.error` gains `insecure_unconfirmed`, `not_reqharbor`, `token_sync_only` and `client_too_old`.
  - `Account` gains `serverName`.
- **Mock switches:** `?mockServer=ok|tokenOnly|legacyOnly|futureApi|otherPublicUrl|unreachable|notReqharbor|insecure|setup|tooOld` and `?mockMethods=password,github,oidc`.
  - `tokenOnly`: the Worker's 404.
  - `legacyOnly`: 200 with `features.accounts:false`, no methods (`server-v0.1.0`); expected `token_sync_only`.
  - `futureApi`: 200 with `apiVersion: 2` and an extra unknown field; expected to behave like `ok`.
  - `otherPublicUrl`: `publicUrl` differs from the entered origin; expected the "Use that address?" prompt.

**Client M1 tests in addition to 04 §12 M1c/M1d**
- **Core:**
  - URL normalisation table.
  - Discovery maps 404 and `features.accounts:false` to `token_sync_only`; `apiVersion: 2` with `minClientVersion: null` is compatible; `apiVersion: 0` is `not_reqharbor`.
  - `publicUrl` differing from the entered origin raises the switch prompt; equal after normalisation does not.
  - Push filtering: with `sync.types` lacking `protoFile`, a dirty `protoFile` entity is not sent and stays dirty; the notice counts it.
  - Restore detection (M0): an epoch change, and separately a cursor below `since`, each reset the cursor to 0, mark every entity dirty and re-pull; a first pull only stores the epoch; the Worker (no `syncEpoch`) never resets on epoch.
  - Token exchange sends the RFC 6749 form fields and parses the snake_case response.
  - `instanceId` change → bindings `Paused`.
  - A's team or personal project never pushes to server B.
  - Keychain key per origin.
  - Loopback survives 100 junk connections with at most 8 concurrent.
  - Restamp-versus-conflict rule.
- **`crates/core/tests/qa_auth_live.rs` (ignored, run by §7.5):** a `reqwest` client with a cookie jar plays the browser against the dev provider of the `devauth` Go build, over loopback. It runs authorize, dev sign-in, the redirect to the core's real loopback listener, and the exchange. It then checks:
  - push and pull in the personal space;
  - refresh rotation and grace;
  - logout gives 401 and then `SignInRequired`.
- **Playwright:** the server-entry states above, the http warning, the method buttons, and the token-sync-only server.

---

## 10. Phased plan

Every phase ends with `docker compose run --rm go go test ./...` green and, from S3 onward, `qa/run_live_server_tests.sh` green.

### M1-server

| Phase | Files | Tests | Exit |
|---|---|---|---|
| **S0 Scaffold** | `server/go.mod`, `cmd/reqharbor-server/{main,serve,migrate,healthcheck}.go`, `internal/{config,db,store,store/sqlite,httpx,clock,ids,jsx,app}`, `db/migrations/0001,0002`, `web/embed.go`, `Dockerfile`, `compose.yaml` (`go` service and volumes), `.github/workflows/server.yml`, `.github/dependabot.yml`, `.gitignore` | `config_test` (defaults, validation errors, `*_FILE`, unknown-var warning, unknown `REQHARBOR_RATE_*`/`QUOTA_*`/`MAX_*` is a startup error, PUBLIC_URL required unless the listener is loopback, `REQHARBOR_HSTS` validation, `DEV_AUTH=true` in a non-devauth build exits 2); `migrate_test` (fresh, golden schema v2, downgrade guard); startup order (`/healthz` 200 while migrations block, `/readyz` and other routes 503 `starting`); `healthcheck` URL derived from `REQHARBOR_LISTEN`; `httpx` tests (`methods` 405 with `Allow`, catch-all 404, `readJSON` order matrix, security-header wrapper, `Set-Cookie` stripping, recover → 500 without leak); `/healthz`, `/readyz` | The image builds for amd64 and arm64 in CI, and `edge` is pushed on `main` |
| **S1 Legacy API** | `internal/{legacy,syncsvc,shares,ratelimit,clientip,jsonx}`; `db/migrations/0003_server_state.sql`; `testutil/harness.go`; `discovery` (`/v1/server` with `accounts:false`, `instanceId`, `syncEpoch`, `sync.types`); `crates/core/tests/serde_parity.rs` | Port `http.test.ts`, `sync.test.ts`, `shares.test.ts` (API cases) and the security half of `qa_parity_security.test.ts`, per §1.6, plus the share check-order cases; `syncsvc` LWW truth table; rev assignment in push order including stale bumps; quota inside the transaction; breaker; `LEGACY_WORKSPACES` `off` (default) / `existing` / `open` matrix and `LEGACY_TOTAL_BYTES`; per-change `invalid_json_value` for `1e400`, `"\uD800"` and depth 101 while the rest of the push applies; `syncEpoch` on pulls; the serde parity fixture (Go) and `serde_parity.rs` (Rust, in the dev container); trusted-proxy parsing (no `private` keyword) and the two one-time warnings; HSTS default and `off`; `FuzzCanonicalize`; `FuzzPushValidation` | All ported legacy tests pass |
| **S2 Share page and cURL** | `internal/{sharepage,curlexport}`, `web/templates/share*.html`, `web/static/share.{css,js},logo.svg` | `curl.test.ts`, `curl_parity` (fixture from `worker/test/fixtures`), the QA parity cases, the share-page cases of `shares.test.ts`, CSP-hash equality, the AC-FWD-6 fallback card for `kind: "grpc"` and `"websocket"`, `FuzzSharePage` | The fixture passes all 39 cases |
| **S3 Parity release** | `qa/run_live_server_tests.sh`; `crates/core/tests/qa_server_live.rs` (poison-value case); `server/README.md` (legacy-only quick start, token sync off by default) | `qa_sync_live`, `qa_server_live` and `worker_smoke.sh` against the devauth build, with the §7.5 settings | Tag `server-v0.1.0` and set the GHCR package to public. **`worker/` is frozen**, with a note at the top of `worker/README.md`. |
| **S4 Accounts core** | `db/migrations/0004_accounts.sql`; `internal/{accounts,auth,auth/throttle}`; `web/templates/{layout,authorize,signup,code,done,error,forgot,reset}.html`, `static/{auth.css,reset.js,pow.js}`; `cmd/.../{user,invite,setupcode}.go`; `discovery` (methods, signup, `setupRequired`) | `auth_test.go`:<br>• PKCE happy path via password, wrong verifier, code reuse, expired code, failed exchange burns the code;<br>• token endpoint: RFC 6749 snake_case fields, form and JSON bodies, `client_id` missing or wrong gives `invalid_grant`, other `Content-Type` gives 415, snake_case response;<br>• `redirect_uri` matrix (`localhost`, `[::1]`, `https`, `oob`, port 80, `/cb/x`, trailing slash);<br>• cookie missing or mismatched gives a login-CSRF reject;<br>• cross-origin POST rejected by `CrossOriginProtection`;<br>• **canonical origin:** PUBLIC_URL `https://reqharbor.test` and a request to `http://127.0.0.1:P/auth/authorize?…` gives 302 to `https://reqharbor.test/auth/authorize?…` with the query intact and no `Set-Cookie`; the flow then completes on the canonical origin; an off-origin POST gives `wrong_origin`; a matching host with unknown scheme from an untrusted peer is served (no loop) and logs the warning once;<br>• `Set-Cookie` only under `/auth/`, and a `/v1` handler ignores a forged cookie;<br>• CSP on every `/auth/*` page;<br>• refresh rotation; the grace window returns the identical pair at 59 s and revokes at 61 s; reuse revokes; logout; the 20-session cap; a disabled user gets 401;<br>• signup modes × (no code, invite code, wrong code, setup code, admin email trusted and untrusted);<br>• `REQHARBOR_MAX_USERS` reached gives `server_user_limit`; the first account is never refused;<br>• the setup code works only while there are 0 users, and two parallel setups produce exactly one admin;<br>• **ADMIN_EMAIL:** applied at creation only while no admin exists; ignored once an admin exists; an admin demoted with `user set-admin --off` stays demoted after signing in again; a password signup without SMTP using the admin email is not admin;<br>• **throttle:** 5 failures from IP A throttle (email, A); a login with the right password from IP B succeeds at once; after 20 failures across IPs the proof of work is required and a right password with a valid proof succeeds; an invalid or reused proof fails; known and unknown emails give byte-identical responses at every stage and run argon2 on the same paths; rows survive a harness restart; `user unlock` and a reset clear every row for the email; GC removes rows after 24 h;<br>• reset by CLI link (fragment token, single use, 24 h, revokes sessions, does not set `email_trusted`);<br>• signup with an existing email (no SMTP) does not name the method; `email_in_use` creates no rows;<br>• the one-method triggers;<br>• tokens never appear in error bodies or logs (capture the `slog` output). | `/v1/server` advertises `password` |
| **S5 GitHub and dev providers** | `internal/auth/providers/github.go`, `providers/dev.go` (`//go:build devauth`) | Fake provider servers with `httptest.NewTLSServer`. Test-only base-URL overrides are set through `config` fields that no env var reaches.<br>• GitHub: primary verified email selection, no verified email, email trusted;<br>• returning user: email updated only when trusted and unused; `is_admin` unchanged by sign-in;<br>• `email_in_use` page: method named for a trusted incoming email only;<br>• `access_denied` is propagated to the loopback;<br>• dev provider (devauth tests): refused from a non-loopback `RemoteAddr` even with a loopback `X-Forwarded-For`; startup refused with a non-loopback `REQHARBOR_LISTEN`; a dev identity whose email equals `ADMIN_EMAIL` is not admin and is `email_trusted=0`;<br>• default build: `go version -m` shows no `devauth`, and `DEV_AUTH=true` exits 2. | |
| **S6 Personal spaces** | `internal/spaces`; `syncsvc` actor support (`updated_by`, `clock_skew`, `serverTime`, `updatedBy` join) and per-change `unsupported_type`; `shares` scope with `created_by` and `options`; version gate; `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` | `spaces_test.go`:<br>• a personal space is created at signup;<br>• `/v1/me`, `/v1/me/sessions` (revoke another session, another user's id gives 404), `/v1/me/spaces`;<br>• pull and push round trip with `updatedBy`, `serverTime` and `syncEpoch`;<br>• **a push mixing a `request` and a future `protoFile` applies the request and returns `rejected`/`unsupported_type` for the other**, with the quota counting only the request; the same push on legacy `/v1/sync` is a 400;<br>• open-signup total-bytes cap returns 507 for open-signup accounts only;<br>• another user's space gives 403 `not_a_member`; an unknown id gives 404; a legacy workspace id through `/v1/spaces` gives 404;<br>• an `rh_` token on `/v1/spaces` gives 401; an `rha_` token on legacy routes gives 401; a `space:` sentinel can never authenticate;<br>• expired access gives `session_expired`;<br>• per-change `clock_skew` at +5 min + 1 ms while the rest applies;<br>• personal quota; breaker 507 with deletes still allowed;<br>• 426 with `MIN_CLIENT_VERSION`; legacy routes never gated;<br>• the share list in a space carries `createdBy`. | |
| **S7 Ops and release** | `internal/jobs` (GC, breaker sampler); `cmd/.../{backup,restore}.go`, `db.Restore`; `docker-compose.example.yml` (fixed-subnet network, `:0.2`), `Caddyfile.example`; full `server/README.md` | GC removes expired rows and keeps live ones (fake clock); a backup opens and matches the row counts, and a push completes while it runs; the `restore` tests (§6 Restore), including the live command and by-hand paths in `qa_server_live`; graceful shutdown finishes an in-flight push (`synctest` or a blocking-store test); the single-instance lock refuses a second `serve` and a `restore`; the populated-DB migration test (§8.6) | Tag `server-v0.2.0` (accounts) |
| **S8 Remove `worker/`** (after `server-v0.2.0` is public and M1-client C1 passes `qa_auth_live`) | **Delete `worker/`.** Move `worker/test/fixtures/curl_parity.json` to `server/internal/curlexport/testdata/curl_parity.json` and `worker/tools/curl-parity-gen` to `tools/curl-parity-gen` (its `reqharbor-core` path becomes `../../crates/core`). Update `crates/core/tests/curl_parity.rs` (fixture path) and `qa_sync_live.rs` (`REQHARBOR_SERVER_URL`, falling back to `REQHARBOR_WORKER_URL`). Delete `qa/run_live_worker_tests.sh`. In `compose.yaml`, remove `worker-node-modules` and port 8787. Update docs: PRODUCT §8 becomes "Sync server API", the API_CONTRACT wording, `docs/plans/accounts/README.md` (link this document), and the root README. Fixture regeneration becomes `docker compose run --rm dev cargo run --manifest-path tools/curl-parity-gen/Cargo.toml --target-dir /tmp/curl-parity-target -- server/internal/curlexport/testdata/curl_parity.json`. | The Rust parity test and the Go parity test read the same moved file | The repo has no Node backend |

#### S4 as built: decisions and deviations

Recorded by the S4 implementation; each point fills a gap in, or narrows, the text above.

- **First account.** Through the pages, the first account always needs the setup code (or a trusted `REQHARBOR_ADMIN_EMAIL`, which only S5 providers and the CLI can supply), in every signup mode; otherwise `setup_required`. The "first user is admin" rule of `accounts.Create` applies only to the CLI. An invite code cannot create the first account, and `GET /auth/authorize` on a server without accounts renders the setup-code page (`code.html`) directly.
- **Admin email and the cap.** `REQHARBOR_ADMIN_EMAIL` bypasses the signup mode only; `REQHARBOR_MAX_USERS` still applies to it unless it creates the first account.
- **Fixed setup code.** `REQHARBOR_SETUP_CODE` must normalise to at least 12 Crockford base32 characters, or startup exits 2; a value that normalised to nothing (such as `----`) could never be entered.
- **Codes.** Setup and invite codes are matched after normalisation (spaces and dashes dropped, upper case, Crockford look-alikes `I`/`L` → `1`, `O` → `0`) and stored as `sha256("code:" + normalised)`. The signup form accepts the invite in an optional `code` field as well as through `POST /auth/code`; both count toward the 10-per-auth-request and 30-per-hour-per-IP limits. In `closed` mode an applied invite does not open signup (`signup_closed`). Public ids of one-time tokens are `XXXX-XXXX`.
- **Logout** answers `204 No Content`.
- **Pair throttle timing.** The fifth consecutive failure from one (email, IP bucket) locks the pair for 1 minute, then 2, 4, … up to 60 (`min(2^(failed−5), 60)` from the fifth failure on, which is what "5 failures throttle (email, A)" in the S4 test list needs).
- **Pair throttle is atomic.** `Throttle.Check` (read transaction) only answers an already locked pair early and computes the proof-of-work difficulty. After the proof, `Throttle.Begin` re-reads the pair row in a write transaction and either answers throttled or counts the attempt as a failure **before** argon2 runs (`failed_count+1`, `last_failed_at`, the `LockFor` lock); a correct password then deletes the row, a wrong one only adds to the in-memory account-wide counter. Concurrent attempts for one pair therefore queue on the single writer, and at most 5 reach argon2 before the lock (test: 20 concurrent wrong passwords over 4 auth requests, known and unknown email). A right password for a disabled account also deletes the pair row.
- **Account-wide proof of work.** The in-memory counter only applies while the email still has `login_throttle` rows. `user unlock` and `user set-password` run in another process and can only delete rows; an emptied row set therefore clears the slow-down too. The proof is the form field `pow=<challenge>.<decimal nonce>`; the `/auth/*` CSP lists both `reset.js` and `pow.js` in `script-src`. The proof-of-work response is not a failure and runs no argon2. `pow.js` computes SHA-256 in plain JS instead of WebCrypto: `crypto.subtle` exists only in secure contexts, so on an http PUBLIC_URL that is not loopback (a LAN address, which §5.9 supports) a WebCrypto proof could never be produced and the slow-down would become a lockout. If the script cannot hash at all, the button stays disabled and the page says so. `qa/server_pow_check.js` runs `pow.js` in node without WebCrypto and checks the proof (part of the live suite). The account-wide counters and expired challenges are swept every minute (`Throttle.Sweep` in `App.Sweepers`), so random emails do not grow memory without bound.
- **Disabled accounts.** A disabled user's access token gives 401 `auth_required`, its refresh gives 400 `invalid_grant`, and the right password on the login page shows "This account is disabled. Ask your server admin." (only after the password checked out, so it reveals nothing to a guesser).
- **Grace window response.** The identical pair is returned with `expires_in` set to the seconds left on that access token. A superseded refresh token of a session that was revoked meanwhile gives 401 `session_revoked`. Reuse detection covers the immediately preceding token only (`prev_refresh_hash`); older ones are unknown (`invalid_grant`). The reuse revocation runs in the same write transaction that detected the reuse, so a rotation committing concurrently cannot turn it into a 500.
- **Session cap** counts sessions that are neither revoked nor past `expires_at`.
- **Version gate.** `httpx.CheckClientVersion` gates `/v1/auth/token` from S4; S6 applies the same helper to `/v1/spaces/*`.
- **Discovery.** `features.accounts` is `true` from S4, and `methods` lists `password` (S5 adds `github`). `setupRequired` is rebuilt when the pages create the first account and, while it is still true, checked against the database on each request, so an account created by the CLI in another process also clears it.
- **GC.** `jobs.GC` (expired auth requests; one-time tokens and sessions dead for 7 days; throttle rows 24 h after their last failure) runs every 10 minutes from S4, because S4 creates those rows; S7 adds its tests for the other tables.
- **CLI.** `user create` takes exactly one of `--password-stdin` and `--reset-link` (the latter sets a random password and prints a 24 h reset link). `user reset-link --ttl` defaults to 24 h and must be positive and at most 168 h. The account commands (`user`, `invite`, `setup-code`) validate the full configuration and refuse a database whose migrations are pending.
- **Live suite.** `qa/run_live_server_tests.sh` keeps the §7.5 server settings, reads the setup code from the server log and passes it to `qa_server_live` as `REQHARBOR_LIVE_SIGNUP_CODE` (its new accounts case signs up the admin through the pages with raw HTTP). It runs `qa_auth_live` with a multi-use invite from `invite create` when `QA_AUTH_LIVE=1`. By default (`QA_AUTH_LIVE=auto`) it runs it only when the server actually serves the S6 routes it needs: an unauthenticated `GET /v1/me` answers 401 (route present), not 404. Until S6 lands it prints `qa_auth_live skipped: server does not serve /v1/me yet (S6)`. `QA_AUTH_LIVE=0` skips it. The condition is the server's routes, not git state, because the client track commits `qa_auth_live.rs` (C1) before S6 serves `/v1/me`.
- **QA follow-ups fixed with S4.** Legacy share create now validates `kind`, `entityId` and the snapshot before it charges the share-create limiter. `server.yml` pushes images (and adds the `sha-` tag) only for `main` and `server-v*` refs, so a `workflow_dispatch` on another branch builds without publishing. `httpx.HandleError` no longer writes a second response after a handler started one, and logs a client cancellation at debug. The `ActiveShares` comment now states what `json_extract` returns (object names come back as JSON text, as in D1). `REQHARBOR_LISTEN` is judged by `IsLoopbackListen` for both the PUBLIC_URL default and the devauth guard, so `localhost:8080` now needs `REQHARBOR_PUBLIC_URL`; `IsLoopbackHost` (which accepts `localhost`) is only for hosts such as the SMTP server.

#### S5 as built: decisions and deviations

Recorded by the S5 implementation (`internal/auth/providers/{providers,github,dev,dev_off}.go`, `internal/auth/external.go`).

- **GitHub flow.** `golang.org/x/oauth2` (v0.38) with `AuthStyleInParams` and PKCE S256. `/auth/start/github?ar=` stores `provider`, `sha256(state)` in `upstream_state_hash` and the verifier in `upstream_verifier`, then answers 302 (`Cache-Control: no-store`). A second start replaces the round trip, so an older state stops working. The callback finds the auth request by the state hash and requires this browser's cookie, an unexpired, unfinished request and the same provider; only then are both upstream columns cleared, so the state works once, and a callback carrying another browser's cookie (or none) gets `request_expired` without burning the victim's state. Profile calls send `User-Agent: reqharbor-server`, `Accept: application/vnd.github+json` and `X-GitHub-Api-Version: 2022-11-28`; responses are capped at 1 MiB, and the whole exchange at 30 s.
- **Email selection.** Only the entry that is both `primary` and `verified` counts; a verified non-primary address does not. The name falls back to the GitHub login; `avatar_url` is kept only when it is https and at most 2048 characters.
- **Order for a new identity.** No qualifying email gives `no_verified_email` (with a "Try again" link to `/auth/start/github?ar=`), then an email that another account uses gives `email_in_use`; both create nothing. Only then the per-IP account-creation limit, the breaker, and `accounts.Create` with the signup rules. A **returning** identity skips all of these (no invite needed); it signs in even without a qualifying email, and then nothing but `last_login_at`, the name and the avatar changes. A trusted incoming email equal to the user's own untrusted `users.email` sets `email_trusted=1`. A disabled account gets the `account_disabled` page ("This account is disabled. Ask your server admin.") and no code.
- **Codes for provider sign-ins.** When `accounts.Create` needs a setup or invite code, the code page carries `continue=<provider>`; after a valid code, `POST /auth/code` answers 303 to `/auth/start/<provider>?ar=` (a second round trip to the provider) instead of the password signup form. `continue` is honoured only for an enabled provider. The setup-code page shows provider buttons only when `REQHARBOR_ADMIN_EMAIL` is set ("Or, if this server's admin email is yours:"); the signup page always shows them. With `REQHARBOR_PASSWORD_AUTH=false` the authorize and signup pages show only the provider buttons.
- **Errors.** `access_denied` consumes the auth request and answers 303 to `redirect_uri?error=access_denied&state=…` with exactly those two parameters. Any other upstream `error` renders `provider_error` (400, never a redirect); a failed token or profile call renders `provider_error` (502) and logs `provider sign-in failed` with a code only (`token_<oauth error>`, `token_request_failed`, `api_status_<n>`, `api_bad_response`, `user_without_id`, `timeout`), never a token, code or secret.
- **Routes.** `/auth/start/{provider}` and `/auth/callback/{provider}` answer the JSON 404 for a provider that is not enabled, before the canonical-origin redirect; enabled ones are GET only.
- **Test overrides.** `config.Config.Test` (`TestOverrides`: GitHub authorize, token and API URLs) is never set by `Parse`; the outbound client comes from `app.Deps.HTTPClient`. `testutil.FakeGitHub` (an `httptest.NewTLSServer`) checks the client secret, the redirect URI and the PKCE verifier against the challenge of each authorize URL.
- **Dev provider.** `GET /auth/start/dev?ar=` renders its form; it posts to `POST /auth/dev` (registered only in a devauth build with `REQHARBOR_DEV_AUTH=true`) with `ar`, `email`, optional `name`, optional `subject` (default: the email; a fixed subject with a new email plays a provider whose email changed) and optional `code` (invite or setup, applied like the signup form's). Signup rules apply as for any provider, so in `invite` mode a new dev account needs `code`; the client track's `qa_auth_live` dev path (`REQHARBOR_DEV_AUTH_PATH`) must post it, and the live script does not set that variable. The loopback check uses `RemoteAddr` only (`127.0.0.1`, `::1` and `::ffff:127.0.0.1` pass); every other peer gets 404 on the start, form and callback routes whatever `X-Forwarded-For`, `X-Real-IP` or `CF-Connecting-IP` say, and the authorize page shows the "Dev sign-in (tests only)" link only to loopback peers. The provider is also disabled when the listener is not loopback, in addition to the startup refusal. Discovery never lists it. Default builds have `providers.DevCompiled == false`, no `/auth/dev` route, and a test that reads the test binary's build info for `-tags devauth`.
- **Live suite.** `qa/run_live_server_tests.sh` creates an invite with `invite create --uses 5 --ttl 1h` and passes it to `qa_server_live` as `REQHARBOR_LIVE_INVITE_CODE`. The new case `live_provider_dev_sign_in_is_unlisted_untrusted_and_follows_signup_rules` checks that discovery lists only `password`, the GitHub routes are 404, the dev form refuses a new account without the invite and accepts it with one (not admin), and a returning dev identity needs no code. It runs after the S4 setup-code case because libtest orders tests by name.

#### S6 as built: decisions and deviations

Recorded by the S6 implementation (`internal/spaces`, `internal/store/sqlite/spaces.go`, and changes in `syncsvc`, `shares`, `jobs/breaker.go` and `accounts`). `server/README.md` § API is the wire reference the client track builds against.

- **Space authorisation** is two reads, not the single joined query of §4: `auth.Service.Authenticate` (the same session rules, `last_used_day` and disabled-user check as every account route), then `SpaceAccess(workspace, user)` joining `workspaces`, `teams`, `users` (the owner's `signup_via`) and `team_members`. The result table of §4 is unchanged. Order on every `/v1/spaces/*` route: route, method, the `MIN_CLIENT_VERSION` gate (426 **before** auth, so a too-old client learns that first), token (401), space (404 `space_not_found` for an unknown id or `team_id IS NULL`, then 403 `not_a_member`), then the body. Role enforcement stays M2; a personal space has only its owner.
- **`updatedBy`** on space pulls is `{"userId","name"}`, `name` being the author's **current** `users.name` (a `LEFT JOIN` at pull time; `null` once the account no longer exists). It is omitted for a version without an author (`updated_by = ''`), which in M1 only legacy rows have; legacy pulls never carry it. A `stale` push keeps the stored author; an applied one records the caller.
- **`clock_skew`** applies to every change on space sync, tombstones included, when `updatedAt > serverNow + 300000` (exactly +5 min passes). It is decided after every 400 check of the change, like `unsupported_type`. Precedence: `unsupported_type`, `clock_skew`, then `invalid_json_value` / `entity_too_large`. A rejected change takes no rev and is excluded from the quota delta. `serverTime` (Unix ms) is on space push responses only, so legacy responses stay byte-identical.
- **Space shares.** `createdBy` is `{"userId","name"}` (`name` `null` for a former member) or `null` for a share without an author. `options` is normalised to `{"includeCredentials":bool,"environmentId":string|null}`: a missing `includeCredentials` is false and unknown keys are dropped, so the stored value is bounded and deterministic. `options` absent or `null` on create stores none; on `PUT` it **keeps** the stored options and an object replaces them (there is no "clear" in M1). Legacy routes ignore an `options` member and never store `created_by` or `options`. The create limit (30 / h) is keyed by the space id on the limiter legacy workspaces use, charged after validation.
- **`/v1/me/sessions`** lists live sessions only (not revoked, refresh not past `idle_expires_at` or `expires_at`), newest first. `lastUsedDay` is Unix ms at 00:00 UTC of that day (the stored day number × 86 400 000), as `docs/types.ts` `SessionInfo` documents. `DELETE /v1/me/sessions/{id}` answers 204; another user's, an unknown or an already revoked id is 404 `not_found`; the caller may revoke its own session.
- **`/v1/me`** `method` is `password` when the account has a password credential, otherwise its identity's provider id (`github`, `dev` in test builds).
- **`quotaBytes`** is `null` when the quota setting is `0` (unlimited), in `/v1/me/spaces` and `GET /v1/spaces/{id}`.
- **`REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES`** is a third breaker sum, `OpenSignupTotalBytes` (entities plus active shares of the personal spaces whose owner has `signup_via = 'open'`), sampled with the others every 10 s. When reached, growing writes to those spaces (push and share create or grow) return 507 `server_full`; `accounts.Create` refuses a code-less open signup with `ErrUserLimit` (`server_user_limit`, password and provider alike). Invite, setup, admin-email and CLI creations are unaffected, as are the other accounts' writes.
- **`REQHARBOR_RATE_SYNC_PER_MIN`** counts GET and POST on `/v1/spaces/{id}/sync` together, per session id.
- **Live suite.** `qa_server_live` gains `live_space_personal_sync_me_and_shares` (sorted after the S5 case; it signs two accounts up through the dev form with the S5 invite, which therefore keeps 5 uses). With the S6 routes served, the script's `QA_AUTH_LIVE=auto` now runs the client track's `qa_auth_live`.

#### S7 as built: decisions and deviations

Recorded by the S7 implementation (`internal/db/{db,restore}.go`, `internal/jobs/{gc,breaker,maintain}.go`, `cmd/.../{backup,restore}.go`, `app.Run`). Not tagged yet: `server-v0.2.0` is the owner's step.

- **Single-instance lock.** `db.AcquireLock(dataDir)` takes the `flock` without opening a database; `serve` takes it through `db.Open`, `restore` on its own (there may be no database). Both return `db.ErrLocked` while another holds it; `restore` then says "stop it first". The account commands and `backup` take no lock (short transactions next to a running `serve`).
- **`backup`.** The default output is `backups/manual-YYYYMMDD-HHMMSS.db` (UTC), not a fixed `manual.db`, so repeated runs never collide; an existing target is refused. It runs `VACUUM INTO` on one plain connection of its own process (no checkpoint, so `serve`'s writer is never held), into a file inside a fresh `0700` directory (`.backup-*`) next to the target, then `chmod 0600` and `os.Link` to the final name, which fails instead of overwriting a file that appeared meanwhile; the temporary directory is always removed. So an interrupted backup never leaves a partial file under the final name, and nobody else can read the snapshot while it is written. It then re-opens the file read-only, runs `PRAGMA quick_check`, and prints the path on the first line and a summary (bytes, schema version, "restore it only with `restore`") on the second. The automatic pre-migration backup uses the same write-then-rename path.
- **`restore`.** As §6, plus:
  - `--rev-base` may also follow `FILE`; it must be above every restored head and every recorded pre-restore head, or the command refuses (no silent clamp).
  - A workspace that the backup has but the current database does not (it was deleted after the backup) has no recorded head either, so it gets the no-database default `max(restored revs) + 2^32`, not `+1`. `Unrecorded` in the summary counts such workspaces.
  - Any shift that would pass 2^53 − 1 is refused before writing.
  - The backup must be a regular file, not the live database itself; it must pass `PRAGMA integrity_check`, have a `schema_migrations` version ≥ 1 and a `workspaces` table. A newer schema is refused with "use a newer image".
  - Step 1 opens the current database on one plain connection **without the downgrade guard** (it reads only `workspaces`, `entities` and `server_state`), so an older binary can restore over a database a newer one already migrated: the README's rollback to a `pre-migrate-…` backup. It then runs `PRAGMA wal_checkpoint(TRUNCATE)` and checks its row: unless `busy = 0` and `log = checkpointed`, the restore is refused ("another process has … open (a backup, a reqharbor-server command, Litestream or another sidecar); stop it first"), because another process pins WAL frames that would not reach the kept file. `db.Checkpoint` (graceful shutdown) checks the same row and returns `ErrCheckpointBusy`, which `serve` logs.
  - The swap renames the live database to `reqharbor.db.pre-restore-<UTC ts>` (`-2`, `-3`, … when two restores share a second) and its `-wal`, if any, to `<that>-wal`, so a frame written after the checkpoint is never deleted; only the `-shm` is dropped. Failure at any step before the swap leaves the live database untouched and removes the temp copy.
  - The summary, and the README, say that a restore undoes every revocation made after the backup (signed-out sessions, disabled accounts, reset passwords, revoked invites) and list the commands to repeat.
  - `restored_at` is stored as Unix ms. If the backup carries a different `instance_id` than the current database, the current one is kept and the summary says so; a pre-0003 backup gets the current one.
- **Graceful shutdown.** After `http.Server.Shutdown` times out, `srv.Close()` cuts the remaining connections and `serve` waits up to 10 s more for the writer pool to go idle (`db.WaitWriterIdle`), because a cut-off handler may still be inside a write transaction; a request's own write is normally rolled back by its cancelled context, and the single writer connection already queues the checkpoint behind it, but the wait makes the bound explicit and logs when it expires. Then the jobs are cancelled and awaited, then the WAL checkpoint, then the pools close and the lock is released. A shutdown during startup (migrations) ends the startup goroutine without marking the migration failed. `app.RunOptions.WrapStore` lets tests put a blocking store under `Run`; the test checks that a push held mid-transaction completes after SIGTERM-equivalent cancellation, that new connections are refused meanwhile, that the lock is free afterwards, and that a request stuck past `REQHARBOR_SHUTDOWN_TIMEOUT` is cut.
- **Jobs.** `jobs.RunMaintenance` runs `db.Maintain` (`PRAGMA optimize`, `PRAGMA incremental_vacuum`) when it is due and records each completed run in `server_state` (`maintenance_at`, Unix ms): 24 h after the last recorded run (at once when that is past, so a server restarted more often than daily still runs it), or 1 h after ready when it never ran. A failed run is retried after 1 h and not recorded. GC and the breaker sampler are unchanged from S4/S6; S7 adds their tests (GC over every table it touches with a fake clock, the breaker sample table, recovery after a sampler error, and transition logging once).
- **Populated-DB migration test** lives in `internal/app/migration_populated_test.go`, not `internal/db/migrate_test.go`, because it drives the HTTP routes through the harness (`testutil.WithDataDir`). The Go server cannot serve a v2 database, so "the same JSON before and after" is checked against the Worker's wire contract computed directly from the v2 rows, after 0003 and after 0004 (`syncEpoch` on pulls being the only addition). The seed (`internal/db/testdata/legacy_seed.sql`) has 3 workspaces, 1,000 entities and 9 shares, 4 of them revoked.
- **File modes.** The database, its `-wal` and `-shm`, backups and pre-restore files are `0600`, and the data directory `0700`: `db.Open` creates the database file as `0600` (and chmods an existing one and its sidecars; SQLite gives the sidecars the database's mode), the binary sets `umask 077` at start on unix, and the image creates `/data` with `mkdir -m 0700` and copies its parent directory (`COPY /out/root/ /`), because copying the directory itself would only copy its contents into a `0755` `/data`.
- **Restore swap rollback.** If moving the current database back after a failed swap also fails, the error names the pre-restore path it now has and tells the admin to rename it back before starting the server.
- **Compose example.** Adds `stop_grace_period: 60s` to the `reqharbor` service: the worst-case shutdown is about 45 s (20 s budget, 10 s writer-idle wait, 10 s checkpoint, 5 s close), and Compose's default is 10 s; the README says to keep it about 40 s above `REQHARBOR_SHUTDOWN_TIMEOUT` and a commented `secrets:` block for the GitHub secret file. `server.yml` validates the file with `docker compose -f docker-compose.example.yml --profile tls config -q`. `.dockerignore` keeps the README, the compose example and the Caddyfile out of the image context.
- **Live suite.** The server start, stop and readiness helpers moved to `qa/live_server_lib.sh`; `qa/live_server_ctl.sh` (`backup`, `restore FILE`, `replace FILE`) is passed to `qa_server_live` as `REQHARBOR_LIVE_SERVER_CTL`. The two restore cases (`live_restore_a_…` with the command, `live_restore_b_…` by hand) each take their own backup inside the case, so the accounts made by earlier cases survive, and assert: the epoch rotates (command) or stays (by hand), the restored server lacks B, device 1's next sync reports `restored` and re-pushes B without conflicts, and device 2 ends with A and B.

### M1-client (after M0)

| Phase | Files | Tests |
|---|---|---|
| **C1 Discovery and sign-in** (04 M1c plus §9) | `crates/core/src/{auth.rs, secrets.rs, server_info.rs (new: normalise + discover)}`; `src-tauri/src/{secret_store.rs, auth_rt.rs, commands.rs (server_discover, auth_sign_in_start(serverUrl, method), …), main.rs (opener plugin), state.rs}`; UI `SignInFlow` (server field, http warning, method buttons, new error states), `AccountSection`, `mock/account.ts`; types in both files | 04 M1c tests, plus §9 core tests and `qa_auth_live.rs`; Playwright for the server-entry states |
| **C2 Personal sync and bindings** (04 M1d plus §9) | As 04 M1d, plus `server_origin`/`server_instance_id` on `SpaceBinding`, the foreign-server pause, `sync_epoch` on space bindings, and push filtering by `sync.types` | 04 M1d tests, plus: a binding from another origin or instance never pushes; restamp versus conflict copy; `legacy_disabled` copy; restore detection on a space binding; an unlisted type stays local-only with the notice |

The M0 phase (04 §10, client forward compatibility) gains two items from this document, because they must ship before any user meets a Go server: **restore detection** (§9) and **push filtering by `sync.types`** for legacy token sync (§9).

### M2 outline (server first, then client)

- **`0005_teams.sql`:**
  - `team_invites`: as 04 §4, plus the CHECK `role <> 'admin' OR max_uses = 1`.
  - `team_departures`: as 04 §4.
  - `signup_tickets(ticket_hash PK, invite_id, expires_at, used_at)`: lets a not-yet-registered invitee sign up while `REQHARBOR_SIGNUP=invite`. The app calls `POST /v1/invites/ticket {token}`, gets a 10-minute ticket and adds `&ticket=` to the authorize URL, so the invite token itself never appears in a URL (PRIV-4).
- **Endpoints:** `/v1/teams*`, `/v1/invites/{preview,accept,ticket}` and `/i`, as in 04 §5.3–§5.4, executed in Go write transactions. The guarded single-statement SQL from 04 can stay as it is, since SQLite runs it unchanged.
- **QA fixes built in:**
  - Removal or demotion below admin revokes `team_invites WHERE created_by = target` in the same transaction. The "revoke all links" checkbox defaults on whenever any active link exists.
  - The owned-team count uses `personal = 0`, and transfer checks the receiver's count (409 `limit_teams`).
  - The spec says plainly that editors can delete content and that admin-only applies to the project container.
  - Client: project tombstones with dirty descendants freeze them, show the banner, and never push orphans.
- **Role enforcement in `syncsvc` (actor role):**
  - A viewer push gets 403 `forbidden_role` for the whole push.
  - A `project` tombstone below admin gets a per-change `forbidden_role`.
  - A team `environment` with `projectId: null` gets `global_env_not_allowed`.
  - `stripDeviceLocalPaths` runs before sizing, in team spaces only.
- **Quotas:** team quota plus owner aggregate. The team-space version gate is fixed at the M0 release version.
- **Teams are off by default** (`REQHARBOR_TEAMS=false`); `features.teams` in `/v1/server` reports it.
- **Tests:** 04 §12 M2a and the QA test plan "Teams and roles", "Invites" and "IDOR" lists, ported to Go table tests.

#### M2 as built: decisions and deviations

Recorded by the M2 server implementation (`db/migrations/0005_teams.sql`, `internal/teams`, `internal/roles`, `internal/store/{teams.go,sqlite/teams.go}`, and changes in `syncsvc`, `spaces`, `shares`, `accounts`, `auth`, `discovery` and `jobs`). `server/README.md` § API (from "Teams: errors and conventions") is the wire reference the client track builds against.

- **Schema.** `0005_teams.sql` has `team_invites`, `team_departures` and `signup_tickets` as 04 §4 and the outline say, plus `auth_requests.ticket_hash` (the hash of the ticket an auth request was started with) and indexes for revoking a creator's links and for GC. The admin-link CHECK is `role <> 'admin' OR max_uses IS 1`: with `= 1` an admin link with `max_uses NULL` (unlimited) passes, because a CHECK that evaluates to NULL passes in SQLite. `team_departures` replaces a same-millisecond duplicate (`INSERT OR REPLACE`).
- **Atomicity.** 04's guarded single-statement SQL is not used. Every team mutation runs in one write transaction that re-reads the team and the caller's membership first; the single serialised writer makes that equivalent and the rules readable Go (`teams.MayChangeRole`, `teams.MayRemove`). A transfer racing a removal therefore either runs first (the removal is then 403: the caller is no longer owner) or second (the transfer is 404 `member_not_found`); tested 15 rounds in parallel.
- **Error codes.** Additions to 04 §5.7: 403 `teams_disabled`; 404 `team_not_found` for an unknown team id and for every personal team id (hidden, as 04 §5.3 says); 403 `not_a_member` when the caller is not in an existing team (as for spaces); **404 `member_not_found`** for a `{uid}` (or transfer `userId`) that is not a member, instead of 04 §5.4's 404 `not_a_member`, so the two meanings never share a code; 404 `invite_not_found` for another team's `{iid}`.
- **Role change** (`PATCH …/members/{uid}`): the rule first (403), then an unchanged role is a 200 no-op, then `last_owner`, then `transfer_required`. `last_owner` comes first so a sole owner gets "A team needs at least one owner" rather than "transfer first". Demotion from admin or owner to editor or viewer revokes the target's links in the same transaction (QA fix). Owner→admin keeps them.
- **Remove** (`DELETE …/members/{uid}`): the body is optional (no body = `revokeInvites:false`); removing yourself is 403 (use leave); the response is `200 {"userId","revokedInvites"}`. The removed member's own links are always revoked (QA fix); `revokeInvites:true` revokes the rest.
- **Leave** also revokes the leaver's own links (not in the QA list; it closes the same alt-account hole for an admin who leaves before being removed). It records a `left` departure, which never blocks rejoining.
- **Delete team** gives every member, the deleter included, a `deleted` departure, so the deleter's other devices learn it too. Co-owners cannot delete; only the billing owner.
- **Transfer** answers `200 Team` from the caller's side (`role: admin`). The owned-team check (409 `limit_teams`) and, when `REQHARBOR_QUOTA_OWNER_BYTES > 0`, the aggregate check (413 `quota_exceeded` with the shortfall) both run inside the transaction.
- **Rename** renames the team space (`workspaces.name`) too, so `GET /v1/spaces/{id}` shows the team name.
- **Invites.** Tokens are `rhi_` + base64url(32 bytes) (256 bits; the spec asks for at least 128), signup tickets `rht_` + the same. Defaults: editor, 1 use, 7 days; `expiresInDays` 1–30 (30 is the bound the 31-day departure retention relies on); `maxUses` 1–10000 or `null`. The daily limit (30 per team) is a database count of the last 24 hours, so it survives restarts. `maxUses`/`expiresInDays` that are not integers are 400.
- **Accept.** A member of the link's team gets `alreadyMember:true` with the current role even when the link is no longer usable (only an unknown or malformed token is 410 first). The removal block is `d.at >= i.created_at`, as 04.
- **Signup tickets.** `POST /v1/invites/ticket` needs no auth (the invitee has no account) and is limited per IP like preview (`REQHARBOR_RATE_AUTH_PER_MIN`). `/auth/authorize` takes an optional `ticket` (a malformed one refuses the request like any bad parameter); only its hash is stored. In `accounts.Create` a usable ticket counts as an invite **in `invite` mode only** (`signup_via='invite'`), consumed in the same transaction; an invite code takes precedence; an unusable ticket is ignored. QA pass 1: in `open` mode the ticket is ignored and not spent, so such accounts stay open signups inside `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` and are refused while it is full (otherwise anyone could create a team and an unlimited link and mint tickets to escape the cap); `closed` stays closed. A link with `max_uses` opens at most that many signups: minting (inside the ticket transaction) and spending a ticket both require `uses + tickets already spent on signups < max_uses`. Accepting the link after a ticket signup only needs a use, so one person spends a ticket slot and a use. Unlimited links have no ticket cap; the per-IP limit, the 30 / h signup limit and `REQHARBOR_MAX_USERS` bound them.
- **Teams switch.** `REQHARBOR_TEAMS=false` answers 403 `teams_disabled` on `/v1/teams*` and `/v1/invites/*` before auth. Existing team spaces keep syncing and stay in `/v1/me/spaces` (turning teams off never strands data). `GET /i` is always served; its preview call then fails and the page shows the invalid-link text.
- **Team-space version gate.** `spaces.TeamSpaceMinClientVersion = "0.1.0"`, applied to every `/v1/spaces/{id}*` call to a team space after the space checks (404/403) and before the role. The M0 client still reports 0.1.0, and pre-M0 clients send no header, so today it means "the header is required"; raise the constant to the first release that ships M0. Raw HTTP clients (tests) must send `X-ReqHarbor-Version`.
- **Role enforcement in sync.** A viewer's push is 403 before the body is read. Push re-reads the membership and role inside its write transaction (`syncsvc.Actor.Recheck` returns a `syncsvc.Policy`), so a removal or demotion that commits between authorisation and the write takes effect (403 `not_a_member` / `forbidden_role`), and the per-change project-tombstone rule is decided there from the current role (QA pass 1: a demotion from admin to editor, or a promotion, between the two now counts). Per-change precedence: `unsupported_type`, `clock_skew`, `forbidden_role`, `global_env_not_allowed`, then `invalid_json_value` / `entity_too_large`. The Global environment is an `environment` upsert whose `data.projectId` is not a non-empty string; environment tombstones are not checked (they carry no data). `syncsvc.StripDeviceLocalPaths` blanks `body.binaryPath` and the `value` of `body.form[]` entries with `kind: "file"` to `""` (team spaces, requests only), before canonicalisation and sizing.
- **Shares in team spaces.** Create needs editor (viewer 403 before the body). Update and revoke: own share for editors, any for admins; order share id (404), role (403), revoked (410), body. `shares.Scope.Guard` repeats the role check inside each create, update and revoke transaction (QA pass 1), so a removal or demotion that lands meanwhile refuses the write.
- **Names.** Team names refuse control and format characters (`unicode.Cf`, for example the bidi override U+202E and zero-width characters). Display names are validated by the sign-in pages, which do not share this check.
- **Restore** (`reqharbor-server restore`) also undoes team changes made after the backup: removals, demotions and revoked team invite links. Its summary and the README tell the admin to have team admins repeat them.
- **Quotas.** `syncsvc.Limits.CheckGrowth` is shared by push and shares: the space quota (personal or team), then the billing owner's aggregate (`REQHARBOR_QUOTA_OWNER_BYTES`, `0` = off; personal space plus owned team spaces, entities plus active share snapshots), then the breaker. The aggregate applies to personal spaces too; its 413 message names the owner.
- **`/v1/me/spaces`** gains `memberCount` and `ownerUserId` per space and a top-level `owner: {bytesUsed, quotaBytes}` (04 §5.3's aggregate). `departed[]` is the latest departure per team the user is not a member of now. `GET /v1/teams` also returns `ownedCount`, `limits` (null = no cap) and `owner`.
- **Limits.** `REQHARBOR_MAX_OWNED_TEAMS=0` and `REQHARBOR_MAX_TEAM_INVITES=0` mean no cap, like `REQHARBOR_MAX_USERS=0`.
- **GC.** `jobs.GC` also deletes invites 30 days after they expired or were revoked, departures after 31 days and expired signup tickets (`WriteTx.GCTeams`).
- **Not built.** 04 §5.3's `oldestClientVersion` on `GET /v1/spaces/{id}` and the `client_versions` table (QA: scope creep; only the header gate remains). No CLI commands for teams.
- **Live suite.** `qa/live_server_lib.sh` runs the server with `REQHARBOR_TEAMS=true`; the script's invite has 10 uses; `qa_server_live` gains `live_teams_invites_ticket_roles_and_team_sync` (raw HTTP: create, links, `/i` CSP, preview, accept, a signup through a ticket without a server invite, viewer/editor/owner pushes, Global env, blanked `binaryPath` parsed by the real `PullPage`, the 426 gate, removal, delete).

### M3

Moved here from the first draft's M1 so that M1 stays "auth + personal spaces" and the client sign-in work (C1, C2) starts sooner. Any of S9–S11 may be pulled forward as a server-only **M1.5** once C2 passes `qa_auth_live`; the client needs no change for them, because it renders whatever `auth.methods` and `passwordReset` discovery reports.

| Phase | Files | Tests |
|---|---|---|
| **S9 SMTP** | `internal/mail`; `web/templates/verify.html`; `POST /auth/password/verify`; `/auth/forgot` by email | Fake SMTP server (`net.Listen` plus a minimal responder in `testutil`): signup code, 5 attempts, 3 sends per auth request; signup with an existing address shows "Check your email" and sends the "you already have an account" email (method named only there), with no code; reset by email for a trusted and an **untrusted** account, which becomes trusted; provider-only reset sends the method email and issues no token; the forgot page is identical in every case; the per-recipient cap (5 / h across purposes) and the global cap suppress sends without changing the page; a password signup confirmed by code with the admin email becomes admin only while no admin exists. |
| **S10 Google and OIDC** | `providers/{google,oidc}.go` | Google: `iss`/`aud`/`nonce`/`exp`/`email_verified`, `prompt=select_account`. OIDC: discovery retry and late enablement; issuer mismatch; **an http `token_endpoint`, `authorization_endpoint` or `userinfo_endpoint` (non-loopback) disables the provider**, and a redirect from the https token endpoint to http is refused; **`aud` as a string, as an array containing `client_id`, as an array without it (rejected), several audiences without `azp` (rejected) or with a wrong `azp` (rejected)**; userinfo fallback with `sub` mismatch; `REQUIRE_VERIFIED_EMAIL=false`: account created untrusted, **an email equal to `ADMIN_EMAIL` does not become admin**, and a returning user's changed email does not update `users.email`. |
| **S11 Scheduled backups** | `internal/jobs` backup ticker | Scheduled `VACUUM INTO` on the backup connection, pruning to `BACKUP_KEEP`, a push completing during a scheduled backup. |

Then:
- `DELETE /v1/me` and `user delete` (04 §5.5).
- `/v1/admin/*` (users list, disable, reset link, invites) for an in-app admin view, guarded by `users.is_admin`.
- Email change for password accounts.
- Docs and mock parity.

---

## 11. Forward compatibility (WebSocket, Socket.IO, gRPC, GraphQL)

- **The server treats entity `data` as opaque JSON.** New request kinds (`kind: "graphql" | "grpc" | "websocket" | "socketio"`), with their sub-objects and `minClientVersion`, pass through push, pull and canonicalisation untouched. Canonicalisation (§1.4) only re-escapes strings, dedupes keys and refuses values the client could not parse; it never drops fields. A new kind therefore needs **no server change** to sync; `sync.requestKinds` only tells the client which kinds the share page previews.
- **Version skew is the normal case.** With self-hosted servers, admins upgrade the server on their own schedule while users upgrade the desktop app, so "deploy the server first" cannot be relied on. The server's `SYNC_TYPES` is still an allowlist (`project, collection, folder, request, environment` today), but a new type is handled at both ends without breaking older peers:
  - **Server:** `/v1/server` publishes the allowlist as `sync.types`. On `/v1/spaces/*/sync`, an unknown type is a per-change `rejected` / `unsupported_type`, and the rest of the push applies. Legacy `/v1/sync` keeps its whole-request 400 for byte-compatibility; newer clients never send it an unlisted type because they filter first.
  - **Client:** pushes only listed types; anything else stays dirty and local-only with a notice until the server lists it (§9). Pull already skips unknown types (AC-FWD-5).
  - Adding a type (for example `protoFile` or `socketioEventSet`) is then a server release that extends the allowlist and a client release that creates the type, in **either** order.
- **Large artefacts** such as proto bundles over 512 KiB will need a content-addressed `blobs` table, or files under `/data/blobs`, behind the `store` interface. That is not designed here; the 512 KiB limit stays.
- **The share page** renders any non-`http` kind as the import card (AC-FWD-6). Per-kind previews (GraphQL query view, Socket.IO event list) are later template additions.
- **Live updates** are possible later as server-sent events (`GET /v1/spaces/{id}/events`) on the same mux, with a per-route `http.ResponseController` write deadline. Nothing in M1 blocks it.

---

## 12. Open items for the owner

Each item has a recommended default, which the implementation uses unless the owner changes it.

1. **Default signup mode `invite`** for safety on public hosts. Alternative: `open`.
2. **`REQHARBOR_LEGACY_WORKSPACES=off` by default.** With `open` as the default, every public instance of the image would offer free anonymous storage (50 MB per workspace, 10 per hour per IP, a window that resets on restart) and public share pages under its admin's domain. Nobody depends on `open`: the Worker was never deployed, and there is no default server URL. The promise therefore changes from "today's client works unchanged" to "today's client works unchanged **against servers whose admin enabled token sync**" (`existing` or `open`). An admin who enables `open` gets the abuse warning at every start and the `LEGACY_TOTAL_BYTES` cap. Gating legacy create with an admin-issued code was considered and rejected: today's client cannot send one, so it would not keep even that narrower promise. Alternative: `open` by default on a loopback or private listener only.
3. **Quota defaults** for self-hosting: personal 100 MiB, team 250 MiB, owner aggregate off, 10 owned teams. These differ from the D1-driven 25/25/100 MB and 3 teams in product §11.
4. **Server release tags** use `server-vX.Y.Z`, independent of the desktop's `vX.Y.Z`. Image tags follow §7.2: no major-only tag while the major is 0.
5. **Global caps:** `REQHARBOR_MAX_USERS=500`, `REQHARBOR_LEGACY_TOTAL_BYTES=1 GiB`, `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES=5 GiB`.
6. **M1 scope:** password, CLI reset links, invites, setup codes and GitHub; Google, OIDC, SMTP and scheduled backups in M3, or an M1.5 once C2 passes `qa_auth_live` (Q19).
7. **04 follow-up:** 04 §2.2 step 5 and §5.3 should adopt this document's RFC 6749 token-endpoint names, including `client_id` (Q10).

Files read for this design (none modified): `worker/src/*.ts`, `worker/migrations/*.sql`, `worker/test/*` (including `fixtures/curl_parity.json`), `worker/README.md`, `worker/package.json`, `worker/vitest.config.ts`, `worker/wrangler.toml`, `worker/tools/curl-parity-gen/Cargo.toml`, `docs/API_CONTRACT.md`, `docs/PRODUCT.md` §8, `docs/plans/accounts/{README,00,02,03,04,05}*.md`, `crates/core/src/{sync,share}.rs`, `crates/core/tests/{curl_parity,qa_sync_live}.rs`, `qa/run_live_worker_tests.sh`, `qa/worker_smoke.sh`, `compose.yaml`, `docker/dev.Dockerfile`, `.github/workflows/desktop-build.yml`, `Cargo.toml`, `.gitignore`.

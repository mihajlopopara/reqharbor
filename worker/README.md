# ReqHarbor Worker

The optional Cloudflare Worker behind ReqHarbor's sync and read-only share links (PRODUCT.md §8).
It is TypeScript with no framework, uses D1 for storage, and the bundle is about 17 KB gzipped.

## Run everything in Docker

All tooling runs inside the repo's dev container (Node 22.21.1), not on the host. Run these from
the repo root:

```sh
docker compose run --rm -w /app/worker dev npm install
docker compose run --rm -w /app/worker dev npm run typecheck
docker compose run --rm -w /app/worker dev npm test
docker compose run --rm -w /app/worker dev npm run build      # wrangler deploy --dry-run --outdir dist
docker compose run --rm --service-ports -w /app/worker dev npm run dev
```

`worker/node_modules` is a named Docker volume holding the Linux `workerd` binaries. If a host
`node_modules` folder exists from an earlier host install, delete it; the container never uses it.

Toolchain pins (exact versions): wrangler 4.168.0, `@cloudflare/vitest-plugin` 1.9.2 (the
maintained successor to `@cloudflare/vitest-pool-workers`), vitest 5.1.2 and TypeScript 7.0.2.
Node 22.12 or later is required. `compatibility_date` is 2026-11-20, which the pinned `workerd`
(1.20261123.1) supports. After changing `wrangler.toml`, regenerate `worker-configuration.d.ts`
with `npm run cf-typegen`.

## Local development

`npm run dev` applies the D1 migrations to the local database in `.wrangler/` and then starts
`wrangler dev` on `0.0.0.0:8787`. The compose file publishes that port, so the desktop app can
point its Worker URL at `http://localhost:8787`. Nothing touches Cloudflare: there is no login and
no remote database.

To create a workspace and token (PRODUCT.md's `POST /v1/workspaces`; open, rate-limited to
10 per hour per IP):

```sh
curl -s -X POST localhost:8787/v1/workspaces -H 'Content-Type: application/json' -d '{"name":"Local dev"}'
# {"workspaceId":"01J…","token":"rh_…"}   <- the token is shown only once
curl -s localhost:8787/v1/workspace -H 'Authorization: Bearer rh_…'
```

## API

Every request and response is JSON. Errors always use this shape:
`{"error":{"code","message"}}` (400 validation_failed / invalid_json / unsupported_type /
duplicate_change, 401 unauthorized, 404, 405, 410 gone, 413 payload_too_large / snapshot_too_large /
too_many_changes / quota_exceeded, 415, 429 rate_limited, 500 internal_error).

| Method & path | Auth | Purpose |
|---|---|---|
| `POST /v1/workspaces` `{name}` | none, 10/h per IPv4 address or IPv6 /64 | → 201 `{workspaceId, token}` |
| `GET /v1/workspace` | bearer | `{workspaceId, name, createdAt, entityCount, bytesUsed, quotaBytes}` |
| `POST /v1/workspace/token/rotate` | bearer | → `{token}`; atomic, the old token stops working immediately |
| `GET /v1/sync?since=&limit=` | bearer | `{changes, cursor, hasMore}`, ordered by `rev`, limit 1–500 (default 500) |
| `POST /v1/sync` `{changes}` (≤ 500, body ≤ 5 MB) | bearer | `{results, cursor}`, one result per change, in push order |
| `POST /v1/shares` `{kind, entityId, snapshot}` | bearer | → 201 `{shareId, url, createdAt, updatedAt}` |
| `GET /v1/shares` | bearer | `{shares:[{shareId, url, kind, entityId, name, createdAt, updatedAt}]}` (active only) |
| `PUT /v1/shares/:id` `{snapshot}` | bearer | Replaces the snapshot behind the same URL (410 if revoked) |
| `DELETE /v1/shares/:id` | bearer | Revokes the share (idempotent) and discards its snapshot |
| `GET /s/:id` | public | Read-only HTML page |
| `GET /s/:id.json` | public | `{shareId, kind, createdAt, updatedAt, snapshot}`; CORS `*` |

**Auth.** Tokens are `rh_` + base64url(32 random bytes). Only `SHA-256(token)` is stored, and
the lookup is followed by a constant-time compare. Rotation is a single compare-and-swap on the
hash of the token that made the call (contract C4): the old token fails from that statement on, and
of several concurrent rotations with the same token exactly one succeeds (the others get 401).
Clients must serialise rotation with their own in-flight sync.

**Sync changes.** A pushed change is
`{type, id, updatedAt, deleted, data, deviceId?}`. `deviceId` is the client's stable ULID (contract
C3); it is optional, and a missing or `null` value is treated as `""` (old clients). Pulled changes
are `{type, id, updatedAt, deleted, deviceId, data, rev}`.

**Last-write-wins (contract C3).** The incoming change wins when its `updatedAt` is greater, or
when `updatedAt` is equal and its `deviceId` is greater than or equal to the stored one (plain
byte-wise string comparison). Clients apply the same rule, so a same-millisecond edit on two devices
converges instead of diverging. The decision runs inside a single transactional D1 batch. Each
accepted change takes the next per-workspace `rev`.

**Push results.** There is one result per change, in push order:
- `{type, id, status: "applied", rev}`: the change won and is stored.
- `{type, id, status: "stale", rev}`: the server copy won. That copy is given a fresh `rev`, so the
  next pull re-delivers it even if the client's cursor was already past it.
- `{type, id, status: "rejected", reason: "entity_too_large"}`: the change has no `rev` and nothing
  was stored (contract C2). It is returned when the serialized `data` exceeds 512 KB, measured after
  secret stripping. The rest of the push still applies. Clients should not re-send a rejected
  change; they should mark it as skipped locally instead.

`cursor` is the workspace's head after the push. Keep pulling from your own pull cursor, because
other devices' changes may sit below that head.

**Pushing in batches (contract C1).** Clients split a push by count (≤ 500 changes) and by size
(≤ 4 MB of serialized request body). The Worker answers a body over 5 MB with a whole-request
413 `payload_too_large`, and over 500 changes with 413 `too_many_changes`. Both reject the entire
request.

**Sync rules.** Tombstones are `deleted: true, data: null`. The server never stores secret
values: a secret environment variable keeps its key and flags, and its `value` is blanked. Clients
must keep their local secret values when they pull. Only `project`, `collection`, `folder`,
`request` and `environment` are accepted.

**Storage quota (QA-R1-024).** Each workspace may store 50 MB (52,428,800 bytes), counting entity
`data` plus the snapshots of active shares. A push, share create or share update that would grow
usage past the quota gets 413 `quota_exceeded`, and nothing in that request is written. Writes
that do not grow usage (deletes, smaller replacements, revokes) are always accepted, so a full
workspace can clean up. `GET /v1/workspace` reports `bytesUsed` and `quotaBytes`. The check runs
before the write, so concurrent requests can overshoot by at most one request body.

**Snapshot format** (built and sanitized by the client):
`{schemaVersion: 1, collection?: {…Collection, folders: Folder[], requests: Request[]}, request?: Request, environment?: {name, variables} | null}`.
Entities follow the PRODUCT.md §3 model. The server also removes secret variables from the
attached environment. Credential placeholders ("Include credentials" unchecked) are the client's
job, because the server can't know that choice.

**Limits.** Request body 5 MB (413 `payload_too_large`); entity `data` 512 KB (per-change
`rejected`, see above); snapshot 1 MB (413 `snapshot_too_large`); 50 MB stored per workspace
(413 `quota_exceeded`).

**Headers.** Every response sets `nosniff`, `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`
and HSTS, and never sets cookies. API responses add `Cache-Control: no-store` and a deny-all CSP.
`/v1/*` sends no CORS headers, since the Rust core makes those calls. Share pages send
`X-Robots-Tag: noindex`, `Cache-Control: public, max-age=60` and a strict CSP: `default-src 'none'`,
with only the page's own inline style and script allowed by SHA-256 hash. All user content is
HTML-escaped, and nothing external is loaded.

**cURL on the share page.** `src/curl.ts` is a line-by-line port of the app's exporter
(`crates/core/src/curl/export.rs` with `prepare.rs` and `urlx.rs`), and `src/variables.ts` ports
`vars.rs`. The output matches the app byte for byte: it uses the same line format, rebuilds the URL
from `params`, adds `--globoff`, writes HEAD as `--head`, and handles `--form-string`, quoted form
paths and percent-encoded Form-encode keys the same way. GraphQL variables are re-serialized the way
serde_json does it (`src/serde-json.ts`). A request the core refuses to export (invalid method, empty
URL, bad GraphQL variables) shows that error on the page instead of a Copy button. When an
environment is attached, the page offers both a resolved and a "Keep {{variables}}" version.

The parity is enforced by shared fixtures in `test/fixtures/curl_parity.json`, run by
`test/curl_parity.test.ts`. Their `expected` values come from the real Rust exporter. Regenerate
them after any exporter change, on either side:

```sh
docker compose run --rm -w /app/worker dev npm run parity:gen   # builds tools/curl-parity-gen against crates/core
```

The Rust side can load the same JSON file in a test to keep both directions honest.

## Deploying later

1. `npx wrangler login`
2. `npx wrangler d1 create reqharbor`, then paste the printed id over the placeholder
   `database_id` in `wrangler.toml`.
3. `npm run db:migrate:remote`
4. Optional: set `PUBLIC_BASE_URL` (for example `https://share.example.com`) as a var if share
   URLs should use a custom domain instead of the request's origin.
5. `npx wrangler deploy`

Run steps 1–3 and 5 inside the container as well, or on any machine with Node 22.12+.
`rate_limits` holds one row per hashed client bucket (an IPv4 address or an IPv6 /64). If it grows
large, add a cron job later to prune old windows. Migrations are additive: `0001_init.sql`, then
`0002_device_id_and_quota.sql`, which adds `entities.device_id` and the `size` columns used by the
quota.

## Layout

`src/index.ts` router and error envelope · `http.ts` limits, JSON body reader, security headers ·
`workspaces.ts` auth, tokens, rate limit · `sync.ts` push/pull · `quota.ts` storage quota ·
`shares.ts` share API and public views · `snapshot.ts` validation and normalization ·
`share-page.ts` HTML, CSS and CSP · `curl.ts` / `variables.ts` / `serde-json.ts` exporter port ·
`migrations/` schema · `tools/curl-parity-gen` Rust fixture generator. Tests in `test/` are named
after the PRODUCT.md AC ids and QA bug ids; `qa_parity_security.test.ts` belongs to QA.

# ReqHarbor server

The self-hostable server behind ReqHarbor's sync and read-only share links. It is one static Go
binary with a SQLite database, shipped as a Docker image. It replaces the Cloudflare Worker in
`worker/`, which is frozen.

This release (0.1, phases S0 to S3 of `docs/plans/accounts/06-go-server.md`) serves the
**legacy token-sync API**, wire-compatible with the Worker:

- `POST /v1/workspaces`, `GET /v1/workspace`, `POST /v1/workspace/token/rotate`;
- `GET`/`POST /v1/sync`;
- `GET`/`POST /v1/shares` and `PUT`/`DELETE /v1/shares/{id}`;
- the public share page `GET /s/{id}` and `GET`/`OPTIONS /s/{id}.json`;
- `GET /v1/server` (discovery), `/healthz` and `/readyz`.

The source tree is past 0.1: phase S4 adds **accounts** (email and password sign-in through hosted
pages under `/auth/*`, the token endpoint `POST /v1/auth/token`, `POST /v1/auth/logout`, setup and
invite codes, CLI reset links) and S5 adds **GitHub sign-in**. Personal-space sync (`/v1/me*`,
`/v1/spaces/*`) follows before the 0.2 release; until then the desktop app syncs with a workspace
token (Settings > Sync).

## Quick start

Token sync is **off by default** (`REQHARBOR_LEGACY_WORKSPACES=off`). With it off, every `/v1`
legacy route answers 403 `legacy_disabled`, and only existing share links keep working. So a fresh
0.1 server is useful only once you turn token sync on.

Build the image (from the repository root), then run it:

```sh
docker build -f server/Dockerfile -t reqharbor-server:dev server

docker volume create reqharbor-data
docker run -d --name reqharbor --restart unless-stopped \
  -p 127.0.0.1:8080:8080 \
  -v reqharbor-data:/data \
  -e REQHARBOR_PUBLIC_URL=http://localhost:8080 \
  -e REQHARBOR_LEGACY_WORKSPACES=open \
  reqharbor-server:dev

curl -s localhost:8080/healthz          # {"status":"ok"}
curl -s localhost:8080/v1/server        # discovery: version, syncEpoch, features, limits
```

Create a workspace and copy its token into the app (Settings > Sync, server URL
`http://localhost:8080`):

```sh
curl -s -X POST localhost:8080/v1/workspaces \
  -H 'Content-Type: application/json' -d '{"name":"My workspace"}'
# {"workspaceId":"01J…","token":"rh_…"}   the token is shown only once
```

Once released, the image is `ghcr.io/mihajlopopara/reqharbor-server`. Pin the minor version
(`:0.1`); there is no major-only tag before 1.0.

### What you must set

- `REQHARBOR_PUBLIC_URL` is **required** unless `REQHARBOR_LISTEN` is a loopback address. It is
  the address users type in the app, for example `https://rh.example.com`. Share links are built
  from it, and HSTS is sent only when it is https.
- `/data` must be writable by uid 65532: use a named volume, or `chown 65532:65532 ./data` for a
  bind mount.
- Run exactly one server per data directory. `serve` takes a lock on `/data/reqharbor.lock` and
  refuses to start when another instance holds it. Network filesystems are not supported.

## Token sync: choose a mode

`REQHARBOR_LEGACY_WORKSPACES` controls the legacy `rh_` workspaces:

| Value | Effect |
|---|---|
| `off` (default) | Every legacy `/v1` route returns 403 `legacy_disabled`. `/s/*` pages of existing shares keep working. |
| `existing` | Creating a workspace returns 403 `legacy_disabled`; existing tokens keep syncing and sharing. |
| `open` | Anyone who can reach the server can create a workspace and store data in it. |

`open` is anonymous storage and an anonymous share-page host for anyone who can reach your server,
and the server logs a warning about it on every start. Keep it behind a private network or VPN, or
turn it on only long enough to create the workspaces you need and then switch to `existing`. These
limits apply:

- `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` (default 10): workspace creates per client IP (IPv6 per
  /64) per hour;
- `REQHARBOR_QUOTA_LEGACY_BYTES` (default 50 MiB): storage per workspace, entities and shares
  together;
- `REQHARBOR_LEGACY_TOTAL_BYTES` (default 1 GiB, `0` for no cap): storage across all legacy
  workspaces. When it is reached, creates and growing writes return 507 `server_full`, while
  deletes keep working.

## Behind a reverse proxy

Terminate TLS in a proxy (Caddy, nginx, Traefik) and forward to port 8080. For per-client rate
limits, trust only the proxy's own address:

```sh
-e REQHARBOR_TRUSTED_PROXIES=172.30.53.10/32   # explicit CIDRs only
-e REQHARBOR_CLIENT_IP_HEADER=X-Forwarded-For   # or X-Real-IP, CF-Connecting-IP
```

There are two ways to get this wrong. A trust list that is too wide lets clients pick their own
rate-limit bucket by sending the header. A missing one puts every client in the proxy's single
bucket. The server logs a one-time warning for each case: when an untrusted peer sends the header,
and when all traffic arrives from one peer.

## Accounts (from 0.2)

- **First run.** While the server has no account, `serve` logs a setup code at each start:
  `docker compose logs reqharbor | grep setup`. In the ReqHarbor app, sign in to your server and
  enter the code; the account you create becomes the server admin. The code works only until the
  first account exists. `reqharbor-server setup-code` prints a fresh one; `REQHARBOR_SETUP_CODE`
  fixes it for automation (at least 12 characters of `0-9` and `A-Z` without `U`; dashes and
  spaces are ignored). Or skip the pages and run
  `reqharbor-server user create --email you@example.com --password-stdin` (the first CLI account
  is always admin).
- **Who may sign up** (`REQHARBOR_SIGNUP`): `invite` (default) needs a code from
  `reqharbor-server invite create [--email E] [--uses N] [--ttl 168h]`; `open` lets anyone with
  the address create an account; `closed` allows only the CLI. `REQHARBOR_MAX_USERS` (default 500)
  caps the total.
- **Passwords** are hashed with argon2id (`REQHARBOR_ARGON2_MEMORY_KIB`, default 64 MiB per hash,
  at most four at once). Without SMTP there is no email reset: run
  `reqharbor-server user reset-link --email E` and send the printed link privately (it works once,
  for 24 hours, and signs the account out everywhere).
- **Brute force.** Five wrong passwords for one email from one network throttle that pair, for 1,
  2, 4, … up to 60 minutes; other networks are unaffected, so nobody can lock the owner out. After
  20 failures for an email in an hour, the sign-in form asks the browser for a few seconds of
  proof of work. `reqharbor-server user unlock --email E` clears both. The database keeps only
  keyed hashes of (email, network) for 24 hours after the last failure.
- **GitHub sign-in** is on when both `REQHARBOR_GITHUB_CLIENT_ID` and
  `REQHARBOR_GITHUB_CLIENT_SECRET` (or `REQHARBOR_GITHUB_CLIENT_SECRET_FILE`, for Docker secrets)
  are set; one without the other is a startup error. On GitHub, open Settings > Developer settings
  > **OAuth Apps** (not a GitHub App) > New OAuth App, set the homepage to your public URL and the
  authorization callback URL to exactly `{REQHARBOR_PUBLIC_URL}/auth/callback/github`, for example
  `https://rh.example.com/auth/callback/github`. The server asks for `read:user user:email`, uses
  the account's **primary verified** email (an account without one cannot sign up), and keys the
  account by GitHub's numeric user id, never by email. GitHub's token is used once to read the
  profile and then dropped. Signup rules apply as for passwords (an invite in `invite` mode; on a
  fresh server the setup code, or a GitHub email equal to `REQHARBOR_ADMIN_EMAIL`, which then
  becomes the admin). An email that already has an account is refused, never merged: one sign-in
  method per account.
- **Personal spaces.** Every account gets one personal space at signup, where the app syncs that
  account's projects (see [API](#api)). `REQHARBOR_QUOTA_PERSONAL_BYTES` (default 100 MiB, `0`
  for no limit) caps each one. With `REQHARBOR_SIGNUP=open`, the personal spaces of accounts that
  signed up without a code also share `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` (default 5 GiB, `0` for
  no cap): once it is reached, their growing writes return 507 and open signup shows "This server
  isn't accepting new accounts" until they free space; invited, setup and CLI accounts are not
  counted. `REQHARBOR_MIN_CLIENT_VERSION` (for example `0.3.0`) refuses older apps on account sync
  and sign-in with 426; token sync is never gated.
- **Other commands:** `user list`, `user set-admin --email E --on|--off`, `user disable|enable
  --email E`, `user sessions revoke --email E`, `user set-password --email E --password-stdin`,
  `invite list`, `invite revoke ID`. In the image: `docker compose exec reqharbor /reqharbor-server
  user list`. They work while the server runs.

## Configuration

Every setting is an environment variable, validated at startup; an invalid value exits with code 2
and a one-line reason. A misspelt name in the limit families (`REQHARBOR_RATE_*`,
`REQHARBOR_QUOTA_*`, `REQHARBOR_MAX_*`) is an error, not a warning. The settings that matter for
this release:

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_PUBLIC_URL` | required (see above) | Origin users enter in the app |
| `REQHARBOR_LISTEN` | `:8080` | Listen address; `healthcheck` derives its URL from it |
| `REQHARBOR_DATA_DIR` | `/data` in the image | Database, lock file and backups |
| `REQHARBOR_LEGACY_WORKSPACES` | `off` | `off`, `existing` or `open` |
| `REQHARBOR_LEGACY_TOTAL_BYTES` | `1073741824` | Total legacy storage; `0` means no cap |
| `REQHARBOR_QUOTA_LEGACY_BYTES` | `52428800` | Per workspace |
| `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` | `10` | Workspace creates per IP bucket |
| `REQHARBOR_TRUSTED_PROXIES` | empty | Proxy CIDRs whose client-IP header is believed |
| `REQHARBOR_CLIENT_IP_HEADER` | `X-Forwarded-For` | Header carrying the client IP |
| `REQHARBOR_HSTS` | `max-age=31536000` | Sent only for an https public URL; `off` disables it |
| `REQHARBOR_MAX_DB_BYTES` | `0` (off) | Growing writes return 507 once the database reaches this size |
| `REQHARBOR_MIN_FREE_DISK_BYTES` | `536870912` | Growing writes return 507 below this much free disk |
| `REQHARBOR_LOG_LEVEL`, `REQHARBOR_LOG_FORMAT` | `info`, `json` | Logging |
| `REQHARBOR_GITHUB_CLIENT_ID`, `REQHARBOR_GITHUB_CLIENT_SECRET` | empty | Both set: GitHub sign-in (the secret also as `…_SECRET_FILE`) |
| `REQHARBOR_ADMIN_EMAIL` | empty | A GitHub (trusted) email that becomes admin when its account is created while no admin exists |
| `REQHARBOR_SIGNUP` | `invite` | `open`, `invite` or `closed` |
| `REQHARBOR_MAX_USERS` | `500` | Total accounts; `0` means no cap |
| `REQHARBOR_QUOTA_PERSONAL_BYTES` | `104857600` | Per personal space; `0` means unlimited |
| `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` | `5368709120` | Personal spaces of open-signup accounts together; `0` means no cap |
| `REQHARBOR_MIN_CLIENT_VERSION` | empty | 426 `client_too_old` on `/v1/spaces/*` and `/v1/auth/token` for older apps |
| `REQHARBOR_RATE_SYNC_PER_MIN` | `240` | `/v1/spaces/*/sync` requests per session |
| `REQHARBOR_RATE_SHARE_CREATE_PER_HOUR` | `30` | Share creates per space or workspace |

The full table, including the account settings that 0.2 uses, is §6 of
`docs/plans/accounts/06-go-server.md`.

## Operations

- **Health:** `/healthz` answers as soon as the listener is up; `/readyz` reports the database,
  migrations and free disk. The image's `HEALTHCHECK` runs `reqharbor-server healthcheck`.
- **Upgrades:** pull the new image and restart. Migrations run automatically at startup, after a
  backup of the database into `/data/backups/`.
- **Backups:** until the `backup` and `restore` commands arrive (0.2), stop the container and copy
  the whole data volume. Never copy a database file over a running server's data.

  > **Warning: restoring by copying the volume back is unsupported.** An older copy rolls the sync
  > revisions back. Devices whose cursors are ahead of it then silently skip every change made
  > after the copy was taken, and changes they had already pushed are never sent again. No 0.1
  > server has a `restore` command, and today's clients do not notice the rollback. If you must
  > put a copy back, make every device re-sync from scratch afterwards, for example by removing
  > and re-adding the workspace token. The supported path is `reqharbor-server restore` (S7),
  > which shifts the revs and starts a new sync epoch so that clients re-sync on their own.

- **Logs:** JSON lines on stdout. Access logs record the route pattern, never the raw path, so
  share ids do not appear in them.
- **Privacy:** the server stores what clients sync, with secret environment values blanked, and the
  share snapshots users publish, with secret variables removed. Client IPs are not logged unless
  `REQHARBOR_LOG_CLIENT_IP=true`.

## API

This section is the wire reference for the account routes that 0.2 adds (06-go-server.md §4 is
the design). Every error is `{"error":{"code","message"}}`; every response is JSON with
`Cache-Control: no-store` unless stated. The legacy token routes (`/v1/workspaces`, `/v1/sync`,
`/v1/shares*`, …) are unchanged from the Worker; see `docs/API_CONTRACT.md`.

### Authentication

`Authorization: Bearer rha_…` (the access token from `POST /v1/auth/token`, valid for 1 hour). A
legacy `rh_` token is never accepted here, and an `rha_` token is never accepted on legacy routes
(401 `unauthorized` there). Every 401 carries
`WWW-Authenticate: Bearer realm="reqharbor", error="invalid_token"`.

| Status | Code | When |
|---|---|---|
| 401 | `auth_required` | No or malformed token, unknown or revoked session, disabled account |
| 401 | `session_expired` | The access token expired: refresh it |
| 404 | `space_not_found` | Unknown space id, or the id of a legacy token workspace |
| 403 | `not_a_member` | The account is not a member of the space |
| 426 | `client_too_old` | `/v1/spaces/*` only, when `REQHARBOR_MIN_CLIENT_VERSION` is set and `X-ReqHarbor-Version` is missing or lower; checked before auth |
| 429 | `rate_limited` | With `Retry-After`; `/v1/spaces/{id}/sync` allows `REQHARBOR_RATE_SYNC_PER_MIN` per session |
| 507 | `server_full` | A growing write while the storage breaker is open, or an open-signup account's write once `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` is reached |

Order on `/v1/spaces/*`: route (404), method (405), version gate (426), token (401), space (404,
then 403), then the body (415, 413, 400).

### `GET /v1/me`

```json
{"user":{"id":"01J…","email":"ada@example.com","name":"Ada","avatarUrl":null,"isAdmin":false},
 "method":"password","sessionId":"01J…"}
```

`method` is `password`, or the provider id of the account's sign-in (`github`; `google` and
`oidc` in M3; `dev` in test builds).

### `GET /v1/me/sessions`, `DELETE /v1/me/sessions/{id}`

```json
{"sessions":[{"id":"01J…","deviceName":"MacBook Pro (macOS)","createdAt":1795435200000,
              "lastUsedDay":1795392000000,"current":true}]}
```

Live sessions only (not revoked, refresh token not expired), newest first. `createdAt` is Unix ms;
`lastUsedDay` is Unix ms at 00:00 UTC of the day the session was last used. `current` marks the
calling session. `DELETE` revokes one of the caller's sessions and answers `204` with no body;
that device gets 401 `auth_required` on its next call and 400 `invalid_grant` on refresh. Another
account's session id, an unknown id and an already revoked session are all 404 `not_found`.
Revoking the current session is allowed (it is logout).

### `GET /v1/me/spaces`

```json
{"spaces":[{"spaceId":"01J…","kind":"personal","teamId":null,"teamName":null,"role":"owner",
            "bytesUsed":1234,"quotaBytes":104857600}],
 "departed":[]}
```

The authoritative list of the account's spaces; in 0.2 it holds exactly the personal space, and the
shape is final. `quotaBytes` is `null` when unlimited. `bytesUsed` counts entity data plus active
share snapshots. `departed` stays empty until teams (M2), when its entries are
`{spaceId, teamId, teamName, reason, at}`.

### `GET /v1/spaces/{id}`

```json
{"spaceId":"01J…","kind":"personal","name":"Personal","createdAt":1795435200000,"entityCount":3,
 "bytesUsed":1234,"quotaBytes":104857600,"role":"owner"}
```

### `GET /v1/spaces/{id}/sync?since=&limit=`

The legacy pull: `since` and `limit` as for `/v1/sync`. Each change additionally carries
`updatedBy` when its version has an author, with the author's current name (`null` once the account
no longer exists):

```json
{"changes":[{"type":"request","id":"r1","updatedAt":1795435200000,"deleted":false,"deviceId":"01DEV",
             "data":{…},"rev":7,"updatedBy":{"userId":"01J…","name":"Ada"}}],
 "cursor":7,"hasMore":false,"syncEpoch":"01J…"}
```

### `POST /v1/spaces/{id}/sync`

The legacy push body `{"changes":[…]}`, validated in the same order with the same 400s (500
changes, `id`, `updatedAt`, `deleted`, `deviceId`, `data`, `duplicate_change`), LWW (contract C3)
and revs in push order. Differences from `/v1/sync`:

- A `type` outside `["project","collection","folder","request","environment"]` (the list
  `/v1/server` publishes as `sync.types`) is the per-change result
  `{"type":"protoFile","id":"p1","status":"rejected","reason":"unsupported_type"}` instead of a
  whole-request 400, provided the change is otherwise well formed (a non-string `type` is still a
  400 `unsupported_type`). It takes no rev and is not counted against the quota.
- A change whose `updatedAt` is more than 5 minutes (300000 ms) ahead of the server's clock is
  `{"status":"rejected","reason":"clock_skew"}`, tombstones included; exactly +5 min is accepted.
- Precedence when several apply: `unsupported_type`, then `clock_skew`, then `invalid_json_value`
  and `entity_too_large`.
- Applied changes record the caller as the author (`updatedBy`); a `stale` change keeps the stored
  author.
- The response adds `serverTime` (the server's clock, Unix ms):

```json
{"results":[{"type":"request","id":"r1","status":"applied","rev":8},
            {"type":"protoFile","id":"p1","status":"rejected","reason":"unsupported_type"},
            {"type":"request","id":"r2","status":"rejected","reason":"clock_skew"}],
 "cursor":8,"serverTime":1795435201234}
```

Quota: 413 `quota_exceeded` when the push grows the space past `REQHARBOR_QUOTA_PERSONAL_BYTES`;
deletes and shrinking pushes always pass, also while 507 applies.

### `GET`/`POST /v1/spaces/{id}/shares`, `PUT`/`DELETE /v1/spaces/{id}/shares/{shareId}`

The legacy share routes scoped to the space, plus `createdBy` and `options`:

- **Create** body: `{"kind","entityId","snapshot","options"?}` with `options`
  `{"includeCredentials":bool,"environmentId":string|null}`, absent or `null` for none. A missing
  `includeCredentials` is `false`; unknown option keys are dropped; a wrong type is 400
  `validation_failed`. 30 creates per hour per space (`REQHARBOR_RATE_SHARE_CREATE_PER_HOUR`),
  charged only after the body validated. Response `201`:

  ```json
  {"shareId":"…","url":"https://rh.example.com/s/…","createdAt":1795435200000,"updatedAt":1795435200000,
   "createdBy":{"userId":"01J…","name":"Ada"},"options":{"includeCredentials":false,"environmentId":null}}
  ```
- **List**: `{"shares":[{"shareId","url","kind","entityId","name","createdAt","updatedAt","createdBy","options"}]}`,
  active shares newest first. `createdBy.name` is `null` when that account no longer exists
  ("Former member"); `options` is `null` when none were stored.
- **Update** (`PUT`) body `{"snapshot","options"?}`: `options` absent or `null` keeps the stored
  options, an object replaces them. Checked in order: share id format (route 404), auth, space,
  share of this space (404 "Share not found"), revoked (410), then the body. Response as for
  create, `200`.
- **Revoke** (`DELETE`) never reads a body: `{"shareId","revokedAt"}`, idempotent.

The public `/s/{id}` and `/s/{id}.json` pages serve space shares exactly like legacy ones.

## Development

Everything runs in Docker, from the repository root:

```sh
docker compose run --rm go go vet ./...
docker compose run --rm go go test ./...
docker compose run --rm go gofmt -l .
docker compose run --rm --service-ports go go run ./cmd/reqharbor-server serve   # :8080, token sync open
docker build -f server/Dockerfile -t reqharbor-server:dev server
```

`docker compose run --rm go go test -tags devauth ./...` adds the dev sign-in provider's tests.
That provider (a form taking any email, for automated tests) exists only in binaries built with
`-tags devauth`; the image and release builds never contain it. A devauth binary refuses a
listener that is not a loopback IP, answers 404 to dev-provider requests from any other TCP peer
(forwarded headers do not count), and dev accounts' emails are never trusted.

The live client suite runs the real Rust client against a devauth build:

```sh
docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
docker compose run --rm dev bash qa/run_live_server_tests.sh
docker compose run --rm dev cargo test -p reqharbor-core --test serde_parity
```

The share page's cURL export must match the Rust exporter byte for byte;
`internal/curlexport/parity_test.go` checks it against `worker/test/fixtures/curl_parity.json`.

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

Accounts, sign-in and personal spaces come in 0.2. Until then, the desktop app syncs with a
workspace token (Settings > Sync).

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

## Development

Everything runs in Docker, from the repository root:

```sh
docker compose run --rm go go vet ./...
docker compose run --rm go go test ./...
docker compose run --rm go gofmt -l .
docker compose run --rm --service-ports go go run ./cmd/reqharbor-server serve   # :8080, token sync open
docker build -f server/Dockerfile -t reqharbor-server:dev server
```

The live client suite runs the real Rust client against a devauth build:

```sh
docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
docker compose run --rm dev bash qa/run_live_server_tests.sh
docker compose run --rm dev cargo test -p reqharbor-core --test serde_parity
```

The share page's cURL export must match the Rust exporter byte for byte;
`internal/curlexport/parity_test.go` checks it against `worker/test/fixtures/curl_parity.json`.

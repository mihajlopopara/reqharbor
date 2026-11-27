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

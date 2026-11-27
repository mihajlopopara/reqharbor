# ReqHarbor server

The self-hostable server behind ReqHarbor's accounts, sync and read-only share links. It is one
static Go binary with a SQLite database, shipped as a Docker image
(`ghcr.io/mihajlopopara/reqharbor-server`, amd64 and arm64). There is no central ReqHarbor
service: you run this server, and people type its address into the app.

What it serves:

- **Accounts.** Email and password sign-in, and GitHub sign-in, through pages the server hosts
  under `/auth/*`. The desktop app always signs in through the system browser. Signup is by invite
  by default.
- **Personal sync.** Every account has a personal space that the app keeps in sync.
- **Share links.** Read-only pages for a request or a collection (`/s/{id}`), with a cURL export.
- **Token sync (legacy).** The anonymous `rh_` workspace tokens of older app versions, wire
  compatible with the retired Cloudflare Worker. **Off by default.**

Contents: [Quick start](#quick-start) · [First run](#first-run) ·
[Sign-in methods](#sign-in-methods) · [Behind a reverse proxy](#behind-a-reverse-proxy) ·
[Token sync](#token-sync-legacy-workspaces) · [Backups and restore](#backups-and-restore) ·
[Upgrading](#upgrading) · [Configuration reference](#configuration-reference) ·
[Operations](#operations) · [Privacy](#privacy) · [API](#api) · [Development](#development)

## Quick start

You need a host with Docker and Compose, and a DNS name pointing at it (`rh.example.com` below).

```sh
mkdir reqharbor && cd reqharbor
curl -fsSLo docker-compose.yml https://raw.githubusercontent.com/mihajlopopara/reqharbor/main/server/docker-compose.example.yml
curl -fsSLo Caddyfile https://raw.githubusercontent.com/mihajlopopara/reqharbor/main/server/Caddyfile.example
# Replace rh.example.com with your domain in both files, then:
docker compose --profile tls up -d
docker compose logs reqharbor | grep setup     # the first-run setup code
```

That starts two containers on a private network with a fixed subnet:

- `reqharbor` on `172.30.53.20`, with its database in the `reqharbor-data` volume. It also listens
  on `127.0.0.1:8080` of the host, for testing (`curl -s localhost:8080/healthz`); remove that
  `ports:` entry once Caddy works.
- `caddy` on `172.30.53.10` (profile `tls`), which gets a certificate for your domain and forwards
  to the server. Ports 80 and 443 must reach the host.

`REQHARBOR_TRUSTED_PROXIES` in the example trusts exactly Caddy's address, so the server sees each
client's own IP. If `172.30.53.0/24` collides with a network you already have, pick another subnet
and change all four values that depend on it: the `subnet` under `networks:`, the `ipv4_address`
of `reqharbor` (`172.30.53.20`), the `ipv4_address` of `caddy` (`172.30.53.10`), and
`REQHARBOR_TRUSTED_PROXIES` (Caddy's address, `/32`).

Pin the minor version (`:0.2`). Releases before 1.0 may change behaviour between minor versions,
so there is no major-only tag yet; `latest` follows the newest stable release.

### What you must get right

- **`REQHARBOR_PUBLIC_URL`** is the exact origin people type in the app, for example
  `https://rh.example.com` (no path). It is required unless `REQHARBOR_LISTEN` is a loopback
  address. Sign-in pages, OAuth callbacks, reset links and share links are built from it, and a
  request that reaches a sign-in page on another address is redirected to it.
- **HTTPS.** Run the server behind TLS. With an `http` public URL that is not loopback, passwords,
  tokens and synced data travel unencrypted, and the server warns about it at every start.
- **One server per data directory.** `serve` holds a lock on `/data/reqharbor.lock` and refuses to
  start while another process holds it. Do not run two replicas on one volume, and do not put
  `/data` on a network filesystem.
- **Permissions.** The image runs as uid 65532. A named volume just works; for a bind mount run
  `chown 65532:65532 ./data` first.

## First run

While the server has no account, `serve` logs a setup code at each start:

```sh
docker compose logs reqharbor | grep setup
# First-run setup: in the ReqHarbor app, sign in to https://rh.example.com and enter setup code K7Q4-ZX2P-8M3D-1R6T
```

In the app, open Settings › Account, choose **Sign in**, enter your server's address and continue.
The browser opens the server's pages; on a fresh server they ask for the setup code ("Finish setting
up this server"). Enter it and create the account: that first account is the **server admin**. The
code works only until the first account exists.

Other ways to create the first account:

- the CLI, with the password on standard input (`-T`, because `exec` must not allocate a terminal
  when its input is a pipe); the first account created by the CLI is always admin:

  ```sh
  printf '%s\n' "$PW" | docker compose exec -T reqharbor /reqharbor-server user create --email you@example.com --password-stdin
  ```
- `REQHARBOR_ADMIN_EMAIL`: a GitHub account whose primary verified email equals it becomes admin
  when it signs up, as long as no admin exists yet;
- `reqharbor-server setup-code` prints a fresh code; `REQHARBOR_SETUP_CODE` fixes one for
  automation.

Then invite people: `docker compose exec reqharbor /reqharbor-server invite create` prints a code
such as `K7Q4-ZX2P-8M3D-1R6T` (`--email E` restricts it to one address, `--uses N` lets it create N
accounts, `--ttl 168h` is the default lifetime). They enter it on the sign-up page.

### Who may sign up

| `REQHARBOR_SIGNUP` | Effect |
|---|---|
| `invite` (default) | A new account needs an invite code (or the setup code, on a fresh server). |
| `open` | Anyone who can reach the server can create an account. |
| `closed` | Only the CLI creates accounts. |

`REQHARBOR_MAX_USERS` (default 500, `0` for no cap) limits the total; the first account is never
refused. With `open`, the personal spaces of accounts that signed up without a code also share
`REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` (default 5 GiB): once it is reached, their growing writes
return 507 and open signup pauses ("This server isn't accepting new accounts") until they free
space. Invited, setup-code and CLI accounts are not counted.

### Account administration

All of these work while the server runs (`docker compose exec reqharbor /reqharbor-server …`):

```
user list
user create --email E [--name N] [--admin] [--force] [--password-stdin | --reset-link]
user set-password --email E --password-stdin
user reset-link --email E [--ttl 24h]      print a one-time password reset link
user set-admin --email E (--on|--off)
user disable --email E                     also signs the account out everywhere
user enable --email E
user unlock --email E                      clear the password throttle for that email
user sessions revoke --email E             sign the account out everywhere
invite create [--email E] [--uses 1] [--ttl 168h]
invite list ; invite revoke ID
setup-code                                 only while no account exists
```

There is no email in this release, so there is no self-service password reset: run
`user reset-link --email E` and send the printed link privately. It works once, for 24 hours, and
signs the account out on every device.

**Passwords** are hashed with argon2id (`REQHARBOR_ARGON2_MEMORY_KIB`, default 64 MiB per hash, at
most four at once). **Brute force:** five wrong passwords for one email from one network throttle
that pair for 1, 2, 4, … up to 60 minutes; other networks are unaffected, so nobody can lock the
owner out. After 20 failures for an email within an hour, the sign-in form asks the browser for a
few seconds of proof of work. `user unlock --email E` clears both. The database keeps only keyed
hashes of (email, network), for 24 hours after the last failure.

## Sign-in methods

**Email and password** is on unless `REQHARBOR_PASSWORD_AUTH=false`.

**GitHub** is on when both `REQHARBOR_GITHUB_CLIENT_ID` and `REQHARBOR_GITHUB_CLIENT_SECRET` (or
`REQHARBOR_GITHUB_CLIENT_SECRET_FILE`, for Docker secrets) are set; one without the other is a
startup error. To register the app:

1. On GitHub, open Settings > Developer settings > **OAuth Apps** (not GitHub Apps) > New OAuth App.
   For an organisation, use the organisation's settings instead.
2. Application name: anything, for example "ReqHarbor (Acme)". Homepage URL: your public URL.
3. Authorization callback URL: exactly `{REQHARBOR_PUBLIC_URL}/auth/callback/github`, for example
   `https://rh.example.com/auth/callback/github`. Leave "Enable Device Flow" off.
4. Register, then "Generate a new client secret". Put the client id and the secret in the server's
   environment; with compose, prefer a secret file:

   ```yaml
   services:
     reqharbor:
       environment:
         REQHARBOR_GITHUB_CLIENT_ID: Ov23liAbCdEf0123
         REQHARBOR_GITHUB_CLIENT_SECRET_FILE: /run/secrets/github_secret
       secrets: [github_secret]
   secrets:
     github_secret:
       file: ./github_client_secret.txt    # chmod 600
   ```
5. `docker compose up -d`. The sign-in page now shows "Continue with GitHub".

The server asks GitHub for `read:user user:email`, uses the account's **primary verified** email
(an account without one cannot sign up), and keys the ReqHarbor account by GitHub's numeric user
id, never by email. GitHub's token is used once to read the profile and then dropped. Signup rules
apply as for passwords. An email that already belongs to another account is refused, never merged:
each account has exactly one sign-in method.

Google, a generic OpenID Connect provider and SMTP (email verification and self-service reset) come
in a later release. Their variables are already accepted (see the reference) but have no effect yet.

## Behind a reverse proxy

The server speaks plain HTTP on port 8080; terminate TLS in a proxy. The compose example uses
Caddy (`Caddyfile.example`):

```
rh.example.com {
    encode zstd gzip
    reverse_proxy reqharbor:8080
}
```

With nginx:

```nginx
server {
    listen 443 ssl;
    server_name rh.example.com;
    # ssl_certificate …; ssl_certificate_key …;
    client_max_body_size 6m;                 # a push can be 5 MiB
    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_read_timeout 120s;
    }
}
```

Here nginx runs on the host and reaches the container through the published `127.0.0.1:8080`
port, so the server sees it as the Docker bridge gateway (`172.30.53.1` with the example network):
trust that address, `REQHARBOR_TRUSTED_PROXIES=172.30.53.1/32`. Only processes on the host can
reach that port.

Tell the server which peers are proxies, so it believes their client-IP header for rate limits:

| Variable | Default | |
|---|---|---|
| `REQHARBOR_TRUSTED_PROXIES` | empty | Comma-separated CIDRs of your proxies, and nothing wider: `172.30.53.10/32`, `127.0.0.1/32` |
| `REQHARBOR_CLIENT_IP_HEADER` | `X-Forwarded-For` | Or `X-Real-IP`, `CF-Connecting-IP` behind Cloudflare |

There are two ways to get this wrong, and the server logs a one-time warning for each:

- **Too wide** (a whole subnet that clients can also reach from): clients send the header
  themselves and pick their own rate-limit bucket. Warning: a peer outside the list sent the header.
- **Missing**: every client lands in the proxy's single bucket, and one busy user throttles
  everyone. Warning: all traffic arrives from one peer.

Keywords such as `private` are not accepted; list exact addresses. With Caddy in the compose
example, requests on the `127.0.0.1:8080` test port arrive from the bridge gateway, which is not
trusted, so their forwarded headers are ignored, and sign-in pages redirect them to the public URL.

`REQHARBOR_HSTS` (default `max-age=31536000`, sent only for an https public URL) sets
`Strict-Transport-Security`; it does not include `includeSubDomains` unless you add it, and `off`
disables it.

## Token sync (legacy workspaces)

App versions before accounts sync through an anonymous workspace token (`rh_…`, Settings > Sync).
`REQHARBOR_LEGACY_WORKSPACES` controls it:

| Value | Effect |
|---|---|
| `off` (default) | Every legacy `/v1` route returns 403 `legacy_disabled`. Share pages of existing shares keep working. |
| `existing` | Creating a workspace returns 403 `legacy_disabled`; existing tokens keep syncing and sharing. |
| `open` | Anyone who can reach the server can create a workspace and store data in it. |

`open` is anonymous storage, and an anonymous share-page host under your domain, for anyone who can
reach the server; it can be abused for spam or phishing, and the server says so at every start.
Use it only on a private network or VPN, or turn it on just long enough to create the workspaces
you need and then switch to `existing`. Limits: `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` (10 creates
per client IP, IPv6 per /64), `REQHARBOR_QUOTA_LEGACY_BYTES` (50 MiB per workspace) and
`REQHARBOR_LEGACY_TOTAL_BYTES` (1 GiB across all of them; then creates and growing writes return
507 while deletes keep working).

## Backups and restore

### Taking backups

```sh
docker compose exec reqharbor /reqharbor-server backup
# /data/backups/manual-20261127-120000.db
# Backup complete: 1843200 bytes, schema v4. …
```

`backup [--out PATH]` writes a consistent snapshot with SQLite's `VACUUM INTO` while the server
keeps running; pushes are not blocked. The file is readable by its owner only (it holds password
and token hashes). Before migrating to a newer schema, the server also takes a
`pre-migrate-v<from>-v<to>-<time>.db` backup on its own.

Backups stay inside the data volume until you copy them out. Do that regularly, for example from
the host with cron:

```sh
docker compose exec -T reqharbor /reqharbor-server backup
docker run --rm -v reqharbor_reqharbor-data:/data:ro -v "$PWD/offsite:/out" alpine \
  sh -c 'cp /data/backups/*.db /out/'
```

Delete old files from `/data/backups` yourself; the server never prunes manual backups. For
continuous replication you can run [Litestream](https://litestream.io) as a sidecar on
`/data/reqharbor.db`; it is not shipped with the image.

### Restoring

> **Warning: only `reqharbor-server restore` is supported. Never copy a backup over
> `reqharbor.db` by hand.** A copied-back file rolls the sync revisions back below what devices
> already have. Devices then silently skip the changes made after the backup, and the changes they
> made themselves after it are never sent again.

First stop everything that has `/data/reqharbor.db` open: the server, and any Litestream or other
sidecar, backup job or `reqharbor-server` command you run against the volume. Then:

```sh
docker compose stop reqharbor
docker compose run --rm reqharbor restore /data/backups/manual-20261127-120000.db
docker compose start reqharbor
```

`restore` must run while the `reqharbor` service is stopped. With the example's fixed
`ipv4_address`, `docker compose run` against a running server fails with "failed to set up
container networking: Address already in use"; run `docker compose stop reqharbor` first.

`restore FILE [--rev-base N]` refuses to run while the server holds the data directory, and while
any other process holds the database open ("another process has … open; stop it first"),
because the database's write-ahead log could not be folded into the copy it keeps. It:

1. reads the current database's revision of every workspace and folds its write-ahead log into the
   file (a database written by a newer server is fine here, so you can go back to the previous
   image and restore its `pre-migrate-…` backup);
2. copies the backup, checks its integrity, refuses one written by a newer server, and migrates an
   older one forward;
3. moves every restored revision above the current ones, so every device receives everything
   again;
4. starts a new **sync epoch** (the server's instance id stays the same);
5. keeps the current database as `reqharbor.db.pre-restore-<time>` (with its `-wal`, if any) and
   puts the restored one in place.

After the restart, apps that support sync epochs notice the new epoch on their next sync, download
everything again and re-upload what they changed after the backup was taken. Older apps also
download everything again, but do not re-upload their own later changes; have their users update
the app first. Devices that signed in, or refreshed their sign-in, after the backup was taken must
sign in again, and workspaces and accounts created after it no longer exist.

**A restore also undoes every revocation made after the backup was taken:** sessions signed out
come back, disabled accounts are enabled again, reset passwords return to the old ones, and revoked
invites work again. Repeat those runs after the restore: `user disable`, `user sessions revoke`,
`user set-password` or `user reset-link`, and `invite revoke`.

If the data volume itself was lost, start from an empty volume and run `restore`: without a
current database, revisions move up by 2³² above the backup's highest one, which is above any
cursor a device can hold. `--rev-base N` sets that base yourself; it must be above every revision
in the backup and in the current database.

## Upgrading

```sh
# edit the image tag in docker-compose.yml (for example :0.2 → :0.3), then:
docker compose pull reqharbor
docker compose up -d reqharbor
docker compose logs -f reqharbor
```

At startup the server answers `/healthz` at once, takes a `pre-migrate-…` backup into
`/data/backups/` when the schema changes, applies the migrations and then opens the routes
(`/readyz` and every route answer 503 `starting` until then). If a migration fails, the server
stays unready with the error in the log, and the database is unchanged by the failed step. To go
back, run the previous image tag and `restore` the pre-migrate backup: a database written by a newer
server is refused by an older one ("database schema vN is newer than this reqharbor-server").

Shutdown is graceful: on `docker compose stop` (SIGTERM) the server stops accepting connections,
lets requests in flight finish for up to `REQHARBOR_SHUTDOWN_TIMEOUT` (20 s), stops its background
jobs, folds the write-ahead log into the database file and releases its lock. In the worst case
that takes about 45 s (the 20 s timeout, up to 10 s for a write that outlived it, then the
checkpoint and closing the database). Compose kills a container after only 10 s by default, so the
example sets `stop_grace_period: 60s`: keep it about 40 s above `REQHARBOR_SHUTDOWN_TIMEOUT` if you
raise the timeout.

## Configuration reference

Every setting is an environment variable, validated at startup; an invalid value exits with code 2
and a one-line reason. A misspelt name in the limit families (`REQHARBOR_RATE_*`,
`REQHARBOR_QUOTA_*`, `REQHARBOR_MAX_*`) is an error ("did you mean …?"); other unknown
`REQHARBOR_*` names are logged as warnings. Every `*_SECRET` and `*_PASSWORD` variable can instead
be given as `<NAME>_FILE`, the path of a file holding the value. Sizes are in bytes; `0` means no
cap unless stated.

**Server**

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_PUBLIC_URL` | required, unless the listener is loopback | Origin users enter in the app |
| `REQHARBOR_LISTEN` | `:8080` | Listen address; the image's healthcheck derives its URL from it |
| `REQHARBOR_DATA_DIR` | `/data` in the image, `./data` otherwise | Database, lock file and `backups/` |
| `REQHARBOR_SERVER_NAME` | host of the public URL | Shown in the app and on the hosted pages |
| `REQHARBOR_HSTS` | `max-age=31536000` | `Strict-Transport-Security` for an https public URL; `off` disables it |
| `REQHARBOR_TRUSTED_PROXIES` | empty | Proxy CIDRs whose client-IP header is believed |
| `REQHARBOR_CLIENT_IP_HEADER` | `X-Forwarded-For` | Header carrying the client IP |
| `REQHARBOR_LOG_LEVEL` | `info` | `debug`, `info`, `warn` or `error` |
| `REQHARBOR_LOG_FORMAT` | `json` | `json` or `text` |
| `REQHARBOR_LOG_CLIENT_IP` | `false` | Include client IPs in access logs |
| `REQHARBOR_SHUTDOWN_TIMEOUT` | `20s` | Graceful-shutdown budget for requests in flight |
| `REQHARBOR_DATABASE_URL` | unset | Reserved for Postgres; refused when set |

**Accounts and sign-in**

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_SIGNUP` | `invite` | `open`, `invite` or `closed` |
| `REQHARBOR_MAX_USERS` | `500` | Total accounts; `0` means no cap |
| `REQHARBOR_ADMIN_EMAIL` | empty | A verified provider email that becomes admin at account creation while no admin exists |
| `REQHARBOR_SETUP_CODE` | random at each start | Fixed first-run setup code (at least 12 of `0-9`, `A-Z` without `U`) |
| `REQHARBOR_PASSWORD_AUTH` | `true` | Email and password sign-in and signup |
| `REQHARBOR_ARGON2_MEMORY_KIB` | `65536` | Memory per argon2id hash |
| `REQHARBOR_GITHUB_CLIENT_ID`, `REQHARBOR_GITHUB_CLIENT_SECRET` | empty | Both set: GitHub sign-in |
| `REQHARBOR_SESSION_MAX` | `20` | Signed-in devices per account; the oldest is signed out |
| `REQHARBOR_MIN_CLIENT_VERSION` | empty | For example `0.3.0`: older apps get 426 on account sync and sign-in (token sync is never gated) |

**Storage limits**

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_QUOTA_PERSONAL_BYTES` | `104857600` (100 MiB) | Per personal space; `0` means unlimited |
| `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` | `5368709120` (5 GiB) | Personal spaces of open-signup accounts together |
| `REQHARBOR_LEGACY_WORKSPACES` | `off` | `off`, `existing` or `open` (see Token sync) |
| `REQHARBOR_QUOTA_LEGACY_BYTES` | `52428800` (50 MiB) | Per legacy workspace |
| `REQHARBOR_LEGACY_TOTAL_BYTES` | `1073741824` (1 GiB) | All legacy workspaces together |
| `REQHARBOR_MAX_DB_BYTES` | `0` (off) | Growing writes return 507 `server_full` once the database file reaches this size |
| `REQHARBOR_MIN_FREE_DISK_BYTES` | `536870912` (512 MiB) | Growing writes return 507 when the data directory has less free space |

The storage breaker is sampled every 10 seconds. While it is open, deletes, shrinking writes and
every read keep working. Fixed protocol limits (the app relies on them): 512 KiB per entity, 1 MiB
per share snapshot, 5 MiB per request body, 500 changes per push or pull.

**Rate limits** (in-process; per client IP bucket, IPv6 per /64, unless stated)

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_RATE_AUTH_PER_MIN` | `120` | Sign-in page requests |
| `REQHARBOR_RATE_LOGIN_PER_MIN` | `30` | Password attempts |
| `REQHARBOR_RATE_SIGNUP_PER_HOUR` | `30` | Account creations |
| `REQHARBOR_RATE_TOKEN_PER_MIN` | `60` | `POST /v1/auth/token` |
| `REQHARBOR_RATE_SYNC_PER_MIN` | `240` | `/v1/spaces/*/sync` requests per signed-in device |
| `REQHARBOR_RATE_SHARE_CREATE_PER_HOUR` | `30` | Share creations per space or workspace |
| `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` | `10` | Legacy workspace creations |

**Accepted for later releases, no effect yet:** `REQHARBOR_GOOGLE_CLIENT_ID`/`_SECRET`,
`REQHARBOR_OIDC_ISSUER`/`_CLIENT_ID`/`_CLIENT_SECRET`/`_NAME`/`_SCOPES`/`_REQUIRE_VERIFIED_EMAIL`,
`REQHARBOR_SMTP_HOST`/`_PORT`/`_USERNAME`/`_PASSWORD`/`_FROM`/`_TLS`,
`REQHARBOR_RATE_EMAIL_PER_HOUR`, `REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR`,
`REQHARBOR_BACKUP_INTERVAL`, `REQHARBOR_BACKUP_KEEP` (scheduled backups), `REQHARBOR_TEAMS`,
`REQHARBOR_QUOTA_TEAM_BYTES`, `REQHARBOR_QUOTA_OWNER_BYTES`, `REQHARBOR_MAX_OWNED_TEAMS`,
`REQHARBOR_MAX_TEAM_MEMBERS`, `REQHARBOR_MAX_TEAM_INVITES`. `REQHARBOR_DEV_AUTH` exists only in
test builds and is refused by the image.

## Operations

- **Health:** `/healthz` answers 200 as soon as the listener is up; `/readyz` reports the database,
  the migrations and free disk, and answers 503 until the server is ready. The image's
  `HEALTHCHECK` runs `reqharbor-server healthcheck`.
- **Logs:** JSON lines on stdout. One access-log line per request with the route pattern (never
  the raw path, so share ids and space ids stay out of logs), status, duration, size and request
  id; every response carries `X-Request-Id`. Query strings, `Authorization`, cookies and form bodies
  are never logged.
- **Background jobs:** the storage breaker every 10 seconds; expired sign-in requests, sessions and
  codes are deleted every 10 minutes (revoked or expired ones after 7 days); `PRAGMA optimize`
  and `PRAGMA incremental_vacuum` once a day.
- **Single instance:** one process per data directory (see above). Scale up, not out.
- **Version:** `docker compose exec reqharbor /reqharbor-server version`; `GET /v1/server` reports
  it too.

## Privacy

The server stores what the apps sync, with secret environment values blanked by the app before
upload, and the share snapshots users publish, with secret variables removed. Passwords are stored
as argon2id hashes; tokens, codes and invite codes only as SHA-256 hashes. Client IPs are not logged
unless `REQHARBOR_LOG_CLIENT_IP=true`; the password throttle keeps keyed hashes for 24 hours.

Deleting data deletes it from the live database at once, but **backups keep deleted data for as
long as you keep the backup files**, including the automatic `pre-migrate-…` and
`…pre-restore-…` files. Tell your users how long you keep them.

## API

This section is the wire reference for the account routes (since 0.2) (06-go-server.md §4 is
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

Run `docker compose run --rm go go test ./internal/db -update` to rewrite the golden schemas after
reviewing a migration. `internal/db/testdata/legacy_seed.sql` is the Worker-era database that the
populated-DB migration test upgrades; keep it loadable at schema v2.

The live client suite runs the real Rust client against a devauth build, including the restore
cases, which stop and restart the server through `qa/live_server_ctl.sh`:

```sh
docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
docker compose run --rm dev bash qa/run_live_server_tests.sh
docker compose run --rm dev cargo test -p reqharbor-core --test serde_parity
```

The share page's cURL export must match the Rust exporter byte for byte;
`internal/curlexport/parity_test.go` checks it against `worker/test/fixtures/curl_parity.json`.

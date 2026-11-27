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
- **Teams** (off by default, `REQHARBOR_TEAMS=true`). Team spaces shared by their members with
  owner, admin, editor and viewer roles, joined through invite links (`/i#…`).
- **Share links.** Read-only pages for a request or a collection (`/s/{id}`), with a cURL export.
- **Token sync (legacy).** The anonymous `rh_` workspace tokens of older app versions, wire
  compatible with the retired Cloudflare Worker. **Off by default.**

Contents: [Quick start](#quick-start) · [First run](#first-run) ·
[Sign-in methods](#sign-in-methods) · [Teams](#teams) · [Behind a reverse proxy](#behind-a-reverse-proxy) ·
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
| `invite` (default) | A new account needs an invite code (or the setup code, on a fresh server), or a team invite link (see Teams). |
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

## Teams

Teams are off until you set `REQHARBOR_TEAMS=true` (`GET /v1/server` reports it as
`features.teams`). Anyone with an account can then create teams; each team has one shared space
that its members' apps sync.

- **Roles.** Viewers read and send; editors also change, create and delete collections, folders,
  requests and environments, create and rename projects, and create share links (and change their
  own); admins also delete projects, change anyone's share links, rename the team, invite and
  remove members and change roles up to admin; owners also create admin invite links, grant or
  revoke owner and transfer ownership. The team's **billing owner** (its creator, until a
  transfer) is the only one who can delete it, and must transfer ownership before leaving or being
  demoted. Editors can delete any content of a team project; admin-only applies to deleting the
  project itself.
- **Invite links** look like `https://rh.example.com/i#rhi_…`. The token sits in the URL fragment,
  so it never reaches a server or proxy log; only its hash is stored. Opening the link in a
  browser shows who invited you and the role, and says to paste it into ReqHarbor (Teams › Join
  team). Admin links are single-use; links last 1 to 30 days. Removing a member revokes the links
  they created, and links created before the removal no longer let that account back in.
- **Signup through an invite.** While `REQHARBOR_SIGNUP=invite`, a person without an account who
  holds a team invite link can sign up without a server invite code: the app trades the link for a
  10-minute, single-use signup ticket. A link limited to N uses opens at most N signups. Tickets
  have no effect in `open` mode (those accounts stay open signups) or `closed` mode.
- **Limits** (`0` means no cap): `REQHARBOR_MAX_OWNED_TEAMS` (default 10 teams per billing owner;
  personal spaces do not count), `REQHARBOR_MAX_TEAM_MEMBERS` (100), `REQHARBOR_MAX_TEAM_INVITES`
  (20 active links per team; plus 30 new links per team per day), `REQHARBOR_QUOTA_TEAM_BYTES`
  (250 MiB per team space) and `REQHARBOR_QUOTA_OWNER_BYTES` (off: when set, the personal space
  plus every team a user is billing owner of share it).
- **Privacy.** Team data, including literal credentials typed into requests, is visible to every
  member. Secret variable values never leave the devices, and local file paths (binary bodies,
  multipart file fields) are blanked before they reach a team space.
- **Turning teams off again** hides the team routes (403 `teams_disabled`) but keeps existing team
  spaces syncing for their members.

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
`user set-password` or `user reset-link`, and `invite revoke`. The same goes for teams: member
removals, role demotions and revoked team invite links made after the backup are undone, so removed
members are back in their teams with their old roles. Tell team admins to repeat those removals,
demotions and revocations in the app.

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
| `REQHARBOR_RATE_AUTH_PER_MIN` | `120` | Sign-in page requests; also `POST /v1/invites/preview` and `/v1/invites/ticket` |
| `REQHARBOR_RATE_LOGIN_PER_MIN` | `30` | Password attempts |
| `REQHARBOR_RATE_SIGNUP_PER_HOUR` | `30` | Account creations |
| `REQHARBOR_RATE_TOKEN_PER_MIN` | `60` | `POST /v1/auth/token` |
| `REQHARBOR_RATE_SYNC_PER_MIN` | `240` | `/v1/spaces/*/sync` requests per signed-in device |
| `REQHARBOR_RATE_SHARE_CREATE_PER_HOUR` | `30` | Share creations per space or workspace |
| `REQHARBOR_RATE_LEGACY_CREATE_PER_HOUR` | `10` | Legacy workspace creations |

**Teams**

| Variable | Default | Meaning |
|---|---|---|
| `REQHARBOR_TEAMS` | `false` | Team routes and invite links (`features.teams` in `/v1/server`) |
| `REQHARBOR_QUOTA_TEAM_BYTES` | `262144000` (250 MiB) | Per team space; `0` means unlimited |
| `REQHARBOR_QUOTA_OWNER_BYTES` | `0` (off) | A billing owner's personal space plus owned team spaces together |
| `REQHARBOR_MAX_OWNED_TEAMS` | `10` | Teams one account may be billing owner of; `0` means no cap |
| `REQHARBOR_MAX_TEAM_MEMBERS` | `100` | Members per team (at least 1) |
| `REQHARBOR_MAX_TEAM_INVITES` | `20` | Active invite links per team; `0` means no cap |

**Accepted for later releases, no effect yet:** `REQHARBOR_GOOGLE_CLIENT_ID`/`_SECRET`,
`REQHARBOR_OIDC_ISSUER`/`_CLIENT_ID`/`_CLIENT_SECRET`/`_NAME`/`_SCOPES`/`_REQUIRE_VERIFIED_EMAIL`,
`REQHARBOR_SMTP_HOST`/`_PORT`/`_USERNAME`/`_PASSWORD`/`_FROM`/`_TLS`,
`REQHARBOR_RATE_EMAIL_PER_HOUR`, `REQHARBOR_RATE_EMAIL_PER_RECIPIENT_PER_HOUR`,
`REQHARBOR_BACKUP_INTERVAL`, `REQHARBOR_BACKUP_KEEP` (scheduled backups). `REQHARBOR_DEV_AUTH`
exists only in test builds and is refused by the image.

## Operations

- **Health:** `/healthz` answers 200 as soon as the listener is up; `/readyz` reports the database,
  the migrations and free disk, and answers 503 until the server is ready. The image's
  `HEALTHCHECK` runs `reqharbor-server healthcheck`.
- **Logs:** JSON lines on stdout. One access-log line per request with the route pattern (never
  the raw path, so share ids and space ids stay out of logs), status, duration, size and request
  id; every response carries `X-Request-Id`. Query strings, `Authorization`, cookies and form bodies
  are never logged.
- **Background jobs:** the storage breaker every 10 seconds; expired sign-in requests, sessions and
  codes are deleted every 10 minutes (revoked or expired ones after 7 days), as are team invite
  links 30 days after they expired or were revoked, team departure records (removal notices) after
  31 days and expired signup tickets; `PRAGMA optimize` and `PRAGMA incremental_vacuum` once a day.
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

This section is the wire reference for the account routes (since 0.2) and the team routes (M2)
(06-go-server.md §4 and §10 "M2 as built" are the design). Every error is `{"error":{"code","message"}}`; every response is JSON with
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
| 403 | `forbidden_role` | The caller's role in a team space does not allow it (a viewer's push or share create; another member's share for an editor) |
| 426 | `client_too_old` | `/v1/spaces/*` only, when `REQHARBOR_MIN_CLIENT_VERSION` is set and `X-ReqHarbor-Version` is missing or lower; checked before auth. Also on **every** `/v1/spaces/{id}*` call to a **team** space when `X-ReqHarbor-Version` is missing or below `0.1.0` (the fixed team-space gate: apps older than M0 send no header and would drop fields of newer request kinds), checked after the space (404/403) |
| 429 | `rate_limited` | With `Retry-After`; `/v1/spaces/{id}/sync` allows `REQHARBOR_RATE_SYNC_PER_MIN` per session |
| 507 | `server_full` | A growing write while the storage breaker is open, or an open-signup account's write once `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` is reached |

Order on `/v1/spaces/*`: route (404), method (405), version gate (426), token (401), space (404,
then 403), the team-space version gate (426), the role (403 `forbidden_role`), then the body (415,
413, 400).

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
            "bytesUsed":1234,"quotaBytes":104857600,"memberCount":1,"ownerUserId":"01J…me"},
           {"spaceId":"01J…","kind":"team","teamId":"01J…","teamName":"Acme","role":"editor",
            "bytesUsed":5678,"quotaBytes":262144000,"memberCount":3,"ownerUserId":"01J…"}],
 "owner":{"bytesUsed":1234,"quotaBytes":null},
 "departed":[{"spaceId":"01J…","teamId":"01J…","teamName":"Globex","reason":"removed","at":1795435200000}]}
```

The authoritative list of the account's spaces (04 §6.7): the personal space first, then every
team space the account is a member of, oldest first. Team spaces are listed whatever
`REQHARBOR_TEAMS` says. `quotaBytes` is `null` when unlimited. `bytesUsed` counts entity data plus
active share snapshots. `memberCount` is 1 for the personal space. `ownerUserId` is the space's
billing owner (the caller for the personal space).

`owner` is the caller's own aggregate: their personal space plus every team they are billing owner
of, against `REQHARBOR_QUOTA_OWNER_BYTES` (`null` when off).

`departed` lists, per team, the latest departure from a team the account is **not** a member of now,
newest first, kept 31 days: `reason` is `removed` (by an admin or owner), `left`, or `deleted` (the
team was deleted; the deleter gets one too). Rejoining a team drops its entry. A space that is
missing from `spaces` and has no entry here was lost for another reason; treat it as removed.

### `GET /v1/spaces/{id}`

```json
{"spaceId":"01J…","kind":"personal","name":"Personal","createdAt":1795435200000,"entityCount":3,
 "bytesUsed":1234,"quotaBytes":104857600,"role":"owner"}
```

For a team space `kind` is `team`, `name` the team name and `role` the caller's role. Any member,
viewers included, may read the space, pull and list its shares.

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
- Applied changes record the caller as the author (`updatedBy`); a `stale` change keeps the stored
  author.
- **Roles (team spaces).** A viewer's push is 403 `forbidden_role` before the body is read, and
  nothing is stored. A `project` tombstone from an editor is the per-change result
  `{"status":"rejected","reason":"forbidden_role"}` while the rest of the push applies; admins and
  owners delete projects. Editors may delete every other type. The role is read again inside the
  push transaction and decides there: a member removed since the request was authorised gets 403
  `not_a_member`, one demoted to viewer 403 `forbidden_role`, and project tombstones follow the
  role at that moment.
- **Team spaces only.** An `environment` upsert whose `data.projectId` is not a non-empty string
  (the Global environment) is the per-change `global_env_not_allowed`; tombstones are not checked.
  For a `request`, `data.body.binaryPath` and the `value` of every `data.body.form[]` entry with
  `"kind":"file"` are blanked (`""`) before the size is measured and stored; keep the local values
  on pull, as for secrets. Personal spaces keep both.
- Precedence when several apply: `unsupported_type`, `clock_skew`, `forbidden_role`,
  `global_env_not_allowed`, then `invalid_json_value` and `entity_too_large`.
- The response adds `serverTime` (the server's clock, Unix ms):

```json
{"results":[{"type":"request","id":"r1","status":"applied","rev":8},
            {"type":"protoFile","id":"p1","status":"rejected","reason":"unsupported_type"},
            {"type":"request","id":"r2","status":"rejected","reason":"clock_skew"}],
 "cursor":8,"serverTime":1795435201234}
```

Quota: 413 `quota_exceeded` when the push grows the space past its quota
(`REQHARBOR_QUOTA_PERSONAL_BYTES` or `REQHARBOR_QUOTA_TEAM_BYTES`; message "Workspace storage quota
of N bytes exceeded …"), or grows the billing owner's aggregate past `REQHARBOR_QUOTA_OWNER_BYTES`
(message "Storage limit of N bytes for the spaces <owner name> owns exceeded …"). Deletes and
shrinking pushes always pass, also while 507 applies.

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
- **Roles (team spaces).** Every member lists. Create needs editor or above (403
  `forbidden_role` for a viewer, before the body). Update and revoke: editors their own shares
  (`createdBy` is them), admins and owners anyone's. The order is share id (404), the role (403),
  then revoked (410) and the body. The role is checked again inside the write transaction, so a
  removal or demotion that lands meanwhile refuses the write (403). Share writes count against the team and owner quotas like
  pushes.

The public `/s/{id}` and `/s/{id}.json` pages serve space shares exactly like legacy ones. Deleting
a team deletes its shares, so their pages answer 404.

### Teams: errors and conventions

Every `/v1/teams*` and `/v1/invites/*` route answers 403 `teams_disabled` while
`REQHARBOR_TEAMS=false`, before auth and before the body. Then the access token (401), then the
team (404 `team_not_found` for an unknown id **and** for any personal team id, which is never
addressable; 403 `not_a_member` when the caller is not in the team). After that each route has its
own order, given with it below: a minimum role that does not depend on the body (403
`forbidden_role`) comes before the body (415, 413, 400 `validation_failed`); checks that depend on
the body or on another member come after it, inside the write transaction, which also re-reads the
team and the caller's own role. These routes have no version gate. Roles rank viewer < editor < admin < owner; the team's **billing owner**
(`ownerUserId`) is always an owner.

| Status | Code | When |
|---|---|---|
| 400 | `validation_failed` | A malformed body (name, role, `maxUses`, …); an admin link that is not single-use; transfer to yourself |
| 403 | `teams_disabled` | `REQHARBOR_TEAMS=false` |
| 403 | `not_a_member` | The caller is not a member of this (existing, non-personal) team |
| 403 | `forbidden_role` | The caller's role does not allow it |
| 404 | `team_not_found` | Unknown team id, or a personal team's id |
| 404 | `member_not_found` | `{uid}` (or a transfer's `userId`) is not a member of this team |
| 404 | `invite_not_found` | `{iid}` is not an invite of this team |
| 409 | `last_owner` | The change would leave the team without an owner |
| 409 | `transfer_required` | The target is the billing owner: transfer ownership first |
| 409 | `limit_teams` | `REQHARBOR_MAX_OWNED_TEAMS` reached (the caller's on create, the receiver's on transfer; the message says which) |
| 409 | `limit_members` | `REQHARBOR_MAX_TEAM_MEMBERS` reached on accept |
| 409 | `limit_invites` | `REQHARBOR_MAX_TEAM_INVITES` active links reached |
| 410 | `invite_invalid` | Any unusable invite link (unknown, malformed, expired, revoked, used up, or created before the caller was removed from the team); always the same message |
| 413 | `quota_exceeded` | Transfer: the receiver's aggregate plus the team exceeds `REQHARBOR_QUOTA_OWNER_BYTES` ("Max doesn't have room for Acme: they need N more bytes") |
| 429 | `rate_limited` | More than 30 invite links created for a team in 24 hours; `/v1/invites/preview` and `/ticket` beyond `REQHARBOR_RATE_AUTH_PER_MIN` per client IP. With `Retry-After` |

The shapes used below:

```json
Team    {"teamId":"01J…","name":"Acme","spaceId":"01J…","role":"owner","ownerUserId":"01J…",
         "memberCount":3,"bytesUsed":5678,"quotaBytes":262144000,"createdAt":1795435200000}
Member  {"userId":"01J…","name":"Sam Lee","email":"sam@acme.dev","avatarUrl":null,"role":"editor",
         "joinedAt":1795435200000,"isBillingOwner":false}
Invite  {"inviteId":"01J…","role":"editor","createdAt":1795435200000,"expiresAt":1796040000000,
         "maxUses":1,"uses":0,"createdBy":{"userId":"01J…","name":"Priya Shah"}}
```

`Team.role` is the caller's role; `quotaBytes` is `null` when unlimited. `Invite.maxUses` is `null`
for an unlimited link; `createdBy` is `null` once that account is deleted, and its `name` is the
creator's current name.

### `GET`/`POST /v1/teams`

`GET` lists the non-personal teams the caller belongs to, by name:

```json
{"teams":[Team…],"ownedCount":1,
 "limits":{"ownedTeams":10,"teamMembers":100,"teamInvites":20},
 "owner":{"bytesUsed":6912,"quotaBytes":null}}
```

`ownedCount` is how many teams the caller is billing owner of; a `limits` value is `null` when that
limit is off (`0`); `owner` is the caller's aggregate as in `/v1/me/spaces`.

`POST {"name":"Acme"}` creates a team, the caller's owner membership and the empty team space in
one transaction: `201 Team`. `name` is 1–80 characters (UTF-16 code units) after trimming, without
control characters. 409 `limit_teams` ("You can own up to 10 teams on this server") once the
caller owns `REQHARBOR_MAX_OWNED_TEAMS` teams.

### `GET`/`PATCH`/`DELETE /v1/teams/{id}`

- `GET`: any member, `200 Team`.
- `PATCH {"name"}`: admin or above (403), then the body (400); renames the team and its space.
  `200 Team`. Names are 1–80 characters with no control or format characters (such as the bidi
  override U+202E).
- `DELETE` (no body): the billing owner only (403 for everyone else, co-owners included). Every
  member, the caller too, gets a `deleted` departure; the memberships, invite links, signup
  tickets, the space, its entities and its shares are deleted (share pages answer 404). `204`.

### `GET /v1/teams/{id}/members`, `PATCH`/`DELETE /v1/teams/{id}/members/{uid}`

`GET`: any member. `{"members":[Member…]}`, owners first, then admins, editors, viewers, each by
join time.

`PATCH {"role":"owner"|"admin"|"editor"|"viewer"}` changes a member's role. Rule (AC-TEAM-3, 04
§5.4): **owners** change anyone to any role; **admins** change only editors and viewers and grant at
most admin; **anyone** may lower their own role. Checked in order: the body (400 for a missing or
unknown role), then inside the transaction the member (404 `member_not_found`), the rule (403), an
unchanged role (`200`, nothing written), the last owner (409 `last_owner`), the billing owner (409
`transfer_required`, whoever asks, the billing owner too). A
demotion from admin or owner to editor or viewer revokes every invite link that member created in
this team, in the same transaction. `200 Member` (the new role).

`DELETE` removes a member; the body is optional: `{"revokeInvites": true|false}` (default false;
no body or `Content-Length: 0` is fine). Rule: owners remove anyone else; admins only editors and
viewers. Checked in order: your own id (403, use leave), a caller below admin (403), the body (400),
then inside the transaction the member (404), the rule (403), the last owner (409), the billing
owner (409 `transfer_required`). One transaction records a `removed` departure
(the member's next request to the space is 403 `not_a_member`; invite links created before now no
longer let that account join), deletes the membership, revokes **every link the member created**
in this team whatever the body says, and with `revokeInvites:true` every other active link too.
Response `200 {"userId":"01J…","revokedInvites":2}`.

### `POST /v1/teams/{id}/transfer`

`{"userId":"01J…"}`: the billing owner hands the team to another member, who becomes owner and
billing owner; the caller becomes admin. Checks: the caller is the billing owner (403), the body
(400, also for your own id), then inside the transaction the member (404 `member_not_found`), the receiver's owned teams (409 `limit_teams`,
"Max already owns 10 teams, the most this server allows"), the receiver's aggregate (413
`quota_exceeded` with the shortfall). Atomic with every other team write, so a removal racing it
either runs first (the transfer is 404) or finds the caller no longer owner (403). `200 Team` (from
the caller's side: `role` is `admin`).

### `POST /v1/teams/{id}/leave`

No body. Any member except the last owner (409 `last_owner`) and the billing owner (409
`transfer_required`). Records a `left` departure (it does not block rejoining with any valid link)
and revokes the links the leaver created in this team. `204`.

### `GET`/`POST /v1/teams/{id}/invites`, `DELETE /v1/teams/{id}/invites/{iid}`

Admin or above (403 for editors and viewers, for all three).

`GET`: the active links (not revoked, not expired, not used up), newest first:
`{"invites":[Invite…]}`. Tokens are never listed.

`POST {"role","maxUses","expiresInDays"}`, every member optional: `role` `admin`, `editor` (default)
or `viewer`; `maxUses` 1 (default) to 10000, or `null` for unlimited; `expiresInDays` 1 to 30
(default 7). Order: a caller below admin (403), the body (400, including "Admin links are
single-use" for an `admin` link without `maxUses: 1`; a database CHECK is the backstop), then
inside the transaction an `admin` link from a non-owner (403). 409 `limit_invites` at `REQHARBOR_MAX_TEAM_INVITES`
active links, 429 after 30 creates for the team in 24 hours. Response `201`, the only time the
token is shown:

```json
{"inviteId":"01J…","role":"editor","createdAt":1795435200000,"expiresAt":1796040000000,"maxUses":1,
 "uses":0,"createdBy":{"userId":"01J…","name":"Priya Shah"},
 "token":"rhi_…43 chars","url":"https://rh.example.com/i#rhi_…"}
```

`url` is `REQHARBOR_PUBLIC_URL + "/i#" + token`. Tokens are `rhi_` + 43 base64url characters; only
their SHA-256 is stored.

`DELETE` (no body) revokes a link of this team: `200 {"inviteId","revokedAt"}`, idempotent (an
already revoked link returns its original `revokedAt`). Another team's invite id is 404
`invite_not_found`.

### `POST /v1/invites/preview`

No auth. `{"token":"rhi_…"}` → `200 {"teamName":"Acme","inviterName":"Priya Shah","role":"editor","expiresAt":1796040000000}`
(`inviterName` is `null` when the creator's account is gone). Nothing else about the team is
revealed. Any unusable link is 410 `invite_invalid`. Rate-limited per client IP.

### `POST /v1/invites/accept`

Access token. `{"token":"rhi_…"}`:

```json
{"alreadyMember":false,"teamId":"01J…","teamName":"Acme","spaceId":"01J…","role":"editor"}
```

A member of the link's team gets `200` with `"alreadyMember":true` and their **current** role,
unchanged (even for an expired link). Otherwise, in one transaction: the link must be usable and
must not predate a removal of this account from the team (both 410 `invite_invalid`), the team must
have room (409 `limit_members`, "Acme has reached 100 members"); then the membership with the link's
role and one use. Concurrent accepts of a single-use link: exactly one wins, the others get 410.
The team space then appears in `/v1/me/spaces`.

### `POST /v1/invites/ticket`

No auth. `{"token":"rhi_…"}` → `200 {"ticket":"rht_…43 chars","expiresAt":1795435800000}` for a
usable link (410 `invite_invalid` otherwise); rate-limited per client IP. A link with `maxUses`
opens at most that many signups: a ticket is minted, and later accepted at signup, only while the
link's uses plus the tickets already spent on signups are below `maxUses` (accepting the link
after such a signup still works, as it only needs a use). The ticket lets a person
without an account sign up while `REQHARBOR_SIGNUP=invite`: add it to the sign-in URL,
`/auth/authorize?…&ticket=rht_…`, so the invite token itself never appears in a URL. While the
ticket is unused, unexpired (10 minutes) and its link still usable, the signup form asks for no
invite code, and provider sign-ins (GitHub, dev) of new identities need none; the account is
created as invited (`signup_via` `invite`) and the ticket is spent. An invite code entered anyway
takes precedence. A used, expired or revoked ticket is ignored (the page then asks for a code, as
without one); a malformed `ticket` makes the authorize page refuse the request. Tickets count in
`invite` mode only: in `open` mode the ticket is ignored and not spent (the account is an ordinary
open signup, counted in `REQHARBOR_OPEN_SIGNUP_TOTAL_BYTES` and refused while that is full), and
`closed` signup stays closed. The ticket only opens signup: joining is still `POST /v1/invites/accept` after
sign-in.

### `GET /i`

The hosted invite page for `https://rh.example.com/i#rhi_…`, served whatever `REQHARBOR_TEAMS`
says. It is static: an inline script (allowed by its hash) reads the token from the fragment and
calls `POST /v1/invites/preview` on the same origin, then shows "<inviter> invited you to join
<team> as <role> · expires <date>", "Open ReqHarbor › Teams › Join team and paste this link." and
a Copy link button; an invalid link shows "This invite link is no longer valid. Ask the person who
sent it for a new one." Without JavaScript only the instructions show, with no team details.
Headers: `Content-Security-Policy: default-src 'none'; style-src 'sha256-…'; script-src
'sha256-…'; connect-src 'self'; img-src data:; form-action 'none'; frame-ancestors 'none';
base-uri 'none'`, `X-Robots-Tag: noindex, nofollow`, `Cache-Control: no-store`, no cookies.

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

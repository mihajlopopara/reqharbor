# ReqHarbor: authentication research (login, sync, team collections)

Investigation only. No repo files were modified. Facts dated to Nov 2026.

## 0. What exists today (constraints that shape the choice)

- **Worker auth is a single bearer token per workspace.** `worker/src/workspaces.ts` `authenticate()` maps `Bearer rh_…` to the SHA-256 hash in `workspaces.token_hash`. Rotation is an atomic compare-and-swap (`rotateToken`). Creation is rate-limited by a D1 fixed-window counter (`rate_limits` table, `enforceCreateRateLimit`).
- **No runtime npm dependencies in the Worker.** `worker/package.json` has devDependencies only. It has a hand-rolled router (`worker/src/index.ts`) and hand-rolled crypto (`worker/src/crypto.ts`: ULIDs, base64url tokens, `sha256Hex`, `timingSafeEqual`). Server-rendered HTML with a strict CSP already exists (`share-page.ts`). A small, dependency-light auth module fits the house style.
- **Sync engine.** `worker/src/sync.ts` treats entity `data` as opaque JSON, apart from stripping secret variable values (`sanitize.ts`). `SYNC_TYPES` is a server-side allowlist. Last-write-wins (LWW) and `rev` are per workspace. This means a **team collection space can simply be a team-owned `workspace` row**, and the sync engine stays unchanged.
- **Client.** `crates/core/src/sync.rs` stores `SyncConfig { worker_url, remote_workspace_id, token, cursor }` in plaintext in the workspace JSON. All `/v1/*` calls come from Rust, so no CORS is involved. AC-SYNC-7 (a 401 disconnects sync) is in `docs/PRODUCT.md §5.10`. `PRODUCT.md §2.2` already lists "OS keychain for tokens" and "deep link `reqharbor://`" as Later items.
- **Tauri plugins.** `src-tauri/Cargo.toml` has dialog, clipboard-manager and single-instance. It has no opener, deep-link or keyring support.
- **Product ethos.** "No account required" (§2.1 item 11), no telemetry SDKs (§7), and a self-hostable Worker (placeholder `database_id` in `worker/wrangler.toml`). Accounts must be optional. Self-hosters must be able to enable whichever login methods they configure.

## 1. Login method options

| # | Option | Security | Effort on Workers + D1 | Desktop UX | Cost | Lock-in | Maintenance status (2026) |
|---|---|---|---|---|---|---|---|
| 1 | **OAuth/OIDC (GitHub + Google), with the Worker acting as broker**: the system browser, PKCE and a loopback redirect to the app | High. Providers handle MFA. Public-client PKCE is required by RFC 9700. | Low to medium. About 2 provider clients plus a tiny first-party authorize/token endpoint. No password storage. | Excellent: "Continue with GitHub" opens the browser, then returns to the app automatically. | Free | Low. Standard protocols; user IDs are ours. | GitHub added PKCE (Jul 2025). Its loopback `redirect_uri` may use any port. Google "Desktop" clients keep loopback. |
| 1b | **OAuth device authorization grant (RFC 8628)** | Medium. Phishing-prone; GitHub made it opt-in for that reason. | Medium | Worse: the user types a code | Free | Low | GitHub supports it (opt-in, no secret). Better-auth has a plugin. |
| 2 | **Email one-time code (OTP)** (preferred over a magic link) | Medium-high, if attempts are rate-limited and codes are short-lived | Low. One table plus an email sender. | Good: works for people without GitHub or Google | Cloudflare Email Service: Workers Paid ($5/mo) needed for arbitrary recipients, 3,000/mo included, then $0.35/1k (public beta Apr 2026). Resend: free 3,000/mo, capped at 100/day. | Low (pluggable sender) | Cloudflare Email Sending is still beta |
| 3 | **Passkeys/WebAuthn inside the Tauri webview** | Highest | High | Broken on some platforms. WKWebView restricts WebAuthn to browsers or entitled apps. WebKitGTK (Linux) has no WebAuthn support. Only WebView2 works. | Free | Low | Not viable in the webview. **Viable later on the Worker's hosted login page in the system browser**, where it works on every OS. |
| 4 | **Email + password** | Medium. Needs hashing, resets and breach handling. | Medium-high. Workers WebCrypto caps PBKDF2 at 100k iterations in production, below OWASP's 600k for SHA-256. Argon2 via WASM burns CPU (10 ms on the free plan). A reset flow needs email anyway. | OK | Free | Low | Not recommended |
| 5a | **better-auth** (self-hosted library) | Good, widely used | Medium. D1 through Kysely (`kysely-d1`, listed in the official docs). Plugins: `organization` (owner/admin/member, invitations, teams), `bearer`, `emailOTP`, `device-authorization`, `passkey`. Desktop loopback PKCE is not first-class: it needs the `oauthProvider` + `jwt` plugins. | Good | Free | Medium (its schema and conventions) | Active. Took over Auth.js in Sep 2025. Joined Vercel 7 Jul 2026 with a stated "framework and platform agnostic" commitment. |
| 5b | **OpenAuth (openauthjs)** | Fine | Medium. Storage is KV, not D1. | Good | Free | Low | README still says "beta". Repo moved from sst to anomalyco. Unclear cadence. **Avoid as a foundation.** |
| 5c | **Auth.js** | Fine | Poor fit: cookie and web-framework centric | n/a | Free | Medium | Maintained by the Better Auth team since Sep 2025, which recommends Better Auth for new projects |
| 5d | **Lucia / Arctic** | n/a | n/a | n/a | n/a | n/a | Lucia was deprecated in Mar 2025 and is now a learning resource. Arctic (OAuth clients) was **deprecated Jul 2026** and points to copy-paste examples. Both remain good blueprints for a hand-rolled design. |
| 5e | **cloudflare/workers-oauth-provider** | Good. OAuth 2.1, PKCE, hashed tokens, RFC 7009 revocation. | Medium. Requires a **KV** binding (`OAUTH_KV`). Leaves upstream login to app code. Aimed at MCP and third-party clients. | n/a | Free | Low | 1.x, active. Overkill for a single first-party client. |
| 5f | **Cloudflare Access / Zero Trust** | High | Low | Poor for desktop: needs cloudflared or service tokens. Designed for employees, not public sign-up. | Free up to 50 seats | High | Only fits as an optional self-host mode for private companies. Defer. |
| 5g | **Hosted: WorkOS AuthKit / Clerk / Auth0** | High | Low to medium. The Worker verifies JWTs via JWKS. | Good. All support native-app PKCE. | WorkOS free to 1M MAU. Clerk free 50k MRU (since Feb 2026). Auth0 free 25k MAU. | **High.** The vendor owns identities. Self-hosters would need their own vendor account. It conflicts with the "no third-party SDKs / self-host" ethos. | Active |

### Key reasoning

- **Have the Worker broker OAuth instead of the app talking to GitHub or Google directly.**
  - The Worker is a confidential client: secrets live in `wrangler secret`, and the callback is `https://<worker>/v1/auth/callback/<provider>`.
  - The desktop app does PKCE against the Worker, its own tiny first-party authorization server.
  - Benefits:
    - Provider quirks (Google's Desktop client type, GitHub's single callback URL) never reach the app.
    - Email OTP and, later, passkeys live on the same hosted page.
    - The app's flow is identical for every method.
  - This is the RFC 8252 pattern: system browser, PKCE, and a loopback IP literal `127.0.0.1` on any port.
- **Use OTP rather than a magic link.**
  - The code is typed into the same browser tab that holds the pending PKCE request.
  - A magic link may open in another browser or device and break the binding.
  - Corporate link scanners pre-fetch magic links and consume them.
- **Loopback rather than deep link for the first version.**
  - The Rust core can bind `127.0.0.1:0` with tokio. No plugin is needed.
  - Deep links need `tauri-plugin-deep-link` plus the single-instance `deep-link` feature, have Linux/AppImage registration caveats, and allow scheme hijacking.
  - Add a "copy this code into ReqHarbor" fallback on the final page for locked-down machines. Same PKCE exchange.
  - Deep links can come later, together with the share-import deep link from §2.2.

## 2. Session and token design for the desktop client

### Recommended: opaque tokens, no JWTs

Every sync call already reads D1, so stateless JWT verification saves nothing. It would only cost revocation.

**Tokens**
- **Access token**: `rha_` + 32 random bytes, valid 1 hour.
- **Refresh token**: `rhr_` + 32 random bytes, sliding 60 days, absolute 1 year, **rotated on every use**.
- RFC 9700 requires public-client refresh tokens to be either rotated or sender-constrained.
- Store only SHA-256 hashes, as `workspaces.token_hash` does today.

**Rotation**
- Rotate with an atomic compare-and-swap: `UPDATE sessions SET refresh_hash=?new, prev_refresh_hash=?old, access_hash=?, access_expires_at=? WHERE refresh_hash=?old AND revoked_at IS NULL RETURNING …`. This is the same pattern as `rotateToken`.
- If a presented refresh token matches some `prev_refresh_hash`, treat it as **reuse**: revoke that session (theft signal).
- Allow a ~30 s grace window for the "client crashed before persisting" race, or accept that the user must sign in again.

**Client side** (`crates/core/src/sync.rs`)
- Keep tokens in the **OS keychain** using the `keyring` crate:
  - macOS: Keychain
  - Windows: Credential Manager
  - Linux: Secret Service; it needs libdbus and is flaky when headless. Fall back to a 0600 file with a warning.
- `SyncConfig` keeps only the non-secret parts.
- Use a single-flight refresh mutex around requests. On 401: refresh once, retry, then apply AC-SYNC-7 (prompt sign-in; local data untouched).

**Revocation**
- A session row has `revoked_at`. Because every request looks up `access_hash`, revocation takes effect immediately.
- Endpoints: `GET /v1/me/sessions` (device list), `DELETE /v1/me/sessions/:id`, `POST /v1/auth/logout`.
- Removing a team member takes effect on their next request, because the access check runs per request. Document that their already-pulled local copy remains.

**Acceptable smaller alternative:** one opaque per-device session token with a 90-day sliding expiry, stored in the keychain, and no refresh. It is simpler, but you lose reuse detection. If the dev pass is time-boxed, start here: the schema below supports both.

## 3. Rate limiting on Workers

- **Workers Rate Limiting binding** (GA 19 Sep 2025). Periods are 10 s or 60 s. It is per Cloudflare location and eventually consistent, so it is approximate. Use it for coarse per-IP and per-session throttles:
  - `/v1/auth/*`, e.g. 20/min/IP
  - sync, e.g. 120/min/session
- **The existing exact D1 fixed-window table** (`rate_limits`, the same SQL as `enforceCreateRateLimit`). Use it for limits that must be strict:
  - OTP sends: 5/hour per email and 20/hour per IP bucket. Reuse `rateLimitBucket()` for IPv6 /64.
  - OTP verify: at most 5 attempts per code, then invalidate.
  - Invite creation per team.
- Store OTP codes as an HMAC with a Worker secret, plus the attempt cap. A 6-digit code space is too small for hashing alone to protect it.
- Turnstile on the email form is optional. Defer it.

## 4. Teams and invites (the usual model, mapped onto this schema)

Keep the sync engine. **A "team collection space" is a `workspaces` row owned by a team.** Each user gets an implicit personal team, so one access-control query covers everything.

Proposed migration `0003_accounts.sql` (sketch):

- `users(id, email UNIQUE COLLATE NOCASE, name, avatar_url, created_at, disabled_at)`
- `identities(provider, provider_user_id, user_id, email, PRIMARY KEY(provider, provider_user_id))`
  - Link accounts across providers **only on a verified email**: GitHub `/user/emails` primary + verified, Google `email_verified`. Otherwise this is an account-takeover vector.
- `auth_requests(id, redirect_uri, code_challenge, state, browser_binding_hash, user_id NULL, code_hash NULL, expires_at)` with a 10-minute TTL. The one-time code lives 60 s and is bound to the challenge and the redirect.
- `email_codes(id, auth_request_id, email, code_hmac, attempts, expires_at, consumed_at)`
- `sessions(id, user_id, device_name, access_hash UNIQUE, access_expires_at, refresh_hash UNIQUE, prev_refresh_hash, created_at, last_used_at, expires_at, revoked_at)`
- `teams(id, name, personal INTEGER, created_by, created_at)`
- `team_members(team_id, user_id, role CHECK (role IN ('owner','admin','editor','viewer')), PRIMARY KEY(team_id, user_id))`
  - Invariant: at least one owner.
- `team_invites(id, team_id, role, token_hash UNIQUE, email NULL, created_by, expires_at, max_uses, uses, revoked_at)`
  - Use invite **links** first. The user pastes the link into the app ("Join team"), mirroring AC-SHARE-5's "Import from link". Emailed invites are optional and require an email sender.
- `workspaces` changes:
  - Add `team_id NULL` (NULL means a legacy token workspace).
  - Make `token_hash` nullable. SQLite cannot drop `NOT NULL`, so this means a table rebuild in the migration.
- `entities`: add `updated_by TEXT` ("last edited by").

### Roles

The Postman-style "editor" role is needed. Better-auth's default "member" role is read-only, which does not fit.

| Role | Permissions |
|---|---|
| viewer | Pull only. Push returns 403. Can see the shares list. |
| editor | Push; create, update and revoke shares |
| admin | Manage members and invites, create or rename team workspaces |
| owner | Delete the team, transfer ownership |

Permissions apply per workspace, not per collection. Per-collection access control is deferred.

### Routes

Keep `/v1/*` token routes byte-for-byte for legacy and self-host use. Add user-scoped routes:

- `GET /v1/auth/providers`, so the app shows only the methods configured on that Worker
- `GET /v1/auth/authorize`, `GET /v1/auth/callback/:provider`, `POST /v1/auth/email/start|verify`, `POST /v1/auth/token` (code and refresh grants), `POST /v1/auth/logout`
- `GET /v1/me`, `GET|DELETE /v1/me/sessions`
- `GET|POST /v1/teams`, `/v1/teams/:id/members`, `/v1/teams/:id/invites`, `POST /v1/invites/:token/accept`
- `GET|POST /v1/w/:workspaceId/sync`, `/v1/w/:workspaceId/shares…`
  - Same handlers as today. Only `authenticate()` changes: it resolves session → user → `team_members.role` for the workspace's `team_id` in one joined query.
- `POST /v1/workspace/claim` (session plus legacy `rh_` token): moves an anonymous workspace into the user's personal team and nulls its token. Existing users migrate without re-uploading.

### Client implication to flag for the designer and dev passes

Today the store has one remote (`Workspace.sync`). Team collections need **multiple remote bindings**. Bind each local `Project` (it already carries `workspaceId`) to one remote workspace, with a per-binding cursor and sync state.

## 5. Forward compatibility with gRPC and GraphQL (not in scope, but must not be blocked)

- Put the request kind **inside the entity data**: `request.kind: "http" | "graphql" | "grpc"`, defaulting to `"http"` when missing. Do not create new sync types per kind. The Worker already treats `data` as opaque, so access control and auth are unaffected.
- gRPC will probably need a proto/descriptor entity. Extending the `SYNC_TYPES` allowlist in `worker/src/sync.ts` is a server-first deploy. Clients should ignore unknown types on pull.
- Large proto bundles may exceed the 512 KB entity limit. Store them in R2 later.
- `snapshot.ts` defaults `method` to "GET". The share page must render non-HTTP kinds gracefully. That is later work.

## 6. Cost and operations

- Everything runs on Workers Free with D1 Free. OAuth exchanges are network I/O and SHA-256 is cheap, so the 10 ms CPU limit is fine.
- Email OTP to arbitrary recipients needs either Workers Paid ($5/mo; Cloudflare Email Service, still beta) or a Resend API key (free tier 100/day).
- Make the sender pluggable: an `EMAIL` binding, else `RESEND_API_KEY`, else email login disabled.
- Providers are enabled by the presence of secrets: `GITHUB_CLIENT_ID`/`GITHUB_CLIENT_SECRET`, `GOOGLE_CLIENT_ID`/`GOOGLE_CLIENT_SECRET`.
- The app needs `tauri-plugin-opener` (or the `open` crate) to launch the system browser, and the `keyring` crate.
- No change to the `tauri.conf.json` CSP, because login happens in the system browser.

## 7. Defer

- Passkeys (later, on the hosted page)
- `reqharbor://` deep links
- Emailed invites (links first)
- SSO/SAML (WorkOS could be added later as just another upstream "provider" on the broker)
- Cloudflare Access trusted-header mode for self-hosters
- Device-code grant
- Turnstile
- Audit log
- Per-collection access control
- Real-time collaboration
- Billing

**Do include in v1:** a minimal "delete my account" endpoint, and the claim flow for legacy workspaces.

## RECOMMENDATION

**Primary: build a small first-party auth module in the existing Worker. The Worker acts as an OAuth broker and as ReqHarbor's own minimal PKCE authorization server.**

- **Login methods:** GitHub and Google OAuth with the Worker as a confidential client, plus email one-time codes, with the sender pluggable between Cloudflare Email Service and Resend.
- **Desktop flow:** open the system browser at `/v1/auth/authorize` with PKCE S256 and a `127.0.0.1:<ephemeral>` loopback redirect. Fall back to a code the user pastes into the app.
- **Tokens:** opaque, hashed in D1. Access tokens last 1 hour; refresh tokens rotate with reuse detection. Tokens live in the OS keychain via `keyring`. Sessions can be listed and revoked.
- **Teams:** `teams` / `team_members` (owner, admin, editor, viewer) / `team_invites` (link-based), plus `workspaces.team_id`. Team collections reuse the existing sync engine unchanged.
- **Rate limits:** the Workers Rate Limiting binding for coarse throttles; the existing D1 `rate_limits` table for exact OTP and invite limits.
- **Compatibility:** legacy `rh_` workspace tokens keep working, and `POST /v1/workspace/claim` upgrades them.

Reasons:

1. **Smallest secure design for this stack.** It needs no password handling and avoids the PBKDF2 100k cap. It adds no runtime dependencies, which matches `worker/package.json` and `crypto.ts`. It follows RFC 8252 and RFC 9700 (PKCE, rotation).
2. **Best desktop UX on all three operating systems.** The browser hosts the login UI, which sidesteps WebView WebAuthn and cookie limits and leaves room for passkeys later.
3. **Keeps ReqHarbor's ethos.** Accounts are optional, self-hosters configure only the providers they want, and no third-party identity vendor or telemetry is involved.
4. **The libraries most people would reach for are in flux.** Lucia and Arctic are deprecated (Arctic in Jul 2026), OpenAuth is still beta and KV-based, and Auth.js is in maintenance mode. Their published code examples are good blueprints to copy, not dependencies to take.
5. **Team collections stay cheap.** A team is an owner of existing workspaces, so the LWW sync, quotas, shares and secret stripping all keep working.

**Fallback: better-auth on the same D1, through `kysely-d1`.**

- Plugins: `organization` with teams and custom roles adding an editor role, `bearer` (or `oauthProvider` + `jwt` for proper desktop PKCE), `emailOTP`, and later `passkey`.
- Choose it if the team wants many providers, 2FA or passkeys quickly and accepts a sizable dependency plus its schema conventions.
- It is actively maintained (now backed by Vercel, with a stated platform-agnostic commitment).
- Caveats: it is web and cookie oriented; its device-authorization CLI flow is described as a "demo feature"; migrations must be generated and fed into `worker/migrations`.

**Not recommended:**
- **Hosted IdPs (WorkOS AuthKit is the best of them, free to 1M MAU):** use only if the project abandons self-hosting.
- **Cloudflare Access:** suitable only for private self-host deployments.
- **Email + password, and in-webview passkeys:** not recommended.

## Sources

- RFC 9700 (OAuth 2.0 Security BCP, PKCE required for public clients, refresh rotation or sender-constraining): https://www.rfc-editor.org/rfc/rfc9700
- OWASP OAuth2 cheat sheet: https://cheatsheetseries.owasp.org/cheatsheets/OAuth2_Cheat_Sheet.html
- RFC 8252 native-app redirect guidance (summary): https://www.oauth.com/oauth2-servers/oauth-native-apps/redirect-urls-for-native-apps/
- GitHub PKCE support (Jul 14, 2025): https://github.blog/changelog/2025-07-14-pkce-support-for-oauth-and-github-app-authentication/
- GitHub authorizing OAuth apps (loopback port need not match; device flow opt-in, no secret): https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps
- GitHub device flow made opt-in (2022): https://github.blog/changelog/2022-03-16-enable-oauth-device-authentication-flow-for-apps
- Google OAuth for desktop apps (public client, always PKCE): https://developers.google.com/identity/protocols/oauth2/native-app
- Google loopback migration guide (desktop clients unaffected): https://developers.google.com/identity/protocols/oauth2/resources/loopback-migration
- Tauri deep-link plugin (Windows/Linux need single-instance): https://docs.rs/crate/tauri-plugin-deep-link/2.4.8/source/README.md
- tauri-plugin-oauth (localhost server, v2.0.0, Nov 2024): https://docs.rs/crate/tauri-plugin-oauth/2.0.0/source/README.md
- WKWebView WebAuthn restrictions: https://developer.apple.com/forums/thread/714785 and https://developer.apple.com/forums/thread/727613
- WebKitGTK WebAuthn request: https://www2.webkit.org/show_bug.cgi?id=205350
- Cloudflare Email Sending public beta (Apr 16, 2026): https://developers.cloudflare.com/changelog/post/2026-04-16-email-sending-public-beta/
- Cloudflare Email Service pricing (3,000 included; $0.35/1k; Workers Paid for arbitrary recipients): https://developers.cloudflare.com/email-service/platform/pricing/
- Resend free tier (3,000/mo, 100/day): https://resend.com/docs/knowledge-base/what-is-resend-pricing
- Workers PBKDF2 100k cap: https://github-cdn.pages.dev/cloudflare/workerd/issues/1346 and https://lord.technology/2024/02/21/hashing-passwords-on-cloudflare-workers.html
- Workers Rate Limiting GA (Sep 19, 2025): https://developers.cloudflare.com/changelog/2025-09-19-ratelimit-workers-ga/
- Rate Limiting binding docs (10/60 s periods, per-location, approximate): https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/
- Better Auth organization plugin: https://better-auth.com/docs/plugins/organization
- Better Auth device authorization: https://better-auth.com/docs/plugins/device-authorization
- Better Auth database / D1 via Kysely: https://better-auth.com/docs/concepts/database
- Better Auth joins Vercel (Jul 7, 2026): https://better-auth.com/blog/better-auth-joins-vercel
- Auth.js now part of Better Auth: https://better-auth.com/blog/authjs-joins-better-auth and https://github.com/nextauthjs/next-auth/discussions/13252
- Lucia deprecation: https://github.com/lucia-auth/lucia
- Arctic deprecated (Jul 2026): https://arctic.pilcrowonpaper.com/
- OpenAuth (README "currently in beta", KV storage): https://github.com/anomalyco/openauth
- Cloudflare workers-oauth-provider (OAuth 2.1, KV, PKCE, revocation): https://github.com/cloudflare/workers-oauth-provider
- Cloudflare Zero Trust free plan, 50 users: https://blog.cloudflare.com/teams-plans
- WorkOS pricing (free 1M MAU): https://workos.com/pricing
- Clerk pricing (50k MRU free since Feb 2026): https://clerk.com/articles/clerk-pricing-explained
- Auth0 vs WorkOS pricing (Auth0 free 25k MAU): https://workos.com/blog/auth0-vs-workos-pricing-comparison
- keyring crate (macOS Keychain, Windows Credential Manager, Linux Secret Service caveats): https://docs.rs/crate/keyring/3.6.3/source/README.md

Repo files cited:
- worker/src/workspaces.ts
- worker/src/index.ts
- worker/src/crypto.ts
- worker/src/sync.ts
- worker/src/sanitize.ts
- worker/src/snapshot.ts
- worker/migrations/0001_init.sql
- worker/migrations/0002_device_id_and_quota.sql
- worker/wrangler.toml
- worker/package.json
- crates/core/src/sync.rs
- src-tauri/Cargo.toml
- src-tauri/tauri.conf.json
- docs/PRODUCT.md
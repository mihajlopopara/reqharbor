# QA review (round 2)

**Verdict:** revise — open issues below must be resolved during implementation.

## Open issues

| Owner | Severity | Issue | Fix |
|---|---|---|---|
| po | major | The three documents still disagree on v1 behaviour, and the disagreements change what QA tests. (1) DEV-3: product AC-AUTH-4, ruling R4 and UX §3/§13 (paste view, `signin-paste` test id, code on the done page, the `oob` page) all specify paste-code in v1, but tech §2.2 makes v1 loopback-only and moves paste-code to M4. (2) DEV-5: product AC-PROJ-4 and edge case 2 say a move keeps the same entity ids and rebinds on other devices, but tech §6.5 moves with new ids. Product §0 still credits the same-id move, and AC-TSHARE-4 implies the moved copy can 'Update share'; under DEV-5 it cannot. (3) DEV-6: product D5/AC-TEAM-4 and UX §6.9 default to Remove after an admin removal; tech defaults to Keep everywhere, including the 7-day expiry. (4) DEV-7: AC-AUTH-7 promises the macOS Keychain, but every current release is ad-hoc signed (`src-tauri/tauri.conf.json` signingIdentity "-", `desktop-build.yml` L7), so all macOS users get the file store with different copy. (5) DEV-1/DEV-2: product §7 lists `/v1/auth/authorize`, `/v1/s/:spaceId` and a nullable `token_hash` table rebuild; UX §13 labels the page `/v1/auth/authorize`; tech uses `/auth/*`, `/v1/spaces/:id` and sentinel hashes. The rebuild would cascade-delete data (verified: `entities` and `shares` use `ON DELETE CASCADE` in `0001_init.sql`). Until these are ruled on, AC-AUTH-4, AC-PROJ-4, AC-TEAM-4 and AC-AUTH-7 cannot be tested. | PO adds rulings R9–R13 to §16.1 accepting DEV-1, 2, 3, 5, 6 and 7 (all are safer than the current spec text). Then rewrite AC-AUTH-4 as M4, AC-PROJ-4 and edge case 2 for new ids (the other device sees the source removed and a new team project, with no rebind), AC-TSHARE-4 ('existing links keep working and stay revocable under the source space; they cannot be updated from the moved copy'), D5 (default Keep), and AC-AUTH-7 (file store on ad-hoc macOS builds). Delete the rebuild note from §7. Designer applies tech §14: drop the paste view and code, add the loopback-bind error, focus Keep, add the Unavailable row and the 'Transfer ownership first' state. |
| dev | major | Invite abuse after removal or demotion. Rejoin blocking keys on `user_id` (`team_departures` plus the `d.at >= i.created_at` check in invite accept), and invites outlive the person who made them. A malicious admin or owner who is removed or demoted keeps every link they created: single-use admin links if they were owner, and up to 20 active editor links. They can use those links from a second account (another provider or email; one identity per account makes alt accounts trivial) or hand them out. The 'Also revoke all active invite links' checkbox defaults on only when a multi-use link exists, so a removed owner's pre-minted single-use admin link survives by default. Product D21 and PRIV-8 say the rejoin hole is closed, which overstates it. | In the same server batch as remove and as any demotion below admin, revoke `team_invites WHERE team_id=?1 AND created_by=?target AND revoked_at IS NULL`, whatever the checkbox says. Default the checkbox on whenever any active link exists, not only multi-use ones. Restate PRIV-8 and the remove-confirm copy honestly: the block applies to this account; revoking links is what stops other accounts. Add tests: a removed admin's own links return 410; a demoted admin's links return 410; an alt account with a link created before the removal returns 410 once links are revoked. |
| dev | major | Moving a project out of a team, or deleting a team project, silently loses teammates' unsynced work and leaves orphans on the server. Tech §6.5 and §13 keep today's rule that a delete beats an older edit. A teammate's offline edits older than the project tombstone are dropped. Newer ones get pushed as child upserts under a tombstoned project; B3 then ignores them on every device, so they are invisible yet still count against the team and owner quota forever. Product edge case 3 offers 'Save as personal copy' for removal, but there is no equivalent when someone else moves or deletes the project. That contradicts G7 ('no silent data loss') in exactly the multi-user case teams create. | When a pulled `project` tombstone (or a tombstone of a collection or folder with dirty descendants) hits local dirty or frozen descendants, freeze those keys and show the AC-ROLE-3-style banner: '<Name> moved or deleted <Project>. You have N unsynced changes. [Save as personal copy] [Discard]'. collect_push must never push a child whose project is tombstoned or unbound. Add an AC to the product spec and a core test: teammate offline edit plus admin move-out leads to the banner, no orphan pushes, and recovery via personal copy. |
| dev | minor | The team cap and transfer logic count the wrong set. Tech §5.3 enforces `MAX_OWNED_TEAMS` with `owner_user_id = me`, which includes the hidden personal team (DEV-4), so users get only 2 real teams instead of AC-TEAM-1's 3. Transfer (§5.4) checks only the quota aggregate, not the receiver's owned-team count, so transfers can push a user past 3 owned teams. | Add `AND personal = 0` to the owned-team count, and apply the same cap check to transfer (409 `limit_teams`, naming the receiver). Test: a fresh account can create exactly 3 teams, and a transfer to a user who already owns 3 is refused. |
| dev | minor | Clock-skew restamp (tech §6.9) can silently overwrite a teammate's edit. Entities rejected for skew carry future `updatedAt` values, so they win locally against any remote edit pulled in between. Restamping them to `now` and pushing then overwrites the teammate's newer change with no conflict copy. | Before restamping, compare each entity against the last pulled remote version. If the remote changed since `synced_at`, run the normal conflict-copy path instead of restamping. Add a core test for this case. |
| dev | minor | The 'sign in as a different account' path (product AC-AUTH-11, UX §4.3 SpaceNoticeDialog in different-account mode) has no design in tech. `account.json` holds one account, and `projectSpaces` still points at the previous user's space ids. The server would return 403 for those (so they go Unavailable), but D11 'new projects default to Personal' and the first-sync checklist could then offer to upload the previous user's team projects into the new user's personal space. | Specify it in tech §6.1/§7. On a user-id change, every existing binding becomes `Paused{foreignAccount}`. Those projects are excluded from the first-sync checklist and from push until the SpaceNoticeDialog choice: Keep makes them unbound local-only copies, Remove calls forget_project. Keychain entries for the old `session:<host>:<userId>` are deleted. Core test: A's team project never reaches B's personal space. |
| po | minor | The IP-based limits fit badly with team onboarding behind a shared office NAT. One sign-in makes about 6 requests (providers, authorize, start, callback, done, token) against `AUTH_LIMITER` at 20/min per IP, and `/v1/invites/preview` shares that limiter. New-account creation is 10/h per IP, and it shares a bucket with legacy workspace creation. Fifteen colleagues onboarding from one office in an hour will be refused. | Give account creation its own bucket (for example 30/h), and limit `/auth/*` per auth request plus per IP at a higher ceiling (about 60/min). Keep the D1 token-failure limit as the brute-force control. Document the vars for self-hosters. |
| po | minor | The admin-only project delete is cosmetic. Editors may tombstone every collection, folder and request (role table row 4), so an editor can empty a team project while AC-ROLE-2 blocks only the `project` tombstone. The spec presents the admin-only rule as protection. | Either say plainly in §6.6 that editors can delete any content and that admin-only applies to removing the project container, or add a server rule: a single push tombstoning more than N entities in a team space needs admin. Prefer the plain statement for v1; D1 Time Travel remains the backstop. |
| dev | minor | Loopback and returning-user details need tightening. (a) GitHub re-approves a previously authorised OAuth app silently, so on a shared or multi-user machine a crafted authorize URL (attacker-chosen challenge and port) completes in one click, and a local listener on that port gets a usable code. (b) The 64-connection cap in §2.2 lets any web page in the user's browser exhaust the listener with repeated fetches to 127.0.0.1:P. (c) Updating the account email on a returning sign-in does not re-check `verified`. | (a) The hosted sign-in page shows device name plus start time and 'Only continue if you just clicked Sign in in ReqHarbor'. Ask `prompt=consent` / `select_account` for Google. (b) Cap concurrent connections, not total, and keep accepting until the matching `state` or the timeout. (c) Update `users.email` only from a verified provider email. Add tests for (b) and (c). |
| dev | minor | `client_versions` and the new legacy `GET /v1/workspace/clients` are scope creep. They add one D1 write statement to every legacy push and a new legacy route, which strains the 'legacy byte for byte unchanged' promise, and nothing in v1 consumes them. | Defer to the gRPC/GraphQL work, or record versions only on `/v1/spaces/*` pushes. Keep the `X-ReqHarbor-Version` header and the 426 gate. |
| dev | minor | The breaker (`capacity.ts`) depends on a per-isolate cache of `meta.size_after` that is not yet verified on reads, and cold-isolate behaviour is undefined. The first growing write after a cold start could pass with no size known, or every isolate could add a `SELECT 1` per minute. | Define fail-open behaviour with a bounded staleness. Verify `size_after` on reads in miniflare in M1b, and add a test covering a cold isolate above the limit. |

## Test plan

### Unit: Rust core (`docker compose run --rm dev cargo test --workspace`)
- **M0 forward-compat:** proptest round-trip of unknown fields at every level (Workspace, SyncState, Project…RequestSettings, Auth, Body, FormField). Also:
  - `save_request` keeps `extra` when the UI rebuilds the object.
  - An unknown `kind` or `BodyType::Other` request is not editable, `write_guard` returns `read_only_newer`, and it is never pushed modified.
  - A pull page with an unknown `type` skips that change and applies the rest.
  - `minClientVersion` greater than the running version leads to read-only.
- **Push results:** a rejected tombstone is retained; `clock_skew` and `forbidden_role` keep the entity dirty; `entity_too_large` is skipped as today.
- **Binding invariants:**
  - B2: a known id from another space is ignored and the binding is unchanged.
  - B3: a foreign parent, or an existing foreign id, is not relocated. Regression for the cross-project `retain` at `crates/core/src/sync.rs` L1040.
  - B4: the Global environment goes only to `globalSpace`.
  - B5: a tombstone affects only its own space.
  - B7: a partial run defers deletes, and the 3-run cutoff then applies them.
  - A local-only project produces zero HTTP calls (wiremock asserts none).
- **Moves:** persisted plan; resume after a crash with the same ids; cancel tombstones whatever was already applied; source shares untouched. New: a teammate's dirty children under a pulled project tombstone freeze and show the banner, with no orphan push.
- **Different account:** A's bindings are paused and never pushed to B's spaces; the old keychain entry is removed.
- **Clock skew:** restamp happens only when the clock is fixed, and a remote change since `synced_at` produces a conflict copy, not an overwrite.
- **Auth:**
  - Loopback ignores stray requests and a wrong `state`, survives more than 64 sequential junk connections, and completes on the matching `/cb`; timeout and cancel work.
  - Concurrent 401s cause a single refresh.
  - Persist-before-use ordering.
  - A failed refresh gives `SignInRequired` with data and bindings intact.
  - The file store is 0600.
  - Tokens never appear in `workspace.json`, `account.json`, logs or `SyncStatus` (grep the serialised output).
- **sync_policy:** Disconnect only for a legacy 401.

### Worker integration (vitest plus miniflare, `docker compose run --rm -w /app/worker dev npm test`)
- **Regression:** `sync.test.ts`, `shares.test.ts`, `http.test.ts` and `qa_parity_security.test.ts` pass unchanged. Golden-response snapshots of every legacy route before and after the migrations.
- **Auth:**
  - PKCE happy path; wrong verifier; code reuse; expired code; a failed exchange burns the code.
  - `redirect_uri` matrix: `localhost`, `https`, `oob`, port below 1024, an extra path, a trailing slash.
  - Refresh rotation; the grace window returns the identical pair within 60 s; reuse after that revokes.
  - Logout; 20-session cap.
  - `email_in_use` creates no rows; no verified email; returning-user email update only from a verified email.
  - `Set-Cookie` appears only under `/auth/*`; a `/v1` handler ignores a forged cookie; a callback without the matching browser cookie is rejected (login-CSRF).
- **Spaces:**
  - A personal space is created at sign-in.
  - Another user's space returns 403 `not_a_member`; an unknown id returns 404; a legacy id via `/v1/spaces` returns 404.
  - An `rh_` token cannot reach an account space, and a `space:` sentinel is never matched by `authenticate()`.
  - Push with 14 rows reaches exactly 100 bound parameters (regression for the D1 limit).
  - Per-change `clock_skew`; `updatedBy` and `serverTime` present.
  - Quotas: personal, team and owner aggregate. Breaker: 507 on growth, deletes and shrinking pushes allowed, cold isolate covered.
- **Teams and roles:**
  - Table-driven actor × target × requested-role matrix for PATCH and DELETE members.
  - `transfer_required` for the billing owner; `last_owner`; transfer racing a removal leaves at least 1 owner.
  - Transfer to a user who owns 3 teams returns `limit_teams`; the personal team is not counted.
  - Viewer push returns 403; an editor's project tombstone gets a per-change `forbidden_role`; `global_env_not_allowed`.
  - `binaryPath` is stripped in team spaces and kept in personal ones.
  - Editor own-share versus admin any-share update and revoke.
- **Invites:**
  - Parallel accepts of a single-use link: exactly one wins.
  - A pre-removal link returns 410 for the removed user, and a post-removal link works.
  - Links created by a removed or demoted admin are revoked automatically.
  - Admin links must be single-use and created by an owner (CHECK constraint plus handler).
  - Member and invite limits; preview is rate-limited and generic on failure.
- **IDOR:** cross-team substitution of `:iid`, `:uid`, `:sid` and session `:id` returns 404 and changes nothing.
- **Delete:** account deletion leaves zero workspace, entity and share rows for the personal space and sole-member teams, personal share URLs return 404, co-owned teams survive with billing reassigned, and the forced FK race rolls back with 409. Team delete cascades, creates departure rows and makes share pages return 404.
- **Hosted pages:** CSP on `/auth/*` and `/i`; the invite page with JS disabled leaks no team details; the invite token never appears in any path or query (assert on request logs).
- **Cron GC:** removes expired auth_requests, sessions, invites and departures, and keeps live rows.

### E2E (Playwright on the mock, UX §14 test ids)
- **Lena regression:** signed out, the switcher and footer are pixel-identical and make zero network requests (route spy).
- **Sign-in states:** ok, timeout, offline, denied, `email_in_use`, `no_port`. Escape keeps the waiting sign-in alive.
- First-sync checklist, plus the fresh-device toast.
- Sign out, then sign in as a different account: SpaceNoticeDialog choices.
- **Teams:** create (cap at 3), invite (Uses before Role, Admin disabled when Uses > 1), join (signed out leads to sign-in, then preview, then join), already a member, invalid link.
- **Viewer:** Save is disabled with a reason, Send works, ⌘S toast, Duplicate to personal.
- Role downgrade banner; removal notice with Keep focused; project moved or deleted by a teammate with unsynced edits shows the banner.
- Move pre-flight lists credentials and secrets; Copy to project lists unresolved variables.
- Quota, `server_full`, `clock_skew` and `client_too_old` messages; footer chip states and the per-space tooltip.

### Security (manual or scripted)
- Malicious editor re-pushes a personal project's ids into the team: the personal copy is untouched.
- Crafted authorize URL with an attacker challenge opened in the victim's browser: the page warns, and the code is unusable without the verifier.
- Refresh-token theft inside and outside the 60 s grace window.
- Removed admin with an alt account.
- Brute force of the invite preview.
- Error bodies never echo tokens.
- Fragment never sent in Referer.

### Migration
- Apply 0001→0004 on a copy of a populated legacy DB: no row loss, legacy responses byte-identical, old client (current release) sync and 507 retry work.
- Client upgrade path: pure-legacy user keeps identical files. Signing in moves the token to the secret store, removes `sync` from `workspace.json` and preserves the cursor and bookkeeping in `sync-spaces.json`.
- Downgrade to the pre-M0 build: the old client shows 'not connected' and pushes nothing. Re-upgrade: `account.json` stays authoritative.
- Two devices merge the same legacy workspace: no duplicates.
- macOS build N to N+1 stays signed in, using the file store on ad-hoc builds.

## Risks

- **The three docs specify different products (paste-code, same-id vs new-id moves, the removal default, macOS keychain).** Mitigation: PO rulings R9–R13 and a UX update before any M1 code, then treat the tech doc's deviations as the spec.
- **Invite and rejoin abuse by removed or demoted admins** through links they created and alt accounts. Mitigation: revoke links by `created_by` on removal and demotion, default the revoke checkbox on, and use honest copy.
- **Silent loss of teammates' work** when a project is moved out or deleted, plus invisible orphan entities eating quota. Mitigation: freeze and banner recovery, and never push children of tombstoned or unbound projects.
- **Cross-space data leaks through id collisions** (today's cross-project `retain` in `crates/core/src/sync.rs`). Mitigation: B1–B7 invariants with dedicated malicious-push tests. New-id moves remove most collision paths.
- **D1 capacity on the default server** (Free tier holds about 4 maxed-out owners). Mitigation: launch on D1 Paid, verify the breaker in miniflare, define cold-isolate behaviour, plan sharding before 50% of the limit.
- **Mixed client versions erasing future gRPC/GraphQL fields.** Mitigation: ship M0 first, gate team spaces with the 426 version check, keep `TEAMS_ENABLED=false` until M0 adoption.
- **Tokens leaking from storage on macOS ad-hoc builds** (file store). Mitigation: 0600 file plus visible notice. Developer ID signing unlocks the keychain.
- **Team onboarding blocked by IP-based limits behind NAT.** Mitigation: a separate account-creation bucket, per-auth-request limits, documented vars.
- **Migration mistakes in a cascading schema** (a `workspaces` rebuild would wipe entities and shares). Mitigation: only ADD COLUMN and new tables, sentinel `token_hash`, and a migration test on a populated legacy DB copy.

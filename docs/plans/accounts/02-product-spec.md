# ReqHarbor: Accounts, Login, Sync and Team Projects (product spec)

Status: **revision 2**, draft for owner review · Owner: Product · Last updated: 2026-11-26
Builds on `docs/PRODUCT.md` §2, §3, §5.8, §5.10, §5.11, §8 and §9. In account mode this spec replaces AC-SYNC-1 and AC-SYNC-7. Anonymous token mode keeps them exactly as they are (§6.2).
Target: the existing Cloudflare Worker plus D1 in `worker/`. The request said "d2"; the repo uses D1, and so does this spec.
This is an investigation and design pass only. No repo files were changed.

---

## 0. Changes since revision 1 (QA round)

| QA item | What changed | Where |
|---|---|---|
| Removed members could rejoin through old invite links | Removal now blocks rejoining through any invite created before the removal. The remove confirm offers "Also revoke active invite links". Multi-use links are capped at **editor**. Invite tokens moved out of URL paths, into a URL fragment and POST bodies. | AC-TEAM-4, AC-INV-1…6, §7 |
| Auto-linking on a verified email allowed account takeover | Auto-link is dropped. v1 allows **one sign-in method per account**. A sign-in whose email matches an existing account is refused with guidance. Explicit linking is in M4. | AC-AUTH-6, D15 |
| Default quotas exceeded D1 limits | Personal quota lowered to 25 MB. Team storage counts against the team owner. Each owner has a 100 MB aggregate cap. The team limit per user is now 3. A server-wide `server_full` circuit breaker was added. D10 is restated with the real D1 limits. | §11, AC-QUOTA-1…4, D10 |
| Untestable or contradictory ACs | Timings are restated as p95 under stated conditions. The sign-in entry points are listed exactly. The footer indicator follows DESIGN §5.14 (hidden when not connected). The paste-code format and flow are fixed. Email OTP moves to M4. There is one release order. Clock skew applies to all account spaces. The project-rename row is unbolded. | §6, §13, §15, decision table R1–R8 |
| Scope creep | Claim is now client-side "Merge into personal" only, with no Worker claim endpoint. Moves are project-level only; collections use "Copy to project…". The pre-flight warns but does not convert. Conflict "Keep this version", device rename, link/unlink and collapsible switcher groups move to Later. | §5, AC-ONB-3, AC-PROJ-4/6, AC-PROJ-1 |

---

## 1. Summary

ReqHarbor gets **optional** accounts. Signing in does three things:

1. Your projects sync across your devices without copying a secret token around.
2. You can create a **team**, invite people with a link, and work together on **team projects**: the collections, folders, requests and project environments inside them.
3. Share links get an owner and a visible creator, so teammates can see who made what.

Nothing changes for someone who never signs in. The app stays local-first and works fully offline. Existing anonymous token workspaces keep working, and their content can be merged into an account.

How it works:
- Login happens in the **system browser**. The Worker brokers OAuth with GitHub and Google, and later email codes. It hands the app opaque tokens through a loopback redirect protected by PKCE.
- Tokens live in the **OS keychain**.
- A team owns a remote **space**, which is a D1 `workspaces` row. The existing sync engine (`worker/src/sync.ts`), quotas (`quota.ts`), secret stripping (`sanitize.ts`) and shares (`shares.ts`) are reused, not rewritten.

### 1.1 Why "team projects" and not "team collections"
Users asked for "team collections". The unit we share is the **project**, which holds collections and project environments. The reasons:
- Collections use project environments through `{{vars}}`. A shared collection without its environments gives teammates requests that do not resolve.
- `Project` is already the top-level synced node, and binding at that level maps cleanly to one remote space.
- A single collection can still reach a team. "Copy to project…" copies it into a team project (AC-PROJ-6).

UI copy says "Team" and "Team projects". See open decision **D1**.

---

## 2. Goals and non-goals

### 2.1 Goals
- **G1. Sign in on the desktop in a few clicks** with GitHub or Google. No passwords. Measurable target: AC-AUTH-3.
- **G2. Personal sync by account.** Signing in on a second device shows the same personal projects, with no token to copy.
- **G3. Team projects.** Create a team, invite with a link, assign roles, and sync team projects to every member. Removing a member takes effect on the server immediately and stays in effect.
- **G4. No forced account.** Every feature except sync, share and teams works signed out and offline (PRODUCT §2.1 item 11 holds).
- **G5. No secret leakage.** Secret variable values never leave the device (AC-ENV-7 holds for teams). Users are warned before literal credentials go to a team.
- **G6. Smooth migration.** Existing anonymous workspaces keep syncing, and their content can be merged into an account without duplicates.
- **G7. Forward compatible.** Team data survives mixed client versions and future request kinds (GraphQL as its own kind, gRPC) without silent data loss.
- **G8. Self-hostable and bounded.** A self-hoster enables only the login methods they configure secrets for, and no third-party identity vendor is required. No single user can fill the database.

### 2.2 Non-goals (v1)
- Real-time co-editing, presence, cursors or field-level merge. Whole-entity last-write-wins stays.
- Per-collection or per-request permissions inside a team project.
- Passwords, passkeys, SSO/SAML, and 2FA beyond what GitHub and Google provide.
- Linking several sign-in methods to one account (M4).
- Email invitations (invite links only), Turnstile, audit log, billing and paid plans.
- End-to-end encryption of synced data (D9). Data is stored on the Worker owner's Cloudflare account.
- A web version of the app, or any web dashboard beyond the minimal hosted sign-in and invite pages.
- Syncing history or settings (unchanged from AC-SYNC-3).
- `reqharbor://` deep links (stay in PRODUCT §2.2 Later).

---

## 3. Personas

| Persona | Today | What they need from this |
|---|---|---|
| **Solo, local-only** ("Lena") | Never opens Settings → Sync | Nothing changes: no nags, no sign-in wall, no network calls. |
| **Solo, multi-device** ("Marco") | Anonymous workspace; copies the `rh_` token between laptop and desktop | Signs in on both devices and merges the existing workspace once, with no duplicates. |
| **Team lead** ("Priya") | Sends read-only share links; teammates re-import them by hand | Creates a team, moves the "Payments API" project into it, sends an invite link, sets roles. |
| **Team member, editor** ("Sam") | Receives share links | Joins from a link, sees team projects next to personal ones, edits them, fills in their own secret values. |
| **Viewer** ("QA/PM Olu") | Opens share pages | Joins as viewer and runs requests, but cannot change team data. Can duplicate a request into a personal project to experiment. |
| **Self-hoster** ("Ops Kai") | Deploys the Worker with `wrangler` | Turns on GitHub login only and sets quotas through Worker vars. |

---

## 4. Concepts and vocabulary

- **Account:** a user on one Worker, identified by an internal ULID and created on first sign-in. It has a verified email (unique per server), a display name, an avatar, and exactly **one** sign-in identity in v1.
- **Identity:** the login method behind an account: a GitHub user id or a Google `sub`. It is keyed by provider subject, never by email.
- **Session:** one signed-in device. Sessions can be listed and revoked.
- **Space:** a remote container of synced entities, which is the existing D1 `workspaces` row. There are three kinds:
  - **Personal space:** one per account, private.
  - **Team space:** one per team in v1 (D13).
  - **Legacy space:** an anonymous workspace reached with an `rh_` token.
- **Team:** a group of accounts with roles. In v1 it owns exactly one team space. Its storage counts against its **owner**.
- **Project binding** (stored on the device): every local project is in exactly one state:
  - `Local only`: never leaves the device.
  - `Personal`: synced to my personal space.
  - `Team <name>`: synced to that team's space.
  - `Legacy`: synced to an anonymous token workspace.
- **Global environment:** stays personal. It syncs to the personal space or a legacy space, never to a team space (AC-TSYNC-6).

---

## 5. Scope

### 5.1 v1 (M0–M3)
1. **Worker auth module.** It has three parts:
   - a hosted sign-in page for GitHub and Google, showing only the methods that are configured;
   - PKCE authorize and token endpoints;
   - access and refresh tokens, sessions, logout, `GET /v1/me`, and delete account.
2. **Desktop sign-in.** It uses the system browser, a loopback redirect and a paste-code fallback. Tokens go in the keychain and refresh silently.
3. **Personal space sync by account.** Bindings are per project, and the first sign-in asks what to sync.
4. **Teams.** Create, rename and delete; members; four roles; invite links (create, revoke, accept); leave; remove (with rejoin blocking); transfer ownership.
5. **Team project sync to all members.** Roles are enforced on the server, edits are attributed ("last edited by"), and a clock-skew guard applies.
6. **Shares in spaces.** Shares record `created_by`, roles are checked, and share options are stored on the server.
7. **Legacy workspaces.** They keep working unchanged, and a client-side "Merge into my personal space" is added.
8. **Forward-compatibility hardening (§13).** It **must ship to clients before team sync is enabled**.
9. **Limits, rate limits, quotas and the circuit breaker** (§11), plus the privacy disclosures (§12).

### 5.2 Later (not v1)
- **M4 candidates, in priority order:**
  - email one-time code sign-in (D2);
  - "Link another method" and unlink;
  - a session device rename;
  - a conflict "Keep this version" action;
  - collapsible switcher groups.
- A server-side legacy **claim** that moves ownership in place, keeps share URLs and invalidates `rh_`.
- Moving collections between projects (as opposed to copying them), and copying an environment along with a move.
- A "Convert to secret variable" quick-fix in the pre-flight.
- Passkeys on the hosted page.
- Emailed invites.
- Several spaces per team.
- Per-collection access.
- Audit log and activity feed.
- Field-level merge or a three-way conflict UI.
- E2E encryption.
- `reqharbor://` deep links.
- SSO/SAML through a broker provider.
- Cloudflare Access mode for self-hosters.
- A team policy for credentials in shares.
- Billing and higher quotas.
- R2 for large blobs such as proto bundles.
- D1 sharding by region or tenant (D10).

---

## 6. User stories and acceptance criteria

Unless an AC says otherwise, every timing below is a **p95 measured on a local Worker (`wrangler dev`) with ≤ 200 entities per space**, and on the mock backend (`ui/src/lib/api/mock`) for e2e tests.

### 6.1 Sign-in and sessions (AUTH)

*As Marco, I want to sign in from the app with GitHub or Google, so I never handle a secret token.*

- **AC-AUTH-1:** Settings gets a new first section, **Account**.
  - Signed out, it shows:
    - "Sign in to sync across devices and work with a team";
    - a **Sign in** button;
    - the server field (D14: prefilled with the default server, editable under "Use a different server").
  - Signed out is the default state. Sign-in is offered **only** in these places:
    - Settings → Account;
    - Settings → Teams, when signed out;
    - the Share dialog, for a `Local only` project (AC-TSHARE-1);
    - Join team (AC-INV-3).

    Nothing else prompts, shows a banner or blocks.
  - The sidebar sync indicator follows DESIGN §5.14 unchanged: it is **hidden** while nothing is connected.
- **AC-AUTH-2:** Clicking **Sign in** does the following:
  1. The app calls `GET /v1/auth/providers`.
  2. It opens the **system browser** at the server's hosted sign-in page, which lists only the configured methods.
  3. It shows "Waiting for you to finish in your browser…" with **Cancel** and **Paste code instead**.
- **AC-AUTH-3:** On success:
  - The browser page says "You're signed in. You can close this tab and return to ReqHarbor."
  - The app shows the account (avatar, name, email) **≤ 2 s p95 after the browser redirect reaches the loopback listener**.
  - The redirect uses PKCE S256 to `http://127.0.0.1:<ephemeral port>/cb`.
  - A pending request expires after 10 minutes, and the app then shows "Sign-in timed out. Try again."
- **AC-AUTH-4 (paste-code fallback; tech §1.3 must match this):**
  - **Code format.** Every sign-in can also finish with a one-time code of **10 characters of Crockford base32**, displayed as `XXXXX-XXXXX`. Input ignores case, hyphens and spaces.
  - **Lifetime and binding.** The code is single-use, valid for 60 s, and bound to the pending request's PKCE challenge. Without this device's verifier it is useless.
  - **Same request.** If the app opened a loopback listener, **Paste code instead** only swaps the waiting view for a code field. The pending request stays the same, and the loopback listener stays open. The final browser page always shows the code with a **Copy** button, below the "return to ReqHarbor" message.
  - **No listener.** If the app could not bind a loopback port, it starts the request with `redirect_uri=urn:ietf:wg:oauth:2.0:oob`. The browser page then shows only the code.
  - **Exchange.** Both paths end in the same `POST /v1/auth/token` with `grant_type=authorization_code`, the code and the verifier.
- **AC-AUTH-5 (email code, M4, not v1):** reserved. When it is built:
  - the code is 6 digits and valid for 10 minutes, and is typed into the same browser tab;
  - 5 wrong attempts invalidate it;
  - at most 5 codes per email per hour;
  - errors are generic;
  - it is subject to AC-AUTH-6.
- **AC-AUTH-6 (one identity per account; no auto-linking):**
  - **First sign-in.** A sign-in with a new provider identity creates the account and its personal space. The provider must supply a **verified** email: GitHub's primary verified email from `/user/emails`, or Google `email_verified=true`. Otherwise the sign-in fails with "Your GitHub account has no verified email. Add one and try again."
  - **Email already in use.** A new identity whose email matches an existing account is **refused**: "An account with *e…@domain* already exists. Sign in with GitHub instead." The message names the existing method, and no account is created or changed.
  - **Returning users.** Later sign-ins match on provider subject only. A changed email at the provider updates the account email if no other account uses it.
- **AC-AUTH-7 (token storage):**
  - Tokens are stored in the OS keychain: macOS Keychain, Windows Credential Manager or Linux Secret Service.
  - If no keychain is available (for example, headless Linux), tokens go to a file in the app data directory with owner-only permissions (0600). Settings → Account then shows: "Your sign-in is stored in a file because no system keychain was found."
  - Tokens never appear in `workspace.json`, logs or error messages.
- **AC-AUTH-8 (token refresh):**
  - Access tokens expire after 1 hour and refresh silently.
  - Refresh tokens rotate on every use. Presenting an already-rotated refresh token revokes that session.
  - A refresh failure never discards local data or unpushed edits.
- **AC-AUTH-9 (replaces AC-SYNC-7 in account mode):** A 401 first triggers one silent refresh and retry. If that also fails, sync pauses and the status becomes **Sign-in required**:
  - the sidebar shows the danger "Sync off" indicator (DESIGN §5.14), which opens Settings → Account;
  - Settings → Account shows a banner with **Sign in again**;
  - editing continues locally.

  After signing in again as the **same** account, queued changes push without any conflicts caused by the pause. Signing in as a **different** account counts as AC-AUTH-11 followed by a fresh sign-in.
- **AC-AUTH-10 (Devices):** Account → **Devices** lists active sessions:
  - device name (OS plus hostname, set at sign-in);
  - signed-in date;
  - last active, to the day;
  - "This device" on the current row.

  **Sign out** on any other row revokes that session. That device gets AC-AUTH-9 on its next request.
- **AC-AUTH-11 (Sign out on this device):**
  1. Revokes the session on the server (best effort when offline).
  2. Clears the keychain entry.
  3. Stops syncing account spaces.

  Afterwards:
  - Local projects stay on disk with their bindings, shown as "Paused: signed out".
  - Signing back in as the same account resumes them.
  - Signing in as another account asks once, per space: "Keep as local-only copy" (the default) or "Remove from this device".
- **AC-AUTH-12 (hosted pages):** Hosted sign-in pages use the same CSP and no-third-party-scripts posture as share pages (`share-page.ts`).
  - They set only one short-lived cookie (`HttpOnly`, `Secure`, `SameSite=Lax`, ≤ 10 min) scoped to the sign-in in progress.
  - The `/v1/*` JSON API stays cookie-free, so `withSecurityHeaders` keeps stripping `Set-Cookie` on API routes.

### 6.2 Existing users: local-only and anonymous workspaces (ONB)

*As Lena, I want nothing to change. As Marco, I want my existing synced workspace to become part of my account without duplicates.*

- **AC-ONB-1 (Lena):**
  - A signed-out user with no sync config sees no change in behaviour, in the PRODUCT §7 performance budgets, or in network traffic.
  - The app makes **no** network call to any Worker until the user starts sign-in or sync.
  - Every non-sync feature works offline.
- **AC-ONB-2 (anonymous token mode stays):**
  - "Create workspace" and "Connect with token" move into Settings → Sync → **Advanced: sync without an account**. They behave exactly as AC-SYNC-1…7 do today, and legacy `/v1/*` routes are unchanged byte for byte. The one exception is the 507 `server_full` breaker (AC-QUOTA-4), which old clients already treat as a retryable 5xx.
  - Projects synced this way show the binding `Legacy`.
  - D4 decides whether creating *new* anonymous workspaces stays enabled on the default server.
- **AC-ONB-3 ("Merge into my personal space", client-side; v1's only claim mode):**
  - **Where it appears.** A signed-in user who also has a legacy connection sees **Merge into my personal space** under Sync.
  - **What it pushes.** Confirming pushes every `Legacy` project into the personal space **with the same entity ids**. That includes the Global environment, merged as today. The projects are rebound to `Personal`.
  - **Afterwards.** The legacy connection then switches to **shares-only** mode:
    - Sync to the legacy space stops.
    - The `rh_` token moves into the keychain.
    - Settings → Shares keeps a **Legacy** group, so those share links can still be listed, updated and revoked.
    - The legacy workspace and its share URLs keep working. Nothing is deleted on the server.
  - **Confirm text.** The dialog says: "Other devices using this token keep syncing to the old workspace until you sign them in and merge there too."
  - **Removing the old workspace.** "Disconnect legacy workspace" (shares-only mode) forgets the token. It warns that the existing legacy share links can then no longer be updated or revoked.
- **AC-ONB-4 (merging on a second device):**
  - A second device that is still on the legacy token keeps syncing to the legacy space.
  - When that device signs in and runs AC-ONB-3, the same entity ids reach the personal space. Last-write-wins (LWW) then resolves them with no duplicates: an entity with an equal `updatedAt` is applied once, and older copies are stale.
- **AC-ONB-5 (first sign-in on an install with local projects):**
  - A "Sync these projects to your account?" checklist appears, with every project pre-checked (D12).
  - Unchecked projects become `Local only`.
  - Nothing uploads before the user confirms.
  - Legacy-bound projects are not listed. They use AC-ONB-3.
- **AC-ONB-6 (first sign-in on a fresh device):** On a device that only has the untouched "Default" project, the app pulls the personal space and every team space the user belongs to (D16). The empty "Default" project is dropped, the same way current sync handles it.

### 6.3 Project bindings: personal, team, local (PROJ)

*As Priya, I want to choose which projects are personal, which belong to my team, and which never leave my machine.*

- **AC-PROJ-1 (switcher):** The project switcher groups projects under fixed, non-collapsible headings:
  - **Personal**;
  - one heading per team, with the team name and a role chip;
  - **On this device only**;
  - **Legacy workspace** (only when one exists).

  Each row shows a binding icon. Collapsible groups are Later.
- **AC-PROJ-2 (new projects):** When signed in, **New project** defaults to `Personal` (D11). The create dialog has a **Location** select with these options:
  - Personal;
  - each team where I am editor or above;
  - This device only.
- **AC-PROJ-3 (changing a binding):** Project context menu → **Sync location…** changes the binding:
  - **Local only → Personal or Team:** the project uploads.
  - **Personal → Local only:** the app asks "Remove from your other devices?". **Yes** tombstones the project in the personal space; the local copy stays.
  - **Personal ↔ Team:** see AC-PROJ-4 and AC-PROJ-5.
- **AC-PROJ-4 (Move to team; project level only; needs editor or above in the target):**
  1. A pre-flight dialog lists what will be shared. It reuses the share dialog's scanner (`share.rs`, AC-SHARE-2):
     - Secret variable values: never uploaded.
     - Literal values in Authorization, Proxy-Authorization and Cookie headers and in Auth tab fields: each is listed by request name and field. There is **no auto-conversion**. The copy reads "Teammates will see these values. Replace them with secret variables before moving if they're personal."
     - Local file paths: kept on this device only (AC-TSYNC-8).
  2. Confirming uploads the project into the team space **with the same entity ids**, then tombstones it in the personal space, so the user's other devices see it move.
- **AC-PROJ-5 (Duplicate to personal):** Available to any role on a team project, collection or request. It creates a copy with new ids in a personal project the user picks. For viewers it is the only way to take team data out. Moving a whole team project back to personal (delete from the team, create in personal) requires **admin** or above.
- **AC-PROJ-6 (Copy to project):** **Copy to project…** on a collection or request copies it with new ids into any project I can write to, including a team project where I am editor or above. The source stays where it is. The pre-flight from AC-PROJ-4 runs when the target is a team project, and it also lists `{{vars}}` that will not resolve in the target. Environments are not copied.
- **AC-PROJ-7 (drag and drop):** Dragging across projects with different bindings is not allowed in v1. The drop target shows "Use Copy to project… or Move to team…". Dragging within one project is unchanged (AC-COL-3).

### 6.4 Teams (TEAM)

*As Priya, I want to create a team and manage who is in it.*

- **AC-TEAM-1:** Settings gets a new **Teams** section. Signed out, it offers sign-in. Signed in, it lists my teams with role, member count and storage used. **Create team** asks for a name (1–80 chars). It makes me owner and creates an empty team space; no project moves automatically. It is blocked at the team limit (§11) with "You can own up to 3 teams on this server."
- **AC-TEAM-2:** The team detail view shows:
  - **Members:** avatar, name, email, role and joined date;
  - **Invites:** active links with role, expiry, uses and creator;
  - **Storage:** this team's usage and the owner's aggregate (AC-QUOTA-2);
  - **Danger zone.**
- **AC-TEAM-3:** Owners and admins can change a member's role. Only owners can grant or revoke owner. A team always has at least one owner, and the server rejects demoting or removing the last owner with 409 `last_owner`.
- **AC-TEAM-4 (Remove member; admin or above):**
  - **On the server:**
    - The removal takes effect **immediately**: the member's next request to that space returns 403 `not_a_member`.
    - The server records a removal row `(team_id, user_id, removed_at, removed_by)`.
    - Any invite **created before `removed_at`** is then refused for that user with `invite_invalid` (AC-INV-4).
    - The removed user can rejoin only through an invite link created after the removal.
  - **The confirm dialog:**
    - says "*Sam* loses access now. Copies already on their devices can't be wiped.";
    - offers a checkbox **Also revoke all active invite links (N)**, checked by default whenever any multi-use link is active.
  - **On the removed member's device**, the next sync:
    - stops syncing that team;
    - shows "You were removed from *Team*";
    - follows D5 for the local copy. The default removes the team projects from the device. A one-time **Keep a personal copy** option stays in the notice for 7 days while the projects are still on disk.
- **AC-TEAM-5 (Leave team):** Any member except the last owner can leave after a confirm. The local outcome is the same as AC-TEAM-4, but the default is **Keep a personal copy** (D5). Leaving does not create a removal row, so the user can rejoin with any valid link. A last owner must transfer ownership or delete the team first.
- **AC-TEAM-6 (Transfer ownership; owner):** Pick a member. They become owner and I become admin. The team's storage then counts against the new owner. If that would exceed their aggregate cap, the transfer is refused with `quota_exceeded` and the message names the shortfall.
- **AC-TEAM-7 (Rename and delete):**
  - **Rename team:** admin or above.
  - **Delete team:** owner only, after typing the team name to confirm. It deletes the team, its space and all entities, and **revokes all its share links and invites**.
  - Every member's device treats a deletion like AC-TEAM-4, with the notice "*Team* was deleted". The deletion is immediate, with no grace period in v1 (D18).
- **AC-TEAM-8 (attribution):**
  - Every entity in a team space records its last editor, stamped by the server from the session.
  - Each team request shows "Edited by Sam · 3h ago" in the request header area.
  - Conflict toasts name the other editor: "Sam's change to *Create order* was newer; your version was kept as *Create order (conflict)*".

### 6.5 Invites (INV)

*As Priya, I want to invite Sam with a link. As Sam, I want joining to take one paste.*

- **AC-INV-1 (Create invite link; admin or above):**
  - **Role:** viewer, editor or admin. The default is **editor**.
  - **Expiry:** 1, 7 or 30 days. The default is 7.
  - **Max uses:** 1, 5, 25 or unlimited. The default is 1.
  - **Role limits.** A link with more than one use is capped at **editor**: the role select disables Admin with the hint "Admin links are single-use". Only an owner can create an admin link.
- **AC-INV-2 (link format):** The link has the form `https://<server>/i#<token>`. The token is 128 bits of randomness, sits in the **URL fragment**, and is never sent in a request path or query, so it stays out of Cloudflare request logs. Only its hash is stored. The UI has a **Copy** button.
- **AC-INV-3 (hosted invite page):**
  - Opening the link in a browser shows a minimal hosted page. Its script reads the fragment and calls `POST /v1/invites/preview {token}`.
  - The page shows the team name, the inviter's name, the role and the expiry. It reads "Open ReqHarbor → Teams → Join team and paste this link", with a copy button.
  - It reveals nothing else about the team, and with JavaScript disabled it shows only the instructions.
- **AC-INV-4 (Join team in the app):**
  - **Join team** accepts a pasted invite link. If I am signed out, it signs me in first (AC-AUTH-2) and then continues.
  - A preview confirms the team, role and inviter.
  - **Join** calls `POST /v1/invites/accept {token}`. The team's projects appear under the team heading **≤ 2 s p95 after accept** (≤ 200 entities, local Worker).
- **AC-INV-5 (invalid links):**
  - An expired, revoked or used-up link, or a link that predates my removal (AC-TEAM-4), shows one generic message: "This invite link is no longer valid. Ask *Inviter* for a new one."
  - Joining a team I already belong to says so and does not change my role.
- **AC-INV-6 (revoking invites):** Admins can revoke any active invite, and revocation is immediate. Invite creation is rate-limited (§11).

### 6.6 Roles and permissions (ROLE)

Permissions apply per team, which means per team space, and never per collection in v1. The server enforces every **bold** row. The UI hides or disables actions the role cannot perform and explains why on hover.

| Capability | Viewer | Editor | Admin | Owner |
|---|---|---|---|---|
| See team projects, collections, requests and environments (non-secret values) | yes | yes | yes | yes |
| Send team requests (with own local secret values) | yes | yes | yes | yes |
| Duplicate to personal | yes | yes | yes | yes |
| **Push edits: create, edit and delete collections, folders, requests and environments; create and rename team projects** | no | yes | yes | yes |
| **Move a personal project into the team; copy items into a team project** | no | yes | yes | yes |
| **Delete a team project, or move it out of the team** (a `project` tombstone) | no | no | yes | yes |
| See the team's share links | yes | yes | yes | yes |
| **Create share links; update and revoke own** | no | yes | yes | yes |
| **Update or revoke anyone's share link** | no | no | yes | yes |
| **Create and revoke invites; change roles (up to admin); remove members** | no | no | yes | yes |
| **Create admin invite links; grant or revoke owner; transfer ownership** | no | no | no | yes |
| **Rename team** | no | no | yes | yes |
| **Delete team** | no | no | no | yes |
| Leave team | yes | yes | yes | yes, if not the last owner |

Renaming a team project is an ordinary edit, so editors may do it. The server cannot tell a rename apart from any other `project` upsert without inspecting `data`, which stays opaque.

- **AC-ROLE-1 (viewers):**
  - Team projects are **read-only in the UI** for viewers. Edits that would persist are disabled with the tooltip "Viewers can't change team projects. Duplicate to personal to experiment."
  - The URL, headers, body and other fields stay editable as an unsaved scratch. Sending works; saving is disabled (D17).
- **AC-ROLE-2 (server enforcement):**
  - A push from a viewer is rejected with 403 `forbidden_role`.
  - Within an editor's push, a `project` tombstone gets a per-change rejection `forbidden_role`, and the rest of the batch applies.
- **AC-ROLE-3 (role lowered with unpushed edits):** Rejected changes are not discarded. The project shows a banner: "Your role changed to Viewer. You have 3 unsynced changes." It offers two actions:
  - **Save as personal copy:** duplicates the affected items into a personal project;
  - **Discard.**

  The banner stays until one is chosen.
- **AC-ROLE-4 (propagation):** Role changes reach other members' UI on their next sync (focus, Sync now or startup). There is no push channel in v1.

### 6.7 Sync behaviour for spaces (TSYNC)

These extend PRODUCT §5.10. AC-SYNC-2 to 6 apply to each space separately.

- **AC-TSYNC-1:**
  - One sync run processes every space this device is bound to (personal, each team, legacy), pulling and pushing each in turn.
  - A failure in one space (offline, 403, quota) does not block the others.
  - The sidebar indicator shows the worst state, and hovering lists the state of each space.
- **AC-TSYNC-2:** Each space has its own cursor and bookkeeping (`synced`, `origins`, `tombstones`, `syncSkipped`). Joining, leaving or merging one space never forces a full re-upload of the others. Today's `reset_for_new_remote` behaviour is limited to the space being connected.
- **AC-TSYNC-3:** Each entity is pushed only to the space its project is bound to. A `Local only` project never produces network traffic.
- **AC-TSYNC-4:** LWW with conflict copies stays as in AC-SYNC-4. Conflict copies of team entities are created **in the same team project**. Only the device that lost creates them, and they sync like any edit, so teammates can see and clean them up.
- **AC-TSYNC-5 (clock-skew guard, D7; applies to personal and team spaces, not legacy):**
  - The server rejects a change whose `updatedAt` is more than 5 minutes ahead of server time, with the per-change rejection `clock_skew`.
  - The client keeps those edits and shows: "Your computer's clock is ahead by about N minutes. Changes can't sync until it's fixed."
  - Legacy routes stay unchanged.
- **AC-TSYNC-6 (Global environment):** The Global environment is never pushed to a team space. Team requests resolve variables from the active **team project environment** first, then from **my** Global (the unchanged order of AC-ENV-2). Each member's Global stays personal.
- **AC-TSYNC-7 (secrets):** AC-ENV-7 holds for every space. In addition:
  - Team environments sync each variable's key, enabled flag and secret flag.
  - Secret values are stripped by the client **and** by the server (`sanitize.ts` stays), and each member keeps their own local value.
  - A team secret with no local value shows "Set your value" in the Env grid. Its hover reads "Secret: set locally by each member".
- **AC-TSYNC-8 (file paths kept on the device):**
  - In team spaces, `body.binaryPath` and form fields with `kind: "file"` sync as empty. The local value is preserved on pull, the same way secrets are.
  - Teammates see "File not set on this device".
  - Personal spaces keep syncing paths as they do today.
- **AC-TSYNC-9:** After I join a team, its space appears on all my devices automatically (D16).
- **AC-TSYNC-10:** Each pulled entity from an account space carries `updatedBy` (`{userId, name}`), which feeds AC-TEAM-8. Legacy spaces have no attribution.

### 6.8 Sharing in spaces (TSHARE)

These extend PRODUCT §5.11.

- **AC-TSHARE-1:**
  - "Share…" requires the item's project to be `Personal`, `Team` (editor or above) or `Legacy`.
  - On a `Local only` project the dialog says: "Sharing needs this project to sync. Change its sync location to Personal first." It offers a one-click action, which first offers sign-in when the user is signed out.
- **AC-TSHARE-2:** Every share created in an account space records its creator. Settings → Shares groups shares by space (Personal, each team, Legacy) and shows each creator's name. Shares on a team are visible to all its members.
- **AC-TSHARE-3:** In account spaces, the share options (include credentials, attached environment id) are stored **with the share on the server**. That way "Update share" by any permitted member reproduces the original options. Legacy shares keep the per-device `settings.shareOptions`.
- **AC-TSHARE-4:** Share URLs keep working when a project moves from personal to team, and after a legacy merge (AC-ONB-3, shares-only mode). A share is revoked only explicitly, or when its team or account is deleted (AC-TEAM-7, AC-ACCT-2).

### 6.9 Account management (ACCT)

- **AC-ACCT-1:** Account shows the name, email, avatar and the single sign-in method. Linking other methods is Later (M4).
- **AC-ACCT-2 (Delete account; type the email to confirm):** Deletion is immediate on the server. It:
  - deletes the account, identity and sessions;
  - deletes the personal space, its entities and **its share links**;
  - removes me from all teams.

  Restrictions and side effects:
  - It is blocked while I am the **last owner of a team with other members**. The dialog lists those teams with "Transfer ownership" and "Delete team" links.
  - Teams where I am the only member are deleted with the account. The confirm dialog lists them first.
  - Shares I created in teams that live on remain, and their creator shows as "Former member".
  - Local data on this device is untouched, and its projects become `Local only`.
- **AC-ACCT-3:** No export is needed, because the data is already local on every device. The delete dialog says so.

### 6.10 Storage and capacity (QUOTA)

- **AC-QUOTA-1:** Each space has a storage quota (§11). A push that grows usage beyond it is rejected with 413 `quota_exceeded` (as `quota.ts` does today). Other spaces keep syncing, and local edits are kept. The status names the space and the limit: "Team storage full (25 MB). Ask an admin to clean up."
- **AC-QUOTA-2:**
  - Each user has an **aggregate cap** covering their personal space plus every team they **own**. Growth that would exceed it is rejected with `quota_exceeded`, which names the owner.
  - Members of a team see "This team has reached its owner's storage limit."
  - Settings → Teams shows each team's usage, and the owner also sees their aggregate.
- **AC-QUOTA-3:** New accounts are rate-limited per IP bucket (§11). A user can own at most 3 teams.
- **AC-QUOTA-4 (server circuit breaker):**
  - When the D1 database size reaches `DB_SOFT_LIMIT_BYTES`, every push that grows usage, in any space including legacy, is rejected with 507 `server_full`. Reads, deletes and shrinking pushes still work.
  - The default limit is 80% of the plan's database limit: 400 MB on Free, 8 GB on Paid.
  - The size is read from D1's reported `meta.size_after` on writes, and cached for 60 s.
  - The app shows: "The sync server is out of space. Your changes are saved on this device."
  - Account creation is also refused while the breaker is open.

---

## 7. Server API (summary for the dev pass)

Existing `/v1/*` token routes stay **unchanged byte for byte** for legacy and self-host clients, except for the AC-QUOTA-4 breaker. New routes:

| Method & path | Auth | Purpose |
|---|---|---|
| `GET /v1/auth/providers` | none | Configured methods (`github`, `google`; `email` in M4) |
| `GET /v1/auth/authorize?client_id=reqharbor-desktop&redirect_uri=http://127.0.0.1:<p>/cb\|urn:ietf:wg:oauth:2.0:oob&code_challenge&code_challenge_method=S256&state` | none | Hosted sign-in page. Only loopback IP literals or `oob` are accepted. |
| `GET /v1/auth/callback/:provider` | none | Provider callback (confidential client; secrets in `wrangler secret`) |
| `POST /v1/auth/token` (`authorization_code` \| `refresh_token`) | PKCE / refresh | `{accessToken, refreshToken, expiresIn, user}` |
| `POST /v1/auth/logout` | access | Revoke this session |
| `GET /v1/me`; `DELETE /v1/me` | access | Profile; delete account (AC-ACCT-2) |
| `GET /v1/me/sessions`; `DELETE /v1/me/sessions/:id` | access | Devices (AC-AUTH-10) |
| `GET /v1/me/spaces` | access | Personal and team spaces with role, quota, usage and owner aggregate |
| `GET/POST /v1/teams`; `PATCH/DELETE /v1/teams/:id` | access + role | Teams |
| `GET/PATCH/DELETE /v1/teams/:id/members[/:userId]` (DELETE accepts `{revokeInvites: bool}`); `POST /v1/teams/:id/transfer` | access + role | Members |
| `GET/POST /v1/teams/:id/invites`; `DELETE /v1/teams/:id/invites/:inviteId` | access + admin | Invites, addressed by invite **id**, never by token |
| `POST /v1/invites/preview {token}`; `POST /v1/invites/accept {token}` | none / access | Preview, used by the hosted page and the app; join |
| `GET /v1/s/:spaceId`; `GET/POST /v1/s/:spaceId/sync`; `GET/POST/PUT/DELETE /v1/s/:spaceId/shares…` | access + role | The existing handlers, scoped to a space |
| `GET /i` | public | Hosted invite page (static; it reads the fragment client-side) |

There is no `/v1/workspace/claim` in v1 (AC-ONB-3 is client-side).

New error codes:

| Code | Status | Meaning |
|---|---|---|
| `auth_required` | 401 | No valid session |
| `session_expired` | 401 | The access token has expired |
| `forbidden_role` | 403, or per change | The role does not allow this action |
| `not_a_member` | 403 | Not a member of this team |
| `last_owner` | 409 | The team would be left without an owner |
| `email_in_use` | sign-in page | The email already belongs to another account (AC-AUTH-6) |
| `invite_invalid` | 410 | The invite link cannot be used |
| `clock_skew` | per change | `updatedAt` is too far ahead of server time |
| `quota_exceeded` | 413 | Space or owner aggregate is full (existing code, extended) |
| `server_full` | 507 | The server-wide circuit breaker is open |
| `rate_limited` | 429 | A rate limit was hit |

D1 migration `0003_accounts.sql` adds:
- `users`, with `email` UNIQUE NOCASE;
- `identities`, with PK `(provider, provider_user_id)`;
- `auth_requests`, `sessions`, `teams`, `team_members`;
- `team_invites`, with `created_at` and `created_by`;
- `team_removals`;
- `workspaces.kind` (`legacy|personal|team`), `owner_user_id`, `team_id`, and a nullable `token_hash` (this needs a table rebuild);
- `entities.updated_by`;
- `shares.created_by` and `shares.options`.

`wrangler.toml` still has a placeholder `database_id`, so now is the cheapest point to change the schema.

## 8. Client data model changes (summary)

```
Workspace      { id, name,
                 account?: { serverUrl, userId, email, name, avatarUrl, sessionId },   // no tokens
                 spaces: SpaceBinding[],
                 sync?: SyncConfig /* legacy; token moved to keychain; mode: "sync"|"sharesOnly" */ }
SpaceBinding   { remoteSpaceId, kind: "personal"|"team"|"legacy", teamId?, teamName?, role?, cursor,
                 state: "active"|"paused"|"removed" }
Project        (unchanged synced shape) + device-local binding: projectId → remoteSpaceId | null
SyncState      per remoteSpaceId: { synced, origins, tombstones, syncSkipped, lastSyncAt }; deviceId stays global
Request/...    `kind` + unknown-field passthrough (§13)
```

The binding is metadata stored on the device, not part of the synced data. A project's binding follows from the space it was pulled from or pushed to.

## 9. UI surfaces touched (for the designer)

- **Settings:**
  - new **Account** section: sign-in, devices and delete;
  - new **Teams** section: list, detail, members, invites, storage and danger zone;
  - **Sync** reworked to show status per space, with legacy under Advanced, the merge action and shares-only mode;
  - **Shares** grouped by space, with the creator.
- **Sidebar:**
  - the project switcher grouped by owner (non-collapsible headings), with binding icons and role chips;
  - the sync indicator from DESIGN §5.14, with a hover that lists each space;
  - "Sync off" for Sign-in required.
- **CollectionsTree:**
  - new menu items: "Move to team…" (project), "Copy to project…", "Duplicate to personal" and "Sync location…";
  - read-only affordances for viewers;
  - a conflict-copy badge.
- **Request header:** the "Edited by X · time" line for team items.
- **Dialogs:**
  - sign-in waiting and paste-code view (`XXXXX-XXXXX`);
  - first sign-in "what to sync";
  - Move to team / Copy pre-flight (warnings only);
  - Join team;
  - remove-member confirm with the revoke-invites checkbox;
  - role-changed banner;
  - removed-from-team notice;
  - server-full and quota messages.
- **Hosted pages** (server-rendered, in the share-page style): sign-in, done with a copyable code, error (`email_in_use`, no verified email) and invite.

## 10. Edge cases (must be handled)

1. **Offline sign-in attempt.** The app shows "Can't reach *server*". Nothing changes locally.
2. **Same project open on two devices; one moves it to a team.**
   - The other device pulls the personal tombstone and the team-space upsert with the same ids.
   - If the user is a member, it rebinds the project to the team, with no conflict copy and no duplicate.
   - If not, it keeps the project as `Local only` and shows "*Project* moved to *Team*; you're not a member".
3. **Removed while offline with edits.** On reconnect the 403 triggers AC-TEAM-4. Unpushed edits are offered for "Save as personal copy" (as in AC-ROLE-3) before the local removal.
4. **Removed member holds an old multi-use link.** Accepting it returns `invite_invalid` (AC-TEAM-4). A link created after the removal works.
5. **Team or owner quota exceeded, or server full.** See AC-QUOTA-1, 2 and 4. Other spaces keep syncing where they can, and local edits are kept.
6. **Two devices merge the same legacy workspace.** The same ids converge through LWW (AC-ONB-4). No server coordination is needed.
7. **Keychain locked or access denied.** The app is treated as signed out, with the banner "ReqHarbor couldn't read your sign-in from the keychain". Local data is untouched.
8. **A user in many teams.** Membership is unbounded, though each user can own at most 3 teams. The switcher scrolls. Sync processes spaces in sequence, within a 30 s overall budget per run; whatever is left goes into the next run.
9. **A sign-in provider changes the user's email to one that another account uses.** The sign-in succeeds, and the account email stays unchanged. Account settings show "Your GitHub email changed; this account still uses *old*."

## 11. Limits, quotas and rate limits (v1 defaults; Worker vars override)

| Item | Default | Var |
|---|---|---|
| Personal space storage | **25 MB** | `QUOTA_PERSONAL_BYTES` |
| Team space storage | **25 MB** per team | `QUOTA_TEAM_BYTES` |
| Owner aggregate (personal plus owned teams) | **100 MB** | `QUOTA_OWNER_BYTES` |
| Legacy workspace storage | 50 MB (unchanged), counted by the breaker | existing |
| Server circuit breaker | 400 MB (80% of D1 Free's 500 MB); 8 GB on Paid | `DB_SOFT_LIMIT_BYTES` |
| Teams owned per user | **3** | `MAX_OWNED_TEAMS` |
| Members per team | 50 | `MAX_TEAM_MEMBERS` |
| Active invites per team | 20; invite creation 30/day per team | `MAX_TEAM_INVITES` |
| Active sessions per user | 20 (the oldest is revoked on overflow) | — |
| New accounts | 10/h per IP bucket (D1 `rate_limits`, same bucket as workspace creation) | — |
| Entity size, push batch, pull page | unchanged: 512 KB, 500, 500 | — |
| Auth endpoints | 20/min per IP (Workers Rate Limiting binding, approximate) | — |
| Sync calls | 120/min per session | — |
| Share create | 30/h per space | — |
| Legacy workspace create | unchanged: 10/h per IP | — |

Every limit returns a clear error code, and the app shows a message that names the limit.

## 12. Privacy and security requirements

- **PRIV-1 (data collected):**
  - Per account: email, display name, avatar URL, provider subject id, session device name, and last-active day.
  - **No IP addresses** are stored, apart from the hashed rate-limit buckets.
  - No analytics or telemetry (PRODUCT §7 holds).
- **PRIV-2 (secrets):** Secret variable values never leave the device, in any space or share (AC-ENV-7, AC-TSYNC-7). The server strips them as defence in depth.
- **PRIV-3 (plaintext data):**
  - Non-secret data syncs in plaintext to the Worker's D1 and to every team member. This **includes literal credentials typed into Auth fields or headers**.
  - The UI says this in two places:
    - the Move-to-team and Copy pre-flight (AC-PROJ-4);
    - the Teams footnote: "Team data is visible to all members and stored on *server*. Use secret variables for tokens and passwords."
- **PRIV-4 (tokens and invites):**
  - On the server, tokens and invite tokens are stored only as SHA-256 hashes.
  - On the client, tokens are stored only in the keychain (AC-AUTH-7).
  - Invite tokens never appear in URL paths or query strings (AC-INV-2).
- **PRIV-5 (identities):** An identity is keyed by provider subject. Emails never link identities in v1 (AC-AUTH-6). A recycled or transferred email domain therefore cannot take over an existing account.
- **PRIV-6 (deletion):** Account and team deletion are immediate in D1. Cloudflare D1 Time Travel may keep recoverable state for up to 30 days. The privacy note states this, and self-hosters control it.
- **PRIV-7 (hosted pages):**
  - strict CSP, `noindex` and `X-Frame-Options: DENY`;
  - only the transient cookie from AC-AUTH-12;
  - OAuth `state` and PKCE S256 are always used;
  - the desktop client may use only loopback IP-literal or `oob` redirect URIs.
- **PRIV-8 (removal):** Removing a member revokes server access immediately and blocks rejoining through old invite links (AC-TEAM-4). Copy in the UI is honest that local copies on their devices cannot be wiped.

## 13. Forward compatibility: GraphQL and gRPC (must ship before team sync)

Mixed client versions are normal in a team. Today an older client drops unknown fields, and LWW then erases them: `model.rs` has no passthrough except on `Settings`. Without a fix, that would silently destroy future GraphQL and gRPC data in team projects.

- **AC-FWD-1:** All synced entity structs preserve unknown JSON fields on read, edit and write. That covers `Project`, `Collection`, `Folder`, `Request` and `Environment`, and the nested `Body`, `Auth`, `KeyValue` and `RequestSettings`.
- **AC-FWD-2:** `Request` gains `kind: "http"`, which is the default when the field is missing. GraphQL and gRPC will be new `kind` values within the existing `request` sync type, not new sync types.
- **AC-FWD-3:** A client that pulls a request with an unknown `kind` or an unknown `body.type` keeps it intact. It:
  - shows it in the tree with a "Requires a newer ReqHarbor" badge;
  - opens it read-only;
  - never pushes a modified version of it.
- **AC-FWD-4:** Entities may carry `minClientVersion`. Older clients treat such entities as in AC-FWD-3.
- **AC-FWD-5:** On pull, unknown sync `type` values are skipped and kept in sync state without an error. A future `protoFile` type can then be added on the server first.
- **AC-FWD-6:** The share page and `normalizeSnapshot` render an unknown request kind as "This request type can't be previewed on the web. Import it into ReqHarbor." They must not render it as a GET with no body.

**Release order** (this is the only release order; tech §11 must match it):
1. **Client M0:** AC-FWD-1…6 (client parts).
2. **Worker M1:** accounts and personal spaces, plus AC-FWD-6 on the server.
3. **Client M1:** sign-in, personal sync, bindings and legacy merge.
4. **Worker M2:** teams enabled. `/v1/s/*` on team spaces requires `X-ReqHarbor-Version` ≥ M0 and returns 426 `client_too_old` otherwise.
5. **Client M2:** the team UI.

## 14. Docs to update when this is approved

- **`PRODUCT.md`:**
  - §2.1: add accounts as optional.
  - §2.2: remove "OS keychain" and "multi-user permissions"; keep real-time collaboration.
  - §5.10: mark AC-SYNC-1 and AC-SYNC-7 as legacy mode.
  - §8: add auth, the token model and the new routes.
  - §9: remove "team accounts, roles".
- **`API_CONTRACT.md`** §3.9 and §3.10: new commands for auth, teams, invites, spaces and bindings.
- **`docs/types.ts`.**
- **`DESIGN.md`** §5.12 and §5.14, plus specs for the new dialogs. The §5.14 hidden-when-not-connected rule stays.
- **The `worker` README for self-hosters:** provider secrets, quota and breaker vars, and the D1 plan limits.

## 15. Delivery plan

1. **M0, forward-compatibility hardening** (§13). Client only, releasable on its own.
2. **M1, accounts and personal sync.**
   - Worker: the auth module (GitHub and Google), sessions, personal spaces, quotas, the breaker, and rate limits.
   - Client: keychain storage, sign-in with paste-code, per-space sync state, project bindings (Personal / Local only), the first-sign-in checklist, and the legacy merge.
3. **M2, teams.**
   - Teams, members, roles and invites, with removal blocking.
   - Team space sync with role enforcement, attribution, the clock-skew guard and file paths kept on the device.
   - Move to team (project level), Copy to project, and Duplicate to personal.
   - Share ownership and share options stored on the server.
4. **M3, polish and QA.**
   - The role-change and removal flows, and delete account.
   - Parity in the mock backend (`ui/src/lib/api/mock/index.ts`).
   - Docs.
5. **M4 (after M3 QA):** the candidates in §5.2: email OTP, linking methods, device rename, "Keep this version" and collapsible groups.

---

## 16. Open decisions for the owner

| # | Decision | Options | Recommendation |
|---|---|---|---|
| **D1** | Unit of team sharing | (a) **Project**, with its collections and project environments; (b) individual collection; (c) everything, like today's whole-workspace sync | **(a) Project.** Environments travel with their requests, and a project maps 1:1 to a space binding. Single collections reach a team through Copy to project (AC-PROJ-6). UI copy uses "Team projects". |
| **D2** | Login methods at launch on the default server | (a) GitHub + Google + email code; (b) GitHub + Google, with email code in M4; (c) GitHub only | **(b).** It needs no email sender (Resend or Cloudflare Email Service on Workers Paid) and none of the OTP abuse surface for v1. Email arrives in M4 together with explicit linking, which it depends on (AC-AUTH-6). Self-hosters choose methods by configuring secrets. |
| **D3** | Session model | (a) 1 h access token plus a rotating refresh token with reuse detection; (b) one opaque 90-day sliding token per device | **(a).** It follows RFC 9700, and the schema also supports (b) if the work has to be time-boxed. |
| **D4** | Anonymous token workspaces on the default server | (a) keep create and connect indefinitely; (b) keep connect, disable *creating new* ones; (c) remove after a migration window | **(a) for v1**, under Advanced, with the server breaker as the backstop. This honours "no account required". If legacy creation drives growth, switch to (b), which is one Worker var. |
| **D5** | Local copy when someone leaves or is removed from a team | (a) always keep a detached personal copy; (b) always remove; (c) prompt, with defaults | **(c).** Removed by an admin: default **remove**, with a 7-day "keep a copy" option. Left voluntarily: default **keep**. Copy must be honest that local data cannot be force-wiped. |
| **D6** | Role set | (a) owner, admin, editor, viewer; (b) owner, editor, viewer, with multiple owners | **(a).** It costs almost nothing, and it lets leads manage members without being able to delete the team. |
| **D7** | Clock-skew protection | (a) trust client clocks (today); (b) reject `updatedAt` more than 5 min ahead of server time; (c) server-assigned timestamps | **(b), in all account spaces** (personal and team; legacy unchanged). It is small, and it stops one fast clock from winning every conflict. (c) changes LWW semantics and is Later. |
| **D8** | Literal credentials in team requests | (a) sync as-is, with a warning; (b) auto-convert to secret variables on move; (c) block the move until they are removed | **(a), warning only, in v1.** The "Convert to secret variable" quick-fix is Later. Blocking is too strict, and auto-conversion surprises people. |
| **D9** | Encryption | (a) Cloudflare at-rest only, plaintext to the Worker owner; (b) E2E with per-team keys | **(a) for v1, disclosed** (PRIV-3). E2E breaks server-side secret stripping and share rendering, and it needs key management for invites. |
| **D10** | Quotas and capacity on the default server | (a) 25 MB personal, 25 MB per team, a 100 MB owner aggregate, 3 owned teams, and the breaker at 80% of the D1 limit; (b) the same with lower numbers; (c) paid tiers | **(a), on D1 Paid** ($5/mo, 10 GB). D1 Free (500 MB per database) holds about 4 maxed-out owners before the breaker trips. It suits a beta only. Paid holds about 80 maxed-out owners; real usage is far below the caps. Sharding (a D1 per region or per tenant, routed by `users.shard`) is Later, and is planned before 50% of the Paid limit. All values are Worker vars. |
| **D11** | Default location for new projects when signed in | (a) Personal (synced); (b) Local only | **(a).** It matches today's "connect means everything syncs" and multi-device expectations. Local only is one click away. |
| **D12** | First sign-in with existing local projects | (a) upload everything silently; (b) checklist, all pre-checked; (c) checklist, none pre-checked | **(b).** The choice is explicit, nothing uploads by surprise, and the common case is still one click. |
| **D13** | Spaces per team | (a) one team space per team; (b) many spaces per team, with roles per space | **(a) for v1.** The schema (`workspaces.team_id`) already allows (b) later. |
| **D14** | Default server | (a) ship an official hosted Worker URL, prefilled; (b) no default, the user enters a URL (as today) | **(a)**, editable under "Use a different server". The owner must then deploy and run the official instance: domain, OAuth apps, D1 Paid, and a privacy note. |
| **D15** | Linking identities across providers | (a) auto-link on a verified matching email; (b) explicit "Link another method" from a signed-in session; (c) no linking in v1, one method per account | **(c) for v1, then (b) in M4.** QA showed that (a) allows account takeover through recycled or transferred domains and weak `email_verified` signals. A matching email is refused with guidance (AC-AUTH-6). |
| **D16** | Team spaces on a new device | (a) automatically sync all my teams; (b) choose per device | **(a)** for v1. A per-device "hide team" setting is Later. |
| **D17** | Viewer editing | (a) fully read-only; (b) scratch edits allowed and sendable, never saved | **(b).** Viewers (QA, PM) need to tweak a parameter and send. Saving is disabled, with a "Duplicate to personal" nudge. |
| **D18** | Deleting a team | (a) immediate, type to confirm; (b) 30-day soft delete with restore | **(a) for v1.** Members keep local copies per D5, and D1 Time Travel is an operations backstop. |
| **D19** | Who may include credentials in team share links | (a) any editor (as anyone with the token can today); (b) admins only; (c) a team policy toggle | **(a) for v1.** The creator is recorded and visible (AC-TSHARE-2). The policy toggle is Later. |
| **D20** | Legacy migration path | (a) client-side "Merge into personal", with the legacy token kept in shares-only mode; (b) a server-side claim that moves ownership in place and invalidates `rh_` | **(a) for v1.** It needs no new Worker endpoint and no token-invalidation semantics, and repeat merges converge by id. (b) is Later if users ask for one place to manage old share links. |
| **D21** | Removed members and invite links | (a) block invites created before the removal; (b) permanent ban until an admin lifts it; (c) no block, admins revoke links by hand | **(a)**, together with the default-on "revoke active links" checkbox and multi-use links capped at editor. It closes the rejoin hole, and a deliberate re-invite still works. |

### 16.1 Consistency rulings (QA round 2; binding on the UX and tech docs)

| # | Conflict | Ruling |
|---|---|---|
| R1 | Timings had no conditions (G1, AC-AUTH-3, AC-INV-3) | All timings are p95 on a local Worker with ≤ 200 entities, or on the mock backend (§6 preamble). G1 is a goal; its measurable proxy is AC-AUTH-3. |
| R2 | "No other screen prompts" versus the UX's Share and Join entry points | The four entry points in AC-AUTH-1 are the complete list. |
| R3 | Footer "Sync off" (spec) versus hidden (UX) | Follow DESIGN §5.14: hidden when nothing is connected; "Sync off" only for unauthorized or error. |
| R4 | Paste code 9 characters (UX) versus 10 (tech); field swap versus new oob flow | 10 characters of Crockford base32, `XXXXX-XXXXX`. Field swap on the same request when a loopback listener exists; an `oob` request only when the app cannot bind a port (AC-AUTH-4). |
| R5 | Email OTP at launch (D2a) versus M4 (tech) | M4 (D2b). |
| R6 | Two release orders (§13 versus tech §11) | §13 is the only release order. |
| R7 | Clock skew for team only versus personal too | All account spaces; legacy is excluded (AC-TSYNC-5). |
| R8 | Project rename shown admin-only but not enforced | Rename is an editor-level edit. Only `project` tombstones are enforced as admin or above (§6.6, AC-ROLE-2). |

---

### Relation to the rest of the user's request
- The **body editor visual refresh** does not depend on this spec and can ship any time.
- The **gRPC and GraphQL extensions** depend only on §13 (M0). They should not start writing team-synced data until M0 is in users' hands.

Key files referenced:
- docs/PRODUCT.md
- docs/DESIGN.md
- ui/src/lib/components/Sidebar.svelte
- worker/src/workspaces.ts
- worker/src/sync.ts
- worker/src/quota.ts
- worker/src/sanitize.ts
- worker/src/snapshot.ts
- worker/src/http.ts
- worker/migrations/0001_init.sql
- worker/migrations/0002_device_id_and_quota.sql
- worker/wrangler.toml
- crates/core/src/model.rs
- crates/core/src/sync.rs
- crates/core/src/storage.rs
- crates/core/src/share.rs
- src-tauri/src/sync_rt.rs
- ui/src/lib/dialogs/SettingsDialog.svelte
- ui/src/lib/components/CollectionsTree.svelte
- ui/src/lib/api/mock/index.ts

No repo files were modified.
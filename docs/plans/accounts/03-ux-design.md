# ReqHarbor UX spec: accounts, sign-in, sync and team projects (designer pass, revision 2)

Status: this is the design draft for the dev pass. It matches **product spec revision 2**, including the R1–R8 rulings, and follows `docs/DESIGN.md` and `docs/design/tokens.css`. No repo files were changed.

Code checked for this revision:
- `ui/src/lib/components/Sidebar.svelte`:
  - `projectMenu()` at L52 is a `MenuHost` menu: the project list, then New, Rename, Duplicate and Delete for the active project.
  - `syncVisible` is at L141.
  - The footer has a `sync-warning` alert icon (L256) and a `Sync off` danger chip that opens Settings › Sync (L259–277).
- `ui/src/lib/dialogs/SettingsDialog.svelte`: 466 lines. Nav is General, Sync, Shares, Shortcuts, About (L19–23).
- `ui/src/lib/dialogs/ShareDialog.svelte`: the `share-needs-sync` empty state with "Set up sync" (L88–101).
- `ui/src/lib/stores/ui.svelte.ts`: `MenuItem` (L6), `ConfirmState` (L27), `settingsDialog.section` (L50), `anyModalOpen` (L68).
- `docs/DESIGN.md` §5.14 (sync chip), §5.15 (toasts) and §5.16 (menus).

---

## 0. Changes since my previous pass

| Product rev 2 item | UX change |
|---|---|
| Email OTP moved to M4. One sign-in method per account. No linking. | Removed the email-code hosted page and the "Sign-in methods / Link another method / Unlink" UI. Added a hosted `email_in_use` error page. |
| Paste code is 10 chars, `XXXXX-XXXXX`. The view swaps on the same request. `oob` is used only when no port can be bound. | §2 rewritten. |
| Exactly four sign-in entry points (R2). Footer follows §5.14 (R3). | Removed the switcher "Join team…" while signed out. Settings › Teams is visible while signed out. The footer chip stays hidden when nothing is connected. "Sync off" is shown only for 401 or error. |
| Account is the **first** Settings section (AC-AUTH-1). | Nav order is Account · General · Teams · Sync · Shares · Shortcuts · About. ⌘, and the gear still open General. UX-D1 is closed. |
| Hover lists each space (AC-TSYNC-1). | Dropped `SyncPopover`. The chip keeps today's click (sync now). Per-space status goes in the tooltip and in Settings › Sync. |
| Non-collapsible switcher headings. | Dropped `ProjectPicker`. The existing `MenuHost` menu is extended with heading rows. |
| Device rename, "Keep this version" and the Convert-to-secret quick-fix are Later. Moves are project-level only. Claim is a client-side merge. | Removed all of them. Collections get "Copy to project…". `ClaimDialog` is replaced by `MergeLegacyDialog` and the shares-only mode. |
| Multi-use invite links are capped at editor. Invite token sits in a URL fragment (`/i#…`). Remove confirm has a revoke-invites checkbox. | §7.3, §7.4 and §13 updated. |
| Quotas are 25 MB personal, 25 MB per team, 100 MB owner aggregate. Max 3 owned teams. New codes `server_full` and `client_too_old`. | All copy updated. New messages added in §6.3. |

---

## 1. Design stance (unchanged)

1. **Quiet.** Being signed out is a normal state. The main chrome has no banner, no badge and no sign-in button.
2. **Binding is shown once, per project**, in the switcher and on the row-1 trigger icon. Tree rows are unchanged.
3. **No new colour tokens and no new motion.**
   - Roles, bindings and attribution use text and `--text-2`/`--text-3`. Status uses `--warning` and `--danger`. Colour never carries meaning alone.
   - Waiting states are static text plus elapsed time.
4. **Copy rules.**
   - Write "Sign in" and "Sign out", "Team" and "Team project".
   - Show a server by its host only.
   - Use "token" only under Settings › Sync › Advanced.
   - Use sentence case, with curly quotes around names (as in ShareDialog).

---

## 2. Sign-in entry points (AC-AUTH-1, R2, R3)

This is the complete list. Nothing else prompts.

| # | Entry point | Shown when | Behaviour |
|---|---|---|---|
| 1 | Settings › **Account** | Always (first nav item) | `SignInFlow` (§3) |
| 2 | Settings › **Teams** | Signed out | "Sign in to create or join a team." with **Sign in** and **Join team…** |
| 3 | Share dialog, `Local only` project | The project isn't synced | **Sign in and sync to Personal**, which runs `SignInFlow` inline and then binds the project (§12.3) |
| 4 | **Join team** dialog | Opened from Teams or the avatar menu | Signs in inline first when signed out (§8) |

Navigation that leads to entry point 1 is not a fifth entry point. Two cases:
- In account mode, the danger "Sync off" chip (AC-AUTH-9) opens **Settings › Account** with the re-auth banner.
- In legacy mode it still opens Settings › Sync, exactly as today.

What is never shown:
- a startup prompt;
- a badge on the gear;
- a footer chip when nothing is connected (`syncVisible` logic is unchanged);
- a "Join team…" switcher item while signed out.

Lena's switcher and footer are therefore pixel-identical to today.

---

## 3. Sign-in flow (AC-AUTH-2…4, 9, edge cases 1 and 7)

### 3.1 `SignInFlow.svelte` (new, reusable)

It is rendered in four places: Settings › Account, Settings › Teams (signed out), the Share dialog step and the Join team dialog. It has a `heading` prop ("Sign in", "Sign in to share", "Sign in to join").

```
idle ─Sign in─▶ checking (GET /v1/auth/providers)
                 ├ unreachable ▶ error:offline
                 ├ no providers ▶ error:none
                 └ ok ▶ waiting   (browser opened; loopback listening, or oob if no port)
waiting ─loopback hit─▶ exchanging ─▶ success
waiting ─Paste code instead─▶ paste (same request; listener stays open) ─Continue─▶ exchanging
paste ─Back─▶ waiting          waiting|paste ─Cancel─▶ idle
waiting|paste ─10 min─▶ error:timeout          exchanging ─fail─▶ error:exchange
paste ─loopback hit arrives anyway─▶ exchanging   (whichever completes first wins)
```

### 3.2 Wireframes (Settings content column, about 550 px)

**Idle (signed out)**
```
Account                                                    (h3, --fs-xl)
Sign in to sync across devices and work with a team.       (--text-2)
[ Sign in ]                                                (btn-primary, data-testid=account-signin)

Server  reqharbor.app                Use a different server   (.link)
        └ expands: [ https://api.acme.dev            ]  (input.mono, 360px)
          Only sign in to servers you trust. Your synced data is stored there.   (--text-3, 11.5)
─────────────────────────────────────────────── (hairline)
Without an account, ReqHarbor works fully offline. Nothing is sent
anywhere until you sign in or turn on sync.               (--text-3, 11.5)
```
If the build ships without a default server (D14 option b), the server field starts expanded and empty, and **Sign in** stays disabled until the URL is valid.

**Waiting (loopback)**
```
  Waiting for you to finish in your browser…              (--fs-lg semibold, aria-live=polite)
  We opened reqharbor.app in your browser. Come back here when it says you're signed in.
  Waiting 0:42                                            (--text-3, tabular-nums, aria-hidden)
  [ Open browser again ]  [ Paste code instead ]  [ Cancel ]
   btn-secondary           btn-ghost (testid signin-paste)  btn-ghost (testid signin-cancel)
```
- **Open browser again** reopens the same authorize URL.
- If the app could not bind a loopback port, it skips this view and opens the **paste** view directly (`oob` request). The helper line becomes "Your browser will show a code when you're done."

**Paste code**
```
  Paste the code from your browser
  [ XXXXX-XXXXX ]    (input.mono, 200px, autofocus, maxlength 13, autocomplete=off, spellcheck=false)
  You'll find it under "You're signed in" in your browser.            (--text-3)
  [ Continue ] (primary, enabled at 10 valid chars)   [ Back ]
```
- The input uppercases as you type and accepts any mix of case, hyphens and spaces. It shows the hyphen after the 5th character.
- Characters outside Crockford base32 (I, L, O, U, punctuation) are not silently dropped. They give the inline hint "Codes use letters and digits only". Crockford aliases are normalised: `O→0`, `I/L→1`.
- **Back** returns to waiting, while the loopback stays open the whole time.

**Exchanging:** buttons are disabled and the line reads "Signing in…". No animation.

**Success:**
- The app window is raised and focused (Tauri `set_focus`) when the loopback hit arrives.
- The section switches to the signed-in view, and a toast reads "Signed in as Priya Shah".
- The calling flow then continues: first-sync checklist (§10), Join preview (§8) or Share form (§12.3).

**Errors** (`.hint.danger`, `role="alert"`, above the button row):

| Case | Copy | Action |
|---|---|---|
| Providers unreachable / DNS / 5xx | "Can't reach reqharbor.app. Check your connection and try again." | **Try again** |
| No providers configured | "This server doesn't have sign-in set up. Ask its admin, or sync with a token under Sync › Advanced." | link to Sync |
| Timeout (10 min) | "Sign-in timed out. Try again." | **Sign in** |
| Bad or expired code | "That code didn't work. Codes are single-use and expire after 60 seconds. Start again from your browser." | **Start again** (new request) |
| Provider denied / cancelled | "Sign-in was cancelled in the browser." | **Sign in** |
| Hosted page refused (`email_in_use`, no verified email) | "Sign-in didn't finish. Your browser shows why." The specific reason lives on the hosted page (§13), so the app never echoes the email. | **Sign in** |
| `rate_limited` | "Too many sign-in attempts. Try again in a minute." | none |
| `server_full` (account creation) | "This server isn't accepting new accounts right now." | none |
| Keychain write denied | "ReqHarbor couldn't save your sign-in to the system keychain." | **Try again** |

**Cancel** closes the listener and drops the verifier. A late browser redirect then lands on a closed port, and the hosted page still shows its "return to ReqHarbor" text, which is harmless.

### 3.3 Keyboard and accessibility
- On entering each state, focus moves to its primary control: waiting → **Paste code instead**, paste → the input, error → the action button.
- Esc closes Settings but does **not** cancel a waiting sign-in. A later success still raises the toast.
- Buttons are at least 28 px tall.

---

## 4. Account menu and Settings › Account (AC-AUTH-7, 10, 11, AC-ACCT-1…3)

### 4.1 Footer avatar (signed in only)
```
Footer:  Env Staging ▾ ········ [⚠][☁ Synced] (PS) [⚙]
                                 warn  chip    avatar gear
```
- New `Avatar.svelte`: a 20 px circle with **initials** (`--bg-active`, `--text-1`, 10.5 semibold).
  - `aria-label="Account: Priya Shah"`, tooltip "Priya Shah · priya@acme.dev".
  - It never loads provider images (UX-D2).
- Under 260 px of sidebar width, the chip drops its label and keeps its icon plus `aria-label` (container query on `.footer`).
- Click opens a `MenuHost` menu:
```
│ Priya Shah                        │  heading (not focusable, never highlighted)
│ priya@acme.dev · reqharbor.app     │  sublabel --text-3 11.5
├────────────────────────────────────┤
│ Account settings…                  │
│ Teams…                             │
│ Join team…                         │
├────────────────────────────────────┤
│ Sync now                           │
├────────────────────────────────────┤
│ Sign out…                          │
```

### 4.2 Settings › Account, signed in
```
Account
(PS)  Priya Shah                                      [ Sign out… ]  (btn-secondary)
32px  priya@acme.dev · Signed in with GitHub · reqharbor.app        (--text-3)

DEVICES                                               (h4 --fs-sm --text-2 semibold)
  MacBook Pro (macOS)   This device   Signed in Nov 2 · Active today
  ubuntu-desk (Linux)                 Signed in Oct 14 · Active Nov 20      Sign out
  (28px rows, hairline separators, .tbl styling as Shares; no rename in v1)

DANGER ZONE
  [ Delete account… ]   (btn-ghost btn-danger)
  Deletes your account, personal space and its share links on reqharbor.app.
  Projects on this device stay here as "This device only".         (--text-3)
```
**Banners** sit at the top of the section, in priority order:

| Condition | Style | Copy | Action |
|---|---|---|---|
| Session ended (AC-AUTH-9) | `.hint.danger` | "Your session ended. Sign in again to keep syncing. Your changes are saved on this device." | inline **Sign in again**, which starts `SignInFlow` |
| Keychain unreadable (edge case 7) | `.hint.danger` | "ReqHarbor couldn't read your sign-in from the keychain." The section then renders the signed-out idle view under this banner. | none |
| File fallback (AC-AUTH-7) | `.hint.warn` | "Your sign-in is stored in a file because no system keychain was found." | none |
| Provider email changed to one in use (edge case 9) | `.hint` | "Your GitHub email changed; this account still uses priya@acme.dev." | none |

**Device sign-out** uses `ui.confirm`:
- Title: "Sign out ubuntu-desk?"
- Body: "It stops syncing on its next request. Its local data stays."
- Confirm label: **Sign out device**.

**States:**
- Loading: "Loading…" in `--text-3` after 300 ms.
- Offline: cached profile, plus the line "Offline · showing what was last synced". Mutations are `aria-disabled` with "You're offline".

### 4.3 Sign out on this device (AC-AUTH-11)
`ConfirmDialog`, 420 px, not danger:
- Title: "Sign out on this device?"
- Body: "Your projects stay on this device. Syncing pauses until you sign in again."
- When there are unpushed edits, add: "**2 changes** haven't synced yet. They'll sync when you sign back in."
- Buttons: **Cancel** (initial focus), **Sign out**.

Afterwards:
- The avatar disappears.
- The footer chip is hidden, because no space is connected (§5.14), unless a legacy workspace is still connected.
- In the switcher, account-bound groups stay but get the heading suffix "· Paused: signed out", and their row icons become `cloud-off`.

**Signing in as a different account** opens `SpaceNoticeDialog` in "different account" mode (480 px):
```
You signed in as sam@acme.dev                                ×
These projects were synced by priya@acme.dev. Choose what happens on this device.
  Personal (4 projects)   [ Keep as local-only copy ▾ ]
  Acme (2 projects)       [ Keep as local-only copy ▾ ]   (options: Keep as local-only copy · Remove from this device)
                                        [ Cancel sign-in ]  [ Continue ]
```
- **Cancel sign-in** signs the new account out.
- **Continue** shows no further confirm when everything is kept. When anything is set to Remove, a danger confirm follows: "Remove 2 projects from this device?"

### 4.4 Delete account (AC-ACCT-2, 3)
Extend `ConfirmDialog` with `typeToConfirm` and `details: string[]`. The danger primary unlocks when the input matches, ignoring case and surrounding whitespace.
```
Delete your account?                                          ×
This immediately deletes on reqharbor.app:
 • the team "Side project" (you're its only member)          ← listed first
 • your account and 2 signed-in devices
 • your personal space (4 projects) and its 3 share links
Share links you created in other teams stay, shown as "Former member".
Nothing on this device is deleted. Your projects stay here as "This device only".
There's nothing to export: your data is already on your devices.
Type priya@acme.dev to confirm  [                         ]
                                   [ Cancel ]  [ Delete account ]  (btn-danger-solid)
```
**Blocked** (last owner of a team with other members): the input and primary are replaced by `.hint.danger`: "You're the last owner of **Acme**." Each blocking team has the links **Transfer ownership** and **Delete team**, which open Teams › that team.

---

## 5. Project switcher (AC-PROJ-1…3)

### 5.1 It stays a `MenuHost` menu
The headings are fixed and non-collapsible, so a menu with non-focusable heading rows is enough:
- `MenuHost` already scrolls (`max-height: calc(100vh - 16px)`, which covers edge case 8).
- Type-ahead already works.
- Role chips sit only on headings, which are never highlighted, so the accent-background contrast concern goes away.

**Row-1 trigger:**
```
[⛶ Payments API ▾] [View only]                        [⤓]
 binding icon       chip (viewers only)
```
- The binding icon replaces `layers`. Signed out with no bindings, it stays `layers` (zero change for Lena).
- Tooltip examples: "Payments API · Acme · Editor · Synced" and "Scratch · This device only".

**Menu, signed in:**
```
│ PERSONAL                               │  heading: 10.5 uppercase +0.06em --text-3
│ ☁  Default                          ✓ │
│ ☁  Side quests                         │
│ ACME                          Editor   │  heading + role text right (--text-3)
│ 👥 Payments API                        │
│ 👥 Internal tools                      │
│ GLOBEX                        Viewer   │
│    No projects yet                     │  disabled muted row for an empty team
│ ON THIS DEVICE ONLY                    │
│ ▭  Scratch                             │
│ LEGACY WORKSPACE                       │  only when present
│ 🔑 Old sync                            │
├────────────────────────────────────────┤
│ New project…                           │
│ Join team…                             │
├────────────────────────────────────────┤  actions for the ACTIVE project (today's block, extended)
│ Rename                              F2 │
│ Duplicate                              │
│ Sync location…                         │  signed in, or project has a binding
│ Move to team…                          │  Personal / Local only project, editor+ in ≥1 team
│ Delete…                                │  team project: disabled for editor/viewer, reason shown
```
- **Signed out:** a flat project list exactly as today, then New project… / Rename / Duplicate / Delete…. No Join team and no headings.
- **Empty groups:** headings are rendered only when the group has projects. Exceptions: teams, which always render with the "No projects yet" row, and "On this device only" while signed in.
- **Binding icons:** new `Icon.svelte` paths `users`, `device` and `key` (16 px, 1.5 stroke, `--text-3`), plus the existing `cloud` and `cloud-off`.
- **Disabled-with-reason:** "Only admins can delete team projects" and "Viewers can't change team projects".

### 5.2 New project dialog (AC-PROJ-2), signed in only
Signed out keeps today's instant create plus inline rename. Signed in opens a 420 px dialog:
```
New project                                        ×
Name      [ New project                 ]
Location  [ Personal                  ▾ ]  Personal · Acme · Internal · Globex (viewer, disabled) · This device only
          Synced to your devices.           ← changes: team "Everyone in Acme can see it." · local "Never leaves this device."
                                  [ Cancel ] [ Create ]
```
- The default is Personal (D11).
- Enter submits.
- `server_full` or a quota error at upload time: the project is still created locally, with the warning toast "Saved on this device. Personal storage is full (25 MB)."

### 5.3 Sync location dialog (AC-PROJ-3), 460 px
Stacked `.cards/.card` radios, reused from Settings › Sync:
```
Sync location for "Payments API"                       ×
( ) Personal              Synced to your devices
(•) Acme                  Everyone in Acme (8 members) can see it
( ) Globex                Viewers can't add projects        (disabled card)
( ) This device only      Never leaves this device
```

| Transition | UI |
|---|---|
| Local only → Personal | Primary **Start syncing** |
| Personal → This device only | Inline `.hint.warn` "Removes it from your other devices. This copy stays." Primary **Stop syncing** |
| Any → Team | Primary **Review and move…**, which opens the pre-flight (§9) |
| Team → anything, as editor | Card disabled: "Only admins can move a project out of Acme. Use Copy to project… instead." |
| Team → Personal / Local, as admin+ | `.hint.warn` "Removes it from Acme for all 8 members." Primary **Move** (danger) |

---

## 6. Collections tree, request pane and sync status

### 6.1 Context menu additions (`CollectionsTree.svelte` `menuFor`)

| Node | Personal / Local / Legacy project | Team project, editor+ | Team project, viewer |
|---|---|---|---|
| Collection | + **Copy to project…** | + **Copy to project…** | Only Open, Settings (read-only) and **Duplicate to personal…** are enabled. The rest are disabled with a reason. Share… gives "Viewers can't create share links". |
| Folder | + Copy to project… | + Copy to project… | as above |
| Request | + Copy to project… | + Copy to project… | Open, Copy as cURL and Duplicate to personal… enabled |

**`CopyToProjectDialog`** extends the `PickerDialog` pattern. It has one component and two labels:
- **Copy to project…**:
  - Target select lists every project I can write to (Personal, Local, Legacy, team editor+).
  - Below it is the collection/folder tree of the chosen project.
  - Primary **Copy**.
  - When the target is a team project, the primary becomes **Review and copy…**, which opens the pre-flight (§9).
- **Duplicate to personal…** (viewers and team items): the same dialog with targets limited to Personal and This device only projects. Primary **Duplicate**.
- Result toast: "Copied to Side quests", with action **Open**.

**Cross-binding drag (AC-PROJ-7).** The tree shows one project, so a cross-binding drag can only happen by dropping onto the switcher trigger. The trigger is not a drop target. While dragging over it, a tooltip shows "Use Copy to project… or Move to team…".

### 6.2 Viewer affordances (AC-ROLE-1, D17)
- **Line under the Collections filter row** (24 px, `lock` icon, `--text-3` 11.5): "View only in Acme · Duplicate to personal to make changes".
- **Edits are scratch.**
  - Save is `aria-disabled`, with the tooltip "Viewers can't change team projects. Duplicate to personal to experiment."
  - The dirty dot still shows.
  - ⌘S raises the same text as an info toast, with the action **Duplicate to personal**.
  - Send works.
- No drag. F2 and double-click rename are suppressed, with the same tooltip.

### 6.3 Device-local values and newer-client items
- **Team secret with no local value:** the Env grid placeholder shows "Set your value" in `--text-3` italic. The lock tooltip reads "Secret: set locally by each member".
- **File body or form file without a path:** the drop zone reads "File not set on this device" with **Choose file…**.
- **Unknown `kind` / `body.type` (AC-FWD-3):**
  - The tree method column shows the raw kind uppercased and cut to 4 chars (`GQL`, `GRPC`), or `NEW` when absent, in `--method-custom`.
  - A 14 px `alert` badge carries the tooltip and `aria-label` "Requires a newer ReqHarbor".
  - The pane renders read-only. The body area shows the `.empty` state "This request needs a newer version of ReqHarbor" with **Check for updates**.
  - Send and Save are disabled, and the context menu allows only Open and Delete (editor+).

### 6.4 Conflict copies (AC-TSYNC-4, AC-TEAM-8)
- **Row:** the name keeps the "(conflict)" suffix. A `--warning` `alert` icon follows it, with `aria-label="Conflict copy"`, and the tooltip "Conflict copy · Sam's change was newer and kept as the original." Text plus icon, never colour alone.
- No extra menu items in v1: Delete and Rename already cover clean-up.
- **One aggregated warning toast per sync run.**
  - One conflict: "Sam's change to "Create order" was newer. Your version was kept as "Create order (conflict)"."
  - Several: "3 conflicts in Acme. Your versions were kept as copies."
  - Action **Show** (8 s) switches to the project, expands the parents and selects the first copy.

### 6.5 "Edited by" (AC-TEAM-8, AC-TSYNC-10)
```
Payments › Orders › Create order •           Edited by Sam · 3h    [Save ⌘S] [⋯]
```
- `--text-3` 11.5, with `tabular-nums` on the time. Tooltip: "Last edited by Sam Lee · Nov 26, 14:02".
- Hidden when the pane is under 520 px wide (container query).
- Omitted when I made the last edit, and in personal and legacy projects.
- Deleted users show "Former member".

### 6.6 Footer sync chip (DESIGN §5.14 kept; R3)

| State | Chip | Click |
|---|---|---|
| Nothing connected (incl. signed out) | hidden | none |
| All ok | `cloud` "Synced", `--text-3` | sync now (unchanged) |
| Syncing | same icon at 60 % opacity | none |
| Offline / 5xx | `cloud-off` "Offline", `--warning` | sync now |
| Any space 401 after refresh (Sign-in required), or error | `cloud-off` "Sync off", `--danger` | account mode → Settings › Account (banner); legacy → Settings › Sync (today) |
| Some spaces have issues (quota, `server_full`, clock skew, removed, `client_too_old`) | the existing separate `sync-warning` alert icon left of the chip, plus the chip's own state | the icon opens Settings › Sync |

The chip shows the **worst** state.

**Tooltip** (also shown on focus, per §5.17). It is multi-line, one line per space, and the same text goes into the chip's `aria-label`:
```
Synced 1 min ago — click to sync now
Personal · Synced
Acme · Synced
Globex · Team storage full (25 MB)
Legacy · Synced
```

### 6.7 Per-space messages
These appear in the tooltip, in the Settings › Sync space rows and, where noted, in a toast.

| Condition | Copy | Toast? |
|---|---|---|
| `quota_exceeded` (space) | "Team storage full (25 MB). Ask an admin to clean up." / "Personal storage full (25 MB)." | once per space per session |
| `quota_exceeded` (owner aggregate) | "This team has reached its owner's storage limit." (owner sees "You've reached your 100 MB storage limit.") | once |
| `server_full` 507 | "The sync server is out of space. Your changes are saved on this device." | once |
| `clock_skew` | "Your computer's clock is ahead by about 7 minutes. Changes can't sync until it's fixed." | once per session |
| `client_too_old` 426 | "Update ReqHarbor to keep syncing Acme." with **Check for updates** | once |
| `forbidden_role` after a downgrade | "Your role in Acme changed to Viewer." (banner §6.8) | no |
| Removed / deleted | "You were removed from Acme · Decide" (link reopens §6.9, for 7 days) | dialog |
| Project moved to a team I'm not in (edge case 2) | info toast: ""Payments API" moved to Acme; you're not a member. It's kept on this device only." | yes |

### 6.8 Role-changed banner (AC-ROLE-3)
A persistent `.hint.warn` at the top of the Collections list for that project, with `role="status"`. It stays until one action is chosen.
```
⚠ Your role changed to Viewer. You have 3 unsynced changes.
  [ Save as personal copy ]  [ Discard ]          (btn-sm)
```
- **Save as personal copy** opens Duplicate to personal, preselected.
- **Discard** opens a danger `ui.confirm`: "Discard 3 unsynced changes?"

### 6.9 Removed from team / team deleted (AC-TEAM-4/5/7, D5, edge case 3)
`SpaceNoticeDialog`, 440 px. It is modal and opens once, when sync first detects the event.
```
You were removed from Acme                                     ×
Acme's projects no longer sync to this device.
2 projects are still here: Payments API, Internal tools.
4 changes hadn't synced yet.                          (only if any)
                      [ Keep a personal copy ]  [ Remove from this device ]
```
- **Defaults** (primary, initial focus):
  - removed by an admin, or team deleted → **Remove from this device**;
  - left voluntarily → **Keep a personal copy**.
- **Title variant:** "Acme was deleted".
- **× means "decide later".** The Sync space row then shows "Removed from Acme · Decide" for 7 days. After 7 days the default is applied.
- **Kept copies** become `This device only` projects named "Payments API (from Acme)".
- **Unsynced edits:** when there are any and the user picks Remove, the dialog first offers "Save 4 changes as a personal copy?"

---

## 7. Teams (AC-TEAM-1…7, AC-INV-1/6, role table)

### 7.1 Settings › Teams

**Signed out:**
```
Teams
Sign in to create or join a team.
[ Sign in ]  [ Join team… ]          (SignInFlow renders inline on Sign in)
```

**Signed in:**
```
Teams                                                [ Create team ]  [ Join team… ]
  Acme       Owner    8 members    12.4 MB of 25 MB    ›
  Globex     Viewer   23 members    3.1 MB of 25 MB    ›
  (36px rows; whole row is a button; data-testid=team-row)
Your storage: 61 MB of 100 MB (personal space + 2 teams you own)     (--text-3; owners only)

Team data is visible to all members and stored on reqharbor.app.
Use secret variables for tokens and passwords.                       (--text-3 11.5, PRIV-3)
```

**Empty:** `.empty` with title "No teams yet" and text "Create a team to share projects with teammates, or join one with an invite link." Buttons **Create team** (primary) and **Join team…**.

**Create team dialog (420 px):**
- Field: "Team name", 1–80 chars. The counter appears past 70 chars.
- Primary: **Create team**. It is disabled at the limit, with the reason "You can own up to 3 teams on this server."
- On success it opens the team detail with the hint: "Next: move a project into Acme, or invite teammates." Buttons **Move a project…** (Sync location for the active project) and **Create invite link**.

### 7.2 Team detail (`settingsDialog = {section: "teams", teamId}`)
```
‹ Teams                                                      (.link; Alt+← also goes back)
Acme                                          Owner          (h3 + role text)
8 members · 12.4 MB of 25 MB  [██████░░░░░░]                 (reuse .usage/.meter)

MEMBERS                                               [ Invite… ]
 (PS) Priya Shah (you)  priya@acme.dev   Owner  ▾   Joined Oct 2    ⋯
 (SL) Sam Lee           sam@acme.dev     Editor ▾   Joined Nov 3    ⋯
 (OO) Olu Okafor        olu@acme.dev     Viewer ▾   Joined Nov 20   ⋯
   role: native <select> (.select-wrap, 110px) for permitted actors; plain text otherwise
   ⋯: Transfer ownership… (owner, others' rows) · Remove from team… (admin+, danger)
   own row ⋯: Leave team…

INVITE LINKS                                          (admin+ only)
  Editor · 1 of 1 left · expires in 6 days · by Priya          Revoke
  Viewer · unlimited · expires Dec 20 · by Sam                  Revoke
  (empty: "No active invite links.")

TEAM
  Name  [ Acme                 ]  [ Save ]     (admin+; others see text)

DANGER ZONE
  [ Leave team… ]    [ Delete team… ] (owner)
```

**Role select**
- Applies on `change`, with the toast "Sam is now Admin" and **Undo** (8 s).
- **Options above the actor's power are disabled.**
  - Admins can't grant Owner.
  - Only owners can grant or revoke owner.
- **Last owner:** the select and Remove are disabled with the reason "A team needs at least one owner". A server `last_owner` maps to the same copy.

**Transfer ownership… dialog (420 px)**
- A member select, and the copy "They become owner. You'll become an admin. Acme's 12.4 MB will count against their storage."
- Primary **Transfer**.
- `quota_exceeded` gives `.hint.danger`: "Sam doesn't have room for Acme. They need 8.1 MB more."

**Leave team…** uses `ConfirmDialog` with a checkbox:
- Body: "Leave Acme? Its projects stop syncing here."
- Checkbox: "Keep a personal copy of 2 projects", checked (D5).
- A last owner sees instead: "You're the last owner. Transfer ownership or delete the team first." with links.

**Delete team…** is type-to-confirm with the team name (§4.4 pattern). Details:
- "2 projects and everything in them"
- "8 members lose access now"
- "5 share links and 2 invite links stop working"

Members keep local copies per D5.

**Viewers and editors** see members read-only (no selects; ⋯ only on their own row). The invite section is hidden.

**Offline and loading:** same rules as §4.2.

**Accessibility:**
- Members render as a `<table>` with a visually hidden caption "Members of Acme".
- Labels: `aria-label="Role for Sam Lee"` and `"More actions for Sam Lee"`.
- After a removal, focus moves to the next row's ⋯, or to **Invite…** if the list is empty.

### 7.3 Remove member (AC-TEAM-4, D21)
`ConfirmDialog` (danger), extended with `checkbox`:
```
Remove Sam Lee from Acme?                                     ×
Sam loses access now. Copies already on their devices can't be wiped.
They can rejoin only with an invite link created after today.
[✓] Also revoke all active invite links (3)
                                         [ Cancel ]  [ Remove ]
```
- The checkbox is checked by default whenever any multi-use link is active, and unchecked otherwise.
- It is hidden when there are no active links.
- Initial focus is on Cancel.

### 7.4 Invite dialog (AC-INV-1/2/6), 460 px
```
Invite to Acme                                          ×
Uses      [ 1 person ▾ ]          1 person · 5 people · 25 people · Unlimited
Role      [ Editor ▾ ]            Viewer · Editor · Admin
          Admin links are single-use.     (--text-3; shown when Uses > 1 and Admin disabled)
Expires   ( 1 day | 7 days | 30 days )    (segmented, DR-002)
Anyone with the link can join as Editor until Dec 3.   (--text-3, live)
                                    [ Cancel ] [ Create link ]
```
- **Uses comes first,** because it constrains Role.
  - Choosing Uses > 1 while Admin is selected resets Role to Editor. The hint line announces this through `aria-live`.
  - For non-owners, Admin is disabled with "Only owners can create admin links".
- **Defaults:** Editor, 7 days, 1 person.
- **Result state** (ShareDialog `.result` pattern):
```
[ https://reqharbor.app/i#k3J9…Qz            ] [ Copy ]
⚠ Copy it now. For security, the link can't be shown again.     (.hint.warn)
They'll open ReqHarbor › Teams › Join team and paste it.         (--text-3)
                                                     [ Done ]
```
- The link is auto-copied, with the toast "Invite link copied".
- **Errors:**
  - limit: "Acme already has 20 active invite links. Revoke some first."
  - rate: "You've created a lot of invites today. Try again tomorrow."
- **Revoke** is immediate, with the toast "Invite link revoked" (no Undo).

---

## 8. Join team dialog (AC-INV-3…5), 480 px

**Step 1, paste:**
```
Join a team                                             ×
Invite link  [ https://reqharbor.app/i#…             ]   (autofocus, mono, data-testid=join-team-input)
             Paste the link a teammate sent you.
                                     [ Cancel ] [ Continue ]
```
- The dialog never reads the clipboard without a gesture.
- **Validation:**
  - Not an `/i#` link, or the fragment is missing: "That isn't a ReqHarbor invite link."
  - Host differs from the signed-in server: `.hint.warn` "This invite is for api.acme.dev. You're signed in to reqharbor.app." with **Sign in to api.acme.dev**, which signs out, switches server and runs `SignInFlow`.

**Step 1b, signed out:** inline `SignInFlow` with the heading "Sign in to join". On success it continues automatically.

**Step 2, preview** (`POST /v1/invites/preview`):
```
Join Acme?
  Priya Shah invited you · you'll join as Editor · link expires Dec 3
  Team data is visible to all members and stored on reqharbor.app.
                                     [ Cancel ] [ Join Acme ]
```
**Success:** the dialog closes. The toast "Joined Acme · syncing 2 projects" carries **Show**, which opens the switcher. The projects appear under the Acme heading.

**Errors:**

| Case | Copy |
|---|---|
| `invite_invalid` (expired, revoked, used up, predates removal) | "This invite link is no longer valid. Ask Priya Shah for a new one." When the preview failed, so no inviter is known: "Ask the person who sent it for a new one." |
| Already a member | `.hint` "You're already in Acme as Admin. Your role hasn't changed." Primary becomes **Open Acme**. |
| Team full | "Acme has reached 50 members." |
| Offline | "Can't reach reqharbor.app." with **Try again** |

---

## 9. Move to team / copy pre-flight (AC-PROJ-4/6, D8), 620 × up to 560

Its layout follows ShareDialog's "Removed before sharing" section (h4 plus `.irow` icon rows), and it calls the same `share.rs` scanner that `share_preview` uses.
```
Move "Payments API" to a team                                    ×
Team   [ Acme ▾ ]                                (editor+ teams only)

NEVER UPLOADED
 🔒 4 secret variables. Each member sets their own value.
 ▭  2 file paths stay on this device.

VISIBLE TO EVERYONE IN ACME (8 MEMBERS)
 ⚠ 3 credentials typed as plain text:
    Create order · Authorization header
    Login · Basic auth password
    Webhooks · Cookie header
   Teammates will see these values. Replace them with secret variables
   before moving if they're personal.                       (--text-2)
 (clean: "✓ No plain-text credentials found")

Your other devices will see this project move to Acme.     (--text-3)
                                         [ Cancel ] [ Move to Acme ]
```
- Each credential row is a link that closes the dialog and opens that request on the right tab (Headers or Auth), so the user can fix it and then come back.
- **Copy mode** (Copy to project with a team target):
  - The title is "Copy "Orders" to "Payments API" (Acme)".
  - It adds **UNRESOLVED VARIABLES**: "{{baseUrl}}, {{tenant}} aren't defined in Payments API's environments. Environments aren't copied."
  - The primary is **Copy to Acme**.
- **Progress:** "Moving… 42 of 120 items". Primary disabled; Cancel is enabled until the push starts.
- **Errors:**
  - `quota_exceeded`: "Acme's storage is full (25 MB). Ask an admin to clean up."
  - `server_full`: the §6.7 text.
  - offline: "You're offline. Nothing was moved."
- ⌘Enter submits.

---

## 10. First sign-in: what to sync (AC-ONB-5/6, D12), 520 px
```
Sync these projects to your account?                          ×
Checked projects sync to your devices. Unchecked ones stay on this
device only. You can change this later per project.
[✓] Select all
[✓] Default            3 collections · 41 requests
[✓] Payments API       2 collections · 18 requests
[ ] Client X (NDA)     1 collection  · 6 requests    This device only (--text-3, when unchecked)
                          [ Keep all on this device ]  [ Sync 2 projects ]
```
- Legacy-bound projects are not listed; a footnote reads "Projects synced with a token are handled under Sync › Merge."
- Nothing uploads before confirm. × behaves as **Keep all on this device**, followed by the toast "Your projects stay on this device. Change any project's sync location from the project switcher."
- **Fresh device** (only an untouched Default): no dialog. Toast: "Signed in · syncing 4 projects from your account and 1 team."

---

## 11. Settings › Sync, reworked (AC-ONB-2/3/4, AC-TSYNC-1)

**Signed in:**
```
Sync
☁ Synced 1 min ago                                        [ Sync now ]
SPACES
  ☁ Personal   4 projects   12.0 MB of 25 MB  [████░░░░]
  👥 Acme       2 projects    8.1 MB of 25 MB  [███░░░░░]   Team settings ›
  👥 Globex     1 project     ⚠ Team storage full (25 MB). Ask an admin to clean up.
  (per-space lines from §6.7; "Removed from Acme · Decide" rows for 7 days)
▸ Advanced: sync without an account        (disclosure, collapsed)
```

**Signed out, no legacy connection:** "Sign in to sync across devices. → Account" (link). Below it, the Advanced disclosure holds today's UI unchanged: Worker URL, the Create workspace and Connect with token cards, the token box and Rotate/Disconnect.

**Signed out, legacy connected:** today's UI, shown expanded.

**Signed in plus a legacy connection:** a block above Spaces:
```
LEGACY WORKSPACE
  🔑 Synced with a token · 3 projects            [ Merge into my personal space… ]
```

**`MergeLegacyDialog` (AC-ONB-3), 480 px, not danger:**
```
Merge into your personal space?                                ×
Your 3 token-synced projects and your Global environment move into
your personal space. Nothing is deleted on the old workspace, and its
share links keep working.
Other devices using this token keep syncing to the old workspace
until you sign them in and merge there too.
                                       [ Cancel ]  [ Merge ]
```
- Progress: "Merging… 40 of 96 items".
- Result toast: "Merged into your personal space."
- Partial failure (quota, offline): projects already pushed are rebound. The rest stay `Legacy`, with the hint "2 projects couldn't be merged yet. Try again." and the button **Merge again**. Merging again is safe, because entities keep their ids.

**After a merge (shares-only mode):**
```
LEGACY WORKSPACE
  🔑 Shares only · syncing stopped · 7 share links          Manage in Shares ›
     [ Disconnect legacy workspace… ]   (btn-ghost btn-danger)
```
**Disconnect** uses a danger `ui.confirm`: "Forget the token? Its 7 share links keep working, but you can no longer update or revoke them from ReqHarbor." Confirm label **Disconnect**.

---

## 12. Shares and the Share dialog (AC-TSHARE-1…4)

### 12.1 Settings › Shares
- The table gains a **Created by** column ("You", "Sam Lee", "Former member").
- Rows are grouped under uppercase 10.5 headers: PERSONAL, ACME, …, LEGACY. The Legacy group persists in shares-only mode.

### 12.2 Permissions
**Update** and **Revoke** are `aria-disabled` with a reason when the role doesn't allow them:
- "Only admins can revoke others' links"
- "Viewers can't change share links"

### 12.3 Share dialog empty state (replaces `share-needs-sync` copy)

| State | Copy | Actions |
|---|---|---|
| `Local only`, signed in | "Sharing needs this project to sync. Change its sync location to Personal first." | **Sync to Personal** (primary): binds, toasts "Payments API now syncs to Personal", then continues into the share form. **Choose location…** (link): §5.3. |
| `Local only`, signed out | same text | **Sign in and sync to Personal**: inline `SignInFlow`, then as above. "Sync with a token instead" (link): Settings › Sync › Advanced. |
| Team project, viewer | — | "Share…" is never reached. The menu item is disabled with "Viewers can't create share links". |

In account spaces the share form's options are loaded from the server's share record, so "Update share" reproduces them.

---

## 13. Hosted pages (Worker, server-rendered)

The pages reuse `worker/src/share-page.ts`: `PAGE_CSS`, `LOGO_SVG`, the `.brand` header, CSP, `noindex` and `X-Frame-Options: DENY`. Layout and assets:
- a 400 px centred column, OS theme only;
- provider logos as inline SVG;
- no third-party resources.

```
Sign in (/v1/auth/authorize)        Done (loopback)                 Code only (oob)
┌───────────────────────────┐  ┌─────────────────────────────┐  ┌─────────────────────────────┐
│ ⛵ ReqHarbor               │  │ ✓ You're signed in           │  │ Almost done                  │
│ Sign in to ReqHarbor       │  │ You can close this tab and   │  │ Paste this code in ReqHarbor:│
│ on reqharbor.app           │  │ return to ReqHarbor.         │  │  K7Q4M-ZX2P8     [ Copy ]    │
│ [ Continue with GitHub ]   │  │ ───────────────────────────  │  │ Expires in 60 seconds.       │
│ [ Continue with Google ]   │  │ App didn't notice? Paste:    │  └─────────────────────────────┘
│ Only methods this server   │  │  K7Q4M-ZX2P8     [ Copy ]    │
│ supports are shown.        │  │ Expires in 60 seconds.       │
└───────────────────────────┘  └─────────────────────────────┘

Email in use (email_in_use)                     No verified email
┌──────────────────────────────────────────┐   ┌──────────────────────────────────────────┐
│ An account with p…@acme.dev already exists│   │ Your GitHub account has no verified email │
│ Sign in with GitHub instead.              │   │ Add one and try again.                    │
│ [ Sign in with GitHub ]  (back to start)  │   │ [ Try again ]                             │
│ You can close this tab.                   │   └──────────────────────────────────────────┘
└──────────────────────────────────────────┘

Invite (/i#token)                               Invalid invite
┌──────────────────────────────────────────┐   ┌──────────────────────────────────────────┐
│ ⛵ ReqHarbor                              │   │ This invite link is no longer valid.      │
│ Priya Shah invited you to join Acme       │   │ Ask the person who sent it for a new one. │
│ as Editor · expires Dec 3                 │   └──────────────────────────────────────────┘
│ Open ReqHarbor › Teams › Join team        │
│ and paste this link.     [ Copy link ]    │
│ Don't have ReqHarbor? reqharbor.app       │
└──────────────────────────────────────────┘
```
- **Code:** shown in mono 20 px with letter-spacing 0.08em. **Copy** confirms with "Copied". Only one copy action per page.
- **Invite page:**
  - An inline script reads `location.hash` and POSTs `/v1/invites/preview`.
  - While loading it shows "Checking invite…".
  - `<noscript>` shows only the generic instructions, with no team details.
  - **Copy link** copies the full URL, fragment included.
- **Keyboard:** fully keyboard operable, with `:focus-visible` rings matching the app.

---

## 14. Component and store changes (for dev)

**New components**

| Component | Purpose |
|---|---|
| `components/Avatar.svelte` | Initials avatar, 20 and 32 px |
| `components/SignInFlow.svelte` | The sign-in flow (§3) |
| `dialogs/NewProjectDialog.svelte` | §5.2 |
| `dialogs/SyncLocationDialog.svelte` | §5.3 |
| `dialogs/MoveToTeamDialog.svelte` | The §9 pre-flight, project and copy modes |
| `dialogs/CopyToProjectDialog.svelte` | §6.1, including the Duplicate-to-personal mode |
| `dialogs/FirstSyncDialog.svelte` | §10 |
| `dialogs/JoinTeamDialog.svelte` | §8 |
| `dialogs/InviteDialog.svelte` | §7.4 |
| `dialogs/CreateTeamDialog.svelte` | §7.1 |
| `dialogs/TransferOwnershipDialog.svelte` | §7.2 |
| `dialogs/MergeLegacyDialog.svelte` | §11 |
| `dialogs/SpaceNoticeDialog.svelte` | Removed, deleted, or different account (§4.3, §6.9) |
| Settings sub-views | `AccountSection.svelte`, `TeamsSection.svelte`, `TeamDetail.svelte` and `SyncSection.svelte`, split out of the 466-line `SettingsDialog.svelte` |

**Extensions to existing code**

`MenuItem` (`ui.svelte.ts` L6) gains:
- `heading?: boolean`, for rows that are not focusable, are skipped by roving focus and type-ahead, and never take the highlight;
- `sublabel?: string`;
- `trailing?: string`, for the role text on headings;
- `icon?: IconName`;
- `reason?: string`, rendered as `title` and `aria-description` on disabled items.

`ConfirmState` (L27) gains:
- `typeToConfirm?: string`;
- `details?: string[]`;
- `checkbox?: {label: string; checked: boolean}`.

`resolve` becomes `(r: {ok: boolean; checked?: boolean}) => void`. Alternatively, add `resolveEx` and keep the current signature for existing callers.

`ui` store:
- `ui.settingsDialog.section` (L50) adds `"account" | "teams"` and `teamId?: string`.
- New fields: `joinTeam`, `newProject`, `syncLocation`, `moveToTeam`, `copyToProject`, `firstSync`, `mergeLegacy`, `spaceNotice`, `invite`, `createTeam`, `transferOwnership`.
- Add all of them to `anyModalOpen` (L68).

`Sidebar.svelte`:
- `projectMenu()` builds grouped items.
- The "Sync off" click target depends on mode.
- The chip `title` becomes the multi-line per-space text.
- The avatar button is added.

`ShareDialog.svelte`: the empty state follows the §12.3 table.

**Icons and CSS**
- New `Icon.svelte` paths: `users`, `device`, `key`, `user`, `log-out` and `external`, all 16 px with a 1.5 stroke.
- `app.css` gets `.chip-role` (16 px, 10.5, `--text-2` on `--bg-active`, radius 3, padding 0 6px), used for "View only". No new tokens.

**Mock backend** (`ui/src/lib/api/mock`):
- a fake provider, switchable with `?mockAuth=ok|timeout|offline|paste|oob|email_in_use`;
- a seeded team "Acme" with owner, editor and viewer;
- switches for quota, `server_full`, clock skew, removal and `client_too_old`.

**Test ids:** `account-signin`, `signin-paste`, `signin-code`, `signin-cancel`, `account-avatar`, `project-group-<spaceId>`, `project-sync-location`, `team-row`, `member-role-<userId>`, `remove-revoke-invites`, `invite-create`, `join-team-input`, `move-team-confirm`, `merge-legacy`, `sync-space-<spaceId>`.

---

## 15. Accessibility checklist additions
- **Announce state changes the user didn't directly trigger.** Browser sign-in finishing, a role change and removal are announced through a toast (`role="status"`, or `alert` for errors) or, for removal, a dialog. Only the removal dialog takes focus.
- **Role-disabled controls** use `aria-disabled` plus a tooltip, so they stay focusable and the reason is reachable from the keyboard.
- **Roles, bindings, view-only state, conflicts and attribution are always text.** Icons supplement them.
- **Menu heading rows** are `role="presentation"` with `aria-hidden="false"` text, and are skipped by ↑/↓. Each option's accessible name includes its group, for example "Payments API, Acme".
- **Dialogs follow the `Dialog.svelte` contract:**
  - focus trap, Esc, ⌘Enter and focus restore on close;
  - destructive confirms focus Cancel first;
  - type-to-confirm inputs have visible labels.
- **Layout holds at 80–150 % zoom.** Member rows move the email under the name below 520 px of content width.

---

## 16. Open UX decisions for the owner

| # | Decision | Recommendation |
|---|---|---|
| UX-D1 | Where ⌘, and the gear land, now that Account is first in the nav | Keep **General**. Account is first in the list but not the landing page, so local-only users see no pitch. |
| UX-D2 | Avatars | **Initials only.** Provider images would mean third-party requests from the webview. |
| UX-D3 | Chip click with several spaces | **Keep "sync now"**. Per-space detail goes in the tooltip and Settings › Sync. A popover is Later if needed. |
| UX-D4 | Removal notice | **Modal once**, then a 7-day "Decide" row in Settings › Sync. |

## 17. Deferred (not designed here)
- Email code sign-in, method linking, device rename, "Keep this version", the Convert-to-secret quick-fix, collapsible switcher groups and a sync popover.
- Emailed invites, deep links, a per-device "hide team", activity and audit, a conflict compare view, team policy toggles and passkeys.
- gRPC and GraphQL editors. Only the AC-FWD "newer version" affordances (§6.3) are designed.

---

Files referenced (all read-only):
- docs/DESIGN.md
- docs/design/tokens.css
- ui/src/lib/components/Sidebar.svelte
- ui/src/lib/components/CollectionsTree.svelte
- ui/src/lib/components/MenuHost.svelte
- ui/src/lib/components/Icon.svelte
- ui/src/lib/dialogs/SettingsDialog.svelte
- ui/src/lib/dialogs/ShareDialog.svelte
- ui/src/lib/dialogs/ConfirmDialog.svelte
- ui/src/lib/dialogs/PickerDialog.svelte
- ui/src/lib/stores/ui.svelte.ts
- ui/src/lib/api/mock/index.ts
- worker/src/share-page.ts

No repo files were modified.
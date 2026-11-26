# ReqHarbor — Design Spec (MVP)

Status: ready for build · Owner: Design · Last updated: 2026-11-26 · Implements PRODUCT.md §2.1, §4, §5, §6
Files: `docs/design/tokens.css` (import as-is, single source of truth for values), `docs/design/mockup.html` (visual reference, open in a browser, theme toggle top-right).

## 1. Principles

1. **The request is the hero.** Chrome recedes: no boxed panels, no card-in-card. Hierarchy comes from type weight, text color (`--text-1/2/3`) and spacing, not borders.
2. **One hairline rule.** Borders only where two regions meet (pane splitters, under tab rows, between table rows). Inputs are filled (`--bg-input`), not outlined.
3. **Color means something.** Color is reserved for methods, status, variables, accent actions and syntax. Everything else is neutral.
4. **Fast-feeling.** No animation over 120 ms, no skeleton shimmer, no layout shift on hover/focus. Every action reachable by keyboard.

## 2. Layout

### 2.1 Grid
Three columns, separated by 1px splitters: `Sidebar | Request | Response`. Every pane shares three aligned rows so the eye reads one horizon line:

| Row | Height | Sidebar | Request | Response |
|---|---|---|---|---|
| 1 header | 32 (`--h-pane-header`) | Project switcher · Import icon | Breadcrumb + unsaved dot · Save · `⋯` | "Sent 2m ago · Staging" · Copy · Save to file · Layout toggle |
| 2 bar | 44 (`--h-pane-bar`) | **New Request** split button | Method + URL + **Send** | Status · Size · Time strip |
| 3 tabs | 34 (`--h-tabs`) | Activity / Collections / Env | Query / Headers / Auth / Body / Tests / Pre Run · gear | Response / Headers / Cookies · Pretty/Raw |
| content | flex | filter + list, footer 30 | tab panel | viewer |

Pane horizontal padding: 12px (`--sp-5`). Window minimum: 960 × 600, enforced by the native window only (`tauri.conf.json`). **[DR-001]** No CSS `min-width` on the app root: at ⌘+ zoom (§8) the viewport can be ~640 CSS px, and the layout must reflow, never clip.

**[DR-003]** Every middle-pane view keeps all three rows, including Collection settings (§5.3) and the Env editor (§5.13): row 1 header, row 2 = 44px bar, row 3 = tabs. Tabs always start at y = 76.

### 2.2 Split panes
| Splitter | Default | Min | Max | Collapse |
|---|---|---|---|---|
| Sidebar width | 300 | 220 | 480 | Drag below 160 → snaps to a 44px **rail** (icons: New, Activity, Collections, Env, Settings). Click a rail icon or drag out to restore to last width. **[DR-001]** Auto-rail: when the area right of the sidebar would be < 360px (window or zoom), render the rail without persisting `sidebarCollapsed`; restore automatically once there's room. |
| Request ↔ Response | 50 % | 360 each | — | Never collapses. When the area right of the sidebar is < 760px, auto-stack: Response goes below Request (min height 200 each), splitter becomes horizontal. Layout toggle in Response row 1 forces Side-by-side / Stacked; choice persists. |

Splitter: 1px `--border-subtle` line, 7px invisible hit area, `col-resize`/`row-resize` cursor. Hover/drag: line becomes 2px `--accent-fg`. Double-click resets to default. Keyboard: splitter is focusable (`role="separator"`, `aria-valuenow/min/max`); ←/→ (↑/↓ when stacked) move 16px, Shift moves 64px, Enter toggles sidebar collapse. Sizes (`sidebarWidth`, `sidebarCollapsed`, `splitRatio`, `layoutMode`) persist in `settings.json`; clamp to min/max on restore and on window resize (shrink Response first, then Request, then sidebar).

## 3. Foundations

### 3.1 Color
All values in `tokens.css`. Dark is default (`:root`); light applies via `[data-theme="light"]` or OS `prefers-color-scheme: light` unless `[data-theme="dark"]` is set. Settings → Theme: System / Dark / Light sets or removes `data-theme` on `<html>`.

Surfaces: `--bg-pane` (request, response, editors), `--bg-sidebar` (one step off), `--bg-input` (fields, segmented tracks), `--bg-raised` (menus, popovers, dialogs, toasts), **[DR-002]** `--bg-input-raised` (inputs, selects, textareas and segmented tracks *on* `--bg-raised`: dialogs re-scope `--bg-input: var(--bg-input-raised)` at their root), `--bg-seg-selected` (selected segment fill, always with `box-shadow: 0 0 0 1px var(--border-strong)`; carries `--text-1`/`--text-2` only), `--bg-hover` / `--bg-active` (translucent overlays, work on any surface), `--bg-selected` (deep blue selected row), `--bg-selected-muted` (selected row when its list is not focused).

Contrast (verified with WCAG formula): every `--text-1/2/3`, `--method-*`, `--status-*`, `--syn-*`, `--var-*` is ≥ 4.5:1 on pane, sidebar, input and raised surfaces in both themes; `--text-1/2` and method colors also ≥ 4.5:1 on `--bg-selected`. White on `--accent` 4.7 (dark) / 5.2 (light). **[DR-002]** Same holds on `--bg-input-raised` (min: `--text-3` 4.52:1 dark). Exception: `--bg-seg-selected` is verified for `--text-1` (8.5 / 16.9) and `--text-2` (4.9 / 7.9) only; never put tertiary or colored text on it. Rule: on a selected row, tertiary text switches to `--text-2`. `--text-disabled` is intentionally below 4.5 (disabled controls are exempt).

### 3.2 Method color map
Badges are **text-only** (no fill): mono 10.5px semibold uppercase, in a fixed 54px column (`--w-method-col`) so URLs align. Colors follow PRODUCT §4; HEAD and OPTIONS stay in the gray family but get distinct tints so they are still tellable apart.

| Method | Token | Dark | Light | Hue |
|---|---|---|---|---|
| GET | `--method-get` | #5cc98a | #167246 | green |
| POST | `--method-post` | #e9b04f | #9a5800 | amber |
| PUT | `--method-put` | #6ea5ff | #1f5fd1 | blue |
| PATCH | `--method-patch` | #b694ff | #7339d0 | purple |
| DELETE | `--method-delete` | #ff7b72 | #bd2f28 | red |
| QUERY | `--method-query` | #2fc9c0 | #08746f | teal (cyan-leaning, ≠ GET green) |
| HEAD | `--method-head` | #93a6c2 | #4a5f7d | slate gray |
| OPTIONS | `--method-options` | #c3aa94 | #75604c | stone gray |
| Custom | `--method-custom` | #a4a8ae | #5c636e | neutral gray; truncate at 7 chars + tooltip |

The method name is always printed, so color is never the only signal. Same map everywhere: method trigger, menu, tree, history, share web view.

### 3.3 Status colors
1xx `--status-1xx` (neutral) · 2xx `--status-2xx` green · 3xx `--status-3xx` blue · 4xx `--status-4xx` amber · 5xx `--status-5xx` red · network error `--status-error` red · cancelled `--status-cancelled` neutral. Rendered as a chip: colored mono text on the matching `--*-soft` tint, radius 3. Status strip chip: 22px tall, 12.5px; history chip: 16px tall, 10.5px. Network errors show the kind (`DNS`, `TLS`, `Timeout`, `Connect`) instead of a code.

### 3.4 Typography
System fonts only, nothing downloaded: `--font-ui` (SF / Segoe UI Variable / system-ui / Ubuntu / Cantarell…) and `--font-mono` (ui-monospace / SF Mono / Menlo / Cascadia Mono / Consolas…).

| Token | Size / line | Use |
|---|---|---|
| `--fs-xs` | 10.5 / 16 | method badges, count superscripts, key-caps, group headers (uppercase, +0.06em) |
| `--fs-sm` | 11.5 / 16 | secondary line, labels, timestamps, menu shortcut hints |
| `--fs-md` | 12.5 / 18 | default UI text, tabs, inputs, buttons |
| `--fs-code` | 12.5 / 19 mono | URL bar, editors, KV values, response body |
| `--fs-lg` | 14 / 20 semibold | dialog titles, empty-state titles |
| `--fs-xl` | 16 / 22 semibold | settings section titles |

Weights: 400 body, 500 active tab/buttons/names, 600 titles/badges. Use `font-variant-numeric: tabular-nums` for Size/Time and history durations.

### 3.5 Spacing, radii, sizes, elevation
Spacing (4px base): 2, 4, 6, 8, 12, 16, 20, 24, 32 (`--sp-1…9`). Radii: 3 (checkbox, chip, key-cap), 4 (buttons, inputs), 6 (menus, popovers, toasts), 10 (dialogs), pill (filter inputs). Control heights: 24 small / 28 default / 30 request bar. Rows: tree 26, KV 28, history 40 (two lines). Elevation only on floating layers: `--shadow-popover`, `--shadow-dialog`. Icons: inline SVG, 16px (14px in dense rows), 1.5 stroke, `currentColor`; bundle a small set (Lucide-style), no icon font.

### 3.6 Motion
`--dur-fast` 80ms for color/background/opacity; `--dur-base` 120ms for menus, popovers, toasts, dialogs (opacity + 4px translate, ease-out). Nothing else animates: no tab sliding, no tree expand animation, no list reordering animation. `prefers-reduced-motion: reduce` sets both to 0 and replaces the loading bar with static text.

## 4. Interaction states (all components)
| State | Treatment |
|---|---|
| Hover | `--bg-hover` overlay; ghost/icon text goes `--text-2` → `--text-1`. Inputs: border `--border-strong`. No hover on touch-less disabled items. |
| Active (pressed) | `--bg-active`; primary uses `--accent-active`. |
| Focus | `:focus-visible` only: 2px `--focus-ring` outline, 1px offset (outline, never border, so no shift). Inputs and the request bar group use a 1px `--border-focus` border instead. Lists use roving focus: focused row gets the ring inset. |
| Selected | `--bg-selected` row (or `--bg-selected-muted` when the list lacks focus). Tabs: `--text-1`, weight 500, 2px `--accent-fg` underline. |
| Disabled | 45 % opacity, `cursor: not-allowed`, no hover, skipped by Tab except where explanation is needed (then `aria-disabled` + tooltip). |
| Invalid | 1px `--danger` border + 11.5px `--danger` message under the field (or tooltip in dense rows). |

## 5. Components

### 5.1 Sidebar
- **Project switcher (row 1):** ghost button "▣ Default ▾" (semibold). Menu: projects (check on current), separator, New project…, Rename (F2), Duplicate, Delete…. Scopes Collections, Env and the env selector; Activity is global.
- **Import icon (row 1):** opens Import dialog (§5.11).
- **New Request split button (row 2):** full width, primary, 28px. Main part: new scratch request (⌘N). Caret (30px, divider `rgba(0,0,0,.22)`) opens: New Request, New Collection, New Folder (enabled when a collection/folder is selected), separator, Import cURL… ⌘U, Import OpenAPI…, Import from link….
- **Tabs (row 3):** Activity / Collections / Env, ⌘1/2/3, ←/→ roving focus. Underline active.
- **Filter row:** pill input (26px, search icon, placeholder "filter activity" / "filter collections" / "filter environments"), ⌘⇧E focuses it, Esc clears then blurs. Right: menu icon (Activity: Clear history…; Collections: Collapse all, Expand all, Sort by name; Env: New environment).
- **Footer (30px):** env selector "Env **Staging** ▾" (or "No environment" in `--text-3`), sync indicator, settings gear (⌘, ).

### 5.2 Activity (history) list
Virtualized, 40px rows. Line 1: method badge column + resolved URL (UI font 12.5, scheme stripped, full URL in tooltip). **[DR-004]** URL renders as host (`--text-2`) + path (`--text-1`) in one flex row; the host truncates first (`flex: 0 1 auto; min-width: 0`, end ellipsis), the path keeps up to 80 % of the width (`max-width: 80%`, end ellipsis), so `/v1/users/search` stays readable and filter matches in the path stay visible. No space is reserved for `⋯` while it's hidden; on hover/focus the URL gets `padding-right: 20px` (only the ellipsis point moves, row size never changes). Line 2 (11.5, `--text-3`): relative time ("now", "2m", "3h", "Yesterday", then "Nov 12"), status chip, duration. Sticky group headers TODAY / YESTERDAY / EARLIER. Selected row `--bg-selected`. Hover reveals `⋯` at the right (Save to collection…, Copy as cURL, Delete). Delete key removes the focused row (no confirm, toast with Undo). Filter matches URL, method, status (AC-HIST-3); matches highlighted with `--bg-mark`.

### 5.3 Collections tree
ARIA `tree`, virtualized, 26px rows, indent 12px per level. Row: chevron (16) · icon (collection = stack, folder = folder) · name · right side: child count (`--text-3`) on collections, unsaved dot (6px `--text-2`) on dirty requests, `⋯` on hover. Requests show the method badge instead of an icon. Filter keeps ancestors expanded and highlights matches. Inline rename (F2 / double-click): the name becomes an input in place; Enter commits, Esc cancels, empty name rejected. **Drag & drop:** 4px drag threshold; drop between rows shows a 2px `--accent-fg` line at the target indent; drop onto a folder/collection highlights it with `--accent-soft`; invalid targets show `not-allowed` cursor; auto-expand a collapsed folder after 600ms hover; auto-scroll near edges. Collection row click opens **Collection settings** in the middle pane. **[DR-003]** Row 1: stack icon + "Collection" (`--text-3`, 11.5) + Close. Row 2 (44): inline-editable collection name (30px, `--fs-lg` semibold, transparent until hover/focus, then `--bg-input`) + **Share…** (secondary) right. Tab "Settings", then description and default Auth form (no separate Name field).

### 5.4 Request header (row 1) and request bar (row 2)
- Breadcrumb: collection › folder › **name** (name `--text-1`, 500). Dirty: 6px dot after name. Scratch request shows "Untitled request" + `--text-3` "not saved". Save (ghost, shows ⌘S hint) is disabled when clean; on a scratch request it opens the Save-to picker (collection/folder tree + name field).
- `⋯` menu: Copy as cURL ⌘⇧C, Export as cURL…, Share…, separator, Rename F2, Duplicate, Move to…, separator, Delete.
- **Request bar:** one filled group (`--bg-input`, 30px, radius 4) holding the method trigger (92px, 1px divider right) and the URL field; Send sits 8px to the right. Focus within → `--border-focus` border on the group.
- **URL field:** CodeMirror single-line, mono. `{{var}}` decorations: resolved `--var-fg` on `--var-bg`; unresolved `--var-unresolved-fg` + wavy underline. Hover over a var shows a tooltip "value · from Staging" (secret: "•••• · click to reveal"). Pasting text starting with `curl ` imports in place and shows toast "Imported from cURL" + Undo. Enter = Send.
- **Send:** primary, **[DR-005]** fixed width 88px (content centered) in both states. Disabled when URL empty. In flight: becomes **Cancel** (secondary style, 12px spinner) at the same 88px, so the URL field never resizes. ⌘Enter toggles.

### 5.5 Method dropdown
Listbox popover (200px) anchored under the trigger: GET, POST, PUT, PATCH, DELETE, QUERY, HEAD, OPTIONS, separator, Custom…. Each item: colored name (mono 12.5, 600) + `--text-3` hint (QUERY: "safe, with body"; HEAD: "headers only"). Keyboard: ↑/↓, Enter, Esc; type-ahead on the trigger without opening (G → GET, P cycles POST/PUT/PATCH, Q → QUERY, D, H, O). **Custom…** turns the trigger into an inline uppercase input (auto-uppercases typing); validates `^[A-Z][A-Z0-9_-]*$` on each keystroke; invalid → danger border + tooltip "Use A–Z, 0–9, _ or -, starting with a letter"; Enter commits, Esc reverts. When method = QUERY and body = None, show the inline info hint "QUERY requests normally carry a body" at the top of the Body tab and a 5px `--warning` dot on the Body tab (non-blocking).

### 5.6 Request tabs
Query / Headers / Auth / Body / Tests / Pre Run. Counts as superscripts (`--fs-xs`, `--accent-fg`, 600) when non-zero: Query and Headers show enabled-row count. Auth shows a 5px dot when type ≠ None; Body shows a dot when type ≠ None. Gear icon at the far right opens **Request settings** popover (follow redirects toggle, timeout ms number input, verify TLS toggle with `--warning` note when off). Tests / Pre Run panel: centered empty state "Coming soon" + one line "Scripts and assertions are on the roadmap."

### 5.7 Key-value table editor (Query, Headers, Form-encode, Form, Env variables)
- Columns: enable checkbox (34px) · Key (34 %) · Value (flex) · [Kind Text/File select for Form, 72px] · [Secret lock toggle for Env, 34px] · row actions (24px). Optional Description column via the table's toolbar.
- Toolbar above table (24px): left `--text-3` "3 headers"; right text buttons "Description" (toggle) and **Bulk edit** (toggle).
- Rows 28px, separated by `--border-subtle` hairlines, no vertical lines, mono 12.5 for keys/values. Cells are borderless inputs; hover → `--bg-hover`, focus → `--bg-input` + 1px `--border-focus` inset.
- Always one trailing ghost row ("Add header" / "Value" placeholders); typing in it creates a real row and a new ghost row.
- Disabled row (unchecked): key/value `--text-3`, strikethrough. Delete appears on row hover/focus (trash icon); ⌘⌫ deletes the focused row.
- Keyboard: Tab moves key → value → next row key; Enter in value moves to next row; Space on checkbox toggles.
- Header key autocomplete (common header names) in a 240px popover; Accept-Query in the response is highlighted, not here.
- **Bulk edit:** replaces the grid with a mono textarea, one row per line: `key: value` (Headers) or `key=value` (Query, Form-encode, Env); disabled rows prefixed `// `. Toggling back parses; parse never loses lines (unparseable lines become keys with empty values).
- Secret variables (Env): value shows `••••••••` until focused; lock icon `--accent-fg` when secret.

### 5.8 Auth forms
Type select (dropdown, 180px): None, Inherit, Basic, Bearer, API Key. Fields stack vertically, label above (11.5, `--text-2`), inputs 28px, max width 480. Basic: Username, Password (masked, eye toggle). Bearer: Token (masked, eye toggle, mono). API Key: Key, Value, "Add to" segmented Header | Query. Inherit: read-only summary row "Inherits **Bearer** from Users API" + link "Edit collection auth". None: one muted line "No auth header will be added." If Headers contains an enabled `Authorization`, show an info hint: "An Authorization header in Headers overrides this." All fields accept `{{vars}}` with the same decorations as the URL.

### 5.9 Body editor
- Sub-tabs as compact chips (22px, radius 4): None · JSON · XML · Text · Form · Form-encode · GraphQL · Binary. Selected chip: `--accent-soft` bg, `--accent-fg` text.
- Label row: "JSON Content" (11.5, `--text-3`) left, **Format** link right (⌘⇧F; shown for JSON/XML/GraphQL variables). Format error: inline danger hint "Invalid JSON at line 4, col 12", cursor moved there, text untouched.
- Editor: CodeMirror 6, mono 12.5/19, gutter 44px line numbers in `--text-3`, active line `--bg-code-active-line`, no gutter border, bracket matching, fold arrows on hover. Syntax via `--syn-*` tokens. Unresolved `{{vars}}` decorated as in the URL.
- GraphQL: Query editor (flex) above Variables editor (min 80px) separated by a horizontal splitter; invalid variables JSON → danger hint under Variables and Send shows tooltip "Fix GraphQL variables".
- Form / Form-encode: KV table (§5.7). Form file rows show a "Choose file…" button + filename.
- Binary: drop zone (only dashed border in the app, 1px `--border-strong`, radius 6, 120px): "Drop a file or **Choose file…**"; selected → file name, size, path (mono, ellipsis middle), "Replace" / "Remove".
- None: muted line "This request has no body."

### 5.10 Response viewer
- **Row 1:** `--text-3` "Sent 2m ago · Staging" (history entries: "From history · Nov 24, 14:02" + **Re-send** link); icon buttons Copy body, Save to file, Layout toggle.
- **Status strip (row 2):** `Status [200 OK]  Size 247 B  Time 184 ms` — labels `--text-2` 12.5, values `--text-1` mono tabular. Size tooltip "Wire size 198 B (gzip)". Unresolved vars at send time append an amber item "⚠ 2 unresolved variables" (click → list popover).
- **Tabs:** Response / Headers n / Cookies n. Right side: segmented Pretty | Raw (JSON/XML), Preview | Raw (images), hidden for plain text.
- Pretty: read-only CodeMirror with highlighting, folding, line numbers; ⌘F opens search. Raw: same viewer, no highlighting, soft-wrap on.
- Binary: centered info "application/pdf · 1.2 MB" + **Save to file** (secondary). Image preview: centered on a checkerboard of `--bg-input`/`--bg-pane`, max 100 %, with dimensions under it.
- > 5 MB: info hint "Showing first 5 MB as plain text" + **Save full body to file**.
- Headers tab: 2-column table (name `--text-2`, value mono `--text-1`), received order, click copies the value (toast "Copied"). `Accept-Query` row: `--accent-soft` background + "QUERY" teal tag.
- Cookies tab: table Name, Value, Domain, Path, Expires, HttpOnly, Secure, SameSite (booleans as ✓ / —); horizontal scroll inside the panel if narrow.

### 5.11 Dialogs (shared rules)
Centered, `--bg-raised`, radius 10, `--shadow-dialog`, overlay `--bg-overlay`. Header 44px (title 14 semibold, close ×), body padding 16, footer 52px right-aligned: secondary Cancel + primary action. Focus trapped; initial focus on the first field; Esc closes (if no unsaved input, else confirm); ⌘Enter / Enter submits when valid. Primary disabled until valid.

- **Import (720 × 520):** segmented tabs cURL | OpenAPI | Link. **cURL:** mono textarea (auto-focused, 160px min) left/top, live preview right/bottom: method badge + URL, summary line "3 headers · JSON body · Basic auth", unknown flags listed in a dismissible warning hint, "Not a cURL command" danger hint when not parseable. Destination: "Open as new request" (default) or "Save to…" picker. Primary: **Import**. **OpenAPI:** drop zone (dashed) "Drop a .json or .yaml file, or paste below" + Choose file + textarea; detected chip "OpenAPI 3.1 · YAML" (danger chip "Unsupported format" otherwise). Preview: collection name, folder list with request counts (scrollable, 200px), environment to be created with `baseUrl`, warnings (OAuth2 skipped, external `$ref`s). Project select. Primary: **Import 42 requests**. After import: toast + import report link. **Link:** URL input → preview of shared tree → **Import copy**.
- **Export as cURL popover (480px, anchored to the trigger):** read-only mono preview (max 240px, scroll), toggle "Keep {{variables}}", primary **Copy**. ⌘⇧C skips the popover and copies with the last toggle value, toast "cURL copied to clipboard".
- **Share (520px):** title "Share “Users API”". Section "Removed before sharing" with icon rows: "3 secret variables — always removed", "Authorization, Cookie headers and auth values → {{placeholders}}". Checkbox "Include credentials" (when checked, a warning hint: "Anyone with the link can see these credentials."). Select "Attach environment" (None / env list; "non-secret values only"). Primary **Create link**. Result state: read-only URL field + Copy; footer links Update share, Revoke (danger, confirm). Not connected: empty state "Sharing needs sync" + **Set up sync**.
- **Confirm delete:** title "Delete “Users”?", body "This also deletes 2 folders and 14 requests.", primary is danger (`--danger-bg`, white text), initial focus on Cancel.

### 5.12 Settings panel
Dialog 760 × 540 with left nav (160px, list rows 28px): General, Sync, Shares, Shortcuts, About. **General:** Theme segmented System | Dark | Light. **Sync:** Worker URL input; two choices as radio cards: "Create workspace" / "Connect with token". Created token shown once in a mono read-only field with Copy + warning hint "Copy it now — it won't be shown again." Connected state: status line ("Synced 1 min ago" / "Offline — retrying in 30 s"), buttons **Sync now**, Rotate token…, Disconnect (danger ghost). 401: danger hint "Token rejected — sync disconnected. Enter a new token." **Shares:** table (Name, Kind, Created, Updated, actions Copy link · Update · Revoke). Empty: "No active shares." **Shortcuts:** read-only list with key-caps.

### 5.13 Environments
- **Env tab list:** "Global" pinned first (globe icon), then project environments; active one shows a check and `--text-1` 500. Row `⋯`: Set active, Rename F2, Duplicate, Delete. Clicking opens the **Env editor** in the middle pane. **[DR-003]** Row 1: env icon + "Environment" (`--text-3`, 11.5) + Close; row 2 (44): env name (inline-editable, same style as collection name §5.3) + "Set active" button right; row 3: tab "Variables"; body = KV table with enabled + secret toggles (§5.7). Response pane shows a muted hint "Variables resolve: project env → Global".
- **Env selector (sidebar footer)** and **⌘E quick picker:** centered-top popover 420px (like a command palette, 64px from the top): search input, list "No environment", project envs (check on active), separator, "Manage environments…". ↑/↓, Enter, Esc. Selection persists per project.

### 5.14 Sync indicator (sidebar footer)
Not connected: hidden. Idle: cloud-check icon + "Synced" (`--text-3`), tooltip with last sync time. Syncing: same icon at 60 % opacity (no spinner loop). Offline / 5xx: `--warning` cloud icon + "Offline", tooltip "Retrying in 30 s" + Sync now. Error 401: `--danger` icon + "Sync off" → opens Settings › Sync.

### 5.15 Toasts
Bottom-right, 16px from edges, max 360px wide, stack up to 3 (newest at bottom, 8px gap). `--bg-raised`, radius 6, popover shadow, 40px min height: status icon (success/info/warning/error color) · message · optional action link (Undo, View) · dismiss ×. Auto-dismiss 4 s; 8 s with an action; errors persist until dismissed. Pause on hover/focus. `role="status"` (errors `role="alert"`). Never steal focus.

### 5.16 Context menus & dropdown menus
`--bg-raised`, radius 6, 4px padding, min 200px. Items 26px, radius 4, 12.5px; shortcut hint right-aligned `--text-3`; separators 1px `--border-subtle` with 4px margins; danger items `--danger`; disabled `--text-disabled`. Highlighted item: `--accent` bg + white text (same for mouse hover and keyboard). Keyboard: ↑/↓ wrap, Home/End, Enter/Space activate, → / ← submenus, type-ahead, Esc closes and returns focus. Open on right-click, Shift+F10, or the context-menu key. Menus flip to stay in the window.

### 5.17 Tooltips
`--bg-raised`, 11.5px, radius 4, 4px 8px padding, 500ms delay (0 when moving between tooltipped items), show on focus too. Include the shortcut as a key-cap where one exists.

### 5.18 Key-caps
18px tall, min 18px wide, 4px side padding, 1px `--border-strong` with 2px bottom border, radius 3, `--bg-input`, 10.5px `--text-2`. Mac glyphs ⌘ ⇧ ⌥ ↵; Win/Linux show "Ctrl", "Shift", "Enter" words (chips grow).

## 6. Empty, loading and error states
| Where | State |
|---|---|
| Response, nothing sent | Centered key-cap table: Send Request ⌘↵ · Import cURL ⌘U · Change Environment ⌘E · New Request ⌘N (platform modifiers, AC-KEY-2). Labels right-aligned `--text-2`. |
| Response, sending | Send → Cancel; 2px indeterminate `--accent-fg` bar under row 2 (reduced motion: no bar); centered "Sending… 1.2 s" (tenths, `--text-3`) after 300 ms. Previous response stays dimmed (opacity .5) rather than vanishing. |
| Response, network error | Status chip `Error`; panel: soft-danger box, title by kind — "Couldn't resolve host" (dns), "Connection refused" (connect), "TLS handshake failed" (tls) + action "Disable TLS verification for this request", "Timed out after 30 s" (timeout) + "Change timeout", plus mono message and **Retry**. Cancelled: neutral chip `Cancelled`, line "Request cancelled after 1.2 s". |
| Activity empty | "No requests yet" (14 semibold) + "Send a request and it lands here." |
| Collections empty | "No collections in Default" + buttons New collection (primary) / Import OpenAPI (secondary). |
| Env empty (project) | Global row stays; below: "No project environments" + New environment. |
| Filter, no matches | "No matches for “xyz”" + Clear filter link. |
| Corrupt project file | Persistent error toast "Users API couldn't be read — saved as users.corrupt-….json" + Show file. |

## 7. Keyboard

### 7.1 Focus order
Tab order follows reading order per pane: **Sidebar** (project switcher → import → New Request → caret → tabs → filter → filter menu → list [one tab stop, roving] → env selector → settings) → **Request** (Save → `⋯` → method → URL → Send → request tabs [one stop] → gear → tab panel content) → **Response** (row-1 icons → response tabs [one stop] → view segmented → body viewer). F6 / Shift+F6 jump between the three panes (focus restores to the last focused element in that pane). Splitters are in the tab order right after the pane they resize. Dialogs trap focus and restore it on close.

### 7.2 Shortcuts
All PRODUCT §6 shortcuts, global (work inside CodeMirror; ⌘F stays editor-local): ⌘↵ send/cancel, ⌘U import cURL, ⌘E env picker, ⌘N new request, ⌘S save, ⌘L focus URL (selects all), ⌘⇧C copy as cURL, ⌘⇧F format body, ⌘⇧E focus sidebar filter, ⌘1/2/3 sidebar tabs (also expands a collapsed sidebar), ⌘F find, F2 rename, Esc close/cancel. Ctrl replaces ⌘ on Windows/Linux. Design additions (need PO sign-off, safe to ship): ⌘B toggle sidebar, ⌘, settings, F6 pane cycling, Delete on a focused list row, Shift+F10 context menu. Every menu item and tooltip shows its shortcut.

## 8. Accessibility checklist
Text ≥ 4.5:1 (§3.1); focus ring ≥ 3:1 against adjacent surfaces; hit targets ≥ 24 × 24; color never the only signal (method names and status codes are text, unresolved vars also wavy-underlined, disabled rows struck through); ARIA roles: `tablist`/`tab`, `tree`/`treeitem`, `listbox`/`option` (history, method), `menu`/`menuitem`, `dialog` + `aria-modal`, `separator` for splitters, `aria-live="polite"` on the status strip; all icon buttons have `aria-label` + tooltip; respects `prefers-reduced-motion` and OS theme; ⌘+ / ⌘− zoom the whole WebView (layout must hold at 80–150 %).

## 9. Implementation notes for dev
Import `tokens.css` at the root and use only tokens in component CSS (no raw hex). Map CodeMirror themes to `--syn-*`, `--bg-pane`, `--text-3` (gutter) so they follow theme switches without reload. Virtualize history and tree (fixed row heights above make this trivial). The share web view (Worker) reuses the method/status tokens and fonts (inline a trimmed copy of `tokens.css`; no external resources, CSP-safe). `mockup.html` is the visual source of truth for spacing and hierarchy; this document wins on behavior.

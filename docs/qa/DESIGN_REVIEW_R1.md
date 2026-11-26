# ReqHarbor — Design QA Review, Round 1

Reviewer: Design · Date: 2026-11-26 · Build: `ui/` (Svelte 5) on the in-browser mock backend, Chromium via Playwright in the dev container.
References: `docs/DESIGN.md`, `docs/design/tokens.css`, `docs/design/mockup.html`.
Screens: `docs/qa/design/screens/` (122 PNGs, dark + light, 1440×900 / 1024×700 / 800×600). Mockup captures: `00-mockup-1440-{dark,light}.png`, `00-mockup-full-{dark,light}.png`. Raw measurements: `docs/qa/design/metrics.json`.
Script (throwaway, not part of the e2e suite): `ui/e2e/design_review/shoot.mjs`, `ui/e2e/design_review/probe800.mjs`. Re-run with
`docker compose run --rm -T -w /app/ui dev sh -c "npx vite --host 127.0.0.1 --port 5173 >/dev/null 2>&1 & sleep 6; node e2e/design_review/shoot.mjs"`.

## Verdict

The build is very close to the mockup: it feels like the same product, calm and dense, much quieter than Postman. Row heights line up exactly at 32 / 44 / 34 px in all three panes (measured, both themes). Method and status colors match the token table. All sampled text passes 4.5:1 in both themes (tertiary text is 5.1 to 5.7:1, white on Send is 4.71 dark and 5.17 light). `:focus-visible` rings show on every control I tabbed through. Menus, toasts, the error panel, empty states and the dark/light parity of the three main panes all follow the spec.

The one must-fix is layout: the app clips below 960 CSS px, which also breaks the spec's 80–150 % zoom requirement. The P2s are mostly secondary screens that drift from the system: dialogs in dark mode, the collection and environment editors, and history-row density.

Counts: **P1 1 · P2 8 · P3 7**

| ID | Sev | Title |
|---|---|---|
| DR-001 | P1 | App hard-clips below 960 CSS px (Send and actions off-screen at zoom ≥ 125 %) |
| DR-002 | P2 | Dark dialogs: selected segment invisible, inputs darker than the surface |
| DR-003 | P2 | Collection settings and Env editor break the 32/44/34 horizon |
| DR-004 | P2 | History rows lose the path: 28 px reserved for the hidden `⋯`, filter marks land on the ellipsis |
| DR-005 | P2 | Send → Cancel grows 76 → 86 px and shifts the URL field |
| DR-006 | P2 | Two tree rows look selected at once |
| DR-007 | P2 | Settings › Theme segmented track stretches to 480 px |
| DR-008 | P2 | OpenAPI import: Project select hidden below the fold of a fixed-height dialog |
| DR-009 | P2 | Orphan "Status —" in response row 2 on first launch and in the editors |
| DR-010 | P3 | Active-line band and bracket boxes in unfocused/read-only editors |
| DR-011 | P3 | Unresolved-variable red wavy underline on disabled KV rows |
| DR-012 | P3 | Selects use a CSS triangle caret; eye toggle sits outside the field |
| DR-013 | P3 | Light theme: focus ring on primary buttons merges with the button |
| DR-014 | P3 | Share dialog copy and empty footer |
| DR-015 | P3 | Method dropdown doesn't mark the current method |
| DR-016 | P3 | Response Headers table: double hairline under the tabs |

---

## P1 — must fix

### DR-001 · App hard-clips below 960 CSS px
**Screens:** `01-main-800-dark.png`, `01-main-800-light.png` (broken) vs `23-800-minwidth-removed-dark.png`, `23-853-minwidth-removed-dark.png` (same build with the rule removed).
**What's wrong:** `.app` has `min-width: 960px` and `overflow: hidden`. Below 960 CSS px the right side is cut off: Send, Save, `⋯`, Format, the response icon buttons and the whole right edge of the response sit off-screen. The Tauri window minimum (960) does not prevent this. With ⌘+ zoom (DESIGN §8: "layout must hold at 80–150 %"), the default 1280 px window at 150 % is 853 CSS px, and Send lands at x = 948. A minimum-size window at 125 % (768 px) is worse. The auto-stack logic already handles narrow widths well (see `01-main-1024-*.png`, `23-853-*.png`), so only this one rule is in the way.
**Fix:**
- `ui/src/App.svelte` `.app`: delete `min-width: 960px`. The window minimum stays in `src-tauri/tauri.conf.json` (`minWidth: 960`).
- In the same component, when `innerWidth - sidebarW < 360` (PANE_MIN), render the sidebar as the 44 px rail without persisting `sidebarCollapsed`. This follows the §2.2 order (shrink Response, then Request, then sidebar) and keeps the main area at 360 px or more at 150 % zoom.

---

## P2 — should fix

### DR-002 · Dark dialogs: selected segment invisible, inputs darker than the surface
**Screens:** `14-import-curl-dark.png`, `15-import-openapi-preview-dark.png` (segmented "cURL | OpenAPI | Link"), `18-settings-general-dark.png` (Theme "System | Dark | Light"), `17-share-dark.png` and `18-settings-sync-dark.png` (selects and inputs). Compare the light versions (`15-import-openapi-preview-light.png`), where the selection reads clearly.
**What's wrong:** This is a token gap on my side that the build exposes. `.seg button[aria-pressed=true]` uses `--bg-raised` (#23262d), which is exactly the dialog surface, so the selected segment has a contrast of 1.00:1 against it and you can't tell which import tab or theme is active. Inputs inside dialogs use `--bg-input` (#1f2228), which is *darker* than the raised surface (1.05:1, inverted depth), so fields like "Attach environment" and "Worker URL" nearly disappear. On panes the same input step is 1.13:1 and lighter-than-surface.
**Fix (tokens + 2 lines of CSS):**
- `docs/design/tokens.css`: add `--bg-input-raised: #282b32` (dark) / `#f1f3f6` (light, unchanged) and `--bg-seg-selected: #3a3f48` (dark) / `#ffffff` (light). Add them to all three theme blocks (`:root`, `[data-theme=light]`, and the `prefers-color-scheme` block).
- `ui/src/lib/components/Dialog.svelte`, dialog root: `--bg-input: var(--bg-input-raised);` (re-scopes every input, select, textarea and seg track inside dialogs; gives 1.07:1, lighter than the surface).
- `ui/src/app.css` `.seg button[aria-pressed="true"], .seg button[aria-selected="true"]`: `background: var(--bg-seg-selected); box-shadow: 0 0 0 1px var(--border-strong);` (1.25:1 fill step plus a visible edge; also strengthens Pretty/Raw on panes).

### DR-003 · Collection settings and Env editor break the 32/44/34 horizon
**Screens:** `04-sidebar-collections-dark.png`, `16-env-editor-dark.png`, `16-env-editor-light.png`.
**What's wrong:** Both middle-pane editors go straight from row 1 (32) to their tab row, so "Settings" and "Variables" sit at y = 32–66 while the sidebar and response tabs sit at y = 76–110. That breaks the core "one horizon line" rule (§2.1). The env header icon also sits about 3 px above the name's baseline.
**Fix:** Add the 44 px bar row (`.row2`, `height: var(--h-pane-bar)`, padding `0 var(--sp-5)`), as §5.13 already describes for env:
- `ui/src/lib/components/EnvEditor.svelte` (row 1 at line ~38): row 1 = icon + "Environment" (`--text-3`, `--fs-sm`) + Close. Row 2 = inline-editable env name (30 px, `--fs-lg`, 600, transparent background until hover or focus, then `--bg-input`) + **Set active** button on the right. Tabs follow.
- `ui/src/lib/components/CollectionSettings.svelte` (line ~36): row 1 = stack icon + "Collection" + Close. Row 2 = inline-editable collection name (same style) + **Share…** (secondary). Remove the separate "Name" field from the form.
- Row 1: `align-items: center` and render the icon as `<Icon small>` inside the flex row so it centers with the text.

### DR-004 · History rows lose the path; filter marks land on the ellipsis
**Screens:** `01-main-1440-dark.png` vs `00-mockup-1440-dark.png`, `04-sidebar-activity-filter-light.png`.
**What's wrong:** `.hurl` has a permanent `padding-right: 20px` on top of the row's 8 px, so the URL ellipsizes about 28 px earlier than in the mockup even when `⋯` is hidden. At the default 300 px sidebar almost every row reads `api.staging.example.com/v1/u…`, so the rows can't be told apart. When filtering by "users", the `--bg-mark` highlight falls on the truncated tail and shows as an orange slab over the ellipsis.
**Fix:** `ui/src/lib/components/ActivityList.svelte`:
- `.hurl { padding-right: 0 }`, then `.hrow:hover .hurl, .hrow:focus-within .hurl { padding-right: 20px }`. Only the ellipsis point moves on hover; the row doesn't change size.
- Spec amendment to §5.2, which I'm making: render the URL as `<span class="host">` + `<span class="path">` in a flex row. `.host { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; color: var(--text-2) }`, `.path { flex: 0 1 auto; min-width: 0; max-width: 80%; overflow: hidden; text-overflow: ellipsis; color: var(--text-1) }`. The host gives way first, so `/v1/users/search` stays visible and filter matches in the path stay on screen. The full URL stays in the tooltip.

### DR-005 · Send → Cancel grows 76 → 86 px and shifts the URL field
**Screens:** `13-sending-light.png` (Cancel at x 772–858) vs `13-404-dark.png` (Send at x 782–858).
**What's wrong:** §5.4 says "same width so nothing jumps". Today the URL field shrinks by 10 px on every send and grows back on completion, which is the most frequent action in the app.
**Fix:** `ui/src/lib/components/RequestPane.svelte` `.send` (line ~373): replace `min-width: 76px` with `width: 88px; justify-content: center;`. I'll update DESIGN §5.4 to "fixed 88px".

### DR-006 · Two tree rows look selected at once
**Screens:** `11-context-menu-dark.png`, `12-toast-dark.png` ("Users API" and "Create user" both have a blue row background).
**What's wrong:** `selected()` in `ui/src/lib/components/CollectionsTree.svelte` (lines 34–38) returns true both for the open request and for `ui.selectedNode` (set by right-click or keyboard on a collection or folder). Two blue rows make it unclear what is open and what the next action targets.
**Fix:** When `app.middle.kind === "request"`, `aria-selected` should be true only for that request. Show `ui.selectedNode` as the *context/roving target* without a fill: `.trow[data-target="true"] { box-shadow: inset 0 0 0 1px var(--border-focus); }`, shown only while the menu is open or the list has focus. Keep `--bg-selected` / `--bg-selected-muted` for the single open item.

### DR-007 · Settings › Theme segmented track stretches to 480 px
**Screens:** `18-settings-general-dark.png`, `18-settings-general-light.png`.
**What's wrong:** The `.seg` sits inside `.field` (a flex column), so it stretches to the field's full width. The three buttons huddle on the left of a long empty track, which reads as broken.
**Fix:** `ui/src/app.css`: `.field > .seg { align-self: flex-start; }`. Better, set `.seg { width: max-content; }` globally, since no segmented control should stretch.

### DR-008 · OpenAPI import: Project select hidden below the fold
**Screens:** `15-import-openapi-preview-dark.png`, `15-import-openapi-preview-light.png` (the "Project" label is visible and the select is cut off).
**What's wrong:** The dialog is fixed at 560 px (`ImportDialog.svelte:250`, `height={560}`). With a parsed spec, the preview (folders, environment, two warnings) pushes the Project select below the visible body, with no scroll cue. That choice decides where 5 requests land, so it shouldn't be hidden.
**Fix:** `ui/src/lib/dialogs/ImportDialog.svelte`: move the Project select into the footer, left-aligned ("Into [Default ▾]", 28 px, 180 px wide, `margin-right: auto`), so it is always next to **Import 5 requests**. Also, in `ui/src/lib/components/Dialog.svelte` `.body`, add a scroll-shadow cue (`background: linear-gradient(var(--bg-raised) 30%, transparent) top, linear-gradient(transparent, var(--bg-raised) 70%) bottom; background-attachment: local; ...`) so overflowing dialog bodies read as scrollable.

### DR-009 · Orphan "Status —" in response row 2
**Screens:** `20-empty-activity-dark.png`, `20-empty-collections-light.png`, `16-env-editor-*.png`, `14-import-curl-dark.png` (background).
**What's wrong:** Before anything is sent, row 2 shows a lone `--text-3` "Status —" above the centered key-cap sheet. On first launch it's the first thing people see, and it looks like a missing value. The mockup has no placeholder strip.
**Fix:** `ui/src/lib/components/ResponsePane.svelte:188`: render nothing in the `{:else}` branch and keep the empty 44 px row so the horizon holds. In the Env editor context, put the "Variables resolve: project env → Global" hint here instead of in row 1.

---

## P3 — polish

### DR-010 · Active-line band and bracket boxes in unfocused/read-only editors
**Screens:** `01-main-1440-light.png` (gray band plus boxed `{`/`}` on line 1 of the request body and the response).
**Fix:** `ui/src/lib/cm/editor.ts` (lines ~226–231): for `readOnly` viewers, drop `highlightActiveLine()`, `highlightActiveLineGutter()` and `bracketMatching()`. For editable editors, add to the theme `"&:not(.cm-focused) .cm-activeLine": { backgroundColor: "transparent" }` and `"&:not(.cm-focused) .cm-matchingBracket": { backgroundColor: "transparent", outline: "none" }`.

### DR-011 · Unresolved-variable red wavy underline on disabled KV rows
**Screens:** `06-headers-dark.png` (`X-Debug: {{debug}}` is struck through *and* red-wavy).
**Fix:** In `KvTable.svelte` / `VarInput.svelte`, on a disabled row the var decoration should drop to `color: var(--text-3); background: none; text-decoration: line-through;`. A disabled row isn't sent, so it shouldn't raise an error.

### DR-012 · Selects use a CSS triangle caret; eye toggle sits outside the field
**Screens:** `07-auth-type-open-dark.png`, `07-auth-bearer-light.png`, `04-sidebar-collections-dark.png`.
**Fix:** In `ui/src/app.css` `select.input`, replace the two-gradient triangle with a wrapper (`.select-wrap { position: relative }`) and an absolutely positioned `<Icon name="chev-d" small>` at `right: 6px`, color `--text-3`, so it matches the method trigger. In `AuthForm.svelte` / `CollectionSettings.svelte`, put the eye toggle inside the input: wrapper `position: relative`, button `position: absolute; right: 2px; top: 2px; width: 24px; height: 24px`, input `padding-right: 28px`. Password and token fields then line up with the 480 px column.

### DR-013 · Light theme: focus ring on primary buttons merges with the button
**Screens:** `03-focus-send-light.png`, `03-focus-newreq-light.png`.
**Fix:** `ui/src/app.css`: `.btn-primary:focus-visible { outline-offset: 2px; }`. The `--focus-ring` and `--accent` colors are the same in light (#2563eb), so the 1 px gap alone barely separates them.

### DR-014 · Share dialog copy and empty footer
**Screens:** `17-share-dark.png` ("**0** secret variables — always removed", "1 folders", "(2 / replaced)" orphan wrap), `17-share-needs-sync-light.png` (empty footer band under "Set up sync").
**Fix:** `ui/src/lib/dialogs/ShareDialog.svelte`: hide the secrets row when the count is 0 (or say "No secret variables"). Pluralize ("1 folder"). Shorten the auth line to "Auth headers and values → `{{placeholders}}` · 2 replaced". In the needs-sync state, don't render the footer.

### DR-015 · Method dropdown doesn't mark the current method
**Screens:** `05-method-dropdown-dark.png` (POST is current and has no marker).
**Fix:** `ui/src/lib/components/MethodSelect.svelte`: current option gets `aria-selected="true"` plus a 14 px check icon at the right in `--text-2` (replace the hint for that row), matching the env picker and project menu.

### DR-016 · Response Headers table: double hairline under the tabs
**Screens:** `09-response-headers-dark.png` (tab-row border at y = 110 and a table top border at y = 114).
**Fix:** In the `ResponsePane.svelte` headers table, `tr:first-child td { border-top: 0; }` (hairlines go *between* rows only, per §5.7).

---

## Checked and OK (no action)
- Header rows: 32 / 44 / 34 at y = 0 / 32 / 76 in all panes, both themes, and in stacked mode. Request bar 30 px, radius 4, method trigger 92 px. Send 76×30 with white text (4.71 / 5.17:1).
- Typography: UI 12.5 system stack; URL, editors and KV values mono 12.5; badges mono 600; tabs 500 when selected.
- Contrast (getComputedStyle sampled, see `metrics.json`): history meta 5.43 / 5.09, breadcrumb 5.72 / 5.45, unselected tabs 7.9 / 7.35, gutter 5.72 / 5.45, status chips 6.72 / 5.19, history chip 6.32 / 4.86. All pass.
- Focus: visible 2 px rings on icon buttons, New Request, sidebar and request/response tabs, the roving history row (inset), the method trigger and Send, in both themes.
- Context menus, request `⋯` menu, New menu, toasts, confirm dialog, env picker, error panel (`13-error-dns-*`), cancelled state, empty states, sidebar rail, auto-stack at 1024 and the forced stacked layout all match the spec.

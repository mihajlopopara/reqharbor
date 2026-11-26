# QA Report — Round 3 (final), Full System

Date: 2026-11-26 · Time-boxed (~40 min) · Docker only (`reqharbor-dev` image, which now ships webkit2gtk-driver and tauri-driver) · No product code changed, no git.
Scope: every R2 bug, every design-review P1/P2 (DR-001…009), the changed contract (API_CONTRACT §0 "QA round 2"), the real release app under Xvfb, and the cheap R2 gaps (axe, narrow widths, long inputs, rapid Send/Cancel).

## Verdict: **GO for the MVP**, with one condition

All R2 bugs and all design-review P1/P2 items are verified fixed in the real release app, the mock and screenshots. The UI ↔ Rust contract has no functional mismatch, and the real app passes 18/18 end-to-end checks. Every measured performance budget is met.

The condition is **QA-R3-001**: the UI unit gate is red because `ui/src/lib/api/types.ts` was not re-synced after a comment-only edit to `docs/types.ts`. It is a one-line copy with no runtime effect, but the gate should be green before tagging.

New findings are P3 only. They are accessibility issues from axe (QA-R3-002), stale mock behaviour (QA-R3-003) and a count label (QA-R3-004). None blocks an MVP; QA-R3-002 is recommended for the first patch release.

## 1. Gates

| Gate | Command | Result |
|---|---|---|
| Rust format | `cargo fmt --all -- --check` | PASS |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| Rust tests | `cargo test --workspace` | PASS: 118 core unit, 10 src-tauri, 11 http_send, 15 sync_mock, 1 curl_parity, qa_curl 26, qa_http 19 (+1 ignored R1-019), qa_openapi 13 (+1 ignored), qa_store_vars 13, qa_sync_live 4 ignored (live) |
| App build | `cargo build -p reqharbor-app` | PASS |
| Release app | `cargo build --release -p reqharbor-app --features tauri/custom-protocol` | PASS (1m37s; embeds the fresh `ui/dist`) |
| Live Worker | `bash qa/run_live_worker_tests.sh` | PASS: curl smoke 0 failures, live Rust⇄Worker 4/4 |
| Worker types / tests / build | `npm run typecheck` / `npm test` / `npm run build` | PASS / **121/121** / PASS (74.2 KiB, 22.9 KiB gz). Run last as asked: the multi-line cURL port was already in, and `curl_parity` is green |
| UI check | `npm run check` | PASS (0 errors, 0 warnings) |
| UI unit | `npm test` | **FAIL 87/88** → QA-R3-001 (re-ran once; still red) |
| UI build / size | `npm run build` / `npm run size` | PASS: 78.6 KB initial; 283.3 KB total excl. mock (budget 350) |
| UI e2e (devs + QA R2) | `npx playwright test` (smoke, flows, layout, qa_shortcuts, qa_contract_bugs, qa_contract_shapes) | PASS **34/34**. `BUG QA-R2-001` now passes |
| UI e2e (QA R3) | `npx playwright test e2e/qa_r3.spec.ts` | 6/8. The 2 failures are the intended repro `BUG QA-R3-002` (axe, dark + light) |
| Static contract | `node qa/r2/contract_check.mjs` | PASS: 62/62 commands, 85 call sites, 0 problems |
| Real app | `qa/r2/real_app_smoke.mjs` (release build, Xvfb, tauri-driver) | PASS **18/18** |

Re-run notes:
- Real-app step 8 failed once because the response body was still "Loading body…" when the step read it. I added a wait to the test (a timing issue in the test, not the app) and it passed on re-run.
- Step R3-b failed once on an element lookup race. I added `waitSel` before clicks and it passed on re-run.

## 2. Verification of R2 bugs and design-review items

| Item | How verified | Result |
|---|---|---|
| QA-R2-001 fractional sidebarWidth | Real IPC: `save_settings` with 312.5 → **313**. Real app: WebDriver pointer drag of the splitter (+37 px), and `get_settings` shows 337 (300 + 37). `BUG QA-R2-001` Playwright passes (UI now rounds). The fix is on both sides | **Fixed** |
| QA-R2-002 share update drops options | Real app (R3-b): create a share with "Include credentials" and an attached env, then Settings › Shares › Update. `GET /s/:id.json` still contains the bearer credential and the env variable after the update | **Fixed** |
| QA-R2-003 share_* null fields | Real IPC: `share_create.name` = "Col"; `share_update` returns kind "collection", entityId and name | **Fixed** |
| QA-R2-004 cURL layout | Real clipboard after Ctrl+Shift+C: `curl \` / `  -X QUERY \` / `  'http…' \` / … (6 lines, every line ` \`-terminated and indented). The real export equals the mock's exactly. Worker `curl_parity` 40/40 | **Fixed** |
| QA-R2-005 duplicate_project active env | Real IPC: the copy's `activeEnvironmentId` points at the copied env | **Fixed** |
| QA-R2-006 mock YAML (3.1 fixture) | Mock now parses `webhooks_v31.yaml`; there is no outcome diff left between real and mock | **Fixed** |
| QA-R2-007 sync listener race | Code: `listen("sync://…")` before `bootstrap`, then `get_sync_status` refresh (`app.svelte.ts:134–161`) | **Fixed** (code review) |
| DR-001 (P1) clip below 960 px | Screenshot `docs/qa/design/screens_r3/30-zoom150-{dark,light}.png` (1280×800 at 150 % = 853 CSS px): Send right edge at 841 of 853, no horizontal scroll. Playwright at 800×600, 853×533 and 768×480: Send, Save, `⋯`, URL and Import submit all in viewport, no overflow, send and import work | **Fixed** |
| DR-002 dark dialogs | `14-import-curl-dark.png`, `15-import-openapi-preview-dark.png`, `18-settings-general-dark.png`: the selected segment (cURL / OpenAPI / System) is clearly raised; inputs are lighter than the surface | **Fixed** |
| DR-003 32/44/34 horizon | `16-env-editor-dark.png`, `04-sidebar-activity-filter-light.png` (collection settings): row 1 "Environment / Collection", row 2 name + action, tabs aligned with sidebar and response tabs | **Fixed** |
| DR-004 history rows | `04-sidebar-activity-filter-light.png`, `30-zoom150-dark.png`: host ellipsizes first, path stays visible, `users` marks land on the path | **Fixed** |
| DR-005 Send/Cancel width | Playwright `DR-005`: Send and Cancel widths equal (±0.5 px); URL field width unchanged while in flight | **Fixed** |
| DR-006 two selected rows | `11-context-menu-dark.png`: only the open request is filled; the context target has an outline | **Fixed** |
| DR-007 theme seg stretch | `18-settings-general-dark.png`: track is content-width | **Fixed** |
| DR-008 OpenAPI project select | `15-import-openapi-preview-dark.png`: "Into [Default]" in the footer next to "Import 5 requests" | **Fixed** |
| DR-009 orphan "Status —" | Empty-state screenshots: no placeholder strip; env editor shows "Variables resolve: project env → Global" there instead | **Fixed** |

New screenshots: `docs/qa/design/screens_r3/` (112 files from `shoot.mjs`, plus `30-zoom150-*` from `ui/e2e/design_review/qa_zoom150.mjs`). The DR baselines in `screens/` are untouched.

## 3. Contract conformance (re-run)

- **Static** (`qa/r2/contract_check.mjs`): 62 Rust commands, all registered and typed, all mocked; 85 UI call sites; events `sync://status` / `sync://changed` consistent. **0 problems.**
- **Dynamic** (`qa/r2/contract_probe.js`, 87 calls incl. live sync/share, real IPC vs mock, `qa/r2/shape_diff.mjs`):
  - No functional shape or error-code difference remains.
  - What's left is session data in the real run (`sidebarTab`, `shareOptions`, sync connected), plus the known benign cases: Tauri arg-deserialisation errors are plain strings, which the UI maps to `internal`.
  - One mock divergence remains: the mock still *rejects* a fractional `sidebarWidth` (`invalid`) while the real backend now accepts and rounds it → QA-R3-003.

## 4. Real app run (release build, custom-protocol, Xvfb + tauri-driver + WebKitWebDriver)

Runner: `docker compose run -d --name qa dev sleep infinity`, then `docker exec qa bash qa/r2/env_up.sh` and `docker exec qa bash -c '. /tmp/qa-env.sh; node qa/r2/real_app_smoke.mjs'`. The one-shot `qa/r2/run_real_app.sh` is also updated for the preinstalled drivers.

| # | Check | Result |
|---|---|---|
| 1–2 | Start; first launch My Workspace / Default / Global | PASS |
| 3 | New request → local echo → 200 + body | PASS (click→status 63–87 ms) |
| 4 | Ctrl+U cURL QUERY + JSON → server sees QUERY + body; Accept-Query highlighted | PASS |
| 5 | Ctrl+Shift+C → clipboard; **multi-line layout** (QA-R2-004) | PASS |
| 6–7 | OpenAPI import via dialog + by path for all 3 fixtures | PASS (7–11 ms backend) |
| 8 | Env `{{baseUrl}}` substitution; secret redacted in history | PASS |
| 9 | Cancel in flight → "Cancelled", history `cancelled` | PASS |
| 10–11 | 6 MB limited view; 1 MB pretty render | PASS |
| 12 | History rows | PASS |
| R3-a | Fractional `save_settings` + real splitter drag persists | PASS |
| R3-b | Share create → Settings › Update keeps credentials + env | PASS |
| 13 | Restart: history + active env survive | PASS |
| 14 | 87-call contract probe through real IPC | PASS |
| 15 | Cold start ×5 | PASS |

## 5. R2 gaps closed this round (Playwright vs mock, `ui/e2e/qa_r3.spec.ts`)

| Check | Result |
|---|---|
| Narrow widths: 800×600, 853×533 (150 %), 768×480 (125 % of min window) | PASS: no horizontal overflow, actions on screen, send + import work |
| 10 KB URL + 4 KB header value | PASS: layout holds, send works, Copy as cURL > 14 KB, history row fits |
| Rapid Send/Cancel ×20 (click), then Mod+Enter ×10 | PASS: ends idle, exactly 20 sends and 20 history entries, then a normal send returns 200, no page errors |
| DR-005 Send/Cancel width | PASS |
| axe-core WCAG 2.0/2.1 A+AA on main, collections, Import, Settings and Env picker (dark + light) | **FAIL → QA-R3-002** (reports in `qa/r2/out/axe-*.json`) |

`@axe-core/playwright@4.10.2` was added as a devDependency of `ui` (pinned).

## 6. New bugs

Count: **P0 0 · P1 0 · P2 0 · P3 4.**

| ID | Sev | Component | AC / area | Summary |
|---|---|---|---|---|
| QA-R3-001 | P3 (gate) | ui `src/lib/api/types.ts` | contract copy | The UI copy is out of sync with `docs/types.ts`: one comment on `Settings.sidebarWidth`. Devs' own unit test `types.ts copy › is identical to docs/types.ts` fails, so `npm test` is red. Fix: copy the file |
| QA-R3-002 | P3 | ui a11y | DESIGN a11y, §4 | axe, same on every screen in both themes: (a) **aria-required-children** (critical): the Request tablist contains the settings gear `button`, and the Response tablist contains the Pretty/Raw `group`; (b) **color-contrast** (serious, 28 nodes): CodeMirror line-number gutter `#737985` = 4.1:1 (< 4.5); (c) **nested-interactive** (serious, 11): history rows (`div[data-index]`) contain focusable buttons; (d) **scrollable-region-focusable** (serious): the read-only response `.cm-scroller` is not keyboard-focusable. Repro: `BUG QA-R3-002` in `ui/e2e/qa_r3.spec.ts` |
| QA-R3-003 | P3 | ui browser mock | test infra | The mock still emulates the old `save_settings` rejection of a fractional `sidebarWidth` (`mock/index.ts:652`); the contract now accepts and rounds. The mock's keep-variables export also places `--globoff` before the URL, while real and contract put it after. The mock no longer mirrors the backend for these cases |
| QA-R3-004 | P3 | ui Env editor | AC-ENV-6 | The Env editor header says "3 variables" (enabled count, from `KvTable`), while the sidebar row for the same env shows "4" (all variables). One label should change ("3 of 4 enabled"), or both should use the same count. See `16-env-editor-dark.png` |

## 7. Final performance (linux/aarch64 container, release build)

| Metric | Budget | Measured | Verdict |
|---|---|---|---|
| Frontend bundle JS+CSS gz | ≤ 350 KB | 283.3 KB excl. mock (305.5 incl.); 78.6 KB eager; CodeMirror chunk lazy (108.8 KB) | PASS |
| Cold start to interactive | ≤ 500 ms p50 / 800 p95 | WebDriver session → UI interactive 370–446 ms over two runs (p50 400–436); webview `performance.now()` at ready 157–182 ms. Light workspace, Xvfb software rendering | PASS (indicative) |
| Response render, 1 MB JSON | ≤ 200 ms | 94–102 ms click → highlighted CodeMirror, end to end | PASS |
| Cancel (AC-REQ-8) | ≤ 100 ms | 16–75 ms click → "Cancelled", incl. WebDriver round trips | PASS |
| 6 MB response (5 MB limited view) | — | 70–75 ms | info |
| OpenAPI import | 1,000 ops ≤ 1.5 s | Fixtures (2–19 ops) 7–11 ms backend, 55–75 ms via dialog. 1,000-op spec not re-measured in release (R1 debug: 1.40 s) | Not measured at budget size |
| Release binary | installers: .dmg ≤ 12 MB, AppImage ≤ 80 MB | `reqharbor` stripped ELF aarch64 **7.67 MB**; installers not built; macOS .dmg not measurable here | Indicative PASS |
| Time-to-send overhead | ≤ 5 ms p95 | Not measured (no core bench) | Gap |
| Idle RAM/CPU, list filters, cURL parse | see PRODUCT §7 | Not measured | Gap |

## 8. Remaining gaps (accepted for MVP)

- Real-app runs are Linux/WebKitGTK only. WKWebView (macOS) and WebView2 (Windows) are untested, so the HiDPI and zoom checks there rest on the Chromium emulation plus the code fixes.
- Not measured: installer sizes, idle RAM/CPU, time-to-send bench, 1,000-op OpenAPI import in release.
- Not driven: sync timers (debounce, focus pull, backoff), native file dialogs / OS file drop, and a second-instance launch.

## 9. QA files touched this round (no product code)

| File | Change |
|---|---|
| `qa/r2/real_app_smoke.mjs` | +multi-line cURL assertion, +R3-a (fractional settings + real drag), +R3-b (share options survive Update), robust waits + failure screenshots |
| `qa/r2/env_up.sh`, `qa/r2/run_real_app.sh` | Use the preinstalled `tauri-driver` / `WebKitWebDriver` |
| `ui/e2e/qa_r3.spec.ts` | axe (`BUG QA-R3-002`), narrow widths / zoom, DR-005, long URL/headers, rapid Send/Cancel |
| `ui/e2e/design_review/qa_zoom150.mjs` | DR-001 150 % zoom screenshots |
| `ui/package.json` (+lock) | devDependency `@axe-core/playwright` 4.10.2 |
| `docs/qa/design/screens_r3/` | Re-shot design screens |
| `qa/r2/out/` | Real-app results, contract JSON, axe reports, screenshots |

# QA Report — Round 2, Full System (core + src-tauri + UI + Worker + real app)

Date: 2026-11-26 · Time-boxed by the PO (≈30 min report window). Everything ran in Docker (`docker compose run --rm dev …`, Rust 1.100, Node 22, linux/aarch64 container on an Apple-silicon host).
Sources of truth: `docs/PRODUCT.md`, `docs/API_CONTRACT.md` (incl. §0 Changes), `docs/types.ts`, `docs/qa/QA_REPORT_R1_backend.md`.

**Verdict: close to releasable.** All gates are green. All 27 fixable R1 bugs are verified fixed, and their repro tests were not edited after R1. The real Tauri app was built (release, `custom-protocol`), started under Xvfb and driven end to end through tauri-driver and WebKitWebDriver (16/16 checks). It was also checked against the browser mock with the same 87-call contract probe. No P0 or P1 contract mismatch was found. One P2 contract bug breaks settings persistence on HiDPI displays (QA-R2-001). The rest are P3.

## 1. Gate results

| Gate | Command | Result |
|---|---|---|
| Rust format | `cargo fmt --all -- --check` | PASS |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| Rust tests | `cargo test --workspace` | PASS: 116 core unit, 10 src-tauri, 11 http_send, 15 sync_mock, 1 curl_parity, qa_curl 26, qa_http 19 (+1 ignored: R1-019 won't fix), qa_openapi 13 (+1 ignored measurement), qa_store_vars 13, qa_sync_live 4 ignored (live) |
| App build | `cargo build -p reqharbor-app` | PASS |
| Live Worker | `bash qa/run_live_worker_tests.sh` | PASS: curl smoke **58/58**, live Rust⇄Worker **4/4** (incl. R1-004/005/026 repros) |
| Worker types | `npm run typecheck` | PASS |
| Worker tests | `npm test` | PASS 121/121 (6 files) |
| Worker build | `npm run build` (dry-run) | PASS: 74.2 KiB, 22.9 KiB gz |
| UI check | `npm run check` | PASS (0 errors, 0 warnings) |
| UI unit | `npm test` | PASS 80/80 |
| UI build | `npm run build` | PASS |
| UI size | `npm run size` | PASS: 77.3 KB initial; 281.5 KB total excl. mock (budget 350 KB) |
| UI e2e (devs) | `npm run e2e` | PASS 17/17 |
| UI e2e (QA R2) | `npx playwright test e2e/qa_*.spec.ts` | 11/12. The one failure is the intended repro `BUG QA-R2-001` |
| Static contract check | `node qa/r2/contract_check.mjs --table` | PASS: 62 commands, 0 problems |
| Real app smoke | `bash qa/r2/run_real_app.sh` (see §4) | PASS 16/16 |

QA changes required by the brief:
- `bug_qa_r1_019_*` is now `#[ignore = "won't fix: reqwest/hyper HeaderMap loses interleaved response header order (accepted P3)"]`.
- `qa/worker_smoke.sh` now expects `entity > 512 KB` to return **200** with `results[0].status == "rejected"` and `reason == "entity_too_large"` (contract C2). All three checks pass.

Green run that skips only open repros: `cargo test --workspace` (green as is), `npx playwright test --grep-invert "BUG QA"`.

## 2. R1 regression verification

Method: (a) re-run every R1 repro; (b) check that the repro tests were not weakened. There is no git, so (b) uses file mtimes. `qa_curl.rs`, `qa_openapi.rs`, `qa_store_vars.rs`, `qa_sync_live.rs` and `worker/test/qa_parity_security.test.ts` were last modified at 01:14–01:17. That is before the R1 report was written (01:19) and before any dev fix (01:23–01:36). Only `qa_http.rs` changed after that, and only by my R1-019 `#[ignore]`. I re-read each repro body against the R1 description; all still assert the R1-described expectation. For the code-review-only bugs I read the fix.

| R1 bug | Sev | Verification | Status |
|---|---|---|---|
| 004 push > 5 MB | P0 | live `bug_qa_r1_004` passes against wrangler (size-bounded batches, `sync_rt::sync_once` → `plan_push`/`push_batch`) | Fixed |
| 005 oversized entity blocks sync | P0 | live `bug_qa_r1_005` passes; Worker returns per-change `rejected` (smoke checks); client reports `skippedEntities`/`tooLarge` | Fixed |
| 001 secrets in history via errors | P1 | `bug_qa_r1_001`, `001b` pass; real app: history `resolvedUrl` shows `{{tok}}` (§4 step 8) | Fixed |
| 002 decompression bomb | P1 | `bug_qa_r1_002` passes. Note: the test also passes when `response` is `None`, which matches the contract (`error.kind "other"`) | Fixed |
| 003 Cookie/Proxy-Auth on cross-host redirect | P1 | `bug_qa_r1_003` passes | Fixed |
| 006a–e share-page cURL parity | P2 | 5 vitest repros pass; a shared fixture parity test was added (`curl_parity.rs` / `curl_parity.test.ts`, 40 cases) | Fixed |
| 007 rotate-token race | P2 | Code: `sync_rotate_token` takes `sync_lock`; `run_sync` treats a 401 after rotation as retry (`sync_policy.rs` "QA-R1-007 / C4") | Fixed (code review; no harness) |
| 008 quadratic nesting on main thread | P2 | `bug_qa_r1_008` passes; `preview_openapi`/`import_openapi` are `async` + `spawn_blocking` | Fixed |
| 024 no quota | P2 | vitest repro passes (50 MB quota, `quota_exceeded`) | Fixed |
| 026 updatedAt tie divergence | P2 | live repro passes (deviceId tie-break, contract C3) | Fixed |
| 027 share import local file refs | P2 | Code: `share::strip_file_references` runs on import, and preview warnings are produced; core unit tests at `share.rs:470/482`. Mock does the same | Fixed |
| 009/009b worker variable semantics | P3 | vitest passes | Fixed |
| 010–016 cURL edge cases | P3 | 7 repros pass | Fixed |
| 017, 018, 020 HTTP edge cases | P3 | repros pass | Fixed |
| 019 header interleaving | P3 | won't fix (PO); ignored with reason | Accepted |
| 021 `save_all` deletes unknown files | P3 | repro passes | Fixed |
| 022 YAML merge keys | P3 | repro passes | Fixed |
| 023 single instance | P3 | `tauri-plugin-single-instance` registered in `main.rs`; the real app ran fine on a session D-Bus | Fixed (not stress-tested) |
| 025 IPv6 /64 rate limit | P3 | vitest passes | Fixed |
| 028 corrupt sync-state drops tombstones | P3 | Code: falls back to `SYNC_STATE_BACKUP` (`storage.rs:301`) | Fixed (code review) |

## 3. Contract conformance (UI ↔ src-tauri)

Three independent checks:

1. **Static** (`qa/r2/contract_check.mjs`). Parses every `#[tauri::command]` in `commands.rs` plus the `generate_handler!` list, the `Commands` map in `docs/types.ts` and `ui/src/lib/api/types.ts`, the mock handler table, all 84 `call("…", {…})` sites in `ui/src`, and the emitted, typed and listened event names. It checks command names, camelCase arg keys, required-vs-optional, and unknown or omitted keys at call sites.
   - **0 problems.** 62/62 commands are registered and typed. The two `types.ts` copies are identical. The mock covers all 62.
   - Events `sync://status` / `sync://changed` match on all three sides.
2. **Dynamic, real IPC vs mock** (`qa/r2/contract_probe.js` + `qa/r2/shape_diff.mjs`). One probe runs **87 calls** covering every command, including error paths and the full sync/share cycle against a live `wrangler dev`. It ran inside the **real Tauri webview** (`window.__TAURI_INTERNALS__.invoke`) and inside Chromium against the **browser mock** (`ui/e2e/qa_contract_shapes.spec.ts`). The JSON shapes were then diffed recursively.
   - Outputs: `qa/r2/out/contract_real.json`, `qa/r2/out/contract_mock.json`.
   - Error codes matched for all 30 error paths: `not_found`, `invalid`, `validation`+details, `format_error`+details, `not_curl`, `curl_*`, `openapi_*`, `sync_not_connected`, `share_gone`. One exception is noted below.
   - `get_response_body` returns an `ArrayBuffer` on both sides.
3. **Real UI flows** (§4) exercised the most-used commands through the real UI code.

Command-level summary (full generated table: `node qa/r2/contract_check.mjs --table`):

| Area | Commands | Static | Real vs mock shapes |
|---|---|---|---|
| Bootstrap/settings | bootstrap, get_settings, save_settings | OK | **save_settings: real rejects fractional `sidebarWidth` (u32), mock accepts → QA-R2-001** |
| Projects | list/get_tree/create/rename/duplicate/delete/move_project, set_active_environment, child_counts | OK | OK. `duplicate_project` copy has `activeEnvironmentId: null` in real, remapped in mock (QA-R2-005) |
| Collections/folders/requests | 15 commands incl. move_node | OK | OK. `move_node` into itself → `invalid` on both sides |
| Environments | 6 commands | OK | OK. Deleting Global → `invalid` on both sides |
| Send/response | send_request, cancel_request, get/save_response_body, format_body, url_to_params, params_to_url | OK | OK (ArrayBuffer, `validation` details kinds, `format_error` line/column) |
| History | 4 commands | OK | OK |
| cURL | import_curl, looks_like_curl, export_curl | OK | Shapes OK. Export text formatting differs (QA-R2-004) |
| OpenAPI | preview_openapi, import_openapi | OK | Real OK. **Mock** fails to parse `qa/fixtures/openapi/webhooks_v31.yaml` (`openapi_parse`) (QA-R2-006) |
| Sync | 7 commands | OK | OK against live Worker |
| Shares | 7 commands | OK | `share_update` returns `kind/entityId/name: null` and `share_create` returns `name: null`. The mock fills them (QA-R2-003) |
| Error shape | all | — | Tauri argument-deserialisation errors and unknown commands reject with a **plain string**, not `CommandError`. The UI's `toCommandError` maps them to `{code:"internal"}`, so nothing crashes. The mock never produces this case |
| Events | sync://status, sync://changed | OK | Emitted during live sync, payloads match `SyncStatus` / `SyncReport` |

## 4. Real app run (release build, custom-protocol, Xvfb + tauri-driver + WebKitWebDriver)

Setup:
- Build: `cargo build --release -p reqharbor-app --features tauri/custom-protocol` (2m12s warm).
- Install: `cargo install tauri-driver --locked --root /cache/target/tools` (tauri-driver 2.1.0, in the `target` volume).
- `apt-get install webkit2gtk-driver` in the run container.
- Session D-Bus via `dbus-daemon --session`.
- Driver: a dependency-free W3C WebDriver client (`qa/r2/real_app_smoke.mjs`).
- Local targets: `qa/r2/echo_server.mjs` (echo, QUERY + `Accept-Query`, 6 MB, slow, gzip, cookies) and `wrangler dev`.
- One-shot runner: `docker compose run --rm dev bash qa/r2/run_real_app.sh`. Long-lived variant: `qa/r2/env_up.sh`.
- Screenshots and logs are in `qa/r2/out/`.

| # | Check | Result |
|---|---|---|
| 1 | App starts, three-pane UI renders inside Tauri (`__TAURI_INTERNALS__` present) | PASS (UI ready 863 ms incl. WebDriver session) |
| 2 | First launch: "My Workspace" / "Default" / Global env (AC-COL-1), project switcher | PASS |
| 3 | New request → `http://127.0.0.1:8099/echo/first?a=1&b=two` → 200, echoed body | PASS (click→status rendered 80 ms) |
| 4 | Ctrl+U cURL import of `-X QUERY … --data-raw '{"role":"admin"}'` → send → server saw QUERY + body; `accept-query` row present in Headers (AC-QRY-2/5/7) | PASS |
| 5 | Ctrl+Shift+C → clipboard plugin → `-X QUERY … --data-raw`, toast (AC-CEXP-1) | PASS (`qa/r2/out/exported_curl.txt`) |
| 6 | OpenAPI import (Petstore 3.0 YAML) via dialog: chip "OpenAPI 3.0 · YAML", "Import 19 requests", collection created | PASS (136 ms) |
| 7 | `import_openapi` by **path** for all 3 fixtures (Swagger 2.0 11 ops, 3.1 2 ops, 3.0 19 ops) | PASS (11–18 ms each) |
| 8 | Env substitution: `{{baseUrl}}/echo/env?k={{tok}}` with Ctrl+E picker → server received resolved URL; history shows `k={{tok}}` (secret redacted) | PASS |
| 9 | Send/Cancel on a slow request → "Cancelled", history `errorKind=cancelled` (AC-REQ-8) | PASS |
| 10 | 6 MB JSON → "Showing first 5 MB as plain text" + size "6 MB" (AC-RES-3) | PASS |
| 11 | 1 MB JSON pretty + highlighted | PASS |
| 12 | 6 history rows in Activity | PASS |
| 13 | **Restart**: history rows and active env survive (AC-HIST-1, COL-6, ENV-5) | PASS |
| 14 | Contract probe through real IPC (87 calls) | PASS (see §3) |
| 15 | Cold start × 5 | PASS (see §6) |
| — | Page errors (`error` / `unhandledrejection`) during run | none |

Environment notes (not product bugs):
- In my first attempt WebDriver `POST /session` hung. That attempt also set `LIBGL_ALWAYS_SOFTWARE=1 WEBKIT_DISABLE_COMPOSITING_MODE=1 NO_AT_BRIDGE=1`; without them the session is created in 0.2–0.8 s. These variables were removed from the runner.
- Recommend adding permanently to `docker/dev.Dockerfile`: **`webkit2gtk-driver`**, plus `tauri-driver` (cargo install). Without them every run spends minutes in `apt-get`. `imagemagick` and `x11-utils` were only needed for debugging.

## 5. Bugs

Count: **P0 0 · P1 0 · P2 2 · P3 5 (7 total).**

| ID | Sev | Component | AC | Summary |
|---|---|---|---|---|
| QA-R2-001 | P2 | ui `App.svelte` ⇄ src-tauri `Settings` | §4 "layout persists", AC-ENV-5 | Fractional sidebar width → `save_settings` rejected → **no setting is persisted any more** (errors swallowed) |
| QA-R2-002 | P2 | ui `SettingsDialog` / `ShareDialog` | AC-SHARE-2/4 | "Update" in Settings → Shares re-uploads with `includeCredentials:false, environmentId:null`, silently dropping the attached environment / included credentials |
| QA-R2-003 | P3 | core `sync::update_share` / `create_share` | AC-SHARE-4/6, contract §3.10 | `share_update` returns `kind/entityId/name = null`, `share_create` returns `name = null` (mock returns them) |
| QA-R2-004 | P3 | core curl export | AC-CEXP-2 | Export puts `curl -X METHOD '<url>'` on one line; AC (and the mock/Worker port) put each part on its own ` \`-joined line |
| QA-R2-005 | P3 | core `storage::duplicate_project` | AC-COL-2, AC-ENV-5 | Duplicated project loses its active environment (copied envs exist, `activeEnvironmentId` = null) |
| QA-R2-006 | P3 | ui mock YAML parser | test infra | Browser mock rejects `qa/fixtures/openapi/webhooks_v31.yaml` with `openapi_parse`; real backend imports it. Mock-based e2e cannot cover real-world 3.1 YAML |
| QA-R2-007 | P3 | ui `app.svelte.ts` init | AC-SYNC-2 | `sync://` listeners are registered only after bootstrap and the tree/history loads. A startup pull that finishes in that window is missed: the indicator can stay "Syncing", and pulled data is not reloaded until the next event (code review, timing-dependent) |

### Details

**QA-R2-001 (P2).** Repro: `ui/e2e/qa_contract_bugs.spec.ts` "BUG QA-R2-001" fails with `sidebarWidth sent to save_settings: 320.5`. Real-IPC evidence (`contract_real.json`, label `save_settings.fractionalSidebarWidth`): `invalid args 'settings' for command 'save_settings': invalid type: floating point '312.5', expected u32`.
- Cause: `Splitter` passes raw `PointerEvent.clientX` deltas. These are fractional on scaled displays (e.g. Windows 125 %/150 %, browser zoom). `sidebarDrag` stores `dragStart + delta` unrounded.
- Rust `Settings.sidebar_width: u32` rejects the whole object, and `updateSettings` does `.catch(() => {})`.
- Effect: from then on every `save_settings` in the session fails silently, losing theme, active project, last opened request, sidebar tab and layout. On restart the user gets the previous state.
- The mock accepts any number, so the devs' e2e could not catch it.
- Fix: `Math.round` in `sidebarDrag` (and/or make Rust accept f64 and round), and surface save failures.

**QA-R2-002 (P2, code review).** `SettingsDialog.updateShare` calls `share_update` with `includeCredentials: false, environmentId: null`. Updating a share that was created with an attached environment, or with "Include credentials", replaces the snapshot without them: recipients lose `baseUrl` and similar variables. The ShareDialog also doesn't remember the previous options (it defaults them). Expected: reuse the share's original options, or ask.

**QA-R2-003 (P3).** The Worker's `PUT /v1/shares/:id` response is passed through unchanged. `ShareDialog.create()` assigns it to `existing`, so its `kind`/`entityId`/`name` become null. There is no visible breakage today, because the next update takes kind/entityId from props. Fill them client-side or in the Worker.

**QA-R2-004 (P3).** Real output: `curl -X QUERY 'http://…/query?x=1' \⏎  -H …`. AC-CEXP-2 lists `curl`, then `-X METHOD`, then `'<url>'`, "joined with ` \` + newline". The mock (and probably the Worker's TS port) emit them on separate lines, so app and share page differ cosmetically. The command is still valid bash.

**QA-R2-005 (P3).** `storage.rs` `duplicate_project` sets `active_environment_id: None` even though it copies the project's environments.

**QA-R2-006 (P3).** `contract_mock.json` → `preview_openapi` / `import_openapi`: `openapi_parse` for the OAI-style 3.1 fixture (webhooks, server variables, `type: [x, 'null']`).

**QA-R2-007 (P3, code review).** `main.rs` spawns `run_sync` in `setup` before the webview loads. `AppStore.init` awaits `bootstrap` and three loads before `listen("sync://…")`. Fix: subscribe before `bootstrap`, or re-read `get_sync_status` after subscribing.

Observations, not filed:
- The macOS `⌘F` e2e only works when the test host is a Mac, because CodeMirror derives `Mod` from the host OS, not from `?platform=`. The spec handles this.
- The `mock-*.js` chunk (21.7 KB gz) ships in `dist` but is never loaded in Tauri.
- A plain `cargo build --release` (without `tauri/custom-protocol` or `tauri build`) produces a binary that loads `devUrl`. Release CI must use `npx tauri build`.

## 6. Performance (linux/aarch64 container, release build unless noted)

| Metric | Budget | Measured | Verdict |
|---|---|---|---|
| Frontend bundle JS+CSS gz | ≤ 350 KB | 281.5 KB total excl. mock (303.2 KB incl. mock); 77.3 KB eager; CodeMirror `editor` chunk 108.7 KB lazy | PASS |
| Cold start to interactive | ≤ 500 ms p50 / 800 p95 | WebDriver session-create → UI interactive: 409–465 ms (p50 434). Webview `performance.now()` at ready: 165–194 ms. Empty-ish workspace, not the 1,000-request reference workspace, under Xvfb software rendering | PASS (indicative) |
| Release binary size | (installer: .dmg ≤ 12 MB; AppImage ≤ 80 MB) | `reqharbor` ELF aarch64 stripped: **7.67 MB**. .dmg/.msi/AppImage not built; macOS .dmg can't be measured here | Indicative PASS |
| Response render, 1 MB JSON | ≤ 200 ms | click → CodeMirror shown 83 ms end-to-end (network 4 ms; 50 ms poll granularity) | PASS |
| 6 MB response (5 MB limited view) | — | 228 ms click → banner | info |
| Cancel | ≤ 100 ms (AC-REQ-8) | click → "Cancelled" 17 ms | PASS |
| OpenAPI import (release, real app) | 1,000 ops ≤ 1.5 s | Fixture specs 11–18 ms (backend), UI dialog 136 ms. The 1,000-op heavy spec was not re-measured in release (R1 debug: 1.40 s) | Not measured at budget size |
| Time-to-send overhead (≤ 5 ms p95) | ≤ 5 ms | Not measured (no core bench written in the time box). UI click→200 rendered was 80 ms incl. WebDriver | Gap |
| Idle RAM / CPU, history/sidebar filter, cURL parse | see §7 | Not measured this round | Gap |

## 7. Coverage gaps

- Not done in the time box:
  - the planned exploratory UI specs: narrow widths, very long URLs/headers, rapid Send/Cancel storms, theme switching, axe-core a11y (`@axe-core/playwright` not added);
  - AC-by-AC UI e2e beyond the shortcut suite;
  - core time-to-send bench;
  - release-mode 1,000-op OpenAPI timing;
  - idle RAM/CPU.
- The real app ran on Linux/WebKitGTK only. WKWebView (macOS) and WebView2 (Windows) were not tested; QA-R2-001 is most likely on Windows HiDPI.
- Sync runtime timers (2 s debounce, 60 s focus pull, backoff) were not exercised in the real app. Sync/share commands were exercised once against live wrangler through the real IPC.
- Not driven: OS file drop / native file pickers (`save_response_body` was only called with a path), and the single-instance second launch.

## 8. QA files (no product code changed)

| File | Purpose |
|---|---|
| `crates/core/tests/qa_http.rs` | R1-019 marked `#[ignore]` (won't fix) |
| `qa/worker_smoke.sh` | entity > 512 KB → 200 + per-change `rejected` (C2) |
| `qa/r2/contract_check.mjs` | Static Rust ⇄ types.ts ⇄ mock ⇄ call-site ⇄ events check (`--table`) |
| `qa/r2/contract_probe.js` | 87-call probe shared by real app and mock |
| `qa/r2/shape_diff.mjs` | Recursive shape diff real vs mock |
| `qa/r2/echo_server.mjs` | Local HTTP target (QUERY, big, slow, gzip, cookies, CORS) |
| `qa/r2/real_app_smoke.mjs`, `qa/r2/run_real_app.sh`, `qa/r2/env_up.sh`, `qa/r2/wd_probe.mjs` | Real-app WebDriver smoke + runners |
| `qa/r2/out/*` | Results JSON, logs, screenshots from the real-app run |
| `ui/e2e/qa_helpers.ts`, `ui/e2e/qa_contract_shapes.spec.ts`, `ui/e2e/qa_contract_bugs.spec.ts` (BUG QA-R2-001), `ui/e2e/qa_shortcuts.spec.ts` (§6 shortcuts ×2 platforms, AC-KEY-1/2, AC-RES-6, F2) | Playwright vs mock |

Convention: tests titled `BUG QA-R2-NNN` are failing repros that should pass once fixed. All other QA tests are passing regression checks.

# QA Report — Round 1, Backend (core + Tauri commands + Worker)

Date: 2026-11-26 · Scope: `crates/core` (reqharbor-core), `src-tauri` (reqharbor-app), `worker/` · UI not tested.
Sources of truth: `docs/PRODUCT.md`, `docs/API_CONTRACT.md`. Accepted deviations not re-reported: header-name casing (AC-REQ-3), HEAD exported as `--head` by the Rust exporter.

**Verdict: not releasable yet.** Two P0 sync bugs make sync fail permanently for ordinary projects, and three P1s cover a secret leak, a memory-exhaustion crash and a credential leak. The happy paths are solid: devs' gates are green, the live end-to-end sync/share flow works against the real Worker, and the Worker's authz/XSS/CSP/SQL handling held up under attack.

## 1. Gate results (devs' gates, re-run in Docker)

| Gate | Command | Result |
|---|---|---|
| Rust format | `cargo fmt --all -- --check` | PASS |
| Rust lint | `cargo clippy --workspace --all-targets -- -D warnings` | PASS |
| Rust tests | `cargo test --workspace` | PASS before QA files (114 unit + 11 http_send + 8 sync_mock; reqharbor-app has **0** tests) |
| App build | `cargo build -p reqharbor-app` | PASS |
| Worker types | `npm run typecheck` | PASS |
| Worker tests | `npm test` | PASS before QA files (59/59) |
| Worker build | `npm run build` (dry-run deploy) | PASS (54.8 KiB, 17.1 KiB gz) |
| Live Worker smoke | `docker compose run --rm dev bash qa/run_live_worker_tests.sh` | 57/57 curl checks PASS; live Rust⇄Worker: 1 pass, 3 bug repros fail |

With the QA files added, fmt, clippy and typecheck stay green. `cargo test --workspace` and `npm test` now go **red on purpose**, because the `bug_*` / `BUG QA-R1-*` repro tests fail until the bugs are fixed. To get a green run that skips only the open repros:
`cargo test --workspace -- --skip bug_` and `npx vitest run -t "^(?!.*BUG QA)"` (both verified green).

## 2. Bugs

Severity: P0 = blocks a core flow for normal users / client–worker contract mismatch; P1 = security/privacy or crash; P2 = significant functional or security weakness; P3 = minor.
Count: **P0 2 · P1 3 · P2 6 · P3 17 (28 total).**

| ID | Sev | Component | AC | Summary |
|---|---|---|---|---|
| QA-R1-004 | P0 | core sync ⇄ worker | §8, AC-SYNC-2/6 | Push batches are capped by count (500) but not bytes; the Worker caps bodies at 5 MB → 413, sync never succeeds |
| QA-R1-005 | P0 | core sync ⇄ worker | §8, AC-SYNC-2 | One entity > 512 KB makes the whole push 413; no other change ever syncs |
| QA-R1-001 | P1 | core send/history | AC-ENV-7 | Secret variable values written to `history.jsonl` through error messages |
| QA-R1-002 | P1 | core http | AC-RES-4 | Decompression bomb: decoded size unbounded → OOM crash |
| QA-R1-003 | P1 | core http | AC-QRY-4, API_CONTRACT §3.5 | `Cookie` / `Proxy-Authorization` forwarded to another host on redirect |
| QA-R1-006 | P2 | worker `curl.ts` | AC-SHARE-3, §8 | Share-page "Copy as cURL" diverges from the Rust exporter (5 sub-cases) |
| QA-R1-007 | P2 | src-tauri sync | AC-SYNC-7 | Token rotation racing an in-flight sync disconnects sync and drops the new token |
| QA-R1-008 | P2 | core openapi + src-tauri | AC-OAS-1/9 | Deeply nested input parses quadratically on the main thread → UI freeze |
| QA-R1-024 | P2 | worker | §8 | No per-workspace storage quota (anonymous workspaces can fill D1) |
| QA-R1-026 | P2 | core sync ⇄ worker | AC-SYNC-4 | Equal `updatedAt` from two devices → permanent divergence |
| QA-R1-027 | P2 | core share import | AC-SHARE-5 | An imported share can point Binary/Form file fields at any local file |
| QA-R1-009 | P3 | worker `variables.ts` | AC-ENV-2 | Variable depth/cycle semantics differ from the core |
| QA-R1-010 | P3 | core curl import | AC-CURL-17 | Windows cmd `^` style imports a garbage URL instead of a clear error |
| QA-R1-011 | P3 | core curl import | AC-CURL-12 | `--url-query` silently loses its parameters |
| QA-R1-012 | P3 | core curl import | AC-CURL-13 | `-b` + `-b` / `-H Cookie` → several Cookie headers |
| QA-R1-013 | P3 | core curl import | AC-CURL-3, AC-REQ-1 | `-X post` imports a method that fails validation on Send |
| QA-R1-014 | P3 | core curl | AC-CEXP-6 | JSON body `{"query": ...}` round-trips as GraphQL |
| QA-R1-015 | P3 | core curl | AC-CEXP-6 | Form-encode row with an empty key round-trips as Text |
| QA-R1-016 | P3 | core curl import | AC-CURL-17 | Leading BOM → "Not a cURL command" |
| QA-R1-017 | P3 | core http | AC-RES-4 | Empty body with `Content-Encoding: gzip` (HEAD/204/304) reports a decode error |
| QA-R1-018 | P3 | core http / curl | AC-CURL-13 | `timeoutMs: 0` (e.g. `curl -m 0`) times out every request |
| QA-R1-019 | P3 | core http | AC-RES-5 | Response headers are not in received order (duplicates grouped) |
| QA-R1-020 | P3 | core http | AC-QRY-4 | Redirect with a raw UTF-8 `Location` is not followed |
| QA-R1-021 | P3 | core storage | AC-COL-6 | `save_all` silently deletes unrecognised files in `projects/` |
| QA-R1-022 | P3 | core openapi | AC-OAS-1 | YAML merge keys (`<<: *x`) are ignored |
| QA-R1-023 | P3 | src-tauri | AC-COL-6 | No single-instance guard: two app instances lose each other's writes |
| QA-R1-025 | P3 | worker | §8 | Workspace-create rate limit is per exact IP; IPv6 /64 bypasses it |
| QA-R1-028 | P3 | core storage/sync | AC-SYNC-5 | Corrupt `sync-state.json` silently drops pending tombstones |

### Details

Commands: Rust repros run with `docker compose run --rm dev cargo test -p reqharbor-core --test <file> <test>`; live ones with `docker compose run --rm dev bash qa/run_live_worker_tests.sh`; worker ones with `docker compose run --rm -w /app/worker dev npx vitest run test/qa_parity_security.test.ts`.

**QA-R1-004 (P0) — push batches exceed the Worker's 5 MB body limit.**
Repro: live `qa_sync_live::bug_qa_r1_004_push_batch_exceeds_worker_5mb_limit` (500 requests with ~12 KB JSON bodies, a normal project).
Expected: the first sync succeeds. Actual: `Rejected("payload_too_large: Request body exceeds 5242880 bytes")`. The error is non-retryable (`sync_rejected`, state `Error`), and every later save re-collects the same batch and hits 413 again, so the workspace can never sync. `sync::push_all` and `sync_rt::run_sync` chunk by `BATCH = 500` changes only. For scale: the heavy OpenAPI test below generated ~47 MB of request bodies for 1,000 operations. Fix direction: chunk by serialized size (< 5 MB) as well as by count.

**QA-R1-005 (P0) — one oversized entity blocks all sync.**
Repro: live `qa_sync_live::bug_qa_r1_005_one_oversized_entity_blocks_all_sync` (one 600 KB request plus one small request).
Expected: the small request reaches the server, and the oversized one is reported (e.g. in `SyncReport.skipped`). Actual: `Rejected("entity_too_large: changes[4].data exceeds 524288 bytes")`; the Worker rejects the whole batch and the small request never syncs, on any retry. The client has no notion of the 512 KB entity limit. Fix direction: client pre-check and skip/report, or the Worker returns a per-change `rejected` status instead of failing the batch.

**QA-R1-001 (P1) — secret values persisted to history via error messages.**
Repro: `qa_store_vars::bug_qa_r1_001_secret_leaks_into_history_via_error_message` and `bug_qa_r1_001b_secret_leaks_via_invalid_url_error`.
Expected (AC-ENV-7): no secret value in `history.jsonl`. `resolvedUrl` is correctly `...?api_key={{token}}`. Actual: `error.message` stores `error sending request for url (http://127.0.0.1:9/v1/items?api_key=TOPSECRET123): ... Connection refused`. reqwest errors (connect, DNS, TLS, timeout-in-body) and the engine's own `Invalid URL "<resolved url>"` both embed the resolved URL. Fix direction: redact resolved secret values (or replace the URL with `display_url`) in `SendError.message` before building the history entry.

**QA-R1-002 (P1) — decompression bomb.**
Repro: `qa_http::bug_qa_r1_002_gzip_bomb_decoded_size_is_unbounded`.
Expected: decoded output bounded (e.g. by `MAX_BODY_BYTES`, with an error or truncation). Actual: 315,300 wire bytes decode to 314,572,800 bytes; brotli reaches far higher ratios. `MAX_BODY_BYTES` is checked only on wire bytes, and `decode_body` uses unbounded `read_to_end`. Any hostile or misconfigured server can make the app allocate gigabytes; release builds use `panic = "abort"`, so OOM kills the app. Fix direction: decode through `.take(MAX_BODY_BYTES + 1)`.

**QA-R1-003 (P1) — credentials forwarded to another host on redirect.**
Repro: `qa_http::bug_qa_r1_003_cookie_header_leaks_on_cross_host_redirect`.
Expected: `Cookie` and `Proxy-Authorization` are stripped on a cross-host hop, like `Authorization` already is (and like curl ≥ 7.83, browsers and reqwest's default policy). Actual: both reach the second host. A session cookie pasted from DevTools via cURL import goes to whatever host the server redirects to. Authorization stripping itself is correct for 301/302/303/307/308, both when typed as a header and when set via the Auth tab (passing test).

**QA-R1-006 (P2) — share-page "Copy as cURL" diverges from the Rust exporter.**
Repro: `worker/test/qa_parity_security.test.ts`, tests "BUG QA-R1-006a…e".
Each sub-case shows the Rust exporter's behaviour, then what `worker/src/curl.ts` does instead:
- **a.** Rust adds `--globoff` for URLs containing `[]` or `{}`; the TS port does not. curl then fails with "bad range" / "nested brace" for `?ids[]=1`, and for every raw "Keep {{variables}}" copy.
- **b.** Rust emits `--head` for HEAD; the TS port emits `-X HEAD`, which makes curl wait for a body that never arrives.
- **c.** Rust quotes form file paths containing `;` or `,`; the TS port leaves them unquoted.
- **d.** Rust uses `--form-string` for text values containing `;` or starting with `"`; the TS port uses `-F`, so curl re-parses the value (e.g. it treats `;type=` as an attribute).
- **e.** Rust percent-encodes Form-encode keys; the TS port emits them raw, so `a=b` changes the field name.

**QA-R1-007 (P2, code review, no harness for src-tauri) — token rotation race.**
`commands::sync_rotate_token` does not take `state.sync_lock`. If `run_sync` (debounced push, focus pull or retry) is mid-flight with the old token, its next call gets 401, and `run_sync` runs `sync::disconnect`, wiping the sync config including the new token that was just saved. Expected: rotation is serialised with sync, and a 401 after rotation does not disconnect. Actual: the user sees "Token rejected — sync disconnected" right after a successful rotate.

**QA-R1-008 (P2) — quadratic parse of deeply nested documents freezes the UI.**
Repro: `qa_openapi::bug_qa_r1_008_deeply_nested_document_parses_quadratically`; timings via `measure_nesting_parse_time` (`--ignored --nocapture`).
Expected: hostile input rejected quickly. Actual (debug build):

| Nesting depth | Time to reject |
|---|---|
| 1k | 12 ms |
| 4k | 118 ms |
| 16k | 1.25 s |
| 200k (≈ 400 KB) | > 40 s (killed) |

JSON text that fails to parse is re-parsed as YAML, doubling the cost. `preview_openapi` and `import_openapi` are synchronous Tauri commands, so this runs on the main thread and freezes the window. Fix direction: run them as `async` / `spawn_blocking`, and pre-check nesting depth or size.

**QA-R1-024 (P2) — no per-workspace quota.**
Repro: vitest "BUG QA-R1-024".
One anonymous workspace stored 60 shares of ~0.9 MB each (54 MB); entities and shares have no count or byte cap. With 10 creates/h/IP (and see 025), D1's 10 GB limit can be filled. Expected: a per-workspace storage or share cap.

**QA-R1-026 (P2) — `updatedAt` tie divergence.**
Repro: live `qa_sync_live::bug_qa_r1_026_equal_updated_at_from_two_devices_diverges`.
The Worker resolves equal `updatedAt` in favour of the later push. The client treats a pulled change with the same `updatedAt` as "already have it" (`apply_pull`, the `local_updated == change.updated_at` branch). Result: `A="from A" B="from B"` permanently. Rare in practice (a same-millisecond edit on two devices), but it is a semantic mismatch between client and Worker. Fix direction: also compare content or the rev, or make ties deterministic on both sides.

**QA-R1-027 (P2, code review) — share import can reference arbitrary local files.**
`share::import_snapshot` copies `body.binaryPath` and `form[].value` for file fields verbatim from an untrusted share link. A crafted share (URL `https://attacker`, Binary body `~/.ssh/id_rsa`) uploads that file on Send. Expected: file paths cleared or flagged on import, with a warning in the import preview.

**QA-R1-009 (P3) — worker variable semantics differ from the core.**
Repro: vitest "BUG QA-R1-009 / 009b".
For `a→b→c→d→"x"` the core returns `x` but the Worker returns `{{d}}` (its depth counter starts at 1). For a cycle `a="x{{b}}", b="y{{a}}"` the core leaves `{{a}}` literal but the Worker yields `xy{{a}}`. As a result, the share page's resolved cURL differs from the app's export.

**QA-R1-010…016 (P3) — cURL import edge cases** (`qa_curl.rs`):
- **010.** Chrome "Copy as cURL (cmd)" imports URL `^https://api.example.com/v1/items^`, with only "Ignored extra argument" warnings. The dialect is out of scope, but it should fail with a clear message.
- **011.** `--url-query 'page=2'` is listed as an unknown flag and the parameter is lost. Expected URL: `?page=2&q=a%20b`.
- **012.** `-b a=1 -b b=2 -H 'Cookie: c=3'` produces 3 Cookie headers. RFC 6265 §5.4 allows one; curl merges `-b` values.
- **013.** `-X post` imports method `post`, which is only rejected when the user clicks Send.
- **014.** A JSON-typed body `{"query":"status:open"}` re-imports as GraphQL (AC-CEXP-6 requires the same body type).
- **015.** A Form-encode row with key `""` exports as `--data-urlencode '=value'` and re-imports as Text.
- **016.** A text starting with a BOM (`\u{feff}curl …`) gives `NotCurl`, and `looks_like_curl` is false too.

**QA-R1-017…020 (P3) — HTTP engine** (`qa_http.rs`):
- **017.** A 204 with `Content-Encoding: gzip` and an empty body gives `decode_error = "gzip decode failed: unexpected end of file"`. This is normal for HEAD/204/304 responses.
- **018.** `timeoutMs: 0` gives `Timeout: "Timed out after 0 ms"`. `curl -m 0` means "no limit" and imports as 0.
- **019.** Received order `z-first, set-cookie, a-second, set-cookie` is reported as `z-first, set-cookie, set-cookie, a-second`, because reqwest's `HeaderMap` iteration groups names.
- **020.** A raw UTF-8 `Location: /café` is not followed: `to_str()` fails and the 302 is returned as final, with no hint. curl and browsers follow it.

**QA-R1-021 (P3).** Repro: `qa_store_vars::bug_qa_r1_021_save_all_deletes_unrecognised_project_files`. Any `projects/*.json` whose stem is not a loaded project id (e.g. a user backup) is deleted by `save_all` after every sync pull, with no quarantine or report.

**QA-R1-022 (P3).** Repro: `qa_openapi::bug_qa_r1_022_yaml_merge_keys_not_applied`. `- <<: *common` followed by `name: tenant` imports no parameter, because serde_yaml_ng → `serde_json::Value` does not apply merge keys. Plain anchors and aliases work (passing test).

**QA-R1-023 (P3).** No `tauri-plugin-single-instance`. Two processes on one data dir lose each other's writes; `qa_store_vars::two_stores_same_dir_last_writer_wins_but_valid` passes and documents the lost update. Files never corrupt (atomic writes held under an 8-thread stress test).

**QA-R1-025 (P3).** Repro: vitest "BUG QA-R1-025". 15 creates from 15 addresses in one IPv6 /64 all succeed. Fix direction: bucket IPv6 by /64.

**QA-R1-028 (P3, code review).** A corrupt `sync-state.json` is quarantined and reset to default. Pending tombstones are lost, so local deletes never propagate, and every entity is re-pushed (which worsens 004).

## 3. AC coverage (backend-relevant)

Covered by the devs' tests, by name or body: AC-REQ-1..11, QRY-2..6, RES-1/2/4/5, CURL-1..17, CEXP-2..6, OAS-1..9, COL-1/2/3/5/6, ENV-1/2/3/6/7, HIST-1/3/4/5, SYNC-1/3/4/5/6/7, SHARE-1..6.

Gaps and weak coverage:
- **reqharbor-app (src-tauri) has 0 tests.** Untested:
  - the `send_request` / `cancel_request` in-flight registry;
  - the 8-entry response cache and `get_response_body(limit)` (AC-RES-3);
  - the `import_share` / `share_*` command glue;
  - `sync_rt` debounce (AC-SYNC-2: 2 s push, 60 s focus pull, no idle timers), retry scheduling (AC-SYNC-6) and 401 → disconnect (AC-SYNC-7);
  - token rotation (see 007).
- **AC-SYNC-\* / AC-SHARE-\*:** the client is only tested against a Rust mock worker (`tests/sync_mock.rs`), which does not enforce the real Worker's 5 MB / 512 KB limits or its tie rule. That is how 004, 005 and 026 slipped through. Recommend keeping `qa_sync_live.rs` and running it in CI with wrangler.
- **AC-SHARE-3:** no test compared the share page's cURL against the Rust exporter (006, 009).
- **AC-ENV-7:** only success-path history was tested; failure paths leak (001).
- **AC-RES-4:** no bound on decoded size (002), and no empty-body case (017).
- **AC-RES-5:** header order with interleaved duplicates untested (019).
- **AC-QRY-4:** cross-origin tests only check `Authorization` (003).
- **AC-ENV-4** (`get_variables` source env, masking): only `VarScope::visible()` is unit-tested, not the command.
- **AC-ENV-5** (active env persists per project): covered indirectly in `ac_env_6_*`.
- **AC-HIST-2:** summary status chip is not asserted.
- **AC-QRY-7:** covered only through `accept_query` extraction.
- **AC-OAS-9:** the devs' 1,000-op test uses small schemas. My heavy 1,000-op spec (12 interlinked schemas, allOf/arrays/cycles) imports in **1.40 s in a debug build**, so it passes; the release build was not measured. That spec generates ~47 MB of request bodies (max 96 KB each), which matters for 004.
- UI-only ACs (COL-4, KEY-*, RES-6, REQ-12, CEXP-1, QRY-1 editor parts) are out of scope this round.

## 4. Notes

- **Real public specs:** downloads from the sandbox host were denied, so per the brief I wrote realistic equivalents in `qa/fixtures/openapi/`: Swagger Petstore OpenAPI 3.0 (YAML, integer response keys, relative server `/api/v3`), Petstore Swagger 2.0 (formData file upload, `in: body`, oauth2 + apiKey) and an OAI-style 3.1 spec (webhooks, server variables, `type: [x, 'null']`, `examples`). All import correctly. Also passing:
  - `$ref` cycles (self, mutual, parameter/requestBody ref loops), anchors and aliases;
  - billion-laughs YAML, rejected in 44 ms;
  - unquoted float versions (`openapi: 3.0`);
  - Postman/AsyncAPI/4.0 rejection;
  - external refs reported.
- **Real Petstore 3.0 quirk (P3, not filed):** its relative server URL yields `baseUrl = "/api/v3"`, and a send then goes to `https:///api/v3/pet`, i.e. host `api`. A warning is shown at import time.
- **Live Worker:** the full Rust-client flow passed against `wrangler dev`: create, connect, push/pull, a second device adopting the tree, LWW conflict, delete propagation, the secret never stored, and share create → public JSON → import → update (same URL) → list → revoke → 410 → update 410, plus token rotation (old token 401). The curl smoke (57 checks) covered auth errors, stale pushes, XSS payloads in every field rendered escaped, strict hash-based CSP, `X-Robots-Tag`, `Cache-Control`, no cookies, CORS on `.json`, cross-workspace isolation (404), 413 (declared and chunked), 415, 400, 404/405, SQL-ish inputs, and the rate limit (11th create → 429 with `Retry-After`). No Worker authz or SQL-injection issues were found.
- **cURL import:** held up well. Chrome (`$'…'` with `!`, `-b`), Safari (`-X 'POST'`, `--data-binary`) and Firefox (`--compressed`, urlencoded) copies, `-G` with data/urlencode/fragment, `@file` vs `--data-raw @`, unbalanced quotes (clear `curl_syntax` error), 30 hostile inputs (no panics), weird-header/body round trips, and a 10 KB command of tiny flags all pass.
- **Not tested:** the Svelte UI; Windows/macOS-specific path behaviour; release-build performance budgets; TLS failure modes beyond the devs' self-signed test.

## 5. QA files (no product code changed)

| File | Contents |
|---|---|
| `crates/core/tests/qa_curl.rs` | 26 tests, 7 `bug_` repros |
| `crates/core/tests/qa_http.rs` | 20 tests, 6 `bug_` repros |
| `crates/core/tests/qa_store_vars.rs` | 13 tests, 3 `bug_` repros |
| `crates/core/tests/qa_openapi.rs` | 13 tests + 1 ignored measurement, 2 `bug_` repros |
| `crates/core/tests/qa_sync_live.rs` | 4 `#[ignore]` live tests, 3 `bug_` repros; needs wrangler — use the runner script |
| `worker/test/qa_parity_security.test.ts` | 14 tests, 9 `BUG QA-R1-*` repros |
| `qa/run_live_worker_tests.sh` | One-shot runner: migrations, wrangler dev, curl smoke, live Rust tests, stop |
| `qa/worker_smoke.sh` | curl smoke (57 checks) |
| `qa/fixtures/openapi/*` | Realistic Petstore 3.0 / Swagger 2.0 / 3.1 specs |

Convention: any test named `bug_qa_r1_NNN_*` (Rust) or titled `BUG QA-R1-NNN` (vitest) is the failing repro for that bug and should pass once it is fixed. Every other QA test is a passing regression check.

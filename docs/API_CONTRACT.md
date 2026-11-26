# ReqHarbor — Frontend ⇄ Rust API Contract

Status: implemented · Source of truth for the Svelte UI · Types: [`docs/types.ts`](./types.ts) (copy it into `ui/src/lib/`).
Backend: `src-tauri/src/commands.rs` (thin wrappers) over `crates/core` (`reqharbor-core`).

## 0. Changes since the first version (QA round 1)

| Change | Where |
|---|---|
| `import_share` now returns `ShareImportResult { tree, warnings }` (was `ProjectTree`). File paths from a share (Binary body, Form file fields) are cleared and the file fields disabled; `warnings` lists them. | §3.10 |
| `preview_share` result gains `warnings: string[]` (same list, before importing). | §3.10 |
| New command `sync_workspace_info` → `RemoteWorkspace` (incl. `bytesUsed`, `quotaBytes`). | §3.9 |
| `SyncStatus` gains `skippedEntities: SkippedEntity[]` and `warning: string \| null`; `SyncReport` gains `tooLarge: SkippedEntity[]`. | §3.9 |
| New error codes `sync_too_large`, `sync_quota_exceeded`. `sync_rotate_token` may reject with `details: { token }` if the new token could not be saved to disk (show it anyway). | §1 |
| `preview_openapi` / `import_openapi` run off the main thread (same signature; they were already promises in JS). Documents nested > 256 levels are rejected with `openapi_invalid`; YAML merge keys (`<<: *x`) are applied. | §3.8 |
| `timeoutMs: 0` means no timeout. | §2 |
| Behaviour: secrets are redacted from everything written to history; Cookie / Proxy-Authorization / API-key headers are dropped on cross-origin redirects; decoded bodies are capped at 256 MB (`error.kind "other"`); only one app instance runs at a time (a second launch focuses the first window). | §3.5 |
| cURL import: leading BOM / zero-width chars ignored; Windows cmd `^` syntax rejected with `curl_syntax`; `-X post` → `POST` (other invalid methods produce a warning); several `-b` / `Cookie` sources are merged into one `Cookie` header; `--url-query` supported; `{"query": …}` without `variables` stays a JSON body. | §3.7 |

### QA round 2

| Change | Where |
|---|---|
| `share_create` now returns `name`; `share_update` returns `kind`, `entityId` and `name`. Signatures unchanged. The UI keeps each share's upload options in `settings.json` under `shareOptions` (preserved by the backend like any unknown settings key) and re-sends them on "Update". | §3.10 |
| `save_settings` accepts fractional numbers for `sidebarWidth` / `windowWidth` / `windowHeight` (rounded, clamped) and out-of-range `splitRatio` (clamped to 0..1); an unknown `theme` falls back to `"system"` instead of rejecting the save. | §3.1 |
| `export_curl` layout: every part on its own line — `curl`, then the method flag (if any), then the URL, then flags, joined with ` \` + newline + two spaces. E.g. `curl \⏎  -X QUERY \⏎  'https://…' \⏎  -H 'Content-Type: application/json' \⏎  --data-raw '{}'`. | §3.7 |
| `duplicate_project` keeps the active environment (pointing at the copied environment). | §3.2 |

## 1. Calling convention

```ts
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { makeCall, type CommandError } from "./types";

export const call = makeCall(invoke);
const tree = await call("get_project_tree", { projectId });
try { await call("save_request", { request }); } catch (e) { const err = e as CommandError; /* err.code */ }
```

* Command names are **snake_case**; argument keys are **camelCase** (Tauri converts them). All argument/return objects use camelCase keys.
* Optional arguments (`?:` in `Commands`) may be omitted or `null`.
* Every command either resolves with the documented result or **rejects with a `CommandError`**:
  `{ code: string, message: string, details?: object }` — `message` is human-readable and may be shown as is.
* JS packages: `@tauri-apps/api@^2`, `@tauri-apps/plugin-dialog@^2` (open/save file pickers, confirm dialogs), `@tauri-apps/plugin-clipboard-manager@^2` (`writeText` for Copy as cURL / Copy link / copy header values), dev dep `@tauri-apps/cli@^2`. Granted permissions: `core:default`, `dialog:allow-open|save|ask|confirm|message`, `clipboard-manager:allow-write-text|read-text` (`src-tauri/capabilities/default.json`). There is no fs plugin: the backend reads/writes files given a path.
* Dev server: Tauri runs `npm run dev` in `/ui` (`devUrl` `http://localhost:5173`); production build runs `npm run build` and serves `/ui/dist`. `ui/dist/index.html` is currently a placeholder.
* CSP: `default-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' data: blob:; style-src 'self' 'unsafe-inline'; font-src 'self' data:; script-src 'self'` — no external resources; use `blob:` URLs for image previews.
* Window label `main`, title "ReqHarbor", 1280×800, min 960×600. `dragDropEnabled: true`: files dropped on the window arrive via `getCurrentWebview().onDragDropEvent()` with absolute paths (use them for OpenAPI import and Binary/Form file fields). Because of this, HTML5 drag-and-drop is unreliable on Windows — implement the sidebar tree DnD (AC-COL-3) with pointer events.

### Error codes

| code | when |
|---|---|
| `not_found` | id does not exist (project, collection, folder, request, environment, history entry, cached response) |
| `invalid` | invalid input: empty name, deleting the last project or the Global environment, moving a node across projects or into itself, bad method on save |
| `io` | disk error |
| `validation` | send blocked; `details` = `{kind:"invalidMethod",message}` \| `{kind:"invalidGraphqlVariables",message}` (AC-REQ-5) \| `{kind:"emptyUrl"}` |
| `format_error` | `format_body` failed; `details` = `{message, line, column}` (1-based) — leave text untouched, move the cursor there (AC-REQ-6) |
| `not_curl` | text is not a curl command → show "Not a cURL command", change nothing (AC-CURL-17) |
| `curl_missing_url`, `curl_syntax` | curl command without URL / with an unterminated quote |
| `openapi_parse`, `openapi_unsupported`, `openapi_invalid` | AC-OAS-1 rejection ("Unsupported format" chip for `openapi_unsupported`) |
| `sync_not_connected` | sync/share call without a connection → "Sharing needs sync" empty state |
| `sync_unauthorized` | 401 — sync has been disconnected (AC-SYNC-7) |
| `sync_offline`, `sync_server` | network / 5xx — automatic retry is already scheduled (AC-SYNC-6) |
| `sync_quota_exceeded` | the remote workspace is full (50 MB) — not retried; show `message` in Settings → Sync |
| `sync_too_large` | a single change exceeded the Worker's body limit (normally handled internally by splitting/skipping) |
| `sync_not_found`, `sync_rejected`, `sync_protocol` | other Worker errors (message includes the Worker's `code: message`) |
| `share_gone` | share link revoked (410) |
| `internal` | unexpected |

## 2. Data model

Exactly PRODUCT.md §3, serialized as in `types.ts`. Notes that matter to the UI:

* **IDs** are ULIDs created by the backend. The only id the UI creates is a scratch request's `id` and the `executionId` of a send (any unique string).
* **Scratch request**: a `Request` with `collectionId: ""`. Build it client-side with defaults: `method "GET"`, `params/headers []`, `auth {type:"none"}`, `body {type:"none"}`, `settings {followRedirects:true, timeoutMs:30000, verifyTls:true}`, `sortOrder 0`, timestamps `0`. Persist it with `create_request` (Save-to picker, AC-REQ-11).
* **URL ⇄ Query (AC-REQ-2)**: `request.url` holds the full URL including the query string of *enabled* params; `params` holds all rows incl. disabled ones. Keep them in sync on every edit with `url_to_params(url, params)` (URL edited) / `params_to_url(url, params)` (rows edited) — or port `crates/core/src/urlx.rs` (≈60 lines) to TS for zero-latency typing. Query values are raw (not decoded); encoding only touches space, `&`, `#`, `"`, `<`, `>`, control/non-ASCII chars (and `=` in keys), so text round-trips unchanged. At send time a non-empty `params` list is the source of truth for the query string.
* **Sorting**: arrays inside `ProjectTree` are not sorted; order siblings by `sortOrder`. Folders and requests of the same parent share one `sortOrder` sequence; `move_node` reorders folders and requests separately (folders first is fine).
* **Auth Inherit** uses the parent collection's saved `auth` at send/export time (AC-COL-5). An enabled `Authorization` header always wins over the Auth tab (AC-REQ-7) — show the hint.
* **Default Content-Type** (AC-REQ-4) is added by the backend at send time only when no enabled `Content-Type` header exists: json/graphql `application/json`, xml `application/xml`, text `text/plain`, formEncoded `application/x-www-form-urlencoded`, form `multipart/form-data; boundary=…`, binary `application/octet-stream`. The UI may display them as greyed "auto" headers.
* **Variables** (AC-ENV-1..4): `{{ name }}` with `name` in `[A-Za-z0-9_.-]+`, whitespace trimmed. Resolution: project's active environment → Global; disabled vars skipped; nested refs up to depth 3; cycles stay literal; unresolved stay literal and are reported in `SendResult.unresolvedVariables`. For editor decorations/hover use `get_variables(projectId)` (first definition per key wins, includes source env name; mask secrets).
* **Timestamps**: `updatedAt` is set by the backend on each mutation (strictly increasing per entity). Do not edit it.
* **Timeout**: `settings.timeoutMs: 0` disables the timeout (imported from `curl -m 0`).

## 3. Commands

Mutating commands persist atomically before resolving (AC-COL-6) and, when sync is connected, schedule a push 2 s later (debounced, AC-SYNC-2). Debounce editor autosave in the UI (≤ 500 ms).

### 3.1 Bootstrap & settings

| command | args | result | notes |
|---|---|---|---|
| `bootstrap` | – | `Bootstrap` | First call on startup. First launch already contains "My Workspace" / project "Default" / Global env (AC-COL-1). Show a persistent error toast for each `corruptFiles[]` entry (AC-COL-6). `platform` drives ⌘ vs Ctrl. |
| `get_settings` | – | `Settings` | |
| `save_settings` | `settings: Settings` | `Settings` | Whole object; unknown keys are preserved, so UI-only prefs can be added freely. Store `activeProjectId`, `lastOpenedRequestId`, pane sizes, theme here. Pixel sizes may be fractional (rounded to integers); the result echoes the normalised values. |

### 3.2 Projects

| command | args | result |
|---|---|---|
| `list_projects` | – | `Project[]` (sorted) |
| `get_project_tree` | `projectId` | `ProjectTree` |
| `create_project` | `name` | `ProjectTree` |
| `rename_project` | `projectId, name` | `Project` |
| `duplicate_project` | `projectId` | `ProjectTree` (deep copy incl. environments, name "X copy"; the copy's active environment is the copy of the original's) |
| `delete_project` | `projectId` | `Project[]` remaining (deletes its collections/folders/requests/environments; `invalid` for the last project) |
| `move_project` | `projectId, index` | `Project[]` |
| `set_active_environment` | `projectId, environmentId \| null` | `Project` (AC-ENV-5; env must belong to the project; `null` = "No environment") |
| `child_counts` | `kind: "project"\|"collection"\|"folder"\|"request", id` | `ChildCounts` — for "This also deletes 2 folders and 14 requests." |

### 3.3 Collections, folders, requests

| command | args | result |
|---|---|---|
| `create_collection` | `projectId, name` | `Collection` |
| `update_collection` | `collection: Collection` | `Collection` — updates `name`, `description`, `auth` only |
| `duplicate_collection` | `collectionId` | `ProjectTree` |
| `delete_collection` | `collectionId` | `ProjectTree` |
| `create_folder` | `collectionId, parentFolderId?, name` | `Folder` |
| `rename_folder` | `folderId, name` | `Folder` |
| `duplicate_folder` | `folderId` | `ProjectTree` (subtree copied) |
| `delete_folder` | `folderId` | `ProjectTree` (subtree deleted) |
| `create_request` | `collectionId, folderId?, request?` | `Request` — saves `request` (scratch, history "Save to collection", import) as a **new** request with a new id; omitted → empty GET "Untitled request" |
| `save_request` | `request: Request` | `Request` — ⌘S; placement fields (`collectionId`, `folderId`, `sortOrder`) are ignored (use `move_node`); `invalid` if the method is invalid |
| `rename_request` | `requestId, name` | `Request` |
| `duplicate_request` | `requestId` | `Request` ("X copy", placed last) |
| `delete_request` | `requestId` | `ProjectTree` |
| `move_node` | `kind: "collection"\|"folder"\|"request", id, targetCollectionId?, targetFolderId?, index` | `ProjectTree` — drag-and-drop (AC-COL-3). Folder/request: required `targetCollectionId` (same project), `targetFolderId` null = collection root, `index` among same-kind siblings. Collection: only `index`. A folder moves with its subtree; moving a folder into itself → `invalid`. |

Dirty state (AC-REQ-11): the UI compares the edited request with the last saved one; sending never saves.

### 3.4 Environments

| command | args | result |
|---|---|---|
| `list_environments` | – | `Environment[]` (Global has `projectId: null`) |
| `create_environment` | `projectId, name` | `Environment` |
| `update_environment` | `environment: Environment` | `Environment` — `name` + `variables` |
| `duplicate_environment` | `environmentId` | `Environment` (Global cannot be duplicated) |
| `delete_environment` | `environmentId` | `Environment[]` (Global cannot be deleted; clears it as active env) |
| `get_variables` | `projectId?` | `VariableInfo[]` |

### 3.5 Sending & responses

| command | args | result |
|---|---|---|
| `send_request` | `executionId, request, projectId?` | `SendResult` |
| `cancel_request` | `executionId` | `boolean` |
| `get_response_body` | `responseId, limit?` | `ArrayBuffer` (raw bytes over IPC) |
| `save_response_body` | `responseId, path` | `null` |
| `format_body` | `kind: "json"\|"xml", text` | `string` (JSON: 2-space, numbers/escapes preserved) |
| `url_to_params` | `url, params` | `KeyValue[]` |
| `params_to_url` | `url, params` | `string` |

Send flow:

1. Generate `executionId`, switch Send → Cancel, call `send_request` with the request **as edited** (it is never saved).
2. ⌘Enter / Cancel → `cancel_request(executionId)`; the pending `send_request` resolves within 100 ms with `error.kind = "cancelled"` (AC-REQ-8).
3. Result: exactly one of `response` / `error` is set. `error.kind` ∈ `dns|connect|tls|timeout|cancelled|other` drives the error panel (AC-REQ-9) — never treat it as an exception. A rejection with code `validation` means nothing was sent (invalid custom method, invalid GraphQL variables → "Fix GraphQL variables", empty URL).
4. Fetch the body with `get_response_body(response.responseId)`. For bodies > 5 MB pass `limit: 5*1024*1024` and show "Showing first 5 MB" + Save full body (`save_response_body` with a path from the dialog plugin's `save()`), AC-RES-3. Decode text with `new TextDecoder()`. The last 8 response bodies stay available.
5. Status strip: `status` + `statusText`, `sizeBytes` (decoded) with tooltip `wireSizeBytes` + `contentEncoding` (AC-RES-4), `timeMs`; `unresolvedVariables.length > 0` → amber warning (AC-ENV-3).
6. Viewer by `bodyKind` (AC-RES-2); Headers tab from `headers` (received order, lowercase names), highlight `accept-query` (AC-QRY-7); Cookies tab from `cookies` (AC-RES-5).
7. A history entry has been appended (`historyId`) — refresh the Activity list (`list_history`) or prepend a summary built from the result.

History never contains secret variable values: every string of a history entry (URL, error message, response headers and text bodies) has them replaced by `{{name}}` (values shorter than 3 characters are not redacted).

Backend behaviour the UI can rely on: URL without scheme → `http://` for localhost/127.0.0.1/[::1], else `https://` (AC-REQ-10); disabled rows are never sent; duplicate headers are sent separately (AC-REQ-3); `Accept-Encoding: gzip, deflate, br`, `Accept: */*` and `User-Agent: ReqHarbor/<version>` are added unless the user set them; gzip/deflate/br are decoded (AC-RES-4); redirects are followed manually (≤ 10) with QUERY semantics (AC-QRY-4) unless `followRedirects` is false; `Authorization`, `Cookie`, `Proxy-Authorization` and the Auth tab's API-key header are dropped on cross-origin redirects; `timeoutMs` covers the whole exchange (`0` = none); decoded bodies over 256 MB fail with `error.kind "other"`. Known limitation: response headers with repeated names are listed grouped at the position of their first occurrence (reqwest/hyper do not keep the interleaving).

### 3.6 History

| command | args | result |
|---|---|---|
| `list_history` | – | `HistorySummary[]`, newest first, ≤ 500 (AC-HIST-1) |
| `open_history_entry` | `historyId` | `OpenedHistory` — request for the editor + `response` meta with a `responseId` for `get_response_body` (AC-HIST-4, no re-send). `entry.error` holds failed/cancelled sends. |
| `delete_history_entry` | `historyId` | `null` |
| `clear_history` | – | `null` (confirm in the UI first) |

Filtering (AC-HIST-3) is done in the UI over the summaries: case-insensitive substring match on `resolvedUrl`, `method`, or `String(status)`. "Save to collection" = `create_request(collectionId, folderId, entry.request)`. Secret variable values never appear in history (`resolvedUrl` shows `{{name}}`).

### 3.7 cURL

| command | args | result |
|---|---|---|
| `import_curl` | `text` | `CurlImport` — rejects `not_curl` / `curl_missing_url` / `curl_syntax` |
| `looks_like_curl` | `text` | `boolean` — URL-bar paste detection (`text.trimStart().startsWith("curl ")` is equivalent) |
| `export_curl` | `request, projectId?, keepVariables` | `string` — multi-line bash, one part per line (AC-CEXP-2..5). `keepVariables:false` resolves all variables incl. secrets against the active env; `true` exports raw `{{vars}}` (adds `--globoff`). Copy with the clipboard plugin. |

Import preview summary ("3 headers · JSON body · Basic auth") is derived from `request`. Unknown flags → dismissible warning from `unknownFlags`; `warnings` lists other notes (e.g. a browser multipart copy omits file contents — "choose it again"). The imported request is a new scratch request; to save it use `create_request`. Export details: `-X` omitted for GET without body and POST with body; HEAD without body is exported as `--head`; implied Content-Type headers are written explicitly so cURL sends the same bytes; Basic → `-u`; Bearer/API key → header/query; `verifyTls:false` → `-k`.

### 3.8 OpenAPI

| command | args | result |
|---|---|---|
| `preview_openapi` | `text?` or `path?` | `OpenApiPreview` — chip `spec.label`, collection name, folders with counts, `baseUrl`, warnings (OAuth2 skipped, external `$ref`s) |
| `import_openapi` | `projectId, text?` or `path?` | `OpenApiImportResult` — creates 1 collection + 1 environment in the project; nothing is created on error (AC-OAS-1) |

`path` comes from the dialog plugin's `open({ filters: [{ name: "OpenAPI", extensions: ["json","yaml","yml"] }] })` or a window file drop. If both are given, non-empty `text` wins. Primary button text: `Import ${report.operationsCreated} requests`.

### 3.9 Sync (Settings → Sync)

| command | args | result |
|---|---|---|
| `get_sync_status` | – | `SyncStatus` |
| `sync_create_workspace` | `workerUrl, name?` | `CreatedWorkspace` — creates the remote workspace, connects, runs the first sync. Show `token` once with Copy (AC-SYNC-1). |
| `sync_connect` | `workerUrl, token` | `SyncStatus` — validates the token (`sync_unauthorized` if wrong), connects, runs the first sync (a fresh device adopts the remote tree; its empty default project is dropped) |
| `sync_disconnect` | – | `SyncStatus` (local data untouched) |
| `sync_rotate_token` | – | `{ token }` (old token invalid immediately; show once) |
| `sync_now` | – | `SyncReport` — "Sync now" (pull, then push) |
| `sync_workspace_info` | – | `RemoteWorkspace` — name, entity count, `bytesUsed` / `quotaBytes` for a storage meter (null if the Worker doesn't report them) |

Automatic behaviour (no UI work needed beyond listening): pull on startup and on window focus (max once per 60 s); push 2 s after any mutation; on offline/5xx a single retry timer with backoff 5 s → 10 s → … → 5 min (`nextRetryAt` for "Retrying in 30 s"); on 401 sync is disconnected and status becomes `unauthorized` → show "Token rejected — sync disconnected. Enter a new token." No timers run when idle.

Events (subscribe with `listen`):

| event | payload | UI action |
|---|---|---|
| `sync://status` | `SyncStatus` | sidebar sync indicator (§5.14 of DESIGN): hidden when `!connected` (unless `unauthorized`), `syncing` → dimmed icon, `offline` → warning, `unauthorized` → danger "Sync off" |
| `sync://changed` | `SyncReport` | reload `list_projects`, the open `get_project_tree`, `list_environments`; for each `conflicts[]` toast "Kept your edit as “{name}”" (AC-SYNC-4) |

Synced: projects, collections, folders, requests, environments (secret values blanked; local secret values are kept on pull). Never synced: history, settings (AC-SYNC-3, AC-ENV-7).

Limits: pushes are batched by ≤ 500 changes and ≤ 4 MB per request. An entity whose data exceeds 512 KB is never sent: it is listed in `SyncStatus.skippedEntities` / `SyncReport.tooLarge` with `reason: "too_large"` and `SyncStatus.warning` carries a ready-made sentence — show it as a warning on the sync indicator (other changes keep syncing). Editing the item so it shrinks makes it sync again. Ties on `updatedAt` are resolved by a per-device id, so two devices always converge. Token rotation is serialised with sync runs.

### 3.10 Sharing

| command | args | result |
|---|---|---|
| `share_preview` | `kind, entityId, includeCredentials, environmentId?` | `SharePreview` — counts for the "Removed before sharing" section (works offline) |
| `share_create` | same | `ShareInfo` — copy `url` (AC-SHARE-1); needs sync (`sync_not_connected` → "Sharing needs sync" + Set up sync) |
| `share_update` | `shareId, kind, entityId, includeCredentials, environmentId?` | `ShareInfo` (same URL, AC-SHARE-4). The snapshot is rebuilt with exactly these options — re-send the share's original options (kept by the UI in `settings.shareOptions`) unless the user changed them. |
| `share_revoke` | `shareId` | `null` (URL then returns 410) |
| `share_list` | – | `ShareInfo[]` — Settings → Shares (AC-SHARE-6) |
| `preview_share` | `url` | `{ kind, snapshot: ShareSnapshot, warnings }` — Import dialog Link tab preview (`warnings` = file references that import will clear); `share_gone` if revoked |
| `import_share` | `url, projectId` | `ShareImportResult { tree, warnings }` — copy with new ids (AC-SHARE-5); a request share lands in a new collection named after the request; an attached environment is created too. Binary body paths and Form file fields from the share are cleared (file fields disabled) so a crafted link cannot upload local files; show `warnings` |

Sanitising (AC-SHARE-2): secret variables are always removed from the attached environment; unless `includeCredentials`, `Authorization` / `Proxy-Authorization` / `Cookie` header values become `{{authorization}}` / `{{proxyAuthorization}}` / `{{cookie}}` and auth values become `{{username}}`, `{{password}}`, `{{token}}`, `{{apiKey}}`. A request share inlines its collection's auth when it uses Inherit. The UI must remember `shareId` per entity for "Update share" — use `share_list` (`entityId`) to find it.

## 4. Storage (for reference)

App data dir (`bootstrap().dataDir`; macOS `~/Library/Application Support/dev.reqharbor.desktop`): `workspace.json`, `projects/<projectId>.json`, `environments.json`, `settings.json`, `sync-state.json` (tombstones + sync bookkeeping), `history.jsonl`. Atomic writes (temp + fsync + rename); corrupt files are renamed `*.corrupt-<ts>.json` and reported in `bootstrap().corruptFiles`.

## 5. Not provided by the backend (UI responsibility)

Sidebar filter (AC-COL-4), history filter (AC-HIST-3), relative times, method/status colours, CodeMirror search/highlighting, Tests/Pre Run placeholders (AC-REQ-12), keyboard shortcuts (AC-KEY-*), QUERY-without-body hint (AC-QRY-3), Format button wiring (`format_body`), toasts, dirty tracking.

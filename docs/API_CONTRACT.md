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

### Forward compatibility (accounts plan M0)

| Change | Where |
|---|---|
| Every data object (`Workspace`, `SyncState`, `Project`, `Collection`, `Folder`, `Request`, `Environment`, `Variable`, `KeyValue`, `Auth`, `ApiKeyAuth`, `Body`, `GraphqlBody`, `FormField`, `RequestSettings`) keeps keys it does not know — on disk, over IPC, on the sync wire, in duplicates, imports and conflict copies. `save_request` keeps unknown keys of the stored request that the update lacks. | §2 |
| `Request` gains `kind?: RequestKind` (`"http"` default, omitted when `"http"`; `"graphql"`, `"grpc"`, `"ws"` or any other string) and `minClientVersion?: string`. `Body.type` accepts any string; unknown values round-trip unchanged. | §2 |
| New error code `read_only_newer`: the request was made by a newer ReqHarbor and can't be edited or sent here. | §1, §3.3, §3.5 |
| `Bootstrap` gains `appVersion`. | §3.1 |
| Sync calls send `X-ReqHarbor-Version: <appVersion>`. Pulled changes of an unknown `type` are skipped without error and kept in `sync-state.json` (`unknownChanges`, capped). | §3.9 |
| `Auth.type`, `ApiKeyAuth.in` and `FormField.kind` accept any string; unknown values round-trip unchanged and make a request read-only. A collection whose auth is unknown keeps it read-only (`update_collection` refuses auth changes); an inheriting request can't be sent (`validation`, `inheritedAuthNeedsNewerVersion`). | §2, §3.3, §3.5 |
| Moves, duplicates and cascading deletes never rewrite, copy or remove a read-only request (`read_only_newer`). | §1, §3.3 |
| Restore detection (`syncEpoch`, cursor going down) and push filtering by the server's `sync.types` (`GET /v1/server`). `SyncStatus.unsupportedEntities`, `SyncReport.unsupported` / `restored`. `SyncStatus.pendingChanges` leaves out `unsupportedEntities`. | §3.9 |
| `Project`, `Collection`, `Folder` and `Environment` honour `minClientVersion` too (AC-FWD-4): when newer than `appVersion` the item can't be renamed, edited, copied, moved or deleted in a cascade (`read_only_newer`); siblings are reordered around it; it can still get children and be deleted on its own. Sync never pushes a locally modified version of one. | §1, §2 |
| Per-change push results: `SkippedEntity.reason` is the server's reason (`invalid_json_value`, `forbidden_role`, … ; `entity_too_large` is reported as `too_large`); `clock_skew` and statuses this build doesn't know leave the change pending; a rejected delete stays pending. | §3.9 |
| Shares without "Include credentials" leave out every field this build doesn't know (each block counts in `credentialsReplaced`), and refuse (`read_only_newer`) a collection or request share that holds an item from a newer ReqHarbor. | §3.10 |

### Accounts: discovery and sign-in (accounts plan M1-client C1)

| Change | Where |
|---|---|
| New commands `server_discover`, `auth_status`, `auth_sign_in_start`, `auth_sign_in_reopen`, `auth_sign_in_cancel`, `auth_sign_out`, `auth_me`, `auth_sessions`, `auth_session_revoke`; new event `auth://status`. | §3.11 |
| `Bootstrap` gains `accounts` (the `accounts` flag). | §3.1 |
| New error codes for discovery and sign-in. | §1 |
| `settings.json` may hold `knownServers` (UI-managed suggestions and per-origin choices, never secret). New files `account.json` and `secrets.json` (0600 fallback store). | §3.11, §4 |
| Sync › Advanced: the field is labelled "Server URL"; a bad URL now reads "Server URL must start with https:// or http://". | §3.9 |

### Accounts: personal sync and bindings (accounts plan M1-client C2)

| Change | Where |
|---|---|
| New commands `project_locations`, `project_set_location`, `first_sync_candidates`, `first_sync_apply`, `space_notice_resolve`, `space_notice_later`, `legacy_merge`, `legacy_disconnect`; event `sync://notice`. `create_project` gains `location?`; `share_revoke` gains `spaceId?`. | §3.12 |
| `SyncStatus` gains `accountMode`, `spaces: SpaceStatus[]`, `legacy: LegacyStatus \| null`, `firstSyncPending`; `SyncStateName` gains `signInRequired` and `paused`. `ShareInfo` gains `spaceId?`. | §3.9, §3.12 |
| New error codes `read_only_role`, `space_unavailable`, `forbidden_role`; sync errors `sign_in_required`, `client_too_old`, `server_full` (retried), `rate_limited` (retried). A legacy 403 `legacy_disabled` reads "This server no longer accepts token sync. Sign in instead." | §1, §3.12 |
| New file `sync-spaces.json` (+ backup); `account.json` holds the bindings. Signing in removes `sync` from `workspace.json`. | §3.12, §4 |

### Accounts: teams (accounts plan M2-client)

| Change | Where |
|---|---|
| New commands `teams_list`, `team_create`, `team_get`, `team_rename`, `team_delete`, `team_set_role`, `team_remove_member`, `team_leave`, `team_transfer`, `invite_create`, `invite_list`, `invite_revoke`, `invite_preview`, `invite_accept`, `invite_ticket`, `move_preview`, `move_project_to_space`, `move_cancel`, `copy_preview`, `copy_to_project`, `project_duplicate_to`, `request_edited_by`, `role_change_resolve`; event `sync://move-progress`. `auth_sign_in_start` gains `inviteTicket?`; `space_notice_resolve` gains `saveChanges?`. | §3.14 |
| `SpaceStatus` gains `teamId`, `memberCount`, `frozen`, `held`; `SpaceIssue.code` gains `roleChanged` and `heldDelete`. `SyncStatus` gains `pendingMoves`. `ShareInfo` gains `createdBy?` and `options?`. | §3.12, §3.14 |
| New error codes `teams_disabled`, `last_owner`, `transfer_required`, `limit_teams`, `limit_members`, `limit_invites`, `invite_invalid`, `invite_not_a_link`, `invite_other_server`, `not_a_member`, `team_not_found`, `member_not_found`, `invite_not_found`, `validation_failed`, `quota_exceeded`, `clock_skew`. `read_only_role` now also covers deleting a team project below admin and adding projects to a viewer's team. | §1, §3.14 |

### WebSocket and Socket.IO (next features §3)

| Change | Where |
|---|---|
| `kind: "ws"` is editable and runnable (AC-WS-1); `Request` gains `ws?: WsConfig` (`messages: SavedMessage[]`, `protocols`, `socketIo?`, `autoReconnect`). Unknown keys inside `ws` round-trip; an unknown `SavedMessage.kind` or a `socketIo.version` other than 4 makes the request read-only. `create_request` / `save_request` stamp `minClientVersion` on `ws` requests with the fixed `WS_MIN_CLIENT_VERSION` "0.1.0" (the first version with WebSocket support, never the running version; never lowering a newer stamp); `save_request` keeps the stored `ws` when the update lacks it. | §2 |
| New commands `ws_connect` (with an `ipc::Channel` argument `onEvent` streaming `WsFrame`s), `ws_send`, `ws_ping`, `ws_close`. | §3.13 |
| `HistoryEntry.ws?` / `HistorySummary.ws` (`WsSessionSummary`): one entry per session, method `"WS"` / `"SIO"`. | §3.6 |
| New error codes `ws_not_connected`, `ws_invalid_message`; `validation` details gain `notHttp` (`send_request` / `export_curl` got a `ws` request), `notWebSocket`, `invalidWsUrl`, `invalidSocketIoAuth`, `undefinedSocketIoAuthVariable`. | §1 |
| `Backend.channel(handler)` in `ui/src/lib/api` creates the channel (Tauri `Channel`, or the mock's). | §3.13 |

### GraphQL schemas and operations (PRODUCT §5.14)

| Change | Where |
|---|---|
| `GraphqlBody` gains `operationName?: string` (omitted when unset; blank = unset). Sent as `"operationName"` in the JSON body (`{"query","operationName","variables"}`) and in cURL export; cURL import restores it. `kind: "graphql"` stays reserved and read-only. | §2, §3.5, §3.7 |
| New commands `graphql_fetch_schema`, `graphql_cached_schema`, `graphql_load_schema_file`, `graphql_store_schema`, `graphql_clear_schema` and types `GraphqlSchema`, `GraphqlCacheLookup`. | §3.15 |
| New error codes `graphql_network`, `graphql_introspection_disabled`, `graphql_introspection_failed`, `graphql_schema_invalid`, `graphql_schema_too_large` (`details`: `GraphqlErrorDetails`). | §1 |
| New local directory `graphql-schemas/` in the app data dir (never synced). | §4 |

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
* CSP: `default-src 'self'; connect-src 'self' ipc: http://ipc.localhost; img-src 'self' data: blob: http: https:; style-src 'self' 'unsafe-inline' http: https:; font-src 'self' data: http: https:; script-src 'self'` — remote images/styles/fonts are allowed only so the sandboxed HTML preview can render pages (it gets a `<base href>` of the response URL); no remote scripts or connections. Use `blob:` URLs for image previews.
* Window label `main`, title "ReqHarbor", 1280×800, min 960×600. `dragDropEnabled: true`: files dropped on the window arrive via `getCurrentWebview().onDragDropEvent()` with absolute paths (use them for OpenAPI import and Binary/Form file fields). Because of this, HTML5 drag-and-drop is unreliable on Windows — implement the sidebar tree DnD (AC-COL-3) with pointer events.

### Error codes

| code | when |
|---|---|
| `not_found` | id does not exist (project, collection, folder, request, environment, history entry, cached response) |
| `invalid` | invalid input: empty name, deleting the last project or the Global environment, moving a node across projects or into itself, bad method on save |
| `io` | disk error |
| `validation` | send blocked; `details` = `{kind:"invalidMethod",message}` \| `{kind:"invalidGraphqlVariables",message}` (AC-REQ-5) \| `{kind:"emptyUrl"}` \| `{kind:"inheritedAuthNeedsNewerVersion"}` \| `{kind:"notHttp"}` (a `ws` request given to `send_request` / `export_curl`) \| `{kind:"notWebSocket"}` \| `{kind:"invalidWsUrl",message}` \| `{kind:"invalidSocketIoAuth",message}` \| `{kind:"undefinedSocketIoAuthVariable",message}` (`message` = the variable name; "Define {{socketIoAuth}} (the shared Socket.IO auth payload) in your environment") (`ws_connect`) |
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
| `read_only_newer` | the request was made by a newer ReqHarbor (non-HTTP `kind`, unknown `body.type`, `auth.type`, `auth.apiKey.in` or form field `kind`, or `minClientVersion` above `appVersion`): `save_request`, `rename_request`, `duplicate_request`, `move_node` (request), `create_request` (from such a template), `send_request` and `export_curl` refuse it. Container actions that would rewrite, copy or remove one refuse too: `move_node` of a folder to another collection, a reorder that can't keep its `sortOrder` (siblings are numbered around read-only requests; two adjacent ones leave no room between them), `duplicate_collection` / `duplicate_folder` / `duplicate_project`, and the cascading `delete_collection` / `delete_folder` / `delete_project` (the message says to delete that request on its own first). `update_collection` refuses changing an unknown auth or setting one. `delete_request` works. The same applies to a project, collection, folder or environment whose `minClientVersion` is newer than `appVersion`: `rename_project`, `set_active_environment`, `move_project` (other projects are numbered around it), `update_collection`, `rename_folder`, `update_environment`, `move_node` of it (siblings are numbered around it), duplicates of it or of a container holding it, and cascading deletes holding it refuse; `create_folder` / `create_request` inside it and deleting it on its own work. `share_preview` / `share_create` / `share_update` without `includeCredentials` refuse a collection or request share holding any such item (the message says to turn on Include credentials). Show the message as an info toast (in the Share dialog, inline). |
| `server_unreachable` | discovery or an account call could not reach the server (network, DNS, 5xx) |
| `token_sync_only`, `not_reqharbor`, `client_too_old`, `no_sign_in_methods` | `auth_sign_in_start` refused the server; `message` is the 06 §9 copy |
| `insecure_unconfirmed` | plain http to a non-loopback host without `insecureConfirmed` or a `knownServers[].insecureAck` |
| `loopback_unavailable` | no `127.0.0.1` port could be opened for the browser redirect |
| `sign_in_cancelled`, `sign_in_timeout`, `sign_in_denied`, `sign_in_failed` | the browser sign-in was cancelled here, timed out (10 min), came back with an error, or the code exchange failed (`invalid_grant`) |
| `sign_in_required` | the session can no longer be refreshed (AC-AUTH-9); local data is untouched |
| `not_signed_in` | an account command without an account |
| `keychain_unavailable` | the session could not be written to (or read from) the secret store |
| `rate_limited`, `server_full`, `server_error`, `auth_rejected` | 429, 507, other 5xx / unexpected answer, other refusals (`message` from the server) |
| `teams_disabled` | a team or invite command while the server doesn't advertise `features.teams` (or answers 403 `teams_disabled`) |
| `last_owner`, `transfer_required` | the change would leave the team without an owner ("A team needs at least one owner") / the target is the billing owner ("Transfer ownership first") |
| `limit_teams`, `limit_members`, `limit_invites`, `validation_failed`, `quota_exceeded` | server limits and validation; `message` is the server's (it names the configured limits) |
| `invite_invalid` | an expired, revoked, used-up or pre-removal invite link (one generic message, AC-INV-5) |
| `invite_not_a_link` | the pasted text isn't an `https://<server>/i#<token>` link ("That isn't a ReqHarbor invite link.") |
| `invite_other_server` | `invite_accept` with a link for another server than the signed-in one |
| `not_a_member`, `team_not_found`, `member_not_found`, `invite_not_found` | the team, member or invite is gone (or never was) |
| `ws_not_connected` | `ws_send` on a connection that isn't open (unknown id, still connecting, or closed) |
| `ws_invalid_message` | `ws_send` refused the message: invalid JSON or base64, no or reserved Socket.IO event name, binary in Socket.IO mode (`message` says which) |
| `graphql_network` | `graphql_fetch_schema` couldn't reach the server; `details` = `{ kind: ErrorKind }` (`cancelled` after `cancel_request`: show nothing) |
| `graphql_introspection_disabled` | the server refuses introspection (an `errors[].message` mentioning introspection, `__schema` or `__type`); `message` = "Introspection is disabled on this server", `details` = `{ serverMessage }` — offer Load from file… (AC-GQL-3) |
| `graphql_introspection_failed` | any other answer without `data.__schema`; `message` says why (first GraphQL error, "The server answered 404 Not Found", "The response isn't JSON…"), `details` = `{ status }` |
| `graphql_schema_invalid` | `graphql_load_schema_file`: not UTF-8, invalid JSON, JSON without `__schema`, or no type definitions; `message` = "This file isn't a GraphQL schema: <reason>" |
| `graphql_schema_too_large` | a schema over 10 MB; `details` = `{ sizeBytes }` |
| `internal` | unexpected |

## 2. Data model

Exactly PRODUCT.md §3, serialized as in `types.ts`. Notes that matter to the UI:

* **IDs** are ULIDs created by the backend. The only id the UI creates is a scratch request's `id` and the `executionId` of a send (any unique string).
* **Scratch request**: a `Request` with `collectionId: ""`. Build it client-side with defaults: `method "GET"`, `params/headers []`, `auth {type:"none"}`, `body {type:"none"}`, `settings {followRedirects:true, timeoutMs:30000, verifyTls:true}`, `sortOrder 0`, timestamps `0`. Persist it with `create_request` (Save-to picker, AC-REQ-11).
* **URL ⇄ Query (AC-REQ-2)**: `request.url` holds the full URL including the query string of *enabled* params; `params` holds all rows incl. disabled ones. Keep them in sync on every edit with `url_to_params(url, params)` (URL edited) / `params_to_url(url, params)` (rows edited) — or port `crates/core/src/urlx.rs` (≈60 lines) to TS for zero-latency typing. Query values are raw (not decoded); encoding only touches space, `&`, `#`, `"`, `<`, `>`, control/non-ASCII chars (and `=` in keys), so text round-trips unchanged. At send time a non-empty `params` list is the source of truth for the query string.
* **Sorting**: arrays inside `ProjectTree` are not sorted; order siblings by `sortOrder`. Folders and requests of the same parent share one `sortOrder` sequence; `move_node` reorders folders and requests separately (folders first is fine).
* **Auth Inherit** uses the parent collection's saved `auth` at send/export time (AC-COL-5). An enabled `Authorization` header always wins over the Auth tab (AC-REQ-7) — show the hint.
* **Default Content-Type** (AC-REQ-4) is added by the backend at send time only when no enabled `Content-Type` header exists: json/graphql `application/json`, xml `application/xml`, text `text/plain`, formEncoded `application/x-www-form-urlencoded`, form `multipart/form-data; boundary=…`, binary `application/octet-stream`. The UI may display them as greyed "auto" headers.
* **Variables** (AC-ENV-1..4): `{{ name }}` with `name` in `\$?[A-Za-z0-9_.-]+` (a leading `$` = dynamic variable, below), whitespace trimmed. Resolution: project's active environment → Global; disabled vars skipped; nested refs up to depth 3; cycles stay literal; unresolved stay literal and are reported in `SendResult.unresolvedVariables`. For editor decorations/hover use `get_variables(projectId)` (first definition per key wins, includes source env name; mask secrets).
* **Dynamic variables** (AC-ENV-8..10): `{{ $name }}` — a leading `$` (only as the first character) names a generator from `get_dynamic_variables()` (`DynamicVariableInfo[]`: `name` incl. `$`, `description`, `example`), e.g. `$uuid`, `$timestamp`, `$isoTimestamp`, `$randomInt`, `$randomEmail`, `$randomFullName`, `$randomIP`, `$randomDatePast`. They are never looked up in an environment, and `update_environment` rejects a variable whose key starts with `$` (`invalid`). One value per name per send: every field of one `send_request` (URL, params, headers, auth, body, and `resolvedUrl` / history) shares the generated values; values may also come in through environment values (`token = "{{$uuid}}"`). Known names are never in `unresolvedVariables`; an unknown `{{$name}}` stays literal and is reported there. Highlight them with the `--var-dynamic-*` tokens and show `"<description> · e.g. <example> · new value each send"` on hover. The UI ships the same catalogue (`ui/src/lib/util/dynamic.ts`) so it renders before the command answers.
* **Timestamps**: `updatedAt` is set by the backend on each mutation (strictly increasing per entity). Do not edit it.
* **Timeout**: `settings.timeoutMs: 0` disables the timeout (imported from `curl -m 0`).
* **Unknown keys (AC-FWD-1)**: objects may carry keys that are not in `types.ts`, written by a newer ReqHarbor (on another device, via sync or a share). The backend preserves them everywhere; the UI must not drop them — copy objects with spread / `structuredClone` instead of rebuilding them. As a safety net `save_request` re-adds keys the stored request has and the update lacks, at every nesting level (list rows are matched by `key`).
* **Newer containers (AC-FWD-4)**: a `Project`, `Collection`, `Folder` or `Environment` may carry `minClientVersion`; newer than `Bootstrap.appVersion` (or not a string) = read-only here, see `read_only_newer` in §1. The tree shows the "Requires a newer ReqHarbor" badge on such a collection or folder, offers no Rename / Duplicate / Move / Settings for it, and never starts a rename or drag of it.
* **GraphQL (AC-GQL-1, AC-GQL-9)**: GraphQL is a body type of an HTTP request, never a `kind`. `body.graphql.operationName` is kept only while the query defines two or more named operations (the UI's Operation picker writes it, first named operation by default) and is omitted otherwise; core sends it only when set and not blank. Before `send_request` / `export_curl` the UI normalises the request with `withOperation` (`ui/src/lib/util/graphql.ts`): no or a stale name with ≥ 2 named operations → the first named one; fewer than two → none. Older builds keep it as an unknown key.
* **Request kind (AC-FWD-2..4, AC-WS-1)**: `kind` is absent (= `"http"`) for HTTP requests and `"ws"` for WebSocket / Socket.IO requests (§3.13). A request is **read-only** when `kind` is anything but `"http"` or `"ws"`, when a saved WebSocket message's `kind` is not `text`/`json`/`binary` or `ws.socketIo.version` is not 4, when `body.type` is not one of the eight known types, when `auth.type` is not one of the five known schemes or `auth.apiKey.in` is not `header`/`query`, when a form field's `kind` is not `text`/`file`, or when `minClientVersion` is newer than `Bootstrap.appVersion` (an unparseable version counts as newer). The stored copy decides: the UI treats an open request as read-only when either the editor copy or the saved one is. When a sync pull turns the open request read-only, the editor switches to the saved, view-only copy; unsaved edits (of it or of a stashed draft) are offered once as "Save my edits as a copy" (a new, editable request via `create_request`). Read-only requests: the tree shows a "Requires a newer ReqHarbor" badge; the pane is view-only, Save and Send are disabled ("This request needs a newer version of ReqHarbor"); the only tree actions are Open and Delete. The backend refuses edits and sends with `read_only_newer` and sync never pushes a locally modified version of one (a remote version always replaces it).

## 3. Commands

Mutating commands persist atomically before resolving (AC-COL-6) and, when sync is connected, schedule a push 2 s later (debounced, AC-SYNC-2). Debounce editor autosave in the UI (≤ 500 ms).

### 3.1 Bootstrap & settings

| command | args | result | notes |
|---|---|---|---|
| `bootstrap` | – | `Bootstrap` | First call on startup. First launch already contains "My Workspace" / project "Default" / Global env (AC-COL-1). Show a persistent error toast for each `corruptFiles[]` entry (AC-COL-6). `platform` drives ⌘ vs Ctrl. `appVersion` is compared with `Request.minClientVersion`. `accounts` is the accounts flag: show Settings › Account only when true (on in debug builds; `REQHARBOR_ACCOUNTS=1/0` overrides). |
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
| `save_request` | `request: Request` | `Request` — ⌘S; placement fields (`collectionId`, `folderId`, `sortOrder`) are ignored (use `move_node`); `invalid` if the method is invalid; `read_only_newer` if the stored or the given request is read-only (§2); unknown keys of the stored version that `request` lacks are kept |
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
| `get_dynamic_variables` | — | `DynamicVariableInfo[]` — the `{{$name}}` catalogue, display order (AC-ENV-8) |

### 3.5 Sending & responses

| command | args | result |
|---|---|---|
| `send_request` | `executionId, request, projectId?` | `SendResult` |
| `cancel_request` | `executionId` | `boolean` |
| `get_response_body` | `responseId, limit?` | `ArrayBuffer` (raw bytes over IPC) |
| `save_response_body` | `responseId, path` | `null` |
| `format_body` | `kind: "json"\|"xml", text` | `string` (JSON: 2-space, numbers/escapes preserved). GraphQL queries are prettified in the UI (graphql-js `print`, AC-GQL-7); variables use `kind: "json"`. |
| `url_to_params` | `url, params` | `KeyValue[]` |
| `params_to_url` | `url, params` | `string` |

Send flow:

1. Generate `executionId`, switch Send → Cancel, call `send_request` with the request **as edited** (it is never saved).
2. ⌘Enter / Cancel → `cancel_request(executionId)`; the pending `send_request` resolves within 100 ms with `error.kind = "cancelled"` (AC-REQ-8).
3. Result: exactly one of `response` / `error` is set. `error.kind` ∈ `dns|connect|tls|timeout|cancelled|other` drives the error panel (AC-REQ-9) — never treat it as an exception. A rejection with code `validation` means nothing was sent (invalid custom method, invalid GraphQL variables → "Fix GraphQL variables", empty URL). A rejection with `read_only_newer` ("This request needs a newer version of ReqHarbor") means the request (or the stored request with the same id) is read-only (§2); nothing was sent.
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

A WebSocket / Socket.IO session writes one entry when it ends (AC-WS-7): `method` `"WS"` / `"SIO"`, no `response`, `ws: WsSessionSummary` (`durationMs`, `sent`, `received`, `closeCode?`, `closeReason?`) and `error` when the connect failed or the connection was lost; `HistorySummary.ws` carries the same summary (`null` for HTTP sends; `status` / `timeMs` are `null` for sessions). `open_history_entry` returns `response: null` for them.

Filtering (AC-HIST-3) is done in the UI over the summaries: case-insensitive substring match on `resolvedUrl`, `method`, or `String(status)`. "Save to collection" = `create_request(collectionId, folderId, entry.request)`. Secret variable values never appear in history (`resolvedUrl` shows `{{name}}`).

### 3.7 cURL

| command | args | result |
|---|---|---|
| `import_curl` | `text` | `CurlImport` — rejects `not_curl` / `curl_missing_url` / `curl_syntax` |
| `looks_like_curl` | `text` | `boolean` — URL-bar paste detection (`text.trimStart().startsWith("curl ")` is equivalent) |
| `export_curl` | `request, projectId?, keepVariables` | `string` — multi-line bash, one part per line (AC-CEXP-2..5). `keepVariables:false` resolves all variables incl. secrets against the active env, and generates each `{{$name}}` once for the whole command (URL, `-u`, form fields agree); `true` exports raw `{{vars}}` and `{{$name}}` (adds `--globoff`). Copy with the clipboard plugin. |

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
| `sync_create_workspace` | `workerUrl, name?` (labelled "Server URL"; plain http off loopback needs "Continue anyway" in the UI) | `CreatedWorkspace` — creates the remote workspace, connects, runs the first sync. Show `token` once with Copy (AC-SYNC-1). |
| `sync_connect` | `workerUrl, token` | `SyncStatus` — validates the token (`sync_unauthorized` if wrong), connects, runs the first sync (a fresh device adopts the remote tree; its empty default project is dropped) |
| `sync_disconnect` | – | `SyncStatus` (local data untouched) |
| `sync_rotate_token` | – | `{ token }` (old token invalid immediately; show once) |
| `sync_now` | – | `SyncReport` — "Sync now" (discovery when due, pull, then push); toast `unsupported` with the notice above |
| `sync_workspace_info` | – | `RemoteWorkspace` — name, entity count, `bytesUsed` / `quotaBytes` for a storage meter (null if the Worker doesn't report them) |

Automatic behaviour (no UI work needed beyond listening): pull on startup and on window focus (max once per 60 s); push 2 s after any mutation; on offline/5xx a single retry timer with backoff 5 s → 10 s → … → 5 min (`nextRetryAt` for "Retrying in 30 s"); on 401 sync is disconnected and status becomes `unauthorized` → show "Token rejected — sync disconnected. Enter a new token." No timers run when idle.

Events (subscribe with `listen`):

| event | payload | UI action |
|---|---|---|
| `sync://status` | `SyncStatus` | sidebar sync indicator (§5.14 of DESIGN): hidden when `!connected` (unless `unauthorized`), `syncing` → dimmed icon, `offline` → warning, `unauthorized` → danger "Sync off" |
| `sync://notice` | `SyncNotice` `{ kind, message }` | toast `message` (the fresh-device sign-in, UX §10) and reload |
| `sync://changed` | `SyncReport` | reload `list_projects`, the open `get_project_tree`, `list_environments`; for each `conflicts[]` toast "Kept your edit as “{name}”" (AC-SYNC-4) |

Synced: projects, collections, folders, requests, environments (secret values blanked; local secret values are kept on pull). Never synced: history, settings (AC-SYNC-3, AC-ENV-7).

Forward compatibility: every sync call carries `X-ReqHarbor-Version: <appVersion>`. A pulled change whose `type` this build does not know is skipped without error (the rest of the page applies and the cursor advances) and its raw JSON is kept in `sync-state.json` → `unknownChanges` (latest per `type`+`id`, at most 5,000 entries / 8 MB; the oldest are dropped first and `unknownChangesTruncated` is set) so a newer build can apply it (AC-FWD-5). Tombstones of an unknown type in `sync-state.json` are kept and never pushed. Unknown keys inside known entities are kept and pushed back unchanged. A read-only request (§2) that is already on the server is never pushed once modified locally — only its deletion; the next remote version replaces it without a conflict copy, and no conflict copy is ever made of one.

Server restore (06-go-server §9): pull pages and `GET /v1/server` may carry `syncEpoch`, stored in `sync-state.json` (`syncEpoch`). On every pull page, before the cursor moves, a `syncEpoch` that differs from the stored one, or a page `cursor` below the `since` that was sent, means the server was restored from a backup: the client sets the cursor to 0, re-pulls, and re-sends every entity (entities unchanged since their last sync are re-sent with their original `deviceId`, so LWW treats equal versions as the same; an older restored version never overwrites a newer local one, and no conflict copies are made for it). A first pull only stores the epoch; the Worker sends none and is only checked by the cursor rule. `SyncReport.restored` is then true and `sync://changed` fires; show "The sync server was restored from a backup. Your data was sent to it again."

Sync types: before syncing, the client asks `GET /v1/server` (no token; again after 15 min, or on every run while items are held back). It pushes only changes whose `type` is in `sync.types`; a 404 (the Worker, an older server) or no `sync.types` means the five base types. A dirty entity of an unlisted type stays dirty and local-only and is listed in `SyncStatus.unsupportedEntities` / `SyncReport.unsupported` (`reason: "unsupported_type"`); `SyncStatus.warning` carries "3 items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server." A per-change push result `rejected` with `reason: "unsupported_type"` is handled the same way (not marked too large) and makes the next run ask `GET /v1/server` again.

`SyncStatus.pendingChanges` counts only what a push can clear: items in `unsupportedEntities` are left out (their notice covers them).

Per-change push results: `applied` makes the entity clean; `stale` leaves it to the next pull. `rejected` is handled by its `reason`: `unsupported_type` as above; `clock_skew` leaves the change dirty (M1 restamps it); any other reason (`invalid_json_value`, `forbidden_role`, `entity_too_large`, or one this build doesn't know) marks the entity skipped until it is edited again and lists it in `SyncStatus.skippedEntities` / `SyncReport.tooLarge` with that reason (`entity_too_large` → `too_large`; no reason → `rejected`). A rejected **delete** keeps its tombstone and is sent again on the next run (the server still has the entity). A `status` this build doesn't know is treated like `stale`, and a result it can't parse is ignored: the change stays pending. `SyncStatus.warning` words each reason ("2 items aren't synced because the server can't store one of their values: …"); the tree badge uses `skippedReasonText(reason)`.

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

Sanitising (AC-SHARE-2): secret variables are always removed from the attached environment; unless `includeCredentials`, `Authorization` / `Proxy-Authorization` / `Cookie` header values become `{{authorization}}` / `{{proxyAuthorization}}` / `{{cookie}}` and auth values become `{{username}}`, `{{password}}`, `{{token}}`, `{{apiKey}}`. Also unless `includeCredentials`, every field this build doesn't know (unknown keys at any level of the collection, its folders and requests, their auth, headers, params, form rows and the attached environment's variables) is left out, because a newer ReqHarbor may keep secrets there (e.g. gRPC metadata); each such block counts once in `credentialFields` / `credentialsReplaced`. A collection or request share holding an item from a newer ReqHarbor (§2: a read-only request, or a container with a newer `minClientVersion`) is refused with `read_only_newer` unless `includeCredentials`; the Share dialog shows the message inline and disables "Create link" until the preview succeeds. A request share inlines its collection's auth when it uses Inherit. The UI must remember `shareId` per entity for "Update share" — use `share_list` (`entityId`) to find it.

### 3.11 Accounts (Settings → Account)

There is no default server: the user types an address. Signing in turns on personal sync (§3.12); token sync under Sync › Advanced keeps working at the same time, even with another server.

| command | args | result |
|---|---|---|
| `server_discover` | `serverUrl` | `ServerDiscovery`. The address is normalised (trim, `https://` added when missing, scheme and host lowercased, default port and trailing `/` dropped); a path, query or fragment rejects with `invalid` "Enter the server's address without a path". Then `GET {origin}/v1/server` (10 s). `status`: network error or 5xx → `unreachable`; 404 `not_found` (the Worker) → `token_sync_only`; not `software: "reqharbor-server"` / unparsable / `apiVersion` < 1 → `not_reqharbor`; `minClientVersion` above `appVersion` → `client_too_old`; `features.accounts: false` → `token_sync_only` (message "no sync enabled yet" when `legacyWorkspaces` is `off`); a higher `apiVersion` is compatible. `setupRequired` keeps `canSignIn` with the setup notice; no methods → `canSignIn: false` with the admin hint. `canonicalUrl` is the normalised `publicUrl` when it differs (offer "Use that address?"; "Keep what I typed" is remembered as `knownServers[].keepTyped`). `insecure` = plain http to a host other than `localhost`, 127/8 or `::1`. For the signed-in account's server an unreachable answer falls back to the cached one (≤ 24 h, `cached: true`). |
| `auth_status` | – | `AuthStatus` |
| `auth_sign_in_start` | `serverUrl, method?, insecureConfirmed?` | `AuthStatus` with `signIn.state = "waiting"`, once the system browser was asked to open the link. If it couldn't be opened, `signIn.message` is "Couldn't open your browser. Copy the sign-in link and open it in your browser." and `signIn.authorizeUrl` holds the link for **Copy link** (both `null` otherwise; a later successful "Open browser again" clears them). Rediscovers first (errors above), refuses an insecure origin unless `insecureConfirmed` or `knownServers[].insecureAck` (`insecure_unconfirmed`), binds `127.0.0.1:0` (`loopback_unavailable`) and opens `{origin}/auth/authorize?client_id=reqharbor-desktop&redirect_uri=http://127.0.0.1:P/cb&code_challenge=…&code_challenge_method=S256&state=…&device_name=…[&method=…]`. The rest arrives on `auth://status`: `exchanging` when the browser comes back, then `success` (account set) or `error` (`signIn.error` holds the code). Starting again cancels a pending sign-in, except while `exchanging` (`invalid` "Sign-in is finishing. Try again in a moment."). |
| `auth_sign_in_reopen` | – | `null` — "Open browser again" with the same URL (`not_found` when nothing is pending) |
| `auth_sign_in_cancel` | – | `AuthStatus` (idle). Closes the listener and drops the verifier. While `exchanging` it does nothing (the sign-in finishes as one unit); a sign-out during the exchange undoes the new sign-in on disk too (revoked, keychain entry and `account.json` entry removed). |
| `auth_sign_out` | – | `AuthStatus` — revokes the session on the server (best effort, ≤ 5 s), deletes the keychain entry and the account from `account.json`. Projects stay. |
| `auth_me` | – | `AuthStatus` — refreshes name, email and method from `GET /v1/me`; offline keeps the cached profile and sets `offline`. |
| `auth_sessions` | – | `SessionInfo[]` (Devices, AC-AUTH-10) |
| `auth_session_revoke` | `sessionId` | `null` — another device's session (this device: `invalid`, use `auth_sign_out`) |

Event `auth://status` (`AuthStatus`): every sign-in step, sign-out, `signInRequired` changes. On a fresh `success` the UI toasts "Signed in as {name}" and adds the server to `knownServers`.

Loopback listener (06 §9, QA 9b): at most 8 connections are handled at once (more wait in the backlog), each with a 5 s read timeout; anything but `GET /cb` gets 404, a `/cb` with a wrong `state` gets 400, and the listener keeps going until the matching `state` arrives (302 to `{origin}/auth/done`, the main window is raised), `?error=` comes back (`sign_in_denied`), 10 minutes pass (`sign_in_timeout`) or the sign-in is cancelled.

Tokens (06 §5.8): `POST /v1/auth/token` with a form body and RFC 6749 names (`grant_type=authorization_code`, `client_id=reqharbor-desktop`, `code`, `code_verifier`, `redirect_uri`; or `grant_type=refresh_token`, `client_id`, `refresh_token`), reading `access_token`, `refresh_token`, `expires_in`, `session_id` and `user`. Errors keep the ReqHarbor envelope `{"error":{"code":"invalid_grant",…}}` (the one deliberate departure from RFC 6749 §5.2). Every call sends `X-ReqHarbor-Version`. The access token (1 h) is refreshed 60 s before it expires; concurrent callers share one refresh; a 401 refreshes once and retries once; a refresh refused by the server (`invalid_grant`, 401 `session_revoked`) sets `signInRequired` and never touches local data. New tokens are written to the secret store **before** they are used (on a failed write the old pair stays in memory and the server's 60 s grace window returns the same new pair next time). At sign-in the order is: secret store, then `GET /v1/me`, then `account.json`.

Secrets (AC-AUTH-7): one keychain entry per session, service `dev.reqharbor.desktop`, entry `session:<serverOrigin>:<userId>`, value `{accessToken, accessExpiresAt, refreshToken, sessionId}`. macOS builds use the file store unless built with `REQHARBOR_MACOS_KEYCHAIN=1`; `REQHARBOR_SECRET_STORE=file` forces it; the keychain is probed (set/get/delete, ≤ 5 s) lazily, on the first real use: loading a saved account with the accounts flag on, or the first sign-in, never at a plain app start; a failed probe (no Secret Service, locked keychain) falls back to it, and `AuthStatus.keychain` / `keychainNote` say so. A keychain read denied twice gives `keychainUnreadable` (signed-out view with a banner). Tokens never appear in `account.json`, `settings.json`, `workspace.json`, logs, errors or `AuthStatus`.

Per-server identity: `account.json` holds `{schemaVersion: 1, account: {serverUrl, serverInstanceId, serverName, userId, email, name, method, sessionId}, server: ServerDiscovery, signInRequired?}`; unknown keys are kept. The account's server is rediscovered at app start and before each sign-in; a changed `instanceId` for the same origin counts as a different server (`signInRequired`). One signed-in account at a time: signing in as another account replaces it and deletes the old keychain entry.

Device name (`device_name`, AC-AUTH-10): the computer name plus ` (macOS|Windows|Linux)`, at most 64 characters, keeping only Unicode general categories L* and N* plus ` ()._-` — the server's `^[\p{L}\p{N} ()._-]*$`. Combining marks (Mn/Mc/Me, e.g. Devanagari or Thai vowel signs) are dropped; a name with nothing left becomes `ReqHarbor (…)`.

Plain-http acknowledgement: "Continue anyway" is stored per origin as `knownServers[].insecureAck` by both the sign-in flow and Sync › Advanced (token sync), and is pre-checked from there for the same origin.

`secrets.json` (file store) that can't be parsed is renamed to `secrets.corrupt-<ts>.json` before a new one is started, like `account.json`.

Mock backend switches (browser only): `?mockServer=ok|tokenOnly|legacyOnly|futureApi|otherPublicUrl|unreachable|notReqharbor|insecure|setup|tooOld` (also picked by a host containing the word, e.g. `https://tokenonly.example`), `?mockMethods=password,github,oidc`, `?mockAuth=ok|timeout|offline|denied|email_in_use|no_port|no_browser|keychain`, `?mockAuthDelay=<ms>` (0 = wait for `window.__mock.accounts.finishBrowser()`), `?mockAccount=signedIn|signInRequired|keychainUnreadable|file`, `?mockAccounts=off`.

### 3.12 Personal sync and bindings (04 §6–§7, 06-go-server §9)

**Modes.** *Pure-legacy* (no bindings in `account.json`): every project syncs with token sync exactly as in §3.9, byte for byte. *Account mode* (`account.json` has `migratedAt`, set by the first sign-in): `account.json → projectSpaces` is authoritative; a project without an entry is "This device only" and never produces traffic. Each bound space keeps its own cursor and bookkeeping in `sync-spaces.json` (same shape as `sync-state.json`, without the device id; written atomically with a backup).

**Sign-in migration** (once, 04 §7). With token sync: the token moves to the secret store (`legacy:<remoteWorkspaceId>`; written first, so a failure changes nothing), the cursor and bookkeeping move into `sync-spaces.json["legacy"]`, every project and the Global bind to `legacy`, and `sync` is removed from `workspace.json` (a downgraded client stops syncing and can't push account data into the legacy workspace). Without token sync: nothing is bound until the first-sync answer (`firstSyncPending`), except a fresh device's untouched Default, which binds to Personal. Later sign-ins only pause bindings of another server, instance or account (`SpaceStatus.state = "paused"`, `reason = "foreignAccount"`): they never pull or push and are resolved with `space_notice_resolve`; signing back in as the account that bound them resumes them (no notice). A binding belongs to an account only for the same origin, user and `instanceId`; while the account's `instanceId` is unknown (no discovery yet) nothing binds or pushes, and the next discovery fills it in. The Global binds to Personal only after the first-sync answer with at least one project (or on a fresh device); "Keep all on this device" and Keep for another account's space leave it local. A fresh device's sign-in ends with `sync://notice` ("Signed in · syncing 4 projects from your account and 1 team."). A `sync` block that a downgraded build wrote to `workspace.json` in account mode for another workspace stays on disk until its token is in the secret store (`legacy:<remoteWorkspaceId>`), then leaves the file. Signing out keeps the bindings; account spaces show `signedOut` and token sync keeps going.

**Run** (account mode): `GET /v1/me/spaces` (a binding is added for each new space of this account; a bound space is marked `removed` only when this 200 list lacks it); then pull every syncable scope in the order personal → legacy → teams within 30 s; apply under the store lock; push. Space calls use `/v1/spaces/{id}/sync` with the session (a 401 refreshes once and retries once; a dead session gives `signInRequired`, data untouched). 403 `not_a_member` / 404 `space_not_found` make the space `unavailable` (retried, data untouched). Pulled `updatedBy` is kept per space (`attribution`), never in the entity JSON. Push filtering by `sync.types` and restore detection (`syncEpoch`, cursor going down) apply per space as in §3.9.

**Binding rules.** B1: bindings change only by a local action (sign-in migration, first-sync answer, Sync location…, merge, a space notice, or a project new to this device arriving in a space). B2: a pulled project known here but bound elsewhere is ignored. B3: a pulled child applies only when the parent named in its data, and its current local project, are bound to that space (nothing is relocated across spaces). B4: the Global environment syncs only with `globalSpace` (Personal, or legacy before the merge). B5: a pulled tombstone deletes only entities of that space. B7: when any space failed transiently in a run, the others apply upserts only and keep their cursor if the pull held deletes; after 3 such runs deletes apply anyway.

**Clock skew** (04 §6.9, 06 §9): a per-change `clock_skew` keeps the change dirty and sets the space issue ("Your computer's clock is ahead by about 7 minutes. Changes can't sync until it's fixed."). Push responses carry `serverTime`; once the clock is within 60 s of it, entities rejected for skew whose remote version is unchanged are restamped to `max(now, syncedAt + 1)` and pushed ("Your clock was corrected; 3 changes will sync now."); one whose remote version changed meanwhile becomes a conflict copy instead, so a future timestamp never wins.

| command | args | result |
|---|---|---|
| `create_project` | `name, location?` | `ProjectTree`. `location` omitted/null: Personal when signed in (D11), legacy with only token sync, else local; `"local"`; or a space id (`not_found` if unknown). |
| `project_locations` | – | `ProjectLocation[]` in switcher order (pure-legacy: `legacy` when token sync is on, else `local`). |
| `project_set_location` | `projectId, spaceId \| null` | `ProjectLocation[]`. To Personal: binds and stamps every entity newer (so an earlier "stop syncing" tombstone can't hide it). To `null`: tombstones in the old space for what it holds ("Removes it from your other devices. This copy stays."). Moves into or out of a team are `move_project_to_space` (new ids, §3.14); here they are `invalid`. A legacy-bound project can't move to Personal here (`invalid`, B6: only `legacy_merge` does that, stopping token sync in the same step); legacy → `null` is allowed. |
| `first_sync_candidates` | – | `FirstSyncCandidate[]` (unbound projects while `firstSyncPending`) |
| `first_sync_apply` | `projectIds` | `SyncStatus`. The listed projects bind to Personal, the rest stay local; `[]` = "Keep all on this device". Nothing uploads before this, the Global included; it binds with the first checked project. |
| `space_notice_resolve` | `spaceId, keep, saveChanges?` | `SyncStatus`. For a paused or removed space: keep → its projects become local-only (team projects renamed "X (from Team)"); remove → they are dropped from this device. No tombstone either way. A Global held by that space stays local. `saveChanges`: first copies each project with unsynced edits to Personal as "X (my changes)" (new ids; edge case 3). |
| `space_notice_later` | `spaceIds` | `SyncStatus`. × on the notice ("decide later", UX §6.9): the first × is remembered; after 7 days, while signed in, Keep applies by itself. |
| `legacy_merge` | – | `LegacyMergeResult` `{ merged, report, syncError }`: the merge is local and done once it returns; the follow-up sync is reported on its own (`syncError` set, the merged data syncs later). Legacy projects and the Global rebind to Personal with the **same ids** and are re-sent (with their original device ids, so a second device's merge converges without duplicates); legacy switches to `sharesOnly` (no more pulls or pushes; share calls keep working). Idempotent. |
| `legacy_disconnect` | – | `SyncStatus`. Forgets the legacy token; projects still bound to it become local-only. |

Shares in account mode go to the entity's project space (`sync_not_connected` "Sharing needs this project to sync. Change its sync location to Personal first." for a local-only project; "This project's sync is paused, so it can't be shared. Resolve it in Settings › Sync first." when its binding isn't an active one of the signed-in account on the session's server — no request is sent then, so one server's tokens never go to another); `share_list` merges every space, tagging `spaceId`.

The viewer guard (04 §6.8): every project-tree mutator refuses with `read_only_role` for a project in a space where the account is a viewer; `delete_project` also refuses below admin in a team ("Only admins can delete team projects."), and `create_project` into a viewer's team refuses.

Mock switches: `?mockSpace=quota|server_full|skew|client_too_old|unavailable|removed|roleChanged`, `?mockTeam=1`, `?mockRole=viewer|editor|admin|owner`, `?mockTeams=off`, `?mockMove=offline|quota|slow`, `?mockLegacy=1`, `?mockForeign=1` (with `mockAccount=signedIn`). Test hooks: `__mock.teams.removeMe("Acme")`, `__mock.teams.setMyRole("Acme", "viewer")` (then `__mock.emitStatus()`).

### 3.14 Teams, invites, moves and copies (04 §5.3–§5.4, §6.5, §6.8; 06-go-server §10 M2)

Teams exist only when the account's server advertises `features.teams`; every team and invite command (except `invite_preview` and `invite_ticket`, which need no account) rejects with `teams_disabled` otherwise. Server calls use the session (a 401 refreshes once and retries once). Refusals keep the server's code; `last_owner`, `transfer_required`, `invite_invalid`, `forbidden_role` and the not-found codes get the UX copy, the limits keep the server's message. Commands that change which spaces this account has (create, join, leave, delete) sync afterwards so `/v1/me/spaces` binds or releases the team space.

**Role changes in sync** (04 §6.8). In a space where the account is a viewer nothing is pushed and pulled versions always win (no conflict copies). When `/v1/me/spaces` lowers the role below editor, or the server refuses a whole push with 403 `forbidden_role`, the space's unsynced changes are frozen (`SpaceStatus.frozen`, issue `roleChanged` "Your role in Acme changed to Viewer. You have 3 unsynced changes."). A per-change `forbidden_role` (e.g. an editor's project tombstone) freezes that change and keeps the tombstone. A pulled tombstone of a project, collection or folder that still holds unsynced edits newer than the delete is held (`SpaceStatus.held`, issue `heldDelete`): the container stays, nothing under it is pushed (no orphans), and its edits are frozen. Frozen keys are never pushed and their pulled versions are skipped until `role_change_resolve`. A per-change `global_env_not_allowed` is dropped silently. Team pushes blank `body.binaryPath` and form file values; pulls keep this device's.

**Moves** (04 §6.5). Every move into, out of or between teams uses new ids. `move_project_to_space` persists a plan (old → new ids) in `sync-spaces.json` (`pendingMoves`), pushes the copy to the target (progress on `sync://move-progress`), and only when every entity is accepted inserts the copy bound to the target (marked synced there), deletes the original (tombstones in its space) and drops the plan; `settings.activeProjectId` follows the copy. On any failure the local project is unchanged ("Nothing was moved"); calling it again resumes with the same ids (the server converges), `move_cancel` pushes tombstones for what already landed and forgets the plan. Unfinished moves are listed in `SyncStatus.pendingMoves` (`PendingMove { moveId, projectId, projectName, to, targetName, done, total, copySaved }`). While a plan is pending, pulled copies of its new ids are skipped, and finishing rewinds the target's cursor to where it was when the plan was made, so a teammate's edit or delete of the partial copy applies afterwards; the local step saves the copy first and removes the source second (idempotent after a crash; a copy already saved is kept as it is, never rebuilt from the source). With `copySaved`, the move can only be finished: `move_cancel` and a move to another target are refused ("Finish the move to Acme first"); a resume removes from the target what was deleted here meanwhile; moving to another target first cancels the older plan. Shares of the original stay in its space. Only admins and owners move a project out of a team; viewers can't move one in; a token-synced project must be merged first.

| command | args | result |
|---|---|---|
| `teams_list` | – | `TeamsOverview { teams, ownedCount, limits {ownedTeams, teamMembers, teamInvites}, owner }` (personal teams never appear; a `null` limit = no cap). Create team is disabled when `ownedCount >= limits.ownedTeams`. |
| `team_create` | `name` | `TeamSummary`; 1–80 characters (`invalid`), 409 `limit_teams` |
| `team_get` | `teamId` | `TeamDetail { team, members, invites (admins and owners; else null), me }` |
| `team_rename` | `teamId, name` | `null` (admin) |
| `team_delete` | `teamId` | `SyncStatus` (billing owner). This device keeps local copies "X (from Acme)". |
| `team_set_role` | `teamId, userId, role` | `null`; `forbidden_role`, `last_owner`, `transfer_required` |
| `team_remove_member` | `teamId, userId, revokeInvites` | `null`; the member's own links are always revoked, `revokeInvites` revokes the rest |
| `team_leave` | `teamId, keepCopy` | `SyncStatus`; `keepCopy` keeps its projects as local copies; `transfer_required` (billing owner), `last_owner` |
| `team_transfer` | `teamId, userId` | `null`; I become admin; `quota_exceeded`, `limit_teams` with the server's message |
| `invite_create` | `teamId, role, maxUses \| null, expiresInDays` | `CreatedInvite { invite, url }` (the link, shown once); admin links need an owner and `maxUses: 1`; `expiresInDays` 1, 7 or 30 |
| `invite_list` | `teamId` | `InviteInfo[]` (active links; admin) |
| `invite_revoke` | `teamId, inviteId` | `null` |
| `invite_preview` | `url, insecureConfirmed?` | `InviteCheck { serverUrl, otherServer, preview }`; `invite_not_a_link`, `invite_invalid`, `insecure_unconfirmed` (plain http to a non-loopback server, until `insecureConfirmed` or a remembered "Continue anyway") |
| `invite_accept` | `url` | `JoinResult { teamId, teamName, role, spaceId, alreadyMember, projects }`; `invite_other_server`, `invite_invalid`, `limit_members` |
| `invite_ticket` | `url, insecureConfirmed?` | the signup ticket for `auth_sign_in_start`'s `inviteTicket`; the same plain-http rule |
| `move_preview` | `projectId, spaceId \| null` | `TransferPreview` (secret variables, file paths, plain-text `credentials[]` from headers (disabled ones too), request and collection auth (`tab: "collection"`), Socket.IO auth, and fields from a newer ReqHarbor (`tab: "request"`), member count) |
| `move_project_to_space` | `projectId, spaceId \| null` | `MoveResult { moveId, projectId (the copy), items }` |
| `move_cancel` | `projectId` | `null`. Removes from the target what already landed and forgets the plan; a plan whose copy was already saved here is finished instead. |
| `copy_preview` | `kind, id, targetProjectId` | `TransferPreview` with `unresolvedVariables` |
| `copy_to_project` | `kind, id, targetProjectId, targetCollectionId?, targetFolderId?` | `CopyResult { tree, copyId }`: new ids; the source is untouched (viewers may copy out); without a target collection a request or folder lands in a new collection named after its own |
| `project_duplicate_to` | `projectId, spaceId \| null, name?` | `ProjectTree`: the whole project with new ids (Duplicate to personal…) |
| `request_edited_by` | `requestId` | `EditedBy \| null` ("Edited by Sam · 3h"; null in personal and local projects and after my own edit) |
| `role_change_resolve` | `spaceId, action: "savePersonal" \| "discard"` | `RoleChangeResult { status, copies }`: `savePersonal` copies the affected projects to Personal as "X (my changes)"; then both unfreeze, drop refused tombstones, apply held deletes and re-pull the space from 0 |

### 3.13 WebSocket and Socket.IO (AC-WS-*)

The webview's CSP allows no `ws:` connections: sockets live in Rust (`crates/core/src/ws.rs`, tokio-tungstenite over tokio-rustls with the platform verifier like reqwest) and stream to the UI over a Tauri 2 `ipc::Channel` per connection, in order.

| command | args | result |
|---|---|---|
| `ws_connect` | `connectionId, request, projectId?, onEvent: Channel<WsFrame>` | `WsConnectResult { connectionId, historyId, method: "WS"\|"SIO", resolvedUrl, unresolvedVariables }` — resolves as soon as the connection is being opened |
| `ws_send` | `connectionId, message: SavedMessage` (`name` ignored) | `{ ackId: number \| null }` once the frame is on the wire (`ackId` for a Socket.IO emit with `ack: true`) |
| `ws_ping` | `connectionId` | `boolean` (false when not connected) |
| `ws_close` | `connectionId, code?, reason?` | `boolean` (false when nothing was open). `code` 1000 (default) or 3000–4999, `reason` ≤ 123 bytes, else `invalid`. While connecting it stops the attempt. |

```ts
const onEvent = await channel<WsFrame>((f) => log.push(f)); // ui/src/lib/api: Backend.channel → new Channel()
const { resolvedUrl } = await call("ws_connect", { connectionId: ulid(), request, projectId, onEvent });
await call("ws_send", { connectionId, message: { name: "", kind: "json", body: '{"op":"hi"}' } });
await call("ws_close", { connectionId, code: 1000 });
```

`ws_connect` rejects (nothing opened) with `read_only_newer`, or `validation` (`notWebSocket`, `emptyUrl`, `invalidWsUrl`, `invalidSocketIoAuth`, `undefinedSocketIoAuthVariable` when the auth payload uses an undefined variable such as a share's `{{socketIoAuth}}`, `inheritedAuthNeedsNewerVersion`). Variables resolve like `send_request` with one `{{$…}}` memo for the whole handshake (URL, params, headers, auth, subprotocols, Socket.IO auth); `ws_send` resolves the message with the project's current environment and fresh `{{$…}}` values. Re-using a `connectionId` closes the previous connection.

**Frames** (`WsFrame`, camelCase): `{ dir: "in"|"out"|"system", ts, kind, data, size, event?, ackId?, code?, truncated?, last? }`.

| `kind` | dir | `data` |
|---|---|---|
| `text` | in / out | the text |
| `binary` | in / out | base64 |
| `ping`, `pong` | in / out | base64 payload (WebSocket); the Engine.IO probe text in Socket.IO mode |
| `close` | in / out | close reason, `code` set |
| `event` | in / out | JSON array of the arguments; `event` = name, `ackId` when an ack was asked for |
| `ack` | in | JSON array of the ack's arguments, `ackId` |
| `open` | system | "Connected to … (101 Switching Protocols) · subprotocol x" / "… · Socket.IO namespace /x · sid …" |
| `error` | system | a connect failure, a dropped connection, a skipped binary attachment, an invalid packet |
| `reconnecting` | system | "Reconnecting in 2 s (attempt 2 of 5)" (`autoReconnect`) |
| `info` | system | other notes (a server close before a reconnect) |
| `close` | system | **always last**, `last: true`, `code` when the socket closed with one: "Disconnected · 1000", "Connection failed", "Connection lost", "Connection cancelled". The history entry is written right after it. |

`data` is cut at 256 KB (`truncated: true`); `size` is the payload size in bytes (Socket.IO: the packet). Messages up to 64 MB are accepted. The UI log keeps the newest 10 000 frames and about 32 MB of payload text (oldest dropped first); its text filter searches the event name and the first 8 KB of each payload. The connection state follows from the frames: `open` → open; an `out` `close` → closing; `reconnecting` → connecting; `last` → closed.

Behaviour: plain WebSocket answers server pings automatically (shown as an `in` ping). Socket.IO (Engine.IO v4): handshake `?EIO=4&transport=websocket` on the configured path; OPEN `0{…}`, CONNECT `40/ns,{auth}` → `40…` (or `44…` = refused, the connect fails with the server's `message`), EVENT `42/ns,id[…]`, ACK `43/ns,id[…]`, PING `2` → PONG `3` (both logged), DISCONNECT `41/ns,` on close; no ping within `pingInterval + pingTimeout` drops the connection (`error`, `timeout`). Packets of another namespace are ignored; BINARY_EVENT / BINARY_ACK (5/6) and binary frames are skipped with one `error` note. An Engine.IO CLOSE (`1`) is answered with a WebSocket close 1000. **Known limitation:** a server event that asks for an acknowledgement (`42…id[…]`, shown with its `ackId`) is never acknowledged; the server's callback won't fire. `autoReconnect`: after an `error`-drop or a server close other than 1000 (Socket.IO: not after the server's namespace DISCONNECT), up to 5 attempts 1, 2, 4, 8, 16 s apart; the count starts over only after a connection stayed up 10 s, so a server that accepts and drops at once gets 5 in all. `ws_close` during the wait ends the session. On app exit every session closes with 1001 "ReqHarbor closed"; when the webview reloads (or its channel can't deliver a frame) its sessions are cancelled the same way. `ws_close` sent while `ws_connect` is still pending is held by the UI and sent once it resolves. Deleting a request (or its folder / collection) closes its connection.

Mock (browser mode): `ui/src/lib/api/mock/ws.ts` echoes text and binary (and greets with `{"type":"welcome",…}`), `close:<code>` makes it close with that code; Socket.IO echoes each event and acknowledges when asked. Host `refused.test` or port 1 → connect error, `*.invalid` → DNS error, a path containing `/forbidden` → 403.

### 3.15 GraphQL schemas (AC-GQL-2..5)

Every command takes the request **as edited** plus `projectId?`, resolved exactly like `send_request` (project env → Global, `{{$…}}`, collection auth for Inherit) and refused like it (`validation`: `emptyUrl`, `invalidMethod`, `notHttp`, `inheritedAuthNeedsNewerVersion`; `read_only_newer`). The **schema URL** (the cache key) is the URL the introspection request would go to: resolved, params and API-key query applied, scheme defaulted, fragment dropped, `{{$dynamic}}` variables left literal, and every secret value masked as `{{name}}` by the history redactor (`Redactor::from_scope`, also when a plain variable pulls a secret in).

The UI never caches what it can't use: it fetches / loads with `store: false`, builds the schema with graphql-js, and only then calls `graphql_store_schema`. A failed build keeps the schema already shown and cached.

| command | args | result |
|---|---|---|
| `graphql_fetch_schema` | `request, projectId?, executionId?, store? = true` | `GraphqlSchema` — POSTs the standard introspection query (`operationName: "IntrospectionQuery"`, `variables: {}`, `Content-Type: application/json`, a user `Content-Type` header dropped) with the request's URL, params, headers, auth and settings (timeout, redirects, Verify TLS); caches the result unless `store: false`; no history entry. `cancel_request(executionId)` cancels it (`graphql_network`, kind `cancelled`). An answer over 40 MB is refused unparsed (`graphql_schema_too_large`). |
| `graphql_cached_schema` | `request, projectId?, known?: {url, fetchedAt}` | `GraphqlCacheLookup { url, unchanged, schema: GraphqlSchema \| null }` — the cache only, never sends. `unchanged: true` (and `schema: null`) when the cached schema is the `known` one, so the shown schema isn't re-sent over IPC. Call it when a GraphQL body is shown and whenever an input of the schema URL changes: URL, params, auth (and the collection's for Inherit), the active and Global environments' values (debounced). |
| `graphql_load_schema_file` | `request, projectId?, path?, text?, fileName?, store? = true` | `GraphqlSchema` (`source: "file"`) — SDL, or introspection JSON (`{"data":{"__schema":…}}` / `{"__schema":…}`, detected by a leading `{` or a `.json` name); `text` (browser mode) wins over reading `path`. Only regular files are opened (a FIFO or device is `graphql_schema_invalid`), read up to 10 MB + 1 byte. Cached for the request's schema URL unless `store: false`. |
| `graphql_store_schema` | `request, projectId?, schema: GraphqlSchema` | `GraphqlSchema` — caches a schema the UI built successfully under `schema.url` (the redacted URL core computed at fetch / load time). If the request's schema URL is different now (the environment or a host variable changed meanwhile) nothing is cached and the schema is returned as is. Empty text → `graphql_schema_invalid`, over 10 MB → `graphql_schema_too_large`. |
| `graphql_clear_schema` | `request, projectId?` | `boolean` — true when a cached schema was removed |

```ts
type GraphqlSchema = {
  url: string;              // schema URL (cache key)
  fetchedAt: number;        // epoch ms
  source: "introspection" | "file";
  fileName?: string;
  format: "introspection" | "sdl";
  schema: string;           // `{"__schema":…}` JSON text, or SDL text
  sizeBytes: number;
};
```

Reading the answer (core `graphql::read_introspection`): a JSON body with `data.__schema` (or `__schema`) whose `types` is a list is the schema, whatever the status; else an `errors[].message` mentioning introspection / `__schema` / `__type` → `graphql_introspection_disabled`; else the first error message, the status line for non-2xx, or "no schema" → `graphql_introspection_failed`. The UI builds the schema with graphql-js (`buildClientSchema` / `buildSchema`) in a lazy chunk; one that doesn't build is reported ("The server's schema can't be used: …" for a fetch, "This file isn't a GraphQL schema: …" for a file) and never stored.

Cache (core `graphql::SchemaCache`): `<data dir>/graphql-schemas/<sha256(url)[..16 bytes] hex>.json` = `{schemaVersion: 1, ...GraphqlSchema}`; written atomically; one schema ≤ 10 MB; at most 50 MB / 200 files, oldest (mtime) removed first after each write; an unreadable file is dropped on read; temp files of interrupted writes (`.<name>.tmp-*`) older than an hour are removed on the next write. Never synced, shared, exported or backed up by sync.

The UI also reports GraphQL `errors` of a response (AC-GQL-11) from the body it already fetched; no backend support is involved.

Mock (browser mode): any `POST …/graphql` URL is a fake endpoint (`ui/src/lib/api/mock/graphql.ts`, executed with graphql-js: `me`, `user(id)`, `users(role, first)`, `search(term)`, `failing` (always errors), `createUser(input)`, types `User`, `Post`, `Node`, `Role`, `SearchResult`, `CreateUserInput`, `DateTime`); a request error (parse / validation, no `data`) answers 400 with `errors`, field errors 200 with `data` and `errors`; hosts containing `nointrospection` refuse introspection like Apollo Server (400); the header `X-Mock-Schema: broken` makes introspection answer a schema graphql-js can't build (a field of an undefined type); `*.invalid` / `refused.*` hosts fail like sends. `graphql_load_schema_file` with a `path` serves the fake schema (`.json` → introspection, else SDL; a path containing `invalid` → not a schema).

## 4. Storage (for reference)

App data dir (`bootstrap().dataDir`; macOS `~/Library/Application Support/dev.reqharbor.desktop`): `workspace.json`, `projects/<projectId>.json`, `environments.json`, `settings.json`, `sync-state.json` (tombstones + sync bookkeeping, incl. `unknownChanges`), `history.jsonl`, `account.json` (signed in, or bindings kept after sign-out; no tokens), `sync-spaces.json` + `sync-spaces.backup.json` (account mode), `secrets.json` (only when the 0600 file store is used), `graphql-schemas/*.json` (GraphQL schema cache, §3.15; local only, size-capped). Unknown keys in these files (the schema cache excepted) are preserved on rewrite (AC-FWD-1). Atomic writes (temp + fsync + rename); corrupt files are renamed `*.corrupt-<ts>.json` and reported in `bootstrap().corruptFiles`.

## 5. Not provided by the backend (UI responsibility)

Sidebar filter (AC-COL-4), history filter (AC-HIST-3), relative times, method/status colours, CodeMirror search/highlighting, Tests/Pre Run placeholders (AC-REQ-12), keyboard shortcuts (AC-KEY-*), QUERY-without-body hint (AC-QRY-3), Format button wiring (`format_body`), toasts, dirty tracking.

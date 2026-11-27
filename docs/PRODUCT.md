# ReqHarbor — Product Spec (MVP)

Status: approved for MVP build · Owner: Product · Last updated: 2026-11-25

## 1. Vision & positioning

ReqHarbor is a native-feeling desktop HTTP client for developers who want Postman's core power without Postman's weight. It opens instantly, idles at a fraction of the memory, and puts the request, not the platform, at the center: a Thunder Client-style three-pane layout, first-class cURL round-tripping, one-click OpenAPI import, projects/collections/environments, and the new HTTP QUERY method on day one. A tiny Cloudflare Worker gives optional sync and read-only share links, so nobody is forced into an account to send a request. Positioning line: **"Postman's job, a tenth of the weight."**

## 2. Scope

### 2.1 MVP (one build pass)

1. Three-pane UI (sidebar / request / response) with dark and light themes following the OS.
2. Request editor: method (incl. QUERY + custom), URL, Query, Headers, Auth (None, Inherit, Basic, Bearer, API Key), Body (None, JSON, XML, Text, Form, Form-encode, GraphQL, Binary), per-request settings (follow redirects, timeout, verify TLS).
3. Tests and Pre Run tabs exist as placeholders ("Coming soon"), no logic.
4. Send / cancel via `reqharbor-core` (reqwest, one shared client, connection reuse).
5. Response pane: Status, Size, Time; Response (pretty JSON/XML + raw), Headers, Cookies.
6. cURL import (paste dialog + auto-detect on paste into URL bar) and cURL export.
7. OpenAPI import: Swagger 2.0, OpenAPI 3.0 / 3.1 / 3.2, JSON or YAML, file or pasted text.
8. Projects > Collections > Folders (nested) > Requests: create, rename, duplicate, delete, drag-reorder.
9. Environments (per-project + one Global) with `{{variables}}`, secret flag, active-env switcher; Postman-compatible dynamic variables (`{{$uuid}}`, `{{$randomEmail}}`, …) that generate fake data at send time.
10. History (Activity tab): last 500 sends, filterable, click to restore.
11. Local persistence as JSON files in the app data dir; no account required.
12. Cloudflare Worker: workspace token auth, push/pull sync, share links with a read-only web view.
13. Keyboard shortcuts in §6.

### 2.2 Later (not in MVP)

Multiple open request tabs; scripting (pre-request/tests); OAuth 1/2, AWS SigV4, Digest, NTLM; GraphQL schema introspection/autocomplete; WebSocket, SSE, gRPC; collection runner; Postman/Insomnia/Thunder import; HAR import; code generation beyond cURL; OS keychain for tokens; real-time collaboration and multi-user permissions; deep link `reqharbor://` to import shares; proxy and client-certificate settings; Windows `cmd` (`^`) and PowerShell cURL dialects.

## 3. Data model

IDs are ULIDs (string). Timestamps are Unix epoch milliseconds (integer). Every persisted entity carries `id`, `createdAt`, `updatedAt`, and `schemaVersion: 1` at file level.

```
Workspace   { id, name, sync?: { workerUrl, remoteWorkspaceId, token, cursor } }
Project     { id, workspaceId, name, sortOrder, activeEnvironmentId|null }
Collection  { id, projectId, name, description, auth: Auth, sortOrder }
Folder      { id, collectionId, parentFolderId|null, name, sortOrder }
Request     { id, collectionId, folderId|null, name, method, url,
              params: KeyValue[], headers: KeyValue[], auth: Auth, body: Body,
              settings: { followRedirects: bool=true, timeoutMs: int=30000, verifyTls: bool=true },
              sortOrder }
KeyValue    { key, value, enabled: bool, description? }
Auth        { type: "none"|"inherit"|"basic"|"bearer"|"apikey",
              username?, password?, token?, apiKey?: { key, value, in: "header"|"query" } }
Body        { type: "none"|"json"|"xml"|"text"|"form"|"formEncoded"|"graphql"|"binary",
              text?, form?: FormField[], formEncoded?: KeyValue[],
              graphql?: { query, variables }, binaryPath? }
FormField   { key, value, kind: "text"|"file", enabled }
Environment { id, workspaceId, projectId|null (null = Global), name,
              variables: { key, value, enabled, secret: bool }[] }
HistoryEntry{ id, timestamp, sourceRequestId|null,
              request: Request (as edited, unresolved) , resolvedUrl, method,
              response?: { status, statusText, timeMs, sizeBytes,
                           headers: [name, value][], body (≤1 MB, truncated flag), bodyEncoding: "utf8"|"base64" },
              error?: { kind: "dns"|"connect"|"tls"|"timeout"|"cancelled"|"other", message } }
```

Rules: `method` is an uppercase token string (`GET … QUERY` or custom matching `^[A-Z][A-Z0-9_-]*$`). `auth.type="inherit"` uses the parent collection's auth. Deleting a container deletes its children. Sync tombstones are kept locally as `{ id, type, deletedAt }` until pushed.

Storage layout (app data dir, atomic write = temp file + rename):
`workspace.json`, `projects/<projectId>.json` (project + its collections, folders, requests), `environments.json`, `history.jsonl` (append-only, compacted to the newest 500 on startup), `settings.json` (theme, window size, sidebar width, last opened request).

## 4. UI layout (Thunder Client parity)

Left sidebar: top tabs **Activity / Collections / Env**, a filter input, and an active-environment dropdown. Activity lists history rows: colored METHOD badge, URL (resolved, ellipsized), relative time ("2m", "3h", "Yesterday"). Badge colors: GET green, POST amber, PUT blue, PATCH purple, DELETE red, QUERY teal, HEAD/OPTIONS/custom gray.
Middle: method dropdown + URL bar + **Send** button; tabs **Query / Headers / Auth / Body / Tests / Pre Run** (counts shown when non-zero, e.g. "Headers 3"); Body sub-tabs **JSON / XML / Text / Form / Form-encode / GraphQL / Binary** with CodeMirror 6 and a **Format** button.
Right: Status · Size · Time strip; tabs **Response / Headers / Cookies**. Empty state shows the shortcut cheat sheet (Send Request ⌘Enter, Import cURL ⌘U, Change Environment ⌘E, New Request ⌘N).
Panes are resizable; layout persists. All shortcuts use Ctrl instead of ⌘ on Windows/Linux.

## 5. User stories & acceptance criteria

### 5.1 Compose and send (REQ)
*As a developer I want to build and send any HTTP request quickly.*
- AC-REQ-1: Method dropdown lists GET, POST, PUT, PATCH, DELETE, QUERY, HEAD, OPTIONS and "Custom…"; a custom method matching `^[A-Z][A-Z0-9_-]*$` is accepted and sent verbatim; invalid input is rejected inline.
- AC-REQ-2: Editing the Query tab rewrites the URL query string and vice versa, preserving order, duplicates, and disabled rows (disabled rows are not in the URL).
- AC-REQ-3: Disabled headers/params/form fields are not sent. Header names are sent with user casing; duplicates are sent as separate headers.
- AC-REQ-4: Selecting body type JSON/XML/Text/Form-encode/GraphQL sets `Content-Type` to `application/json`, `application/xml`, `text/plain`, `application/x-www-form-urlencoded`, `application/json` respectively unless the user set it explicitly; Form sends `multipart/form-data` with a generated boundary; Binary streams the file at `binaryPath` with `application/octet-stream` unless overridden.
- AC-REQ-5: GraphQL body is sent as `{"query": ..., "variables": ...}`; invalid variables JSON blocks send with an inline error.
- AC-REQ-6: Format pretty-prints JSON (2 spaces) and XML; on invalid content it shows the parse error position and leaves text untouched.
- AC-REQ-7: Auth Basic adds `Authorization: Basic base64(user:pass)`; Bearer adds `Authorization: Bearer <token>`; API Key adds a header or query param; an explicit `Authorization` header in Headers wins over the Auth tab.
- AC-REQ-8: While in flight, Send becomes **Cancel**; cancelling aborts the request within 100 ms and records a history entry with `error.kind="cancelled"`.
- AC-REQ-9: Timeout, DNS, connect, and TLS failures show a typed error in the response pane, never a crash; `verifyTls=false` accepts self-signed certificates.
- AC-REQ-10: A URL without scheme is sent as `http://` for `localhost`/`127.0.0.1`/`[::1]` hosts and `https://` otherwise.
- AC-REQ-11: Unsaved edits to a saved request show a dot on its sidebar item; ⌘S saves; sending never auto-saves. An unsaved "scratch" request can be saved into any collection/folder via a picker.
- AC-REQ-12: Tests and Pre Run tabs render a "Coming soon" placeholder and have no effect on sending.

### 5.2 HTTP QUERY method (QRY)
*As an API developer I want to use QUERY (IETF draft "The HTTP QUERY Method") as a first-class method.*
- AC-QRY-1: QUERY is selectable in the method dropdown and keyboard-accessible; its badge is teal everywhere (editor, sidebar, history, share view).
- AC-QRY-2: QUERY sends the request body exactly like POST, with the selected body type and Content-Type.
- AC-QRY-3: QUERY with body type None sends with no body and shows a non-blocking hint "QUERY requests normally carry a body".
- AC-QRY-4: Redirects for QUERY: 307/308 re-send method and body; 303 follows with GET and no body; 301/302 re-send QUERY with body (no legacy POST→GET rewrite). Max 10 redirects. The core implements this redirect policy itself rather than relying on reqwest defaults.
- AC-QRY-5: cURL export of a QUERY request emits `-X QUERY` plus the body flag; cURL import of `-X QUERY` restores method QUERY with body.
- AC-QRY-6: OpenAPI 3.2 `query` operations import as QUERY requests.
- AC-QRY-7: If the response carries an `Accept-Query` header, the Headers tab highlights it.

### 5.3 Response viewing (RES)
- AC-RES-1: Status strip shows status code + reason (colored 2xx green, 3xx blue, 4xx amber, 5xx red), size of the decoded body in B/KB/MB, and total time in ms.
- AC-RES-2: JSON and XML bodies (by Content-Type or sniffing) render pretty-printed and syntax-highlighted with a Raw toggle; other text renders as plain text; binary bodies show type + size and a **Save to file** action; images (png/jpeg/gif/webp/svg) render as a preview.
- AC-RES-3: Bodies over 5 MB show the first 5 MB as plain text without highlighting, plus **Save full body to file**.
- AC-RES-4: Gzip, deflate, and brotli responses are transparently decoded; Size shows decoded size with the wire size in a tooltip.
- AC-RES-5: Headers tab lists headers in received order with copy-on-click; Cookies tab parses every `Set-Cookie` into name, value, domain, path, expires, HttpOnly, Secure, SameSite.
- AC-RES-6: ⌘F inside the response body opens CodeMirror search.

### 5.4 cURL import (CURL)
*As a developer I want to paste a cURL command and get an editable request.*
Entry points: ⌘U dialog; pasting text starting with `curl ` into the URL bar triggers import (with Undo). Unknown flags are ignored and listed in a dismissible warning; parse never panics.
- AC-CURL-1: Multi-line commands with trailing `\` line continuations (including `\` followed by CRLF) parse identically to the single-line form.
- AC-CURL-2: Single quotes (literal), double quotes (with `\"`, `\\`, `\$` escapes), and `$'...'` ANSI-C quoting (`\n`, `\t`, `\'`, `\\`) are handled; the shell-escape `'\''` produces a literal `'`.
- AC-CURL-3: URL is taken from the first non-flag argument or `--url`; `-X`/`--request` sets the method (any token, including QUERY).
- AC-CURL-4: `-H`/`--header` adds headers, tolerating no space after the colon; `-H 'X-Empty;'` yields a header with an empty value.
- AC-CURL-5: `-d`, `--data`, `--data-ascii`, `--data-raw`, `--data-binary` set the body; multiple `-d` values join with `&`; without `-X` the method becomes POST. `-d @file` / `--data-binary @file` becomes a Binary body pointing at that path (`--data-raw @x` stays literal).
- AC-CURL-6: `--data-urlencode name=value` percent-encodes the value and joins like `-d`.
- AC-CURL-7: Body type detection: `Content-Type` containing `json` (or a body that parses as JSON with no Content-Type) → JSON; `xml` → XML; `application/x-www-form-urlencoded` or no Content-Type with `k=v&k2=v2` shape → Form-encode as key/value rows; otherwise Text.
- AC-CURL-8: `--json <data>` sets JSON body, `Content-Type: application/json`, `Accept: application/json`, and method POST unless `-X` is given.
- AC-CURL-9: `-u user:pass` / `--user` sets Auth Basic; `-u user` (no colon) sets username with empty password.
- AC-CURL-10: `-F name=value` adds a text form field; `-F name=@path` adds a file field; `;type=` suffixes are kept as the field content type; method becomes POST; body type Form.
- AC-CURL-11: `--compressed` is accepted silently (decompression is always on) and adds no header.
- AC-CURL-12: `-G`/`--get` moves all `-d` data into the query string and forces GET (unless `-X` overrides).
- AC-CURL-13: `-k`/`--insecure` sets `verifyTls=false`; `-L` sets `followRedirects=true`; `-I`/`--head` sets HEAD; `-A` sets `User-Agent`; `-b`/`--cookie` sets `Cookie`; `-e` sets `Referer`; `-m`/`--max-time` sets `timeoutMs`.
- AC-CURL-14: Combined short flags (`-sSLk`) and `--flag=value` forms are recognized; output-only flags (`-s`, `-S`, `-v`, `-i`, `-o`, `--progress-bar`) are ignored without warning.
- AC-CURL-15: An `Authorization: Bearer x` header is kept as a header (not converted to the Auth tab), preserving round-trip fidelity.
- AC-CURL-16: Chrome DevTools and Firefox "Copy as cURL (bash)" output for a GET, a JSON POST, and a multipart upload imports correctly (fixtures in core tests).
- AC-CURL-17: Input that is not a curl command shows "Not a cURL command" and changes nothing.

### 5.5 cURL export (CEXP)
- AC-CEXP-1: ⌘⇧C and the request "…" menu copy a cURL command to the clipboard with a toast.
- AC-CEXP-2: Output is multi-line bash: `curl` then `-X METHOD` (omitted for GET, and for POST when a body flag is present), `'<url>'`, one `-H` per enabled header, then the body flag, joined with ` \` + newline.
- AC-CEXP-3: All values are single-quoted with `'` escaped as `'\''`.
- AC-CEXP-4: JSON/XML/Text/GraphQL use `--data-raw`; Form-encode uses one `--data-urlencode` per field; Form uses `-F` (files as `@path`); Binary uses `--data-binary @path`; Basic auth uses `-u`; Bearer/API Key become headers/query params; `verifyTls=false` adds `-k`.
- AC-CEXP-5: Variables are resolved against the active environment by default; a toggle "Keep {{variables}}" exports raw. Secret variables are always resolved only when the toggle is off.
- AC-CEXP-6: Round-trip: export → import yields an equivalent request (method, URL, enabled headers, body type and content, auth) for every body type; covered by property tests in core.

### 5.6 OpenAPI import (OAS)
- AC-OAS-1: Import accepts a file (picker or drag-drop) or pasted text; format (JSON/YAML) and version (Swagger 2.0, OpenAPI 3.0.x, 3.1.x, 3.2.x) are auto-detected; anything else is rejected with a clear error and no partial data.
- AC-OAS-2: Creates one collection named `info.title` in the chosen project; operations are grouped into folders by their first tag; untagged operations live at collection root.
- AC-OAS-3: Request name = `summary` ‖ `operationId` ‖ `METHOD path`. URL = `{{baseUrl}}` + path, with `{param}` path segments converted to `{{param}}`.
- AC-OAS-4: An environment named after the API is created with `baseUrl` = first `servers[].url` (3.x, server variables substituted with defaults) or `scheme://host+basePath` (2.0, first scheme), plus one empty variable per distinct path parameter.
- AC-OAS-5: Query params become rows (required enabled, optional disabled) with example/default values; header params become header rows.
- AC-OAS-6: Request body uses the first `application/json` media type (else first available): its `example`/first `examples` value, otherwise a skeleton generated from the schema (internal `$ref`s resolved, max depth 5, cycles stop with `{}`); `multipart/form-data` and `x-www-form-urlencoded` schemas become Form / Form-encode rows. Swagger 2.0 `in: body` and `in: formData` map equivalently.
- AC-OAS-7: Security schemes map to collection auth: HTTP bearer → Bearer `{{token}}`, HTTP basic → Basic `{{username}}`/`{{password}}`, apiKey → API Key `{{apiKey}}` in the declared location; OAuth2/OpenID schemes import as None with a warning.
- AC-OAS-8: External `$ref`s (other files/URLs) are not fetched; affected parts are skipped and listed in an import report (operations created, warnings).
- AC-OAS-9: A 1,000-operation spec imports in ≤ 1.5 s.

### 5.7 Projects, collections, folders (COL)
- AC-COL-1: First launch creates workspace "My Workspace" with project "Default"; the sidebar shows a project switcher above the tree.
- AC-COL-2: Create, rename (inline, F2), duplicate, and delete (with confirm and child count) for projects, collections, folders, and requests.
- AC-COL-3: Folders nest to any depth; drag-and-drop reorders and moves requests/folders within and across collections of the same project; order persists.
- AC-COL-4: The sidebar filter matches request name or URL case-insensitively and keeps matching items' ancestors expanded.
- AC-COL-5: Collection settings expose description and default Auth; requests with Auth "Inherit" use it.
- AC-COL-6: Changes are written to disk within 500 ms (debounced) and survive a force quit; a corrupted project file is renamed `*.corrupt-<ts>.json` and reported, never silently dropped.

### 5.8 Environments & variables (ENV)
- AC-ENV-1: `{{name}}` (name matches `\$?[A-Za-z0-9_.-]+`, whitespace inside braces trimmed) resolves in URL, params, headers, auth fields, body text, form values, and file paths.
- AC-ENV-2: Resolution order: active project environment, then Global; disabled variables are skipped. Values may reference other variables up to depth 3; cycles leave the literal text.
- AC-ENV-3: Unresolved variables are highlighted red in editors; sending is allowed, the literal `{{name}}` is sent, and a warning shows in the response strip.
- AC-ENV-4: Hovering a resolved variable shows its value and source environment (secret values masked until clicked).
- AC-ENV-5: ⌘E opens a quick picker of the current project's environments plus "No environment"; selection persists per project.
- AC-ENV-6: Env tab supports create/rename/duplicate/delete environments and editing variables in a key/value grid with enabled and secret toggles.
- AC-ENV-7: Secret values are never synced, shared, or written to history; they stay only in local `environments.json`.
- AC-ENV-8: `{{$name}}` (a leading `$`) is a dynamic variable: a value generated at send time from a built-in catalogue of ~45 Postman-compatible generators (`$uuid`, `$guid`, `$timestamp`, `$isoTimestamp`, `$randomInt`, `$randomEmail`, `$randomFullName`, `$randomIP`, `$randomPrice`, `$randomLoremSentence`, `$randomDatePast`, …). It resolves everywhere AC-ENV-1 does, including inside environment values. One value per name per send: two `{{$uuid}}` in one request agree, and the URL in history is the URL that was sent. A new send draws new values. Known dynamic variables never count as unresolved; an unknown `{{$name}}` is sent literally and reported like AC-ENV-3. Environment variable names may not start with `$` (the Env editor and the store reject them). cURL export with "Keep {{variables}}" leaves `{{$name}}` literal, without it generates one value per name for the whole command. Share pages show `{{$name}}` literally.
- AC-ENV-9: Dynamic variables have their own highlight colour (`--var-dynamic-fg`); hovering one shows "Random email · e.g. jane.doe@example.com · new value each send". Typing `{{` in the URL bar, key/value cells, form fields and body editors opens autocomplete: environment variables first (value · source), then the `$` catalogue (description); prefix matches rank first; Enter/Tab accepts, Esc closes.
- AC-ENV-10: JSON, XML, Text and GraphQL-variables editors have "Insert fake data" next to Format: a menu of the catalogue that inserts `{{$name}}` at the caret.

### 5.9 History (HIST)
- AC-HIST-1: Every send (success, error, or cancel) appends a history entry; the newest 500 are kept.
- AC-HIST-2: Activity rows show method badge, resolved URL, relative time (refreshing each minute), and status code chip.
- AC-HIST-3: The filter matches URL, method, or status code (e.g. `404`, `QUERY`) and updates on each keystroke.
- AC-HIST-4: Clicking a row opens the request in the editor and its stored response in the response pane without re-sending; "Save to collection" turns it into a saved request.
- AC-HIST-5: Individual entries can be deleted; "Clear history" asks for confirmation.

### 5.10 Sync (SYNC)
- AC-SYNC-1: Settings → Sync accepts a Worker URL and either "Create workspace" (returns a token shown once with a copy button) or "Connect" with an existing token.
- AC-SYNC-2: When connected, local changes push within 2 s of a save (debounced); pull runs on startup, on window focus (max once per 60 s), and via "Sync now". No polling timers when idle.
- AC-SYNC-3: Synced entity types: project, collection, folder, request, environment. History and settings are never synced.
- AC-SYNC-4: Conflicts resolve last-write-wins by `updatedAt`; a losing local edit is kept as a sibling copy named "<name> (conflict)" and a toast is shown.
- AC-SYNC-5: Deletes propagate as tombstones; a deleted parent removes children on the other device.
- AC-SYNC-6: Offline or 5xx errors show a sync badge in the sidebar and retry with backoff (5 s → 5 min); the app remains fully usable.
- AC-SYNC-7: A 401 disconnects sync and prompts for a new token; local data is untouched.

### 5.11 Sharing (SHARE)
- AC-SHARE-1: "Share…" on a collection or request (requires sync connection) uploads an immutable snapshot and copies the share URL.
- AC-SHARE-2: Before sharing, a dialog lists what will be stripped: secret variables are always removed; Authorization/Cookie headers and Auth tab values are replaced by `{{placeholder}}` unless "Include credentials" is checked. Without credentials, fields this version doesn't know are left out too (a newer ReqHarbor may keep secrets there), and an item made with a newer ReqHarbor can only be shared with "Include credentials" checked. Optionally an environment (non-secret variables only) can be attached.
- AC-SHARE-3: The share URL opens a read-only web page with the collection tree, method badges, URLs, headers, and bodies, and a "Copy as cURL" button per request. No editing is possible.
- AC-SHARE-4: "Update share" replaces the snapshot behind the same URL; "Revoke" makes the URL return 410 Gone.
- AC-SHARE-5: "Import from link" in the app accepts a share URL and creates a copy in the current project (new IDs).
- AC-SHARE-6: Settings → Shares lists active shares with created/updated time, copy link, update, and revoke.

## 6. Keyboard shortcuts

| Action | macOS | Win/Linux |
|---|---|---|
| Send request / cancel when in flight | ⌘Enter | Ctrl+Enter |
| Import cURL | ⌘U | Ctrl+U |
| Change environment | ⌘E | Ctrl+E |
| New request | ⌘N | Ctrl+N |
| Save request | ⌘S | Ctrl+S |
| Focus URL bar | ⌘L | Ctrl+L |
| Copy as cURL | ⌘⇧C | Ctrl+Shift+C |
| Format body | ⌘⇧F | Ctrl+Shift+F |
| Focus sidebar filter | ⌘⇧E | Ctrl+Shift+E |
| Switch sidebar tab Activity / Collections / Env | ⌘1 / ⌘2 / ⌘3 | Ctrl+1/2/3 |
| Find in editor/response | ⌘F | Ctrl+F |
| Rename selected item | F2 | F2 |
| Close dialog / cancel | Esc | Esc |

AC-KEY-1: Every shortcut above works with focus anywhere in the window, including inside CodeMirror (editor-local bindings like ⌘F stay local). AC-KEY-2: The response empty state lists Send, Import cURL, Change Environment, and New Request with the platform-correct modifier.

## 7. Performance budgets (release blockers)

Reference machines: Apple M1 8 GB / macOS 14+, and Intel i5-8250U 8 GB / Windows 11. Measured on release builds, workspace with 1,000 saved requests and 500 history entries.

| Metric | Budget |
|---|---|
| Cold start to interactive (window shown, last state restored) | ≤ 500 ms p50, ≤ 800 ms p95 |
| Idle RAM, all processes incl. WebView, after 5 min idle | ≤ 120 MB (Rust process alone ≤ 30 MB) |
| Idle CPU | ~0% (no timers except the 1/min relative-time tick) |
| Installer size | macOS per-arch .dmg ≤ 12 MB (universal ≤ 22 MB); Windows .msi ≤ 10 MB; Linux AppImage ≤ 80 MB |
| Frontend bundle (JS + CSS, gzipped) | ≤ 350 KB, CodeMirror language modes lazy-loaded |
| Time-to-send overhead (Send keypress → first byte on socket, warm connection, excl. network) | ≤ 5 ms p95 |
| Response render, 1 MB JSON pretty + highlighted | ≤ 200 ms |
| History filter of 500 entries / sidebar filter of 1,000 requests | ≤ 16 ms per keystroke |
| cURL parse | ≤ 1 ms for a 10 KB command |
| OpenAPI import, 1,000 operations | ≤ 1.5 s |

Engineering guardrails: one long-lived reqwest `Client` per TLS setting; response bodies cross IPC as raw bytes (Tauri 2 binary response), never as JSON-escaped strings; sidebar and history lists are virtualized; no analytics or telemetry SDKs.

## 8. Cloudflare Worker API

Base: `https://<worker-host>`. JSON in/out, UTF-8. Errors: `{ "error": { "code": "string", "message": "string" } }` with proper HTTP status. All sync calls are made from the Rust core, so no CORS is needed for `/v1/*`.

Auth: `Authorization: Bearer <token>`. Token = 32 random bytes, base64url, prefixed `rh_`. The Worker stores only `SHA-256(token)`. One token per workspace for MVP; rotating invalidates the old token immediately.

| Method & path | Auth | Purpose |
|---|---|---|
| `POST /v1/workspaces` `{name}` | none (rate-limited 10/h/IP) | Create remote workspace → `{workspaceId, token}` (token returned once) |
| `GET /v1/workspace` | token | `{workspaceId, name, createdAt, entityCount}` |
| `POST /v1/workspace/token/rotate` | token | → `{token}` |
| `GET /v1/sync?since=<cursor>&limit=500` | token | → `{changes: Change[], cursor, hasMore}` ordered by `rev` |
| `POST /v1/sync` `{changes: Change[]}` (≤ 500 per call) | token | Apply LWW → `{results: [{type,id,status:"applied"\|"stale",rev}], cursor}` |
| `POST /v1/shares` `{kind:"collection"\|"request", entityId, snapshot}` | token | → `{shareId, url}` |
| `GET /v1/shares` | token | List the workspace's shares |
| `PUT /v1/shares/:shareId` `{snapshot}` | token | Replace snapshot, same URL |
| `DELETE /v1/shares/:shareId` | token | Revoke |
| `GET /s/:shareId` | public | Read-only HTML view |
| `GET /s/:shareId.json` | public | Snapshot JSON (used by "Import from link") |

`Change = { type, id, updatedAt, deleted: bool, data: object|null, rev? }`. The server assigns a per-workspace monotonically increasing `rev`; `cursor` is the highest `rev` seen. A push whose `updatedAt` is older than the stored one returns `stale` with the current server copy included in the next pull.

Self-hosted servers (accounts plan, `06-go-server.md` §9) add two things the client honours with any server: a pull page (and `GET /v1/server`) may carry `syncEpoch`; when it changes, or a pull returns a `cursor` below the `since` that was sent, the server was restored from a backup, so the client re-pulls from 0 and re-sends everything (LWW makes this idempotent). `GET /v1/server` may list `sync.types`; the client pushes only those types and keeps the others local-only with a notice ("N items aren't synced because this server is older than your app…"). The Worker has neither: a 404 means the five base types.

Share semantics: `shareId` is 128 bits of randomness, base62 (unguessable; possession = read access). The snapshot is built and sanitized by the client (§5.11), contains `schemaVersion`, the entity tree, and an optional environment, and is immutable until `PUT`. Revoked shares return 410. Shared pages send `X-Robots-Tag: noindex`, a strict CSP, no cookies, and `Cache-Control: public, max-age=60`. The HTML view is server-rendered with no client framework; its "Copy as cURL" uses a small TS port of the §5.5 exporter.

D1 schema: `workspaces(id PK, name, token_hash UNIQUE, rev INT, created_at)`, `entities(workspace_id, type, id, rev, updated_at, deleted, data TEXT, PK(workspace_id,type,id))` with index `(workspace_id, rev)`, `shares(id PK, workspace_id, kind, entity_id, snapshot TEXT, created_at, updated_at, revoked_at)`. Limits: entity `data` ≤ 512 KB, snapshot ≤ 1 MB, request body ≤ 5 MB (413 otherwise).

## 9. Out of scope (MVP)

Scripting and test assertions; OAuth and other advanced auth flows; GraphQL beyond sending a body; WebSocket/SSE/gRPC; team accounts, roles, and real-time collaboration; editable or commentable shared views; importers other than cURL and OpenAPI; mock servers, monitors, and API docs publishing; plugin system; mobile or web versions of the app; telemetry.

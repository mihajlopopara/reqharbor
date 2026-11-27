/**
 * ReqHarbor IPC types — hand-written mirror of the Rust serde types in
 * crates/core and src-tauri (see docs/API_CONTRACT.md for semantics).
 *
 * Conventions
 * - All keys are camelCase. IDs are ULID strings. Timestamps are Unix epoch ms (number).
 * - `T | null` means the key is always present (Rust Option without skip).
 * - `key?: T` means the key is omitted when empty (Rust Option with skip_serializing_if).
 *   When sending objects back to Rust, omitted and `null` are both accepted for these.
 * - Rust accepts missing fields that have defaults (see API_CONTRACT.md §2), but the UI
 *   should send complete objects as received.
 * - Forward compatibility (AC-FWD-1): data objects may carry keys not listed here, written by a
 *   newer ReqHarbor. Rust keeps them on disk and on the sync wire; the UI must not drop them
 *   (copy with spread / structuredClone instead of rebuilding objects field by field).
 */

// ───────────────────────────── Data model (PRODUCT.md §3) ─────────────────────────────

/** `^[A-Z][A-Z0-9_-]*$` — GET, POST, PUT, PATCH, DELETE, QUERY, HEAD, OPTIONS or custom. */
export type HttpMethod = string;
export const STANDARD_METHODS = ["GET", "POST", "PUT", "PATCH", "DELETE", "QUERY", "HEAD", "OPTIONS"] as const;
export const METHOD_RE = /^[A-Z][A-Z0-9_-]*$/;

export interface Project {
  id: string;
  workspaceId: string;
  name: string;
  sortOrder: number;
  /** Selected project environment (AC-ENV-5); null = "No environment" (Global still applies). */
  activeEnvironmentId: string | null;
  createdAt: number;
  updatedAt: number;
  /**
   * Oldest ReqHarbor allowed to change this item (AC-FWD-4); newer than `Bootstrap.appVersion` =
   * read-only here (rename, edit, move, copy and cascading deletes fail with `read_only_newer`).
   * Only a newer ReqHarbor writes it.
   */
  minClientVersion?: string;
}

export interface Collection {
  id: string;
  projectId: string;
  name: string;
  description: string;
  /** Default auth for requests whose auth.type is "inherit" (AC-COL-5). */
  auth: Auth;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
  /**
   * Oldest ReqHarbor allowed to change this item (AC-FWD-4); newer than `Bootstrap.appVersion` =
   * read-only here (rename, edit, move, copy and cascading deletes fail with `read_only_newer`).
   * Only a newer ReqHarbor writes it.
   */
  minClientVersion?: string;
}

export interface Folder {
  id: string;
  collectionId: string;
  /** null = directly under the collection. Folders nest to any depth. */
  parentFolderId: string | null;
  name: string;
  /** Order among siblings (folders and requests of the same parent share one sequence). */
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
  /**
   * Oldest ReqHarbor allowed to change this item (AC-FWD-4); newer than `Bootstrap.appVersion` =
   * read-only here (rename, edit, move, copy and cascading deletes fail with `read_only_newer`).
   * Only a newer ReqHarbor writes it.
   */
  minClientVersion?: string;
}

/**
 * What a request talks (AC-FWD-2). Only "http" is editable and sendable in this build; any other
 * value (including ones this build doesn't know) makes the request read-only.
 */
export type RequestKind = "http" | "graphql" | "grpc" | "ws" | (string & {});

export interface Request {
  id: string;
  /** Omitted (= "http") for every request this build creates. */
  kind?: RequestKind;
  /** Oldest ReqHarbor allowed to edit this request (AC-FWD-4); newer than `Bootstrap.appVersion` = read-only. */
  minClientVersion?: string;
  /** Owning collection; "" (empty string) for an unsaved scratch request. */
  collectionId: string;
  folderId: string | null;
  name: string;
  method: HttpMethod;
  /** Full URL as shown in the URL bar, including the query string of enabled params. */
  url: string;
  /** Query rows; source of truth for the query string at send time when non-empty (AC-REQ-2). */
  params: KeyValue[];
  headers: KeyValue[];
  auth: Auth;
  body: Body;
  settings: RequestSettings;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
}

export interface KeyValue {
  key: string;
  value: string;
  enabled: boolean;
  description?: string;
}

export type KnownAuthType = "none" | "inherit" | "basic" | "bearer" | "apikey";
/** Any other string comes from a newer ReqHarbor (e.g. "oauth2"): kept as is; such auth is read-only (AC-FWD-3). */
export type AuthType = KnownAuthType | (string & {});

export interface Auth {
  type: AuthType;
  /** basic */
  username?: string;
  /** basic */
  password?: string;
  /** bearer */
  token?: string;
  /** apikey */
  apiKey?: ApiKeyAuth;
}

export interface ApiKeyAuth {
  key: string;
  value: string;
  /** Any other value comes from a newer ReqHarbor and makes the request read-only. */
  in: "header" | "query" | (string & {});
}

export type KnownBodyType = "none" | "json" | "xml" | "text" | "form" | "formEncoded" | "graphql" | "binary";
/** Any other string comes from a newer ReqHarbor: kept as is, and the request is read-only (AC-FWD-3). */
export type BodyType = KnownBodyType | (string & {});

export interface Body {
  type: BodyType;
  /** json / xml / text */
  text?: string;
  /** form (multipart) */
  form?: FormField[];
  /** formEncoded (x-www-form-urlencoded) */
  formEncoded?: KeyValue[];
  graphql?: GraphqlBody;
  /** binary: absolute path of the file to send */
  binaryPath?: string;
}

export interface GraphqlBody {
  query: string;
  /** Variables as JSON text, exactly as edited. "" = no variables (sent as {}). */
  variables: string;
}

export interface FormField {
  key: string;
  /** Text value, or the absolute file path when kind = "file". */
  value: string;
  /** Any other value comes from a newer ReqHarbor and makes the request read-only. */
  kind: "text" | "file" | (string & {});
  enabled: boolean;
  /** Optional part Content-Type (e.g. from cURL `-F 'f=@a.png;type=image/png'`). */
  contentType?: string;
}

export interface RequestSettings {
  /** default true */
  followRedirects: boolean;
  /** default 30000; covers the whole exchange incl. redirects and body download; 0 = no limit */
  timeoutMs: number;
  /** default true; false accepts self-signed / invalid certificates */
  verifyTls: boolean;
}

export interface Environment {
  id: string;
  workspaceId: string;
  /** null = the single Global environment. */
  projectId: string | null;
  name: string;
  variables: Variable[];
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
  /**
   * Oldest ReqHarbor allowed to change this item (AC-FWD-4); newer than `Bootstrap.appVersion` =
   * read-only here (rename, edit, move, copy and cascading deletes fail with `read_only_newer`).
   * Only a newer ReqHarbor writes it.
   */
  minClientVersion?: string;
}

export interface Variable {
  /** Referenced as {{key}}; key should match [A-Za-z0-9_.-]+ and may not start with "$" ({{$name}} is a dynamic variable) */
  key: string;
  value: string;
  enabled: boolean;
  /** Never synced, shared or written to history (AC-ENV-7). */
  secret: boolean;
}

/** One project's full tree (also the on-disk format of projects/<id>.json). Arrays are unsorted: sort by sortOrder. */
export interface ProjectTree {
  project: Project;
  collections: Collection[];
  folders: Folder[];
  requests: Request[];
}

export type ErrorKind = "dns" | "connect" | "tls" | "timeout" | "cancelled" | "other";

/** A typed network error shown in the response pane (AC-REQ-9). */
export interface SendError {
  kind: ErrorKind;
  message: string;
}

export interface HistoryResponse {
  status: number;
  statusText: string;
  timeMs: number;
  /** Full decoded size, even if `body` is truncated. */
  sizeBytes: number;
  /** [name, value] in received order; names lowercase. */
  headers: [string, string][];
  /** Always "" when returned by open_history_entry — fetch bytes with get_response_body. */
  body: string;
  truncated: boolean;
  bodyEncoding: "utf8" | "base64";
}

export interface HistoryEntry {
  id: string;
  timestamp: number;
  /** Saved request this send came from; null for scratch requests. */
  sourceRequestId: string | null;
  /** The request exactly as edited (variables unresolved). */
  request: Request;
  /** URL as sent; secret variable values appear as {{name}}. */
  resolvedUrl: string;
  method: HttpMethod;
  response?: HistoryResponse;
  error?: SendError;
}

export type Theme = "system" | "dark" | "light";

/** settings.json. Unknown keys are preserved, so the UI may add its own (e.g. `shortcutsSeen`). */
export interface Settings {
  theme: Theme;
  windowWidth: number | null;
  windowHeight: number | null;
  /** default 300. Fractional values are accepted and rounded; persisted as an integer. */
  sidebarWidth: number;
  sidebarCollapsed: boolean;
  /** Request/Response split, 0..1, default 0.5 */
  splitRatio: number;
  /** "auto" | "side" | "stacked", default "auto" */
  layoutMode: string;
  lastOpenedRequestId: string | null;
  activeProjectId: string | null;
  [extra: string]: unknown;
}

// ───────────────────────────── Command payloads ─────────────────────────────

/** Rejection value of every command (`invoke(...).catch(e => e as CommandError)`). */
export interface CommandError {
  code: CommandErrorCode;
  message: string;
  /** Present for code "validation" (PrepareErrorDetails) and "format_error" (FormatErrorDetails). */
  details?: PrepareErrorDetails | FormatErrorDetails;
}

export type CommandErrorCode =
  | "not_found"
  | "invalid"
  | "io"
  | "validation"
  | "format_error"
  | "not_curl"
  | "curl_missing_url"
  | "curl_syntax"
  | "openapi_parse"
  | "openapi_unsupported"
  | "openapi_invalid"
  | "sync_not_connected"
  | "sync_unauthorized"
  | "sync_offline"
  | "sync_server"
  | "sync_not_found"
  | "sync_rejected"
  | "sync_too_large"
  | "sync_quota_exceeded"
  | "sync_protocol"
  | "share_gone"
  /** The request was made by a newer ReqHarbor: it can't be edited or sent here (AC-FWD-3/4). */
  | "read_only_newer"
  | "internal";

export type PrepareErrorDetails =
  | { kind: "invalidMethod"; message: string }
  | { kind: "invalidGraphqlVariables"; message: string }
  | { kind: "emptyUrl" }
  /** The request inherits a collection auth from a newer ReqHarbor (unknown scheme). */
  | { kind: "inheritedAuthNeedsNewerVersion" };

export interface FormatErrorDetails {
  message: string;
  /** 1-based */
  line: number;
  /** 1-based */
  column: number;
}

export interface CorruptFile {
  originalPath: string;
  /** Path of the renamed `*.corrupt-<ts>.json`; "" if renaming failed. */
  renamedTo: string;
  error: string;
}

export interface Bootstrap {
  workspace: { id: string; name: string };
  /** Sorted by sortOrder. */
  projects: Project[];
  /** Global first, then project environments. */
  environments: Environment[];
  settings: Settings;
  /** Files quarantined at startup (AC-COL-6): show a persistent error toast per file. */
  corruptFiles: CorruptFile[];
  sync: SyncStatus;
  /** "macos" | "windows" | "linux" — for ⌘ vs Ctrl (AC-KEY-2). */
  platform: string;
  dataDir: string;
  /** This build's version, e.g. "0.1.0"; compared with `Request.minClientVersion`. */
  appVersion: string;
}

export interface ChildCounts {
  collections: number;
  folders: number;
  requests: number;
}

export type NodeKind = "collection" | "folder" | "request";

export interface VariableInfo {
  key: string;
  /** Raw value (secret values included — mask them in the UI until clicked). */
  value: string;
  secret: boolean;
  environmentId: string;
  environmentName: string;
}

/** One dynamic variable: `{{$name}}` is replaced by a generated value at send time (AC-ENV-8). */
export interface DynamicVariableInfo {
  /** Including the leading "$", e.g. "$randomEmail". */
  name: string;
  /** e.g. "Random email" */
  description: string;
  /** e.g. "jane.doe@example.com" */
  example: string;
}

export interface HistorySummary {
  id: string;
  timestamp: number;
  method: HttpMethod;
  resolvedUrl: string;
  /** null when the send failed / was cancelled */
  status: number | null;
  timeMs: number | null;
  errorKind: ErrorKind | null;
  sourceRequestId: string | null;
}

export interface ResponseCookie {
  name: string;
  value: string;
  domain: string | null;
  path: string | null;
  /** Raw Expires attribute */
  expires: string | null;
  maxAge: number | null;
  httpOnly: boolean;
  secure: boolean;
  sameSite: string | null;
}

export type BodyKind = "json" | "xml" | "html" | "text" | "image" | "binary" | "empty";

export interface RedirectHop {
  status: number;
  method: HttpMethod;
  url: string;
}

/** Response metadata. The body is fetched separately as raw bytes: get_response_body(responseId). */
export interface ResponseMeta {
  /** Key for get_response_body / save_response_body. */
  responseId: string;
  status: number;
  /** Canonical reason phrase, e.g. "OK" ("" if unknown). */
  statusText: string;
  /** "HTTP/1.1", "HTTP/2.0"…; "" for history entries */
  httpVersion: string;
  /** Received order, names lowercase (AC-RES-5). */
  headers: [string, string][];
  /** Every Set-Cookie parsed (AC-RES-5). */
  cookies: ResponseCookie[];
  /** Total time incl. redirects and body download. */
  timeMs: number;
  /** Decoded body size (AC-RES-1/4). */
  sizeBytes: number;
  /** Bytes on the wire (tooltip "Wire size … (gzip)"); null for history entries. */
  wireSizeBytes: number | null;
  /** e.g. "gzip", "br", "deflate"; null if not encoded */
  contentEncoding: string | null;
  contentType: string | null;
  /** Viewer choice by Content-Type, then sniffing (AC-RES-2). */
  bodyKind: BodyKind;
  /** Value of the Accept-Query response header — highlight it (AC-QRY-7). */
  acceptQuery: string | null;
  finalUrl: string | null;
  redirects: RedirectHop[];
  /** Decompression failed; the body holds the raw bytes. */
  decodeError: string | null;
  /** History entries keep at most 1 MB of body. */
  bodyTruncated: boolean;
}

export interface SendRequestArgs {
  /** Client-generated id (e.g. a ULID/uuid) used by cancel_request and as responseId. */
  executionId: string;
  /** The request as currently edited (saved or scratch). Never auto-saved. */
  request: Request;
  /** Project whose active environment resolves variables. Defaults to the request's collection's project. */
  projectId?: string | null;
}

export interface SendResult {
  executionId: string;
  historyId: string;
  method: HttpMethod;
  /** URL as sent (secret values shown as {{name}}). */
  resolvedUrl: string;
  /** Variables that were not defined: show "⚠ n unresolved variables" (AC-ENV-3). */
  unresolvedVariables: string[];
  /** Set on any HTTP response (any status code). */
  response: ResponseMeta | null;
  /** Set on network failure / cancel / timeout. Exactly one of response / error is non-null. */
  error: SendError | null;
}

export interface OpenedHistory {
  /** entry.response.body is "" — use response.responseId with get_response_body. */
  entry: HistoryEntry;
  response: ResponseMeta | null;
}

export interface CurlImport {
  /** New unsaved request (collectionId ""), fresh id. */
  request: Request;
  /** Ignored unknown / unsupported flags (dismissible warning). */
  unknownFlags: string[];
  /** Other notes, e.g. files referenced by a browser multipart copy. */
  warnings: string[];
}

export interface SpecInfo {
  format: "json" | "yaml";
  kind: "swagger2" | "openapi3";
  /** Raw version, e.g. "2.0", "3.1.0" */
  version: string;
  /** Chip text, e.g. "OpenAPI 3.1 · YAML" */
  label: string;
}

export interface ImportReport {
  spec: SpecInfo;
  collectionName: string;
  operationsCreated: number;
  folders: { name: string; requestCount: number }[];
  rootRequestCount: number;
  baseUrl: string;
  warnings: string[];
}

export interface OpenApiPreview {
  spec: SpecInfo;
  report: ImportReport;
}

export interface OpenApiImportResult {
  report: ImportReport;
  collectionId: string;
  /** The environment created for the API (baseUrl + path params + auth vars). */
  environment: Environment;
  tree: ProjectTree;
}

export type SyncStateName = "disconnected" | "idle" | "syncing" | "offline" | "unauthorized" | "error";

export interface SyncStatus {
  state: SyncStateName;
  connected: boolean;
  workerUrl: string | null;
  remoteWorkspaceId: string | null;
  lastSyncAt: number | null;
  lastPullAt: number | null;
  lastError: string | null;
  /** When offline: time of the next automatic retry ("Retrying in 30 s"). */
  nextRetryAt: number | null;
  retryAttempt: number;
  /** Local changes a push can clear; items in `unsupportedEntities` are not counted (they wait for a server update). */
  pendingChanges: number;
  /** Local items not synced because they exceed 512 KB (never retried until edited). */
  skippedEntities: SkippedEntity[];
  /**
   * Local items of a type the server doesn't accept (`reason: "unsupported_type"`): they stay
   * local-only until the server is updated. `warning` then carries "3 items aren't synced because
   * this server is older than your app. Ask its admin to update reqharbor-server."
   */
  unsupportedEntities: SkippedEntity[];
  /** Non-fatal warning for the sync indicator, e.g. "1 item is too large to sync (over 512 KB): Big upload". */
  warning: string | null;
}

export interface SkippedEntity {
  type: EntityType;
  id: string;
  name: string;
  /**
   * "too_large" (over 512 KB), "unsupported_type" (the server doesn't take this type yet), or the
   * reason the server rejected the change with, e.g. "invalid_json_value" or "forbidden_role" (any
   * other string may appear). The UI words it with `skippedReasonText` (ui/src/lib/util/format.ts).
   */
  reason: string;
}

export interface RemoteWorkspace {
  workspaceId: string;
  name: string;
  createdAt: number;
  entityCount: number;
  /** Storage used on the Worker (bytes); null if the Worker does not report it. */
  bytesUsed: number | null;
  /** Workspace quota (bytes, 50 MB); null if not reported. */
  quotaBytes: number | null;
}

export interface ShareImportResult {
  tree: ProjectTree;
  /** File paths removed from the untrusted share (Binary body / Form file fields). */
  warnings: string[];
}

export interface ConflictCopy {
  type: EntityType;
  /** The entity that took the remote version. */
  id: string;
  /** "<name> (conflict)" */
  name: string;
  /** The new sibling holding the losing local edit. */
  copyId: string;
}

export type EntityType = "project" | "collection" | "folder" | "request" | "environment";

export interface SyncReport {
  pushed: number;
  pulled: number;
  deleted: number;
  /** Non-empty => show a toast (AC-SYNC-4). */
  conflicts: ConflictCopy[];
  /** Remote changes that could not be applied (orphans, malformed). */
  skipped: string[];
  /** Local entities currently excluded from sync because they exceed 512 KB. */
  tooLarge: SkippedEntity[];
  /** Local entities of a type this server doesn't accept; they stay local-only. */
  unsupported: SkippedEntity[];
  /** The server was restored from a backup: the pull restarted from 0 and everything was re-sent. */
  restored: boolean;
  cursor: number;
}

export interface CreatedWorkspace {
  workspaceId: string;
  /** Shown once (AC-SYNC-1). */
  token: string;
}

export type ShareKind = "collection" | "request";

export interface ShareInfo {
  shareId: string;
  url: string;
  kind: ShareKind | null;
  entityId: string | null;
  name: string | null;
  createdAt: number | null;
  updatedAt: number | null;
}

export interface SharePreview {
  name: string;
  requestCount: number;
  folderCount: number;
  /** Secret variables removed from the attached environment (always). */
  secretVariablesRemoved: number;
  /**
   * Authorization / Proxy-Authorization / Cookie headers and auth values replaced by {{placeholders}},
   * plus each block of fields this build doesn't know (left out, as they may hold secrets).
   */
  credentialsReplaced: number;
  /** Same count regardless of the "Include credentials" choice. */
  credentialFields: number;
}

/** Snapshot uploaded to the Worker (also what preview_share returns under `snapshot`). */
export interface ShareSnapshot {
  schemaVersion: 1;
  collection?: Collection & { folders: Folder[]; requests: Request[] };
  request?: Request;
  environment: { name: string; variables: Variable[] } | null;
}

// ───────────────────────────── Events ─────────────────────────────

export interface Events {
  /** Sync indicator state changed (AC-SYNC-2/6/7). */
  "sync://status": SyncStatus;
  /** A pull changed local data: reload project trees + environments; toast conflicts. */
  "sync://changed": SyncReport;
}

// ───────────────────────────── Command map ─────────────────────────────

/** `invoke(name, args)` signatures. Optional args may be omitted or null. */
export interface Commands {
  bootstrap: { args: {}; result: Bootstrap };
  get_settings: { args: {}; result: Settings };
  save_settings: { args: { settings: Settings }; result: Settings };

  list_projects: { args: {}; result: Project[] };
  get_project_tree: { args: { projectId: string }; result: ProjectTree };
  create_project: { args: { name: string }; result: ProjectTree };
  rename_project: { args: { projectId: string; name: string }; result: Project };
  duplicate_project: { args: { projectId: string }; result: ProjectTree };
  delete_project: { args: { projectId: string }; result: Project[] };
  move_project: { args: { projectId: string; index: number }; result: Project[] };
  set_active_environment: { args: { projectId: string; environmentId?: string | null }; result: Project };
  child_counts: { args: { kind: "project" | "collection" | "folder" | "request"; id: string }; result: ChildCounts };

  create_collection: { args: { projectId: string; name: string }; result: Collection };
  update_collection: { args: { collection: Collection }; result: Collection };
  duplicate_collection: { args: { collectionId: string }; result: ProjectTree };
  delete_collection: { args: { collectionId: string }; result: ProjectTree };

  create_folder: { args: { collectionId: string; parentFolderId?: string | null; name: string }; result: Folder };
  rename_folder: { args: { folderId: string; name: string }; result: Folder };
  duplicate_folder: { args: { folderId: string }; result: ProjectTree };
  delete_folder: { args: { folderId: string }; result: ProjectTree };

  create_request: {
    args: { collectionId: string; folderId?: string | null; request?: Request | null };
    result: Request;
  };
  save_request: { args: { request: Request }; result: Request };
  rename_request: { args: { requestId: string; name: string }; result: Request };
  duplicate_request: { args: { requestId: string }; result: Request };
  delete_request: { args: { requestId: string }; result: ProjectTree };
  move_node: {
    args: {
      kind: NodeKind;
      id: string;
      targetCollectionId?: string | null;
      targetFolderId?: string | null;
      index: number;
    };
    result: ProjectTree;
  };

  list_environments: { args: {}; result: Environment[] };
  create_environment: { args: { projectId: string; name: string }; result: Environment };
  update_environment: { args: { environment: Environment }; result: Environment };
  duplicate_environment: { args: { environmentId: string }; result: Environment };
  delete_environment: { args: { environmentId: string }; result: Environment[] };
  get_variables: { args: { projectId?: string | null }; result: VariableInfo[] };
  get_dynamic_variables: { args: {}; result: DynamicVariableInfo[] };

  list_history: { args: {}; result: HistorySummary[] };
  open_history_entry: { args: { historyId: string }; result: OpenedHistory };
  delete_history_entry: { args: { historyId: string }; result: null };
  clear_history: { args: {}; result: null };

  send_request: { args: SendRequestArgs; result: SendResult };
  cancel_request: { args: { executionId: string }; result: boolean };
  get_response_body: { args: { responseId: string; limit?: number | null }; result: ArrayBuffer };
  save_response_body: { args: { responseId: string; path: string }; result: null };

  format_body: { args: { kind: "json" | "xml"; text: string }; result: string };
  url_to_params: { args: { url: string; params: KeyValue[] }; result: KeyValue[] };
  params_to_url: { args: { url: string; params: KeyValue[] }; result: string };

  import_curl: { args: { text: string }; result: CurlImport };
  looks_like_curl: { args: { text: string }; result: boolean };
  export_curl: {
    args: { request: Request; projectId?: string | null; keepVariables: boolean };
    result: string;
  };

  preview_openapi: { args: { text?: string | null; path?: string | null }; result: OpenApiPreview };
  import_openapi: {
    args: { projectId: string; text?: string | null; path?: string | null };
    result: OpenApiImportResult;
  };

  get_sync_status: { args: {}; result: SyncStatus };
  sync_create_workspace: { args: { workerUrl: string; name?: string | null }; result: CreatedWorkspace };
  sync_connect: { args: { workerUrl: string; token: string }; result: SyncStatus };
  sync_disconnect: { args: {}; result: SyncStatus };
  sync_rotate_token: { args: {}; result: { token: string } };
  sync_now: { args: {}; result: SyncReport };
  sync_workspace_info: { args: {}; result: RemoteWorkspace };

  share_preview: {
    args: { kind: ShareKind; entityId: string; includeCredentials: boolean; environmentId?: string | null };
    result: SharePreview;
  };
  share_create: {
    args: { kind: ShareKind; entityId: string; includeCredentials: boolean; environmentId?: string | null };
    result: ShareInfo;
  };
  share_update: {
    args: {
      shareId: string;
      kind: ShareKind;
      entityId: string;
      includeCredentials: boolean;
      environmentId?: string | null;
    };
    result: ShareInfo;
  };
  share_revoke: { args: { shareId: string }; result: null };
  share_list: { args: {}; result: ShareInfo[] };
  import_share: { args: { url: string; projectId: string }; result: ShareImportResult };
  preview_share: {
    args: { url: string };
    result: { kind: ShareKind; snapshot: ShareSnapshot; warnings: string[] };
  };
}

export type CommandName = keyof Commands;

/**
 * Typed wrapper. Usage in the UI:
 *   import { invoke as rawInvoke } from "@tauri-apps/api/core";
 *   export const call = makeCall(rawInvoke);
 *   const tree = await call("get_project_tree", { projectId });
 */
export function makeCall(
  rawInvoke: (cmd: string, args?: Record<string, unknown>) => Promise<unknown>,
) {
  return function call<K extends CommandName>(cmd: K, args: Commands[K]["args"]): Promise<Commands[K]["result"]> {
    return rawInvoke(cmd, args as Record<string, unknown>) as Promise<Commands[K]["result"]>;
  };
}

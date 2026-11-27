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
 * What a request talks (AC-FWD-2). "http", "ws" (WebSocket; Socket.IO when `ws.socketIo` is set,
 * AC-WS-1) and "grpc" (AC-GRPC-1) are editable in this build; any other value (including ones this
 * build doesn't know) makes the request read-only.
 */
export type RequestKind = "http" | "graphql" | "grpc" | "ws" | (string & {});

export interface Request {
  id: string;
  /** Omitted (= "http") for HTTP requests; "ws" for WebSocket / Socket.IO requests; "grpc" for gRPC requests. */
  kind?: RequestKind;
  /**
   * Oldest ReqHarbor allowed to edit this request (AC-FWD-4); newer than `Bootstrap.appVersion` = read-only.
   * WebSocket requests are stamped with WS_MIN_CLIENT_VERSION "0.1.0" (AC-WS-1), gRPC requests with
   * GRPC_MIN_CLIENT_VERSION "0.1.0" (AC-GRPC-1): fixed values, never the running version, never lowered.
   */
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
  /**
   * WebSocket settings (kind "ws"). Headers, auth, params and settings apply to the opening handshake;
   * `body` is unused. Kept when the kind is switched back to HTTP.
   */
  ws?: WsConfig;
  /**
   * gRPC settings (kind "grpc", AC-GRPC-*). `headers` are the call's metadata, `auth` applies as metadata,
   * `settings.timeoutMs` is the unary deadline and `settings.verifyTls` applies; `params` and `body` are
   * unused. Kept when the kind is switched back to HTTP.
   */
  grpc?: GrpcConfig;
  sortOrder: number;
  createdAt: number;
  updatedAt: number;
}

export interface WsConfig {
  /** Saved messages, in user order. */
  messages: SavedMessage[];
  /** `Sec-WebSocket-Protocol` values offered on connect. */
  protocols: string[];
  /** Set = Socket.IO mode (Engine.IO v4 over the WebSocket). */
  socketIo?: SocketIoConfig;
  /** Reconnect (up to 5 times, 1 s … 16 s apart) after the connection drops; never after Disconnect. */
  autoReconnect: boolean;
}

/** Any other string comes from a newer ReqHarbor and makes the request read-only (AC-FWD-3). */
export type MessageKind = "text" | "json" | "binary" | (string & {});

export interface SavedMessage {
  name: string;
  kind: MessageKind;
  /**
   * Text, JSON text, or base64 for "binary". Variables resolve when sent. Socket.IO: the body is the
   * event's single argument (a JSON value; a string for "text"; empty JSON = no argument).
   */
  body: string;
  /** Socket.IO: event name. */
  event?: string;
  /** Socket.IO: ask for an acknowledgement (omitted when false). */
  ack?: boolean;
}

export interface SocketIoConfig {
  /** Engine.IO path, default "/socket.io/" (replaces the URL's path). */
  path: string;
  /** Namespace, default "/". */
  namespace: string;
  /** Always 4 in this build; any other value makes the request read-only. */
  version: number;
  /** Optional JSON object sent as the CONNECT auth payload (variables resolve). */
  auth?: string;
}

/** Where a gRPC request's services come from. Any other string makes the request read-only (AC-FWD-3). */
export type GrpcProtoSource = "reflection" | "files" | (string & {});

export interface GrpcConfig {
  /** Full service name, e.g. "demo.Greeter"; "" until a method is picked. */
  service: string;
  /** Method name, e.g. "SayHello"; "" until one is picked. */
  method: string;
  /** The request message as JSON text (proto3 JSON mapping). Variables resolve when sent; "" counts as {}. */
  message: string;
  /** Default "reflection". */
  protoSource: GrpcProtoSource;
  /**
   * Absolute paths of .proto files (protoSource "files"). Device-local: emptied in team pushes (pulls keep this
   * device's list) and in shares (AC-GRPC-4, AC-GRPC-12). Variables resolve.
   */
  protoFiles: string[];
  /** Folders searched for imports (before each file's own folder and the bundled well-known types). Device-local. */
  importPaths: string[];
  /** Saved messages, in user order (AC-GRPC-6). */
  messages: GrpcSavedMessage[];
}

export interface GrpcSavedMessage {
  name: string;
  /** JSON text. */
  body: string;
}

export type GrpcCallKind = "unary" | "serverStreaming" | "clientStreaming" | "bidiStreaming";

/** Services and the message shapes of their methods (AC-GRPC-3..6). */
export interface GrpcDescriptor {
  /**
   * The resolved origin it belongs to, secrets masked, e.g. "grpcs://api.example.com:443": the cache key of a
   * reflection result.
   */
  url: string;
  /** When it was fetched (reflection) or compiled (files), epoch ms. */
  fetchedAt: number;
  source: "reflection" | "files";
  /** source "files": the .proto files compiled (resolved paths); [] for reflection. */
  files: string[];
  /** Sorted by name; reflection services (`grpc.reflection.*`) left out. */
  services: GrpcServiceInfo[];
  /** Every message type the methods use, transitively, by full name (e.g. "demo.HelloRequest"). */
  messages: Record<string, GrpcMessageInfo>;
  /** Every enum those messages use, by full name. */
  enums: Record<string, GrpcEnumInfo>;
  /** Size of the encoded descriptor set. */
  sizeBytes: number;
}

export interface GrpcServiceInfo {
  /** Full name, e.g. "demo.Greeter". */
  name: string;
  methods: GrpcMethodInfo[];
}

export interface GrpcMethodInfo {
  name: string;
  callKind: GrpcCallKind;
  /** Full message names. */
  inputType: string;
  outputType: string;
}

export type GrpcScalarKind =
  | "double" | "float" | "int32" | "int64" | "uint32" | "uint64" | "sint32" | "sint64"
  | "fixed32" | "fixed64" | "sfixed32" | "sfixed64" | "bool" | "string" | "bytes";

export interface GrpcMessageInfo {
  /** In declaration order. */
  fields: GrpcFieldInfo[];
}

export interface GrpcFieldInfo {
  /** Name in the .proto file. */
  name: string;
  /** lowerCamelCase JSON name (what completion offers; the proto name is accepted too). */
  jsonName: string;
  number: number;
  /** For a map field: the value's kind. */
  kind: GrpcScalarKind | "message" | "enum";
  /** Full message or enum name when `kind` is "message" / "enum" (map fields: of the value). */
  typeName?: string;
  /** Repeated (never true for maps). */
  repeated: boolean;
  /** Set for `map<K, V>` fields: the key's kind. */
  mapKey?: GrpcScalarKind;
  /** The oneof it belongs to (synthetic oneofs of proto3 `optional` are left out). */
  oneof?: string;
}

export interface GrpcEnumInfo {
  /** Value names in declaration order. */
  values: string[];
}

/** A gRPC status (AC-GRPC-7). */
export interface GrpcStatus {
  /** 0 = OK … 16 = UNAUTHENTICATED. */
  code: number;
  /** "OK", "NOT_FOUND", … ("CODE_17" for a code this build doesn't know). */
  name: string;
  message: string;
  /** Decoded `grpc-status-details-bin` (google.rpc.Status details); [] when absent. */
  details: GrpcStatusDetail[];
}

export interface GrpcStatusDetail {
  /** e.g. "type.googleapis.com/google.rpc.BadRequest" */
  typeUrl: string;
  /** Pretty JSON when the type is known (google.rpc error details or the service's own types), else null. */
  json: string | null;
  /** The raw value, base64. */
  base64: string;
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
  /**
   * The operation to run when the document defines several (AC-GQL-9); sent as `operationName`.
   * Omitted (or blank) = not sent. Older builds keep it as an unknown key and send without it.
   */
  operationName?: string;
}

/** A GraphQL schema for one endpoint (AC-GQL-2..5): fetched by introspection or loaded from a file. Local only, never synced. */
export interface GraphqlSchema {
  /** The resolved URL it belongs to (secret variables shown as `{{name}}`): the cache key. */
  url: string;
  /** When it was fetched or loaded (epoch ms). */
  fetchedAt: number;
  source: "introspection" | "file";
  /** For a schema loaded from a file. */
  fileName?: string;
  /** "introspection": `schema` is JSON text `{"__schema": …}`; "sdl": GraphQL SDL text. */
  format: "introspection" | "sdl";
  schema: string;
  sizeBytes: number;
}

/** Args shared by the graphql_* schema commands: the request as edited, resolved like `send_request`. */
export interface GraphqlSchemaArgs {
  request: Request;
  projectId?: string | null;
}

/** graphql_cached_schema result. */
export interface GraphqlCacheLookup {
  /** The request's schema URL (the cache key: `{{$dynamic}}` literal, secrets as `{{name}}`). */
  url: string;
  /** The cached schema is the `known` one passed in: `schema` is left out (no 10 MB re-send). */
  unchanged: boolean;
  schema: GraphqlSchema | null;
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
  /** WebSocket / Socket.IO session (method "WS" / "SIO"); `response` is absent. */
  ws?: WsSessionSummary;
  /** gRPC call or stream (method "GRPC", AC-GRPC-10); `response` is absent. */
  grpc?: GrpcCallSummary;
}

/** One gRPC call or stream in history (AC-GRPC-10). */
export interface GrpcCallSummary {
  service: string;
  method: string;
  callKind: GrpcCallKind;
  durationMs: number;
  /** Messages sent / received. */
  sent: number;
  received: number;
  /** Absent when the call never got a status (connect error: see `HistoryEntry.error`). */
  status?: GrpcStatus;
  /** Unary: the response message as pretty JSON (at most 1 MB; `responseTruncated`). Not in HistorySummary. */
  response?: string;
  responseTruncated?: boolean;
  /** Unary: response headers and trailers (names lowercase). Not in HistorySummary. */
  headers?: [string, string][];
  trailers?: [string, string][];
}

export interface WsSessionSummary {
  durationMs: number;
  /** Data messages sent (text, binary, Socket.IO events). */
  sent: number;
  /** Data messages received (text, binary, Socket.IO events and acks). */
  received: number;
  closeCode?: number;
  closeReason?: string;
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
  /** Present for code "validation" (PrepareErrorDetails), "format_error" (FormatErrorDetails) and the graphql_* codes (GraphqlErrorDetails). */
  details?: PrepareErrorDetails | FormatErrorDetails | GraphqlErrorDetails | GrpcErrorDetails;
}

/** `details` of the graphql_* error codes (AC-GQL-3, AC-GQL-5). */
export type GraphqlErrorDetails =
  /** graphql_network */
  | { kind: ErrorKind }
  /** graphql_introspection_disabled */
  | { serverMessage: string }
  /** graphql_introspection_failed */
  | { status: number | null }
  /** graphql_schema_too_large */
  | { sizeBytes: number };

/** `details` of the grpc_* error codes. */
export type GrpcErrorDetails =
  /** grpc_network */
  | { kind: ErrorKind }
  /** grpc_reflection_failed: the status the reflection service answered */
  | { code: number; name: string }
  /** grpc_descriptor_too_large */
  | { sizeBytes: number };

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
  // ── accounts: discovery and sign-in (06-go-server §9) ──
  /** Network error, DNS or 5xx: "Can't reach rh.acme.dev. Check the address and your connection." */
  | "server_unreachable"
  /** The Worker, or a server without accounts: "This server only supports token sync. Use Sync › Advanced." */
  | "token_sync_only"
  | "not_reqharbor"
  /** "This server needs ReqHarbor X or newer." Sign-in is blocked. */
  | "client_too_old"
  /** The server lists no sign-in methods. */
  | "no_sign_in_methods"
  /** Plain http to a non-loopback host without "Continue anyway". */
  | "insecure_unconfirmed"
  /** No local port could be opened for the browser redirect (mock: `?mockAuth=no_port`). */
  | "loopback_unavailable"
  | "sign_in_cancelled"
  /** 10 minutes passed without the browser coming back. */
  | "sign_in_timeout"
  /** The browser came back with an error (cancelled or denied at the provider). */
  | "sign_in_denied"
  /** The code exchange failed (`invalid_grant`). */
  | "sign_in_failed"
  /** The session ended (AC-AUTH-9): sign in again; local data is untouched. */
  | "sign_in_required"
  | "not_signed_in"
  /** The session could not be saved to (or read from) the keychain. */
  | "keychain_unavailable"
  | "rate_limited"
  | "server_full"
  | "server_error"
  /** Any other refusal from an account endpoint (message from the server). */
  | "auth_rejected"
  // ── WebSocket (AC-WS-*) ──
  /** ws_send on a connection that isn't open (never opened, still connecting, or closed). */
  | "ws_not_connected"
  /** ws_send: the message failed validation (invalid JSON or base64, missing event name…); the message says why. */
  | "ws_invalid_message"
  // ── accounts: personal sync and bindings (04 §6, 06-go-server §9) ──
  /** The project syncs with a space where this account is a viewer (04 §6.8). */
  | "read_only_role"
  /** 403 `not_a_member` / 404 `space_not_found` on a space call: temporarily unavailable, never a removal. */
  | "space_unavailable"
  | "forbidden_role"
  // ── teams and invites (04 §5.7, 06-go-server §10 M2) ──
  /** The server doesn't advertise `features.teams`. */
  | "teams_disabled"
  | "last_owner"
  | "transfer_required"
  | "limit_teams"
  | "limit_members"
  | "limit_invites"
  /** Expired, revoked, used up or created before a removal: one generic message (AC-INV-5). */
  | "invite_invalid"
  /** The pasted text isn't an `/i#…` invite link. */
  | "invite_not_a_link"
  /** The invite is for another server than the signed-in one. */
  | "invite_other_server"
  | "not_a_member"
  | "team_not_found"
  | "member_not_found"
  | "invite_not_found"
  /** 400 from a team endpoint: the server's message says what's wrong. */
  | "validation_failed"
  | "quota_exceeded"
  | "clock_skew"
  // ── GraphQL schemas (AC-GQL-2..5) ──
  /** graphql_fetch_schema: network failure; `details` = `{ kind: ErrorKind }`. */
  | "graphql_network"
  /** The server refuses introspection; `message` = "Introspection is disabled on this server", `details` = `{ serverMessage }`. */
  | "graphql_introspection_disabled"
  /** Any other answer without a schema; `message` says why, `details` = `{ status: number | null }`. */
  | "graphql_introspection_failed"
  /** graphql_load_schema_file: not SDL or introspection JSON. */
  | "graphql_schema_invalid"
  /** Over 10 MB; `details` = `{ sizeBytes }`. */
  | "graphql_schema_too_large"
  // ── gRPC (AC-GRPC-*) ──
  /** grpc_fetch_descriptor: network failure; `details` = `{ kind: ErrorKind }` (`cancelled` after cancel_request: show nothing). */
  | "grpc_network"
  /** The server has no reflection service (v1 and v1alpha answer UNIMPLEMENTED): offer "Use .proto files". */
  | "grpc_reflection_unsupported"
  /** Reflection answered another error status, or an unusable descriptor; `details` = `{ code, name }` when a status. */
  | "grpc_reflection_failed"
  /** The .proto files don't compile (or can't be read); `message` = "greeter.proto:12:5: …". */
  | "grpc_proto_invalid"
  /** A descriptor set over 10 MB; `details` = `{ sizeBytes }`. */
  | "grpc_descriptor_too_large"
  /** grpc_send on a call that isn't open, already ended its sending side, or is unknown. */
  | "grpc_not_connected"
  /** grpc_send: the message isn't valid JSON or doesn't fit the input type; the message says why. */
  | "grpc_invalid_message"
  | "internal";

export type PrepareErrorDetails =
  | { kind: "invalidMethod"; message: string }
  | { kind: "invalidGraphqlVariables"; message: string }
  | { kind: "emptyUrl" }
  /** The request inherits a collection auth from a newer ReqHarbor (unknown scheme). */
  | { kind: "inheritedAuthNeedsNewerVersion" }
  /** send_request / export_curl / graphql_* got a WebSocket or gRPC request. */
  | { kind: "notHttp" }
  /** ws_connect got a request whose kind isn't "ws". */
  | { kind: "notWebSocket" }
  | { kind: "invalidWsUrl"; message: string }
  | { kind: "invalidSocketIoAuth"; message: string }
  /** ws_connect: the Socket.IO auth payload uses an undefined variable (`message` = its name), e.g. `{{socketIoAuth}}` of a share. */
  | { kind: "undefinedSocketIoAuthVariable"; message: string }
  /** A grpc_* command or export_grpcurl got a request whose kind isn't "grpc". */
  | { kind: "notGrpc" }
  /** Another scheme than grpc / grpcs / http / https, or no host. */
  | { kind: "invalidGrpcUrl"; message: string }
  /** No service or method picked ("Pick a method"). */
  | { kind: "noGrpcMethod" }
  /** The picked method isn't in the services (`message` = "demo.Greeter/X isn't in the descriptor"). */
  | { kind: "unknownGrpcMethod"; message: string }
  /** A metadata name that isn't a header name or is reserved, or a `-bin` value that isn't base64. */
  | { kind: "invalidGrpcMetadata"; message: string }
  /** The message isn't JSON ("Invalid JSON at line L, col C: …") or doesn't fit the input type. */
  | { kind: "invalidGrpcMessage"; message: string }
  /** grpc_invoke on a method that streams (`callKind` ≠ "unary"): nothing was sent; connect to it instead (grpc_start). */
  | { kind: "grpcStreamingMethod"; callKind: GrpcCallKind };

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
  /** The `accounts` flag: show Settings › Account and sign-in (off in release builds until personal sync ships). */
  accounts: boolean;
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
  /** WebSocket / Socket.IO sessions: duration and message counts (status and timeMs are null). */
  ws: WsSessionSummary | null;
  /** gRPC calls (method "GRPC"): without response, headers, trailers and status details; status and timeMs are null. */
  grpc: GrpcCallSummary | null;
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

/**
 * `signInRequired`: account mode, the session ended (account spaces wait; token sync keeps going).
 * `paused`: account mode with nothing syncing right now (signed out, or every space paused).
 */
export type SyncStateName = "disconnected" | "idle" | "syncing" | "offline" | "unauthorized" | "error" | "signInRequired" | "paused";

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
  /** `account.json` holds bindings (04 §6.1): every project syncs with its own space or stays on this device. */
  accountMode: boolean;
  /** One entry per bound space: personal first, then teams, then the legacy workspace (UX §6.6, §11). */
  spaces: SpaceStatus[];
  /** The token-synced workspace while in account mode (UX §11 "LEGACY WORKSPACE"). */
  legacy: LegacyStatus | null;
  /** The first-sync checklist is waiting for an answer (UX §10): open `FirstSyncDialog`. */
  firstSyncPending: boolean;
  /** Moves that didn't finish (UX §9): offer Finish move / Cancel move. */
  pendingMoves: PendingMove[];
}

/** An unfinished project move: nothing changed locally yet; resume or cancel it. */
export interface PendingMove {
  moveId: string;
  projectId: string;
  projectName: string;
  /** Target space; null = this device only. */
  to: string | null;
  targetName: string;
  /** Items the target already holds, of `total`. */
  done: number;
  total: number;
  /** The copy is already saved here: offer Finish move only (Cancel and other targets are refused). */
  copySaved: boolean;
}

// ───────────────────────────── Spaces (04 §6, 06-go-server §9) ─────────────────────────────

export type SpaceKind = "personal" | "team" | "legacy";
/** "unknown": a role from a newer server, treated as read-only. */
export type SpaceRole = "viewer" | "editor" | "admin" | "owner" | "unknown";
/**
 * `paused` (another server, instance or account signed in: `reason: "foreignAccount"`), `unavailable`
 * (a transient 403/404; retried, local data untouched), `removed` (only after an authoritative
 * `/v1/me/spaces`), `signedOut` (no account signed in), `sharesOnly` (legacy after the merge).
 */
export type SpaceStateName = "active" | "paused" | "unavailable" | "removed" | "signedOut" | "sharesOnly";

/** A per-space problem; `message` is the UX §6.7 copy. */
export interface SpaceIssue {
  /**
   * `roleChanged`: unsynced changes are frozen after a role downgrade or a `forbidden_role` refusal
   * (the role-changed banner, UX §6.8). `heldDelete`: a project, collection or folder deleted on the
   * server is kept here because it holds newer unsynced edits (same banner).
   */
  code: "quota" | "serverFull" | "clockSkew" | "clockFixed" | "clientTooOld" | "unavailable" | "legacyDisabled" | "roleChanged" | "heldDelete" | "error" | (string & {});
  message: string;
  at: number;
}

export interface SpaceStatus {
  spaceId: string;
  kind: SpaceKind;
  /** "Personal", the team name, or "Legacy". */
  name: string;
  role: SpaceRole | null;
  state: SpaceStateName;
  /** Why it is paused ("foreignAccount") or removed ("removed", "left", …). */
  reason: string | null;
  issue: SpaceIssue | null;
  lastSyncAt: number | null;
  pendingChanges: number;
  projectCount: number;
  bytesUsed: number | null;
  quotaBytes: number | null;
  /** Items held back because this server doesn't take their type yet. */
  unsupported: number;
  /** The account that bound the space ("These projects were synced by priya@acme.dev"). */
  accountEmail: string;
  /** Team spaces: the team (for Team settings ›) and how many members can see it. */
  teamId: string | null;
  memberCount: number | null;
  /** Changes waiting for the role-changed banner (UX §6.8): Save as personal copy or Discard. */
  frozen: number;
  /** `"type:id"` of containers deleted on the server but kept here for newer unsynced edits. */
  held: string[];
}

export interface LegacyStatus {
  workerUrl: string;
  remoteWorkspaceId: string;
  /** "sharesOnly" after the merge: no pulls or pushes, share links still manageable. */
  mode: "sync" | "sharesOnly";
  projectCount: number;
  /** The token could not be read from the secret store. */
  tokenMissing: boolean;
}

/** Where a project syncs (the grouped project switcher, UX §5.1). */
export interface ProjectLocation {
  projectId: string;
  /** null: this device only. */
  spaceId: string | null;
  kind: "personal" | "team" | "legacy" | "local";
  /** "Personal", the team name, "Legacy workspace" or "This device only". */
  label: string;
  role: SpaceRole | null;
  /** The binding's state; null for local-only projects. */
  state: SpaceStateName | null;
}

/** One row of the first-sync checklist (UX §10). */
export interface FirstSyncCandidate {
  projectId: string;
  name: string;
  collections: number;
  requests: number;
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

/** Payload of `sync://notice`: "Signed in · syncing 4 projects from your account and 1 team." */
export interface SyncNotice {
  kind: "info" | "success";
  message: string;
}

/**
 * `legacy_merge`: the merge is local and complete once the command returns; the follow-up sync
 * is reported separately, so a failed sync never reads as a failed merge.
 */
export interface LegacyMergeResult {
  /** Projects moved from the legacy workspace into Personal. */
  merged: number;
  /** The follow-up sync, when it succeeded. */
  report: SyncReport | null;
  /** Why the follow-up sync failed; the merged data syncs on a later run. */
  syncError: CommandError | null;
}

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
  /** Account mode: the space the share lives in ("legacy" for token sync). */
  spaceId?: string | null;
  /** Space shares: who created it; `name` null = "Former member" (AC-TSHARE-2). */
  createdBy?: UserRef | null;
  /** Space shares: the options it was uploaded with (AC-TSHARE-3). */
  options?: { includeCredentials: boolean; environmentId: string | null } | null;
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

// ───────────────────────────── Accounts (06-go-server §5, §9) ─────────────────────────────

/** One sign-in method of a server. Unknown ids get a generic "Continue with {label}" button. */
export interface AuthMethod {
  id: "password" | "github" | "google" | "oidc" | (string & {});
  label: string;
}

/** `GET /v1/server` (unknown fields from a newer server may appear and are kept). */
export interface ServerInfo {
  software: string;
  version: string;
  apiVersion: number;
  instanceId: string;
  syncEpoch: string;
  name: string;
  publicUrl: string;
  minClientVersion: string | null;
  auth: {
    methods: AuthMethod[];
    signup: "open" | "invite" | "closed";
    passwordReset: "email" | "admin";
    setupRequired: boolean;
  };
  features: { accounts: boolean; teams: boolean; legacyWorkspaces: "open" | "existing" | "off" };
  sync: { types: string[]; requestKinds: string[] };
}

export type DiscoveryStatus = "ok" | "unreachable" | "token_sync_only" | "not_reqharbor" | "client_too_old";

/** Result of `server_discover` (the 06 §9 table). */
export interface ServerDiscovery {
  /** The normalised origin that was asked, e.g. "https://rh.acme.dev". */
  serverUrl: string;
  status: DiscoveryStatus;
  /** Error or notice copy (unreachable, token sync only, setup required, no methods…). */
  message: string | null;
  info: ServerInfo | null;
  /** `publicUrl` when it differs from `serverUrl`: offer "Use that address?". */
  canonicalUrl: string | null;
  /** Plain http to a non-loopback host: warn, and require "Continue anyway". */
  insecure: boolean;
  /** Whether Sign in may be enabled. */
  canSignIn: boolean;
  fetchedAt: number;
  /** The server is unreachable; this is the cached answer (≤ 24 h old). */
  cached: boolean;
}

/** The signed-in account (account.json; never holds a token). */
export interface Account {
  serverUrl: string;
  serverInstanceId: string;
  serverName: string;
  userId: string;
  email: string;
  name: string;
  /** "password" | "github" | "google" | "oidc" | … */
  method: string;
  sessionId: string;
}

export type SignInStateName = "idle" | "checking" | "waiting" | "exchanging" | "success" | "error";

export interface SignInStatus {
  state: SignInStateName;
  serverUrl: string | null;
  method: string | null;
  /** When the browser was opened (the "Waiting 0:42" timer). */
  startedAt: number | null;
  /** Set when `state` is "error". */
  error: CommandErrorCode | null;
  /** The error's copy; while "waiting", "Couldn't open your browser…" when the browser didn't open. */
  message: string | null;
  /** While "waiting", when the system browser couldn't be opened: the sign-in link for "Copy link". */
  authorizeUrl: string | null;
}

/** `auth_status` and the `auth://status` event. */
export interface AuthStatus {
  /** The `accounts` flag. */
  enabled: boolean;
  account: Account | null;
  /** AC-AUTH-9: "Your session ended. Sign in again to keep syncing." */
  signInRequired: boolean;
  keychain: "system" | "file" | "unavailable";
  /** Shown as a warning when the 0600 file store is used. */
  keychainNote: string | null;
  /** The keychain refused to return the session: signed-out view with a banner. */
  keychainUnreadable: boolean;
  /** Last discovery of the account's server. */
  server: ServerDiscovery | null;
  /** The last profile refresh couldn't reach the server ("Offline · showing what was last synced"). */
  offline: boolean;
  signIn: SignInStatus;
}

/** One device (session) of the account (AC-AUTH-10). */
export interface SessionInfo {
  id: string;
  deviceName: string;
  createdAt: number | null;
  /** Unix ms of the day it was last used. */
  lastUsedDay: number | null;
  /** This device. */
  current: boolean;
}

/** `settings.json` → `knownServers[]`: suggestions and per-origin choices (not secret). */
export interface KnownServer {
  /** Normalised origin. */
  url: string;
  name?: string;
  lastUsedAt: number;
  /** "Continue anyway" was checked for this plain-http origin. */
  insecureAck?: boolean;
  /** "Keep what I typed" was chosen over the server's publicUrl. */
  keepTyped?: boolean;
}

// ───────────────────────────── Teams (04 §5.3, 06-go-server §10 M2) ─────────────────────────────

export type TeamRole = "owner" | "admin" | "editor" | "viewer";

/** `teams_list`: my teams, how many I own and the server's limits (`null` = no cap). */
export interface TeamsOverview {
  teams: TeamSummary[];
  ownedCount: number | null;
  limits: { ownedTeams: number | null; teamMembers: number | null; teamInvites: number | null };
  /** What I'm billed for: personal space plus the teams I own. */
  owner: { bytesUsed: number | null; quotaBytes: number | null } | null;
}

/** A user reference; `name` null = a former member. */
export interface UserRef {
  userId: string;
  name: string | null;
}

/** `teams_list` row (personal teams are never listed). */
export interface TeamSummary {
  teamId: string;
  name: string;
  /** My role. */
  role: TeamRole | (string & {});
  spaceId: string | null;
  memberCount: number | null;
  bytesUsed: number | null;
  quotaBytes: number | null;
  /** The billing owner: can't be demoted or removed before a transfer (DEV-8). */
  ownerUserId: string | null;
  createdAt: number | null;
}

export interface TeamMember {
  userId: string;
  name: string;
  email: string;
  role: TeamRole | (string & {});
  joinedAt: number | null;
}

/** An active invite link (the token is shown only once, at creation). */
export interface InviteInfo {
  inviteId: string;
  role: TeamRole | (string & {});
  /** null = unlimited. */
  maxUses: number | null;
  uses: number;
  expiresAt: number | null;
  createdAt: number | null;
  createdBy: UserRef | null;
}

export interface CreatedInvite {
  invite: InviteInfo;
  /** `https://<server>/i#<token>`: copy it now, it can't be shown again. */
  url: string;
}

/** `team_get`: invites are listed for admins and owners only (null otherwise). */
export interface TeamDetail {
  team: TeamSummary;
  members: TeamMember[];
  invites: InviteInfo[] | null;
  /** My user id. */
  me: string;
}

export interface InvitePreview {
  teamId: string | null;
  teamName: string;
  inviterName: string | null;
  role: TeamRole | (string & {});
  expiresAt: number | null;
}

/** `invite_preview`: which server the link belongs to, and its team. */
export interface InviteCheck {
  serverUrl: string;
  /** Signed in to another server: offer "Sign in to <host>". */
  otherServer: boolean;
  preview: InvitePreview;
}

export interface JoinResult {
  teamId: string;
  teamName: string;
  role: TeamRole | (string & {});
  spaceId: string | null;
  /** Already a member: "Your role hasn't changed." */
  alreadyMember: boolean;
  /** Team projects on this device after the first sync ("syncing 2 projects"). */
  projects: number;
}

/** One credential typed as plain text (the Move / Copy pre-flight, UX §9). */
export interface CredentialRow {
  requestId: string;
  requestName: string;
  /** "Authorization header", "Basic auth password", "Bearer token", "API key", "Cookie header", … */
  field: string;
  /** Where to fix it. */
  /** "collection": the collection's auth (requestId/requestName are the collection's). "request": fields from a newer ReqHarbor. */
  tab: "headers" | "auth" | "ws" | "collection" | "request";
}

/** The Move / Copy pre-flight (UX §9). */
export interface TransferPreview {
  name: string;
  targetName: string;
  targetKind: "personal" | "team" | "legacy" | "local";
  memberCount: number | null;
  items: number;
  /** Secret variables: never uploaded, each member sets their own value. */
  secretVariables: number;
  /** Binary bodies, form files and gRPC .proto files / import paths: paths that stay on this device. */
  filePaths: number;
  credentials: CredentialRow[];
  /** Copy mode: variables the target project's environments don't define. */
  unresolvedVariables: string[];
  /** "Existing share links stay with your personal space and keep working." */
  sharesStay: boolean;
}

export interface MoveResult {
  moveId: string;
  /** The moved copy's (new) project id. */
  projectId: string;
  items: number;
}

/** Payload of `sync://move-progress`: "Moving… 42 of 120 items". */
export interface MoveProgress {
  projectId: string;
  done: number;
  total: number;
}

export interface CopyResult {
  /** The target project's tree. */
  tree: ProjectTree;
  copyId: string;
}

/** "Edited by Sam · 3h" (UX §6.5). */
export interface EditedBy {
  userId: string;
  /** "Former member" when the account no longer exists. */
  name: string;
  updatedAt: number;
}

export interface RoleChangeResult {
  status: SyncStatus;
  /** Personal copies made by "savePersonal". */
  copies: string[];
}

// ───────────────────────────── WebSocket (AC-WS-*) ─────────────────────────────

/**
 * An ordered per-call stream from Rust (a Tauri 2 `ipc::Channel`). Create it with
 * `backend.channel(handler)` and pass it as a command argument.
 */
export interface EventChannel<T> {
  onmessage: (message: T) => void;
}

export type WsFrameKind =
  | "text"
  | "binary"
  | "ping"
  | "pong"
  | "close"
  /** Socket.IO event: `event` = name, `data` = JSON array of its arguments. */
  | "event"
  /** Socket.IO acknowledgement: `ackId`, `data` = JSON array of its arguments. */
  | "ack"
  | "error"
  /** system: connected (`data` = summary, e.g. "Connected to ws://… · subprotocol chat"). */
  | "open"
  /** system: the connection dropped and a reconnect is scheduled (autoReconnect). */
  | "reconnecting"
  /** system: any other note. */
  | "info";

/** One message-log entry (ws_connect's channel), delivered in order. */
export interface WsFrame {
  dir: "in" | "out" | "system";
  /** Unix epoch ms. */
  ts: number;
  kind: WsFrameKind;
  /** Text; base64 for binary / ping / pong payloads; JSON args for events and acks; a sentence for system frames. */
  data: string;
  /** Payload bytes (0 for system frames). */
  size: number;
  event?: string;
  ackId?: number;
  /** Close code (close frames and the final system close). */
  code?: number;
  /** `data` was cut at 256 KB (`size` is the whole payload). */
  truncated?: boolean;
  /** The final system "close": nothing follows on this connection (history is written). */
  last?: boolean;
}

// ───────────────────────────── gRPC (AC-GRPC-*) ─────────────────────────────

export type GrpcFrameKind =
  /** in / out: one message; `data` = its JSON (every field for received ones). */
  | "message"
  /** in: response headers; `data` = "name: value" lines. */
  | "headers"
  /** in: trailers; `data` = "name: value" lines. */
  | "trailers"
  /** out: the sending side was closed (End stream, or automatically for unary / server streaming). */
  | "end"
  /** system: the final status; `code`, `data` = "OK" or "NOT_FOUND: message", `details`. */
  | "status"
  /** system: connected (`data` = e.g. "Connected to grpcs://host:443 · demo.Greeter/Chat (bidi streaming)"). */
  | "open"
  | "error"
  | "info"
  /** system, always last (`last: true`): "Finished · 0 OK", "Failed · 14 UNAVAILABLE", "Connection failed", "Cancelled". */
  | "close";

/** One message-log entry of a gRPC stream (grpc_start's channel), delivered in order. Same shape as WsFrame. */
export interface GrpcFrame {
  dir: "in" | "out" | "system";
  /** Unix epoch ms. */
  ts: number;
  kind: GrpcFrameKind;
  data: string;
  /** Encoded message bytes for messages; 0 otherwise. */
  size: number;
  /** status / close: the gRPC status code. */
  code?: number;
  /** status: decoded status details. */
  details?: GrpcStatusDetail[];
  /** `data` was cut at 256 KB (`size` is the whole message). */
  truncated?: boolean;
  /** The final system "close": nothing follows on this call (history is written). */
  last?: boolean;
}

export interface GrpcRequestArgs {
  /** The request as edited (kind "grpc"). Never auto-saved. */
  request: Request;
  projectId?: string | null;
}

/** grpc_cached_descriptor result. */
export interface GrpcDescriptorLookup {
  /** The request's descriptor URL (cache key). */
  url: string;
  /** The cached descriptor is the `known` one: `descriptor` is left out. */
  unchanged: boolean;
  descriptor: GrpcDescriptor | null;
}

export interface GrpcInvokeArgs extends GrpcRequestArgs {
  /** Client-generated id: cancel_request(executionId) cancels the call. */
  executionId: string;
}

export interface GrpcInvokeResult {
  executionId: string;
  historyId: string;
  callKind: GrpcCallKind;
  /** e.g. "grpcs://api.example.com:443/demo.Greeter/SayHello" (secret values shown as {{name}}). */
  resolvedUrl: string;
  unresolvedVariables: string[];
  /** The call got a status (any code). Exactly one of status / error is non-null. */
  status: GrpcStatus | null;
  /** DNS, connect, TLS, timeout (connecting) or cancelled. */
  error: SendError | null;
  /** The response message as pretty JSON (every field); null without one. */
  response: string | null;
  /** Names lowercase, received order. */
  headers: [string, string][];
  trailers: [string, string][];
  timeMs: number;
  /** Encoded response message bytes. */
  sizeBytes: number;
}

export interface GrpcStartArgs extends GrpcRequestArgs {
  /** Client-generated id; re-using one cancels the previous call. */
  connectionId: string;
  onEvent: EventChannel<GrpcFrame>;
}

export interface GrpcStartResult {
  connectionId: string;
  /** History entry written when the call ends (after the `last` frame). */
  historyId: string;
  callKind: GrpcCallKind;
  resolvedUrl: string;
  unresolvedVariables: string[];
}

export interface WsConnectArgs {
  /** Client-generated id; re-using one closes the previous connection. */
  connectionId: string;
  /** The request as edited (kind "ws"). Never auto-saved. */
  request: Request;
  projectId?: string | null;
  onEvent: EventChannel<WsFrame>;
}

export interface WsConnectResult {
  connectionId: string;
  /** History entry written when the session ends (after the `last` frame). */
  historyId: string;
  method: "WS" | "SIO";
  /** URL being opened (secret values shown as {{name}}). */
  resolvedUrl: string;
  unresolvedVariables: string[];
}

// ───────────────────────────── Events ─────────────────────────────

export interface Events {
  /** Sync indicator state changed (AC-SYNC-2/6/7). */
  "sync://status": SyncStatus;
  /** A one-off message to toast, e.g. the fresh-device sign-in (UX §10). */
  "sync://notice": SyncNotice;
  /** A pull changed local data: reload project trees + environments; toast conflicts. */
  "sync://changed": SyncReport;
  /** Sign-in progress, sign-out, session ended (AC-AUTH-9). */
  "auth://status": AuthStatus;
  /** A project move pushing its copy (UX §9 progress). */
  "sync://move-progress": MoveProgress;
}

// ───────────────────────────── Command map ─────────────────────────────

/** `invoke(name, args)` signatures. Optional args may be omitted or null. */
export interface Commands {
  bootstrap: { args: {}; result: Bootstrap };
  get_settings: { args: {}; result: Settings };
  save_settings: { args: { settings: Settings }; result: Settings };

  list_projects: { args: {}; result: Project[] };
  get_project_tree: { args: { projectId: string }; result: ProjectTree };
  /** `location`: omitted/null = default (Personal when signed in), "local" = this device only, or a space id. */
  create_project: { args: { name: string; location?: string | null }; result: ProjectTree };
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
  /** Resolves once the connection is being opened; everything else arrives on `onEvent` (AC-WS-2/3). */
  ws_connect: { args: WsConnectArgs; result: WsConnectResult };
  /** Name is ignored. Rejects with "ws_not_connected" or "ws_invalid_message". */
  ws_send: { args: { connectionId: string; message: SavedMessage }; result: { ackId: number | null } };
  /** WebSocket ping frame; false when not connected. */
  ws_ping: { args: { connectionId: string }; result: boolean };
  /** Close (or stop connecting). `code`: 1000 or 3000–4999 (else "invalid"); reason ≤ 123 bytes. False when nothing was open. */
  ws_close: { args: { connectionId: string; code?: number | null; reason?: string | null }; result: boolean };
  cancel_request: { args: { executionId: string }; result: boolean };

  /**
   * AC-GRPC-3/4: the services without a network call. Reflection: the cached descriptor for the request's origin
   * (null when none); files: compiles the .proto files (rejects with grpc_proto_invalid). `known` = the one shown.
   */
  grpc_cached_descriptor: { args: GrpcRequestArgs & { known?: { url: string; fetchedAt: number } | null }; result: GrpcDescriptorLookup };
  /**
   * Reflection: asks the server (v1, then v1alpha), validates and caches the result; files: compiles them.
   * Cancellable with cancel_request(executionId); not in history. Rejects with validation / read_only_newer or the grpc_* codes.
   */
  grpc_fetch_descriptor: { args: GrpcRequestArgs & { executionId?: string | null }; result: GrpcDescriptor };
  /** Forget the cached reflection result of the request's origin; true when there was one. */
  grpc_clear_descriptor: { args: GrpcRequestArgs; result: boolean };
  /** AC-GRPC-7: one unary call (any call kind works: streams are reduced to one message each way). cancel_request(executionId) cancels it. */
  grpc_invoke: { args: GrpcInvokeArgs; result: GrpcInvokeResult };
  /** AC-GRPC-8: start a call; frames arrive on `onEvent`. Server streaming (and unary) send `grpc.message` and end at once. */
  grpc_start: { args: GrpcStartArgs; result: GrpcStartResult };
  /** Send one message (JSON text; variables resolve, fresh {{$…}}). Rejects with grpc_not_connected / grpc_invalid_message. */
  grpc_send: { args: { connectionId: string; message: string }; result: null };
  /** Close the sending side (half-close). False when nothing is open. */
  grpc_end: { args: { connectionId: string }; result: boolean };
  /** Cancel the call (CANCELLED). False when nothing is open. */
  grpc_cancel: { args: { connectionId: string }; result: boolean };

  /**
   * AC-GQL-2..4: POST the introspection query with the request's URL, params, headers, auth, settings and variables;
   * cache the schema for the schema URL unless `store` is false (the UI passes false, builds it, then calls
   * graphql_store_schema). Not recorded in history. `executionId` makes it cancellable with cancel_request.
   * Rejects with validation / read_only_newer (like send_request) or the graphql_* codes.
   */
  graphql_fetch_schema: { args: GraphqlSchemaArgs & { executionId?: string | null; store?: boolean | null }; result: GraphqlSchema };
  /** The cached schema for the request's schema URL. Never sends. `known` = the schema already shown. */
  graphql_cached_schema: { args: GraphqlSchemaArgs & { known?: { url: string; fetchedAt: number } | null }; result: GraphqlCacheLookup };
  /** AC-GQL-5: SDL or introspection JSON from `path` (or `text`, browser mode); cached unless `store` is false. */
  graphql_load_schema_file: {
    args: GraphqlSchemaArgs & { path?: string | null; text?: string | null; fileName?: string | null; store?: boolean | null };
    result: GraphqlSchema;
  };
  /** Cache a schema the UI has built successfully under `schema.url`; nothing is cached when the request's schema URL has changed since. */
  graphql_store_schema: { args: GraphqlSchemaArgs & { schema: GraphqlSchema }; result: GraphqlSchema };
  /** Forget the cached schema for the request's resolved URL; true when there was one. */
  graphql_clear_schema: { args: GraphqlSchemaArgs; result: boolean };
  get_response_body: { args: { responseId: string; limit?: number | null }; result: ArrayBuffer };
  save_response_body: { args: { responseId: string; path: string }; result: null };

  format_body: { args: { kind: "json" | "xml"; text: string }; result: string };
  url_to_params: { args: { url: string; params: KeyValue[] }; result: KeyValue[] };
  /** AC-GRPC-11: a grpcurl command for a gRPC request (validation `notGrpc` for others). */
  export_grpcurl: { args: { request: Request; projectId?: string | null; keepVariables: boolean }; result: string };
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
  share_revoke: { args: { shareId: string; spaceId?: string | null }; result: null };
  share_list: { args: {}; result: ShareInfo[] };
  import_share: { args: { url: string; projectId: string }; result: ShareImportResult };
  preview_share: {
    args: { url: string };
    result: { kind: ShareKind; snapshot: ShareSnapshot; warnings: string[] };
  };

  /** Rejects with "invalid" for an address with a path ("Enter the server's address without a path"). */
  server_discover: { args: { serverUrl: string }; result: ServerDiscovery };
  auth_status: { args: {}; result: AuthStatus };
  /** Resolves once the browser is open (signIn.state "waiting"); the rest arrives on `auth://status`. */
  /** `inviteTicket` (from invite_ticket) lets an invitee without an account sign up on an invite-only server. */
  auth_sign_in_start: {
    args: { serverUrl: string; method?: string | null; insecureConfirmed?: boolean | null; inviteTicket?: string | null };
    result: AuthStatus;
  };
  auth_sign_in_reopen: { args: {}; result: null };
  auth_sign_in_cancel: { args: {}; result: AuthStatus };
  auth_sign_out: { args: {}; result: AuthStatus };
  auth_me: { args: {}; result: AuthStatus };
  auth_sessions: { args: {}; result: SessionInfo[] };
  /** Another device's session; this device signs out with auth_sign_out. */
  auth_session_revoke: { args: { sessionId: string }; result: null };

  /** Where every project syncs (account mode; pure-legacy: "legacy" when token sync is on, else "local"). */
  project_locations: { args: {}; result: ProjectLocation[] };
  /** Sync location… (B1): a space id, or null for this device only. Team moves: move_project_to_space. */
  project_set_location: { args: { projectId: string; spaceId: string | null }; result: ProjectLocation[] };
  first_sync_candidates: { args: {}; result: FirstSyncCandidate[] };
  /** The checked projects sync to Personal; the rest stay on this device (also "Keep all": `[]`). */
  first_sync_apply: { args: { projectIds: string[] }; result: SyncStatus };
  /**
   * SpaceNoticeDialog: keep its projects as local-only copies, or remove them from this device.
   * `saveChanges`: first copy projects with unsynced edits to Personal ("X (my changes)", edge case 3).
   */
  space_notice_resolve: { args: { spaceId: string; keep: boolean; saveChanges?: boolean | null }; result: SyncStatus };
  /** × on the space notice ("decide later", UX §6.9): Keep applies by itself after 7 days. */
  space_notice_later: { args: { spaceIds: string[] }; result: SyncStatus };
  /** Merge the token-synced workspace into Personal (same ids), then sync. Safe to repeat. */
  legacy_merge: { args: {}; result: LegacyMergeResult };
  /** Forget the legacy token; its share links keep working on the server. */
  legacy_disconnect: { args: {}; result: SyncStatus };

  // ── teams (only when the server advertises features.teams; else "teams_disabled") ──
  teams_list: { args: {}; result: TeamsOverview };
  /** 1–80 characters; 409 "limit_teams". Syncs so the team's space binds. */
  team_create: { args: { name: string }; result: TeamSummary };
  team_get: { args: { teamId: string }; result: TeamDetail };
  /** Admin. */
  team_rename: { args: { teamId: string; name: string }; result: null };
  /** Billing owner. This device keeps local copies of the team's projects (D5). */
  team_delete: { args: { teamId: string }; result: SyncStatus };
  /** Rejects with "forbidden_role", "last_owner" or "transfer_required". */
  team_set_role: { args: { teamId: string; userId: string; role: TeamRole }; result: null };
  team_remove_member: { args: { teamId: string; userId: string; revokeInvites: boolean }; result: null };
  /** `keepCopy`: its projects stay here as "X (from Acme)"; else they're removed from this device. */
  team_leave: { args: { teamId: string; keepCopy: boolean }; result: SyncStatus };
  /** Billing owner; 413 "quota_exceeded" when the receiver has no room, 409 "limit_teams". */
  team_transfer: { args: { teamId: string; userId: string }; result: null };
  /** `maxUses` null = unlimited; admin links only by an owner and single-use; `expiresInDays` 1, 7 or 30. */
  invite_create: { args: { teamId: string; role: TeamRole; maxUses: number | null; expiresInDays: number }; result: CreatedInvite };
  invite_list: { args: { teamId: string }; result: InviteInfo[] };
  invite_revoke: { args: { teamId: string; inviteId: string }; result: null };
  /**
   * No sign-in needed. Rejects with "invite_not_a_link", "invite_invalid", or "insecure_unconfirmed"
   * for a plain-http server until `insecureConfirmed` (or a remembered "Continue anyway").
   */
  invite_preview: { args: { url: string; insecureConfirmed?: boolean | null }; result: InviteCheck };
  /** Joins, then syncs so the team's projects arrive. */
  invite_accept: { args: { url: string }; result: JoinResult };
  /** A 10-minute signup ticket for `auth_sign_in_start` (signed-out invitees). */
  invite_ticket: { args: { url: string; insecureConfirmed?: boolean | null }; result: string };

  // ── moves and copies (new ids, 04 §6.5) ──
  /** `spaceId` null = this device only. */
  move_preview: { args: { projectId: string; spaceId: string | null }; result: TransferPreview };
  /** Online and resumable: on failure nothing moved; calling it again resumes with the same ids. */
  move_project_to_space: { args: { projectId: string; spaceId: string | null }; result: MoveResult };
  /** Undo what an interrupted move already pushed, and forget it (finishes it instead when its copy is already saved). */
  move_cancel: { args: { projectId: string }; result: null };
  copy_preview: { args: { kind: NodeKind; id: string; targetProjectId: string }; result: TransferPreview };
  /** Without `targetCollectionId` a request or folder lands in a new collection named after its own. */
  copy_to_project: {
    args: { kind: NodeKind; id: string; targetProjectId: string; targetCollectionId?: string | null; targetFolderId?: string | null };
    result: CopyResult;
  };
  /** Duplicate to personal… for a whole project (`spaceId` null = this device only). */
  project_duplicate_to: { args: { projectId: string; spaceId: string | null; name?: string | null }; result: ProjectTree };
  /** Who last changed a team project's request; null in personal/local projects or when it was me. */
  request_edited_by: { args: { requestId: string }; result: EditedBy | null };
  /** The role-changed banner (UX §6.8). */
  role_change_resolve: { args: { spaceId: string; action: "savePersonal" | "discard" }; result: RoleChangeResult };
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

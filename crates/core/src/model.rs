//! Data model (PRODUCT.md §3). All structs serialize with camelCase keys so the
//! JSON on disk, over Tauri IPC and on the sync wire is identical.

use serde::{Deserialize, Serialize};

/// File-level schema version written into every persisted JSON file.
pub const SCHEMA_VERSION: u32 = 1;

/// This build's version, compared against an entity's `minClientVersion`
/// and sent as `X-ReqHarbor-Version` on sync calls.
pub const CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Unknown JSON keys preserved on read and written back unchanged
/// (forward compatibility, AC-FWD-1). Never holds a key the struct itself
/// declares: serde hands known keys to their fields first.
pub type Extra = serde_json::Map<String, serde_json::Value>;

/// A string-backed enum whose unknown values (written by a newer ReqHarbor)
/// are kept verbatim in `Other` and serialized back byte-for-byte.
macro_rules! open_string_enum {
    (
        $(#[$meta:meta])*
        pub enum $name:ident {
            $( $(#[$vmeta:meta])* $variant:ident = $s:literal, )+
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
        pub enum $name {
            $( $(#[$vmeta])* $variant, )+
            /// A value this build does not know; round-trips unchanged.
            Other(String),
        }

        impl $name {
            /// The wire string.
            pub fn as_str(&self) -> &str {
                match self {
                    $( $name::$variant => $s, )+
                    $name::Other(s) => s.as_str(),
                }
            }
            /// Parse a wire string; unknown values become `Other`.
            pub fn parse(s: &str) -> Self {
                match s {
                    $( $s => $name::$variant, )+
                    other => $name::Other(other.to_string()),
                }
            }
            /// False for `Other` (a value from a newer client).
            pub fn is_known(&self) -> bool {
                !matches!(self, $name::Other(_))
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = std::borrow::Cow::<'de, str>::deserialize(d)?;
                Ok($name::parse(&s))
            }
        }
    };
}

open_string_enum! {
    /// What a request talks (AC-FWD-2). `Http`, `Ws` (WebSocket, with
    /// Socket.IO as a mode of it) and `Grpc` are editable in this build;
    /// `Graphql` stays reserved (GraphQL is a body type, PRODUCT §5.14).
    /// Omitted from JSON when `Http`, so older readers see no change.
    pub enum RequestKind {
        #[default]
        Http = "http",
        Graphql = "graphql",
        Grpc = "grpc",
        Ws = "ws",
    }
}

impl RequestKind {
    pub fn is_http(&self) -> bool {
        *self == RequestKind::Http
    }
    pub fn is_ws(&self) -> bool {
        *self == RequestKind::Ws
    }
    pub fn is_grpc(&self) -> bool {
        *self == RequestKind::Grpc
    }
    /// Kinds this build can edit and run: HTTP, WebSocket and gRPC.
    pub fn is_supported(&self) -> bool {
        matches!(
            self,
            RequestKind::Http | RequestKind::Ws | RequestKind::Grpc
        )
    }
}

/// Parse `major.minor.patch` (missing parts are 0; a leading `v` and any
/// `-pre`/`+build` suffix are ignored).
fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let core = v.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let mut next = || -> Option<u64> {
        match parts.next() {
            None => Some(0),
            Some(p) => p.parse().ok(),
        }
    };
    let out = (next()?, next()?, next()?);
    parts.next().is_none().then_some(out)
}

/// Whether `required` is newer than this build. Unparseable versions count
/// as newer, so an entity we cannot reason about is never modified.
pub fn requires_newer_client(required: &str) -> bool {
    match (parse_version(required), parse_version(CLIENT_VERSION)) {
        (Some(req), Some(cur)) => req > cur,
        _ => true,
    }
}

/// Whether an entity's unknown keys carry a `minClientVersion` newer than
/// this build (AC-FWD-4). Containers (projects, collections, folders,
/// environments) keep the key in `extra`; a value that isn't a string or
/// `null` counts as newer, so an entity we cannot reason about is never
/// modified.
pub fn extra_requires_newer(extra: &Extra) -> bool {
    match extra.get("minClientVersion") {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::String(v)) => requires_newer_client(v),
        Some(_) => true,
    }
}

/// Generate a new ULID string id.
pub fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

/// Current Unix time in epoch milliseconds.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Methods offered in the dropdown (AC-REQ-1). Any other method must match
/// [`is_valid_method`].
pub const STANDARD_METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "QUERY", "HEAD", "OPTIONS",
];

/// `^[A-Z][A-Z0-9_-]*$` (AC-REQ-1).
pub fn is_valid_method(method: &str) -> bool {
    let mut chars = method.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    pub worker_url: String,
    pub remote_workspace_id: String,
    pub token: String,
    /// Highest server `rev` seen by the last pull.
    #[serde(default)]
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub active_environment_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Project {
    /// False when the project carries a `minClientVersion` newer than this
    /// build (AC-FWD-4): it is read-only here and never pushed modified.
    pub fn is_editable(&self) -> bool {
        !extra_requires_newer(&self.extra)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: String,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Collection {
    /// False when the collection's auth comes from a newer ReqHarbor: its
    /// auth can't be changed here and isn't applied to inheriting requests.
    pub fn auth_is_known(&self) -> bool {
        self.auth.is_known()
    }
    /// False when the collection carries a `minClientVersion` newer than
    /// this build (AC-FWD-4): it is read-only here and never pushed
    /// modified. Unknown auth alone keeps the rest of it editable.
    pub fn is_editable(&self) -> bool {
        !extra_requires_newer(&self.extra)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub collection_id: String,
    #[serde(default)]
    pub parent_folder_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Folder {
    /// False when the folder carries a `minClientVersion` newer than this
    /// build (AC-FWD-4).
    pub fn is_editable(&self) -> bool {
        !extra_requires_newer(&self.extra)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    /// `http` unless written by a newer client (AC-FWD-2). Omitted when `http`.
    #[serde(default, skip_serializing_if = "RequestKind::is_http")]
    pub kind: RequestKind,
    /// Oldest ReqHarbor that may edit this request (AC-FWD-4).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client_version: Option<String>,
    /// Owning collection. Empty string for an unsaved "scratch" request.
    #[serde(default)]
    pub collection_id: String,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub params: Vec<KeyValue>,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub settings: RequestSettings,
    /// WebSocket / Socket.IO settings (kind `ws`). Kept when the kind is
    /// switched back to HTTP, so switching again loses nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws: Option<WsConfig>,
    /// gRPC settings (kind `grpc`, AC-GRPC-*). Kept when the kind is
    /// switched back to HTTP.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<GrpcConfig>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_method() -> String {
    "GET".to_string()
}

impl Default for Request {
    fn default() -> Self {
        Request {
            id: new_id(),
            kind: RequestKind::Http,
            min_client_version: None,
            collection_id: String::new(),
            folder_id: None,
            name: String::new(),
            method: default_method(),
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            auth: Auth::default(),
            body: Body::default(),
            settings: RequestSettings::default(),
            ws: None,
            grpc: None,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Extra::new(),
        }
    }
}

/// Message shown when a request from a newer ReqHarbor is sent or edited.
pub const NEEDS_NEWER_VERSION: &str = "This request needs a newer version of ReqHarbor";

impl Request {
    /// False when this build cannot faithfully edit or send the request: a
    /// kind other than HTTP, WebSocket or gRPC, an unknown body type, form
    /// field kind, auth scheme, API key location, WebSocket message kind,
    /// Socket.IO version or gRPC proto source, or a `minClientVersion` newer
    /// than this build
    /// (AC-FWD-3/4). Such requests are read-only, are never sent, and a
    /// locally modified version is never pushed.
    pub fn is_editable(&self) -> bool {
        self.kind.is_supported()
            && self.body.is_known()
            && self.auth.is_known()
            && self.ws.as_ref().is_none_or(WsConfig::is_known)
            && self.grpc.as_ref().is_none_or(GrpcConfig::is_known)
            && !self
                .min_client_version
                .as_deref()
                .is_some_and(requires_newer_client)
    }

    /// A WebSocket or gRPC request must not be edited by a ReqHarbor that
    /// predates its kind (an older client would drop `ws` / `grpc` on save):
    /// stamp `minClientVersion` with [`WS_MIN_CLIENT_VERSION`] /
    /// [`GRPC_MIN_CLIENT_VERSION`] unless an equal or newer one is already
    /// there. Never the running version, so a newer patch build doesn't lock
    /// out older builds that support the kind (AC-WS-1, AC-GRPC-1).
    pub fn stamp_min_client_version(&mut self) {
        let min = match self.kind {
            RequestKind::Ws => WS_MIN_CLIENT_VERSION,
            RequestKind::Grpc => GRPC_MIN_CLIENT_VERSION,
            _ => return,
        };
        let older = match self.min_client_version.as_deref() {
            None => true,
            Some(v) => match (parse_version(v), parse_version(min)) {
                (Some(have), Some(min)) => have < min,
                _ => false,
            },
        };
        if older {
            self.min_client_version = Some(min.to_string());
        }
    }
}

/// The first ReqHarbor that edits and runs WebSocket / Socket.IO requests:
/// the `minClientVersion` stamped on every `ws` request (AC-WS-1). Fixed;
/// raise it only when the `ws` format changes in a way older builds would
/// lose on save.
pub const WS_MIN_CLIENT_VERSION: &str = "0.1.0";

/// The first ReqHarbor that edits and runs gRPC requests: the
/// `minClientVersion` stamped on every `grpc` request (AC-GRPC-1). Fixed;
/// builds before gRPC support already treat the kind as read-only, so raise
/// it only when the `grpc` format changes in a way older gRPC-capable builds
/// would lose on save.
pub const GRPC_MIN_CLIENT_VERSION: &str = "0.1.0";

open_string_enum! {
    /// Where a gRPC request's services come from. Unknown values round-trip
    /// and make the request read-only (AC-FWD-3).
    pub enum ProtoSource {
        #[default]
        Reflection = "reflection",
        Files = "files",
    }
}

/// gRPC settings of a `grpc` request (AC-GRPC-*). The request's headers are
/// the metadata, its auth applies as metadata, `settings.timeoutMs` is the
/// unary deadline and `settings.verifyTls` applies; TLS follows the URL
/// scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcConfig {
    /// Full service name (`demo.Greeter`); empty until a method is picked.
    #[serde(default)]
    pub service: String,
    /// Method name (`SayHello`).
    #[serde(default)]
    pub method: String,
    /// The request message as JSON text; empty counts as `{}`.
    #[serde(default = "default_grpc_message")]
    pub message: String,
    #[serde(default)]
    pub proto_source: ProtoSource,
    /// Device-local `.proto` paths (AC-GRPC-4).
    #[serde(default)]
    pub proto_files: Vec<String>,
    /// Device-local import folders.
    #[serde(default)]
    pub import_paths: Vec<String>,
    /// Saved messages, in user order.
    #[serde(default)]
    pub messages: Vec<GrpcSavedMessage>,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_grpc_message() -> String {
    "{}".to_string()
}

impl Default for GrpcConfig {
    fn default() -> Self {
        GrpcConfig {
            service: String::new(),
            method: String::new(),
            message: default_grpc_message(),
            proto_source: ProtoSource::Reflection,
            proto_files: Vec::new(),
            import_paths: Vec::new(),
            messages: Vec::new(),
            extra: Extra::new(),
        }
    }
}

impl GrpcConfig {
    /// False when the proto source comes from a newer ReqHarbor.
    pub fn is_known(&self) -> bool {
        self.proto_source.is_known()
    }
    /// Device-local paths (proto files and import folders) it holds.
    pub fn local_paths(&self) -> usize {
        self.proto_files
            .iter()
            .chain(&self.import_paths)
            .filter(|p| !p.trim().is_empty())
            .count()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcSavedMessage {
    #[serde(default)]
    pub name: String,
    /// JSON text.
    #[serde(default)]
    pub body: String,
    #[serde(flatten)]
    pub extra: Extra,
}

/// Socket.IO protocol version this build speaks (Engine.IO v4 transport).
pub const SOCKET_IO_VERSION: u32 = 4;

/// WebSocket settings of a `ws` request. Headers, auth, query params and
/// settings of the request apply to the opening handshake.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsConfig {
    /// Saved messages, in user order.
    #[serde(default)]
    pub messages: Vec<SavedMessage>,
    /// `Sec-WebSocket-Protocol` values offered on connect.
    #[serde(default)]
    pub protocols: Vec<String>,
    /// Set = Socket.IO mode (Engine.IO v4 over the WebSocket).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub socket_io: Option<SocketIoConfig>,
    /// Reconnect after the connection drops (not after Disconnect).
    #[serde(default)]
    pub auto_reconnect: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl WsConfig {
    /// False when a message kind or the Socket.IO version comes from a newer
    /// ReqHarbor (AC-FWD-3).
    pub fn is_known(&self) -> bool {
        self.messages.iter().all(|m| m.kind.is_known())
            && self
                .socket_io
                .as_ref()
                .is_none_or(|s| s.version == SOCKET_IO_VERSION)
    }
}

open_string_enum! {
    /// Payload kind of a saved WebSocket message. `Binary` bodies are base64.
    /// Unknown values round-trip and make a request read-only (AC-FWD-3).
    pub enum MessageKind {
        #[default]
        Text = "text",
        Json = "json",
        Binary = "binary",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedMessage {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: MessageKind,
    /// Text, JSON text, or base64 for `binary`. Variables are resolved when
    /// it is sent.
    #[serde(default)]
    pub body: String,
    /// Socket.IO: the event name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    /// Socket.IO: ask the server for an acknowledgement.
    #[serde(default, skip_serializing_if = "is_false")]
    pub ack: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SocketIoConfig {
    /// Engine.IO endpoint path, default `/socket.io/`.
    #[serde(default = "default_sio_path")]
    pub path: String,
    /// Namespace joined after the handshake, default `/`.
    #[serde(default = "default_sio_namespace")]
    pub namespace: String,
    #[serde(default = "default_sio_version")]
    pub version: u32,
    /// Optional JSON object sent as the CONNECT packet's auth payload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_sio_path() -> String {
    "/socket.io/".to_string()
}
fn default_sio_namespace() -> String {
    "/".to_string()
}
fn default_sio_version() -> u32 {
    SOCKET_IO_VERSION
}

impl Default for SocketIoConfig {
    fn default() -> Self {
        SocketIoConfig {
            path: default_sio_path(),
            namespace: default_sio_namespace(),
            version: SOCKET_IO_VERSION,
            auth: None,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        KeyValue {
            key: key.into(),
            value: value.into(),
            enabled: true,
            description: None,
            extra: Extra::new(),
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

fn default_true() -> bool {
    true
}

open_string_enum! {
    /// Auth scheme. An unknown value (from a newer client, e.g. `oauth2`) is
    /// kept in `Other` and makes a request read-only (AC-FWD-3).
    pub enum AuthType {
        #[default]
        None = "none",
        Inherit = "inherit",
        Basic = "basic",
        Bearer = "bearer",
        Apikey = "apikey",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    #[serde(rename = "type")]
    pub auth_type: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyAuth>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Auth {
    /// False when the scheme or the API key location comes from a newer
    /// ReqHarbor: this build cannot apply or edit such auth faithfully.
    pub fn is_known(&self) -> bool {
        self.auth_type.is_known() && self.api_key.as_ref().is_none_or(|k| k.location.is_known())
    }
    pub fn none() -> Self {
        Auth::default()
    }
    pub fn inherit() -> Self {
        Auth {
            auth_type: AuthType::Inherit,
            ..Auth::default()
        }
    }
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth {
            auth_type: AuthType::Basic,
            username: Some(username.into()),
            password: Some(password.into()),
            ..Auth::default()
        }
    }
    pub fn bearer(token: impl Into<String>) -> Self {
        Auth {
            auth_type: AuthType::Bearer,
            token: Some(token.into()),
            ..Auth::default()
        }
    }
    pub fn api_key(
        key: impl Into<String>,
        value: impl Into<String>,
        location: ApiKeyLocation,
    ) -> Self {
        Auth {
            auth_type: AuthType::Apikey,
            api_key: Some(ApiKeyAuth {
                key: key.into(),
                value: value.into(),
                location,
                extra: Extra::new(),
            }),
            ..Auth::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuth {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(rename = "in", default)]
    pub location: ApiKeyLocation,
    #[serde(flatten)]
    pub extra: Extra,
}

open_string_enum! {
    /// Where an API key goes. Unknown values (e.g. `cookie`) round-trip and
    /// make a request read-only (AC-FWD-3).
    pub enum ApiKeyLocation {
        #[default]
        Header = "header",
        Query = "query",
    }
}

open_string_enum! {
    /// Body editor type. An unknown value (from a newer client) is kept in
    /// `Other` and makes the request read-only (AC-FWD-3).
    pub enum BodyType {
        #[default]
        None = "none",
        Json = "json",
        Xml = "xml",
        Text = "text",
        Form = "form",
        FormEncoded = "formEncoded",
        Graphql = "graphql",
        Binary = "binary",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    #[serde(rename = "type")]
    pub body_type: BodyType,
    /// Content for json / xml / text bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Multipart fields (body type `form`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Vec<FormField>>,
    /// URL-encoded fields (body type `formEncoded`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_encoded: Option<Vec<KeyValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<GraphqlBody>,
    /// File streamed as the body (body type `binary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Body {
    /// False when the body type or a multipart field kind comes from a newer
    /// ReqHarbor (AC-FWD-3).
    pub fn is_known(&self) -> bool {
        self.body_type.is_known() && self.form.iter().flatten().all(|f| f.kind.is_known())
    }
    pub fn none() -> Self {
        Body::default()
    }
    pub fn text_of(body_type: BodyType, text: impl Into<String>) -> Self {
        Body {
            body_type,
            text: Some(text.into()),
            ..Body::default()
        }
    }
    pub fn json(text: impl Into<String>) -> Self {
        Self::text_of(BodyType::Json, text)
    }
    pub fn form(fields: Vec<FormField>) -> Self {
        Body {
            body_type: BodyType::Form,
            form: Some(fields),
            ..Body::default()
        }
    }
    pub fn form_encoded(fields: Vec<KeyValue>) -> Self {
        Body {
            body_type: BodyType::FormEncoded,
            form_encoded: Some(fields),
            ..Body::default()
        }
    }
    pub fn graphql(query: impl Into<String>, variables: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Graphql,
            graphql: Some(GraphqlBody {
                query: query.into(),
                variables: variables.into(),
                operation_name: None,
                extra: Extra::new(),
            }),
            ..Body::default()
        }
    }
    pub fn binary(path: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Binary,
            binary_path: Some(path.into()),
            ..Body::default()
        }
    }
    /// Text content (empty string when absent).
    pub fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphqlBody {
    #[serde(default)]
    pub query: String,
    /// Variables as JSON text, exactly as edited. Empty means "no variables".
    #[serde(default)]
    pub variables: String,
    /// The operation to run when the document defines several (AC-GQL-9).
    /// Sent as `operationName`; omitted when unset. Older builds keep it in
    /// `extra` (AC-FWD-1) and send without it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_name: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl GraphqlBody {
    /// The operation name to send: set and not blank.
    pub fn operation(&self) -> Option<&str> {
        self.operation_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }
}

open_string_enum! {
    /// Multipart field kind. Unknown values round-trip and make a request
    /// read-only (AC-FWD-3).
    pub enum FormFieldKind {
        #[default]
        Text = "text",
        File = "file",
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub key: String,
    /// Text value, or the file path for `kind = "file"`.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub kind: FormFieldKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional part Content-Type (from cURL `;type=` suffixes, AC-CURL-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(flatten)]
    pub extra: Extra,
}

impl FormField {
    pub fn text(key: impl Into<String>, value: impl Into<String>) -> Self {
        FormField {
            key: key.into(),
            value: value.into(),
            kind: FormFieldKind::Text,
            enabled: true,
            content_type: None,
            extra: Extra::new(),
        }
    }
    pub fn file(key: impl Into<String>, path: impl Into<String>) -> Self {
        FormField {
            key: key.into(),
            value: path.into(),
            kind: FormFieldKind::File,
            enabled: true,
            content_type: None,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

fn default_timeout() -> u64 {
    30_000
}

impl Default for RequestSettings {
    fn default() -> Self {
        RequestSettings {
            follow_redirects: true,
            timeout_ms: default_timeout(),
            verify_tls: true,
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: String,
    pub workspace_id: String,
    /// `None` = the Global environment.
    #[serde(default)]
    pub project_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Environment {
    /// False when the environment carries a `minClientVersion` newer than
    /// this build (AC-FWD-4).
    pub fn is_editable(&self) -> bool {
        !extra_requires_newer(&self.extra)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub secret: bool,
    #[serde(flatten)]
    pub extra: Extra,
}

impl Variable {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Variable {
            key: key.into(),
            value: value.into(),
            enabled: true,
            secret: false,
            extra: Extra::new(),
        }
    }
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    Dns,
    Connect,
    Tls,
    Timeout,
    Cancelled,
    Other,
}

/// A typed network error (AC-REQ-9). Never a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyEncoding {
    #[default]
    Utf8,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    pub status: u16,
    pub status_text: String,
    pub time_ms: u64,
    /// Decoded body size in bytes (full size, even when `body` is truncated).
    pub size_bytes: u64,
    pub headers: Vec<(String, String)>,
    /// At most 1 MB of the decoded body, encoded per `bodyEncoding`.
    pub body: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub body_encoding: BodyEncoding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: i64,
    #[serde(default)]
    pub source_request_id: Option<String>,
    /// The request as edited (variables unresolved).
    pub request: Request,
    /// The URL actually sent; secret variable values are kept as `{{name}}`.
    pub resolved_url: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<HistoryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SendError>,
    /// Set for a WebSocket / Socket.IO session (method `WS` / `SIO`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ws: Option<WsSessionSummary>,
    /// Set for a gRPC call or stream (method `GRPC`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grpc: Option<GrpcCallSummary>,
}

/// The four gRPC call kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum GrpcCallKind {
    Unary,
    ServerStreaming,
    ClientStreaming,
    BidiStreaming,
}

impl GrpcCallKind {
    pub fn from_streaming(client: bool, server: bool) -> Self {
        match (client, server) {
            (false, false) => GrpcCallKind::Unary,
            (false, true) => GrpcCallKind::ServerStreaming,
            (true, false) => GrpcCallKind::ClientStreaming,
            (true, true) => GrpcCallKind::BidiStreaming,
        }
    }
    /// "unary", "server streaming", …
    pub fn label(&self) -> &'static str {
        match self {
            GrpcCallKind::Unary => "unary",
            GrpcCallKind::ServerStreaming => "server streaming",
            GrpcCallKind::ClientStreaming => "client streaming",
            GrpcCallKind::BidiStreaming => "bidi streaming",
        }
    }
    /// The client sends one message and ends (unary, server streaming).
    pub fn single_request(&self) -> bool {
        matches!(self, GrpcCallKind::Unary | GrpcCallKind::ServerStreaming)
    }
}

/// A decoded `google.rpc.Status` detail.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcStatusDetail {
    pub type_url: String,
    /// Pretty JSON when the type is known.
    pub json: Option<String>,
    pub base64: String,
}

/// A gRPC status (AC-GRPC-7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcStatus {
    pub code: i32,
    /// `OK`, `NOT_FOUND`, … (`CODE_17` for an unknown code).
    pub name: String,
    pub message: String,
    #[serde(default)]
    pub details: Vec<GrpcStatusDetail>,
}

/// One gRPC call in history (AC-GRPC-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcCallSummary {
    pub service: String,
    pub method: String,
    pub call_kind: GrpcCallKind,
    pub duration_ms: u64,
    pub sent: u64,
    pub received: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<GrpcStatus>,
    /// Unary: the response message as pretty JSON (≤ 1 MB).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub response_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Vec<(String, String)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trailers: Option<Vec<(String, String)>>,
    /// Keys from a newer ReqHarbor, kept on rewrite (AC-FWD-1).
    #[serde(flatten)]
    pub extra: Extra,
}

/// Summary of a finished WebSocket session, for history (AC-WS-7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsSessionSummary {
    pub duration_ms: u64,
    /// Data messages sent (text, binary, Socket.IO events).
    pub sent: u64,
    /// Data messages received (text, binary, Socket.IO events and acks).
    pub received: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_code: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
    /// Keys from a newer ReqHarbor, kept on rewrite (AC-FWD-1).
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Project,
    Collection,
    Folder,
    Request,
    Environment,
}

impl EntityType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityType::Project => "project",
            EntityType::Collection => "collection",
            EntityType::Folder => "folder",
            EntityType::Request => "request",
            EntityType::Environment => "environment",
        }
    }
    /// Parse a wire `type`; `None` for types this build does not know.
    pub fn parse(s: &str) -> Option<EntityType> {
        Some(match s {
            "project" => EntityType::Project,
            "collection" => EntityType::Collection,
            "folder" => EntityType::Folder,
            "request" => EntityType::Request,
            "environment" => EntityType::Environment,
            _ => return None,
        })
    }
    /// Parent-first ordering used when applying pulled changes.
    pub fn depth(&self) -> u8 {
        match self {
            EntityType::Project => 0,
            EntityType::Environment => 1,
            EntityType::Collection => 1,
            EntityType::Folder => 2,
            EntityType::Request => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub deleted_at: i64,
    #[serde(flatten)]
    pub extra: Extra,
}

/// One project's file: `projects/<projectId>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTree {
    pub project: Project,
    #[serde(default)]
    pub collections: Vec<Collection>,
    #[serde(default)]
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub requests: Vec<Request>,
    /// Unknown file-level keys of `projects/<id>.json` (AC-FWD-1).
    #[serde(flatten)]
    pub extra: Extra,
}

impl ProjectTree {
    /// A project with no children.
    pub fn new(project: Project) -> Self {
        ProjectTree {
            project,
            collections: Vec::new(),
            folders: Vec::new(),
            requests: Vec::new(),
            extra: Extra::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// `settings.json`. Unknown keys written by the UI are preserved in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "lenient_theme")]
    pub theme: Theme,
    #[serde(default, deserialize_with = "lenient_opt_px")]
    pub window_width: Option<u32>,
    #[serde(default, deserialize_with = "lenient_opt_px")]
    pub window_height: Option<u32>,
    /// Accepts fractional pixels from HiDPI layouts; rounded (QA-R2-001).
    #[serde(default = "default_sidebar_width", deserialize_with = "lenient_px")]
    pub sidebar_width: u32,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default = "default_split_ratio", deserialize_with = "lenient_ratio")]
    pub split_ratio: f64,
    /// "auto" | "side" | "stacked"
    #[serde(default = "default_layout_mode")]
    pub layout_mode: String,
    #[serde(default)]
    pub last_opened_request_id: Option<String>,
    #[serde(default)]
    pub active_project_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_sidebar_width() -> u32 {
    300
}

/// Largest pixel size accepted for persisted layout values.
const MAX_PX: f64 = 100_000.0;

/// Any JSON number (or numeric string) -> rounded, clamped pixel count.
/// Non-numbers yield `None` instead of failing the whole settings object.
fn px_from_value(v: &serde_json::Value) -> Option<u32> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    n.is_finite().then(|| n.round().clamp(0.0, MAX_PX) as u32)
}

fn lenient_px<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(px_from_value(&v).unwrap_or_else(default_sidebar_width))
}

fn lenient_opt_px<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(px_from_value(&v))
}

fn lenient_ratio<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_f64()
        .filter(|r| r.is_finite())
        .map(|r| r.clamp(0.0, 1.0))
        .unwrap_or_else(default_split_ratio))
}

fn lenient_theme<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}
fn default_split_ratio() -> f64 {
    0.5
}
fn default_layout_mode() -> String {
    "auto".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            window_width: None,
            window_height: None,
            sidebar_width: default_sidebar_width(),
            sidebar_collapsed: false,
            split_ratio: default_split_ratio(),
            layout_mode: default_layout_mode(),
            last_opened_request_id: None,
            active_project_id: None,
            extra: serde_json::Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_gql_1_operation_name_serde_and_forward_compat() {
        // Absent: None, and not written back (older files stay byte-identical).
        let g: GraphqlBody = serde_json::from_str(r#"{"query":"{a}","variables":""}"#).unwrap();
        assert_eq!(g.operation_name, None);
        assert_eq!(
            serde_json::to_string(&g).unwrap(),
            r#"{"query":"{a}","variables":""}"#
        );
        // Present: kept next to keys from a newer build.
        let text = r#"{"query":"query A { a } query B { b }","variables":"","operationName":"B","fxPersisted":{"sha":"x"}}"#;
        let g: GraphqlBody = serde_json::from_str(text).unwrap();
        assert_eq!(g.operation_name.as_deref(), Some("B"));
        assert_eq!(g.operation(), Some("B"));
        assert!(g.extra.contains_key("fxPersisted"));
        assert!(!g.extra.contains_key("operationName"));
        assert_eq!(serde_json::to_string(&g).unwrap(), text);
        // A whole request round-trips it, and the body stays known/editable.
        let r: Request = serde_json::from_value(serde_json::json!({
            "id": "r", "collectionId": "c", "name": "n", "method": "POST", "url": "u",
            "body": {"type": "graphql", "graphql": {"query": "{a}", "variables": "", "operationName": "A"}}
        }))
        .unwrap();
        assert!(r.is_editable());
        let back = serde_json::to_value(&r).unwrap();
        assert_eq!(back["body"]["graphql"]["operationName"], "A");
        // Blank names are not an operation.
        let blank = GraphqlBody {
            operation_name: Some(" ".into()),
            ..GraphqlBody::default()
        };
        assert_eq!(blank.operation(), None);
    }

    #[test]
    fn ac_req_1_method_validation() {
        for m in STANDARD_METHODS {
            assert!(is_valid_method(m));
        }
        assert!(is_valid_method("PURGE"));
        assert!(is_valid_method("X-CUSTOM_1"));
        assert!(!is_valid_method("get"));
        assert!(!is_valid_method("1GET"));
        assert!(!is_valid_method(""));
        assert!(!is_valid_method("GE T"));
        assert!(!is_valid_method("-GET"));
    }

    #[test]
    fn ids_are_ulids() {
        let id = new_id();
        assert_eq!(id.len(), 26);
        assert!(ulid::Ulid::from_string(&id).is_ok());
    }

    #[test]
    fn request_json_shape_is_camel_case_with_defaults() {
        let req: Request = serde_json::from_str(r#"{"id":"x","url":"http://a"}"#).unwrap();
        assert_eq!(req.method, "GET");
        assert!(req.settings.follow_redirects);
        assert_eq!(req.settings.timeout_ms, 30000);
        assert!(req.settings.verify_tls);
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("collectionId").is_some());
        assert_eq!(v["auth"]["type"], "none");
        assert_eq!(v["body"]["type"], "none");
        let b = serde_json::to_value(Body::form_encoded(vec![])).unwrap();
        assert_eq!(b["type"], "formEncoded");
        let a = serde_json::to_value(Auth::api_key("k", "v", ApiKeyLocation::Query)).unwrap();
        assert_eq!(a["apiKey"]["in"], "query");
        assert_eq!(a["type"], "apikey");
    }

    #[test]
    fn qa_r2_001_settings_accept_fractional_and_odd_numbers() {
        let s: Settings = serde_json::from_str(
            r#"{"sidebarWidth":320.5,"windowWidth":1279.6,"windowHeight":null,"splitRatio":1.7,"theme":"neon"}"#,
        )
        .unwrap();
        assert_eq!(s.sidebar_width, 321);
        assert_eq!(s.window_width, Some(1280));
        assert_eq!(s.window_height, None);
        assert_eq!(s.split_ratio, 1.0);
        assert_eq!(s.theme, Theme::System);
        let s: Settings =
            serde_json::from_str(r#"{"sidebarWidth":-5,"windowWidth":"800"}"#).unwrap();
        assert_eq!(s.sidebar_width, 0);
        assert_eq!(s.window_width, Some(800));
        let s: Settings = serde_json::from_str(r#"{"sidebarWidth":"wide"}"#).unwrap();
        assert_eq!(s.sidebar_width, 300);
        // round-trips as integers
        let v = serde_json::to_value(Settings {
            sidebar_width: 321,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(v["sidebarWidth"], 321);
    }

    #[test]
    fn qa_r2_002_settings_round_trip_ui_share_options_key() {
        let raw = r#"{"theme":"dark","sidebarWidth":312.5,"shareOptions":{"S1":{"includeCredentials":true,"environmentId":"e1"},"S2":{"includeCredentials":false,"environmentId":null}}}"#;
        let s: Settings = serde_json::from_str(raw).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["shareOptions"]["S1"]["includeCredentials"], true);
        assert_eq!(v["shareOptions"]["S1"]["environmentId"], "e1");
        assert!(v["shareOptions"]["S2"]["environmentId"].is_null());
        assert_eq!(v["sidebarWidth"], 313);
        let again: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn settings_preserve_unknown_keys() {
        let s: Settings = serde_json::from_str(r#"{"theme":"dark","fooBar":42}"#).unwrap();
        assert_eq!(s.theme, Theme::Dark);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["fooBar"], 42);
        assert_eq!(v["sidebarWidth"], 300);
    }
}

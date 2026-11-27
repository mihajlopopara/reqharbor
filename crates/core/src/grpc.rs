//! gRPC requests (AC-GRPC-*).
//!
//! * [`prepare_grpc`] turns a `grpc` request into a [`PreparedGrpc`]:
//!   variables (environment and `{{$…}}`, one memo per call), metadata from
//!   the headers and the auth, the target from the URL (TLS by scheme) and
//!   the resolved message and proto paths.
//! * Services come from server reflection ([`reflect`], v1 with a v1alpha
//!   fallback) or `.proto` files compiled in-process ([`compile_files`],
//!   protox). Reflection results are cached on disk ([`DescriptorCache`]),
//!   only after they build.
//! * [`run_call`] runs one call of any kind with dynamic messages
//!   (prost-reflect, proto3 JSON mapping) over tonic's HTTP/2 client and our
//!   own TCP / tokio-rustls connector, streaming [`GrpcFrame`]s in order.
//!   [`invoke`] is the unary form; both return [`CallStats`] for history.

use crate::dynamic::DynamicCtx;
use crate::model::{
    ApiKeyLocation, Auth, AuthType, ErrorKind, GrpcCallKind, GrpcCallSummary, GrpcStatus,
    GrpcStatusDetail, HistoryEntry, ProtoSource, Request, RequestKind, SendError, now_ms,
};
use crate::prepare::{PrepareError, effective_auth, prepare_in};
use crate::redact::Redactor;
use crate::vars::{ResolveOptions, Unresolved, VarScope};
use crate::ws::Dir;
use base64::Engine as _;
use prost::Message as _;
use prost_reflect::{
    DescriptorPool, DynamicMessage, FieldDescriptor, Kind, MessageDescriptor, MethodDescriptor,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tonic::metadata::{MetadataKey, MetadataMap, MetadataValue};
use tonic::transport::Endpoint;

/// Types the app shell needs without depending on the crates itself.
pub use prost_reflect::{
    DescriptorPool as Pool, DynamicMessage as Message, MessageDescriptor as InputType,
};
pub use tonic::transport::Channel;

/// Largest message encoded or decoded.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Frame data longer than this is cut in the UI event (`truncated: true`).
pub const MAX_FRAME_DATA: usize = crate::ws::MAX_FRAME_DATA;
/// A cached descriptor set is at most this large.
pub const DESCRIPTOR_MAX_BYTES: usize = 10 * 1024 * 1024;
/// History keeps at most this much of a unary response.
pub const HISTORY_RESPONSE_MAX: usize = 1024 * 1024;
/// Cache directory inside the app data dir.
pub const CACHE_DIR: &str = "grpc-descriptors";
const CACHE_SCHEMA_VERSION: u32 = 1;
const V1_PATH: &str = "/grpc.reflection.v1.ServerReflection/ServerReflectionInfo";
const V1ALPHA_PATH: &str = "/grpc.reflection.v1alpha.ServerReflection/ServerReflectionInfo";

/// The names of the 17 status codes.
pub const CODE_NAMES: [&str; 17] = [
    "OK",
    "CANCELLED",
    "UNKNOWN",
    "INVALID_ARGUMENT",
    "DEADLINE_EXCEEDED",
    "NOT_FOUND",
    "ALREADY_EXISTS",
    "PERMISSION_DENIED",
    "RESOURCE_EXHAUSTED",
    "FAILED_PRECONDITION",
    "ABORTED",
    "OUT_OF_RANGE",
    "UNIMPLEMENTED",
    "INTERNAL",
    "UNAVAILABLE",
    "DATA_LOSS",
    "UNAUTHENTICATED",
];

/// `5` → `NOT_FOUND`; unknown codes → `CODE_17`.
pub fn code_name(code: i32) -> String {
    usize::try_from(code)
        .ok()
        .and_then(|c| CODE_NAMES.get(c))
        .map_or_else(|| format!("CODE_{code}"), |s| s.to_string())
}

/// Metadata names ReqHarbor sets itself (AC-GRPC-2).
const RESERVED: [&str; 5] = ["content-type", "te", "host", "connection", "user-agent"];

// ------------------------------------------------------------------- errors

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GrpcError {
    /// Nothing was sent: the request can't be prepared.
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    /// A network failure (AC-REQ-9 kinds).
    #[error("{}", .0.message)]
    Network(SendError),
    #[error("This server doesn't support reflection. Use .proto files instead.")]
    ReflectionUnsupported,
    /// Another reflection status (`code`, name) or an unusable answer.
    #[error("{message}")]
    ReflectionFailed {
        message: String,
        status: Option<(i32, String)>,
    },
    /// `.proto` files that don't compile.
    #[error("{0}")]
    ProtoInvalid(String),
    #[error("The descriptor set is larger than 10 MB ({} MB)", .0 / (1024 * 1024))]
    TooLarge(u64),
    #[error("{0}")]
    Io(String),
    /// `grpc_invoke` on a method that streams: it can't be a one-shot call.
    #[error("This is a {} method: connect to it instead of invoking it", .0.label())]
    StreamingMethod(GrpcCallKind),
}

fn err(kind: ErrorKind, message: impl Into<String>) -> SendError {
    SendError {
        kind,
        message: message.into(),
    }
}

// ------------------------------------------------------------------- target

/// Where a call goes (AC-GRPC-2): TLS from the scheme, host and port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub tls: bool,
    /// Without IPv6 brackets.
    pub host: String,
    pub port: u16,
    /// The URL had a path, query or fragment (ignored).
    pub has_path: bool,
}

impl Target {
    /// `grpcs://host:port` / `grpc://host:port`.
    pub fn origin(&self) -> String {
        format!(
            "{}://{}:{}",
            if self.tls { "grpcs" } else { "grpc" },
            self.authority_host(),
            self.port
        )
    }
    /// What tonic's endpoint is given (`https://` / `http://`).
    fn endpoint_uri(&self) -> String {
        format!(
            "{}://{}:{}",
            if self.tls { "https" } else { "http" },
            self.authority_host(),
            self.port
        )
    }
    fn authority_host(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        }
    }
    /// `host:port` (grpcurl's address).
    pub fn address(&self) -> String {
        format!("{}:{}", self.authority_host(), self.port)
    }
}

/// Parse a gRPC URL: `grpcs://` or `https://` = TLS, `grpc://` or
/// `http://` = plaintext; no scheme = AC-REQ-10 (loopback plaintext).
pub fn parse_target(url: &str) -> Result<Target, String> {
    let url = crate::urlx::apply_default_scheme(url.trim());
    let Some(i) = url.find("://") else {
        return Err(format!(
            "\u{201c}{url}\u{201d} isn't a grpc://, grpcs://, http:// or https:// URL"
        ));
    };
    let scheme = url[..i].to_ascii_lowercase();
    let tls = match scheme.as_str() {
        "grpcs" | "https" => true,
        "grpc" | "http" => false,
        other => {
            return Err(format!(
                "Use grpc://, grpcs://, http:// or https:// (not {other}://)"
            ));
        }
    };
    let rest = &url[i + 3..];
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let tail = &rest[end..];
    let authority = authority.rsplit_once('@').map_or(authority, |(_, a)| a);
    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        let close = stripped
            .find(']')
            .ok_or_else(|| "Unclosed [ in the host".to_string())?;
        let host = &stripped[..close];
        let after = &stripped[close + 1..];
        let port = match after.strip_prefix(':') {
            Some(p) => Some(p),
            None if after.is_empty() => None,
            None => return Err(format!("Invalid host \u{201c}{authority}\u{201d}")),
        };
        (host.to_string(), port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (authority.to_string(), None),
        }
    };
    if host.is_empty() {
        return Err("The URL has no host".into());
    }
    let port = match port {
        None | Some("") => {
            if tls {
                443
            } else {
                80
            }
        }
        Some(p) => p
            .parse::<u16>()
            .map_err(|_| format!("Invalid port \u{201c}{p}\u{201d}"))?,
    };
    Ok(Target {
        tls,
        host,
        port,
        has_path: !(tail.is_empty() || tail == "/"),
    })
}

// ------------------------------------------------------------------ prepare

/// A `grpc` request ready to run.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedGrpc {
    pub target: Target,
    /// `grpcs://host:port/pkg.Service/Method`, secret variables kept as
    /// `{{name}}` (history, UI).
    pub display_url: String,
    /// `grpcs://host:port`, secrets masked: the descriptor cache key.
    pub display_origin: String,
    pub service: String,
    pub method: String,
    /// (lowercase name, value); `-bin` values are base64 text.
    pub metadata: Vec<(String, String)>,
    /// Resolved JSON text (`{}` when empty).
    pub message: String,
    pub proto_source: ProtoSource,
    pub proto_files: Vec<String>,
    pub import_paths: Vec<String>,
    /// Unary deadline / connect limit; 0 = none.
    pub timeout_ms: u64,
    pub verify_tls: bool,
    /// Variables referenced but not defined (AC-ENV-3).
    pub unresolved: Vec<String>,
}

impl PreparedGrpc {
    /// `pkg.Service/Method`.
    pub fn full_method(&self) -> String {
        format!("{}/{}", self.service, self.method)
    }
}

fn valid_base64(s: &str) -> bool {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.trim_end_matches('=');
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(t)
        .is_ok()
        || base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(t)
            .is_ok()
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let t: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.trim_end_matches('=');
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(t)
        .or_else(|_| base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(t))
        .ok()
}

/// Check metadata names and values (AC-GRPC-2) and lowercase the names.
pub fn check_metadata(
    headers: Vec<(String, String)>,
) -> Result<Vec<(String, String)>, PrepareError> {
    let mut out = Vec::with_capacity(headers.len());
    for (k, v) in headers {
        let name = k.trim().to_ascii_lowercase();
        if name.starts_with(':') || name.starts_with("grpc-") || RESERVED.contains(&name.as_str()) {
            return Err(PrepareError::InvalidGrpcMetadata(format!(
                "\u{201c}{name}\u{201d} is set by ReqHarbor: remove it from Metadata"
            )));
        }
        if http::header::HeaderName::from_bytes(name.as_bytes()).is_err() {
            return Err(PrepareError::InvalidGrpcMetadata(format!(
                "\u{201c}{k}\u{201d} isn't a valid metadata name"
            )));
        }
        if name.ends_with("-bin") {
            if !valid_base64(&v) {
                return Err(PrepareError::InvalidGrpcMetadata(format!(
                    "The value of \u{201c}{name}\u{201d} must be base64 (a -bin key carries bytes)"
                )));
            }
        } else if http::header::HeaderValue::from_str(&v).is_err() || !v.is_ascii() {
            return Err(PrepareError::InvalidGrpcMetadata(format!(
                "The value of \u{201c}{name}\u{201d} has characters metadata can't carry (use a -bin key for bytes)"
            )));
        }
        out.push((name, v));
    }
    Ok(out)
}

/// The auth that applies, with an API key always sent as metadata (gRPC has
/// no query string).
fn grpc_auth(request: &Request, collection_auth: Option<&Auth>) -> Result<Auth, PrepareError> {
    let auth = effective_auth(&request.auth, collection_auth);
    if auth.is_some_and(|a| !a.is_known()) {
        return Err(PrepareError::InheritedAuthNeedsNewerVersion);
    }
    let mut auth = auth.cloned().unwrap_or_else(Auth::none);
    if auth.auth_type == AuthType::Apikey
        && let Some(k) = auth.api_key.as_mut()
        && k.location == ApiKeyLocation::Query
    {
        k.location = ApiKeyLocation::Header;
    }
    Ok(auth)
}

/// Prepare a `grpc` request (AC-GRPC-2/9). `require_method`: a call needs a
/// service and a method; descriptor lookups don't. `dynamic` is the
/// `{{$…}}` memo of the call.
pub fn prepare_grpc(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    require_method: bool,
    dynamic: &DynamicCtx,
) -> Result<PreparedGrpc, PrepareError> {
    if !request.is_editable() {
        return Err(PrepareError::NeedsNewerVersion);
    }
    if !request.kind.is_grpc() {
        return Err(PrepareError::NotGrpc);
    }
    if request.url.trim().is_empty() {
        return Err(PrepareError::EmptyUrl);
    }
    let cfg = request.grpc.clone().unwrap_or_default();
    let service = cfg.service.trim().to_string();
    let method = cfg.method.trim().to_string();
    if require_method && (service.is_empty() || method.is_empty()) {
        return Err(PrepareError::NoGrpcMethod);
    }
    // URL, headers and auth exactly like an HTTP send.
    let mut http = request.clone();
    http.kind = RequestKind::Http;
    http.method = "POST".into();
    http.params = Vec::new();
    http.body = Default::default();
    http.ws = None;
    http.grpc = None;
    http.auth = grpc_auth(request, collection_auth)?;
    let p = prepare_in(&http, scope, None, "", dynamic)?;
    // The message must not show resolved secrets (it quotes the URL).
    let target = parse_target(&p.url)
        .map_err(|e| PrepareError::InvalidGrpcUrl(Redactor::from_scope(scope).redact(&e)))?;
    let display_origin = parse_target(&p.display_url)
        .map(|t| t.origin())
        .unwrap_or_else(|_| target.origin());
    let metadata = check_metadata(p.headers)?;
    let mut unresolved = Unresolved::default();
    let mut r = |s: &str| scope.resolve_in(s, ResolveOptions::default(), &mut unresolved, dynamic);
    let message = {
        let m = r(&cfg.message);
        if m.trim().is_empty() {
            "{}".to_string()
        } else {
            m
        }
    };
    let paths = |list: &[String], r: &mut dyn FnMut(&str) -> String| -> Vec<String> {
        list.iter()
            .map(|p| r(p.trim()))
            .filter(|p| !p.is_empty())
            .collect()
    };
    let proto_files = paths(&cfg.proto_files, &mut r);
    let import_paths = paths(&cfg.import_paths, &mut r);
    let mut names = p.unresolved;
    for n in unresolved.into_names() {
        if !names.contains(&n) {
            names.push(n);
        }
    }
    Ok(PreparedGrpc {
        display_url: format!("{display_origin}/{service}/{method}"),
        display_origin,
        target,
        service,
        method,
        metadata,
        message,
        proto_source: cfg.proto_source,
        proto_files,
        import_paths,
        timeout_ms: p.settings.timeout_ms,
        verify_tls: p.settings.verify_tls,
        unresolved: names,
    })
}

/// The descriptor cache key of `request` without sending anything: its
/// origin with `{{$dynamic}}` variables kept literal and secrets masked
/// (like the GraphQL schema URL).
pub fn descriptor_url(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
) -> Result<String, PrepareError> {
    let p = prepare_grpc(request, scope, collection_auth, false, &DynamicCtx::keep())?;
    Ok(Redactor::from_scope(scope).redact(&p.display_origin))
}

// -------------------------------------------------------------- descriptors

/// What the UI gets about a descriptor (AC-GRPC-3..6). See `GrpcDescriptor`
/// in docs/types.ts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcDescriptor {
    pub url: String,
    pub fetched_at: i64,
    pub source: DescriptorSource,
    pub files: Vec<String>,
    pub services: Vec<ServiceInfo>,
    pub messages: BTreeMap<String, MessageInfo>,
    pub enums: BTreeMap<String, EnumInfo>,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DescriptorSource {
    Reflection,
    Files,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub name: String,
    pub methods: Vec<MethodInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MethodInfo {
    pub name: String,
    pub call_kind: GrpcCallKind,
    pub input_type: String,
    pub output_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageInfo {
    pub fields: Vec<FieldInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FieldInfo {
    pub name: String,
    pub json_name: String,
    pub number: u32,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_name: Option<String>,
    pub repeated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub map_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oneof: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnumInfo {
    pub values: Vec<String>,
}

fn kind_name(k: &Kind) -> (&'static str, Option<String>) {
    match k {
        Kind::Double => ("double", None),
        Kind::Float => ("float", None),
        Kind::Int32 => ("int32", None),
        Kind::Int64 => ("int64", None),
        Kind::Uint32 => ("uint32", None),
        Kind::Uint64 => ("uint64", None),
        Kind::Sint32 => ("sint32", None),
        Kind::Sint64 => ("sint64", None),
        Kind::Fixed32 => ("fixed32", None),
        Kind::Fixed64 => ("fixed64", None),
        Kind::Sfixed32 => ("sfixed32", None),
        Kind::Sfixed64 => ("sfixed64", None),
        Kind::Bool => ("bool", None),
        Kind::String => ("string", None),
        Kind::Bytes => ("bytes", None),
        Kind::Message(m) => ("message", Some(m.full_name().to_string())),
        Kind::Enum(e) => ("enum", Some(e.full_name().to_string())),
    }
}

fn field_info(f: &FieldDescriptor) -> FieldInfo {
    let (kind, type_name, map_key) = if f.is_map() {
        let Kind::Message(entry) = f.kind() else {
            unreachable!("map fields are messages")
        };
        let (vk, vt) = kind_name(&entry.map_entry_value_field().kind());
        let (kk, _) = kind_name(&entry.map_entry_key_field().kind());
        (vk, vt, Some(kk.to_string()))
    } else {
        let (k, t) = kind_name(&f.kind());
        (k, t, None)
    };
    FieldInfo {
        name: f.name().to_string(),
        json_name: f.json_name().to_string(),
        number: f.number(),
        kind: kind.to_string(),
        type_name,
        repeated: f.is_list(),
        map_key,
        oneof: f
            .containing_oneof()
            .filter(|o| !o.is_synthetic())
            .map(|o| o.name().to_string()),
    }
}

/// Reflection services (`grpc.reflection.*`) are hidden from the picker.
fn is_reflection_service(name: &str) -> bool {
    name.starts_with("grpc.reflection.")
}

/// Services a pool offers (reflection services left out).
pub fn user_services(pool: &DescriptorPool) -> usize {
    pool.services()
        .filter(|s| !is_reflection_service(s.full_name()))
        .count()
}

/// Summarise `pool` for the UI: services sorted by name with their methods,
/// and every message and enum the methods reach.
pub fn describe(
    pool: &DescriptorPool,
    url: &str,
    source: DescriptorSource,
    files: Vec<String>,
    fetched_at: i64,
) -> GrpcDescriptor {
    let mut services: Vec<ServiceInfo> = pool
        .services()
        .filter(|s| !is_reflection_service(s.full_name()))
        .map(|s| ServiceInfo {
            name: s.full_name().to_string(),
            methods: s
                .methods()
                .map(|m| MethodInfo {
                    name: m.name().to_string(),
                    call_kind: GrpcCallKind::from_streaming(
                        m.is_client_streaming(),
                        m.is_server_streaming(),
                    ),
                    input_type: m.input().full_name().to_string(),
                    output_type: m.output().full_name().to_string(),
                })
                .collect(),
        })
        .collect();
    services.sort_by(|a, b| a.name.cmp(&b.name));
    let mut messages = BTreeMap::new();
    let mut enums = BTreeMap::new();
    let mut queue: VecDeque<MessageDescriptor> = pool
        .services()
        .filter(|s| !is_reflection_service(s.full_name()))
        .flat_map(|s| {
            s.methods()
                .flat_map(|m| [m.input(), m.output()])
                .collect::<Vec<_>>()
        })
        .collect();
    let mut seen = HashSet::new();
    while let Some(m) = queue.pop_front() {
        if !seen.insert(m.full_name().to_string()) {
            continue;
        }
        let mut fields = Vec::new();
        for f in m.fields() {
            fields.push(field_info(&f));
            let kinds = if f.is_map() {
                match f.kind() {
                    Kind::Message(entry) => vec![
                        entry.map_entry_key_field().kind(),
                        entry.map_entry_value_field().kind(),
                    ],
                    k => vec![k],
                }
            } else {
                vec![f.kind()]
            };
            for k in kinds {
                match k {
                    Kind::Message(n) => queue.push_back(n),
                    Kind::Enum(e) => {
                        enums
                            .entry(e.full_name().to_string())
                            .or_insert_with(|| EnumInfo {
                                values: e.values().map(|v| v.name().to_string()).collect(),
                            });
                    }
                    _ => {}
                }
            }
        }
        messages.insert(m.full_name().to_string(), MessageInfo { fields });
    }
    GrpcDescriptor {
        url: url.to_string(),
        fetched_at,
        source,
        files,
        services,
        messages,
        enums,
        size_bytes: pool.encode_to_vec().len() as u64,
    }
}

/// The method `service/method` in `pool`.
pub fn find_method(pool: &DescriptorPool, service: &str, method: &str) -> Option<MethodDescriptor> {
    pool.get_service_by_name(service)?
        .methods()
        .find(|m| m.name() == method)
}

/// The descriptor of a prepared call's method, or `UnknownGrpcMethod`.
pub fn method_of(pool: &DescriptorPool, p: &PreparedGrpc) -> Result<MethodDescriptor, GrpcError> {
    find_method(pool, &p.service, &p.method).ok_or_else(|| {
        GrpcError::Prepare(PrepareError::UnknownGrpcMethod(format!(
            "{} isn't in the descriptor",
            p.full_method()
        )))
    })
}

// ------------------------------------------------------------- proto files

const IMPORT_REFUSED: &str = "invalid import: imports are relative to an import path and can't be absolute or contain \"..\"";

/// "file.proto:12:5: message" for a protox error.
fn proto_error(e: &protox::Error) -> String {
    use miette::Diagnostic;
    let file = e.file().map(str::to_string);
    let location = (|| {
        let src = e.source_code()?;
        let label = e.labels()?.next()?;
        let span = src.read_span(label.inner(), 0, 0).ok()?;
        Some((span.line() + 1, span.column() + 1))
    })();
    let mut message = e.to_string();
    // protox-parse reports a refused import (absolute, `..`, backslash) as
    // "invalid group name"; say what is wrong.
    if message == "invalid group name"
        && e.help()
            .is_some_and(|h| h.to_string().contains("imports may not"))
    {
        message = IMPORT_REFUSED.into();
    }
    match (file, location) {
        (Some(f), Some((l, c))) => format!("{f}:{l}:{c}: {message}"),
        (Some(f), None) if !message.contains(&f) => format!("{f}: {message}"),
        _ => message,
    }
}

/// protox's resolver, refusing imports that leave the import paths:
/// absolute names and any `..` component.
struct ConfinedResolver(protox::file::ChainFileResolver);

impl protox::file::FileResolver for ConfinedResolver {
    fn resolve_path(&self, path: &Path) -> Option<String> {
        self.0.resolve_path(path)
    }

    fn open_file(&self, name: &str) -> Result<protox::file::File, protox::Error> {
        let absolute = name.starts_with(['/', '\\']) || Path::new(name).has_root() || {
            let b = name.as_bytes();
            b.len() > 1 && b[1] == b':' && b[0].is_ascii_alphabetic()
        };
        let parent = name.split(['/', '\\']).any(|c| c == "..");
        if absolute || parent {
            return Err(protox::Error::new(format!(
                "import \"{name}\" isn't allowed: imports are relative to an import path and can't be absolute or contain \"..\""
            )));
        }
        self.0.open_file(name)
    }
}

/// Compile `.proto` files (AC-GRPC-4). Imports resolve against
/// `import_paths`, then each file's own folder, then the bundled
/// `google/protobuf/*` well-known types.
pub fn compile_files(
    files: &[String],
    import_paths: &[String],
) -> Result<DescriptorPool, GrpcError> {
    if files.is_empty() {
        return Err(GrpcError::ProtoInvalid(
            "No .proto files: add them in the Proto tab".into(),
        ));
    }
    let mut includes: Vec<PathBuf> = Vec::new();
    for p in import_paths {
        let p = PathBuf::from(p);
        if !includes.contains(&p) {
            includes.push(p);
        }
    }
    for f in files {
        let path = Path::new(f);
        if !path.is_file() {
            return Err(GrpcError::ProtoInvalid(format!(
                "{f}: the file doesn't exist or isn't a regular file"
            )));
        }
        if let Some(dir) = path.parent().map(Path::to_path_buf)
            && !includes.contains(&dir)
        {
            includes.push(dir);
        }
    }
    let mut chain = protox::file::ChainFileResolver::new();
    for dir in &includes {
        chain.add(protox::file::IncludeFileResolver::new(dir.clone()));
    }
    chain.add(protox::file::GoogleFileResolver::new());
    let mut compiler = protox::Compiler::with_file_resolver(ConfinedResolver(chain));
    compiler.include_imports(true);
    for f in files {
        compiler
            .open_file(f)
            .map_err(|e| GrpcError::ProtoInvalid(proto_error(&e)))?;
    }
    let pool = compiler.descriptor_pool();
    if user_services(&pool) == 0 {
        return Err(GrpcError::ProtoInvalid(
            "The .proto files declare no service".into(),
        ));
    }
    Ok(pool)
}

// -------------------------------------------------------------------- cache

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    schema_version: u32,
    url: String,
    fetched_at: i64,
    /// base64 of the encoded FileDescriptorSet.
    descriptor_set: String,
}

/// A cached descriptor set.
#[derive(Debug, Clone)]
pub struct CachedDescriptor {
    pub url: String,
    pub fetched_at: i64,
    pub pool: DescriptorPool,
}

/// Reflection results on disk, one file per origin (AC-GRPC-3), with the
/// GraphQL schema cache's layout and eviction.
#[derive(Debug, Clone)]
pub struct DescriptorCache {
    files: crate::graphql::SchemaCache,
}

impl DescriptorCache {
    pub fn new(data_dir: &Path) -> Self {
        DescriptorCache {
            files: crate::graphql::SchemaCache::in_dir(data_dir.join(CACHE_DIR)),
        }
    }

    pub fn dir(&self) -> &Path {
        self.files.dir()
    }

    /// The cached descriptor of `url`. An unreadable file, or one that no
    /// longer builds, counts as missing (and is removed).
    pub fn get(&self, url: &str) -> Option<CachedDescriptor> {
        let path = self.files.file_for(url);
        let bytes = std::fs::read(&path).ok()?;
        let decoded = serde_json::from_slice::<CacheFile>(&bytes)
            .ok()
            .and_then(|f| {
                let raw = base64::engine::general_purpose::STANDARD
                    .decode(&f.descriptor_set)
                    .ok()?;
                let pool = DescriptorPool::decode(raw.as_slice()).ok()?;
                Some((f, pool))
            });
        match decoded {
            Some((f, pool)) if f.url == url => Some(CachedDescriptor {
                url: f.url,
                fetched_at: f.fetched_at,
                pool,
            }),
            // Another URL with the same hash prefix: leave it alone.
            Some(_) => None,
            None => {
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Store a descriptor set that built (validated by the caller), then
    /// trim the cache.
    pub fn put(&self, url: &str, fetched_at: i64, pool: &DescriptorPool) -> Result<(), GrpcError> {
        let raw = pool.encode_to_vec();
        if raw.len() > DESCRIPTOR_MAX_BYTES {
            return Err(GrpcError::TooLarge(raw.len() as u64));
        }
        let file = CacheFile {
            schema_version: CACHE_SCHEMA_VERSION,
            url: url.to_string(),
            fetched_at,
            descriptor_set: base64::engine::general_purpose::STANDARD.encode(raw),
        };
        let path = self.files.file_for(url);
        let bytes = serde_json::to_vec(&file).map_err(|e| GrpcError::Io(e.to_string()))?;
        crate::storage::atomic_write(&path, &bytes).map_err(|e| GrpcError::Io(e.to_string()))?;
        self.files.evict(
            &path,
            crate::graphql::CACHE_MAX_BYTES,
            crate::graphql::CACHE_MAX_ENTRIES,
        );
        Ok(())
    }

    /// Forget the descriptor of `url`. True when there was one.
    pub fn remove(&self, url: &str) -> Result<bool, GrpcError> {
        match std::fs::remove_file(self.files.file_for(url)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(GrpcError::Io(e.to_string())),
        }
    }
}

// --------------------------------------------------------------- connecting

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

async fn open_io(target: &Target, verify_tls: bool) -> Result<Box<dyn Io>, SendError> {
    let host = target.host.clone();
    let port = target.port;
    let addrs: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
        .await
        .map_err(|e| err(ErrorKind::Dns, format!("Couldn't resolve {host}: {e}")))?
        .collect();
    if addrs.is_empty() {
        return Err(err(ErrorKind::Dns, format!("Couldn't resolve {host}")));
    }
    let mut last = None;
    let mut tcp = None;
    for a in addrs {
        match tokio::net::TcpStream::connect(a).await {
            Ok(s) => {
                tcp = Some(s);
                break;
            }
            Err(e) => last = Some(e),
        }
    }
    let tcp = tcp.ok_or_else(|| {
        err(
            ErrorKind::Connect,
            format!(
                "Couldn't connect to {}: {}",
                target.address(),
                last.map(|e| e.to_string()).unwrap_or_default()
            ),
        )
    })?;
    let _ = tcp.set_nodelay(true);
    if !target.tls {
        return Ok(Box::new(tcp));
    }
    crate::http::install_crypto_provider();
    let cfg = crate::ws::tls::h2_config(verify_tls).map_err(|e| err(ErrorKind::Tls, e))?;
    let name = rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| {
        err(
            ErrorKind::Tls,
            format!("Invalid TLS server name {host}: {e}"),
        )
    })?;
    let tls = tokio_rustls::TlsConnector::from(cfg)
        .connect(name, tcp)
        .await
        .map_err(|e| {
            err(
                ErrorKind::Tls,
                format!("TLS handshake with {host} failed: {e}"),
            )
        })?;
    Ok(Box::new(tls))
}

/// Connect (TCP, TLS with ALPN h2, HTTP/2 handshake) within `timeout_ms`
/// (0 = none).
pub async fn connect(
    target: &Target,
    verify_tls: bool,
    timeout_ms: u64,
) -> Result<Channel, SendError> {
    let work = async {
        let io = open_io(target, verify_tls).await?;
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some(hyper_util::rt::TokioIo::new(
            io,
        ))));
        let connector = tower::service_fn(move |_uri: http::Uri| {
            let io = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
            async move {
                io.ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::NotConnected,
                        "the connection was closed",
                    )
                })
            }
        });
        let endpoint = Endpoint::from_shared(target.endpoint_uri())
            .map_err(|e| err(ErrorKind::Other, format!("Invalid URL: {e}")))?
            .user_agent(crate::http::USER_AGENT)
            .map_err(|e| err(ErrorKind::Other, e.to_string()))?;
        endpoint
            .connect_with_connector(connector)
            .await
            .map_err(|e| {
                let mut msg = e.to_string();
                let mut src = std::error::Error::source(&e);
                while let Some(s) = src {
                    msg = format!("{msg}: {s}");
                    src = s.source();
                }
                err(
                    ErrorKind::Connect,
                    format!("HTTP/2 connection to {} failed: {msg}", target.address()),
                )
            })
    };
    if timeout_ms == 0 {
        return work.await;
    }
    match tokio::time::timeout(Duration::from_millis(timeout_ms), work).await {
        Ok(r) => r,
        Err(_) => Err(err(
            ErrorKind::Timeout,
            format!("Timed out after {} connecting", human_ms(timeout_ms)),
        )),
    }
}

pub fn human_ms(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        format!("{} s", ms / 1000)
    } else if ms >= 1000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

/// Build tonic metadata from prepared pairs.
fn metadata_map(pairs: &[(String, String)]) -> MetadataMap {
    let mut md = MetadataMap::new();
    for (k, v) in pairs {
        if k.ends_with("-bin") {
            if let (Ok(key), Some(bytes)) = (
                MetadataKey::<tonic::metadata::Binary>::from_bytes(k.as_bytes()),
                decode_base64(v),
            ) {
                md.append_bin(key, MetadataValue::from_bytes(&bytes));
            }
        } else if let (Ok(key), Ok(value)) = (
            MetadataKey::<tonic::metadata::Ascii>::from_bytes(k.as_bytes()),
            MetadataValue::try_from(v.as_str()),
        ) {
            md.append(key, value);
        }
    }
    md
}

/// (name, value) pairs of received metadata, `-bin` values as base64 text.
pub fn metadata_pairs(md: &MetadataMap) -> Vec<(String, String)> {
    let headers = md.clone().into_headers();
    headers
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                v.to_str()
                    .map(str::to_string)
                    .unwrap_or_else(|_| String::from_utf8_lossy(v.as_bytes()).into_owned()),
            )
        })
        .collect()
}

// --------------------------------------------------------------- reflection

use tonic_reflection::pb::v1::{
    ServerReflectionRequest, ServerReflectionResponse, server_reflection_request::MessageRequest,
    server_reflection_response::MessageResponse,
};

struct ReflectionSession {
    tx: mpsc::UnboundedSender<ServerReflectionRequest>,
    inbound: tonic::Streaming<ServerReflectionResponse>,
}

fn reflection_stream(
    rx: mpsc::UnboundedReceiver<ServerReflectionRequest>,
) -> impl futures_util::Stream<Item = ServerReflectionRequest> + Send + 'static {
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) })
}

fn status_failure(st: &tonic::Status) -> GrpcError {
    let code = st.code() as i32;
    GrpcError::ReflectionFailed {
        message: format!("Reflection failed: {}: {}", code_name(code), st.message()),
        status: Some((code, code_name(code))),
    }
}

impl ReflectionSession {
    async fn open(
        channel: Channel,
        path: &'static str,
        metadata: &[(String, String)],
        first: MessageRequest,
    ) -> Result<(Self, ServerReflectionResponse), tonic::Status> {
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(ServerReflectionRequest {
            host: String::new(),
            message_request: Some(first),
        });
        let mut grpc = tonic::client::Grpc::new(channel)
            .max_decoding_message_size(DESCRIPTOR_MAX_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        grpc.ready()
            .await
            .map_err(|e| tonic::Status::unavailable(e.to_string()))?;
        let mut req = tonic::Request::new(reflection_stream(rx));
        *req.metadata_mut() = metadata_map(metadata);
        let codec =
            tonic_prost::ProstCodec::<ServerReflectionRequest, ServerReflectionResponse>::default();
        let resp = grpc
            .streaming(req, http::uri::PathAndQuery::from_static(path), codec)
            .await?;
        let mut inbound = resp.into_inner();
        let answer = inbound
            .message()
            .await?
            .ok_or_else(|| tonic::Status::unknown("The reflection stream ended early"))?;
        Ok((ReflectionSession { tx, inbound }, answer))
    }

    async fn ask(&mut self, req: MessageRequest) -> Result<MessageResponse, GrpcError> {
        self.tx
            .send(ServerReflectionRequest {
                host: String::new(),
                message_request: Some(req),
            })
            .map_err(|_| GrpcError::ReflectionFailed {
                message: "Reflection failed: the stream closed".into(),
                status: None,
            })?;
        let answer = self
            .inbound
            .message()
            .await
            .map_err(|s| status_failure(&s))?
            .ok_or_else(|| GrpcError::ReflectionFailed {
                message: "Reflection failed: the stream ended early".into(),
                status: None,
            })?;
        answer
            .message_response
            .ok_or_else(|| GrpcError::ReflectionFailed {
                message: "Reflection failed: an empty answer".into(),
                status: None,
            })
    }
}

/// Decoded file descriptors from a reflection answer.
fn files_of(
    r: MessageResponse,
    what: &str,
) -> Result<Vec<prost_types::FileDescriptorProto>, Option<GrpcError>> {
    match r {
        MessageResponse::FileDescriptorResponse(f) => f
            .file_descriptor_proto
            .iter()
            .map(|b| {
                prost_types::FileDescriptorProto::decode(b.as_slice()).map_err(|e| {
                    Some(GrpcError::ReflectionFailed {
                        message: format!("Reflection failed: an invalid file descriptor: {e}"),
                        status: None,
                    })
                })
            })
            .collect(),
        MessageResponse::ErrorResponse(e) => {
            if e.error_code == tonic::Code::NotFound as i32 {
                Err(None)
            } else {
                Err(Some(GrpcError::ReflectionFailed {
                    message: format!(
                        "Reflection failed for {what}: {}: {}",
                        code_name(e.error_code),
                        e.error_message
                    ),
                    status: Some((e.error_code, code_name(e.error_code))),
                }))
            }
        }
        _ => Err(Some(GrpcError::ReflectionFailed {
            message: format!("Reflection failed: an unexpected answer for {what}"),
            status: None,
        })),
    }
}

/// Most files one reflection may collect.
pub const REFLECTION_MAX_FILES: usize = 1000;

/// Add `list` to `files` (new names only), keeping count of the collected
/// bytes: past 10 MB or 1,000 files reflection stops.
fn collect_files(
    files: &mut BTreeMap<String, prost_types::FileDescriptorProto>,
    bytes: &mut usize,
    list: impl IntoIterator<Item = prost_types::FileDescriptorProto>,
) -> Result<(), GrpcError> {
    for f in list {
        let name = f.name().to_string();
        if files.contains_key(&name) {
            continue;
        }
        *bytes += f.encoded_len();
        if *bytes > DESCRIPTOR_MAX_BYTES {
            return Err(GrpcError::TooLarge(*bytes as u64));
        }
        if files.len() >= REFLECTION_MAX_FILES {
            return Err(GrpcError::ReflectionFailed {
                message: format!(
                    "Reflection failed: the server sent more than {REFLECTION_MAX_FILES} files"
                ),
                status: None,
            });
        }
        files.insert(name, f);
    }
    Ok(())
}

/// A bundled well-known file (`google/protobuf/*.proto`).
fn bundled_file(name: &str) -> Option<prost_types::FileDescriptorProto> {
    use protox::file::FileResolver;
    let f = protox::file::GoogleFileResolver::new()
        .open_file(name)
        .ok()?;
    Some(f.file_descriptor_proto().clone())
}

/// Ask the server for its services and every file they need (AC-GRPC-3):
/// reflection v1, falling back to v1alpha when v1 is UNIMPLEMENTED.
pub async fn reflect(
    channel: &Channel,
    metadata: &[(String, String)],
) -> Result<DescriptorPool, GrpcError> {
    let list = MessageRequest::ListServices(String::new());
    let opened =
        match ReflectionSession::open(channel.clone(), V1_PATH, metadata, list.clone()).await {
            Err(s) if s.code() == tonic::Code::Unimplemented => {
                match ReflectionSession::open(channel.clone(), V1ALPHA_PATH, metadata, list).await {
                    Err(s) if s.code() == tonic::Code::Unimplemented => {
                        return Err(GrpcError::ReflectionUnsupported);
                    }
                    other => other,
                }
            }
            other => other,
        };
    let (mut session, first) = opened.map_err(|s| status_failure(&s))?;
    let services: Vec<String> = match first.message_response {
        Some(MessageResponse::ListServicesResponse(l)) => {
            l.service.into_iter().map(|s| s.name).collect()
        }
        Some(MessageResponse::ErrorResponse(e)) => {
            return Err(GrpcError::ReflectionFailed {
                message: format!(
                    "Reflection failed: {}: {}",
                    code_name(e.error_code),
                    e.error_message
                ),
                status: Some((e.error_code, code_name(e.error_code))),
            });
        }
        _ => {
            return Err(GrpcError::ReflectionFailed {
                message: "Reflection failed: the server didn't list its services".into(),
                status: None,
            });
        }
    };
    let mut files: BTreeMap<String, prost_types::FileDescriptorProto> = BTreeMap::new();
    let mut bytes = 0usize;
    for s in services.iter().filter(|s| !is_reflection_service(s)) {
        let answer = session
            .ask(MessageRequest::FileContainingSymbol(s.clone()))
            .await?;
        match files_of(answer, s) {
            Ok(list) => collect_files(&mut files, &mut bytes, list)?,
            Err(Some(e)) => return Err(e),
            Err(None) => {
                return Err(GrpcError::ReflectionFailed {
                    message: format!("Reflection failed: the server couldn't describe {s}"),
                    status: None,
                });
            }
        }
    }
    // Dependencies the answers left out.
    let mut asked: BTreeSet<String> = BTreeSet::new();
    loop {
        let missing: Vec<String> = files
            .values()
            .flat_map(|f| f.dependency.iter().cloned())
            .filter(|d| !files.contains_key(d) && !asked.contains(d))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        if missing.is_empty() {
            break;
        }
        for name in missing {
            asked.insert(name.clone());
            let answer = session
                .ask(MessageRequest::FileByFilename(name.clone()))
                .await?;
            match files_of(answer, &name) {
                Ok(list) => collect_files(&mut files, &mut bytes, list)?,
                Err(Some(e)) => return Err(e),
                Err(None) => match bundled_file(&name) {
                    Some(f) => collect_files(&mut files, &mut bytes, [f])?,
                    None => {
                        return Err(GrpcError::ReflectionFailed {
                            message: format!("Reflection failed: the server couldn't send {name}"),
                            status: None,
                        });
                    }
                },
            }
        }
    }
    drop(session);
    let mut pool = DescriptorPool::new();
    pool.add_file_descriptor_protos(files.into_values())
        .map_err(|e| GrpcError::ReflectionFailed {
            message: format!("The server's descriptors don't build: {e}"),
            status: None,
        })?;
    if user_services(&pool) == 0 {
        return Err(GrpcError::ReflectionFailed {
            message: "The server lists no services".into(),
            status: None,
        });
    }
    if pool.encode_to_vec().len() > DESCRIPTOR_MAX_BYTES {
        return Err(GrpcError::TooLarge(pool.encode_to_vec().len() as u64));
    }
    Ok(pool)
}

/// Connect and reflect within the request's timeout, cancellable.
pub async fn fetch_reflection(
    p: &PreparedGrpc,
    cancel: &CancellationToken,
) -> Result<DescriptorPool, GrpcError> {
    let work = async {
        let channel = connect(&p.target, p.verify_tls, 0)
            .await
            .map_err(GrpcError::Network)?;
        reflect(&channel, &p.metadata).await
    };
    let limited = async {
        if p.timeout_ms == 0 {
            work.await
        } else {
            match tokio::time::timeout(Duration::from_millis(p.timeout_ms), work).await {
                Ok(r) => r,
                Err(_) => Err(GrpcError::Network(err(
                    ErrorKind::Timeout,
                    format!("Timed out after {}", human_ms(p.timeout_ms)),
                ))),
            }
        }
    };
    tokio::select! {
        r = limited => r,
        _ = cancel.cancelled() => Err(GrpcError::Network(err(ErrorKind::Cancelled, "Cancelled"))),
    }
}

// --------------------------------------------------------------------- JSON

/// JSON text → a message of `desc` (proto3 JSON mapping; unknown fields
/// refused). An empty text is `{}`.
pub fn message_from_json(desc: &MessageDescriptor, json: &str) -> Result<DynamicMessage, String> {
    let text = if json.trim().is_empty() { "{}" } else { json };
    let mut de = serde_json::Deserializer::from_str(text);
    let out = DynamicMessage::deserialize_with_options(
        desc.clone(),
        &mut de,
        &prost_reflect::DeserializeOptions::new(),
    )
    .and_then(|m| de.end().map(|_| m));
    out.map_err(|e| {
        use serde_json::error::Category;
        match e.classify() {
            Category::Syntax | Category::Eof => format!(
                "Invalid JSON at line {}, col {}: {}",
                e.line(),
                e.column(),
                strip_position(&e.to_string())
            ),
            _ => format!(
                "The message doesn't match {}: {}",
                desc.full_name(),
                strip_position(&e.to_string())
            ),
        }
    })
}

/// "foo at line 1 column 2" → "foo".
fn strip_position(s: &str) -> String {
    match s.rfind(" at line ") {
        Some(i) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// A message as pretty JSON with every field (defaults included).
pub fn message_to_json(msg: &DynamicMessage) -> String {
    let opts = prost_reflect::SerializeOptions::new().skip_default_fields(false);
    let mut out = Vec::new();
    let mut ser = serde_json::Serializer::with_formatter(
        &mut out,
        serde_json::ser::PrettyFormatter::with_indent(b"  "),
    );
    match msg.serialize_with_options(&mut ser, &opts) {
        Ok(()) => String::from_utf8(out).unwrap_or_default(),
        Err(e) => format!("\"<unprintable message: {e}>\""),
    }
}

// ----------------------------------------------------------- status details

/// `google.rpc.Status` (the payload of `grpc-status-details-bin`).
#[derive(Clone, PartialEq, prost::Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}

const ERROR_DETAILS_PROTO: &str = r#"syntax = "proto3";
package google.rpc;
import "google/protobuf/duration.proto";
message ErrorInfo { string reason = 1; string domain = 2; map<string, string> metadata = 3; }
message RetryInfo { google.protobuf.Duration retry_delay = 1; }
message DebugInfo { repeated string stack_entries = 1; string detail = 2; }
message QuotaFailure {
  message Violation { string subject = 1; string description = 2; }
  repeated Violation violations = 1;
}
message PreconditionFailure {
  message Violation { string type = 1; string subject = 2; string description = 3; }
  repeated Violation violations = 1;
}
message BadRequest {
  message FieldViolation { string field = 1; string description = 2; string reason = 3; LocalizedMessage localized_message = 4; }
  repeated FieldViolation field_violations = 1;
}
message RequestInfo { string request_id = 1; string serving_data = 2; }
message ResourceInfo { string resource_type = 1; string resource_name = 2; string owner = 3; string description = 4; }
message Help {
  message Link { string description = 1; string url = 2; }
  repeated Link links = 1;
}
message LocalizedMessage { string locale = 1; string message = 2; }
"#;

/// The bundled `google.rpc` error details.
fn error_details_pool() -> Option<&'static DescriptorPool> {
    static POOL: std::sync::OnceLock<Option<DescriptorPool>> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        use protox::file::{ChainFileResolver, File, FileResolver, GoogleFileResolver};
        struct Inline;
        impl FileResolver for Inline {
            fn open_file(&self, name: &str) -> Result<File, protox::Error> {
                if name == "google/rpc/error_details.proto" {
                    File::from_source(name, ERROR_DETAILS_PROTO)
                } else {
                    Err(protox::Error::file_not_found(name))
                }
            }
        }
        let mut chain = ChainFileResolver::new();
        chain.add(Inline);
        chain.add(GoogleFileResolver::new());
        let mut c = protox::Compiler::with_file_resolver(chain);
        c.include_imports(true);
        c.open_file("google/rpc/error_details.proto").ok()?;
        Some(c.descriptor_pool())
    })
    .as_ref()
}

fn decode_detail(any: &prost_types::Any, pool: Option<&DescriptorPool>) -> GrpcStatusDetail {
    let name = any.type_url.rsplit('/').next().unwrap_or("");
    let desc = pool
        .and_then(|p| p.get_message_by_name(name))
        .or_else(|| error_details_pool().and_then(|p| p.get_message_by_name(name)));
    let json = desc
        .and_then(|d| DynamicMessage::decode(d, any.value.as_slice()).ok())
        .map(|m| message_to_json(&m));
    GrpcStatusDetail {
        type_url: any.type_url.clone(),
        json,
        base64: base64::engine::general_purpose::STANDARD.encode(&any.value),
    }
}

/// A tonic status as shown (AC-GRPC-7), its details decoded with `pool`
/// (the call's) and the bundled error details.
pub fn status_of(st: &tonic::Status, pool: Option<&DescriptorPool>) -> GrpcStatus {
    let code = st.code() as i32;
    let details = if st.details().is_empty() {
        Vec::new()
    } else {
        RpcStatus::decode(st.details())
            .map(|s| s.details.iter().map(|d| decode_detail(d, pool)).collect())
            .unwrap_or_default()
    };
    GrpcStatus {
        code,
        name: code_name(code),
        message: st.message().to_string(),
        details,
    }
}

fn ok_status() -> GrpcStatus {
    GrpcStatus {
        code: 0,
        name: "OK".into(),
        message: String::new(),
        details: Vec::new(),
    }
}

fn local_status(code: tonic::Code, message: impl Into<String>) -> GrpcStatus {
    let c = code as i32;
    GrpcStatus {
        code: c,
        name: code_name(c),
        message: message.into(),
        details: Vec::new(),
    }
}

// -------------------------------------------------------------------- codec

/// Dynamic messages on the wire: encodes any [`DynamicMessage`], decodes the
/// method's output type.
#[derive(Clone)]
struct DynCodec(MessageDescriptor);

struct DynEncoder;
struct DynDecoder(MessageDescriptor);

impl tonic::codec::Codec for DynCodec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = DynEncoder;
    type Decoder = DynDecoder;
    fn encoder(&mut self) -> Self::Encoder {
        DynEncoder
    }
    fn decoder(&mut self) -> Self::Decoder {
        DynDecoder(self.0.clone())
    }
}

impl tonic::codec::Encoder for DynEncoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;
    fn encode(
        &mut self,
        item: DynamicMessage,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), tonic::Status> {
        item.encode(dst)
            .map_err(|e| tonic::Status::internal(format!("Couldn't encode the message: {e}")))
    }
}

impl tonic::codec::Decoder for DynDecoder {
    type Item = DynamicMessage;
    type Error = tonic::Status;
    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<DynamicMessage>, tonic::Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|e| {
                tonic::Status::internal(format!(
                    "Couldn't decode the answer as {}: {e}",
                    self.0.full_name()
                ))
            })
    }
}

// ------------------------------------------------------------------- frames

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GrpcFrameKind {
    Message,
    Headers,
    Trailers,
    End,
    Status,
    Open,
    Error,
    Info,
    Close,
}

/// One entry of a stream's message log (AC-GRPC-8), the `WsFrame` shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrpcFrame {
    pub dir: Dir,
    pub ts: i64,
    pub kind: GrpcFrameKind,
    pub data: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Vec<GrpcStatusDetail>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last: bool,
}

impl GrpcFrame {
    pub fn new(dir: Dir, kind: GrpcFrameKind, data: impl Into<String>, size: u64) -> Self {
        let mut data: String = data.into();
        let truncated = data.len() > MAX_FRAME_DATA;
        if truncated {
            let mut end = MAX_FRAME_DATA;
            while !data.is_char_boundary(end) {
                end -= 1;
            }
            data.truncate(end);
        }
        GrpcFrame {
            dir,
            ts: now_ms(),
            kind,
            data,
            size,
            code: None,
            details: None,
            truncated,
            last: false,
        }
    }
    fn system(kind: GrpcFrameKind, data: impl Into<String>) -> Self {
        GrpcFrame::new(Dir::System, kind, data, 0)
    }
}

/// A frame as the UI may see it: the system frames (open, error, status,
/// close, info) carry connection details and error texts that can hold
/// resolved secrets, so they pass through `redactor`. Message data is the
/// server's own and stays as received.
pub fn redact_frame(redactor: &Redactor, mut frame: GrpcFrame) -> GrpcFrame {
    match frame.kind {
        GrpcFrameKind::Open
        | GrpcFrameKind::Error
        | GrpcFrameKind::Status
        | GrpcFrameKind::Close
        | GrpcFrameKind::Info => {
            // Only the texts: the enum fields must stay what they are.
            frame.data = redactor.redact(&frame.data);
            if let Some(d) = frame.details.as_mut() {
                redact_details(redactor, d);
            }
            frame
        }
        _ => frame,
    }
}

fn redact_details(redactor: &Redactor, details: &mut [GrpcStatusDetail]) {
    for d in details {
        d.type_url = redactor.redact(&d.type_url);
        d.json = d.json.as_deref().map(|j| redactor.redact(j));
        d.base64 = redactor.redact(&d.base64);
    }
}

/// A status with its texts masked (name and code stay).
pub fn redact_status(redactor: &Redactor, mut st: GrpcStatus) -> GrpcStatus {
    st.message = redactor.redact(&st.message);
    redact_details(redactor, &mut st.details);
    st
}

/// An error with its message masked (the kind stays).
pub fn redact_send_error(redactor: &Redactor, mut e: SendError) -> SendError {
    e.message = redactor.redact(&e.message);
    e
}

fn pairs_text(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}: {v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

// -------------------------------------------------------------------- calls

/// What the owner of a call asks it to do.
#[derive(Debug)]
pub enum GrpcCommand {
    /// Send a message (already resolved and parsed); the reply says whether
    /// it was queued.
    Send(DynamicMessage, String, oneshot::Sender<Result<(), String>>),
    /// Close the sending side.
    End,
    /// Abort the call (CANCELLED).
    Cancel,
}

/// Outcome of a call, for the result and history.
#[derive(Debug, Clone, Default)]
pub struct CallStats {
    pub duration_ms: u64,
    pub sent: u64,
    pub received: u64,
    /// Set when the call got a status (any code).
    pub status: Option<GrpcStatus>,
    /// Set when the connection failed or the call was cancelled before a
    /// status.
    pub error: Option<SendError>,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    /// The first received message (JSON, encoded size).
    pub first_response: Option<(String, u64)>,
    /// The HTTP/2 connection was up.
    pub connected: bool,
}

/// Everything a call needs besides the connection.
#[derive(Clone)]
pub struct CallPlan {
    pub prepared: PreparedGrpc,
    pub method: MethodDescriptor,
    /// The first message (unary, server streaming).
    pub first: Option<(DynamicMessage, String)>,
}

impl CallPlan {
    pub fn call_kind(&self) -> GrpcCallKind {
        GrpcCallKind::from_streaming(
            self.method.is_client_streaming(),
            self.method.is_server_streaming(),
        )
    }
}

/// Resolve the method and parse the call's message (unary and server
/// streaming send it at once).
pub fn plan(prepared: PreparedGrpc, pool: &DescriptorPool) -> Result<CallPlan, GrpcError> {
    let method = method_of(pool, &prepared)?;
    let kind =
        GrpcCallKind::from_streaming(method.is_client_streaming(), method.is_server_streaming());
    let first = if kind.single_request() {
        let msg = message_from_json(&method.input(), &prepared.message)
            .map_err(|m| GrpcError::Prepare(PrepareError::InvalidGrpcMessage(m)))?;
        Some((msg, prepared.message.clone()))
    } else {
        None
    };
    Ok(CallPlan {
        prepared,
        method,
        first,
    })
}

/// Resolve a message sent on an open stream (fresh `{{$…}}` values).
pub fn resolve_stream_message(
    text: &str,
    scope: &VarScope,
    input: &MessageDescriptor,
) -> Result<(DynamicMessage, String), String> {
    let mut u = Unresolved::default();
    let resolved = scope.resolve_in(text, ResolveOptions::default(), &mut u, &DynamicCtx::new());
    let msg = message_from_json(input, &resolved)?;
    Ok((msg, resolved))
}

/// Events queued between a call's reader and its emitter: a full queue
/// stops the reader, so a flooding server is held back by HTTP/2 flow control.
const EVENT_QUEUE: usize = 256;

enum Event {
    Headers(MetadataMap),
    Message(DynamicMessage),
    Done(Result<Option<MetadataMap>, tonic::Status>),
}

fn request_stream(
    rx: mpsc::UnboundedReceiver<DynamicMessage>,
) -> impl futures_util::Stream<Item = DynamicMessage> + Send + 'static {
    futures_util::stream::unfold(rx, |mut rx| async move { rx.recv().await.map(|m| (m, rx)) })
}

/// Run one call (AC-GRPC-7/8): connect unless `channel` is given, send the
/// planned first message (or the ones `commands` bring), stream every
/// frame to `emit` in order, and return the stats. `deadline` (unary) ends
/// the call with DEADLINE_EXCEEDED; `cancel` with CANCELLED.
pub async fn run_call(
    plan: CallPlan,
    channel: Option<Channel>,
    mut commands: mpsc::UnboundedReceiver<GrpcCommand>,
    mut emit: impl FnMut(GrpcFrame),
    cancel: CancellationToken,
    deadline: Option<Duration>,
) -> CallStats {
    let started = Instant::now();
    let p = &plan.prepared;
    let kind = plan.call_kind();
    let pool = plan.method.parent_pool().clone();
    let mut stats = CallStats::default();
    let channel = match channel {
        Some(c) => c,
        None => {
            let connecting = connect(&p.target, p.verify_tls, p.timeout_ms);
            let res = tokio::select! {
                r = connecting => r,
                _ = cancel.cancelled() => Err(err(ErrorKind::Cancelled, "Cancelled")),
            };
            match res {
                Ok(c) => c,
                Err(e) => {
                    let cancelled = e.kind == ErrorKind::Cancelled;
                    if !cancelled {
                        emit(GrpcFrame::system(GrpcFrameKind::Error, e.message.clone()));
                    }
                    let mut f = GrpcFrame::system(
                        GrpcFrameKind::Close,
                        if cancelled {
                            "Cancelled"
                        } else {
                            "Connection failed"
                        },
                    );
                    f.last = true;
                    emit(f);
                    stats.error = Some(e);
                    stats.duration_ms = started.elapsed().as_millis() as u64;
                    return stats;
                }
            }
        }
    };
    stats.connected = true;
    emit(GrpcFrame::system(
        GrpcFrameKind::Open,
        format!(
            "Connected to {} · {} ({})",
            p.target.origin(),
            p.full_method(),
            kind.label()
        ),
    ));

    let (out_tx, out_rx) = mpsc::unbounded_channel::<DynamicMessage>();
    let mut out_tx = Some(out_tx);
    if let Some((msg, text)) = plan.first.clone() {
        let size = msg.encoded_len() as u64;
        if let Some(tx) = &out_tx {
            let _ = tx.send(msg);
        }
        stats.sent += 1;
        emit(GrpcFrame::new(Dir::Out, GrpcFrameKind::Message, text, size));
        out_tx = None;
        emit(GrpcFrame::new(
            Dir::Out,
            GrpcFrameKind::End,
            "End of stream",
            0,
        ));
    }

    let (ev_tx, mut ev_rx) = mpsc::channel::<Event>(EVENT_QUEUE);
    let path = format!("/{}/{}", p.service, p.method);
    let metadata = metadata_map(&p.metadata);
    let output = plan.method.output();
    let timeout = deadline;
    let reader = tokio::spawn(async move {
        let mut grpc = tonic::client::Grpc::new(channel)
            .max_decoding_message_size(MAX_MESSAGE_BYTES)
            .max_encoding_message_size(MAX_MESSAGE_BYTES);
        if let Err(e) = grpc.ready().await {
            let _ = ev_tx
                .send(Event::Done(Err(tonic::Status::unavailable(e.to_string()))))
                .await;
            return;
        }
        let mut req = tonic::Request::new(request_stream(out_rx));
        *req.metadata_mut() = metadata;
        if let Some(t) = timeout {
            req.set_timeout(t);
        }
        let path = match http::uri::PathAndQuery::try_from(path) {
            Ok(p) => p,
            Err(e) => {
                let _ = ev_tx
                    .send(Event::Done(Err(tonic::Status::internal(format!(
                        "Invalid method path: {e}"
                    )))))
                    .await;
                return;
            }
        };
        match grpc.streaming(req, path, DynCodec(output)).await {
            Err(st) => {
                let _ = ev_tx.send(Event::Done(Err(st))).await;
            }
            Ok(resp) => {
                let _ = ev_tx.send(Event::Headers(resp.metadata().clone())).await;
                let mut inbound = resp.into_inner();
                loop {
                    match inbound.message().await {
                        Ok(Some(m)) => {
                            if ev_tx.send(Event::Message(m)).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => {
                            let tr = inbound.trailers().await;
                            let _ = ev_tx.send(Event::Done(tr)).await;
                            return;
                        }
                        Err(st) => {
                            let _ = ev_tx.send(Event::Done(Err(st))).await;
                            return;
                        }
                    }
                }
            }
        }
    });

    let sleep = async {
        match deadline {
            Some(d) => tokio::time::sleep(d).await,
            None => std::future::pending().await,
        }
    };
    tokio::pin!(sleep);
    let mut commands_open = true;
    let final_status: GrpcStatus = loop {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                break local_status(tonic::Code::Cancelled, "Cancelled by the user");
            }
            _ = &mut sleep => {
                let ms = deadline.map(|d| d.as_millis() as u64).unwrap_or_default();
                break local_status(
                    tonic::Code::DeadlineExceeded,
                    format!("Deadline exceeded after {}", human_ms(ms)),
                );
            }
            ev = ev_rx.recv() => match ev {
                Some(Event::Headers(md)) => {
                    let pairs = metadata_pairs(&md);
                    emit(GrpcFrame::new(Dir::In, GrpcFrameKind::Headers, pairs_text(&pairs), 0));
                    stats.headers = pairs;
                }
                Some(Event::Message(m)) => {
                    let size = m.encoded_len() as u64;
                    let json = message_to_json(&m);
                    stats.received += 1;
                    if stats.first_response.is_none() {
                        stats.first_response = Some((json.clone(), size));
                    }
                    emit(GrpcFrame::new(Dir::In, GrpcFrameKind::Message, json, size));
                }
                Some(Event::Done(Ok(trailers))) => {
                    let pairs = trailers.as_ref().map(metadata_pairs).unwrap_or_default();
                    if !pairs.is_empty() {
                        emit(GrpcFrame::new(Dir::In, GrpcFrameKind::Trailers, pairs_text(&pairs), 0));
                    }
                    stats.trailers = pairs;
                    break ok_status();
                }
                Some(Event::Done(Err(st))) => {
                    // Trailers-only answers carry the headers and trailers in the status.
                    let pairs: Vec<(String, String)> = metadata_pairs(st.metadata())
                        .into_iter()
                        .filter(|(k, _)| !k.starts_with("grpc-"))
                        .collect();
                    if !pairs.is_empty() {
                        emit(GrpcFrame::new(Dir::In, GrpcFrameKind::Trailers, pairs_text(&pairs), 0));
                    }
                    stats.trailers = pairs;
                    break status_of(&st, Some(&pool));
                }
                None => break local_status(tonic::Code::Unknown, "The call ended without a status"),
            },
            cmd = commands.recv(), if commands_open => match cmd {
                Some(GrpcCommand::Send(msg, text, reply)) => match &out_tx {
                    Some(tx) => {
                        let size = msg.encoded_len() as u64;
                        if tx.send(msg).is_ok() {
                            stats.sent += 1;
                            emit(GrpcFrame::new(Dir::Out, GrpcFrameKind::Message, text, size));
                            let _ = reply.send(Ok(()));
                        } else {
                            let _ = reply.send(Err("The call has ended".into()));
                        }
                    }
                    None => {
                        let _ = reply.send(Err("The stream's sending side is closed".into()));
                    }
                },
                Some(GrpcCommand::End) => {
                    if out_tx.take().is_some() {
                        emit(GrpcFrame::new(Dir::Out, GrpcFrameKind::End, "End of stream", 0));
                    }
                }
                Some(GrpcCommand::Cancel) => {
                    break local_status(tonic::Code::Cancelled, "Cancelled by the user");
                }
                None => commands_open = false,
            },
        }
    };
    // Dropping the reader resets the HTTP/2 stream (RST_STREAM CANCEL).
    reader.abort();
    drop(out_tx);
    let mut f = GrpcFrame::system(
        GrpcFrameKind::Status,
        if final_status.message.is_empty() {
            final_status.name.clone()
        } else {
            format!("{}: {}", final_status.name, final_status.message)
        },
    );
    f.code = Some(final_status.code);
    if !final_status.details.is_empty() {
        f.details = Some(final_status.details.clone());
    }
    emit(f);
    let cancelled = final_status.code == tonic::Code::Cancelled as i32
        && final_status.message == "Cancelled by the user";
    let mut close = GrpcFrame::system(
        GrpcFrameKind::Close,
        if cancelled {
            "Cancelled".to_string()
        } else if final_status.code == 0 {
            "Finished · 0 OK".to_string()
        } else {
            format!("Failed · {} {}", final_status.code, final_status.name)
        },
    );
    close.code = Some(final_status.code);
    close.last = true;
    emit(close);
    stats.status = Some(final_status);
    stats.duration_ms = started.elapsed().as_millis() as u64;
    stats
}

/// A unary call (AC-GRPC-7): the plan's message, the request's deadline.
pub async fn invoke(
    plan: CallPlan,
    channel: Option<Channel>,
    cancel: CancellationToken,
) -> CallStats {
    // A streaming method can't be a one-shot call: a client or bidi call
    // would wait for messages that never come. Nothing is sent.
    let kind = plan.call_kind();
    if kind != GrpcCallKind::Unary {
        return CallStats {
            error: Some(err(
                ErrorKind::Other,
                GrpcError::StreamingMethod(kind).to_string(),
            )),
            ..Default::default()
        };
    }
    let deadline =
        (plan.prepared.timeout_ms > 0).then(|| Duration::from_millis(plan.prepared.timeout_ms));
    let (_tx, rx) = mpsc::unbounded_channel();
    // A cancel before any status is a cancelled send (AC-REQ-8), like HTTP.
    let mut stats = run_call(plan, channel, rx, |_| {}, cancel.clone(), deadline).await;
    if cancel.is_cancelled()
        && stats
            .status
            .as_ref()
            .is_some_and(|s| s.code == tonic::Code::Cancelled as i32)
    {
        stats.status = None;
        stats.error = Some(err(ErrorKind::Cancelled, "Cancelled"));
    }
    stats
}

// ------------------------------------------------------------------ history

/// The history entry of a finished call (AC-GRPC-10). Everything passes
/// through `redactor`.
pub fn history_entry(
    id: &str,
    started_at: i64,
    request: &Request,
    prepared: &PreparedGrpc,
    call_kind: GrpcCallKind,
    stats: &CallStats,
    redactor: &Redactor,
) -> HistoryEntry {
    let unary = call_kind == GrpcCallKind::Unary;
    let (response, truncated) = match (&stats.first_response, unary) {
        (Some((json, _)), true) => {
            if json.len() > HISTORY_RESPONSE_MAX {
                let mut end = HISTORY_RESPONSE_MAX;
                while !json.is_char_boundary(end) {
                    end -= 1;
                }
                (Some(json[..end].to_string()), true)
            } else {
                (Some(json.clone()), false)
            }
        }
        _ => (None, false),
    };
    let entry = HistoryEntry {
        id: id.to_string(),
        timestamp: started_at,
        source_request_id: (!request.collection_id.is_empty()).then(|| request.id.clone()),
        request: request.clone(),
        resolved_url: prepared.display_url.clone(),
        method: "GRPC".into(),
        response: None,
        error: stats.error.clone(),
        ws: None,
        grpc: Some(GrpcCallSummary {
            service: prepared.service.clone(),
            method: prepared.method.clone(),
            call_kind,
            duration_ms: stats.duration_ms,
            sent: stats.sent,
            received: stats.received,
            status: stats.status.clone(),
            response,
            response_truncated: truncated,
            headers: unary.then(|| stats.headers.clone()),
            trailers: unary.then(|| stats.trailers.clone()),
            extra: Default::default(),
        }),
    };
    redactor.redact_struct(&entry)
}

/// A fresh history id for a call about to start.
pub fn new_history_id() -> String {
    crate::model::new_id()
}

// ------------------------------------------------------------------- export

/// A grpcurl command for `request` (AC-GRPC-11). `keep_variables` leaves
/// `{{name}}` and `{{$name}}` literal.
pub fn export_grpcurl(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    keep_variables: bool,
) -> Result<String, PrepareError> {
    use crate::curl::shell::quote;
    let empty = VarScope::empty();
    let scope = if keep_variables { &empty } else { scope };
    let dynamic = if keep_variables {
        DynamicCtx::keep()
    } else {
        DynamicCtx::new()
    };
    let p = match prepare_grpc(request, scope, collection_auth, true, &dynamic) {
        Ok(p) => p,
        // `{{host}}` kept literal may not parse as a URL: export the text.
        Err(PrepareError::InvalidGrpcUrl(_)) if keep_variables => {
            return export_unparsed(request, scope, collection_auth, &dynamic);
        }
        Err(e) => return Err(e),
    };
    let mut parts = vec!["grpcurl".to_string()];
    if !p.target.tls {
        parts.push("-plaintext".into());
    } else if !p.verify_tls {
        parts.push("-insecure".into());
    }
    for (k, v) in &p.metadata {
        parts.push(format!("-H {}", quote(&format!("{k}: {v}"))));
    }
    if p.proto_source == ProtoSource::Files {
        let (dirs, names) = proto_args(&p.proto_files, &p.import_paths);
        for dir in dirs {
            parts.push(format!("-import-path {}", quote(&dir)));
        }
        for name in names {
            parts.push(format!("-proto {}", quote(&name)));
        }
    }
    parts.push(format!("-d {}", quote(&p.message)));
    if p.timeout_ms > 0 {
        parts.push(format!("-max-time {}", seconds(p.timeout_ms)));
    }
    parts.push(quote(&p.target.address()));
    parts.push(quote(&p.full_method()));
    Ok(parts.join(crate::curl::LINE_JOIN))
}

fn export_unparsed(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    dynamic: &DynamicCtx,
) -> Result<String, PrepareError> {
    use crate::curl::shell::quote;
    let cfg = request.grpc.clone().unwrap_or_default();
    if cfg.service.trim().is_empty() || cfg.method.trim().is_empty() {
        return Err(PrepareError::NoGrpcMethod);
    }
    let mut http = request.clone();
    http.kind = RequestKind::Http;
    http.params = Vec::new();
    http.body = Default::default();
    http.grpc = None;
    http.auth = grpc_auth(request, collection_auth)?;
    let pr = prepare_in(&http, scope, None, "", dynamic)?;
    let mut parts = vec!["grpcurl".to_string()];
    for (k, v) in check_metadata(pr.headers)? {
        parts.push(format!("-H {}", quote(&format!("{k}: {v}"))));
    }
    let msg = if cfg.message.trim().is_empty() {
        "{}".to_string()
    } else {
        cfg.message.clone()
    };
    parts.push(format!("-d {}", quote(&msg)));
    let addr = pr
        .url
        .split_once("://")
        .map_or(pr.url.as_str(), |(_, r)| r)
        .trim_end_matches('/');
    parts.push(quote(addr));
    parts.push(quote(&format!(
        "{}/{}",
        cfg.service.trim(),
        cfg.method.trim()
    )));
    Ok(parts.join(crate::curl::LINE_JOIN))
}

fn seconds(ms: u64) -> String {
    if ms.is_multiple_of(1000) {
        (ms / 1000).to_string()
    } else {
        format!("{}", ms as f64 / 1000.0)
    }
}

/// grpcurl `-import-path` folders (the user's first, then each file's own
/// folder unless an import path already holds it) and `-proto` names
/// relative to them.
fn proto_args(files: &[String], import_paths: &[String]) -> (Vec<String>, Vec<String>) {
    let mut dirs: Vec<String> = import_paths.to_vec();
    let mut names = Vec::new();
    for f in files {
        let path = Path::new(f);
        let under = import_paths.iter().find_map(|ip| {
            path.strip_prefix(ip)
                .ok()
                .map(|rel| rel.to_string_lossy().replace('\\', "/"))
        });
        match under {
            Some(rel) => names.push(rel),
            None => {
                let dir = path
                    .parent()
                    .map(|d| d.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if !dir.is_empty() && !dirs.contains(&dir) {
                    dirs.push(dir);
                }
                names.push(
                    path.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| f.clone()),
                );
            }
        }
    }
    (dirs, names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{GrpcConfig, KeyValue};

    fn grpc_request(url: &str) -> Request {
        Request {
            kind: RequestKind::Grpc,
            url: url.into(),
            grpc: Some(GrpcConfig {
                service: "demo.Greeter".into(),
                method: "SayHello".into(),
                message: r#"{"name":"{{$randomFirstName}}"}"#.into(),
                ..GrpcConfig::default()
            }),
            ..Request::default()
        }
    }

    #[test]
    fn ac_grpc_2_tls_follows_the_scheme() {
        let t = parse_target("grpcs://api.dev").unwrap();
        assert!(t.tls);
        assert_eq!((t.host.as_str(), t.port), ("api.dev", 443));
        let t = parse_target("grpc://localhost:50051").unwrap();
        assert!(!t.tls);
        assert_eq!(t.port, 50051);
        assert!(parse_target("https://x.dev:8443/prefix").unwrap().has_path);
        assert!(!parse_target("http://x.dev/").unwrap().has_path);
        // AC-REQ-10: no scheme.
        assert!(!parse_target("localhost:50051").unwrap().tls);
        assert!(parse_target("api.dev:443").unwrap().tls);
        let v6 = parse_target("grpc://[::1]:9000").unwrap();
        assert_eq!(v6.host, "::1");
        assert_eq!(v6.origin(), "grpc://[::1]:9000");
        assert!(parse_target("ws://x").unwrap_err().contains("Use grpc://"));
        assert!(parse_target("grpc://x:notaport").is_err());
        assert!(parse_target("grpc://").is_err());
    }

    #[test]
    fn ac_grpc_9_metadata_and_auth() {
        let mut r = grpc_request("grpc://localhost:1");
        r.headers = vec![
            KeyValue::new("X-Team", "core"),
            KeyValue::new("trace-bin", "AAEC"),
            KeyValue::new("Disabled", "x").disabled(),
        ];
        r.auth = Auth::bearer("tok");
        let p = prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap();
        assert_eq!(
            p.metadata,
            vec![
                ("x-team".to_string(), "core".to_string()),
                ("trace-bin".to_string(), "AAEC".to_string()),
                ("authorization".to_string(), "Bearer tok".to_string()),
            ]
        );
        // API key "in query" still goes as metadata.
        r.auth = Auth::api_key("x-api-key", "k1", ApiKeyLocation::Query);
        let p = prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap();
        assert!(p.metadata.contains(&("x-api-key".into(), "k1".into())));
        // An explicit authorization row wins.
        r.headers = vec![KeyValue::new("Authorization", "Bearer mine")];
        r.auth = Auth::basic("u", "p");
        let p = prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap();
        assert_eq!(
            p.metadata,
            vec![("authorization".into(), "Bearer mine".into())]
        );
        // Reserved and invalid names, bad -bin values.
        for (k, v) in [
            ("TE", "trailers"),
            ("grpc-timeout", "1S"),
            ("bad name", "x"),
            ("x-bin", "%%%"),
        ] {
            r.headers = vec![KeyValue::new(k, v)];
            let e =
                prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap_err();
            assert!(
                matches!(e, PrepareError::InvalidGrpcMetadata(_)),
                "{k}: {e:?}"
            );
        }
    }

    #[test]
    fn ac_grpc_2_validation() {
        let mut r = grpc_request("");
        assert_eq!(
            prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap_err(),
            PrepareError::EmptyUrl
        );
        r.url = "ftp://x".into();
        assert!(matches!(
            prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap_err(),
            PrepareError::InvalidGrpcUrl(_)
        ));
        r.url = "grpc://x:1".into();
        r.grpc.as_mut().unwrap().method.clear();
        assert_eq!(
            prepare_grpc(&r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap_err(),
            PrepareError::NoGrpcMethod
        );
        assert!(prepare_grpc(&r, &VarScope::empty(), None, false, &DynamicCtx::new()).is_ok());
        r.kind = RequestKind::Http;
        assert_eq!(
            prepare_grpc(&r, &VarScope::empty(), None, false, &DynamicCtx::new()).unwrap_err(),
            PrepareError::NotGrpc
        );
    }

    #[test]
    fn ac_grpc_2_variables_and_dynamic_values_resolve_in_the_message() {
        use crate::model::{Environment, Variable};
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Dev".into(),
            variables: vec![
                Variable::new("host", "localhost:7000"),
                Variable::new("token", "s3cret-token").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let scope = VarScope::new(Some(&env), None);
        let mut r = grpc_request("grpc://{{host}}");
        r.grpc.as_mut().unwrap().message =
            r#"{"name":"{{$uuid}}","again":"{{$uuid}}","t":"{{token}}"}"#.into();
        r.auth = Auth::bearer("{{token}}");
        let p = prepare_grpc(&r, &scope, None, true, &DynamicCtx::new()).unwrap();
        assert_eq!(p.target.port, 7000);
        let v: serde_json::Value = serde_json::from_str(&p.message).unwrap();
        assert_eq!(v["name"], v["again"], "one value per name per call");
        assert_ne!(v["name"], "{{$uuid}}");
        assert_eq!(v["t"], "s3cret-token");
        assert_eq!(p.display_url, "grpc://localhost:7000/demo.Greeter/SayHello");
        // The cache key masks secrets and keeps dynamic values literal.
        let mut r2 = grpc_request("grpc://{{token}}.dev:1/{{$uuid}}");
        r2.grpc.as_mut().unwrap().message = "{}".into();
        let key = descriptor_url(&r2, &scope, None).unwrap();
        assert_eq!(key, "grpc://{{token}}.dev:1");
    }

    #[test]
    fn invalid_url_errors_never_show_secret_values() {
        use crate::model::{Environment, Variable};
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Dev".into(),
            variables: vec![Variable::new("token", "s3cret-token").secret()],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let scope = VarScope::new(Some(&env), None);
        for url in [
            "{{token}}://host",
            "grpc://[{{token}}",
            "grpc://host:{{token}}",
        ] {
            let r = grpc_request(url);
            let e = prepare_grpc(&r, &scope, None, true, &DynamicCtx::new()).unwrap_err();
            assert!(matches!(e, PrepareError::InvalidGrpcUrl(_)), "{url}: {e:?}");
            assert!(!e.to_string().contains("s3cret-token"), "{url}: {e}");
        }
        // The placeholder is shown instead where the URL itself is the problem.
        let e = prepare_grpc(
            &grpc_request("{{token}}://host"),
            &scope,
            None,
            true,
            &DynamicCtx::new(),
        )
        .unwrap_err();
        assert!(e.to_string().contains("{{token}}"), "{e}");
    }

    #[test]
    fn system_frames_are_redacted_and_messages_are_not() {
        let mut red = Redactor::default();
        red.add("tok", "s3cret-token");
        let leak = "Couldn't connect to s3cret-token.dev:1";
        for kind in [
            GrpcFrameKind::Open,
            GrpcFrameKind::Error,
            GrpcFrameKind::Status,
            GrpcFrameKind::Close,
        ] {
            let f = redact_frame(&red, GrpcFrame::system(kind, leak));
            assert_eq!(f.data, "Couldn't connect to {{tok}}.dev:1");
        }
        let m = GrpcFrame::new(
            Dir::In,
            GrpcFrameKind::Message,
            "{\"a\":\"s3cret-token\"}",
            1,
        );
        assert_eq!(redact_frame(&red, m.clone()), m);
    }

    #[test]
    fn code_names() {
        assert_eq!(code_name(0), "OK");
        assert_eq!(code_name(5), "NOT_FOUND");
        assert_eq!(code_name(16), "UNAUTHENTICATED");
        assert_eq!(code_name(17), "CODE_17");
        assert_eq!(code_name(-1), "CODE_-1");
    }

    #[test]
    fn ac_grpc_11_grpcurl_export() {
        let mut r = grpc_request("grpcs://api.dev");
        r.grpc.as_mut().unwrap().message = r#"{"name":"it's"}"#.into();
        r.headers = vec![KeyValue::new("x-team", "core")];
        r.auth = Auth::bearer("tok");
        r.settings.timeout_ms = 2500;
        r.settings.verify_tls = false;
        let out = export_grpcurl(&r, &VarScope::empty(), None, false).unwrap();
        assert_eq!(
            out,
            [
                "grpcurl",
                "-insecure",
                "-H 'x-team: core'",
                "-H 'authorization: Bearer tok'",
                r#"-d '{"name":"it'\''s"}'"#,
                "-max-time 2.5",
                "'api.dev:443'",
                "'demo.Greeter/SayHello'",
            ]
            .join(crate::curl::LINE_JOIN)
        );
        r.url = "grpc://localhost:50051".into();
        r.settings.timeout_ms = 0;
        let g = r.grpc.as_mut().unwrap();
        g.proto_source = ProtoSource::Files;
        g.proto_files = vec!["/protos/demo/greeter.proto".into(), "/other/x.proto".into()];
        g.import_paths = vec!["/protos".into()];
        let out = export_grpcurl(&r, &VarScope::empty(), None, false).unwrap();
        assert!(out.contains("-plaintext"));
        assert!(out.contains("-import-path '/protos'"));
        assert!(out.contains("-import-path '/other'"));
        assert!(out.contains("-proto 'demo/greeter.proto'"));
        assert!(out.contains("-proto 'x.proto'"));
        assert!(!out.contains("-max-time"));
        // Keep {{variables}}: literal, even a host from a variable.
        r.url = "{{baseUrl}}".into();
        r.grpc.as_mut().unwrap().message = r#"{"id":"{{$uuid}}"}"#.into();
        let out = export_grpcurl(&r, &VarScope::empty(), None, true).unwrap();
        assert!(out.contains("{{$uuid}}"), "{out}");
        assert!(out.contains("'{{baseUrl}}'"), "{out}");
        // Not gRPC / no method.
        r.kind = RequestKind::Http;
        assert_eq!(
            export_grpcurl(&r, &VarScope::empty(), None, false).unwrap_err(),
            PrepareError::NotGrpc
        );
    }

    #[test]
    fn error_details_pool_builds() {
        let pool = error_details_pool().expect("bundled error details compile");
        assert!(pool.get_message_by_name("google.rpc.BadRequest").is_some());
        assert!(pool.get_message_by_name("google.rpc.RetryInfo").is_some());
    }
}

//! WebSocket and Socket.IO sessions (AC-WS-*).
//!
//! * [`prepare_ws`] turns a `ws` request into a [`PreparedWs`]: variables
//!   (environment and `{{$…}}`, one memo per connect), query params, headers
//!   and auth exactly like an HTTP send, then `http(s)` → `ws(s)` and, in
//!   Socket.IO mode, the Engine.IO path and `EIO=4&transport=websocket`.
//! * [`run`] owns one connection: it connects (TCP, TLS with the platform
//!   verifier like reqwest, or an accept-anything verifier for
//!   `verifyTls: false`), streams every frame to `emit` in order, executes
//!   [`WsCommand`]s, reconnects when asked to, and returns [`SessionStats`]
//!   for the history entry.
//! * Socket.IO v4 framing lives in [`sio`] (pure, unit-tested). Binary
//!   attachments (packet types 5 and 6) are reported as unsupported.

use crate::dynamic::DynamicCtx;
pub use crate::model::WsSessionSummary;
use crate::model::{
    Auth, ErrorKind, HistoryEntry, MessageKind, Request, RequestKind, SavedMessage, SendError,
    SocketIoConfig, new_id, now_ms,
};
use crate::prepare::{PrepareError, prepare_in};
use crate::redact::Redactor;
use crate::vars::{ResolveOptions, Unresolved, VarScope};
use base64::Engine as _;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue, Uri};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_util::sync::CancellationToken;

/// Largest message accepted from a server.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Reconnect attempts after a dropped connection (`autoReconnect`).
pub const MAX_RECONNECTS: u32 = 5;
/// Frame data longer than this is cut in the UI event (`truncated: true`).
pub const MAX_FRAME_DATA: usize = 256 * 1024;
/// A connection up this long resets the reconnect attempts (AC-WS-6).
pub const STABLE_CONNECTION: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------ prepare

/// Socket.IO part of a prepared connection.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedSio {
    pub namespace: String,
    /// CONNECT payload (resolved JSON object), if any.
    pub auth: Option<serde_json::Value>,
}

/// A connection ready to open.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedWs {
    /// `ws://` / `wss://` URL with variables resolved and params applied.
    pub url: String,
    /// The same URL with secret variables kept as `{{name}}` (history, UI).
    pub display_url: String,
    pub headers: Vec<(String, String)>,
    pub protocols: Vec<String>,
    pub socket_io: Option<PreparedSio>,
    /// Connect + handshake limit; 0 = none.
    pub timeout_ms: u64,
    pub verify_tls: bool,
    pub auto_reconnect: bool,
    /// Variables referenced but not defined (AC-ENV-3).
    pub unresolved: Vec<String>,
}

impl PreparedWs {
    /// "WS" or "SIO": the method column of history and the tree.
    pub fn method(&self) -> &'static str {
        if self.socket_io.is_some() {
            "SIO"
        } else {
            "WS"
        }
    }
}

/// Prepare a `ws` request (AC-WS-2). `collection_auth` is the parent
/// collection's auth for Inherit.
pub fn prepare_ws(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
) -> Result<PreparedWs, PrepareError> {
    if !request.is_editable() {
        return Err(PrepareError::NeedsNewerVersion);
    }
    if !request.kind.is_ws() {
        return Err(PrepareError::NotWebSocket);
    }
    let ws = request.ws.clone().unwrap_or_default();
    // The handshake is a GET without body; reuse the HTTP preparation for
    // URL, params, headers and auth.
    let mut http = request.clone();
    http.kind = RequestKind::Http;
    http.method = "GET".into();
    http.body = Default::default();
    let dynamic = DynamicCtx::new();
    let p = prepare_in(&http, scope, collection_auth, "", &dynamic)?;
    let sio = ws.socket_io.as_ref();
    let url = ws_url(&p.url, sio).map_err(PrepareError::InvalidWsUrl)?;
    let display_url = ws_url(&p.display_url, sio).unwrap_or_else(|_| p.display_url.clone());
    let mut unresolved = Unresolved::default();
    let socket_io = match sio {
        None => None,
        Some(cfg) => {
            let auth = match cfg.auth.as_deref().map(str::trim).filter(|a| !a.is_empty()) {
                None => None,
                Some(text) => {
                    let mut missing = Unresolved::default();
                    let resolved =
                        scope.resolve_in(text, ResolveOptions::default(), &mut missing, &dynamic);
                    // An undefined variable left as `{{name}}` would only surface as
                    // a confusing JSON error: name it instead.
                    if let Some(name) = missing.into_names().into_iter().next() {
                        return Err(PrepareError::UndefinedSocketIoAuthVariable(name));
                    }
                    let v: serde_json::Value = serde_json::from_str(&resolved).map_err(|e| {
                        PrepareError::InvalidSocketIoAuth(format!(
                            "line {}, column {}: {e}",
                            e.line(),
                            e.column()
                        ))
                    })?;
                    if !v.is_object() {
                        return Err(PrepareError::InvalidSocketIoAuth(
                            "it must be a JSON object".into(),
                        ));
                    }
                    Some(v)
                }
            };
            Some(PreparedSio {
                namespace: normalize_namespace(&cfg.namespace),
                auth,
            })
        }
    };
    let protocols = ws
        .protocols
        .iter()
        .map(|s| {
            scope.resolve_in(
                s.trim(),
                ResolveOptions::default(),
                &mut unresolved,
                &dynamic,
            )
        })
        .filter(|s| !s.is_empty())
        .collect();
    Ok(PreparedWs {
        url,
        display_url,
        headers: p.headers,
        protocols,
        socket_io,
        timeout_ms: p.settings.timeout_ms,
        verify_tls: p.settings.verify_tls,
        auto_reconnect: ws.auto_reconnect,
        unresolved: {
            let mut names = p.unresolved;
            for n in unresolved.into_names() {
                if !names.contains(&n) {
                    names.push(n);
                }
            }
            names
        },
    })
}

/// `/chat` → `/chat`, `chat` → `/chat`, `` → `/`.
pub fn normalize_namespace(ns: &str) -> String {
    let ns = ns.trim();
    if ns.is_empty() || ns == "/" {
        "/".into()
    } else if ns.starts_with('/') {
        ns.to_string()
    } else {
        format!("/{ns}")
    }
}

/// Map an HTTP(S) or WS(S) URL to the WebSocket URL to open. In Socket.IO
/// mode the path becomes the Engine.IO path and `EIO=4&transport=websocket`
/// is added to the query. The fragment is dropped (never sent).
pub fn ws_url(url: &str, sio: Option<&SocketIoConfig>) -> Result<String, String> {
    let url = url.trim();
    let Some(i) = url.find("://") else {
        return Err(format!(
            "\u{201c}{url}\u{201d} is not a ws://, wss://, http:// or https:// URL"
        ));
    };
    let scheme = match url[..i].to_ascii_lowercase().as_str() {
        "ws" | "http" => "ws",
        "wss" | "https" => "wss",
        other => {
            return Err(format!(
                "Use ws://, wss://, http:// or https:// (not {other}://)"
            ));
        }
    };
    let rest = &url[i + 3..];
    let rest = rest.split('#').next().unwrap_or("");
    let auth_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(auth_end);
    if authority.is_empty() {
        return Err("The URL has no host".into());
    }
    let (path, query) = match tail.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (tail, None),
    };
    let Some(cfg) = sio else {
        let path = if path.is_empty() { "/" } else { path };
        return Ok(match query {
            Some(q) => format!("{scheme}://{authority}{path}?{q}"),
            None => format!("{scheme}://{authority}{path}"),
        });
    };
    let mut sio_path = cfg.path.trim().to_string();
    if sio_path.is_empty() {
        sio_path = "/socket.io/".into();
    }
    if !sio_path.starts_with('/') {
        sio_path.insert(0, '/');
    }
    let mut q: Vec<&str> = query
        .unwrap_or("")
        .split('&')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let k = p.split('=').next().unwrap_or("");
            k != "EIO" && k != "transport"
        })
        .collect();
    q.push("EIO=4");
    q.push("transport=websocket");
    Ok(format!("{scheme}://{authority}{sio_path}?{}", q.join("&")))
}

// ----------------------------------------------------------------- messages

/// A message ready to go out (variables resolved, payload validated).
#[derive(Debug, Clone, PartialEq)]
pub enum OutMessage {
    Text(String),
    Binary(Vec<u8>),
    /// Socket.IO emit.
    Event {
        name: String,
        args: Vec<serde_json::Value>,
        ack: bool,
    },
}

/// Resolve and validate a message from the composer or the saved list
/// (AC-WS-4). `socket_io` selects Socket.IO semantics: the body is one
/// argument (JSON value, or a string for `text`; empty JSON = no argument).
pub fn resolve_message(
    msg: &SavedMessage,
    scope: &VarScope,
    socket_io: bool,
) -> Result<OutMessage, String> {
    let dynamic = DynamicCtx::new();
    let mut unresolved = Unresolved::default();
    let body = scope.resolve_in(
        &msg.body,
        ResolveOptions::default(),
        &mut unresolved,
        &dynamic,
    );
    let parse_json = |text: &str| -> Result<serde_json::Value, String> {
        serde_json::from_str(text).map_err(|e| {
            format!(
                "Invalid JSON (line {}, column {}): {e}",
                e.line(),
                e.column()
            )
        })
    };
    if socket_io {
        let name = scope.resolve_in(
            msg.event.as_deref().unwrap_or("").trim(),
            ResolveOptions::default(),
            &mut unresolved,
            &dynamic,
        );
        if name.is_empty() {
            return Err("Enter an event name".into());
        }
        if sio::RESERVED_EVENTS.contains(&name.as_str()) {
            return Err(format!(
                "\u{201c}{name}\u{201d} is a reserved Socket.IO event name"
            ));
        }
        let args = match &msg.kind {
            MessageKind::Json if body.trim().is_empty() => vec![],
            MessageKind::Json => vec![parse_json(&body)?],
            MessageKind::Text => vec![serde_json::Value::String(body)],
            MessageKind::Binary => {
                return Err("Binary arguments aren't supported in Socket.IO mode".into());
            }
            MessageKind::Other(k) => {
                return Err(format!(
                    "Message kind \u{201c}{k}\u{201d} needs a newer version of ReqHarbor"
                ));
            }
        };
        return Ok(OutMessage::Event {
            name,
            args,
            ack: msg.ack,
        });
    }
    match &msg.kind {
        MessageKind::Text => Ok(OutMessage::Text(body)),
        MessageKind::Json => {
            parse_json(&body)?;
            Ok(OutMessage::Text(body))
        }
        MessageKind::Binary => {
            let compact: String = body.chars().filter(|c| !c.is_whitespace()).collect();
            base64::engine::general_purpose::STANDARD
                .decode(compact.as_bytes())
                .map(OutMessage::Binary)
                .map_err(|e| format!("Invalid base64: {e}"))
        }
        MessageKind::Other(k) => Err(format!(
            "Message kind \u{201c}{k}\u{201d} needs a newer version of ReqHarbor"
        )),
    }
}

// ------------------------------------------------------------------- frames

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    In,
    Out,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrameKind {
    Text,
    Binary,
    Ping,
    Pong,
    Close,
    /// Socket.IO event (`event` holds its name, `data` the JSON args array).
    Event,
    /// Socket.IO acknowledgement (`ackId`, `data` = JSON args array).
    Ack,
    Error,
    /// System: connected (`data` = handshake summary).
    Open,
    /// System: the connection dropped and a reconnect is scheduled.
    Reconnecting,
    /// System: any other note.
    Info,
}

/// One entry of the message log, streamed to the UI in order (AC-WS-5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WsFrame {
    pub dir: Dir,
    /// Unix epoch ms.
    pub ts: i64,
    pub kind: FrameKind,
    /// Text; base64 for binary / ping / pong payloads; JSON args for
    /// Socket.IO events and acks; a human text for system frames.
    pub data: String,
    /// Payload size in bytes (0 for system frames).
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_id: Option<u64>,
    /// Close code (close frames, the final system close).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<u16>,
    /// `data` was cut to [`MAX_FRAME_DATA`] bytes.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    /// The final system close: nothing follows on this connection.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub last: bool,
}

impl WsFrame {
    pub fn new(dir: Dir, kind: FrameKind, data: impl Into<String>, size: u64) -> Self {
        let mut data: String = data.into();
        let truncated = data.len() > MAX_FRAME_DATA;
        if truncated {
            let mut end = MAX_FRAME_DATA;
            while !data.is_char_boundary(end) {
                end -= 1;
            }
            data.truncate(end);
        }
        WsFrame {
            dir,
            ts: now_ms(),
            kind,
            data,
            size,
            event: None,
            ack_id: None,
            code: None,
            truncated,
            last: false,
        }
    }
    fn system(kind: FrameKind, data: impl Into<String>) -> Self {
        WsFrame::new(Dir::System, kind, data, 0)
    }
    fn bytes(dir: Dir, kind: FrameKind, bytes: &[u8]) -> Self {
        WsFrame::new(
            dir,
            kind,
            base64::engine::general_purpose::STANDARD.encode(bytes),
            bytes.len() as u64,
        )
    }
}

// ----------------------------------------------------------------- commands

/// What the owner of a session asks it to do.
#[derive(Debug)]
pub enum WsCommand {
    /// Send; the reply carries the Socket.IO ack id, if one was requested.
    Send(OutMessage, oneshot::Sender<Result<Option<u64>, String>>),
    Ping(Vec<u8>),
    /// Close politely (default 1000 / "").
    Close {
        code: Option<u16>,
        reason: String,
    },
}

/// Outcome of [`run`].
#[derive(Debug, Clone, Default)]
pub struct SessionStats {
    pub summary: WsSessionSummary,
    /// Set when the first connect failed or the connection ended in error.
    pub error: Option<SendError>,
    /// The connection was open at least once.
    pub connected: bool,
}

// -------------------------------------------------------------- connecting

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Socket = WebSocketStream<Box<dyn Io>>;

fn err(kind: ErrorKind, message: impl Into<String>) -> SendError {
    SendError {
        kind,
        message: message.into(),
    }
}

mod tls {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::CryptoProvider;
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{ClientConfig, DigitallySignedStruct, SignatureScheme};
    use rustls_platform_verifier::BuilderVerifierExt;
    use std::sync::{Arc, OnceLock};

    /// `verifyTls: false`: any certificate is accepted, but the handshake
    /// signatures are still checked (the peer holds the certificate's key).
    #[derive(Debug)]
    struct AcceptAny(Arc<CryptoProvider>);

    impl ServerCertVerifier for AcceptAny {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            Ok(ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }

    fn provider() -> Arc<CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    fn build(verify: bool) -> Result<Arc<ClientConfig>, String> {
        let p = provider();
        let builder = ClientConfig::builder_with_provider(p.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?;
        let cfg = if verify {
            builder
                .with_platform_verifier()
                .map_err(|e| e.to_string())?
                .with_no_client_auth()
        } else {
            builder
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAny(p)))
                .with_no_client_auth()
        };
        Ok(Arc::new(cfg))
    }

    /// One shared client config per TLS setting. Only a config that built
    /// is cached: a failure (say, platform roots not readable yet) is tried
    /// again on the next connect.
    pub fn config(verify: bool) -> Result<Arc<ClientConfig>, String> {
        static SECURE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        static INSECURE: OnceLock<Arc<ClientConfig>> = OnceLock::new();
        let cell = if verify { &SECURE } else { &INSECURE };
        if let Some(c) = cell.get() {
            return Ok(c.clone());
        }
        let built = build(verify)?;
        Ok(cell.get_or_init(|| built).clone())
    }
}

/// What the server answered to the opening handshake.
#[derive(Debug, Clone, Default)]
struct Handshake {
    status: u16,
    protocol: Option<String>,
}

async fn open_socket(p: &PreparedWs) -> Result<(Socket, Handshake), SendError> {
    let uri: Uri = p.url.parse().map_err(|e| {
        err(
            ErrorKind::Other,
            format!("Invalid URL \"{}\": {e}", p.display_url),
        )
    })?;
    let secure = uri.scheme_str() == Some("wss");
    let host = uri
        .host()
        .ok_or_else(|| err(ErrorKind::Other, "The URL has no host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
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
                "Couldn't connect to {host}:{port}: {}",
                last.map(|e| e.to_string()).unwrap_or_default()
            ),
        )
    })?;
    let _ = tcp.set_nodelay(true);
    let stream: Box<dyn Io> = if secure {
        crate::http::install_crypto_provider();
        let cfg = tls::config(p.verify_tls).map_err(|e| err(ErrorKind::Tls, e))?;
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
        Box::new(tls)
    } else {
        Box::new(tcp)
    };

    let mut req = uri.clone().into_client_request().map_err(|e| {
        err(
            ErrorKind::Other,
            format!("Invalid URL \"{}\": {e}", p.display_url),
        )
    })?;
    const OWN: [&str; 5] = [
        "connection",
        "upgrade",
        "sec-websocket-version",
        "sec-websocket-key",
        "sec-websocket-protocol",
    ];
    let headers = req.headers_mut();
    for (k, v) in &p.headers {
        let lower = k.to_ascii_lowercase();
        if OWN.contains(&lower.as_str()) {
            continue;
        }
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|_| err(ErrorKind::Other, format!("Invalid header name \"{k}\"")))?;
        let value = HeaderValue::from_str(v).map_err(|_| {
            err(
                ErrorKind::Other,
                format!("Invalid value for header \"{k}\""),
            )
        })?;
        if lower == "host" {
            headers.insert(name, value);
        } else {
            headers.append(name, value);
        }
    }
    if !headers.contains_key("user-agent") {
        headers.insert(
            "user-agent",
            HeaderValue::from_static(crate::http::USER_AGENT),
        );
    }
    if !p.protocols.is_empty() {
        let joined = p.protocols.join(", ");
        let value = HeaderValue::from_str(&joined).map_err(|_| {
            err(
                ErrorKind::Other,
                format!("Invalid subprotocol \"{joined}\""),
            )
        })?;
        headers.insert("sec-websocket-protocol", value);
    }
    let config = WebSocketConfig::default()
        .max_message_size(Some(MAX_MESSAGE_BYTES))
        .max_frame_size(Some(MAX_MESSAGE_BYTES));
    let (socket, resp) = tokio_tungstenite::client_async_with_config(req, stream, Some(config))
        .await
        .map_err(handshake_error)?;
    let protocol = resp
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    Ok((
        socket,
        Handshake {
            status: resp.status().as_u16(),
            protocol,
        },
    ))
}

fn handshake_error(e: tokio_tungstenite::tungstenite::Error) -> SendError {
    use tokio_tungstenite::tungstenite::Error as E;
    match e {
        E::Http(resp) => {
            let status = resp.status();
            let body = resp
                .body()
                .as_deref()
                .map(|b| {
                    String::from_utf8_lossy(&b[..b.len().min(512)])
                        .trim()
                        .to_string()
                })
                .filter(|b| !b.is_empty());
            let reason = status.canonical_reason().unwrap_or("");
            err(
                ErrorKind::Other,
                match body {
                    Some(b) => format!(
                        "The server answered {} {reason} instead of switching to WebSocket: {b}",
                        status.as_u16()
                    ),
                    None => format!(
                        "The server answered {} {reason} instead of switching to WebSocket",
                        status.as_u16()
                    ),
                },
            )
        }
        E::Io(io) => err(ErrorKind::Connect, format!("Connection failed: {io}")),
        E::Protocol(p) => err(ErrorKind::Other, format!("WebSocket handshake failed: {p}")),
        other => err(
            ErrorKind::Other,
            format!("WebSocket handshake failed: {other}"),
        ),
    }
}

/// Socket.IO state after the namespace is joined.
#[derive(Debug, Clone)]
struct SioState {
    namespace: String,
    ping_interval: Duration,
    ping_timeout: Duration,
    next_ack: u64,
}

/// Connect, and in Socket.IO mode run the Engine.IO open + namespace CONNECT
/// exchange. Returns the open socket and the "Connected" summary.
async fn connect(
    p: &PreparedWs,
    emit: &(dyn Fn(WsFrame) + Send + Sync),
) -> Result<(Socket, Option<SioState>, String), SendError> {
    let (mut socket, hs) = open_socket(p).await?;
    let mut summary = format!(
        "Connected to {} ({} Switching Protocols)",
        p.display_url, hs.status
    );
    if let Some(proto) = &hs.protocol {
        summary.push_str(&format!(" · subprotocol {proto}"));
    }
    let Some(cfg) = &p.socket_io else {
        return Ok((socket, None, summary));
    };
    // Engine.IO OPEN.
    let open = loop {
        match socket.next().await {
            Some(Ok(Message::Text(t))) => match sio::decode_eio(t.as_str()) {
                Ok(sio::EioPacket::Open(json)) => break json,
                Ok(_) => continue,
                Err(e) => {
                    return Err(err(
                        ErrorKind::Other,
                        format!("Not a Socket.IO server: {e}"),
                    ));
                }
            },
            Some(Ok(Message::Close(f))) => {
                return Err(err(
                    ErrorKind::Other,
                    format!(
                        "The server closed the connection{}",
                        close_suffix(f.as_ref())
                    ),
                ));
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(err(ErrorKind::Connect, format!("Connection failed: {e}"))),
            None => return Err(err(ErrorKind::Connect, "The server closed the connection")),
        }
    };
    let hs: sio::EioHandshake = serde_json::from_str(&open).map_err(|e| {
        err(
            ErrorKind::Other,
            format!("Invalid Engine.IO handshake: {e}"),
        )
    })?;
    let connect = sio::encode(&sio::SioPacket::Connect {
        namespace: cfg.namespace.clone(),
        data: cfg.auth.clone(),
    });
    socket
        .send(Message::text(connect))
        .await
        .map_err(|e| err(ErrorKind::Connect, format!("Connection failed: {e}")))?;
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(t))) => match sio::decode_eio(t.as_str()) {
                Ok(sio::EioPacket::Ping(d)) => {
                    emit(WsFrame::new(
                        Dir::In,
                        FrameKind::Ping,
                        d.clone(),
                        d.len() as u64,
                    ));
                    let _ = socket.send(Message::text(format!("3{d}"))).await;
                    emit(WsFrame::new(
                        Dir::Out,
                        FrameKind::Pong,
                        d.clone(),
                        d.len() as u64,
                    ));
                }
                Ok(sio::EioPacket::Message(m)) => match sio::decode(&m) {
                    Ok(sio::SioPacket::Connect { namespace, data })
                        if namespace == cfg.namespace =>
                    {
                        summary = format!(
                            "Connected to {} · Socket.IO namespace {namespace}{}",
                            p.display_url,
                            data.as_ref()
                                .and_then(|d| d.get("sid"))
                                .and_then(|s| s.as_str())
                                .map(|s| format!(" · sid {s}"))
                                .unwrap_or_default()
                        );
                        return Ok((
                            socket,
                            Some(SioState {
                                namespace,
                                ping_interval: Duration::from_millis(hs.ping_interval),
                                ping_timeout: Duration::from_millis(hs.ping_timeout),
                                next_ack: 0,
                            }),
                            summary,
                        ));
                    }
                    Ok(sio::SioPacket::ConnectError { namespace, data })
                        if namespace == cfg.namespace =>
                    {
                        let msg = data
                            .as_ref()
                            .and_then(|d| {
                                d.get("message")
                                    .and_then(|m| m.as_str())
                                    .map(str::to_string)
                            })
                            .or_else(|| data.as_ref().map(|d| d.to_string()))
                            .unwrap_or_else(|| "refused".into());
                        return Err(err(
                            ErrorKind::Other,
                            format!("Socket.IO refused namespace {namespace}: {msg}"),
                        ));
                    }
                    _ => continue,
                },
                Ok(sio::EioPacket::Close) => {
                    return Err(err(
                        ErrorKind::Other,
                        "The Socket.IO server closed the session",
                    ));
                }
                _ => continue,
            },
            Some(Ok(Message::Close(f))) => {
                return Err(err(
                    ErrorKind::Other,
                    format!(
                        "The server closed the connection{}",
                        close_suffix(f.as_ref())
                    ),
                ));
            }
            Some(Ok(_)) => continue,
            Some(Err(e)) => return Err(err(ErrorKind::Connect, format!("Connection failed: {e}"))),
            None => return Err(err(ErrorKind::Connect, "The server closed the connection")),
        }
    }
}

fn close_suffix(f: Option<&CloseFrame>) -> String {
    match f {
        Some(f) if !f.reason.is_empty() => {
            format!(" ({} {})", u16::from(f.code), f.reason.as_str())
        }
        Some(f) => format!(" ({})", u16::from(f.code)),
        None => String::new(),
    }
}

// ------------------------------------------------------------------ session

/// How one connection ended.
enum End {
    /// The owner asked to close (or the command channel was dropped).
    Closed { code: u16, reason: String },
    /// The server sent a close frame / Socket.IO disconnect.
    ServerClosed {
        code: Option<u16>,
        reason: String,
        reconnect: bool,
    },
    /// Network error, protocol error or ping timeout.
    Dropped(SendError),
    /// The app is exiting.
    Cancelled,
}

/// Run one session until it ends (AC-WS-3..7). Every frame goes to `emit`
/// in order; the last one is a system `close` with `last: true`.
pub async fn run(
    prepared: PreparedWs,
    mut commands: mpsc::UnboundedReceiver<WsCommand>,
    emit: impl Fn(WsFrame) + Send + Sync,
    cancel: CancellationToken,
) -> SessionStats {
    let started = std::time::Instant::now();
    let mut stats = SessionStats::default();
    let mut attempt = 0u32;
    let timeout = (prepared.timeout_ms > 0).then(|| Duration::from_millis(prepared.timeout_ms));
    let final_close = loop {
        let connecting = async {
            match timeout {
                Some(t) => match tokio::time::timeout(t, connect(&prepared, &emit)).await {
                    Ok(r) => r,
                    Err(_) => Err(err(
                        ErrorKind::Timeout,
                        format!("Timed out after {} ms", prepared.timeout_ms),
                    )),
                },
                None => connect(&prepared, &emit).await,
            }
        };
        // While connecting, a Close (Disconnect) or app exit stops everything.
        let outcome = tokio::select! {
            biased;
            _ = cancel.cancelled() => None,
            r = connecting => Some(r),
            _ = wait_for_close(&mut commands) => None,
        };
        let Some(outcome) = outcome else {
            break (None, "Connection cancelled".to_string());
        };
        let (socket, sio_state, summary) = match outcome {
            Ok(x) => x,
            Err(e) => {
                emit(WsFrame::system(FrameKind::Error, e.message.clone()));
                if stats.connected && prepared.auto_reconnect && attempt < MAX_RECONNECTS {
                    attempt += 1;
                    if !backoff(attempt, &mut commands, &cancel, &emit).await {
                        break (None, "Disconnected".to_string());
                    }
                    continue;
                }
                stats.error = Some(e);
                break (None, "Connection failed".to_string());
            }
        };
        stats.connected = true;
        stats.error = None;
        emit(WsFrame::system(FrameKind::Open, summary));
        let up_since = std::time::Instant::now();
        let end = session(
            socket,
            sio_state,
            &mut commands,
            &emit,
            &cancel,
            &mut stats.summary,
        )
        .await;
        // Only a connection that stayed up earns a fresh set of reconnects:
        // a server that accepts and drops at once gets MAX_RECONNECTS in all.
        if up_since.elapsed() >= STABLE_CONNECTION {
            attempt = 0;
        }
        match end {
            End::Closed { code, reason } => break (Some(code), reason),
            End::Cancelled => break (Some(1001), "ReqHarbor closed".to_string()),
            End::ServerClosed {
                code,
                reason,
                reconnect,
            } => {
                let text = match (code, reason.is_empty()) {
                    (Some(c), false) => format!("Server closed the connection: {c} {reason}"),
                    (Some(c), true) => format!("Server closed the connection: {c}"),
                    (None, _) => "Server closed the connection".to_string(),
                };
                if reconnect && prepared.auto_reconnect && attempt < MAX_RECONNECTS {
                    emit(WsFrame::system(FrameKind::Info, text));
                    attempt += 1;
                    if !backoff(attempt, &mut commands, &cancel, &emit).await {
                        break (code, reason);
                    }
                    continue;
                }
                break (code, reason);
            }
            End::Dropped(e) => {
                emit(WsFrame::system(FrameKind::Error, e.message.clone()));
                if prepared.auto_reconnect && attempt < MAX_RECONNECTS {
                    attempt += 1;
                    if !backoff(attempt, &mut commands, &cancel, &emit).await {
                        break (None, "Disconnected".to_string());
                    }
                    continue;
                }
                stats.error = Some(e);
                break (None, "Connection lost".to_string());
            }
        }
    };
    let (code, reason) = final_close;
    stats.summary.duration_ms = started.elapsed().as_millis() as u64;
    stats.summary.close_code = code;
    stats.summary.close_reason = (!reason.is_empty()).then(|| reason.clone());
    let text = match code {
        Some(c) if !reason.is_empty() => format!("Disconnected · {c} {reason}"),
        Some(c) => format!("Disconnected · {c}"),
        None if !reason.is_empty() => reason,
        None => "Disconnected".to_string(),
    };
    let mut f = WsFrame::system(FrameKind::Close, text);
    f.code = code;
    f.last = true;
    emit(f);
    // Anything still queued can't be sent any more.
    commands.close();
    while let Ok(cmd) = commands.try_recv() {
        if let WsCommand::Send(_, reply) = cmd {
            let _ = reply.send(Err("Not connected".into()));
        }
    }
    stats
}

/// Resolve when a Close arrives (Sends are refused meanwhile).
async fn wait_for_close(commands: &mut mpsc::UnboundedReceiver<WsCommand>) {
    loop {
        match commands.recv().await {
            None | Some(WsCommand::Close { .. }) => return,
            Some(WsCommand::Send(_, reply)) => {
                let _ = reply.send(Err("Not connected yet".into()));
            }
            Some(WsCommand::Ping(_)) => {}
        }
    }
}

/// Wait before reconnect `attempt` (1 s, 2 s, 4 s, … max 30 s). False when
/// a Close or app exit came first.
async fn backoff(
    attempt: u32,
    commands: &mut mpsc::UnboundedReceiver<WsCommand>,
    cancel: &CancellationToken,
    emit: &(dyn Fn(WsFrame) + Send + Sync),
) -> bool {
    let secs = (1u64 << (attempt - 1).min(5)).min(30);
    emit(WsFrame::system(
        FrameKind::Reconnecting,
        format!("Reconnecting in {secs} s (attempt {attempt} of {MAX_RECONNECTS})"),
    ));
    tokio::select! {
        biased;
        _ = cancel.cancelled() => false,
        _ = wait_for_close(commands) => false,
        _ = tokio::time::sleep(reconnect_delay(secs)) => true,
    }
}

#[cfg(not(test))]
fn reconnect_delay(secs: u64) -> Duration {
    Duration::from_secs(secs)
}
#[cfg(test)]
fn reconnect_delay(secs: u64) -> Duration {
    Duration::from_millis(secs * 20)
}

async fn session(
    socket: Socket,
    mut sio_state: Option<SioState>,
    commands: &mut mpsc::UnboundedReceiver<WsCommand>,
    emit: &(dyn Fn(WsFrame) + Send + Sync),
    cancel: &CancellationToken,
    counts: &mut WsSessionSummary,
) -> End {
    let (mut tx, mut rx) = socket.split();
    let far = || tokio::time::Instant::now() + Duration::from_secs(365 * 24 * 3600);
    let ping_deadline = |s: &Option<SioState>| match s {
        Some(s) => tokio::time::Instant::now() + s.ping_interval + s.ping_timeout,
        None => far(),
    };
    let mut deadline = ping_deadline(&sio_state);
    // After we sent a close: wait (briefly) for the server's close echo.
    let mut closing: Option<(u16, String)> = None;
    let mut binary_warned = false;
    loop {
        let close_wait = async {
            if closing.is_some() {
                tokio::time::sleep(Duration::from_secs(3)).await
            } else {
                std::future::pending::<()>().await
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => {
                let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Away, reason: "ReqHarbor closed".into() }))).await;
                return End::Cancelled;
            }
            _ = close_wait => {
                let (code, reason) = closing.take().unwrap_or((1000, String::new()));
                return End::Closed { code, reason };
            }
            _ = tokio::time::sleep_until(deadline), if sio_state.is_some() && closing.is_none() => {
                return End::Dropped(err(ErrorKind::Timeout, "Socket.IO ping timeout: the server stopped sending pings"));
            }
            cmd = commands.recv(), if closing.is_none() => {
                let Some(cmd) = cmd else {
                    let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                    return End::Closed { code: 1000, reason: String::new() };
                };
                match cmd {
                    WsCommand::Close { code, reason } => {
                        let code = code.unwrap_or(1000);
                        if let Some(s) = &sio_state {
                            let pkt = sio::encode(&sio::SioPacket::Disconnect { namespace: s.namespace.clone() });
                            let _ = tx.send(Message::text(pkt)).await;
                        }
                        let mut f = WsFrame::new(Dir::Out, FrameKind::Close, reason.clone(), reason.len() as u64);
                        f.code = Some(code);
                        match tx.send(Message::Close(Some(CloseFrame { code: CloseCode::from(code), reason: reason.clone().into() }))).await {
                            Ok(()) => {
                                emit(f);
                                closing = Some((code, reason));
                            }
                            Err(_) => return End::Closed { code, reason },
                        }
                    }
                    WsCommand::Ping(data) => {
                        match tx.send(Message::Ping(data.clone().into())).await {
                            Ok(()) => emit(WsFrame::bytes(Dir::Out, FrameKind::Ping, &data)),
                            Err(e) => return End::Dropped(err(ErrorKind::Connect, format!("Connection lost: {e}"))),
                        }
                    }
                    WsCommand::Send(msg, reply) => {
                        let (wire, frame, ack_id) = match (&msg, sio_state.as_mut()) {
                            (OutMessage::Text(t), None) => (Message::text(t.clone()), WsFrame::new(Dir::Out, FrameKind::Text, t.clone(), t.len() as u64), None),
                            (OutMessage::Binary(b), None) => (Message::binary(b.clone()), WsFrame::bytes(Dir::Out, FrameKind::Binary, b), None),
                            (OutMessage::Event { .. }, None) => {
                                let _ = reply.send(Err("Socket.IO events need Socket.IO mode".into()));
                                continue;
                            }
                            (OutMessage::Event { name, args, ack }, Some(s)) => {
                                let id = ack.then(|| {
                                    let id = s.next_ack;
                                    s.next_ack += 1;
                                    id
                                });
                                let mut data = vec![serde_json::Value::String(name.clone())];
                                data.extend(args.iter().cloned());
                                let pkt = sio::encode(&sio::SioPacket::Event { namespace: s.namespace.clone(), id, data });
                                let args_json = serde_json::to_string(args).unwrap_or_default();
                                let mut f = WsFrame::new(Dir::Out, FrameKind::Event, args_json, pkt.len() as u64);
                                f.event = Some(name.clone());
                                f.ack_id = id;
                                (Message::text(pkt), f, id)
                            }
                            (_, Some(_)) => {
                                let _ = reply.send(Err("Socket.IO mode sends events: enter an event name".into()));
                                continue;
                            }
                        };
                        match tx.send(wire).await {
                            Ok(()) => {
                                counts.sent += 1;
                                emit(frame);
                                let _ = reply.send(Ok(ack_id));
                            }
                            Err(e) => {
                                let _ = reply.send(Err(format!("Send failed: {e}")));
                                return End::Dropped(err(ErrorKind::Connect, format!("Connection lost: {e}")));
                            }
                        }
                    }
                }
            }
            msg = rx.next() => {
                let msg = match msg {
                    None => {
                        return match closing.take() {
                            Some((code, reason)) => End::Closed { code, reason },
                            None => End::Dropped(err(ErrorKind::Connect, "Connection lost")),
                        };
                    }
                    Some(Err(e)) => {
                        return match closing.take() {
                            Some((code, reason)) => End::Closed { code, reason },
                            None => End::Dropped(err(ErrorKind::Connect, format!("Connection lost: {e}"))),
                        };
                    }
                    Some(Ok(m)) => m,
                };
                match msg {
                    Message::Close(f) => {
                        let code = f.as_ref().map(|f| u16::from(f.code));
                        let reason = f.as_ref().map(|f| f.reason.to_string()).unwrap_or_default();
                        let mut fr = WsFrame::new(Dir::In, FrameKind::Close, reason.clone(), reason.len() as u64);
                        fr.code = code;
                        emit(fr);
                        if let Some((code, reason)) = closing.take() {
                            return End::Closed { code, reason };
                        }
                        // Echo the close (tungstenite queues it) and stop.
                        let _ = tx.flush().await;
                        let reconnect = !matches!(code, Some(1000));
                        return End::ServerClosed { code, reason, reconnect };
                    }
                    Message::Ping(d) => emit(WsFrame::bytes(Dir::In, FrameKind::Ping, &d)),
                    Message::Pong(d) => emit(WsFrame::bytes(Dir::In, FrameKind::Pong, &d)),
                    Message::Frame(_) => {}
                    Message::Binary(b) => {
                        if sio_state.is_some() {
                            if !binary_warned {
                                binary_warned = true;
                                emit(WsFrame::system(FrameKind::Error, "Socket.IO binary attachments aren't supported; binary data was skipped"));
                            }
                            continue;
                        }
                        counts.received += 1;
                        emit(WsFrame::bytes(Dir::In, FrameKind::Binary, &b));
                    }
                    Message::Text(t) => {
                        let Some(state) = sio_state.as_mut() else {
                            counts.received += 1;
                            emit(WsFrame::new(Dir::In, FrameKind::Text, t.as_str(), t.len() as u64));
                            continue;
                        };
                        match sio::decode_eio(t.as_str()) {
                            Ok(sio::EioPacket::Ping(d)) => {
                                deadline = ping_deadline(&Some(state.clone()));
                                emit(WsFrame::new(Dir::In, FrameKind::Ping, d.clone(), d.len() as u64));
                                if let Err(e) = tx.send(Message::text(format!("3{d}"))).await {
                                    return End::Dropped(err(ErrorKind::Connect, format!("Connection lost: {e}")));
                                }
                                emit(WsFrame::new(Dir::Out, FrameKind::Pong, d.clone(), d.len() as u64));
                            }
                            Ok(sio::EioPacket::Close) => {
                                // Engine.IO CLOSE: the transport is done; close the WebSocket politely.
                                let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                                return End::ServerClosed { code: None, reason: "Engine.IO close".into(), reconnect: true };
                            }
                            Ok(sio::EioPacket::Message(m)) => match sio::decode(&m) {
                                Ok(pkt) if pkt.namespace() != state.namespace => {}
                                Ok(sio::SioPacket::Event { id, data, .. }) => {
                                    let mut it = data.into_iter();
                                    let name = it.next().and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                                    let args: Vec<_> = it.collect();
                                    counts.received += 1;
                                    let mut f = WsFrame::new(Dir::In, FrameKind::Event, serde_json::to_string(&args).unwrap_or_default(), t.len() as u64);
                                    f.event = Some(name);
                                    f.ack_id = id;
                                    emit(f);
                                }
                                Ok(sio::SioPacket::Ack { id, data, .. }) => {
                                    counts.received += 1;
                                    let mut f = WsFrame::new(Dir::In, FrameKind::Ack, serde_json::to_string(&data).unwrap_or_default(), t.len() as u64);
                                    f.ack_id = Some(id);
                                    emit(f);
                                }
                                Ok(sio::SioPacket::Disconnect { .. }) => {
                                    let _ = tx.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() }))).await;
                                    return End::ServerClosed { code: None, reason: "Socket.IO server disconnected the namespace".into(), reconnect: false };
                                }
                                Ok(sio::SioPacket::ConnectError { data, .. }) => {
                                    emit(WsFrame::system(FrameKind::Error, format!("Socket.IO connect error: {}", data.map(|d| d.to_string()).unwrap_or_default())));
                                }
                                Ok(sio::SioPacket::Connect { .. }) => {}
                                Ok(sio::SioPacket::BinaryEvent { .. } | sio::SioPacket::BinaryAck { .. }) => {
                                    if !binary_warned {
                                        binary_warned = true;
                                        emit(WsFrame::system(FrameKind::Error, "Socket.IO binary attachments aren't supported; the packet was skipped"));
                                    }
                                }
                                Err(e) => emit(WsFrame::system(FrameKind::Error, format!("Invalid Socket.IO packet: {e}"))),
                            },
                            Ok(_) => {}
                            Err(e) => emit(WsFrame::system(FrameKind::Error, format!("Invalid Engine.IO packet: {e}"))),
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------ history

/// The history entry of a finished session (AC-WS-7): the request as edited,
/// the URL (secrets masked), counts and duration. Everything passes through
/// `redactor`.
pub fn history_entry(
    id: &str,
    started_at: i64,
    request: &Request,
    prepared: &PreparedWs,
    stats: &SessionStats,
    redactor: &Redactor,
) -> HistoryEntry {
    let entry = HistoryEntry {
        id: id.to_string(),
        timestamp: started_at,
        source_request_id: (!request.collection_id.is_empty()).then(|| request.id.clone()),
        request: request.clone(),
        resolved_url: prepared.display_url.clone(),
        method: prepared.method().to_string(),
        response: None,
        error: stats.error.as_ref().map(|e| SendError {
            kind: e.kind,
            message: e.message.replace(&prepared.url, &prepared.display_url),
        }),
        ws: Some(stats.summary.clone()),
    };
    redactor.redact_struct(&entry)
}

/// A fresh history id for a session about to start.
pub fn new_history_id() -> String {
    new_id()
}

// --------------------------------------------------------------- Socket.IO

/// Engine.IO v4 / Socket.IO v5 protocol (the one Socket.IO 3 and 4 speak)
/// framing. Pure functions; no I/O.
pub mod sio {
    use serde::Deserialize;
    use serde_json::Value;

    /// Event names a client must not emit.
    pub const RESERVED_EVENTS: [&str; 6] = [
        "connect",
        "connect_error",
        "disconnect",
        "disconnecting",
        "newListener",
        "removeListener",
    ];

    /// The Engine.IO OPEN packet's payload.
    #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
    #[serde(rename_all = "camelCase")]
    pub struct EioHandshake {
        pub sid: String,
        #[serde(default)]
        pub upgrades: Vec<String>,
        #[serde(default = "default_ping_interval")]
        pub ping_interval: u64,
        #[serde(default = "default_ping_timeout")]
        pub ping_timeout: u64,
        #[serde(default)]
        pub max_payload: Option<u64>,
    }
    fn default_ping_interval() -> u64 {
        25_000
    }
    fn default_ping_timeout() -> u64 {
        20_000
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum EioPacket {
        Open(String),
        Close,
        Ping(String),
        Pong(String),
        Message(String),
        Upgrade,
        Noop,
    }

    pub fn decode_eio(s: &str) -> Result<EioPacket, String> {
        let mut chars = s.chars();
        let Some(t) = chars.next() else {
            return Err("empty packet".into());
        };
        let rest = chars.as_str().to_string();
        Ok(match t {
            '0' => EioPacket::Open(rest),
            '1' => EioPacket::Close,
            '2' => EioPacket::Ping(rest),
            '3' => EioPacket::Pong(rest),
            '4' => EioPacket::Message(rest),
            '5' => EioPacket::Upgrade,
            '6' => EioPacket::Noop,
            other => return Err(format!("unknown Engine.IO packet type {other:?}")),
        })
    }

    pub fn encode_eio(p: &EioPacket) -> String {
        match p {
            EioPacket::Open(d) => format!("0{d}"),
            EioPacket::Close => "1".into(),
            EioPacket::Ping(d) => format!("2{d}"),
            EioPacket::Pong(d) => format!("3{d}"),
            EioPacket::Message(d) => format!("4{d}"),
            EioPacket::Upgrade => "5".into(),
            EioPacket::Noop => "6".into(),
        }
    }

    /// A Socket.IO packet. `data` of events holds the event name first.
    #[derive(Debug, Clone, PartialEq)]
    pub enum SioPacket {
        Connect {
            namespace: String,
            data: Option<Value>,
        },
        Disconnect {
            namespace: String,
        },
        Event {
            namespace: String,
            id: Option<u64>,
            data: Vec<Value>,
        },
        Ack {
            namespace: String,
            id: u64,
            data: Vec<Value>,
        },
        ConnectError {
            namespace: String,
            data: Option<Value>,
        },
        BinaryEvent {
            namespace: String,
            id: Option<u64>,
            attachments: u32,
            data: Vec<Value>,
        },
        BinaryAck {
            namespace: String,
            id: u64,
            attachments: u32,
            data: Vec<Value>,
        },
    }

    impl SioPacket {
        pub fn namespace(&self) -> &str {
            match self {
                SioPacket::Connect { namespace, .. }
                | SioPacket::Disconnect { namespace }
                | SioPacket::Event { namespace, .. }
                | SioPacket::Ack { namespace, .. }
                | SioPacket::ConnectError { namespace, .. }
                | SioPacket::BinaryEvent { namespace, .. }
                | SioPacket::BinaryAck { namespace, .. } => namespace,
            }
        }
    }

    /// Encode as an Engine.IO MESSAGE (`4` + Socket.IO packet).
    pub fn encode(p: &SioPacket) -> String {
        let mut out = String::from("4");
        let (t, ns, attachments, id, data): (char, &str, Option<u32>, Option<u64>, Option<String>) =
            match p {
                SioPacket::Connect { namespace, data } => (
                    '0',
                    namespace,
                    None,
                    None,
                    data.as_ref().map(Value::to_string),
                ),
                SioPacket::Disconnect { namespace } => ('1', namespace, None, None, None),
                SioPacket::Event {
                    namespace,
                    id,
                    data,
                } => (
                    '2',
                    namespace,
                    None,
                    *id,
                    Some(Value::Array(data.clone()).to_string()),
                ),
                SioPacket::Ack {
                    namespace,
                    id,
                    data,
                } => (
                    '3',
                    namespace,
                    None,
                    Some(*id),
                    Some(Value::Array(data.clone()).to_string()),
                ),
                SioPacket::ConnectError { namespace, data } => (
                    '4',
                    namespace,
                    None,
                    None,
                    data.as_ref().map(Value::to_string),
                ),
                SioPacket::BinaryEvent {
                    namespace,
                    id,
                    attachments,
                    data,
                } => (
                    '5',
                    namespace,
                    Some(*attachments),
                    *id,
                    Some(Value::Array(data.clone()).to_string()),
                ),
                SioPacket::BinaryAck {
                    namespace,
                    id,
                    attachments,
                    data,
                } => (
                    '6',
                    namespace,
                    Some(*attachments),
                    Some(*id),
                    Some(Value::Array(data.clone()).to_string()),
                ),
            };
        out.push(t);
        if let Some(a) = attachments {
            out.push_str(&format!("{a}-"));
        }
        if ns != "/" && !ns.is_empty() {
            out.push_str(ns);
            out.push(',');
        }
        if let Some(id) = id {
            out.push_str(&id.to_string());
        }
        if let Some(d) = data {
            out.push_str(&d);
        }
        out
    }

    fn array(v: Option<Value>, what: &str) -> Result<Vec<Value>, String> {
        match v {
            Some(Value::Array(a)) => Ok(a),
            Some(_) => Err(format!("{what} payload must be an array")),
            None => Err(format!("{what} has no payload")),
        }
    }

    /// Decode the payload of an Engine.IO MESSAGE.
    pub fn decode(s: &str) -> Result<SioPacket, String> {
        // Untrusted input: never slice at a byte offset that could split a
        // multi-byte character (every other offset below follows an ASCII
        // character found by `find`, or counts ASCII digits).
        let mut chars = s.chars();
        let t = chars.next().ok_or("empty Socket.IO packet")?;
        let mut rest = chars.as_str();
        let mut attachments = None;
        if matches!(t, '5' | '6') {
            let dash = rest
                .find('-')
                .ok_or("binary packet without attachment count")?;
            attachments = Some(
                rest[..dash]
                    .parse::<u32>()
                    .map_err(|_| "invalid attachment count".to_string())?,
            );
            rest = &rest[dash + 1..];
        }
        let mut namespace = "/".to_string();
        if rest.starts_with('/') {
            let end = rest.find(',').unwrap_or(rest.len());
            namespace = rest[..end].to_string();
            rest = rest.get(end + 1..).unwrap_or("");
        }
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        let id = if digits > 0 {
            Some(
                rest[..digits]
                    .parse::<u64>()
                    .map_err(|_| "invalid packet id".to_string())?,
            )
        } else {
            None
        };
        rest = &rest[digits..];
        let data: Option<Value> = if rest.is_empty() {
            None
        } else {
            Some(serde_json::from_str(rest).map_err(|e| format!("invalid JSON payload: {e}"))?)
        };
        Ok(match t {
            '0' => SioPacket::Connect { namespace, data },
            '1' => SioPacket::Disconnect { namespace },
            '2' => {
                let data = array(data, "EVENT")?;
                if !data.first().is_some_and(Value::is_string) {
                    return Err("EVENT without an event name".into());
                }
                SioPacket::Event {
                    namespace,
                    id,
                    data,
                }
            }
            '3' => SioPacket::Ack {
                namespace,
                id: id.ok_or("ACK without an id")?,
                data: array(data, "ACK")?,
            },
            '4' => SioPacket::ConnectError { namespace, data },
            '5' => SioPacket::BinaryEvent {
                namespace,
                id,
                attachments: attachments.unwrap_or(0),
                data: array(data, "BINARY_EVENT")?,
            },
            '6' => SioPacket::BinaryAck {
                namespace,
                id: id.ok_or("BINARY_ACK without an id")?,
                attachments: attachments.unwrap_or(0),
                data: array(data, "BINARY_ACK")?,
            },
            other => return Err(format!("unknown Socket.IO packet type {other:?}")),
        })
    }
}

/// "Is the TLS stack usable" probe for diagnostics (cheap once it is).
pub fn tls_ready() -> bool {
    tls::config(true).is_ok() && tls::config(false).is_ok()
}

#[cfg(test)]
mod tests {
    use super::sio::*;
    use super::*;
    use crate::model::{Environment, KeyValue, Variable, WsConfig};
    use serde_json::json;

    fn ws_request(url: &str) -> Request {
        Request {
            kind: RequestKind::Ws,
            url: url.into(),
            ws: Some(WsConfig::default()),
            ..Request::default()
        }
    }

    #[test]
    fn ws_url_maps_schemes_and_drops_fragment() {
        assert_eq!(
            ws_url("http://a.dev/x?y=1#f", None).unwrap(),
            "ws://a.dev/x?y=1"
        );
        assert_eq!(ws_url("https://a.dev", None).unwrap(), "wss://a.dev/");
        assert_eq!(
            ws_url("WSS://a.dev:8443/s", None).unwrap(),
            "wss://a.dev:8443/s"
        );
        assert_eq!(
            ws_url("ws://[::1]:9000?q", None).unwrap(),
            "ws://[::1]:9000/?q"
        );
        assert!(ws_url("ftp://a.dev", None).is_err());
        assert!(ws_url("a.dev", None).is_err());
        assert!(ws_url("ws://", None).is_err());
    }

    #[test]
    fn ws_url_socket_io_path_and_query() {
        let cfg = SocketIoConfig::default();
        assert_eq!(
            ws_url("http://localhost:3000", Some(&cfg)).unwrap(),
            "ws://localhost:3000/socket.io/?EIO=4&transport=websocket"
        );
        assert_eq!(
            ws_url("https://a.dev/ignored?token=t&EIO=3", Some(&cfg)).unwrap(),
            "wss://a.dev/socket.io/?token=t&EIO=4&transport=websocket"
        );
        let custom = SocketIoConfig {
            path: "rt".into(),
            ..SocketIoConfig::default()
        };
        assert_eq!(
            ws_url("ws://a.dev", Some(&custom)).unwrap(),
            "ws://a.dev/rt?EIO=4&transport=websocket"
        );
    }

    #[test]
    fn prepare_ws_resolves_variables_params_headers_and_auth() {
        let mut env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Dev".into(),
            variables: vec![
                Variable::new("host", "localhost:9001"),
                Variable::new("tok", "s3cret").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        env.variables.push(Variable::new("room", "lobby"));
        let scope = VarScope::new(Some(&env), None);
        let mut r = ws_request("http://{{host}}/chat");
        r.params = vec![
            KeyValue::new("room", "{{room}}"),
            KeyValue::new("t", "{{tok}}"),
        ];
        r.headers = vec![KeyValue::new("X-Id", "{{$uuid}}")];
        r.auth = Auth::bearer("{{tok}}");
        r.ws.as_mut().unwrap().protocols = vec!["chat.v1".into(), " ".into()];
        let p = prepare_ws(&r, &scope, None).unwrap();
        assert_eq!(p.url, "ws://localhost:9001/chat?room=lobby&t=s3cret");
        assert_eq!(
            p.display_url,
            "ws://localhost:9001/chat?room=lobby&t={{tok}}"
        );
        assert_eq!(p.method(), "WS");
        assert!(
            p.headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer s3cret")
        );
        let id = &p.headers.iter().find(|(k, _)| k == "X-Id").unwrap().1;
        assert_eq!(id.len(), 36, "{{{{$uuid}}}} generated: {id}");
        assert_eq!(p.protocols, vec!["chat.v1"]);
        assert!(p.unresolved.is_empty());
    }

    #[test]
    fn prepare_ws_refuses_http_requests_and_bad_urls() {
        let mut r = ws_request("ws://a");
        r.kind = RequestKind::Http;
        assert_eq!(
            prepare_ws(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::NotWebSocket
        );
        let r = ws_request("ftp://a");
        assert!(matches!(
            prepare_ws(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::InvalidWsUrl(_)
        ));
        let mut r = ws_request("ws://a");
        r.ws.as_mut().unwrap().socket_io = Some(SocketIoConfig {
            auth: Some("[1]".into()),
            ..SocketIoConfig::default()
        });
        assert!(matches!(
            prepare_ws(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::InvalidSocketIoAuth(_)
        ));
        let r = Request {
            kind: RequestKind::Grpc,
            ..ws_request("ws://a")
        };
        assert_eq!(
            prepare_ws(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::NeedsNewerVersion
        );
    }

    #[test]
    fn prepare_ws_names_an_undefined_socket_io_auth_variable() {
        // A share imported without credentials carries `{{socketIoAuth}}`.
        let mut r = ws_request("ws://a");
        r.ws.as_mut().unwrap().socket_io = Some(SocketIoConfig {
            auth: Some(crate::share::SOCKET_IO_AUTH_PLACEHOLDER.into()),
            ..SocketIoConfig::default()
        });
        let e = prepare_ws(&r, &VarScope::empty(), None).unwrap_err();
        assert_eq!(
            e,
            PrepareError::UndefinedSocketIoAuthVariable("socketIoAuth".into())
        );
        assert_eq!(
            e.to_string(),
            "Define {{socketIoAuth}} (the shared Socket.IO auth payload) in your environment"
        );
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["kind"], "undefinedSocketIoAuthVariable");
        assert_eq!(v["message"], "socketIoAuth");
        // Inside a payload too, naming the variable.
        r.ws.as_mut().unwrap().socket_io.as_mut().unwrap().auth =
            Some(r#"{"token":"{{tok}}"}"#.into());
        assert_eq!(
            prepare_ws(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::UndefinedSocketIoAuthVariable("tok".into())
        );
        // Defined: connects.
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: None,
            name: "E".into(),
            variables: vec![Variable::new("socketIoAuth", r#"{"token":"t"}"#)],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        r.ws.as_mut().unwrap().socket_io.as_mut().unwrap().auth =
            Some(crate::share::SOCKET_IO_AUTH_PLACEHOLDER.into());
        let p = prepare_ws(&r, &VarScope::new(Some(&env), None), None).unwrap();
        assert_eq!(p.socket_io.unwrap().auth.unwrap()["token"], "t");
    }

    #[test]
    fn prepare_ws_socket_io_namespace_and_auth() {
        let mut r = ws_request("localhost:3000");
        r.ws.as_mut().unwrap().socket_io = Some(SocketIoConfig {
            namespace: "admin".into(),
            auth: Some(r#"{"token":"{{$randomInt}}"}"#.into()),
            ..SocketIoConfig::default()
        });
        let p = prepare_ws(&r, &VarScope::empty(), None).unwrap();
        assert_eq!(
            p.url,
            "ws://localhost:3000/socket.io/?EIO=4&transport=websocket"
        );
        assert_eq!(p.method(), "SIO");
        let sio = p.socket_io.unwrap();
        assert_eq!(sio.namespace, "/admin");
        assert!(
            sio.auth.unwrap()["token"]
                .as_str()
                .unwrap()
                .parse::<i64>()
                .is_ok()
        );
    }

    #[test]
    fn resolve_message_validates_payloads() {
        let scope = VarScope::empty();
        let m = |kind: MessageKind, body: &str| SavedMessage {
            kind,
            body: body.into(),
            ..SavedMessage::default()
        };
        assert_eq!(
            resolve_message(&m(MessageKind::Text, "hi"), &scope, false).unwrap(),
            OutMessage::Text("hi".into())
        );
        assert!(
            resolve_message(&m(MessageKind::Json, "{oops"), &scope, false)
                .unwrap_err()
                .starts_with("Invalid JSON")
        );
        assert_eq!(
            resolve_message(&m(MessageKind::Binary, "AAEC\n/w=="), &scope, false).unwrap(),
            OutMessage::Binary(vec![0, 1, 2, 255])
        );
        assert!(resolve_message(&m(MessageKind::Binary, "@@"), &scope, false).is_err());
        let mut ev = m(MessageKind::Json, r#"{"a":1}"#);
        assert_eq!(
            resolve_message(&ev, &scope, true).unwrap_err(),
            "Enter an event name"
        );
        ev.event = Some("chat".into());
        ev.ack = true;
        assert_eq!(
            resolve_message(&ev, &scope, true).unwrap(),
            OutMessage::Event {
                name: "chat".into(),
                args: vec![json!({"a":1})],
                ack: true
            }
        );
        ev.body = " ".into();
        assert!(
            matches!(resolve_message(&ev, &scope, true).unwrap(), OutMessage::Event { args, .. } if args.is_empty())
        );
        ev.event = Some("disconnect".into());
        assert!(resolve_message(&ev, &scope, true).is_err());
    }

    #[test]
    fn engine_io_packets() {
        assert_eq!(
            decode_eio("0{\"sid\":\"x\"}").unwrap(),
            EioPacket::Open("{\"sid\":\"x\"}".into())
        );
        assert_eq!(decode_eio("2").unwrap(), EioPacket::Ping(String::new()));
        assert_eq!(
            decode_eio("3probe").unwrap(),
            EioPacket::Pong("probe".into())
        );
        assert_eq!(decode_eio("40").unwrap(), EioPacket::Message("0".into()));
        assert_eq!(decode_eio("1").unwrap(), EioPacket::Close);
        assert_eq!(decode_eio("6").unwrap(), EioPacket::Noop);
        assert!(decode_eio("").is_err());
        assert!(decode_eio("9").is_err());
        for p in [
            EioPacket::Ping("x".into()),
            EioPacket::Pong(String::new()),
            EioPacket::Message("2[\"a\"]".into()),
            EioPacket::Close,
        ] {
            assert_eq!(decode_eio(&encode_eio(&p)).unwrap(), p);
        }
        let hs: EioHandshake = serde_json::from_str(
            r#"{"sid":"lv_VI97HAXpY6yYWAAAC","upgrades":[],"pingInterval":25000,"pingTimeout":5000,"maxPayload":1000000}"#,
        )
        .unwrap();
        assert_eq!(hs.ping_timeout, 5000);
        assert_eq!(hs.max_payload, Some(1_000_000));
    }

    #[test]
    fn socket_io_encode_matches_reference_examples() {
        // Examples from the Socket.IO v5 protocol document.
        let e = |p: SioPacket| encode(&p);
        assert_eq!(
            e(SioPacket::Connect {
                namespace: "/".into(),
                data: None
            }),
            "40"
        );
        assert_eq!(
            e(SioPacket::Connect {
                namespace: "/admin".into(),
                data: Some(json!({"token":"123"}))
            }),
            r#"40/admin,{"token":"123"}"#
        );
        assert_eq!(
            e(SioPacket::Disconnect {
                namespace: "/admin".into()
            }),
            "41/admin,"
        );
        assert_eq!(
            e(SioPacket::Event {
                namespace: "/".into(),
                id: None,
                data: vec![json!("foo")]
            }),
            r#"42["foo"]"#
        );
        assert_eq!(
            e(SioPacket::Event {
                namespace: "/admin".into(),
                id: Some(12),
                data: vec![json!("bar")]
            }),
            r#"42/admin,12["bar"]"#
        );
        assert_eq!(
            e(SioPacket::Ack {
                namespace: "/admin".into(),
                id: 13,
                data: vec![json!("bar")]
            }),
            r#"43/admin,13["bar"]"#
        );
        assert_eq!(
            e(SioPacket::BinaryEvent {
                namespace: "/".into(),
                id: None,
                attachments: 1,
                data: vec![json!("baz"), json!({"_placeholder":true,"num":0})]
            }),
            r#"451-["baz",{"_placeholder":true,"num":0}]"#
        );
    }

    #[test]
    fn socket_io_decode_namespaces_ids_and_acks() {
        assert_eq!(
            decode(r#"0{"sid":"oSO0OpakMV_3jnilAAAA"}"#).unwrap(),
            SioPacket::Connect {
                namespace: "/".into(),
                data: Some(json!({"sid":"oSO0OpakMV_3jnilAAAA"}))
            }
        );
        assert_eq!(
            decode(r#"4/admin,{"message":"Not authorized"}"#).unwrap(),
            SioPacket::ConnectError {
                namespace: "/admin".into(),
                data: Some(json!({"message":"Not authorized"}))
            }
        );
        assert_eq!(
            decode("1/admin,").unwrap(),
            SioPacket::Disconnect {
                namespace: "/admin".into()
            }
        );
        assert_eq!(
            decode("1").unwrap(),
            SioPacket::Disconnect {
                namespace: "/".into()
            }
        );
        let ev = decode(r#"2/chat,7["msg",{"t":"hi"},3]"#).unwrap();
        assert_eq!(
            ev,
            SioPacket::Event {
                namespace: "/chat".into(),
                id: Some(7),
                data: vec![json!("msg"), json!({"t":"hi"}), json!(3)]
            }
        );
        assert_eq!(ev.namespace(), "/chat");
        assert_eq!(
            decode(r#"312["ok"]"#).unwrap(),
            SioPacket::Ack {
                namespace: "/".into(),
                id: 12,
                data: vec![json!("ok")]
            }
        );
        assert!(decode("3[\"no id\"]").is_err());
        assert!(decode("2[1]").is_err(), "event name must be a string");
        assert!(decode("2{").is_err());
        assert!(decode("9").is_err());
        assert!(matches!(
            decode(r#"52-["a",{"_placeholder":true,"num":0},{"_placeholder":true,"num":1}]"#)
                .unwrap(),
            SioPacket::BinaryEvent { attachments: 2, .. }
        ));
        assert!(matches!(
            decode(r#"61-/a,4[{"_placeholder":true,"num":0}]"#).unwrap(),
            SioPacket::BinaryAck {
                id: 4,
                attachments: 1,
                ..
            }
        ));
        // Round trips.
        for p in [
            SioPacket::Event {
                namespace: "/x".into(),
                id: Some(0),
                data: vec![json!("e"), json!([1, 2])],
            },
            SioPacket::Ack {
                namespace: "/".into(),
                id: 99,
                data: vec![],
            },
            SioPacket::Connect {
                namespace: "/n".into(),
                data: None,
            },
        ] {
            let wire = encode(&p);
            assert_eq!(decode(&wire[1..]).unwrap(), p, "{wire}");
        }
    }

    #[test]
    fn decoders_survive_multibyte_and_malformed_input() {
        for s in [
            "é",
            "4é",
            "é0",
            "5é-",
            "51-é",
            "2/é,",
            "2/ns,é",
            "3/é",
            "2/ns",
            "61-",
            "5-",
            "4/",
            "2/€,1[\"e\"]",
            "0\u{10FFFF}",
            "2\u{0}",
            "3\u{1F600}[1]",
            "29999999999999999999999[\"e\"]",
        ] {
            let _ = decode(s);
            let _ = decode_eio(s);
        }
        assert!(
            decode("4é").is_err(),
            "a garbage payload is an error, not a panic"
        );
        assert!(decode("é").is_err());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2000))]
        /// Server packets are untrusted: decoding never panics (release builds abort).
        #[test]
        fn decoders_never_panic(s in "\\PC*", t in "[0-6]", tail in proptest::prelude::any::<String>()) {
            let _ = decode(&s);
            let _ = decode_eio(&s);
            let framed = format!("{t}{tail}");
            let _ = decode(&framed);
            let _ = decode_eio(&framed);
            let _ = decode_eio(&format!("4{framed}"));
        }
    }

    /// AC-WS-6: a server that accepts and drops at once gets MAX_RECONNECTS
    /// reconnects in all, not a fresh five after every short-lived connect.
    #[tokio::test]
    async fn ac_ws_6_reconnects_are_capped_when_the_server_keeps_dropping() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::{Arc, Mutex};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(AtomicU32::new(0));
        let accepted2 = accepted.clone();
        tokio::spawn(async move {
            while let Ok((tcp, _)) = listener.accept().await {
                if let Ok(ws) = tokio_tungstenite::accept_async(tcp).await {
                    accepted2.fetch_add(1, Ordering::SeqCst);
                    drop(ws); // no close frame: a dropped connection
                }
            }
        });
        let url = format!("ws://127.0.0.1:{port}/");
        let prepared = PreparedWs {
            url: url.clone(),
            display_url: url,
            headers: vec![],
            protocols: vec![],
            socket_io: None,
            timeout_ms: 2_000,
            verify_tls: true,
            auto_reconnect: true,
            unresolved: vec![],
        };
        let (_keep, rx) = mpsc::unbounded_channel();
        let frames = Arc::new(Mutex::new(Vec::<WsFrame>::new()));
        let sink = frames.clone();
        let stats = tokio::time::timeout(
            Duration::from_secs(10),
            run(
                prepared,
                rx,
                move |f| sink.lock().unwrap().push(f),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("the session ends instead of reconnecting forever");
        assert_eq!(accepted.load(Ordering::SeqCst), 1 + MAX_RECONNECTS);
        let frames = frames.lock().unwrap();
        let retries = frames
            .iter()
            .filter(|f| f.kind == FrameKind::Reconnecting)
            .count();
        assert_eq!(retries as u32, MAX_RECONNECTS);
        assert!(frames.last().unwrap().last);
        assert!(stats.error.is_some(), "ends as a lost connection");
    }

    #[test]
    fn namespaces_normalize() {
        assert_eq!(normalize_namespace(""), "/");
        assert_eq!(normalize_namespace(" / "), "/");
        assert_eq!(normalize_namespace("admin"), "/admin");
        assert_eq!(normalize_namespace("/a/b"), "/a/b");
    }

    #[test]
    fn frames_serialize_camel_case_and_truncate() {
        let mut f = WsFrame::new(Dir::In, FrameKind::Event, "[1]", 3);
        f.event = Some("e".into());
        f.ack_id = Some(2);
        let v = serde_json::to_value(&f).unwrap();
        assert_eq!(v["dir"], "in");
        assert_eq!(v["kind"], "event");
        assert_eq!(v["ackId"], 2);
        assert!(v.get("code").is_none() && v.get("truncated").is_none() && v.get("last").is_none());
        let big = "é".repeat(MAX_FRAME_DATA);
        let f = WsFrame::new(Dir::In, FrameKind::Text, big, 0);
        assert!(f.truncated && f.data.len() <= MAX_FRAME_DATA);
    }

    #[test]
    fn tls_configs_build_and_only_successes_are_cached() {
        crate::http::install_crypto_provider();
        assert!(tls_ready());
        let a = tls::config(true).unwrap();
        let b = tls::config(true).unwrap();
        assert!(std::sync::Arc::ptr_eq(&a, &b), "a built config is reused");
        assert!(!std::sync::Arc::ptr_eq(&a, &tls::config(false).unwrap()));
    }
}

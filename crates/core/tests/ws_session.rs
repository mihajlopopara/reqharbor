//! WebSocket and Socket.IO sessions against local servers (AC-WS-2..7):
//! a tokio-tungstenite echo server, a TLS one with a self-signed
//! certificate, and a tiny in-test Socket.IO (Engine.IO v4) server.

// tungstenite's handshake callback returns its own (large) error response type.
#![allow(clippy::result_large_err)]

use futures_util::{SinkExt, StreamExt};
use reqharbor_core::CancellationToken;
use reqharbor_core::model::*;
use reqharbor_core::vars::VarScope;
use reqharbor_core::ws::{
    Dir, FrameKind, OutMessage, SessionStats, WsCommand, WsFrame, prepare_ws, run,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{
    Request as HsRequest, Response as HsResponse,
};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

type Seen = Arc<Mutex<Vec<String>>>;

fn ws_request(url: &str) -> Request {
    Request {
        kind: RequestKind::Ws,
        url: url.into(),
        ws: Some(WsConfig::default()),
        settings: RequestSettings {
            timeout_ms: 5_000,
            ..RequestSettings::default()
        },
        ..Request::default()
    }
}

/// Echo server: echoes text/binary, closes when asked by a text "close:<code>",
/// records the handshake (path, X-Test header, offered subprotocols).
async fn echo_server() -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let seen = seen2.clone();
            tokio::spawn(async move {
                let cb = |req: &HsRequest, mut resp: HsResponse| {
                    let mut s = seen.lock().unwrap();
                    s.push(format!("path {}", req.uri()));
                    if let Some(v) = req.headers().get("x-test") {
                        s.push(format!("x-test {}", v.to_str().unwrap()));
                    }
                    if let Some(v) = req.headers().get("authorization") {
                        s.push(format!("auth {}", v.to_str().unwrap()));
                    }
                    if let Some(v) = req.headers().get("sec-websocket-protocol") {
                        s.push(format!("protocols {}", v.to_str().unwrap()));
                        resp.headers_mut()
                            .insert("sec-websocket-protocol", "chat.v2".parse().unwrap());
                    }
                    if req.uri().path() == "/forbidden" {
                        let mut r = tokio_tungstenite::tungstenite::http::Response::new(Some(
                            "nope".to_string(),
                        ));
                        *r.status_mut() =
                            tokio_tungstenite::tungstenite::http::StatusCode::FORBIDDEN;
                        return Err(r);
                    }
                    Ok(resp)
                };
                let Ok(mut ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                while let Some(Ok(m)) = ws.next().await {
                    match m {
                        Message::Text(t) if t.as_str().starts_with("close:") => {
                            let code: u16 = t.as_str()[6..].parse().unwrap();
                            let _ = ws
                                .send(Message::Close(Some(CloseFrame {
                                    code: CloseCode::from(code),
                                    reason: "server says bye".into(),
                                })))
                                .await;
                            break;
                        }
                        Message::Text(_) | Message::Binary(_) => {
                            if ws.send(m).await.is_err() {
                                break;
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
            });
        }
    });
    (port, seen)
}

struct Session {
    tx: mpsc::UnboundedSender<WsCommand>,
    frames: mpsc::UnboundedReceiver<WsFrame>,
    done: tokio::task::JoinHandle<SessionStats>,
    cancel: CancellationToken,
}

fn start(r: &Request, scope: &VarScope) -> Session {
    let prepared = prepare_ws(r, scope, None).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let (ftx, frames) = mpsc::unbounded_channel();
    let cancel = CancellationToken::new();
    let done = tokio::spawn(run(
        prepared,
        rx,
        move |f| {
            let _ = ftx.send(f);
        },
        cancel.clone(),
    ));
    Session {
        tx,
        frames,
        done,
        cancel,
    }
}

impl Session {
    async fn next(&mut self) -> WsFrame {
        tokio::time::timeout(Duration::from_secs(5), self.frames.recv())
            .await
            .expect("frame in time")
            .expect("frame")
    }
    /// Skip frames until one matches.
    async fn until(&mut self, pred: impl Fn(&WsFrame) -> bool) -> WsFrame {
        loop {
            let f = self.next().await;
            if pred(&f) {
                return f;
            }
        }
    }
    async fn send(&self, m: OutMessage) -> Result<Option<u64>, String> {
        let (rtx, rrx) = oneshot::channel();
        self.tx.send(WsCommand::Send(m, rtx)).unwrap();
        rrx.await.unwrap()
    }
    async fn finish(self) -> (SessionStats, Vec<WsFrame>) {
        let stats = tokio::time::timeout(Duration::from_secs(10), self.done)
            .await
            .unwrap()
            .unwrap();
        let mut rest = Vec::new();
        let mut frames = self.frames;
        while let Ok(f) = frames.try_recv() {
            rest.push(f);
        }
        (stats, rest)
    }
}

fn is(dir: Dir, kind: FrameKind) -> impl Fn(&WsFrame) -> bool {
    move |f| f.dir == dir && f.kind == kind
}

#[tokio::test]
async fn ac_ws_3_echo_text_binary_ping_and_close_in_order() {
    let (port, seen) = echo_server().await;
    let env = Environment {
        id: "e".into(),
        workspace_id: "w".into(),
        project_id: Some("p".into()),
        name: "Dev".into(),
        variables: vec![
            Variable::new("v", "from-env"),
            Variable::new("tok", "t0k").secret(),
        ],
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
        extra: Default::default(),
    };
    let scope = VarScope::new(Some(&env), None);
    let mut r = ws_request(&format!("http://127.0.0.1:{port}/echo"));
    r.params = vec![KeyValue::new("room", "1")];
    r.headers = vec![KeyValue::new("X-Test", "{{v}}")];
    r.auth = Auth::bearer("{{tok}}");
    r.ws.as_mut().unwrap().protocols = vec!["chat.v1".into(), "chat.v2".into()];
    let mut s = start(&r, &scope);

    let open = s.next().await;
    assert_eq!(
        (open.dir, open.kind),
        (Dir::System, FrameKind::Open),
        "{open:?}"
    );
    assert!(open.data.contains("subprotocol chat.v2"), "{}", open.data);
    {
        let seen = seen.lock().unwrap();
        assert!(seen.contains(&"path /echo?room=1".to_string()), "{seen:?}");
        assert!(seen.contains(&"x-test from-env".to_string()), "{seen:?}");
        assert!(seen.contains(&"auth Bearer t0k".to_string()), "{seen:?}");
        assert!(
            seen.contains(&"protocols chat.v1, chat.v2".to_string()),
            "{seen:?}"
        );
    }

    assert_eq!(s.send(OutMessage::Text("héllo".into())).await, Ok(None));
    let out = s.next().await;
    assert_eq!(
        (out.dir, out.kind, out.data.as_str(), out.size),
        (Dir::Out, FrameKind::Text, "héllo", 6)
    );
    let echo = s.next().await;
    assert_eq!(
        (echo.dir, echo.kind, echo.data.as_str()),
        (Dir::In, FrameKind::Text, "héllo")
    );

    s.send(OutMessage::Binary(vec![0, 1, 255])).await.unwrap();
    let out = s.next().await;
    assert_eq!(
        (out.kind, out.data.as_str(), out.size),
        (FrameKind::Binary, "AAH/", 3)
    );
    let echo = s.next().await;
    assert_eq!(
        (echo.dir, echo.kind, echo.data.as_str()),
        (Dir::In, FrameKind::Binary, "AAH/")
    );

    s.tx.send(WsCommand::Ping(b"hb".to_vec())).unwrap();
    assert_eq!(s.next().await.kind, FrameKind::Ping);
    let pong = s.next().await;
    assert_eq!(
        (pong.dir, pong.kind, pong.data.as_str()),
        (Dir::In, FrameKind::Pong, "aGI=")
    );

    // Socket.IO events need Socket.IO mode.
    assert!(
        s.send(OutMessage::Event {
            name: "e".into(),
            args: vec![],
            ack: false
        })
        .await
        .is_err()
    );

    s.tx.send(WsCommand::Close {
        code: Some(4000),
        reason: "done".into(),
    })
    .unwrap();
    let close = s.next().await;
    assert_eq!(
        (close.dir, close.kind, close.code, close.data.as_str()),
        (Dir::Out, FrameKind::Close, Some(4000), "done")
    );
    let last = s.until(|f| f.last).await;
    assert_eq!(
        (last.dir, last.kind, last.code),
        (Dir::System, FrameKind::Close, Some(4000))
    );
    let (stats, rest) = s.finish().await;
    assert!(rest.is_empty(), "nothing after the last frame: {rest:?}");
    assert!(stats.connected && stats.error.is_none());
    assert_eq!((stats.summary.sent, stats.summary.received), (2, 2));
    assert_eq!(stats.summary.close_code, Some(4000));
    assert_eq!(stats.summary.close_reason.as_deref(), Some("done"));
}

#[tokio::test]
async fn ac_ws_6_server_close_reconnects_when_auto_reconnect_is_on() {
    let (port, _) = echo_server().await;
    let mut r = ws_request(&format!("ws://127.0.0.1:{port}/"));
    r.ws.as_mut().unwrap().auto_reconnect = true;
    let mut s = start(&r, &VarScope::empty());
    s.until(is(Dir::System, FrameKind::Open)).await;
    s.send(OutMessage::Text("close:1011".into())).await.unwrap();
    let close = s.until(is(Dir::In, FrameKind::Close)).await;
    assert_eq!(close.code, Some(1011));
    assert_eq!(close.data, "server says bye");
    s.until(is(Dir::System, FrameKind::Reconnecting)).await;
    s.until(is(Dir::System, FrameKind::Open)).await;
    // Normal close (1000) from the server: no reconnect.
    s.send(OutMessage::Text("close:1000".into())).await.unwrap();
    let last = s.until(|f| f.last).await;
    assert_eq!(last.code, Some(1000));
    let (stats, _) = s.finish().await;
    assert!(stats.error.is_none());
}

#[tokio::test]
async fn ac_ws_6_server_close_ends_the_session_without_auto_reconnect() {
    let (port, _) = echo_server().await;
    let mut s = start(
        &ws_request(&format!("ws://127.0.0.1:{port}/")),
        &VarScope::empty(),
    );
    s.until(is(Dir::System, FrameKind::Open)).await;
    s.send(OutMessage::Text("close:1011".into())).await.unwrap();
    let last = s.until(|f| f.last).await;
    assert_eq!(last.code, Some(1011));
    let (stats, _) = s.finish().await;
    assert_eq!(stats.summary.close_code, Some(1011));
    assert_eq!(
        stats.summary.close_reason.as_deref(),
        Some("server says bye")
    );
}

#[tokio::test]
async fn ac_ws_2_connect_failures_are_typed() {
    // Nothing listens on this port.
    let free = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let mut s = start(
        &ws_request(&format!("ws://127.0.0.1:{port}/")),
        &VarScope::empty(),
    );
    let e = s.next().await;
    assert_eq!((e.dir, e.kind), (Dir::System, FrameKind::Error));
    assert!(s.next().await.last);
    let (stats, _) = s.finish().await;
    assert!(!stats.connected);
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Connect);

    let (port, _) = echo_server().await;
    let s = start(
        &ws_request(&format!("ws://127.0.0.1:{port}/forbidden")),
        &VarScope::empty(),
    );
    let (stats, _) = s.finish().await;
    let e = stats.error.unwrap();
    assert!(e.message.contains("403"), "{}", e.message);

    let s = start(
        &ws_request("ws://no-such-host.invalid/"),
        &VarScope::empty(),
    );
    let (stats, _) = s.finish().await;
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Dns);
}

#[tokio::test]
async fn disconnect_while_connecting_and_app_exit() {
    // A listener that never answers the handshake.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut keep = Vec::new();
        while let Ok((tcp, _)) = listener.accept().await {
            keep.push(tcp);
        }
    });
    let mut r = ws_request(&format!("ws://127.0.0.1:{port}/"));
    r.settings.timeout_ms = 0;
    let s = start(&r, &VarScope::empty());
    tokio::time::sleep(Duration::from_millis(50)).await;
    s.tx.send(WsCommand::Close {
        code: None,
        reason: String::new(),
    })
    .unwrap();
    let (stats, frames) = s.finish().await;
    assert!(!stats.connected && stats.error.is_none());
    assert!(frames.last().unwrap().last);

    r.settings.timeout_ms = 200;
    let s = start(&r, &VarScope::empty());
    let (stats, _) = s.finish().await;
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Timeout);

    let (port, _) = echo_server().await;
    let mut s = start(
        &ws_request(&format!("ws://127.0.0.1:{port}/")),
        &VarScope::empty(),
    );
    s.until(is(Dir::System, FrameKind::Open)).await;
    s.cancel.cancel();
    let last = s.until(|f| f.last).await;
    assert_eq!(last.code, Some(1001));
    let (stats, _) = s.finish().await;
    assert!(stats.error.is_none());
}

fn self_signed_acceptor() -> tokio_rustls::TlsAcceptor {
    reqharbor_core::http::install_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = cert.cert.der().clone();
    let key: rustls::pki_types::PrivateKeyDer<'static> =
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into();
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
}

#[tokio::test]
async fn ac_ws_2_wss_self_signed_needs_verify_tls_off() {
    let acceptor = self_signed_acceptor();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else {
                    return;
                };
                let Ok(mut ws) = tokio_tungstenite::accept_async(tls).await else {
                    return;
                };
                while let Some(Ok(m)) = ws.next().await {
                    if m.is_text() && ws.send(m).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    let url = format!("wss://localhost:{port}/");
    let s = start(&ws_request(&url), &VarScope::empty());
    let (stats, _) = s.finish().await;
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Tls);

    let mut r = ws_request(&url);
    r.settings.verify_tls = false;
    let mut s = start(&r, &VarScope::empty());
    s.until(is(Dir::System, FrameKind::Open)).await;
    s.send(OutMessage::Text("secure".into())).await.unwrap();
    let echo = s.until(is(Dir::In, FrameKind::Text)).await;
    assert_eq!(echo.data, "secure");
    s.tx.send(WsCommand::Close {
        code: None,
        reason: String::new(),
    })
    .unwrap();
    let (stats, _) = s.finish().await;
    assert!(stats.error.is_none());
    assert_eq!(stats.summary.close_code, Some(1000));
}

// ------------------------------------------------------------- Socket.IO

#[derive(Clone, Copy, PartialEq)]
enum SioMode {
    Normal,
    /// Never pings: the client must time out.
    Silent,
    /// Refuses the namespace.
    Refuse,
    /// Sends an Engine.IO CLOSE (`1`) right after the namespace CONNECT.
    EngineClose,
}

/// A minimal Socket.IO v4 server: one namespace `/admin` that requires
/// `{"token":"t"}`; `echo` events are echoed back as an event and, with an
/// ack id, acknowledged with the same args.
async fn sio_server(mode: SioMode) -> (u16, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen: Seen = Arc::default();
    let seen2 = seen.clone();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let seen = seen2.clone();
            tokio::spawn(async move {
                let seen_hs = seen.clone();
                let cb = move |req: &HsRequest, resp: HsResponse| {
                    seen_hs.lock().unwrap().push(format!("path {}", req.uri()));
                    Ok(resp)
                };
                let Ok(ws) = tokio_tungstenite::accept_hdr_async(tcp, cb).await else {
                    return;
                };
                let (mut tx, mut rx) = ws.split();
                let (interval, timeout) = if mode == SioMode::Silent {
                    (100, 100)
                } else {
                    (150, 1000)
                };
                tx.send(Message::text(format!(
                    r#"0{{"sid":"eio1","upgrades":[],"pingInterval":{interval},"pingTimeout":{timeout},"maxPayload":1000000}}"#
                )))
                .await
                .unwrap();
                let mut ping = tokio::time::interval(Duration::from_millis(interval));
                ping.tick().await;
                loop {
                    tokio::select! {
                        _ = ping.tick(), if mode != SioMode::Silent => {
                            if tx.send(Message::text("2")).await.is_err() { return; }
                        }
                        m = rx.next() => {
                            let Some(Ok(m)) = m else { return };
                            if let Message::Close(f) = &m {
                                let code = f.as_ref().map(|f| u16::from(f.code)).unwrap_or(0);
                                seen.lock().unwrap().push(format!("close {code}"));
                                return;
                            }
                            let Message::Text(t) = m else { continue };
                            let t = t.as_str().to_string();
                            seen.lock().unwrap().push(t.clone());
                            if t == "3" {
                                continue;
                            }
                            if let Some(connect) = t.strip_prefix("40/admin,") {
                                let ok = mode != SioMode::Refuse && connect == r#"{"token":"t"}"#;
                                let reply = if ok { r#"40/admin,{"sid":"sio1"}"# } else { r#"44/admin,{"message":"Not authorized"}"# };
                                if tx.send(Message::text(reply)).await.is_err() { return; }
                                // A binary attachment packet the client must skip.
                                if ok && mode == SioMode::EngineClose {
                                    let _ = tx.send(Message::text("1")).await;
                                } else if ok {
                                    let _ = tx.send(Message::text(r#"451-/admin,["file",{"_placeholder":true,"num":0}]"#)).await;
                                    let _ = tx.send(Message::binary(vec![1, 2, 3])).await;
                                }
                            } else if let Some(rest) = t.strip_prefix("42/admin,") {
                                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                                let json = &rest[digits.len()..];
                                let v: serde_json::Value = serde_json::from_str(json).unwrap();
                                let args = serde_json::Value::Array(v.as_array().unwrap()[1..].to_vec());
                                if !digits.is_empty() {
                                    let _ = tx.send(Message::text(format!("43/admin,{digits}{args}"))).await;
                                }
                                let _ = tx.send(Message::text(format!("42/admin,{v}"))).await;
                            } else if t == "41/admin," {
                                let _ = tx.send(Message::Close(None)).await;
                                return;
                            }
                        }
                    }
                }
            });
        }
    });
    (port, seen)
}

fn sio_request(port: u16, auth: &str) -> Request {
    let mut r = ws_request(&format!("http://127.0.0.1:{port}/ignored?v=1"));
    r.ws.as_mut().unwrap().socket_io = Some(SocketIoConfig {
        namespace: "admin".into(),
        auth: Some(auth.into()),
        ..SocketIoConfig::default()
    });
    r
}

#[tokio::test]
async fn ac_ws_8_socket_io_namespace_events_acks_and_pings() {
    let (port, seen) = sio_server(SioMode::Normal).await;
    let mut s = start(
        &sio_request(port, r#"{"token":"{{$randomInt}}"}"#),
        &VarScope::empty(),
    );
    // Wrong token: the namespace is refused.
    let e = s.until(is(Dir::System, FrameKind::Error)).await;
    assert!(e.data.contains("Not authorized"), "{}", e.data);
    let (stats, _) = s.finish().await;
    assert!(!stats.connected);

    let mut s = start(&sio_request(port, r#"{"token":"t"}"#), &VarScope::empty());
    let open = s.until(is(Dir::System, FrameKind::Open)).await;
    assert!(
        open.data.contains("namespace /admin") && open.data.contains("sid sio1"),
        "{}",
        open.data
    );
    assert!(
        seen.lock()
            .unwrap()
            .contains(&"path /socket.io/?v=1&EIO=4&transport=websocket".to_string())
    );
    let skipped = s.until(is(Dir::System, FrameKind::Error)).await;
    assert!(
        skipped.data.contains("binary attachments"),
        "{}",
        skipped.data
    );

    let ack = s
        .send(OutMessage::Event {
            name: "echo".into(),
            args: vec![serde_json::json!({"a": 1})],
            ack: true,
        })
        .await
        .unwrap();
    assert_eq!(ack, Some(0));
    let out = s.until(is(Dir::Out, FrameKind::Event)).await;
    assert_eq!(
        (out.event.as_deref(), out.ack_id, out.data.as_str()),
        (Some("echo"), Some(0), r#"[{"a":1}]"#)
    );
    let got = s.until(is(Dir::In, FrameKind::Ack)).await;
    assert_eq!((got.ack_id, got.data.as_str()), (Some(0), r#"[{"a":1}]"#));
    let ev = s.until(is(Dir::In, FrameKind::Event)).await;
    assert_eq!(
        (ev.event.as_deref(), ev.data.as_str()),
        (Some("echo"), r#"[{"a":1}]"#)
    );
    // Second ack gets the next id; plain text is refused in Socket.IO mode.
    assert_eq!(
        s.send(OutMessage::Event {
            name: "echo".into(),
            args: vec![],
            ack: true
        })
        .await,
        Ok(Some(1))
    );
    assert!(s.send(OutMessage::Text("raw".into())).await.is_err());
    let got = s.until(is(Dir::In, FrameKind::Ack)).await;
    assert_eq!((got.ack_id, got.data.as_str()), (Some(1), "[]"));
    s.until(is(Dir::In, FrameKind::Event)).await;

    // Engine.IO heartbeat: server ping, automatic pong.
    s.until(is(Dir::In, FrameKind::Ping)).await;
    s.until(is(Dir::Out, FrameKind::Pong)).await;

    s.tx.send(WsCommand::Close {
        code: None,
        reason: String::new(),
    })
    .unwrap();
    let last = s.until(|f| f.last).await;
    assert_eq!(last.code, Some(1000));
    let (stats, _) = s.finish().await;
    assert!(stats.error.is_none(), "{:?}", stats.error);
    assert_eq!(stats.summary.sent, 2);
    assert_eq!(stats.summary.received, 4, "2 acks + 2 echoed events");
    let seen = seen.lock().unwrap();
    assert!(
        seen.contains(&r#"40/admin,{"token":"t"}"#.to_string()),
        "{seen:?}"
    );
    assert!(
        seen.contains(&r#"42/admin,0["echo",{"a":1}]"#.to_string()),
        "{seen:?}"
    );
    assert!(seen.contains(&"3".to_string()), "pong sent: {seen:?}");
    assert!(
        seen.contains(&"41/admin,".to_string()),
        "namespace disconnect sent: {seen:?}"
    );
}

#[tokio::test]
async fn ac_ws_8_socket_io_ping_timeout_drops_the_connection() {
    let (port, _) = sio_server(SioMode::Silent).await;
    let mut s = start(&sio_request(port, r#"{"token":"t"}"#), &VarScope::empty());
    s.until(is(Dir::System, FrameKind::Open)).await;
    let e = s
        .until(|f| {
            f.dir == Dir::System && f.kind == FrameKind::Error && f.data.contains("ping timeout")
        })
        .await;
    assert!(e.data.contains("ping timeout"));
    let (stats, _) = s.finish().await;
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Timeout);
}

#[tokio::test]
async fn ac_ws_8_socket_io_refused_namespace() {
    let (port, _) = sio_server(SioMode::Refuse).await;
    let s = start(&sio_request(port, r#"{"token":"t"}"#), &VarScope::empty());
    let (stats, frames) = s.finish().await;
    assert!(!stats.connected);
    assert!(
        stats
            .error
            .unwrap()
            .message
            .contains("refused namespace /admin")
    );
    assert!(frames.iter().any(|f| f.kind == FrameKind::Error));
}

#[test]
fn ac_ws_7_history_entry_masks_secrets_and_counts() {
    let env = Environment {
        id: "e".into(),
        workspace_id: "w".into(),
        project_id: None,
        name: "Globals".into(),
        variables: vec![Variable::new("tok", "SEKRET").secret()],
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
        extra: Default::default(),
    };
    let scope = VarScope::new(None, Some(&env));
    let mut r = ws_request("ws://a.dev/?t={{tok}}");
    r.collection_id = "c1".into();
    r.headers = vec![KeyValue::new("X-Key", "SEKRET")];
    let p = prepare_ws(&r, &scope, None).unwrap();
    assert!(p.url.contains("SEKRET"));
    let stats = SessionStats {
        summary: WsSessionSummary {
            duration_ms: 1234,
            sent: 3,
            received: 4,
            close_code: Some(1000),
            close_reason: None,
            ..Default::default()
        },
        error: None,
        connected: true,
    };
    let h = reqharbor_core::ws::history_entry(
        "H1",
        42,
        &r,
        &p,
        &stats,
        &reqharbor_core::redact::Redactor::from_scope(&scope),
    );
    let text = serde_json::to_string(&h).unwrap();
    assert!(!text.contains("SEKRET"), "{text}");
    assert_eq!(h.method, "WS");
    assert_eq!(h.resolved_url, "ws://a.dev/?t={{tok}}");
    assert_eq!(h.source_request_id.as_deref(), Some(r.id.as_str()));
    let v = serde_json::to_value(&h).unwrap();
    assert_eq!(v["ws"]["durationMs"], 1234);
    assert_eq!(v["ws"]["received"], 4);
    let summary = reqharbor_core::history::HistorySummary::from_entry(&h);
    assert_eq!(summary.ws.unwrap().sent, 3);
}

#[tokio::test]
async fn ac_ws_8_engine_io_close_is_answered_with_a_websocket_close() {
    let (port, seen) = sio_server(SioMode::EngineClose).await;
    let s = start(&sio_request(port, r#"{"token":"t"}"#), &VarScope::empty());
    let (stats, _) = s.finish().await;
    assert!(stats.connected);
    // The server records the close frame it got (give it a moment to read it).
    for _ in 0..50 {
        if seen.lock().unwrap().iter().any(|l| l.starts_with("close ")) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let seen = seen.lock().unwrap().clone();
    assert!(seen.contains(&"close 1000".to_string()), "{seen:?}");
}

#[tokio::test]
async fn invalid_url_errors_never_show_secret_values() {
    let prepared = reqharbor_core::ws::PreparedWs {
        url: "ws://exa mple/?t=SEKRET".into(),
        display_url: "ws://exa mple/?t={{tok}}".into(),
        headers: vec![],
        protocols: vec![],
        socket_io: None,
        timeout_ms: 1_000,
        verify_tls: true,
        auto_reconnect: false,
        unresolved: vec![],
    };
    let (_keep, rx) = mpsc::unbounded_channel();
    let frames: Arc<Mutex<Vec<WsFrame>>> = Arc::default();
    let sink = frames.clone();
    let stats = run(
        prepared,
        rx,
        move |f| sink.lock().unwrap().push(f),
        CancellationToken::new(),
    )
    .await;
    let e = stats.error.unwrap();
    assert!(e.message.contains("Invalid URL"), "{}", e.message);
    assert!(e.message.contains("{{tok}}"), "{}", e.message);
    assert!(!e.message.contains("SEKRET"));
    let frames = frames.lock().unwrap();
    assert!(frames.iter().all(|f| !f.data.contains("SEKRET")));
}

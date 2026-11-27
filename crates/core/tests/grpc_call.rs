//! gRPC calls against an in-process tonic server with reflection
//! (AC-GRPC-3..9): reflection v1 / v1alpha, `.proto` compilation with
//! imports, unary and all three stream kinds with dynamic messages, error
//! status details, deadlines, cancellation, TLS with a self-signed
//! certificate and `verifyTls: false`, JSON mapping and the descriptor cache.

use futures_util::StreamExt;
use prost::Message as _;
use prost_reflect::{DescriptorPool, DynamicMessage, MessageDescriptor, Value as PValue};
use reqharbor_core::dynamic::DynamicCtx;
use reqharbor_core::grpc::{self, GrpcCommand, GrpcError, GrpcFrame, GrpcFrameKind};
use reqharbor_core::model::*;
use reqharbor_core::vars::VarScope;
use std::convert::Infallible;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tonic::{Request as TReq, Response as TResp, Status, Streaming};

const TYPES_PROTO: &str = r#"syntax = "proto3";
package demo;
enum Mood { MOOD_UNSPECIFIED = 0; HAPPY = 1; GRUMPY = 2; }
"#;

const GREETER_PROTO: &str = r#"syntax = "proto3";
package demo;
import "google/protobuf/timestamp.proto";
import "shared/types.proto";

service Greeter {
  rpc SayHello(HelloRequest) returns (HelloReply);
  rpc LotsOfReplies(HelloRequest) returns (stream HelloReply);
  rpc LotsOfGreetings(stream HelloRequest) returns (HelloReply);
  rpc Chat(stream HelloRequest) returns (stream HelloReply);
  rpc Fail(HelloRequest) returns (HelloReply);
  rpc Slow(HelloRequest) returns (HelloReply);
}

message HelloRequest {
  string name = 1;
  int64 big = 2;
  Mood mood = 3;
  bytes blob = 4;
  google.protobuf.Timestamp at = 5;
  map<string, int32> tags = 6;
  oneof choice { string a = 7; int32 b = 8; }
  repeated string items = 9;
  optional string nick = 10;
}

message HelloReply {
  string message = 1;
  HelloRequest echo = 2;
  int32 count = 3;
}
"#;

/// The test protos on disk: `<dir>/greeter.proto` importing
/// `shared/types.proto` from an import path `<dir>/include`.
fn write_protos() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let include = dir.path().join("include");
    std::fs::create_dir_all(include.join("shared")).unwrap();
    std::fs::write(include.join("shared/types.proto"), TYPES_PROTO).unwrap();
    let greeter = dir.path().join("greeter.proto");
    std::fs::write(&greeter, GREETER_PROTO).unwrap();
    (dir, greeter, include)
}

fn compiled() -> DescriptorPool {
    let (_d, greeter, include) = write_protos();
    grpc::compile_files(
        &[greeter.to_string_lossy().into_owned()],
        &[include.to_string_lossy().into_owned()],
    )
    .unwrap()
}

// ------------------------------------------------------------------ server

#[derive(Clone)]
struct Codec(MessageDescriptor);
struct Enc;
struct Dec(MessageDescriptor);

impl tonic::codec::Codec for Codec {
    type Encode = DynamicMessage;
    type Decode = DynamicMessage;
    type Encoder = Enc;
    type Decoder = Dec;
    fn encoder(&mut self) -> Enc {
        Enc
    }
    fn decoder(&mut self) -> Dec {
        Dec(self.0.clone())
    }
}
impl tonic::codec::Encoder for Enc {
    type Item = DynamicMessage;
    type Error = Status;
    fn encode(
        &mut self,
        m: DynamicMessage,
        dst: &mut tonic::codec::EncodeBuf<'_>,
    ) -> Result<(), Status> {
        m.encode(dst).map_err(|e| Status::internal(e.to_string()))
    }
}
impl tonic::codec::Decoder for Dec {
    type Item = DynamicMessage;
    type Error = Status;
    fn decode(
        &mut self,
        src: &mut tonic::codec::DecodeBuf<'_>,
    ) -> Result<Option<DynamicMessage>, Status> {
        DynamicMessage::decode(self.0.clone(), src)
            .map(Some)
            .map_err(|e| Status::internal(e.to_string()))
    }
}

#[derive(Clone, PartialEq, prost::Message)]
struct RpcStatus {
    #[prost(int32, tag = "1")]
    code: i32,
    #[prost(string, tag = "2")]
    message: String,
    #[prost(message, repeated, tag = "3")]
    details: Vec<prost_types::Any>,
}
#[derive(Clone, PartialEq, prost::Message)]
struct FieldViolation {
    #[prost(string, tag = "1")]
    field: String,
    #[prost(string, tag = "2")]
    description: String,
}
#[derive(Clone, PartialEq, prost::Message)]
struct BadRequest {
    #[prost(message, repeated, tag = "1")]
    field_violations: Vec<FieldViolation>,
}

type Out = Pin<Box<dyn futures_util::Stream<Item = Result<DynamicMessage, Status>> + Send>>;

fn reply(
    desc: &MessageDescriptor,
    text: String,
    echo: Option<DynamicMessage>,
    count: i32,
) -> DynamicMessage {
    let mut m = DynamicMessage::new(desc.clone());
    m.set_field_by_name("message", PValue::String(text));
    if let Some(e) = echo {
        m.set_field_by_name("echo", PValue::Message(e));
    }
    m.set_field_by_name("count", PValue::I32(count));
    m
}

fn name_of(m: &DynamicMessage) -> String {
    m.get_field_by_name("name")
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

async fn handle(
    method: String,
    out_desc: MessageDescriptor,
    req: TReq<Streaming<DynamicMessage>>,
) -> Result<TResp<Out>, Status> {
    let auth = req
        .metadata()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let seen_timeout = req
        .metadata()
        .get("grpc-timeout")
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let team_bin = req
        .metadata()
        .get_bin("team-bin")
        .and_then(|v| v.to_bytes().ok())
        .map(|b| b.to_vec());
    let mut inbound = req.into_inner();
    let d = out_desc.clone();
    let stream: Out = match method.as_str() {
        "SayHello" => {
            let m = inbound
                .message()
                .await?
                .unwrap_or_else(|| DynamicMessage::new(out_desc.clone()));
            let r = reply(&d, format!("Hello, {}!", name_of(&m)), Some(m), 1);
            Box::pin(futures_util::stream::iter([Ok(r)]))
        }
        "LotsOfReplies" => {
            let m = inbound.message().await?.unwrap();
            let name = name_of(&m);
            Box::pin(futures_util::stream::iter((1..=3).map(move |i| {
                Ok(reply(&d, format!("Hello {i}, {name}"), None, i))
            })))
        }
        "LotsOfGreetings" => {
            let mut names = Vec::new();
            while let Some(m) = inbound.message().await? {
                names.push(name_of(&m));
            }
            let n = names.len() as i32;
            let r = reply(&d, format!("Hello {}", names.join(", ")), None, n);
            Box::pin(futures_util::stream::iter([Ok(r)]))
        }
        "Chat" => Box::pin(
            inbound.map(move |m| m.map(|m| reply(&d, format!("echo: {}", name_of(&m)), None, 0))),
        ),
        "Fail" => {
            let detail = BadRequest {
                field_violations: vec![FieldViolation {
                    field: "name".into(),
                    description: "unknown user".into(),
                }],
            };
            let st = RpcStatus {
                code: 5,
                message: "no such user".into(),
                details: vec![prost_types::Any {
                    type_url: "type.googleapis.com/google.rpc.BadRequest".into(),
                    value: detail.encode_to_vec(),
                }],
            };
            return Err(Status::with_details(
                tonic::Code::NotFound,
                "no such user",
                bytes::Bytes::from(st.encode_to_vec()),
            ));
        }
        "Slow" => {
            tokio::time::sleep(Duration::from_secs(3)).await;
            Box::pin(futures_util::stream::iter([Ok(reply(
                &d,
                "late".into(),
                None,
                0,
            ))]))
        }
        other => return Err(Status::unimplemented(other.to_string())),
    };
    let mut resp = TResp::new(stream);
    resp.metadata_mut()
        .insert("x-seen-auth", auth.parse().unwrap());
    if let Some(t) = seen_timeout {
        resp.metadata_mut()
            .insert("x-seen-timeout", t.parse().unwrap());
    }
    if let Some(b) = team_bin {
        resp.metadata_mut()
            .insert("x-team-len", b.len().to_string().parse().unwrap());
    }
    Ok(resp)
}

#[derive(Clone)]
struct Greeter(DescriptorPool);

impl tonic::server::NamedService for Greeter {
    const NAME: &'static str = "demo.Greeter";
}

impl tower::Service<http::Request<tonic::body::Body>> for Greeter {
    type Response = http::Response<tonic::body::Body>;
    type Error = Infallible;
    type Future =
        Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Infallible>> + Send>>;
    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Infallible>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
        let method = req
            .uri()
            .path()
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_string();
        let input = self.0.get_message_by_name("demo.HelloRequest").unwrap();
        let output = self.0.get_message_by_name("demo.HelloReply").unwrap();
        Box::pin(async move {
            let svc = tower::service_fn(move |r: TReq<Streaming<DynamicMessage>>| {
                handle(method.clone(), output.clone(), r)
            });
            let mut g = tonic::server::Grpc::new(Codec(input));
            Ok(g.streaming(svc, req).await)
        })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Reflection {
    V1,
    V1Alpha,
    None,
}

fn fds_bytes(pool: &DescriptorPool) -> &'static [u8] {
    Box::leak(pool.encode_to_vec().into_boxed_slice())
}

fn router(pool: &DescriptorPool, reflection: Reflection) -> tonic::service::Routes {
    let mut b = tonic::service::Routes::builder();
    b.add_service(Greeter(pool.clone()));
    let fds = fds_bytes(pool);
    match reflection {
        Reflection::V1 => {
            b.add_service(
                tonic_reflection::server::Builder::configure()
                    .register_encoded_file_descriptor_set(fds)
                    .build_v1()
                    .unwrap(),
            );
        }
        Reflection::V1Alpha => {
            b.add_service(
                tonic_reflection::server::Builder::configure()
                    .register_encoded_file_descriptor_set(fds)
                    .build_v1alpha()
                    .unwrap(),
            );
        }
        Reflection::None => {}
    }
    b.routes()
}

/// A plaintext server; returns its port.
async fn serve(reflection: Reflection) -> u16 {
    let pool = compiled();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let incoming = futures_util::stream::unfold(listener, |l| async move {
        let r = l.accept().await.map(|(s, _)| s);
        Some((r, l))
    });
    let routes = router(&pool, reflection);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    port
}

/// A TLS server with a self-signed certificate for `localhost`.
async fn serve_tls() -> u16 {
    reqharbor_core::http::install_crypto_provider();
    let pool = compiled();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let key = rustls::pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into());
    let mut cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert.cert.der().clone()], key)
    .unwrap();
    cfg.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let incoming = futures_util::stream::unfold((listener, acceptor), |(l, a)| async move {
        loop {
            let Ok((s, _)) = l.accept().await else {
                continue;
            };
            match a.accept(s).await {
                Ok(t) => return Some((Ok::<_, std::io::Error>(t), (l, a))),
                Err(_) => continue,
            }
        }
    });
    let routes = router(&pool, Reflection::V1);
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_routes(routes)
            .serve_with_incoming(incoming)
            .await
            .unwrap();
    });
    port
}

// ----------------------------------------------------------------- helpers

fn request(url: &str, method: &str, message: &str) -> Request {
    Request {
        kind: RequestKind::Grpc,
        url: url.into(),
        grpc: Some(GrpcConfig {
            service: "demo.Greeter".into(),
            method: method.into(),
            message: message.into(),
            ..GrpcConfig::default()
        }),
        ..Request::default()
    }
}

fn prepared(r: &Request) -> grpc::PreparedGrpc {
    grpc::prepare_grpc(r, &VarScope::empty(), None, true, &DynamicCtx::new()).unwrap()
}

async fn reflected(p: &grpc::PreparedGrpc) -> DescriptorPool {
    grpc::fetch_reflection(p, &CancellationToken::new())
        .await
        .unwrap()
}

async fn unary(r: &Request) -> grpc::CallStats {
    let p = prepared(r);
    let pool = reflected(&p).await;
    let plan = grpc::plan(p, &pool).unwrap();
    grpc::invoke(plan, None, CancellationToken::new()).await
}

fn json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).unwrap()
}

/// Run a stream call, feeding `commands`; collect the frames.
async fn stream(
    r: &Request,
    commands: Vec<(u64, Option<&str>)>,
) -> (grpc::CallStats, Vec<GrpcFrame>) {
    let p = prepared(r);
    let pool = reflected(&p).await;
    let plan = grpc::plan(p, &pool).unwrap();
    let input = plan.method.input();
    let (tx, rx) = mpsc::unbounded_channel();
    let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let f2 = frames.clone();
    let runner = tokio::spawn(grpc::run_call(
        plan,
        None,
        rx,
        move |f| f2.lock().unwrap().push(f),
        CancellationToken::new(),
        None,
    ));
    for (delay, msg) in commands {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        match msg {
            Some(text) => {
                let (m, t) =
                    grpc::resolve_stream_message(text, &VarScope::empty(), &input).unwrap();
                let (reply, rx) = tokio::sync::oneshot::channel();
                tx.send(GrpcCommand::Send(m, t, reply)).unwrap();
                rx.await.unwrap().unwrap();
            }
            None => tx.send(GrpcCommand::End).unwrap(),
        }
    }
    let stats = tokio::time::timeout(Duration::from_secs(10), runner)
        .await
        .unwrap()
        .unwrap();
    let frames = frames.lock().unwrap().clone();
    (stats, frames)
}

fn kinds(frames: &[GrpcFrame]) -> Vec<String> {
    frames
        .iter()
        .map(|f| format!("{:?}:{:?}", f.dir, f.kind).to_lowercase())
        .collect()
}

// ------------------------------------------------------------------- tests

#[tokio::test]
async fn ac_grpc_3_reflection_v1_lists_services_and_their_types() {
    let port = serve(Reflection::V1).await;
    let r = request(&format!("grpc://127.0.0.1:{port}"), "SayHello", "{}");
    let p = prepared(&r);
    let pool = reflected(&p).await;
    let d = grpc::describe(
        &pool,
        &p.display_origin,
        grpc::DescriptorSource::Reflection,
        vec![],
        1,
    );
    assert_eq!(d.url, format!("grpc://127.0.0.1:{port}"));
    let names: Vec<_> = d.services.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, ["demo.Greeter"], "reflection services are hidden");
    let kinds: Vec<_> = d.services[0]
        .methods
        .iter()
        .map(|m| (m.name.as_str(), m.call_kind))
        .collect();
    assert!(kinds.contains(&("SayHello", GrpcCallKind::Unary)));
    assert!(kinds.contains(&("LotsOfReplies", GrpcCallKind::ServerStreaming)));
    assert!(kinds.contains(&("LotsOfGreetings", GrpcCallKind::ClientStreaming)));
    assert!(kinds.contains(&("Chat", GrpcCallKind::BidiStreaming)));
    let req = &d.messages["demo.HelloRequest"];
    let f = |n: &str| req.fields.iter().find(|f| f.name == n).unwrap();
    assert_eq!(f("mood").kind, "enum");
    assert_eq!(f("mood").type_name.as_deref(), Some("demo.Mood"));
    assert_eq!(f("tags").map_key.as_deref(), Some("string"));
    assert_eq!(f("tags").kind, "int32");
    assert!(!f("tags").repeated);
    assert!(f("items").repeated);
    assert_eq!(f("a").oneof.as_deref(), Some("choice"));
    assert_eq!(
        f("nick").oneof,
        None,
        "synthetic optional oneofs are left out"
    );
    assert_eq!(
        f("at").type_name.as_deref(),
        Some("google.protobuf.Timestamp")
    );
    assert!(d.messages.contains_key("google.protobuf.Timestamp"));
    assert_eq!(
        d.enums["demo.Mood"].values,
        ["MOOD_UNSPECIFIED", "HAPPY", "GRUMPY"]
    );
    assert!(d.size_bytes > 0);
}

#[tokio::test]
async fn ac_grpc_3_reflection_falls_back_to_v1alpha() {
    let port = serve(Reflection::V1Alpha).await;
    let p = prepared(&request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        "{}",
    ));
    let pool = reflected(&p).await;
    assert!(pool.get_service_by_name("demo.Greeter").is_some());
}

#[tokio::test]
async fn ac_grpc_3_a_server_without_reflection_is_reported() {
    let port = serve(Reflection::None).await;
    let p = prepared(&request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        "{}",
    ));
    let e = grpc::fetch_reflection(&p, &CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(e, GrpcError::ReflectionUnsupported);
    assert_eq!(
        e.to_string(),
        "This server doesn't support reflection. Use .proto files instead."
    );
}

#[tokio::test]
async fn ac_grpc_7_unary_with_metadata_and_full_json() {
    let port = serve(Reflection::V1).await;
    let mut r = request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        r#"{"name":"Ada","big":"9007199254740993","mood":"HAPPY","blob":"AQI=","tags":{"x":1},"at":"2026-01-02T03:04:05Z"}"#,
    );
    r.auth = Auth::bearer("tok");
    r.headers = vec![KeyValue::new("team-bin", "AQID")];
    let stats = unary(&r).await;
    let st = stats.status.clone().unwrap();
    assert_eq!((st.code, st.name.as_str()), (0, "OK"), "{st:?}");
    assert_eq!((stats.sent, stats.received), (1, 1));
    let (resp, size) = stats.first_response.clone().unwrap();
    assert!(size > 0);
    let v = json(&resp);
    assert_eq!(v["message"], "Hello, Ada!");
    assert_eq!(v["echo"]["big"], "9007199254740993", "int64 as a string");
    assert_eq!(v["echo"]["mood"], "HAPPY");
    assert_eq!(v["echo"]["blob"], "AQI=");
    assert_eq!(v["echo"]["tags"]["x"], 1);
    assert_eq!(v["echo"]["at"], "2026-01-02T03:04:05Z");
    // Defaults are shown too.
    assert_eq!(v["echo"]["items"], serde_json::json!([]));
    assert!(v["echo"].get("a").is_none() || v["echo"]["a"].is_string());
    let header = |n: &str| {
        stats
            .headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(header("x-seen-auth").as_deref(), Some("Bearer tok"));
    assert_eq!(
        header("x-team-len").as_deref(),
        Some("3"),
        "-bin metadata is sent as bytes"
    );
}

#[tokio::test]
async fn ac_grpc_8_server_streaming() {
    let port = serve(Reflection::V1).await;
    let r = request(
        &format!("grpc://127.0.0.1:{port}"),
        "LotsOfReplies",
        r#"{"name":"Bo"}"#,
    );
    let (stats, frames) = stream(&r, vec![]).await;
    assert_eq!(stats.status.as_ref().unwrap().code, 0);
    assert_eq!((stats.sent, stats.received), (1, 3));
    let k = kinds(&frames);
    assert_eq!(k[0], "system:open");
    assert_eq!(k[1], "out:message");
    assert_eq!(k[2], "out:end");
    assert_eq!(k[3], "in:headers");
    assert_eq!(&k[4..7], ["in:message", "in:message", "in:message"]);
    assert_eq!(k[k.len() - 2], "system:status");
    assert_eq!(k[k.len() - 1], "system:close");
    let last = frames.last().unwrap();
    assert!(last.last);
    assert_eq!(last.data, "Finished · 0 OK");
    assert_eq!(json(&frames[6].data)["message"], "Hello 3, Bo");
    assert!(frames[1..].iter().all(|f| f.ts >= frames[0].ts));
}

#[tokio::test]
async fn ac_grpc_8_client_streaming() {
    let port = serve(Reflection::V1).await;
    let r = request(&format!("grpc://127.0.0.1:{port}"), "LotsOfGreetings", "");
    let (stats, frames) = stream(
        &r,
        vec![
            (20, Some(r#"{"name":"a"}"#)),
            (0, Some(r#"{"name":"b"}"#)),
            (0, Some(r#"{"name":"c"}"#)),
            (0, None),
        ],
    )
    .await;
    assert_eq!(stats.status.as_ref().unwrap().code, 0);
    assert_eq!((stats.sent, stats.received), (3, 1));
    let reply = frames
        .iter()
        .find(|f| f.dir == reqharbor_core::ws::Dir::In && f.kind == GrpcFrameKind::Message)
        .unwrap();
    assert_eq!(json(&reply.data)["message"], "Hello a, b, c");
    assert_eq!(json(&reply.data)["count"], 3);
}

#[tokio::test]
async fn ac_grpc_8_bidi_echoes_each_message_and_ends() {
    let port = serve(Reflection::V1).await;
    let r = request(&format!("grpc://127.0.0.1:{port}"), "Chat", "");
    let (stats, frames) = stream(
        &r,
        vec![
            (20, Some(r#"{"name":"one"}"#)),
            (100, Some(r#"{"name":"two"}"#)),
            (100, None),
        ],
    )
    .await;
    assert_eq!(stats.status.as_ref().unwrap().code, 0);
    assert_eq!((stats.sent, stats.received), (2, 2));
    let k = kinds(&frames);
    // Interleaved: each echo arrives before the next send.
    let first_in = k.iter().position(|x| x == "in:message").unwrap();
    let second_out = k
        .iter()
        .enumerate()
        .filter(|(_, x)| *x == "out:message")
        .nth(1)
        .unwrap()
        .0;
    assert!(first_in < second_out, "{k:?}");
    assert!(k.contains(&"out:end".to_string()));
    let echoes: Vec<_> = frames
        .iter()
        .filter(|f| f.dir == reqharbor_core::ws::Dir::In && f.kind == GrpcFrameKind::Message)
        .map(|f| json(&f.data)["message"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(echoes, ["echo: one", "echo: two"]);
}

#[tokio::test]
async fn ac_grpc_8_cancel_ends_a_stream_as_cancelled() {
    let port = serve(Reflection::V1).await;
    let r = request(&format!("grpc://127.0.0.1:{port}"), "Chat", "");
    let p = prepared(&r);
    let pool = reflected(&p).await;
    let plan = grpc::plan(p, &pool).unwrap();
    let (tx, rx) = mpsc::unbounded_channel();
    let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
    let f2 = frames.clone();
    let runner = tokio::spawn(grpc::run_call(
        plan,
        None,
        rx,
        move |f| f2.lock().unwrap().push(f),
        CancellationToken::new(),
        None,
    ));
    tokio::time::sleep(Duration::from_millis(100)).await;
    tx.send(GrpcCommand::Cancel).unwrap();
    let stats = tokio::time::timeout(Duration::from_secs(5), runner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stats.status.unwrap().code, 1);
    assert_eq!(frames.lock().unwrap().last().unwrap().data, "Cancelled");
}

#[tokio::test]
async fn ac_grpc_7_error_status_with_details() {
    let port = serve(Reflection::V1).await;
    let stats = unary(&request(&format!("grpc://127.0.0.1:{port}"), "Fail", "{}")).await;
    let st = stats.status.unwrap();
    assert_eq!(
        (st.code, st.name.as_str(), st.message.as_str()),
        (5, "NOT_FOUND", "no such user")
    );
    assert_eq!(st.details.len(), 1);
    let d = &st.details[0];
    assert_eq!(d.type_url, "type.googleapis.com/google.rpc.BadRequest");
    let v = json(d.json.as_deref().unwrap());
    assert_eq!(v["fieldViolations"][0]["field"], "name");
    assert_eq!(v["fieldViolations"][0]["description"], "unknown user");
    assert!(!d.base64.is_empty());
    assert!(stats.error.is_none());
}

#[tokio::test]
async fn ac_grpc_7_deadline_exceeded() {
    let port = serve(Reflection::V1).await;
    let mut r = request(&format!("grpc://127.0.0.1:{port}"), "Slow", "{}");
    r.settings.timeout_ms = 2000;
    let p = prepared(&r);
    let pool = reflected(&p).await;
    let mut p2 = p.clone();
    p2.timeout_ms = 300;
    let plan = grpc::plan(p2, &pool).unwrap();
    let t0 = std::time::Instant::now();
    let stats = grpc::invoke(plan, None, CancellationToken::new()).await;
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    let st = stats.status.unwrap();
    assert_eq!(
        (st.code, st.name.as_str()),
        (4, "DEADLINE_EXCEEDED"),
        "{st:?}"
    );
}

#[tokio::test]
async fn ac_grpc_7_cancel_is_a_cancelled_error() {
    let port = serve(Reflection::V1).await;
    let r = request(&format!("grpc://127.0.0.1:{port}"), "Slow", "{}");
    let p = prepared(&r);
    let pool = reflected(&p).await;
    let plan = grpc::plan(p, &pool).unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        c2.cancel();
    });
    let stats = grpc::invoke(plan, None, cancel).await;
    assert!(stats.status.is_none());
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Cancelled);
}

#[tokio::test]
async fn ac_grpc_7_tls_self_signed_needs_verify_off() {
    let port = serve_tls().await;
    let mut r = request(
        &format!("grpcs://localhost:{port}"),
        "SayHello",
        r#"{"name":"tls"}"#,
    );
    r.settings.verify_tls = false;
    let stats = unary(&r).await;
    assert_eq!(stats.status.as_ref().unwrap().code, 0, "{stats:?}");
    assert_eq!(
        json(&stats.first_response.unwrap().0)["message"],
        "Hello, tls!"
    );
    // Verified: the self-signed certificate is refused as a TLS error.
    r.settings.verify_tls = true;
    let p = prepared(&r);
    let e = grpc::connect(&p.target, true, 5000).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Tls, "{e:?}");
}

#[tokio::test]
async fn ac_req_9_connect_failures_are_typed() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let p = prepared(&request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        "{}",
    ));
    let e = grpc::connect(&p.target, true, 2000).await.unwrap_err();
    assert_eq!(e.kind, ErrorKind::Connect);
    let pool = compiled();
    let plan = grpc::plan(p, &pool).unwrap();
    let stats = grpc::invoke(plan, None, CancellationToken::new()).await;
    assert_eq!(stats.error.unwrap().kind, ErrorKind::Connect);
    assert!(stats.status.is_none());
}

#[test]
fn ac_grpc_4_proto_files_compile_with_imports_and_report_errors() {
    let pool = compiled();
    assert!(pool.get_service_by_name("demo.Greeter").is_some());
    assert!(pool.get_enum_by_name("demo.Mood").is_some());
    let (dir, greeter, include) = write_protos();
    // Without the import path, shared/types.proto can't be found.
    let e = grpc::compile_files(&[greeter.to_string_lossy().into_owned()], &[]).unwrap_err();
    let GrpcError::ProtoInvalid(m) = e else {
        panic!()
    };
    assert!(m.contains("shared/types.proto"), "{m}");
    // A syntax error names file, line and column.
    let bad = dir.path().join("bad.proto");
    std::fs::write(
        &bad,
        "syntax = \"proto3\";\npackage x;\nmessage M {\n  string = 1;\n}\n",
    )
    .unwrap();
    let e = grpc::compile_files(&[bad.to_string_lossy().into_owned()], &[]).unwrap_err();
    let GrpcError::ProtoInvalid(m) = e else {
        panic!()
    };
    assert!(m.starts_with("bad.proto:4:"), "{m}");
    // A file without services is refused; a missing file too.
    let plain = include.join("shared/types.proto");
    let e = grpc::compile_files(&[plain.to_string_lossy().into_owned()], &[]).unwrap_err();
    assert_eq!(
        e,
        GrpcError::ProtoInvalid("The .proto files declare no service".into())
    );
    assert!(grpc::compile_files(&["/nope/x.proto".into()], &[]).is_err());
    assert!(grpc::compile_files(&[], &[]).is_err());
}

#[test]
fn ac_grpc_6_json_mapping_round_trips_and_errors() {
    let pool = compiled();
    let desc = pool.get_message_by_name("demo.HelloRequest").unwrap();
    let input = r#"{"name":"x","big":"-12","mood":"GRUMPY","blob":"AAEC","at":"2026-01-02T03:04:05.500Z","tags":{"k":7},"b":3,"items":["p","q"],"nick":"n"}"#;
    let m = grpc::message_from_json(&desc, input).unwrap();
    let out = grpc::message_to_json(&m);
    let again = grpc::message_from_json(&desc, &out).unwrap();
    assert_eq!(m, again, "JSON → proto → JSON → proto is lossless");
    let v = json(&out);
    assert_eq!(v["big"], "-12");
    assert_eq!(v["mood"], "GRUMPY");
    assert_eq!(v["b"], 3);
    // Proto field names and enum numbers are accepted.
    let m2 = grpc::message_from_json(&desc, r#"{"name":"x","mood":2}"#).unwrap();
    assert_eq!(json(&grpc::message_to_json(&m2))["mood"], "GRUMPY");
    // Empty is {}; every field is printed.
    let e = grpc::message_from_json(&desc, "  ").unwrap();
    let v = json(&grpc::message_to_json(&e));
    assert_eq!(v["name"], "");
    assert_eq!(v["big"], "0");
    assert_eq!(v["mood"], "MOOD_UNSPECIFIED");
    let err = grpc::message_from_json(&desc, r#"{"name": }"#).unwrap_err();
    assert!(err.starts_with("Invalid JSON at line 1, col"), "{err}");
    let err = grpc::message_from_json(&desc, r#"{"nmae":"x"}"#).unwrap_err();
    assert!(
        err.starts_with("The message doesn't match demo.HelloRequest:"),
        "{err}"
    );
    assert!(err.contains("nmae"), "{err}");
}

#[test]
fn ac_grpc_3_descriptor_cache_put_get_forget() {
    let dir = tempfile::tempdir().unwrap();
    let cache = grpc::DescriptorCache::new(dir.path());
    let pool = compiled();
    assert!(cache.get("grpc://a:1").is_none());
    cache.put("grpc://a:1", 42, &pool).unwrap();
    let got = cache.get("grpc://a:1").unwrap();
    assert_eq!(got.fetched_at, 42);
    assert!(got.pool.get_service_by_name("demo.Greeter").is_some());
    assert!(cache.get("grpc://b:1").is_none());
    // A corrupt entry is dropped on read.
    let path = std::fs::read_dir(cache.dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::write(&path, b"{not json").unwrap();
    assert!(cache.get("grpc://a:1").is_none());
    assert!(!Path::new(&path).exists());
    cache.put("grpc://a:1", 1, &pool).unwrap();
    assert!(cache.remove("grpc://a:1").unwrap());
    assert!(!cache.remove("grpc://a:1").unwrap());
}

#[tokio::test]
async fn ac_grpc_10_history_entry_is_redacted_and_keeps_the_unary_answer() {
    let port = serve(Reflection::V1).await;
    let r = request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        r#"{"name":"s3cret-value"}"#,
    );
    let p = prepared(&r);
    let stats = unary(&r).await;
    let mut red = reqharbor_core::redact::Redactor::default();
    red.add("tok", "s3cret-value");
    let e = grpc::history_entry("h1", 1, &r, &p, GrpcCallKind::Unary, &stats, &red);
    assert_eq!(e.method, "GRPC");
    assert_eq!(
        e.resolved_url,
        format!("grpc://127.0.0.1:{port}/demo.Greeter/SayHello")
    );
    let g = e.grpc.unwrap();
    assert_eq!(g.status.unwrap().code, 0);
    let resp = g.response.unwrap();
    assert!(resp.contains("{{tok}}") && !resp.contains("s3cret-value"));
    assert!(g.headers.is_some() && g.trailers.is_some());
}

#[tokio::test]
async fn ac_grpc_7_unary_sends_the_deadline_as_a_grpc_timeout_header() {
    let port = serve(Reflection::V1).await;
    let mut r = request(&format!("grpc://127.0.0.1:{port}"), "SayHello", "{}");
    r.settings.timeout_ms = 1500;
    let stats = unary(&r).await;
    assert_eq!(stats.status.as_ref().unwrap().code, 0, "{stats:?}");
    let seen = stats
        .headers
        .iter()
        .find(|(k, _)| k == "x-seen-timeout")
        .map(|(_, v)| v.clone())
        .expect("the server saw a grpc-timeout header");
    // gRPC wire format: 1-8 digits and a unit (H M S m u n).
    assert!(
        seen.len() >= 2
            && seen.len() <= 9
            && seen[..seen.len() - 1].bytes().all(|b| b.is_ascii_digit())
            && "HMSmun".contains(&seen[seen.len() - 1..]),
        "{seen}"
    );
    let (digits, unit) = seen.split_at(seen.len() - 1);
    let ms: u64 = digits.parse::<u64>().unwrap()
        * match unit {
            "m" => 1,
            "S" => 1000,
            "u" => 0,
            "n" => 0,
            _ => 60_000,
        };
    assert!(
        ms <= 1500 && (ms >= 1000 || unit == "u" || unit == "n"),
        "{seen}"
    );
    // No deadline (0) sends no header.
    r.settings.timeout_ms = 0;
    let stats = unary(&r).await;
    assert!(stats.headers.iter().all(|(k, _)| k != "x-seen-timeout"));
}

#[test]
fn ac_grpc_4_imports_cannot_leave_the_import_paths() {
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret.proto");
    std::fs::write(&secret, "syntax = \"proto3\";\npackage s;\nmessage S {}\n").unwrap();
    let inc = dir.path().join("inc");
    std::fs::create_dir_all(&inc).unwrap();
    let compile = |import: &str| {
        let main = inc.join("main.proto");
        std::fs::write(
            &main,
            format!(
                "syntax = \"proto3\";\npackage m;\nimport \"{import}\";\nservice A {{ rpc B(M) returns (M); }}\nmessage M {{}}\n"
            ),
        )
        .unwrap();
        grpc::compile_files(
            &[main.to_string_lossy().into_owned()],
            &[inc.to_string_lossy().into_owned()],
        )
    };
    for bad in [
        secret.to_string_lossy().into_owned(),
        "/etc/hostname".to_string(),
        "../secret.proto".to_string(),
        "a/../../secret.proto".to_string(),
    ] {
        let GrpcError::ProtoInvalid(m) = compile(&bad).unwrap_err() else {
            panic!("{bad}")
        };
        assert!(
            m.starts_with("main.proto:3:") && m.contains("can't be absolute or contain \"..\""),
            "{bad}: {m}"
        );
    }
    // A plain relative import still works.
    std::fs::write(
        inc.join("ok.proto"),
        "syntax = \"proto3\";\npackage o;\nmessage O {}\n",
    )
    .unwrap();
    assert!(compile("ok.proto").is_ok());
}

/// A pool whose encoding is over 10 MB (a huge file option).
fn oversized_pool() -> DescriptorPool {
    let big = "x".repeat(grpc::DESCRIPTOR_MAX_BYTES + 1024);
    let fds = prost_types::FileDescriptorSet {
        file: vec![prost_types::FileDescriptorProto {
            name: Some("big.proto".into()),
            package: Some("big".into()),
            syntax: Some("proto3".into()),
            options: Some(prost_types::FileOptions {
                go_package: Some(big),
                ..Default::default()
            }),
            ..Default::default()
        }],
    };
    DescriptorPool::from_file_descriptor_set(fds).unwrap()
}

#[test]
fn ac_grpc_3_cache_refuses_pools_that_do_not_build_or_are_too_big() {
    let dir = tempfile::tempdir().unwrap();
    let cache = grpc::DescriptorCache::new(dir.path());
    // Over 10 MB: refused, nothing written.
    let e = cache.put("grpc://big:1", 1, &oversized_pool()).unwrap_err();
    assert!(matches!(e, GrpcError::TooLarge(n) if n > grpc::DESCRIPTOR_MAX_BYTES as u64));
    assert!(cache.get("grpc://big:1").is_none());
    assert!(!cache.dir().exists() || std::fs::read_dir(cache.dir()).unwrap().next().is_none());
    // A cached set that no longer builds (a file importing a missing one)
    // counts as missing and is removed.
    cache.put("grpc://a:1", 1, &compiled()).unwrap();
    let path = std::fs::read_dir(cache.dir())
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let broken = prost_types::FileDescriptorSet {
        file: vec![prost_types::FileDescriptorProto {
            name: Some("a.proto".into()),
            dependency: vec!["missing.proto".into()],
            ..Default::default()
        }],
    };
    use base64::Engine as _;
    let entry = serde_json::json!({
        "schemaVersion": 1,
        "url": "grpc://a:1",
        "fetchedAt": 1,
        "descriptorSet": base64::engine::general_purpose::STANDARD.encode(broken.encode_to_vec()),
    });
    std::fs::write(&path, serde_json::to_vec(&entry).unwrap()).unwrap();
    assert!(cache.get("grpc://a:1").is_none());
    assert!(!path.exists());
}

// ------------------------------------------- hostile / stalled reflection

mod fake_reflection {
    use super::*;
    pub use tonic_reflection::pb::v1::server_reflection_request::MessageRequest;
    use tonic_reflection::pb::v1::{
        FileDescriptorResponse, ListServiceResponse, ServerReflectionRequest,
        ServerReflectionResponse, ServiceResponse, server_reflection_response::MessageResponse,
    };

    /// What the fake answers; `None` = never answers.
    pub type Answer = Arc<dyn Fn(&MessageRequest) -> Option<MessageResponse> + Send + Sync>;

    #[derive(Clone)]
    pub struct Fake(pub Answer);

    impl tonic::server::NamedService for Fake {
        const NAME: &'static str = "grpc.reflection.v1.ServerReflection";
    }

    impl tower::Service<http::Request<tonic::body::Body>> for Fake {
        type Response = http::Response<tonic::body::Body>;
        type Error = Infallible;
        type Future =
            Pin<Box<dyn std::future::Future<Output = Result<Self::Response, Infallible>> + Send>>;
        fn poll_ready(
            &mut self,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), Infallible>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn call(&mut self, req: http::Request<tonic::body::Body>) -> Self::Future {
            let answer = self.0.clone();
            Box::pin(async move {
                let svc = tower::service_fn(move |r: TReq<Streaming<ServerReflectionRequest>>| {
                    let answer = answer.clone();
                    async move {
                        let out = r.into_inner().then(move |m| {
                            let answer = answer.clone();
                            async move {
                                let m = m?;
                                let req = m.message_request.clone().unwrap();
                                match answer(&req) {
                                    Some(resp) => Ok(ServerReflectionResponse {
                                        valid_host: String::new(),
                                        original_request: Some(m),
                                        message_response: Some(resp),
                                    }),
                                    None => std::future::pending().await,
                                }
                            }
                        });
                        let out: Pin<
                            Box<
                                dyn futures_util::Stream<
                                        Item = Result<ServerReflectionResponse, Status>,
                                    > + Send,
                            >,
                        > = Box::pin(out);
                        Ok::<_, Status>(TResp::new(out))
                    }
                });
                let mut g = tonic::server::Grpc::new(tonic_prost::ProstCodec::<
                    ServerReflectionResponse,
                    ServerReflectionRequest,
                >::default());
                Ok(g.streaming(svc, req).await)
            })
        }
    }

    pub async fn serve(answer: Answer) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let incoming = futures_util::stream::unfold(listener, |l| async move {
            let r = l.accept().await.map(|(s, _)| s);
            Some((r, l))
        });
        let mut b = tonic::service::Routes::builder();
        b.add_service(Fake(answer));
        let routes = b.routes();
        tokio::spawn(async move {
            let _ = tonic::transport::Server::builder()
                .add_routes(routes)
                .serve_with_incoming(incoming)
                .await;
        });
        port
    }

    pub fn list(names: &[&str]) -> MessageResponse {
        MessageResponse::ListServicesResponse(ListServiceResponse {
            service: names
                .iter()
                .map(|n| ServiceResponse {
                    name: n.to_string(),
                })
                .collect(),
        })
    }

    pub fn files(list: Vec<prost_types::FileDescriptorProto>) -> MessageResponse {
        MessageResponse::FileDescriptorResponse(FileDescriptorResponse {
            file_descriptor_proto: list.iter().map(|f| f.encode_to_vec()).collect(),
        })
    }
}

fn plain_file(name: &str, deps: &[&str], padding: usize) -> prost_types::FileDescriptorProto {
    prost_types::FileDescriptorProto {
        name: Some(name.into()),
        dependency: deps.iter().map(|d| d.to_string()).collect(),
        syntax: Some("proto3".into()),
        options: (padding > 0).then(|| prost_types::FileOptions {
            go_package: Some("x".repeat(padding)),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn reflect_against(answer: fake_reflection::Answer) -> Result<DescriptorPool, GrpcError> {
    let port = fake_reflection::serve(answer).await;
    let p = prepared(&request(
        &format!("grpc://127.0.0.1:{port}"),
        "SayHello",
        "{}",
    ));
    let channel = grpc::connect(&p.target, true, 5000).await.unwrap();
    tokio::time::timeout(Duration::from_secs(20), grpc::reflect(&channel, &[]))
        .await
        .expect("reflection stopped")
}

#[tokio::test]
async fn ac_grpc_3_reflection_stops_past_1000_files() {
    use fake_reflection::*;
    let answer: Answer = Arc::new(|req| match req {
        MessageRequest::ListServices(_) => Some(list(&["demo.Greeter"])),
        _ => Some(files(
            (0..=grpc::REFLECTION_MAX_FILES)
                .map(|i| plain_file(&format!("f{i}.proto"), &[], 0))
                .collect(),
        )),
    });
    let e = reflect_against(answer).await.unwrap_err();
    let GrpcError::ReflectionFailed { message, .. } = e else {
        panic!("{e:?}")
    };
    assert!(message.contains("more than 1000 files"), "{message}");
}

#[tokio::test]
async fn ac_grpc_3_reflection_stops_past_10_mb() {
    use fake_reflection::*;
    // Each answer is under the 10 MB message limit; together they pass it.
    let third = 3 * 1024 * 1024;
    let answer: Answer = Arc::new(move |req| match req {
        MessageRequest::ListServices(_) => Some(list(&["demo.Greeter"])),
        MessageRequest::FileContainingSymbol(_) => Some(files(vec![plain_file(
            "root.proto",
            &["a.proto", "b.proto", "c.proto", "d.proto"],
            third,
        )])),
        MessageRequest::FileByFilename(n) => Some(files(vec![plain_file(n, &[], third)])),
        _ => None,
    });
    let e = reflect_against(answer).await.unwrap_err();
    assert!(
        matches!(e, GrpcError::TooLarge(n) if n > grpc::DESCRIPTOR_MAX_BYTES as u64),
        "{e:?}"
    );
}

#[tokio::test]
async fn ac_grpc_3_reflection_decode_limit_is_10_mb() {
    use fake_reflection::*;
    let answer: Answer = Arc::new(|req| match req {
        MessageRequest::ListServices(_) => Some(list(&["demo.Greeter"])),
        _ => Some(files(vec![plain_file(
            "huge.proto",
            &[],
            grpc::DESCRIPTOR_MAX_BYTES + 1024,
        )])),
    });
    let e = reflect_against(answer).await.unwrap_err();
    assert!(matches!(e, GrpcError::ReflectionFailed { .. }), "{e:?}");
}

#[tokio::test]
async fn ac_grpc_3_stalled_reflection_times_out_and_cancels() {
    use fake_reflection::*;
    let answer: Answer = Arc::new(|_| None);
    let port = fake_reflection::serve(answer).await;
    let mut r = request(&format!("grpc://127.0.0.1:{port}"), "SayHello", "{}");
    r.settings.timeout_ms = 400;
    let p = prepared(&r);
    let t0 = std::time::Instant::now();
    let e = grpc::fetch_reflection(&p, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        matches!(&e, GrpcError::Network(s) if s.kind == ErrorKind::Timeout),
        "{e:?}"
    );
    assert!(t0.elapsed() < Duration::from_secs(5));
}

#[tokio::test]
async fn ac_grpc_7_invoke_itself_refuses_streaming_plans() {
    let pool = compiled();
    for m in ["Chat", "LotsOfGreetings", "LotsOfReplies"] {
        let p = prepared(&request("grpc://127.0.0.1:1", m, "{}"));
        let plan = grpc::plan(p, &pool).unwrap();
        // timeoutMs 0 and a closed port would otherwise hang or fail to connect.
        let stats = tokio::time::timeout(
            Duration::from_secs(2),
            grpc::invoke(plan, None, CancellationToken::new()),
        )
        .await
        .expect("invoke must not wait");
        let e = stats.error.expect("an error");
        assert_eq!(e.kind, ErrorKind::Other);
        assert!(e.message.contains("connect to it instead"), "{}", e.message);
        assert!(stats.status.is_none() && !stats.connected && stats.sent == 0);
    }
}

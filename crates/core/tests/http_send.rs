//! End-to-end HTTP tests against local mock servers (no internet needed).

use reqharbor_core::CancellationToken;
use reqharbor_core::http::HttpEngine;
use reqharbor_core::model::*;
use reqharbor_core::send::{BodyKind, ResponseMeta, send_request};
use reqharbor_core::vars::VarScope;
use std::io::Write as _;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{body_string, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req(m: &str, url: &str) -> Request {
    Request {
        method: m.into(),
        url: url.into(),
        ..Request::default()
    }
}

async fn send(r: &Request) -> reqharbor_core::send::SendOutcome {
    let engine = HttpEngine::new().unwrap();
    send_request(
        &engine,
        r,
        &VarScope::empty(),
        None,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn ac_qry_4_307_308_301_302_resend_query_with_body() {
    for status in [307u16, 308, 301, 302] {
        let server = MockServer::start().await;
        Mock::given(method("QUERY"))
            .and(path("/start"))
            .respond_with(ResponseTemplate::new(status).insert_header("Location", "/final"))
            .mount(&server)
            .await;
        Mock::given(method("QUERY"))
            .and(path("/final"))
            .and(body_string(r#"{"q":1}"#))
            .and(header("content-type", "application/json"))
            .respond_with(ResponseTemplate::new(200).set_body_string("found"))
            .mount(&server)
            .await;
        let mut r = req("QUERY", &format!("{}/start", server.uri()));
        r.body = Body::json(r#"{"q":1}"#);
        let out = send(&r).await;
        let resp = out
            .response
            .unwrap_or_else(|| panic!("{status}: {:?}", out.error));
        assert_eq!(resp.status, 200, "status {status}");
        assert_eq!(resp.body, b"found");
        assert_eq!(resp.final_method, "QUERY");
        assert_eq!(resp.redirects.len(), 1);
        assert_eq!(resp.redirects[0].status, status);
    }
}

#[tokio::test]
async fn ac_qry_4_303_follows_with_get_and_no_body() {
    let server = MockServer::start().await;
    Mock::given(method("QUERY"))
        .and(path("/start"))
        .respond_with(ResponseTemplate::new(303).insert_header("Location", "/see"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/see"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;
    let mut r = req("QUERY", &format!("{}/start", server.uri()));
    r.body = Body::json("{}");
    let resp = send(&r).await.response.unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.final_method, "GET");
    let received = server.received_requests().await.unwrap();
    let last = received.last().unwrap();
    assert_eq!(last.method.as_str(), "GET");
    assert!(last.body.is_empty());
    assert!(!last.headers.contains_key("content-type"));
}

#[tokio::test]
async fn ac_qry_4_post_302_becomes_get_and_max_10_redirects() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/form"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/done"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/done"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(path("/loop"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "/loop"))
        .mount(&server)
        .await;
    let mut r = req("POST", &format!("{}/form", server.uri()));
    r.body = Body::text_of(BodyType::Text, "x");
    assert_eq!(send(&r).await.response.unwrap().final_method, "GET");

    let out = send(&req("QUERY", &format!("{}/loop", server.uri()))).await;
    let err = out.error.unwrap();
    assert_eq!(err.kind, ErrorKind::Other);
    assert!(
        err.message.contains("Too many redirects"),
        "{}",
        err.message
    );
    // 1 initial + 10 follows
    assert_eq!(
        server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/loop")
            .count(),
        11
    );

    // followRedirects = false returns the 3xx itself
    let mut r = req("GET", &format!("{}/loop", server.uri()));
    r.settings.follow_redirects = false;
    assert_eq!(send(&r).await.response.unwrap().status, 302);
}

#[tokio::test]
async fn redirect_across_origins_drops_authorization() {
    let a = MockServer::start().await;
    let b = MockServer::start().await;
    Mock::given(path("/x"))
        .respond_with(
            ResponseTemplate::new(307).insert_header("Location", format!("{}/y", b.uri()).as_str()),
        )
        .mount(&a)
        .await;
    Mock::given(path("/y"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&b)
        .await;
    let mut r = req("GET", &format!("{}/x", a.uri()));
    r.auth = Auth::bearer("t");
    send(&r).await.response.unwrap();
    let got = b.received_requests().await.unwrap();
    assert!(!got[0].headers.contains_key("authorization"));
    assert!(
        a.received_requests().await.unwrap()[0]
            .headers
            .contains_key("authorization")
    );
}

#[tokio::test]
async fn ac_req_8_cancel_within_100ms_and_recorded() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;
    let engine = HttpEngine::new().unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let canceled_at = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
        Instant::now()
    });
    let r = req("GET", &format!("{}/slow", server.uri()));
    let out = send_request(&engine, &r, &VarScope::empty(), None, &cancel)
        .await
        .unwrap();
    let done = Instant::now();
    let at = canceled_at.await.unwrap();
    assert!(
        done.duration_since(at) < Duration::from_millis(100),
        "{:?}",
        done.duration_since(at)
    );
    assert_eq!(out.error.as_ref().unwrap().kind, ErrorKind::Cancelled);
    assert_eq!(out.history.error.unwrap().kind, ErrorKind::Cancelled);
    assert!(out.history.response.is_none());
}

#[tokio::test]
async fn ac_req_9_typed_errors_timeout_connect_dns() {
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;
    let mut r = req("GET", &format!("{}/slow", server.uri()));
    r.settings.timeout_ms = 200;
    let start = Instant::now();
    let out = send(&r).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Timeout);
    assert!(start.elapsed() < Duration::from_secs(2));

    // connection refused: bind then drop a listener to get a closed port
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let out = send(&req("GET", &format!("http://127.0.0.1:{port}/"))).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Connect);

    let out = send(&req("GET", "http://no-such-host.invalid/")).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Dns);

    // invalid URL is a typed error too, never a panic
    let out = send(&req("GET", "http://exa mple.com/")).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Other);
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
    tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(cfg))
}

#[tokio::test]
async fn ac_req_9_self_signed_tls_error_and_verify_tls_false() {
    let acceptor = self_signed_acceptor();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                break;
            };
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let mut buf = vec![0u8; 4096];
                    let _ = tls.read(&mut buf).await;
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nsecure")
                        .await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    let url = format!("https://localhost:{port}/");
    let out = send(&req("GET", &url)).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Tls);
    let mut r = req("GET", &url);
    r.settings.verify_tls = false;
    let resp = send(&r).await.response.unwrap();
    assert_eq!(resp.body, b"secure");
}

#[tokio::test]
async fn ac_res_4_gzip_decoded_with_wire_size_and_headers_kept() {
    let server = MockServer::start().await;
    let body = "{\"hello\":\"world\"}".repeat(50);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(body.as_bytes()).unwrap();
    let gz = gz.finish().unwrap();
    Mock::given(path("/gz"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Encoding", "gzip")
                .insert_header("Content-Type", "application/json")
                .insert_header("Accept-Query", "application/json")
                .insert_header("Set-Cookie", "sid=1; HttpOnly; Path=/")
                .set_body_bytes(gz.clone()),
        )
        .mount(&server)
        .await;
    let out = send(&req("GET", &format!("{}/gz", server.uri()))).await;
    let resp = out.response.unwrap();
    assert_eq!(resp.body, body.as_bytes());
    assert_eq!(resp.wire_size, gz.len() as u64);
    let meta = ResponseMeta::from_response(&resp);
    assert_eq!(meta.size_bytes, body.len() as u64);
    assert_eq!(meta.wire_size_bytes, Some(gz.len() as u64));
    assert_eq!(meta.content_encoding.as_deref(), Some("gzip"));
    assert_eq!(meta.body_kind, BodyKind::Json);
    // AC-QRY-7 / AC-RES-5
    assert_eq!(meta.accept_query.as_deref(), Some("application/json"));
    assert_eq!(meta.cookies[0].name, "sid");
    assert!(meta.cookies[0].http_only);
    assert!(meta.headers.iter().any(|(k, _)| k == "content-encoding"));
    // AC-RES-1: status text
    assert_eq!(meta.status_text, "OK");
    // default Accept-Encoding advertises all three codings
    let got = server.received_requests().await.unwrap();
    assert_eq!(
        got[0].headers.get("accept-encoding").unwrap(),
        "gzip, deflate, br"
    );
}

/// One-shot raw TCP server returning the bytes of the first request.
async fn raw_server() -> (u16, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).await.unwrap();
            buf.extend_from_slice(&chunk[..n]);
            if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        sock.write_all(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let _ = tx.send(String::from_utf8_lossy(&buf).into_owned());
    });
    (port, rx)
}

#[tokio::test]
async fn ac_req_3_duplicate_headers_sent_separately_and_disabled_omitted() {
    let (port, rx) = raw_server().await;
    let mut r = req("GET", &format!("localhost:{port}/p"));
    r.params = vec![
        KeyValue::new("a", "1"),
        KeyValue::new("off", "1").disabled(),
    ];
    r.headers = vec![
        KeyValue::new("X-Dup", "one"),
        KeyValue::new("X-Dup", "two"),
        KeyValue::new("X-Disabled", "no").disabled(),
        KeyValue::new("X-Custom-Header", "v"),
    ];
    // AC-REQ-10: scheme-less localhost goes over http
    let out = send(&r).await;
    assert_eq!(out.response.unwrap().status, 204);
    let raw = rx.await.unwrap();
    assert!(raw.starts_with("GET /p?a=1 HTTP/1.1\r\n"), "{raw}");
    assert!(raw.contains("X-Dup: one\r\n"), "{raw}");
    assert!(raw.contains("X-Dup: two\r\n"), "{raw}");
    assert!(raw.contains("X-Custom-Header: v\r\n"), "{raw}");
    assert!(!raw.contains("X-Disabled"));
}

#[tokio::test]
async fn ac_qry_3_query_without_body_sends_no_body() {
    let (port, rx) = raw_server().await;
    send(&req("QUERY", &format!("http://127.0.0.1:{port}/q"))).await;
    let raw = rx.await.unwrap();
    assert!(raw.starts_with("QUERY /q HTTP/1.1"), "{raw}");
    assert!(!raw.to_ascii_lowercase().contains("content-type"));
}

#[tokio::test]
async fn ac_hist_1_send_produces_history_with_masked_secrets() {
    let server = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(404).set_body_string("nope"))
        .mount(&server)
        .await;
    let env = Environment {
        id: "g".into(),
        workspace_id: "w".into(),
        project_id: None,
        name: "Global".into(),
        variables: vec![
            Variable::new("base", server.uri()),
            Variable::new("key", "SECRET").secret(),
        ],
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
    };
    let scope = VarScope::new(None, Some(&env));
    let mut r = req("GET", "{{base}}/a?k={{key}}");
    r.params = reqharbor_core::urlx::url_to_params(&r.url, &[]);
    let engine = HttpEngine::new().unwrap();
    let out = send_request(&engine, &r, &scope, None, &CancellationToken::new())
        .await
        .unwrap();
    let h = out.history;
    assert_eq!(h.response.as_ref().unwrap().status, 404);
    assert_eq!(h.response.as_ref().unwrap().body, "nope");
    assert_eq!(h.resolved_url, format!("{}/a?k={{{{key}}}}", server.uri()));
    let line = serde_json::to_string(&h).unwrap();
    assert!(
        !line.contains("SECRET"),
        "AC-ENV-7: secrets never written to history"
    );
    assert_eq!(
        server.received_requests().await.unwrap()[0].url.query(),
        Some("k=SECRET")
    );
}

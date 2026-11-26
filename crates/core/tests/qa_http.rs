//! QA round 1 — adversarial HTTP engine tests against local mock servers
//! (AC-QRY-2/4, AC-REQ-8/9/10, AC-RES-4/5).
//!
//! Tests whose name starts with `bug_` are EXPECTED TO FAIL until the
//! referenced QA-R1-xxx bug is fixed — they are the repro.
//!
//! Run: docker compose run --rm dev cargo test -p reqharbor-core --test qa_http

use reqharbor_core::CancellationToken;
use reqharbor_core::http::{HttpEngine, MAX_BODY_BYTES, MAX_REDIRECTS};
use reqharbor_core::model::*;
use reqharbor_core::send::{ResponseMeta, SendOutcome, send_request};
use reqharbor_core::vars::VarScope;
use std::io::Write as _;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn req(m: &str, url: &str) -> Request {
    Request {
        method: m.into(),
        url: url.into(),
        ..Request::default()
    }
}

async fn send_with(r: &Request, cancel: &CancellationToken) -> SendOutcome {
    let engine = HttpEngine::new().unwrap();
    send_request(&engine, r, &VarScope::empty(), None, cancel)
        .await
        .unwrap()
}

async fn send(r: &Request) -> SendOutcome {
    send_with(r, &CancellationToken::new()).await
}

/// `http://localhost:<port>` — a different *host* than wiremock's 127.0.0.1.
fn as_localhost(uri: &str) -> String {
    uri.replace("127.0.0.1", "localhost")
}

// ------------------------------------------------------------- redirects

/// Every redirect code, cross-origin: Authorization must be dropped
/// (API_CONTRACT §3.5), for the header both typed by the user and generated
/// by the Auth tab.
#[tokio::test]
async fn cross_origin_redirect_drops_authorization_for_every_code() {
    for status in [301u16, 302, 303, 307, 308] {
        for via_auth_tab in [false, true] {
            let a = MockServer::start().await;
            let b = MockServer::start().await;
            Mock::given(path("/start"))
                .respond_with(
                    ResponseTemplate::new(status)
                        .insert_header("Location", format!("{}/final", b.uri())),
                )
                .mount(&a)
                .await;
            Mock::given(path("/final"))
                .respond_with(ResponseTemplate::new(200))
                .mount(&b)
                .await;
            let mut r = req("QUERY", &format!("{}/start", a.uri()));
            r.body = Body::json("{}");
            if via_auth_tab {
                r.auth = Auth::bearer("s3cret");
            } else {
                r.headers = vec![KeyValue::new("Authorization", "Bearer s3cret")];
            }
            let resp = send(&r).await.response.expect("response");
            assert_eq!(resp.status, 200);
            let got = b.received_requests().await.unwrap();
            assert_eq!(got.len(), 1);
            assert!(
                !got[0].headers.contains_key("authorization"),
                "{status}: Authorization leaked to other origin"
            );
            // first hop did carry it
            assert!(
                a.received_requests().await.unwrap()[0]
                    .headers
                    .contains_key("authorization")
            );
        }
    }
}

#[tokio::test]
async fn same_origin_redirect_keeps_authorization() {
    let a = MockServer::start().await;
    Mock::given(path("/start"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", "/final"))
        .mount(&a)
        .await;
    Mock::given(path("/final"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&a)
        .await;
    let mut r = req("GET", &format!("{}/start", a.uri()));
    r.auth = Auth::basic("u", "p");
    send(&r).await.response.unwrap();
    let got = a.received_requests().await.unwrap();
    assert!(got[1].headers.contains_key("authorization"));
}

/// A user-typed `Cookie` (and `Proxy-Authorization`) header is forwarded to a
/// different host on redirect. curl (>= 7.83), browsers and reqwest's own
/// redirect policy all strip these on cross-host redirects. Session cookies
/// copied from DevTools leak to whatever host the server redirects to.
#[tokio::test]
async fn bug_qa_r1_003_cookie_header_leaks_on_cross_host_redirect() {
    let a = MockServer::start().await;
    let b = MockServer::start().await;
    Mock::given(path("/login"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/evil", as_localhost(&b.uri()))),
        )
        .mount(&a)
        .await;
    Mock::given(path("/evil"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&b)
        .await;
    let mut r = req("GET", &format!("{}/login", a.uri()));
    r.headers = vec![
        KeyValue::new("Cookie", "session=SECRET"),
        KeyValue::new("Proxy-Authorization", "Basic cHJveHk6cHc="),
    ];
    let out = send(&r).await;
    assert!(out.response.is_some(), "{:?}", out.error);
    let got = b.received_requests().await.unwrap();
    assert_eq!(got.len(), 1);
    let leaked: Vec<&str> = ["cookie", "proxy-authorization"]
        .into_iter()
        .filter(|h| got[0].headers.contains_key(*h))
        .collect();
    assert!(
        leaked.is_empty(),
        "credentials forwarded to a different host: {leaked:?}"
    );
}

/// Cookies set by a redirect response must not be sent to another host.
#[tokio::test]
async fn redirect_set_cookie_not_carried_to_other_host() {
    let a = MockServer::start().await;
    let b = MockServer::start().await;
    Mock::given(path("/a"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Set-Cookie", "sid=abc; Path=/")
                .insert_header("Location", format!("{}/b", as_localhost(&b.uri()))),
        )
        .mount(&a)
        .await;
    Mock::given(path("/b"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&b)
        .await;
    let out = send(&req("GET", &format!("{}/a", a.uri()))).await;
    assert!(out.response.is_some(), "{:?}", out.error);
    let got = b.received_requests().await.unwrap();
    assert!(!got[0].headers.contains_key("cookie"));
}

#[tokio::test]
async fn redirect_chain_of_exactly_max_succeeds_and_loop_fails() {
    let s = MockServer::start().await;
    for i in 0..MAX_REDIRECTS {
        Mock::given(path(format!("/r{i}")))
            .respond_with(
                ResponseTemplate::new(308).insert_header("Location", format!("r{}", i + 1)),
            )
            .mount(&s)
            .await;
    }
    Mock::given(path(format!("/r{MAX_REDIRECTS}")))
        .respond_with(ResponseTemplate::new(200).set_body_string("end"))
        .mount(&s)
        .await;
    let resp = send(&req("QUERY", &format!("{}/r0", s.uri())))
        .await
        .response
        .unwrap();
    assert_eq!(resp.status, 200);
    assert_eq!(resp.redirects.len(), MAX_REDIRECTS);

    // two-step loop a -> b -> a ...
    Mock::given(path("/la"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", "/lb"))
        .mount(&s)
        .await;
    Mock::given(path("/lb"))
        .respond_with(ResponseTemplate::new(301).insert_header("Location", "/la"))
        .mount(&s)
        .await;
    let out = send(&req("GET", &format!("{}/la", s.uri()))).await;
    assert!(out.error.unwrap().message.contains("Too many redirects"));
}

#[tokio::test]
async fn redirect_edge_cases_do_not_panic() {
    let s = MockServer::start().await;
    // non-http scheme
    Mock::given(path("/file"))
        .respond_with(ResponseTemplate::new(302).insert_header("Location", "file:///etc/passwd"))
        .mount(&s)
        .await;
    // missing Location: returned as the final response
    Mock::given(path("/noloc"))
        .respond_with(ResponseTemplate::new(302))
        .mount(&s)
        .await;
    // 300 / 304 are not redirects to follow
    Mock::given(path("/multi"))
        .respond_with(ResponseTemplate::new(300).insert_header("Location", "/x"))
        .mount(&s)
        .await;
    // relative + dot segments + unicode in Location
    Mock::given(path("/deep/a/b"))
        .respond_with(ResponseTemplate::new(307).insert_header("Location", "../c?q=%C3%BC"))
        .mount(&s)
        .await;
    Mock::given(path("/deep/c"))
        .respond_with(ResponseTemplate::new(200).set_body_string("c"))
        .mount(&s)
        .await;

    let out = send(&req("GET", &format!("{}/file", s.uri()))).await;
    let err = out
        .error
        .expect("file:// redirect must not be followed silently");
    assert_eq!(err.kind, ErrorKind::Other);
    assert!(out.response.is_none());

    let resp = send(&req("GET", &format!("{}/noloc", s.uri())))
        .await
        .response
        .unwrap();
    assert_eq!(resp.status, 302);
    let resp = send(&req("GET", &format!("{}/multi", s.uri())))
        .await
        .response
        .unwrap();
    assert_eq!(resp.status, 300);
    let resp = send(&req("GET", &format!("{}/deep/a/b", s.uri())))
        .await
        .response
        .unwrap();
    assert_eq!(resp.status, 200);
    assert!(resp.final_url.contains("/deep/c?q="), "{}", resp.final_url);

    let mut r = req("GET", &format!("{}/deep/a/b", s.uri()));
    r.settings.follow_redirects = false;
    assert_eq!(send(&r).await.response.unwrap().status, 307);
}

/// AC-QRY-4 with 301/302 for PUT/PATCH/DELETE: only POST is rewritten.
#[tokio::test]
async fn redirect_302_keeps_put_patch_delete_and_body() {
    for m in ["PUT", "PATCH", "DELETE", "QUERY"] {
        let s = MockServer::start().await;
        Mock::given(path("/a"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", "/b"))
            .mount(&s)
            .await;
        Mock::given(path("/b"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        let mut r = req(m, &format!("{}/a", s.uri()));
        r.body = Body::text_of(BodyType::Text, "payload");
        send(&r).await.response.unwrap();
        let got = s.received_requests().await.unwrap();
        assert_eq!(got[1].method.as_str(), m);
        assert_eq!(got[1].body, b"payload");
    }
}

// -------------------------------------------------- QUERY + body types

/// AC-QRY-2: QUERY sends every body type exactly like POST.
#[tokio::test]
async fn query_sends_every_body_type_like_post() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("blob.bin");
    std::fs::write(&file, [0u8, 1, 2, 255]).unwrap();
    let bodies: Vec<(Body, &str)> = vec![
        (Body::json(r#"{"a":1}"#), "application/json"),
        (Body::text_of(BodyType::Xml, "<a/>"), "application/xml"),
        (Body::text_of(BodyType::Text, "hi"), "text/plain"),
        (
            Body::form_encoded(vec![KeyValue::new("a b", "c&d")]),
            "application/x-www-form-urlencoded",
        ),
        (Body::graphql("{ me }", ""), "application/json"),
        (
            Body::form(vec![
                FormField::text("t", "v"),
                FormField::file("f", file.to_string_lossy()),
            ]),
            "multipart/form-data; boundary=",
        ),
        (
            Body::binary(file.to_string_lossy()),
            "application/octet-stream",
        ),
    ];
    for (body, ct) in bodies {
        let mut bodies_seen = Vec::new();
        for m in ["QUERY", "POST"] {
            let s = MockServer::start().await;
            Mock::given(method(m))
                .respond_with(ResponseTemplate::new(200))
                .mount(&s)
                .await;
            let mut r = req(m, &format!("{}/q", s.uri()));
            r.body = body.clone();
            let out = send(&r).await;
            assert!(
                out.response.is_some(),
                "{m} {:?}: {:?}",
                body.body_type,
                out.error
            );
            let got = s.received_requests().await.unwrap();
            let got_ct = got[0]
                .headers
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .to_string();
            assert!(got_ct.starts_with(ct), "{m} {:?}: {got_ct}", body.body_type);
            let mut b = got[0].body.clone();
            if ct.starts_with("multipart") {
                // boundaries differ per send; compare after normalising
                let boundary = got_ct.split("boundary=").nth(1).unwrap().to_string();
                b = String::from_utf8_lossy(&b)
                    .replace(&boundary, "B")
                    .into_bytes();
            }
            bodies_seen.push(b);
        }
        assert_eq!(bodies_seen[0], bodies_seen[1], "{:?}", body.body_type);
    }
}

/// Header injection via CR/LF in a header value must be rejected, not sent.
#[tokio::test]
async fn header_crlf_injection_is_rejected() {
    let s = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    let mut r = req("GET", &format!("{}/", s.uri()));
    r.headers = vec![KeyValue::new("X-A", "1\r\nX-Injected: yes")];
    let out = send(&r).await;
    assert!(out.error.is_some());
    assert!(s.received_requests().await.unwrap().is_empty());
}

// ---------------------------------------------------- decoding / sizes

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// AC-RES-4 size accounting for gzip, deflate (zlib), chained encodings.
#[tokio::test]
async fn size_accounting_decoded_vs_wire() {
    let text = "ReqHarbor ".repeat(10_000).into_bytes();
    let gz = gzip(&text);
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    z.write_all(&text).unwrap();
    let z = z.finish().unwrap();
    // "gzip, deflate" means deflate applied after gzip: decode deflate first
    let mut chained = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    chained.write_all(&gz).unwrap();
    let chained = chained.finish().unwrap();

    for (enc, wire) in [("gzip", gz), ("deflate", z), ("gzip, deflate", chained)] {
        let s = MockServer::start().await;
        Mock::given(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Encoding", enc)
                    .set_body_raw(wire.clone(), "text/plain"),
            )
            .mount(&s)
            .await;
        let out = send(&req("GET", &format!("{}/", s.uri()))).await;
        let resp = out.response.unwrap();
        assert!(
            resp.decode_error.is_none(),
            "{enc}: {:?}",
            resp.decode_error
        );
        let meta = ResponseMeta::from_response(&resp);
        assert_eq!(meta.size_bytes, text.len() as u64, "{enc}");
        assert_eq!(meta.wire_size_bytes, Some(wire.len() as u64), "{enc}");
        assert_eq!(meta.content_encoding.as_deref(), Some(enc));
        // history stores the decoded size
        assert_eq!(out.history.response.unwrap().size_bytes, text.len() as u64);
    }
}

/// A server that sends `Content-Encoding: gzip` but plain bytes, or an
/// unsupported coding (zstd), must yield the raw body + decode_error, not a
/// failed send.
#[tokio::test]
async fn bad_or_unknown_encoding_keeps_raw_body() {
    for (enc, body) in [
        ("gzip", b"plain text".to_vec()),
        ("zstd", b"\x28\xb5\x2f\xfd".to_vec()),
    ] {
        let s = MockServer::start().await;
        Mock::given(path("/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("Content-Encoding", enc)
                    .set_body_raw(body.clone(), "text/plain"),
            )
            .mount(&s)
            .await;
        let resp = send(&req("GET", &format!("{}/", s.uri())))
            .await
            .response
            .unwrap();
        assert_eq!(resp.body, body);
        assert!(resp.decode_error.is_some());
    }
}

/// HEAD / 204 / 304 responses carry `Content-Encoding: gzip` with an empty
/// body (that is normal: the header describes the GET representation). The
/// engine must not report a decode error for an empty body.
#[tokio::test]
async fn bug_qa_r1_017_empty_gzip_body_reports_decode_error() {
    let s = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(204).insert_header("Content-Encoding", "gzip"))
        .mount(&s)
        .await;
    let resp = send(&req("GET", &format!("{}/", s.uri())))
        .await
        .response
        .unwrap();
    assert_eq!(resp.status, 204);
    assert!(
        resp.decode_error.is_none(),
        "spurious decode error on empty body: {:?}",
        resp.decode_error
    );
}

/// Decompression bomb: ~300 KB on the wire (300 concatenated gzip members of
/// 1 MiB of zeros) expands to 300 MiB, past MAX_BODY_BYTES (256 MiB). The cap
/// is only enforced on *wire* bytes; the decoded size is unbounded, so a
/// hostile server can make the app allocate gigabytes and crash (release
/// builds use panic = "abort", OOM kills the process).
#[tokio::test]
async fn bug_qa_r1_002_gzip_bomb_decoded_size_is_unbounded() {
    let member = gzip(&vec![0u8; 1024 * 1024]);
    let mut bomb = Vec::with_capacity(member.len() * 300);
    for _ in 0..300 {
        bomb.extend_from_slice(&member);
    }
    let wire_len = bomb.len();
    let s = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Content-Encoding", "gzip")
                .set_body_raw(bomb, "application/octet-stream"),
        )
        .mount(&s)
        .await;
    let out = send(&req("GET", &format!("{}/", s.uri()))).await;
    if let Some(resp) = out.response {
        assert!(
            resp.body.len() <= MAX_BODY_BYTES,
            "{wire_len} wire bytes decoded to {} bytes (> MAX_BODY_BYTES {MAX_BODY_BYTES})",
            resp.body.len()
        );
    }
}

/// AC-RES-5: "Headers tab lists headers in received order". The engine
/// iterates reqwest's HeaderMap, which groups repeated names together, so
/// interleaved Set-Cookie headers are reported out of wire order.
#[tokio::test]
#[ignore = "won't fix: reqwest/hyper HeaderMap loses interleaved response header order (accepted P3)"]
async fn bug_qa_r1_019_response_headers_reordered_duplicates_grouped() {
    let (port, _rx) = raw_server(
        "HTTP/1.1 200 OK\r\nZ-First: 1\r\nSet-Cookie: a=1; Path=/; HttpOnly\r\nA-Second: 2\r\nSet-Cookie: b=\"q\"; Domain=.x.dev; Secure; SameSite=Lax; Max-Age=60\r\nContent-Length: 0\r\n\r\n",
        Duration::ZERO,
    )
    .await;
    let resp = send(&req("GET", &format!("http://127.0.0.1:{port}/")))
        .await
        .response
        .unwrap();
    let names: Vec<&str> = resp.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "z-first",
            "set-cookie",
            "a-second",
            "set-cookie",
            "content-length"
        ]
    );
    let meta = ResponseMeta::from_response(&resp);
    assert_eq!(meta.cookies.len(), 2);
    assert_eq!(meta.cookies[1].value, "q");
    assert_eq!(meta.cookies[1].domain.as_deref(), Some("x.dev"));
    assert!(meta.cookies[1].secure);
    assert_eq!(meta.cookies[1].same_site.as_deref(), Some("Lax"));
}

// ------------------------------------------------- timeouts and cancel

/// Raw TCP server: reads the request head, waits `delay`, then writes
/// `response` byte by byte slowly when `delay` is non-zero.
async fn raw_server(
    response: &'static str,
    delay: Duration,
) -> (u16, tokio::sync::oneshot::Receiver<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 65536];
        let n = sock.read(&mut buf).await.unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&buf[..n]).into_owned());
        tokio::time::sleep(delay).await;
        let _ = sock.write_all(response.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    (port, rx)
}

/// Body trickles slowly after fast headers: the timeout covers the whole
/// exchange (API_CONTRACT §3.5).
#[tokio::test]
async fn timeout_covers_slow_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n")
            .await;
        for _ in 0..1000 {
            if sock.write_all(b"x").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let mut r = req("GET", &format!("http://127.0.0.1:{port}/"));
    r.settings.timeout_ms = 400;
    let t = Instant::now();
    let out = send(&r).await;
    let err = out.error.expect("must time out");
    assert_eq!(err.kind, ErrorKind::Timeout);
    assert!(
        t.elapsed() < Duration::from_millis(1500),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(out.history.error.unwrap().kind, ErrorKind::Timeout);
}

/// AC-REQ-8: cancel while the body is streaming aborts within 100 ms.
#[tokio::test]
async fn cancel_during_body_stream_within_100ms() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let _ = sock.read(&mut buf).await;
        let _ = sock
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100000\r\n\r\n")
            .await;
        loop {
            if sock.write_all(b"x").await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    let r = req("GET", &format!("http://127.0.0.1:{port}/"));
    let handle = tokio::spawn(async move { send_with(&r, &c2).await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t = Instant::now();
    cancel.cancel();
    let out = handle.await.unwrap();
    assert!(
        t.elapsed() < Duration::from_millis(100),
        "{:?}",
        t.elapsed()
    );
    assert_eq!(out.error.unwrap().kind, ErrorKind::Cancelled);
    assert_eq!(out.history.error.unwrap().kind, ErrorKind::Cancelled);
}

/// Cancelling a token that was already cancelled before send starts must not
/// hit the network at all.
#[tokio::test]
async fn pre_cancelled_send_is_cancelled() {
    let s = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&s)
        .await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    let out = send_with(&req("GET", &format!("{}/", s.uri())), &cancel).await;
    assert_eq!(out.error.unwrap().kind, ErrorKind::Cancelled);
}

/// `timeoutMs: 0` (e.g. from `curl -m 0`, which curl documents as "no
/// limit") is turned into a 1 ms timeout, so every request fails with
/// "Timed out after 0 ms".
#[tokio::test]
async fn bug_qa_r1_018_timeout_zero_times_out_immediately() {
    let s = MockServer::start().await;
    Mock::given(path("/"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(30)))
        .mount(&s)
        .await;
    let mut r = req("GET", &format!("{}/", s.uri()));
    r.settings.timeout_ms = 0;
    let out = send(&r).await;
    assert!(out.response.is_some(), "timeoutMs 0 => {:?}", out.error);
}

// ------------------------------------------------------ default scheme

#[test]
fn ac_req_10_default_scheme_matrix() {
    use reqharbor_core::urlx::apply_default_scheme as d;
    assert_eq!(d("localhost:8080/x"), "http://localhost:8080/x");
    assert_eq!(d("LOCALHOST/x"), "http://LOCALHOST/x");
    assert_eq!(d("127.0.0.1"), "http://127.0.0.1");
    assert_eq!(d("[::1]:3000/a"), "http://[::1]:3000/a");
    assert_eq!(
        d("localhost.example.com/a"),
        "https://localhost.example.com/a"
    );
    assert_eq!(
        d("api.example.com/a?b=http://c"),
        "https://api.example.com/a?b=http://c"
    );
    assert_eq!(d("HTTPS://x.dev"), "HTTPS://x.dev");
    assert_eq!(d("//cdn.x.dev/a"), "https://cdn.x.dev/a");
}

/// A Location header with raw UTF-8 bytes (seen in the wild, e.g. IIS/nginx
/// redirects to non-ASCII paths) is not followed: `to_str()` fails and the
/// 3xx is returned as the final response without any hint. curl and browsers
/// follow it (percent-encoding the bytes).
#[tokio::test]
async fn bug_qa_r1_020_utf8_location_redirect_not_followed() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let head = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp: Vec<u8> = if head.starts_with("GET /start") {
                    let mut r = b"HTTP/1.1 302 Found\r\nLocation: /caf\xc3\xa9\r\nContent-Length: 0\r\n\r\n".to_vec();
                    r.shrink_to_fit();
                    r
                } else {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_vec()
                };
                let _ = sock.write_all(&resp).await;
            });
        }
    });
    let resp = send(&req("GET", &format!("http://127.0.0.1:{port}/start")))
        .await
        .response
        .unwrap();
    assert_eq!(
        resp.status, 200,
        "redirect to UTF-8 Location was not followed"
    );
    assert!(resp.final_url.ends_with("/caf%C3%A9"), "{}", resp.final_url);
}

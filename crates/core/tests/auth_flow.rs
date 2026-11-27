//! Desktop sign-in against a wiremock server and a fake browser that talks
//! to the core's real loopback listener (04 §12 M1c, 06-go-server §9).

use reqharbor_core::CancellationToken;
use reqharbor_core::auth::{
    self, AccountFile, AuthClient, AuthError, LoopbackConfig, PendingSignIn, ServerIdentity,
    Session, Tokens,
};
use reqharbor_core::model::now_ms;
use reqharbor_core::secrets::{MemorySecretStore, SecretStore};
use reqharbor_core::server_info::{self, DiscoveryStatus};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate};

fn form(req: &WmRequest) -> HashMap<String, String> {
    url::form_urlencoded::parse(&req.body)
        .into_owned()
        .collect()
}

fn token_json(access: &str, refresh: &str) -> Value {
    json!({
        "access_token": access, "refresh_token": refresh, "token_type": "Bearer",
        "expires_in": 3600, "session_id": "sess1",
        "user": {"id": "u1", "email": "priya@acme.dev", "name": "Priya Shah", "avatarUrl": null, "isAdmin": false}
    })
}

fn me_json() -> Value {
    json!({"user": {"id": "u1", "email": "priya@acme.dev", "name": "Priya Shah", "avatarUrl": null, "isAdmin": true},
           "method": "github", "sessionId": "sess1"})
}

/// A minimal raw HTTP GET; returns (status, head).
async fn raw_get(port: u16, target: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(format!("GET {target} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").as_bytes())
        .await
        .unwrap();
    let mut out = Vec::new();
    s.read_to_end(&mut out).await.unwrap();
    let text = String::from_utf8_lossy(&out).into_owned();
    let status = text
        .split(' ')
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    (status, text)
}

fn query(url: &str) -> HashMap<String, String> {
    url::Url::parse(url)
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn fast() -> LoopbackConfig {
    LoopbackConfig {
        max_conns: 8,
        read_timeout: Duration::from_millis(300),
        timeout: Duration::from_secs(20),
    }
}

/// Records the token-endpoint form and checks the secret store already
/// holds the session when `/v1/me` is first called (persist before use).
struct Server {
    forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
    secrets: Arc<MemorySecretStore>,
    me_saw_tokens: Arc<Mutex<Option<bool>>>,
    origin: Arc<Mutex<String>>,
}

impl Respond for Server {
    fn respond(&self, req: &WmRequest) -> ResponseTemplate {
        match req.url.path() {
            "/v1/auth/token" => {
                let f = form(req);
                self.forms.lock().unwrap().push(f.clone());
                let ok = f.get("grant_type").map(String::as_str) == Some("authorization_code")
                    && f.get("client_id").map(String::as_str) == Some("reqharbor-desktop")
                    && f.get("code").map(String::as_str) == Some("CODE123")
                    && f.get("code_verifier").is_some_and(|v| v.len() == 43)
                    && f.get("redirect_uri")
                        .is_some_and(|r| r.starts_with("http://127.0.0.1:") && r.ends_with("/cb"));
                let ct = req
                    .headers
                    .get("content-type")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("");
                let version = req.headers.get("x-reqharbor-version").is_some();
                if ok && ct == "application/x-www-form-urlencoded" && version {
                    ResponseTemplate::new(200).set_body_json(token_json("rha_A1", "rhr_R1"))
                } else {
                    ResponseTemplate::new(400).set_body_json(
                        json!({"error": {"code": "invalid_grant", "message": "bad"}}),
                    )
                }
            }
            "/v1/me" => {
                let key = format!("session:{}:u1", self.origin.lock().unwrap());
                let has = self.secrets.keys().contains(&key);
                self.me_saw_tokens.lock().unwrap().get_or_insert(has);
                let auth = req
                    .headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok());
                if auth == Some("Bearer rha_A1") {
                    ResponseTemplate::new(200).set_body_json(me_json())
                } else {
                    ResponseTemplate::new(401)
                        .set_body_json(json!({"error": {"code": "auth_required", "message": "no"}}))
                }
            }
            _ => ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "not_found", "message": "Not found"}})),
        }
    }
}

#[tokio::test]
async fn full_sign_in_with_a_fake_browser() {
    let server = MockServer::start().await;
    let secrets = Arc::new(MemorySecretStore::new());
    let forms = Arc::new(Mutex::new(Vec::new()));
    let me_saw_tokens = Arc::new(Mutex::new(None));
    let origin = server.uri();
    Mock::given(wiremock::matchers::any())
        .respond_with(Server {
            forms: forms.clone(),
            secrets: secrets.clone(),
            me_saw_tokens: me_saw_tokens.clone(),
            origin: Arc::new(Mutex::new(origin.clone())),
        })
        .mount(&server)
        .await;
    let pending = PendingSignIn::start(&origin, Some("github"), "Test Mac (macOS)")
        .await
        .unwrap();
    let q = query(&pending.authorize_url);
    assert!(
        pending
            .authorize_url
            .starts_with(&format!("{origin}/auth/authorize?"))
    );
    assert_eq!(q["code_challenge_method"], "S256");
    assert_eq!(q["method"], "github");
    let state = q["state"].clone();
    let port = pending.port();
    assert!(port >= 1024);

    let cancel = CancellationToken::new();
    let waiter = tokio::spawn(pending.wait(fast(), cancel.clone()));
    // The "browser": stray requests and a wrong state never finish it.
    let (s, _) = raw_get(port, "/favicon.ico").await;
    assert_eq!(s, 404);
    let (s, _) = raw_get(port, "/cb?code=EVIL&state=wrong").await;
    assert_eq!(s, 400);
    let (s, _) = raw_get(port, "/cb?code=EVIL").await;
    assert_eq!(s, 400);
    assert!(!waiter.is_finished());
    // The real redirect.
    let (s, head) = raw_get(port, &format!("/cb?code=CODE123&state={state}")).await;
    assert_eq!(s, 302);
    assert!(
        head.contains(&format!("Location: {origin}/auth/done")),
        "{head}"
    );
    let received = waiter.await.unwrap().unwrap();
    // The listener has stopped.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());

    let client = AuthClient::new(&origin).unwrap();
    let resp = received.exchange(&client).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (account, session) = auth::complete_sign_in(
        dir.path(),
        secrets.clone(),
        &client,
        &ServerIdentity {
            instance_id: "INST".into(),
            name: "Acme".into(),
        },
        None,
        Some("github"),
        resp,
    )
    .await
    .unwrap();
    assert_eq!(account.user_id, "u1");
    assert_eq!(account.email, "priya@acme.dev");
    assert_eq!(account.method, "github");
    assert_eq!(account.server_url, origin);
    assert_eq!(account.server_instance_id, "INST");
    assert_eq!(account.session_id, "sess1");
    assert_eq!(
        *me_saw_tokens.lock().unwrap(),
        Some(true),
        "tokens persisted before use"
    );
    assert_eq!(secrets.keys(), vec![format!("session:{origin}:u1")]);
    let stored: Tokens = serde_json::from_str(
        &secrets
            .get(&format!("session:{origin}:u1"))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.access_token, "rha_A1");
    assert_eq!(stored.refresh_token, "rhr_R1");
    assert!(stored.access_expires_at > now_ms() + 3_000_000);
    // account.json holds no token.
    let raw = std::fs::read_to_string(dir.path().join(auth::ACCOUNT_FILE)).unwrap();
    assert!(!raw.contains("rha_") && !raw.contains("rhr_"), "{raw}");
    let file = AccountFile::load(dir.path()).unwrap().unwrap();
    assert_eq!(file.account.as_ref(), Some(&account));
    assert_eq!(session.refresh_count(), 0);
    let f = forms.lock().unwrap();
    assert_eq!(f.len(), 1);
    let keys: Vec<&str> = {
        let mut k: Vec<&str> = f[0].keys().map(String::as_str).collect();
        k.sort();
        k
    };
    assert_eq!(
        keys,
        vec![
            "client_id",
            "code",
            "code_verifier",
            "grant_type",
            "redirect_uri"
        ]
    );
}

#[tokio::test]
async fn browser_denial_and_wrong_code_are_reported() {
    let pending = PendingSignIn::start("https://rh.example", None, "X (Linux)")
        .await
        .unwrap();
    let state = query(&pending.authorize_url)["state"].clone();
    let port = pending.port();
    let waiter = tokio::spawn(pending.wait(fast(), CancellationToken::new()));
    let (s, _) = raw_get(port, &format!("/cb?error=access_denied&state={state}")).await;
    assert_eq!(s, 200);
    assert_eq!(
        waiter.await.unwrap().err(),
        Some(AuthError::Denied("access_denied".into()))
    );

    // A burnt or wrong code: 400 invalid_grant.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/token"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"code": "invalid_grant", "message": "x"}})),
        )
        .mount(&server)
        .await;
    let client = AuthClient::new(&server.uri()).unwrap();
    let err = client
        .exchange_code("c", "v", "http://127.0.0.1:1/cb")
        .await
        .unwrap_err();
    assert_eq!(err, AuthError::InvalidGrant);
}

#[tokio::test]
async fn loopback_times_out_and_cancels() {
    let pending = PendingSignIn::start("https://rh.example", None, "X (Linux)")
        .await
        .unwrap();
    let cfg = LoopbackConfig {
        timeout: Duration::from_millis(200),
        ..fast()
    };
    assert_eq!(
        pending.wait(cfg, CancellationToken::new()).await.err(),
        Some(AuthError::Timeout)
    );

    let pending = PendingSignIn::start("https://rh.example", None, "X (Linux)")
        .await
        .unwrap();
    let port = pending.port();
    let cancel = CancellationToken::new();
    let waiter = tokio::spawn(pending.wait(fast(), cancel.clone()));
    tokio::time::sleep(Duration::from_millis(50)).await;
    cancel.cancel();
    assert_eq!(waiter.await.unwrap().err(), Some(AuthError::Cancelled));
    tokio::time::sleep(Duration::from_millis(50)).await;
    // A late browser redirect lands on a closed port.
    assert!(TcpStream::connect(("127.0.0.1", port)).await.is_err());
}

#[tokio::test]
async fn loopback_survives_100_junk_connections_with_at_most_8_concurrent() {
    let pending = PendingSignIn::start("https://rh.example", None, "X (Linux)")
        .await
        .unwrap();
    let state = query(&pending.authorize_url)["state"].clone();
    let port = pending.port();
    let peak = pending.peak_concurrency();
    let waiter = tokio::spawn(pending.wait(fast(), CancellationToken::new()));
    // 100 connections that send nothing (or garbage) and hang around.
    let mut junk = Vec::new();
    for i in 0..100 {
        let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        if i % 3 == 0 {
            let _ = s.write_all(b"\x16\x03\x01garbage").await;
        }
        junk.push(s);
    }
    let real =
        tokio::spawn(async move { raw_get(port, &format!("/cb?code=OK&state={state}")).await });
    let (status, _) = tokio::time::timeout(Duration::from_secs(15), real)
        .await
        .expect("the real callback got through")
        .unwrap();
    assert_eq!(status, 302);
    let code = waiter.await.unwrap().unwrap();
    drop(code);
    let seen = peak.load(Ordering::SeqCst);
    assert!(seen <= 8, "peak concurrency {seen}");
    assert!(
        seen >= 2,
        "the junk was actually handled concurrently ({seen})"
    );
    drop(junk);
}

#[tokio::test]
async fn bind_failure_is_loopback_unavailable() {
    // A real bind error: the port is already taken by another listener.
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port();
    let err = match PendingSignIn::start_on(
        ("127.0.0.1", port),
        "https://rh.acme.dev",
        None,
        "MacBook Pro (macOS)",
    )
    .await
    {
        Ok(_) => panic!("binding a taken port must fail"),
        Err(e) => e,
    };
    assert_eq!(err, AuthError::LoopbackUnavailable("address in use".into()));
    assert!(err.to_string().contains("Sync › Advanced"), "{err}");
    // Only the error kind is kept: no address or port in the payload.
    assert!(!format!("{err:?}").contains(&port.to_string()), "{err:?}");
    // The same mapping for any other io error.
    let io = std::io::Error::from(std::io::ErrorKind::AddrNotAvailable);
    assert!(matches!(
        auth::loopback_error(&io),
        AuthError::LoopbackUnavailable(_)
    ));
    // The default address still binds an ephemeral loopback port.
    let ok = PendingSignIn::start("https://rh.acme.dev", None, "x")
        .await
        .unwrap();
    assert!(ok.redirect_uri.starts_with("http://127.0.0.1:"));
    assert_ne!(ok.port(), 0);
}

// ───────────────────────────── sessions ─────────────────────────────

/// Refresh endpoint: rotates rhr_N -> rhr_{N+1}, counts calls, can be made
/// slow or dead.
#[derive(Clone)]
struct Rotating {
    calls: Arc<AtomicUsize>,
    delay: Duration,
    dead: Arc<Mutex<bool>>,
}

impl Respond for Rotating {
    fn respond(&self, req: &WmRequest) -> ResponseTemplate {
        if req.url.path() == "/v1/auth/token" {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            let f = form(req);
            assert_eq!(f["grant_type"], "refresh_token");
            assert_eq!(f["client_id"], "reqharbor-desktop");
            assert!(f["refresh_token"].starts_with("rhr_"));
            if *self.dead.lock().unwrap() {
                return ResponseTemplate::new(401).set_body_json(
                    json!({"error": {"code": "session_revoked", "message": "Session revoked"}}),
                );
            }
            return ResponseTemplate::new(200)
                .set_delay(self.delay)
                .set_body_json(token_json(&format!("rha_{n}"), &format!("rhr_{n}")));
        }
        // /v1/me accepts only refreshed tokens (rha_1, rha_2, …).
        let auth = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth.starts_with("Bearer rha_") && auth != "Bearer rha_OLD" {
            ResponseTemplate::new(200).set_body_json(me_json())
        } else {
            ResponseTemplate::new(401)
                .insert_header(
                    "WWW-Authenticate",
                    "Bearer realm=\"reqharbor\", error=\"invalid_token\"",
                )
                .set_body_json(json!({"error": {"code": "session_expired", "message": "expired"}}))
        }
    }
}

async fn rotating(delay: Duration) -> (MockServer, Rotating) {
    let server = MockServer::start().await;
    let r = Rotating {
        calls: Arc::new(AtomicUsize::new(0)),
        delay,
        dead: Arc::new(Mutex::new(false)),
    };
    Mock::given(wiremock::matchers::any())
        .respond_with(r.clone())
        .mount(&server)
        .await;
    (server, r)
}

fn session(origin: &str, secrets: Arc<dyn SecretStore>, expires_at: i64) -> Session {
    Session::new(
        AuthClient::new(origin).unwrap(),
        origin,
        "u1",
        secrets,
        Tokens {
            access_token: "rha_OLD".into(),
            access_expires_at: expires_at,
            refresh_token: "rhr_OLD".into(),
            session_id: "sess1".into(),
        },
    )
}

#[tokio::test]
async fn concurrent_callers_share_one_refresh() {
    let (server, r) = rotating(Duration::from_millis(200)).await;
    let secrets = Arc::new(MemorySecretStore::new());
    let s = Arc::new(session(&server.uri(), secrets.clone(), now_ms() - 1));
    let mut tasks = Vec::new();
    for _ in 0..10 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move { s.access_token().await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap(), "rha_1");
    }
    assert_eq!(
        r.calls.load(Ordering::SeqCst),
        1,
        "exactly one HTTP refresh"
    );
    // The rotated pair was stored.
    let stored: Tokens = serde_json::from_str(
        &secrets
            .get(&format!("session:{}:u1", server.uri()))
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(stored.refresh_token, "rhr_1");
}

#[tokio::test]
async fn a_401_refreshes_once_and_retries() {
    let (server, r) = rotating(Duration::ZERO).await;
    let secrets = Arc::new(MemorySecretStore::new());
    // Not expired locally, but the server says it is (revoked clock etc.).
    let s = Arc::new(session(&server.uri(), secrets, now_ms() + 3_600_000));
    // Several concurrent 401s still make one refresh (single-flight on the
    // token that failed).
    let mut tasks = Vec::new();
    for _ in 0..5 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move { s.me().await }));
    }
    for t in tasks {
        assert_eq!(t.await.unwrap().unwrap().method, "github");
    }
    assert_eq!(r.calls.load(Ordering::SeqCst), 1);
    assert!(!s.sign_in_required());
}

#[tokio::test]
async fn a_failed_refresh_is_sign_in_required_with_data_intact() {
    let (server, r) = rotating(Duration::ZERO).await;
    *r.dead.lock().unwrap() = true;
    let secrets = Arc::new(MemorySecretStore::new());
    let origin = server.uri();
    let key = format!("session:{origin}:u1");
    secrets.set(&key, "{\"kept\":true}").unwrap();
    // Local data: a store with a project and an account file.
    let dir = tempfile::tempdir().unwrap();
    let mut store = reqharbor_core::storage::Store::open(dir.path()).unwrap();
    store.create_project("Payments API").unwrap();
    let before = store.project_list();
    let file = AccountFile {
        account: Some(auth::Account {
            server_url: origin.clone(),
            server_instance_id: "I".into(),
            server_name: "Acme".into(),
            user_id: "u1".into(),
            email: "priya@acme.dev".into(),
            name: "Priya".into(),
            method: "github".into(),
            session_id: "sess1".into(),
        }),
        ..Default::default()
    };
    file.save(dir.path()).unwrap();

    let s = session(&origin, secrets.clone(), now_ms() - 1);
    assert_eq!(s.me().await.unwrap_err(), AuthError::SignInRequired);
    assert!(s.sign_in_required());
    // Later calls fail fast, without more refreshes.
    assert_eq!(s.sessions().await.unwrap_err(), AuthError::SignInRequired);
    assert_eq!(r.calls.load(Ordering::SeqCst), 1);
    // Nothing local was touched.
    let reopened = reqharbor_core::storage::Store::open(dir.path()).unwrap();
    assert_eq!(reopened.project_list(), before);
    assert_eq!(
        AccountFile::load(dir.path()).unwrap().unwrap().account,
        file.account
    );
    assert_eq!(
        secrets.get(&key).unwrap().as_deref(),
        Some("{\"kept\":true}")
    );
}

#[tokio::test]
async fn refreshed_tokens_are_persisted_before_use() {
    let (server, r) = rotating(Duration::ZERO).await;
    let secrets = Arc::new(MemorySecretStore::new());
    secrets.set_fail_writes(true);
    let s = session(&server.uri(), secrets.clone(), now_ms() - 1);
    // The refresh succeeds on the server but cannot be stored: the new
    // token is not used.
    assert!(matches!(
        s.access_token().await.unwrap_err(),
        AuthError::Keychain(_)
    ));
    assert_eq!(r.calls.load(Ordering::SeqCst), 1);
    assert!(!s.sign_in_required());
    // Once the store works again, the next refresh is stored, then used.
    secrets.set_fail_writes(false);
    assert_eq!(s.access_token().await.unwrap(), "rha_2");
    let log = secrets.log();
    assert_eq!(
        log,
        vec![
            format!("set-failed:session:{}:u1", server.uri()),
            format!("set:session:{}:u1", server.uri())
        ]
    );
}

#[tokio::test]
async fn sign_in_writes_no_account_when_the_keychain_fails() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(me_json()))
        .mount(&server)
        .await;
    let secrets = Arc::new(MemorySecretStore::new());
    secrets.set_fail_writes(true);
    let dir = tempfile::tempdir().unwrap();
    let client = AuthClient::new(&server.uri()).unwrap();
    let resp: auth::TokenResponse = serde_json::from_value(token_json("rha_A", "rhr_R")).unwrap();
    let err = auth::complete_sign_in(
        dir.path(),
        secrets,
        &client,
        &ServerIdentity::default(),
        None,
        None,
        resp,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, AuthError::Keychain(_)));
    assert!(!dir.path().join(auth::ACCOUNT_FILE).exists());
    // /v1/me was never called with the unsaved token.
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn signing_in_as_another_account_drops_the_old_keychain_entry() {
    let server = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "user": {"id": "u2", "email": "sam@acme.dev", "name": "Sam"}, "method": "password", "sessionId": "s2"
        })))
        .mount(&server)
        .await;
    let secrets = Arc::new(MemorySecretStore::new());
    let dir = tempfile::tempdir().unwrap();
    let old_key = "session:https://old.example:u1".to_string();
    secrets.set(&old_key, "{}").unwrap();
    let file = AccountFile {
        account: Some(auth::Account {
            server_url: "https://old.example".into(),
            server_instance_id: String::new(),
            server_name: String::new(),
            user_id: "u1".into(),
            email: String::new(),
            name: String::new(),
            method: String::new(),
            session_id: String::new(),
        }),
        ..Default::default()
    };
    file.save(dir.path()).unwrap();
    let client = AuthClient::new(&server.uri()).unwrap();
    let mut resp = token_json("rha_B", "rhr_B");
    resp["user"] = json!({"id": "u2", "email": "sam@acme.dev", "name": "Sam"});
    resp["session_id"] = json!("s2");
    let (account, _) = auth::complete_sign_in(
        dir.path(),
        secrets.clone(),
        &client,
        &ServerIdentity::default(),
        None,
        Some("password"),
        serde_json::from_value(resp).unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(account.user_id, "u2");
    assert_eq!(account.method, "password");
    assert_eq!(secrets.keys(), vec![format!("session:{}:u2", server.uri())]);
}

#[tokio::test]
async fn logout_and_devices() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/auth/logout"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/me/sessions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"sessions": [
            {"id": "sess1", "deviceName": "Mac (macOS)", "createdAt": 1764000000000i64, "lastUsedDay": 20419, "current": true},
            {"id": "sess2", "deviceName": "ubuntu-desk (Linux)", "createdAt": 1763000000000i64, "lastUsedDay": 20400, "current": false}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/me/sessions/sess2"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/v1/me/sessions/other"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(
                json!({"error": {"code": "not_found", "message": "Session not found"}}),
            ),
        )
        .mount(&server)
        .await;
    let secrets = Arc::new(MemorySecretStore::new());
    let s = session(&server.uri(), secrets.clone(), now_ms() + 3_600_000);
    let rows = s.sessions().await.unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows[0].current);
    s.revoke_session("sess2").await.unwrap();
    assert_eq!(
        s.revoke_session("other").await.unwrap_err(),
        AuthError::Rejected("Session not found".into())
    );
    s.logout().await.unwrap();
    s.forget().await.unwrap();
    assert!(secrets.keys().is_empty());
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter()
            .all(|r| r.headers.get("authorization").unwrap() == "Bearer rha_OLD")
    );
}

#[tokio::test]
async fn token_errors_map_to_typed_errors() {
    for (status, code, want) in [
        (426, "client_too_old", AuthError::ClientTooOld),
        (429, "rate_limited", AuthError::RateLimited),
        (507, "server_full", AuthError::ServerFull),
        (401, "session_revoked", AuthError::InvalidGrant),
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_json(json!({"error": {"code": code, "message": code}})),
            )
            .mount(&server)
            .await;
        let c = AuthClient::new(&server.uri()).unwrap();
        assert_eq!(c.refresh("rhr_x").await.unwrap_err(), want, "{status}");
    }
    let c = AuthClient::new("http://127.0.0.1:1").unwrap();
    let e = c.refresh("rhr_SECRET").await.unwrap_err();
    assert!(matches!(e, AuthError::Offline(_)));
    assert!(!format!("{e:?}").contains("SECRET"));
}

// ───────────────────────────── discovery ─────────────────────────────

#[tokio::test]
async fn discovery_over_http() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "software": "reqharbor-server", "version": "0.2.0", "apiVersion": 1,
            "instanceId": "I", "syncEpoch": "E", "name": "Local", "publicUrl": "https://rh.acme.dev",
            "minClientVersion": null,
            "auth": {"methods": [{"id": "password", "label": "Email and password"}], "signup": "invite",
                     "passwordReset": "admin", "setupRequired": false},
            "features": {"accounts": true, "teams": false, "legacyWorkspaces": "off"},
            "sync": {"types": ["project"], "requestKinds": ["http"]}
        })))
        .mount(&server)
        .await;
    let d = server_info::discover(&format!("{}/", server.uri()))
        .await
        .unwrap();
    assert_eq!(d.status, DiscoveryStatus::Ok);
    assert_eq!(d.server_url, server.uri());
    assert_eq!(d.canonical_url.as_deref(), Some("https://rh.acme.dev"));
    assert!(!d.insecure, "loopback http is not warned about");
    let req = &server.received_requests().await.unwrap()[0];
    assert!(req.headers.get("x-reqharbor-version").is_some());

    // The Worker answers 404 not_found.
    let worker = MockServer::start().await;
    Mock::given(wiremock::matchers::any())
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "not_found", "message": "Not found"}})),
        )
        .mount(&worker)
        .await;
    let d = server_info::discover(&worker.uri()).await.unwrap();
    assert_eq!(d.status, DiscoveryStatus::TokenSyncOnly);

    let d = server_info::discover("http://127.0.0.1:1").await.unwrap();
    assert_eq!(d.status, DiscoveryStatus::Unreachable);
    assert_eq!(
        d.message.as_deref(),
        Some("Can't reach 127.0.0.1:1. Check the address and your connection.")
    );
    assert!(server_info::discover("https://x.dev/api").await.is_err());
}

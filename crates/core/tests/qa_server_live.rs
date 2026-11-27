//! LIVE tests that only the Go server can pass (docs/plans/accounts/06-go-server.md §1.6, §7.5):
//! the real Rust client against the real `reqharbor-server`, for cases that need raw HTTP.
//!
//! All tests are #[ignore] (they need a running server). Run them with
//! qa/run_live_server_tests.sh, which starts the server and sets REQHARBOR_SERVER_URL:
//!
//!   docker compose run --rm go go build -tags devauth -o bin/reqharbor-server-devauth ./cmd/reqharbor-server
//!   docker compose run --rm dev bash qa/run_live_server_tests.sh
//!
//! The raw workspace creates here send a random `CF-Connecting-IP` (the script trusts 127.0.0.1
//! as a proxy), so they never use the 127.0.0.1 bucket that qa_sync_live's creates need under the
//! default 10 per hour limit. (b) of §1.6, the restore paths, arrives with `restore` in S7.
//!
//! The accounts case (S4) signs up the first account through the hosted pages with raw HTTP. It
//! needs the setup code, which the script reads from the server log and passes as
//! `REQHARBOR_LIVE_SIGNUP_CODE`.

use std::time::{SystemTime, UNIX_EPOCH};

use reqharbor_core::sync::SyncClient;
use serde_json::{Value, json};

fn server_url() -> String {
    std::env::var("REQHARBOR_SERVER_URL")
        .or_else(|_| std::env::var("REQHARBOR_WORKER_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8080".into())
}

/// A random address in 198.18.0.0/15 (benchmarking), so each run gets a fresh rate-limit bucket.
fn random_ip() -> String {
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    format!(
        "198.{}.{}.{}",
        18 + (n & 1),
        (n >> 1) % 256,
        (n >> 9) % 254 + 1
    )
}

fn http() -> reqwest::Client {
    reqharbor_core::http::install_crypto_provider();
    reqwest::Client::new()
}

/// `POST /v1/workspaces` from a random client IP; returns the token.
async fn raw_workspace(c: &reqwest::Client) -> String {
    let res = c
        .post(format!("{}/v1/workspaces", server_url()))
        .header("Content-Type", "application/json")
        .header("CF-Connecting-IP", random_ip())
        .body(r#"{"name":"qa server live"}"#)
        .send()
        .await
        .expect("create workspace");
    assert_eq!(res.status().as_u16(), 201, "create workspace");
    let v: Value = res.json().await.unwrap();
    v["token"].as_str().unwrap().to_string()
}

/// A JSON string literal holding a lone leading surrogate escape. Built from parts so no tool
/// turns the escape into a character.
fn lone_surrogate_literal() -> String {
    concat!("\"\\", "uD800\"").to_string()
}

fn nested(depth: usize) -> String {
    let mut s = String::from("{\"d\":");
    s.push_str(&"[".repeat(depth - 1));
    s.push_str(&"]".repeat(depth - 1));
    s.push('}');
    s
}

#[tokio::test]
#[ignore]
async fn live_poison_values_are_rejected_per_change_and_pull_all_still_succeeds() {
    let c = http();
    let token = raw_workspace(&c).await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    // serde_json cannot even write these values, so the push is raw text.
    let changes = [
        format!(
            r#"{{"type":"request","id":"OK1","updatedAt":{now},"deleted":false,"deviceId":"qa","data":{{"id":"OK1","name":"fine","n":1e308}}}}"#
        ),
        format!(
            r#"{{"type":"request","id":"BIG","updatedAt":{now},"deleted":false,"deviceId":"qa","data":{{"id":"BIG","n":1e400}}}}"#
        ),
        format!(
            r#"{{"type":"request","id":"LONE","updatedAt":{now},"deleted":false,"deviceId":"qa","data":{{"id":"LONE","s":{}}}}}"#,
            lone_surrogate_literal()
        ),
        format!(
            r#"{{"type":"folder","id":"DEEP","updatedAt":{now},"deleted":false,"deviceId":"qa","data":{}}}"#,
            nested(101)
        ),
        format!(
            r#"{{"type":"folder","id":"OK2","updatedAt":{now},"deleted":false,"deviceId":"qa","data":{}}}"#,
            nested(100)
        ),
    ];
    let body = format!(r#"{{"changes":[{}]}}"#, changes.join(","));
    let res = c
        .post(format!("{}/v1/sync", server_url()))
        .bearer_auth(&token)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let pushed: Value = res.json().await.unwrap();
    let results = pushed["results"].as_array().unwrap();
    let summary: Vec<(String, String)> = results
        .iter()
        .map(|r| {
            (
                r["id"].as_str().unwrap().to_string(),
                r["status"].as_str().unwrap().to_string()
                    + r["reason"]
                        .as_str()
                        .map(|s| format!(":{s}"))
                        .as_deref()
                        .unwrap_or(""),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("OK1".into(), "applied".into()),
            ("BIG".into(), "rejected:invalid_json_value".into()),
            ("LONE".into(), "rejected:invalid_json_value".into()),
            ("DEEP".into(), "rejected:invalid_json_value".into()),
            ("OK2".into(), "applied".into()),
        ]
    );
    // Rejected changes take no rev; the two applied ones take 1 and 2.
    assert_eq!(pushed["cursor"], json!(2));

    // The real client pulls the workspace from cursor 0 without a Protocol error (W9).
    let client = SyncClient::new(&server_url(), Some(&token)).unwrap();
    let pulled = client
        .pull_all(0)
        .await
        .expect("pull_all after poison push");
    let mut ids: Vec<&str> = pulled.changes.iter().map(|ch| ch.id.as_str()).collect();
    ids.sort();
    assert_eq!(ids, ["OK1", "OK2"]);
    assert_eq!(pulled.cursor, 2);
    assert!(!pulled.restored);

    // The pull's epoch is the one discovery reports.
    let info = client.server_info().await.unwrap().expect("GET /v1/server");
    assert_eq!(info.software.as_deref(), Some("reqharbor-server"));
    assert!(info.sync_epoch.is_some());
    assert_eq!(info.sync_epoch, pulled.sync_epoch);
}

#[tokio::test]
#[ignore]
async fn live_share_with_an_unreadable_snapshot_value_is_400() {
    let c = http();
    let token = raw_workspace(&c).await;
    let body = format!(
        r#"{{"kind":"request","entityId":"R1","snapshot":{{"schemaVersion":1,"request":{{"name":{},"url":"https://a.test"}}}}}}"#,
        lone_surrogate_literal()
    );
    let res = c
        .post(format!("{}/v1/shares", server_url()))
        .bearer_auth(&token)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 400);
    let v: Value = res.json().await.unwrap();
    assert_eq!(v["error"]["code"], json!("validation_failed"));
    assert_eq!(
        v["error"]["message"],
        json!("Snapshot contains a value ReqHarbor cannot read")
    );
}

#[tokio::test]
#[ignore]
async fn live_share_page_renders_an_import_card_for_non_http_kinds() {
    let c = http();
    let token = raw_workspace(&c).await;
    let snapshot = json!({
        "schemaVersion": 1,
        "collection": {"name": "Kinds", "requests": [
            {"name": "Plain", "method": "GET", "url": "https://api.example.com/x"},
            {"name": "Stream", "kind": "grpc", "url": "grpc://orders:50051"},
            {"name": "Feed", "kind": "websocket", "url": "wss://feed.example"}
        ]}
    });
    let res = c
        .post(format!("{}/v1/shares", server_url()))
        .bearer_auth(&token)
        .json(&json!({"kind": "collection", "entityId": "C1", "snapshot": snapshot}))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 201);
    let url = res.json::<Value>().await.unwrap()["url"]
        .as_str()
        .unwrap()
        .to_string();
    let page = c.get(&url).send().await.unwrap();
    assert_eq!(page.status().as_u16(), 200);
    let csp = page.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_string();
    assert!(csp.contains("script-src 'sha256-"), "{csp}");
    let html = page.text().await.unwrap();
    let card = "This request type can't be previewed on the web. Import it into ReqHarbor.";
    assert_eq!(html.matches(card).count(), 2);
    assert_eq!(html.matches("data-copy=").count(), 1);
}

// --- S4 accounts: the hosted password flow, the token endpoint, refresh grace and logout ---

/// RFC 7636 appendix B: a verifier and its S256 challenge, so the test needs no SHA-256 crate.
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://127.0.0.1:49152/cb";

fn form(pairs: &[(&str, &str)]) -> String {
    let mut s = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        s.append_pair(k, v);
    }
    s.finish()
}

/// One browser: no redirects followed, the auth cookie kept by hand, a fixed client IP.
struct Browser {
    c: reqwest::Client,
    cookie: Option<String>,
    ip: String,
}

impl Browser {
    fn new() -> Self {
        reqharbor_core::http::install_crypto_provider();
        let c = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        Browser {
            c,
            cookie: None,
            ip: random_ip(),
        }
    }

    async fn send(
        &mut self,
        req: reqwest::RequestBuilder,
    ) -> (u16, reqwest::header::HeaderMap, String) {
        let mut req = req.header("CF-Connecting-IP", &self.ip);
        if let Some(c) = &self.cookie {
            req = req.header("Cookie", c);
        }
        let res = req.send().await.unwrap();
        let status = res.status().as_u16();
        let headers = res.headers().clone();
        if let Some(set) = headers.get("set-cookie") {
            let pair = set.to_str().unwrap().split(';').next().unwrap().to_string();
            self.cookie = Some(pair);
        }
        (status, headers, res.text().await.unwrap())
    }

    async fn get(&mut self, path: &str) -> (u16, reqwest::header::HeaderMap, String) {
        let req = self.c.get(format!("{}{path}", server_url()));
        self.send(req).await
    }

    async fn post(
        &mut self,
        path: &str,
        body: String,
    ) -> (u16, reqwest::header::HeaderMap, String) {
        let req = self
            .c
            .post(format!("{}{path}", server_url()))
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body);
        self.send(req).await
    }

    /// Opens the sign-in page and returns the auth request id.
    async fn authorize(&mut self, state: &str) -> (String, String) {
        let q = form(&[
            ("client_id", "reqharbor-desktop"),
            ("redirect_uri", REDIRECT),
            ("code_challenge", PKCE_CHALLENGE),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("device_name", "QA live (Linux)"),
        ]);
        let (status, headers, html) = self.get(&format!("/auth/authorize?{q}")).await;
        assert_eq!(status, 200, "{html}");
        let csp = headers["content-security-policy"].to_str().unwrap();
        assert!(
            csp.contains("form-action 'self' http://127.0.0.1:*"),
            "{csp}"
        );
        let cookie = self.cookie.clone().expect("auth cookie");
        assert!(cookie.starts_with("rh_ar="), "{cookie}"); // http PUBLIC_URL: no __Secure- prefix
        let marker = r#"name="ar" value=""#;
        let start = html.find(marker).expect("ar field") + marker.len();
        (html[start..start + 22].to_string(), html)
    }
}

/// The authorization code from a 303 to the loopback, after checking the state.
fn loopback_code(
    status: u16,
    headers: &reqwest::header::HeaderMap,
    body: &str,
    state: &str,
) -> String {
    assert_eq!(status, 303, "{body}");
    let loc = url::Url::parse(headers["location"].to_str().unwrap()).unwrap();
    assert_eq!(
        format!(
            "{}://{}:{}{}",
            loc.scheme(),
            loc.host_str().unwrap(),
            loc.port().unwrap(),
            loc.path()
        ),
        REDIRECT
    );
    let q: std::collections::HashMap<_, _> = loc.query_pairs().into_owned().collect();
    assert_eq!(q["state"], state);
    q["code"].clone()
}

async fn token(c: &reqwest::Client, pairs: &[(&str, &str)]) -> (u16, Value) {
    let res = c
        .post(format!("{}/v1/auth/token", server_url()))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .header("CF-Connecting-IP", random_ip())
        .body(form(pairs))
        .send()
        .await
        .unwrap();
    (res.status().as_u16(), res.json().await.unwrap())
}

async fn logout(c: &reqwest::Client, access: &str) -> (u16, String) {
    let res = c
        .post(format!("{}/v1/auth/logout", server_url()))
        .bearer_auth(access)
        .send()
        .await
        .unwrap();
    (res.status().as_u16(), res.text().await.unwrap())
}

#[tokio::test]
#[ignore]
async fn live_password_sign_up_with_setup_code_then_tokens_refresh_grace_and_logout() {
    let code = std::env::var("REQHARBOR_LIVE_SIGNUP_CODE")
        .expect("REQHARBOR_LIVE_SIGNUP_CODE: the setup code from the server log (qa/run_live_server_tests.sh sets it)");
    let c = http();
    let info: Value = c
        .get(format!("{}/v1/server", server_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["features"]["accounts"], json!(true));
    assert_eq!(info["auth"]["methods"][0]["id"], json!("password"));
    assert_eq!(
        info["auth"]["setupRequired"],
        json!(true),
        "run against a fresh server"
    );

    // Setup code, then the admin signup, then the 303 to the loopback with the code.
    let mut b = Browser::new();
    let (ar, html) = b.authorize("st-1").await;
    assert!(html.contains("Finish setting up this server"), "{html}");
    let (status, headers, body) = b
        .post("/auth/code", form(&[("ar", &ar), ("code", &code)]))
        .await;
    assert_eq!(status, 303, "{body}");
    assert_eq!(
        headers["location"].to_str().unwrap(),
        format!("/auth/signup?ar={ar}")
    );
    let email = format!("qa-{}@example.test", random_ip().replace('.', "-"));
    let (status, headers, body) = b
        .post(
            "/auth/password/signup",
            form(&[
                ("ar", &ar),
                ("name", "QA"),
                ("email", &email),
                ("password", "qa live password"),
            ]),
        )
        .await;
    let auth_code = loopback_code(status, &headers, &body, "st-1");

    // Exchange: RFC 6749 snake_case; a second exchange of the same code fails.
    let exchange = [
        ("grant_type", "authorization_code"),
        ("client_id", "reqharbor-desktop"),
        ("code", auth_code.as_str()),
        ("code_verifier", PKCE_VERIFIER),
        ("redirect_uri", REDIRECT),
    ];
    let (status, t0) = token(&c, &exchange).await;
    assert_eq!(status, 200, "{t0}");
    assert_eq!(t0["token_type"], json!("Bearer"));
    assert_eq!(t0["expires_in"], json!(3600));
    assert_eq!(t0["user"]["email"], json!(email));
    assert_eq!(t0["user"]["isAdmin"], json!(true));
    let (status, err) = token(&c, &exchange).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (400, json!("invalid_grant"))
    );

    // Refresh rotates; the superseded token returns the identical pair inside the grace window.
    let old_refresh = t0["refresh_token"].as_str().unwrap().to_string();
    let refresh = [
        ("grant_type", "refresh_token"),
        ("client_id", "reqharbor-desktop"),
        ("refresh_token", old_refresh.as_str()),
    ];
    let (status, t1) = token(&c, &refresh).await;
    assert_eq!(status, 200, "{t1}");
    assert_ne!(t1["refresh_token"], t0["refresh_token"]);
    assert_eq!(t1["session_id"], t0["session_id"]);
    let (status, again) = token(&c, &refresh).await;
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["access_token"], t1["access_token"]);
    assert_eq!(again["refresh_token"], t1["refresh_token"]);

    // Setup is done; a password login works for the same account.
    let info: Value = c
        .get(format!("{}/v1/server", server_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(info["auth"]["setupRequired"], json!(false));
    let mut b2 = Browser::new();
    let (ar2, _) = b2.authorize("st-2").await;
    let (status, headers, body) = b2
        .post(
            "/auth/password/login",
            form(&[
                ("ar", &ar2),
                ("email", &email),
                ("password", "qa live password"),
            ]),
        )
        .await;
    loopback_code(status, &headers, &body, "st-2");
    // The first browser's cookie does not work for the second browser's request (login CSRF).
    let (status, _, body) = b
        .post(
            "/auth/password/login",
            form(&[
                ("ar", &ar2),
                ("email", &email),
                ("password", "qa live password"),
            ]),
        )
        .await;
    assert_eq!(status, 400, "{body}");

    // Logout revokes the session.
    let access = t1["access_token"].as_str().unwrap();
    assert_eq!(logout(&c, access).await.0, 204);
    let (status, body) = logout(&c, access).await;
    assert_eq!(status, 401, "{body}");
    assert!(body.contains("auth_required"), "{body}");
}

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
//! default 10 per hour limit.
//!
//! The accounts case (S4) signs up the first account through the hosted pages with raw HTTP. It
//! needs the setup code, which the script reads from the server log and passes as
//! `REQHARBOR_LIVE_SIGNUP_CODE`.
//!
//! The dev-provider case (S5) signs up through the devauth build's dev form with an invite that
//! the script creates with `invite create` and passes as `REQHARBOR_LIVE_INVITE_CODE`. An invite
//! cannot create the first account, so it relies on libtest's name order (with
//! --test-threads=1): `live_password_…` runs before `live_provider_dev_…`.
//!
//! The restore cases (S7, §6 "Restore") stop and restart the server through
//! qa/live_server_ctl.sh, which the script passes as `REQHARBOR_LIVE_SERVER_CTL`: one restores a
//! backup with `reqharbor-server restore`, the other copies it over the database by hand (the
//! unsupported path). Each restores a backup taken inside the case, so the accounts that earlier
//! cases created survive it.

use std::time::{SystemTime, UNIX_EPOCH};

use reqharbor_core::model::Request;
use reqharbor_core::storage::Store;
use reqharbor_core::sync::{self, SyncClient};
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

#[tokio::test]
#[ignore]
async fn live_provider_dev_sign_in_is_unlisted_untrusted_and_follows_signup_rules() {
    let invite = std::env::var("REQHARBOR_LIVE_INVITE_CODE")
        .expect("REQHARBOR_LIVE_INVITE_CODE: an invite from `invite create` (qa/run_live_server_tests.sh sets it)");
    let c = http();
    let info: Value = c
        .get(format!("{}/v1/server", server_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        info["auth"]["setupRequired"],
        json!(false),
        "the S4 case creates the first account before this one runs"
    );
    let methods: Vec<&str> = info["auth"]["methods"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        methods,
        ["password"],
        "dev is never listed; GitHub is not configured"
    );

    // GitHub is not configured, so its routes do not exist.
    let mut b = Browser::new();
    let (status, _, _) = b.get("/auth/start/github?ar=x").await;
    assert_eq!(status, 404);

    // The authorize page offers the dev form to this loopback peer.
    let (ar, html) = b.authorize("st-dev").await;
    assert!(
        html.contains(&format!("href=\"/auth/start/dev?ar={ar}\"")),
        "{html}"
    );
    let (status, _, form_page) = b.get(&format!("/auth/start/dev?ar={ar}")).await;
    assert_eq!(status, 200, "{form_page}");
    assert!(form_page.contains(r#"action="/auth/dev""#), "{form_page}");

    // Invite mode: without the code nothing is created.
    let email = format!("dev-{}@example.test", random_ip().replace('.', "-"));
    let (status, _, body) = b
        .post(
            "/auth/dev",
            form(&[("ar", &ar), ("email", &email), ("name", "Dev")]),
        )
        .await;
    assert_eq!(status, 400, "{body}");
    assert!(
        body.contains("Enter the invite code your server admin gave you."),
        "{body}"
    );
    let (status, headers, body) = b
        .post(
            "/auth/dev",
            form(&[
                ("ar", &ar),
                ("email", &email),
                ("name", "Dev"),
                ("code", &invite),
            ]),
        )
        .await;
    let auth_code = loopback_code(status, &headers, &body, "st-dev");
    let (status, t) = token(
        &c,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", "reqharbor-desktop"),
            ("code", auth_code.as_str()),
            ("code_verifier", PKCE_VERIFIER),
            ("redirect_uri", REDIRECT),
        ],
    )
    .await;
    assert_eq!(status, 200, "{t}");
    assert_eq!(t["user"]["email"], json!(email));
    assert_eq!(t["user"]["isAdmin"], json!(false));

    // A returning dev identity signs in without a code.
    let mut b2 = Browser::new();
    let (ar2, _) = b2.authorize("st-dev-2").await;
    let (status, headers, body) = b2
        .post("/auth/dev", form(&[("ar", &ar2), ("email", &email)]))
        .await;
    loopback_code(status, &headers, &body, "st-dev-2");
}

/// Signs a new account up through the devauth build's dev form with an invite; returns the token
/// response.
async fn dev_sign_up(c: &reqwest::Client, invite: &str, email: &str, state: &str) -> Value {
    let mut b = Browser::new();
    let (ar, _) = b.authorize(state).await;
    let (status, headers, body) = b
        .post(
            "/auth/dev",
            form(&[("ar", &ar), ("email", email), ("name", "Space QA"), ("code", invite)]),
        )
        .await;
    let auth_code = loopback_code(status, &headers, &body, state);
    let (status, t) = token(
        c,
        &[
            ("grant_type", "authorization_code"),
            ("client_id", "reqharbor-desktop"),
            ("code", auth_code.as_str()),
            ("code_verifier", PKCE_VERIFIER),
            ("redirect_uri", REDIRECT),
        ],
    )
    .await;
    assert_eq!(status, 200, "{t}");
    t
}

async fn get_json(c: &reqwest::Client, path: &str, access: &str) -> (u16, Value) {
    let res = c
        .get(format!("{}{path}", server_url()))
        .bearer_auth(access)
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap_or(Value::Null))
}

async fn post_json(c: &reqwest::Client, path: &str, access: &str, body: &Value) -> (u16, Value) {
    let res = c
        .post(format!("{}{path}", server_url()))
        .bearer_auth(access)
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = res.status().as_u16();
    (status, res.json().await.unwrap_or(Value::Null))
}

/// S6 personal spaces (06-go-server.md §4, §10 S6), with raw HTTP: /v1/me, /v1/me/sessions,
/// /v1/me/spaces, a push mixing a request with a future `protoFile` and a skewed change, a pull
/// with `updatedBy` and `syncEpoch` that the real client parses, space shares with `createdBy` and
/// `options`, and the token schemes kept apart. Runs after the S4 and S5 cases (name order), so
/// the first account exists and the S5 invite has uses left.
#[tokio::test]
#[ignore]
async fn live_space_personal_sync_me_and_shares() {
    let invite = std::env::var("REQHARBOR_LIVE_INVITE_CODE")
        .expect("REQHARBOR_LIVE_INVITE_CODE: an invite from `invite create` (qa/run_live_server_tests.sh sets it)");
    let c = http();
    let email = format!("space-{}@example.test", random_ip().replace('.', "-"));
    let t = dev_sign_up(&c, &invite, &email, "st-space").await;
    let access = t["access_token"].as_str().unwrap().to_string();
    let user_id = t["user"]["id"].as_str().unwrap().to_string();

    // /v1/me: the user, the method and this session.
    let (status, me) = get_json(&c, "/v1/me", &access).await;
    assert_eq!(status, 200, "{me}");
    assert_eq!(me["user"]["email"], json!(email));
    assert_eq!(me["method"], json!("dev"));
    assert_eq!(me["sessionId"], t["session_id"]);

    // /v1/me/sessions: exactly this device, marked current.
    let (status, sessions) = get_json(&c, "/v1/me/sessions", &access).await;
    assert_eq!(status, 200, "{sessions}");
    let list = sessions["sessions"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{sessions}");
    assert_eq!(list[0]["current"], json!(true));
    assert_eq!(list[0]["deviceName"], json!("QA live (Linux)"));

    // /v1/me/spaces: the personal space, with the final M1 shape.
    let (status, spaces) = get_json(&c, "/v1/me/spaces", &access).await;
    assert_eq!(status, 200, "{spaces}");
    assert_eq!(spaces["departed"], json!([]));
    let personal = &spaces["spaces"][0];
    assert_eq!(personal["kind"], json!("personal"));
    assert_eq!(personal["role"], json!("owner"));
    assert_eq!(personal["teamId"], Value::Null);
    let space = personal["spaceId"].as_str().unwrap().to_string();
    let sync = format!("/v1/spaces/{space}/sync");

    // A push mixing a request, a future type and a change 10 minutes ahead.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let push = json!({"changes": [
        {"type": "request", "id": "r1", "updatedAt": now, "deleted": false, "deviceId": "01QA",
         "data": {"id": "r1", "name": "Live request"}},
        {"type": "protoFile", "id": "p1", "updatedAt": now, "deleted": false, "data": {"id": "p1"}},
        {"type": "request", "id": "r2", "updatedAt": now + 600_000, "deleted": false,
         "data": {"id": "r2", "name": "From the future"}},
    ]});
    let (status, resp) = post_json(&c, &sync, &access, &push).await;
    assert_eq!(status, 200, "{resp}");
    assert_eq!(
        resp["results"],
        json!([
            {"type": "request", "id": "r1", "status": "applied", "rev": 1},
            {"type": "protoFile", "id": "p1", "status": "rejected", "reason": "unsupported_type"},
            {"type": "request", "id": "r2", "status": "rejected", "reason": "clock_skew"},
        ])
    );
    assert_eq!(resp["cursor"], json!(1));
    let server_time = resp["serverTime"].as_u64().expect("serverTime");
    assert!(server_time.abs_diff(now) < 120_000, "{resp}");

    // The same push on legacy token sync is a whole-request 400.
    let legacy = raw_workspace(&c).await;
    let (status, err) = post_json(&c, "/v1/sync", &legacy, &push).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (400, json!("unsupported_type"))
    );

    // Pull: updatedBy and syncEpoch, and the page parses as the real client's PullPage.
    let res = c
        .get(format!("{}{sync}?since=0", server_url()))
        .bearer_auth(&access)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status().as_u16(), 200);
    let text = res.text().await.unwrap();
    let page: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        page["changes"][0]["updatedBy"],
        json!({"userId": user_id, "name": "Space QA"})
    );
    assert_eq!(page["changes"][0]["deviceId"], json!("01QA"));
    let parsed: reqharbor_core::sync::PullPage = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed.changes.len(), 1);
    assert_eq!(parsed.cursor, 1);
    assert!(parsed.sync_epoch.as_deref().is_some_and(|e| !e.is_empty()));

    // Shares in the space carry createdBy and the stored options.
    let snapshot = json!({"schemaVersion": 1, "request": {"id": "r1", "name": "Live request", "method": "GET",
        "url": "https://example.com", "params": [], "headers": [], "auth": {"type": "none"},
        "body": {"type": "none"}}, "environment": null});
    let (status, created) = post_json(
        &c,
        &format!("/v1/spaces/{space}/shares"),
        &access,
        &json!({"kind": "request", "entityId": "r1", "snapshot": snapshot,
                "options": {"includeCredentials": false, "environmentId": null}}),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    assert_eq!(created["createdBy"]["userId"], json!(user_id));
    assert_eq!(
        created["options"],
        json!({"includeCredentials": false, "environmentId": null})
    );
    let (status, listed) = get_json(&c, &format!("/v1/spaces/{space}/shares"), &access).await;
    assert_eq!(status, 200, "{listed}");
    assert_eq!(listed["shares"][0]["shareId"], created["shareId"]);
    assert_eq!(listed["shares"][0]["createdBy"]["name"], json!("Space QA"));

    // The token schemes never cross: rh_ on a space, rha_ on legacy.
    let (status, err) = get_json(&c, &sync, &legacy).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (401, json!("auth_required"))
    );
    let (status, err) = get_json(&c, "/v1/sync", &access).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (401, json!("unauthorized"))
    );
    // Another account's space is 403; an unknown one 404.
    let other = dev_sign_up(
        &c,
        &invite,
        &format!("other-{}@example.test", random_ip().replace('.', "-")),
        "st-space-2",
    )
    .await;
    let (status, err) = get_json(&c, &sync, other["access_token"].as_str().unwrap()).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (403, json!("not_a_member"))
    );
    let (status, err) = get_json(&c, "/v1/spaces/01UNKNOWN0000000000000000X/sync", &access).await;
    assert_eq!(
        (status, err["error"]["code"].clone()),
        (404, json!("space_not_found"))
    );
}

/// Runs qa/live_server_ctl.sh with `args` and returns its stdout.
fn server_ctl(args: &[&str]) -> String {
    let ctl = std::env::var("REQHARBOR_LIVE_SERVER_CTL").expect(
        "REQHARBOR_LIVE_SERVER_CTL: qa/live_server_ctl.sh (qa/run_live_server_tests.sh sets it)",
    );
    let out = std::process::Command::new("bash")
        .arg(&ctl)
        .args(args)
        .output()
        .expect("run the server control script");
    assert!(
        out.status.success(),
        "server ctl {args:?} failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// A device: a fresh local store connected to the legacy workspace of `token`.
async fn sync_device(token: &str) -> (tempfile::TempDir, Store, SyncClient) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    sync::connect(&mut s, &server_url(), token)
        .await
        .expect("connect");
    let c = sync::client_for(&s).unwrap();
    (dir, s, c)
}

async fn sync_epoch(c: &reqwest::Client) -> String {
    let v: Value = c
        .get(format!("{}/v1/server", server_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    v["syncEpoch"].as_str().unwrap().to_string()
}

/// §6 Restore, both paths: device 1 pushes A, the admin takes a backup, device 1 pushes B (and
/// syncs again, so its cursor covers B), the backup is put back, then device 1 syncs: it notices
/// the restore (the new epoch, or by hand the cursor regression), pulls everything again and
/// re-pushes B; device 2 ends with A and B and no conflict.
async fn restore_round_trip(by_hand: bool) {
    let c = http();
    let token = raw_workspace(&c).await;
    let (_d1, mut a, ca) = sync_device(&token).await;
    let pid = a.project_list()[0].id.clone();
    let col = a.create_collection(&pid, "Restore").unwrap();
    let req = |name: &str| Request {
        name: name.into(),
        method: "GET".into(),
        url: format!("https://api.example.com/{name}"),
        ..Request::default()
    };
    let ra = a.create_request(&col.id, None, req("a")).unwrap();
    sync::sync_now(&ca, &mut a).await.expect("device 1 pushes A");
    let (_d2, mut b, cb) = sync_device(&token).await;
    sync::sync_now(&cb, &mut b).await.expect("device 2 pulls A");
    assert!(b.request(&ra.id).is_some(), "device 2 has A");

    let backup = server_ctl(&["backup"]).trim().to_string();
    assert!(backup.ends_with(".db"), "backup path {backup:?}");

    let rb = a.create_request(&col.id, None, req("b")).unwrap();
    let rep = sync::sync_now(&ca, &mut a).await.expect("device 1 pushes B");
    assert!(rep.pushed >= 1, "{rep:?}");
    sync::sync_now(&ca, &mut a).await.expect("device 1 pulls its own B");
    let epoch_before = sync_epoch(&c).await;

    server_ctl(&[if by_hand { "replace" } else { "restore" }, &backup]);
    // The server process was restarted: connections pooled before it (keep-alive) are dead, so
    // take fresh clients, as an app does after a failed request.
    let c = http();
    let ca = sync::client_for(&a).unwrap();
    let cb = sync::client_for(&b).unwrap();
    let epoch_after = sync_epoch(&c).await;
    if by_hand {
        assert_eq!(epoch_after, epoch_before, "copying a file keeps the epoch");
    } else {
        assert_ne!(epoch_after, epoch_before, "restore rotates the epoch");
    }
    let lost = ca.pull_all(0).await.expect("pull after the restore");
    assert!(
        !lost.changes.iter().any(|ch| ch.id == rb.id),
        "B was pushed after the backup, so the restored server lacks it"
    );

    let rep = sync::sync_now(&ca, &mut a)
        .await
        .expect("device 1 after the restore");
    assert!(rep.restored, "device 1 noticed the restore: {rep:?}");
    assert!(rep.pushed >= 1, "device 1 re-pushed B: {rep:?}");
    assert!(rep.conflicts.is_empty(), "{rep:?}");
    let back = ca.pull_all(0).await.unwrap();
    assert!(
        back.changes.iter().any(|ch| ch.id == rb.id && !ch.deleted),
        "the server has B again"
    );

    let rep = sync::sync_now(&cb, &mut b)
        .await
        .expect("device 2 after the restore");
    assert!(rep.conflicts.is_empty(), "{rep:?}");
    assert!(b.request(&ra.id).is_some(), "device 2 keeps A");
    assert_eq!(b.request(&rb.id).map(|r| r.name.as_str()), Some("b"), "device 2 has B");
}

#[tokio::test]
#[ignore]
async fn live_restore_a_with_the_command_rotates_the_epoch_and_clients_re_push() {
    restore_round_trip(false).await;
}

#[tokio::test]
#[ignore]
async fn live_restore_b_by_hand_is_recovered_by_the_cursor_regression_rule() {
    restore_round_trip(true).await;
}

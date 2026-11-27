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

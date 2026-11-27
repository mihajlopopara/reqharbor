//! QA round 1 — LIVE end-to-end tests: the real Rust sync/share client
//! (crates/core) against the real Worker (worker/) running under `wrangler dev`.
//! These catch client <-> worker semantic mismatches that the devs' Rust mock
//! worker (tests/sync_mock.rs) cannot.
//!
//! All tests are #[ignore] (they need a running worker). Run them with
//! qa/run_live_worker_tests.sh, which starts wrangler and sets
//! REQHARBOR_WORKER_URL. Tests whose name starts with `bug_` are EXPECTED TO
//! FAIL until the referenced QA-R1-xxx bug is fixed.

use reqharbor_core::model::*;
use reqharbor_core::share::{ShareOptions, build_snapshot, import_snapshot};
use reqharbor_core::storage::Store;
use reqharbor_core::sync::{self, ShareKind, SyncClient, SyncError};

fn worker_url() -> String {
    std::env::var("REQHARBOR_WORKER_URL").unwrap_or_else(|_| "http://127.0.0.1:8787".into())
}

fn device() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

/// Each test creates its own workspace. The worker rate-limits creation to
/// 10/hour/IP, so tests share as few creates as possible.
async fn new_workspace() -> (String, String) {
    let c = SyncClient::new(&worker_url(), None).unwrap();
    let ws = c.create_workspace("qa").await.expect("create workspace");
    (ws.workspace_id, ws.token)
}

async fn connected_device(token: &str) -> (tempfile::TempDir, Store, SyncClient) {
    let (d, mut s) = device();
    sync::connect(&mut s, &worker_url(), token)
        .await
        .expect("connect");
    let c = sync::client_for(&s).unwrap();
    (d, s, c)
}

fn first_project(s: &Store) -> String {
    s.project_list()[0].id.clone()
}

fn add_request(s: &mut Store, cid: &str, name: &str, body: &str) -> Request {
    let mut r = Request {
        name: name.into(),
        method: "POST".into(),
        url: "https://api.example.com/{{path}}".into(),
        ..Request::default()
    };
    r.body = Body::json(body);
    s.create_request(cid, None, r).unwrap()
}

#[tokio::test]
#[ignore]
async fn live_two_devices_sync_conflict_delete_secrets_and_shares() {
    let (_ws, token) = new_workspace().await;
    let (_da, mut a, ca) = connected_device(&token).await;
    let pid = first_project(&a);
    let col = a.create_collection(&pid, "Live").unwrap();
    let r1 = add_request(&mut a, &col.id, "one", r#"{"a":1}"#);
    let r2 = add_request(&mut a, &col.id, "two", r#"{"b":2}"#);
    let mut g = a.global_environment().unwrap().clone();
    g.variables.push(Variable::new("path", "v1/items"));
    g.variables
        .push(Variable::new("token", "S3CRET-VALUE").secret());
    a.update_environment(&g).unwrap();
    let rep = sync::sync_now(&ca, &mut a).await.expect("A sync");
    assert!(rep.pushed >= 4, "{rep:?}");

    // Server never stores the secret value.
    let changes = ca.pull_all(0).await.unwrap().changes;
    let raw = serde_json::to_string(&changes).unwrap();
    assert!(!raw.contains("S3CRET-VALUE"));

    // Device B adopts the tree.
    let (_db, mut b, cb) = connected_device(&token).await;
    sync::sync_now(&cb, &mut b).await.expect("B sync");
    assert!(b.request(&r1.id).is_some(), "B did not receive request one");
    assert_eq!(
        b.project_list().len(),
        1,
        "fresh device should drop its empty Default"
    );

    // Conflict: both edit r2; A pushes first, B's older-based edit loses
    // and is kept as a "(conflict)" copy.
    let mut ea = a.request(&r2.id).unwrap().clone();
    ea.name = "two (A)".into();
    a.save_request(&ea).unwrap();
    let mut eb = b.request(&r2.id).unwrap().clone();
    eb.name = "two (B)".into();
    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    b.save_request(&eb).unwrap();
    sync::sync_now(&ca, &mut a).await.unwrap();
    let rb = sync::sync_now(&cb, &mut b).await.unwrap();
    sync::sync_now(&ca, &mut a).await.unwrap();
    // B's edit is newer by updatedAt so it wins everywhere (LWW).
    assert_eq!(a.request(&r2.id).unwrap().name, "two (B)", "{rb:?}");
    assert_eq!(b.request(&r2.id).unwrap().name, "two (B)");

    // Delete propagates.
    a.delete_request(&r1.id).unwrap();
    sync::sync_now(&ca, &mut a).await.unwrap();
    sync::sync_now(&cb, &mut b).await.unwrap();
    assert!(b.request(&r1.id).is_none(), "delete did not propagate");

    // Shares: create -> public json -> import -> update -> list -> revoke -> 410
    let (snap, _) =
        build_snapshot(&a, ShareKind::Collection, &col.id, &ShareOptions::default()).unwrap();
    let info = ca
        .create_share(ShareKind::Collection, &col.id, &snap)
        .await
        .expect("share");
    assert!(info.url.contains("/s/"), "{info:?}");
    let public = SyncClient::fetch_public_share(&info.url)
        .await
        .expect("public json");
    assert_eq!(public.kind, ShareKind::Collection);
    let bp = first_project(&b);
    let tree = import_snapshot(&mut b, &bp, public.kind, &public.snapshot).unwrap();
    assert_eq!(
        tree.collections.iter().filter(|c| c.name == "Live").count(),
        2
    );
    let upd = ca
        .update_share(&info.share_id, &snap)
        .await
        .expect("update");
    assert_eq!(upd.url, info.url);
    let list = ca.list_shares().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].entity_id.as_deref(), Some(col.id.as_str()));
    ca.revoke_share(&info.share_id).await.unwrap();
    match SyncClient::fetch_public_share(&info.url).await {
        Err(SyncError::Gone(_)) => {}
        other => panic!("revoked share: {other:?}"),
    }
    assert!(matches!(
        ca.update_share(&info.share_id, &snap).await,
        Err(SyncError::Gone(_))
    ));

    // Token rotation invalidates the old token immediately (AC-SYNC-7 path).
    let new_token = ca.rotate_token().await.unwrap();
    assert!(matches!(cb.workspace().await, Err(SyncError::Unauthorized)));
    let c2 = SyncClient::new(&worker_url(), Some(&new_token)).unwrap();
    assert!(c2.workspace().await.is_ok());
}

/// PRODUCT §8 caps a request body at 5 MB, but the client batches pushes by
/// COUNT only (500 changes). A first sync of a normal project — 500 requests
/// with ~12 KB JSON bodies each — produces one ~6 MB POST /v1/sync, which the
/// Worker rejects with 413. The error is non-retryable (`sync_rejected`) and
/// recurs on every attempt: sync never succeeds for that workspace.
#[tokio::test]
#[ignore]
async fn bug_qa_r1_004_push_batch_exceeds_worker_5mb_limit() {
    let (_ws, token) = new_workspace().await;
    let (_d, mut s, c) = connected_device(&token).await;
    let pid = first_project(&s);
    let col = s.create_collection(&pid, "Big").unwrap();
    let body = format!("{{\"blob\":\"{}\"}}", "x".repeat(12 * 1024));
    for i in 0..500 {
        add_request(&mut s, &col.id, &format!("r{i}"), &body);
    }
    let res = sync::sync_now(&c, &mut s).await;
    assert!(
        res.is_ok(),
        "first sync of a 6 MB project failed: {:?}",
        res.err()
    );
}

/// One request whose JSON body is > 512 KB (Worker entity limit) makes the
/// whole push fail with 413 `entity_too_large`; every other pending change
/// (here a small, unrelated request) is never synced either, on every retry.
#[tokio::test]
#[ignore]
async fn bug_qa_r1_005_one_oversized_entity_blocks_all_sync() {
    let (_ws, token) = new_workspace().await;
    let (_d, mut s, c) = connected_device(&token).await;
    let pid = first_project(&s);
    let col = s.create_collection(&pid, "Mixed").unwrap();
    let small = add_request(&mut s, &col.id, "small", "{}");
    let big = format!("{{\"blob\":\"{}\"}}", "x".repeat(600 * 1024));
    add_request(&mut s, &col.id, "big", &big);
    let res = sync::sync_now(&c, &mut s).await;
    // Whatever happens to the big one, the small one must reach the server.
    let changes = c.pull_all(0).await.unwrap().changes;
    let small_synced = changes.iter().any(|ch| ch.id == small.id);
    assert!(
        small_synced,
        "small request never synced; sync_now => {:?}",
        res.err()
    );
}

/// Worker's per-workspace LWW with equal `updatedAt` from two devices: the
/// server takes the later push, but the first pusher treats the remote copy
/// (same updatedAt) as its own and never converges.
#[tokio::test]
#[ignore]
async fn bug_qa_r1_026_equal_updated_at_from_two_devices_diverges() {
    let (_ws, token) = new_workspace().await;
    let (_da, mut a, ca) = connected_device(&token).await;
    let pid = first_project(&a);
    let col = a.create_collection(&pid, "Tie").unwrap();
    let r = add_request(&mut a, &col.id, "tie", "{}");
    sync::sync_now(&ca, &mut a).await.unwrap();
    let (_db, mut b, cb) = connected_device(&token).await;
    sync::sync_now(&cb, &mut b).await.unwrap();

    // Force identical updatedAt on both devices with different content.
    let ts = now_ms() + 10_000;
    for (store, name) in [(&mut a, "from A"), (&mut b, "from B")] {
        let pid = store
            .project_of(reqharbor_core::storage::NodeKind::Request, &r.id)
            .unwrap();
        let tree = store.projects.get_mut(&pid).unwrap();
        let req = tree.requests.iter_mut().find(|x| x.id == r.id).unwrap();
        req.name = name.into();
        req.updated_at = ts;
        store.save_project(&pid).unwrap();
    }
    sync::sync_now(&ca, &mut a).await.unwrap();
    sync::sync_now(&cb, &mut b).await.unwrap();
    sync::sync_now(&ca, &mut a).await.unwrap();
    let na = a.request(&r.id).unwrap().name.clone();
    let nb = b.request(&r.id).unwrap().name.clone();
    eprintln!("tie: A={na:?} B={nb:?}");
    assert_eq!(na, nb, "devices diverged permanently on an updatedAt tie");
}

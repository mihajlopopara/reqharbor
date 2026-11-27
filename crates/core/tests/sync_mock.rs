//! Sync client + engine against an in-memory mock of the Worker API
//! (wiremock with a stateful responder implementing PRODUCT §8 semantics).

use reqharbor_core::model::*;
use reqharbor_core::share::{ShareOptions, build_snapshot, import_snapshot};
use reqharbor_core::storage::Store;
use reqharbor_core::sync::{self, ShareKind, SyncClient, SyncError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate, matchers::any};

#[derive(Default)]
struct Entity {
    rev: u64,
    updated_at: i64,
    device_id: String,
    deleted: bool,
    data: Option<Value>,
}

#[derive(Default)]
struct WorkerState {
    token: String,
    rev: u64,
    entities: BTreeMap<(String, String), Entity>,
    shares: BTreeMap<String, (String, String, Option<Value>)>, // id -> (kind, entityId, snapshot|None=revoked)
    force_status: Option<u16>,
    base: String,
    /// 0 = default 5 MB (PRODUCT §8).
    body_limit: usize,
    /// 0 = default 512 KB (contract C2).
    entity_limit: usize,
    max_body_seen: usize,
    quota_full: bool,
    push_calls: usize,
}

#[derive(Clone, Default)]
struct MockWorker(Arc<Mutex<WorkerState>>);

fn err(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({"error": {"code": code, "message": code}}))
}

impl Respond for MockWorker {
    fn respond(&self, req: &WmRequest) -> ResponseTemplate {
        let mut st = self.0.lock().unwrap();
        if let Some(s) = st.force_status {
            return err(s, "forced");
        }
        let path = req.url.path().to_string();
        let m = req.method.as_str().to_string();
        let body_limit = if st.body_limit == 0 {
            5 * 1024 * 1024
        } else {
            st.body_limit
        };
        st.max_body_seen = st.max_body_seen.max(req.body.len());
        if req.body.len() > body_limit {
            return err(413, "payload_too_large");
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        if m == "POST" && path == "/v1/workspaces" {
            st.token = format!("rh_{}", new_id());
            return ResponseTemplate::new(201)
                .set_body_json(json!({"workspaceId": "remote-ws", "token": st.token}));
        }
        if let Some(id) = path
            .strip_prefix("/s/")
            .and_then(|p| p.strip_suffix(".json"))
        {
            return match st.shares.get(id) {
                Some((kind, _, Some(snap))) => ResponseTemplate::new(200)
                    .set_body_json(json!({"shareId": id, "kind": kind, "snapshot": snap})),
                Some((_, _, None)) => err(410, "gone"),
                None => err(404, "not_found"),
            };
        }
        let auth = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if st.token.is_empty() || auth != format!("Bearer {}", st.token) {
            return err(401, "unauthorized");
        }
        match (m.as_str(), path.as_str()) {
            ("GET", "/v1/workspace") => ResponseTemplate::new(200).set_body_json(
                json!({"workspaceId": "remote-ws", "name": "W", "createdAt": 1, "entityCount": st.entities.len()}),
            ),
            ("POST", "/v1/workspace/token/rotate") => {
                st.token = format!("rh_{}", new_id());
                ResponseTemplate::new(200).set_body_json(json!({"token": st.token}))
            }
            ("GET", "/v1/sync") => {
                let q: BTreeMap<String, String> = req.url.query_pairs().into_owned().collect();
                let since: u64 = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
                let limit: usize = q.get("limit").and_then(|s| s.parse().ok()).unwrap_or(500);
                let mut rows: Vec<(&(String, String), &Entity)> = st.entities.iter().filter(|(_, e)| e.rev > since).collect();
                rows.sort_by_key(|(_, e)| e.rev);
                let has_more = rows.len() > limit;
                rows.truncate(limit);
                let cursor = rows.last().map(|(_, e)| e.rev).unwrap_or(st.rev);
                let changes: Vec<Value> = rows
                    .iter()
                    .map(|((t, id), e)| {
                        json!({"type": t, "id": id, "updatedAt": e.updated_at, "deleted": e.deleted, "data": e.data, "rev": e.rev, "deviceId": e.device_id})
                    })
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({"changes": changes, "cursor": cursor, "hasMore": has_more}))
            }
            ("POST", "/v1/sync") => {
                st.push_calls += 1;
                if st.quota_full {
                    return err(413, "quota_exceeded");
                }
                let mut results = Vec::new();
                for c in body["changes"].as_array().cloned().unwrap_or_default() {
                    let t = c["type"].as_str().unwrap().to_string();
                    assert!(["project", "collection", "folder", "request", "environment"].contains(&t.as_str()));
                    let id = c["id"].as_str().unwrap().to_string();
                    let updated = c["updatedAt"].as_i64().unwrap();
                    let deleted = c["deleted"].as_bool().unwrap();
                    let device = c["deviceId"].as_str().unwrap_or("").to_string();
                    let entity_limit = if st.entity_limit == 0 { 512 * 1024 } else { st.entity_limit };
                    if !deleted && serde_json::to_vec(&c["data"]).unwrap().len() > entity_limit {
                        results.push(json!({"type": t, "id": id, "status": "rejected", "reason": "entity_too_large", "rev": 0}));
                        continue;
                    }
                    st.rev += 1;
                    let rev = st.rev;
                    let e = st.entities.entry((t.clone(), id.clone())).or_default();
                    // LWW by (updatedAt, deviceId) — contract C3.
                    let status = if e.rev > 0 && (e.updated_at, e.device_id.as_str()) > (updated, device.as_str()) {
                        "stale"
                    } else {
                        e.updated_at = updated;
                        e.device_id = device;
                        e.deleted = deleted;
                        e.data = if deleted { None } else { Some(c["data"].clone()) };
                        "applied"
                    };
                    e.rev = rev; // stale pushes also bump the server copy
                    results.push(json!({"type": t, "id": id, "status": status, "rev": rev}));
                }
                ResponseTemplate::new(200).set_body_json(json!({"results": results, "cursor": st.rev}))
            }
            ("POST", "/v1/shares") => {
                let id = format!("S{}", st.shares.len() + 1);
                st.shares.insert(
                    id.clone(),
                    (body["kind"].as_str().unwrap().into(), body["entityId"].as_str().unwrap().into(), Some(body["snapshot"].clone())),
                );
                let url = format!("{}/s/{id}", st.base);
                ResponseTemplate::new(201).set_body_json(json!({"shareId": id, "url": url, "createdAt": 1, "updatedAt": 1}))
            }
            ("GET", "/v1/shares") => {
                let list: Vec<Value> = st
                    .shares
                    .iter()
                    .filter(|(_, s)| s.2.is_some())
                    .map(|(id, s)| json!({"shareId": id, "url": format!("{}/s/{id}", st.base), "kind": s.0, "entityId": s.1, "createdAt": 1, "updatedAt": 2}))
                    .collect();
                ResponseTemplate::new(200).set_body_json(json!({"shares": list}))
            }
            (verb, p) if p.starts_with("/v1/shares/") => {
                let id = p.trim_start_matches("/v1/shares/").to_string();
                let base = st.base.clone();
                let Some(share) = st.shares.get_mut(&id) else { return err(404, "not_found") };
                match verb {
                    "PUT" => {
                        share.2 = Some(body["snapshot"].clone());
                        ResponseTemplate::new(200).set_body_json(json!({"shareId": id, "url": format!("{base}/s/{id}")}))
                    }
                    "DELETE" => {
                        share.2 = None;
                        ResponseTemplate::new(200).set_body_json(json!({"shareId": id, "revokedAt": 5}))
                    }
                    _ => err(405, "method"),
                }
            }
            _ => err(404, "no_route"),
        }
    }
}

async fn worker() -> (MockServer, MockWorker) {
    let server = MockServer::start().await;
    let w = MockWorker::default();
    w.0.lock().unwrap().base = server.uri();
    Mock::given(any())
        .respond_with(w.clone())
        .mount(&server)
        .await;
    (server, w)
}

fn device() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    (dir, store)
}

async fn create_and_connect(server: &MockServer) -> (tempfile::TempDir, Store, String) {
    let (dir, mut store) = device();
    let created = SyncClient::new(&server.uri(), None)
        .unwrap()
        .create_workspace("W")
        .await
        .unwrap();
    assert!(created.token.starts_with("rh_"));
    sync::connect(&mut store, &server.uri(), &created.token)
        .await
        .unwrap();
    (dir, store, created.token)
}

async fn sync_now(store: &mut Store) -> sync::SyncReport {
    let client = sync::client_for(store).unwrap();
    sync::sync_now(&client, store).await.unwrap()
}

fn first_project(s: &Store) -> String {
    s.project_list()[0].id.clone()
}

#[tokio::test]
async fn ac_sync_1_create_workspace_and_connect_with_token() {
    let (server, _w) = worker().await;
    let (_d, store, token) = create_and_connect(&server).await;
    let cfg = store.workspace.sync.clone().unwrap();
    assert_eq!(cfg.remote_workspace_id, "remote-ws");
    assert_eq!(cfg.token, token);
    // Connecting with a wrong token fails with 401 and leaves sync off.
    let (_d2, mut other) = device();
    let e = sync::connect(&mut other, &server.uri(), "rh_wrong")
        .await
        .unwrap_err();
    assert_eq!(e, SyncError::Unauthorized);
    assert!(other.workspace.sync.is_none());
    // Rotation invalidates the old token immediately.
    let client = sync::client_for(&store).unwrap();
    let new_token = client.rotate_token().await.unwrap();
    assert_ne!(new_token, token);
    assert_eq!(
        client.workspace().await.unwrap_err(),
        SyncError::Unauthorized
    );
}

#[tokio::test]
async fn ac_sync_3_two_devices_converge_and_only_entities_sync() {
    let (server, w) = worker().await;
    let (_da, mut a, token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "Users").unwrap();
    let f = a.create_folder(&c.id, None, "Admin").unwrap();
    a.create_request(
        &c.id,
        Some(&f.id),
        Request {
            name: "List".into(),
            url: "https://x".into(),
            ..Request::default()
        },
    )
    .unwrap();
    a.history
        .append(&HistoryEntry {
            id: new_id(),
            timestamp: 1,
            source_request_id: None,
            request: Request::default(),
            resolved_url: "https://x".into(),
            method: "GET".into(),
            response: None,
            error: None,
            ws: None,
            grpc: None,
        })
        .unwrap();
    let rep = sync_now(&mut a).await;
    assert!(rep.pushed >= 5, "{rep:?}"); // project, global env, collection, folder, request
    let types: Vec<String> =
        w.0.lock()
            .unwrap()
            .entities
            .keys()
            .map(|k| k.0.clone())
            .collect();
    assert!(types.iter().all(|t| {
        ["project", "collection", "folder", "request", "environment"].contains(&t.as_str())
    }));
    // Nothing left to push afterwards.
    assert!(sync::collect_push(&a).is_empty());

    let (_db, mut b) = device();
    sync::connect(&mut b, &server.uri(), &token).await.unwrap();
    sync_now(&mut b).await;
    // B's empty "Default" project is replaced by A's.
    assert_eq!(b.project_list().len(), 1);
    let tree = b.project(&pid).unwrap();
    assert_eq!(tree.collections[0].name, "Users");
    assert_eq!(tree.folders.len(), 1);
    assert_eq!(tree.requests[0].name, "List");
    // Exactly one Global environment on B.
    assert_eq!(
        b.environments
            .iter()
            .filter(|e| e.project_id.is_none())
            .count(),
        1
    );
    assert!(b.history.is_empty(), "history is never synced");
}

#[tokio::test]
async fn ac_sync_4_last_write_wins_with_conflict_copy() {
    let (server, _w) = worker().await;
    let (_da, mut a, token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    let r = a
        .create_request(
            &c.id,
            None,
            Request {
                name: "Orig".into(),
                ..Request::default()
            },
        )
        .unwrap();
    sync_now(&mut a).await;
    let (_db, mut b) = device();
    sync::connect(&mut b, &server.uri(), &token).await.unwrap();
    sync_now(&mut b).await;

    // Both edit offline; B's edit is older, A's newer.
    let mut rb = b.request(&r.id).unwrap().clone();
    rb.name = "B edit".into();
    b.save_request(&rb).unwrap();
    let mut ra = a.request(&r.id).unwrap().clone();
    ra.name = "A edit".into();
    a.save_request(&ra).unwrap();
    // force ordering of timestamps
    let set_ts = |s: &mut Store, ts: i64| {
        for t in s.projects.values_mut() {
            for q in t.requests.iter_mut().filter(|q| q.id == r.id) {
                q.updated_at = ts;
            }
        }
    };
    let now = now_ms();
    set_ts(&mut b, now + 1_000);
    set_ts(&mut a, now + 2_000);

    sync_now(&mut a).await; // newer edit lands first
    let rep = sync_now(&mut b).await; // B's push is stale, pull brings A's version
    assert_eq!(rep.conflicts.len(), 1, "{rep:?}");
    assert_eq!(rep.conflicts[0].name, "B edit (conflict)");
    let tree = b.project(&pid).unwrap();
    assert_eq!(
        tree.requests.iter().find(|q| q.id == r.id).unwrap().name,
        "A edit"
    );
    assert!(tree.requests.iter().any(|q| q.name == "B edit (conflict)"));

    // The conflict copy propagates back to A.
    sync_now(&mut a).await;
    assert!(
        a.project(&pid)
            .unwrap()
            .requests
            .iter()
            .any(|q| q.name == "B edit (conflict)")
    );
    // A newer local edit is never overwritten by an older remote one.
    let mut ra = a.request(&r.id).unwrap().clone();
    ra.name = "A again".into();
    a.save_request(&ra).unwrap();
    sync_now(&mut a).await;
    sync_now(&mut b).await;
    assert_eq!(b.request(&r.id).unwrap().name, "A again");
}

#[tokio::test]
async fn ac_sync_5_deletes_propagate_and_cascade() {
    let (server, w) = worker().await;
    let (_da, mut a, token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    let f = a.create_folder(&c.id, None, "F").unwrap();
    let r = a
        .create_request(&c.id, Some(&f.id), Request::default())
        .unwrap();
    sync_now(&mut a).await;
    let (_db, mut b) = device();
    sync::connect(&mut b, &server.uri(), &token).await.unwrap();
    sync_now(&mut b).await;
    assert!(b.request(&r.id).is_some());

    a.delete_collection(&c.id).unwrap();
    assert_eq!(a.sync_state.tombstones.len(), 3);
    sync_now(&mut a).await;
    assert!(
        a.sync_state.tombstones.is_empty(),
        "tombstones kept only until pushed"
    );
    assert!(
        w.0.lock()
            .unwrap()
            .entities
            .get(&("collection".into(), c.id.clone()))
            .unwrap()
            .deleted
    );

    let rep = sync_now(&mut b).await;
    assert!(rep.deleted >= 1);
    assert!(b.collection(&c.id).is_none());
    assert!(b.request(&r.id).is_none());
    assert!(b.project(&pid).unwrap().folders.is_empty());

    // A deleted parent removes children even if only the parent tombstone arrives.
    let c2 = a.create_collection(&pid, "C2").unwrap();
    let r2 = a.create_request(&c2.id, None, Request::default()).unwrap();
    sync_now(&mut a).await;
    sync_now(&mut b).await;
    assert!(b.request(&r2.id).is_some());
    {
        let mut st = w.0.lock().unwrap();
        st.rev += 1;
        let rev = st.rev;
        let e = st
            .entities
            .get_mut(&("collection".into(), c2.id.clone()))
            .unwrap();
        e.deleted = true;
        e.data = None;
        e.updated_at = now_ms() + 10_000;
        e.rev = rev;
    }
    sync_now(&mut b).await;
    assert!(b.request(&r2.id).is_none());
}

#[tokio::test]
async fn ac_sync_6_offline_and_5xx_are_retryable_and_harmless() {
    let (server, w) = worker().await;
    let (_d, mut a, _token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    a.create_collection(&pid, "Local").unwrap();
    w.0.lock().unwrap().force_status = Some(503);
    let client = sync::client_for(&a).unwrap();
    let e = sync::sync_now(&client, &mut a).await.unwrap_err();
    assert!(matches!(e, SyncError::Server(_)), "{e:?}");
    assert!(e.is_retryable());
    assert_eq!(a.project(&pid).unwrap().collections.len(), 1);
    assert!(!sync::collect_push(&a).is_empty(), "changes stay queued");

    // Unreachable worker -> Offline.
    let port = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap().port()
    };
    let offline = SyncClient::new(&format!("http://127.0.0.1:{port}"), Some("rh_x")).unwrap();
    let e = sync::sync_now(&offline, &mut a).await.unwrap_err();
    assert!(matches!(e, SyncError::Offline(_)), "{e:?}");
    assert!(e.is_retryable());

    w.0.lock().unwrap().force_status = None;
    sync_now(&mut a).await;
    assert!(sync::collect_push(&a).is_empty());
}

#[tokio::test]
async fn ac_sync_7_401_disconnects_and_keeps_local_data() {
    let (server, w) = worker().await;
    let (_d, mut a, _token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    a.create_collection(&pid, "Keep me").unwrap();
    w.0.lock().unwrap().token = "rh_rotated_elsewhere".into();
    let client = sync::client_for(&a).unwrap();
    let e = sync::sync_now(&client, &mut a).await.unwrap_err();
    assert_eq!(e, SyncError::Unauthorized);
    assert!(!e.is_retryable());
    sync::disconnect(&mut a).unwrap();
    assert!(a.workspace.sync.is_none());
    assert_eq!(a.project(&pid).unwrap().collections[0].name, "Keep me");
    assert_eq!(sync::client_for(&a).err(), Some(SyncError::NotConnected));
}

#[tokio::test]
async fn ac_env_7_secret_values_never_leave_the_device() {
    let (server, w) = worker().await;
    let (_da, mut a, token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let mut env = a.create_environment(Some(&pid), "Prod").unwrap();
    env.variables = vec![
        Variable::new("host", "prod.dev"),
        Variable::new("token", "TOP-SECRET").secret(),
    ];
    a.update_environment(&env).unwrap();
    sync_now(&mut a).await;
    let stored =
        w.0.lock()
            .unwrap()
            .entities
            .get(&("environment".into(), env.id.clone()))
            .unwrap()
            .data
            .clone()
            .unwrap();
    assert!(!stored.to_string().contains("TOP-SECRET"));
    assert_eq!(stored["variables"][1]["secret"], true);

    let (_db, mut b) = device();
    sync::connect(&mut b, &server.uri(), &token).await.unwrap();
    sync_now(&mut b).await;
    let benv = b.environment(&env.id).unwrap().clone();
    assert_eq!(benv.variables[1].value, "");
    // B fills in its own secret; a later pull of a non-secret edit keeps it.
    let mut benv2 = benv.clone();
    benv2.variables[1].value = "B-SECRET".into();
    b.update_environment(&benv2).unwrap();
    sync_now(&mut b).await;
    let mut aenv = a.environment(&env.id).unwrap().clone();
    aenv.variables[0].value = "prod2.dev".into();
    a.update_environment(&aenv).unwrap();
    let ts = now_ms() + 5_000;
    a.environments
        .iter_mut()
        .find(|e| e.id == env.id)
        .unwrap()
        .updated_at = ts;
    sync_now(&mut a).await;
    sync_now(&mut b).await;
    let benv = b.environment(&env.id).unwrap();
    assert_eq!(benv.variables[0].value, "prod2.dev");
    assert_eq!(benv.variables[1].value, "B-SECRET");
    assert_eq!(
        a.environment(&env.id).unwrap().variables[1].value,
        "TOP-SECRET"
    );
}

#[tokio::test]
async fn ac_share_1_4_5_6_create_list_update_revoke_import() {
    let (server, _w) = worker().await;
    let (_d, mut a, _token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "Shared API").unwrap();
    a.create_request(
        &c.id,
        None,
        Request {
            name: "Get".into(),
            headers: vec![KeyValue::new("Authorization", "Bearer real")],
            ..Request::default()
        },
    )
    .unwrap();
    let client = sync::client_for(&a).unwrap();
    let (snap, _) =
        build_snapshot(&a, ShareKind::Collection, &c.id, &ShareOptions::default()).unwrap();
    let share = client
        .create_share(ShareKind::Collection, &c.id, &snap)
        .await
        .unwrap();
    assert!(share.url.ends_with(&format!("/s/{}", share.share_id)));
    // QA-R2-003: name/kind/entityId are always populated.
    assert_eq!(share.name.as_deref(), Some("Shared API"));
    let list = client.list_shares().await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].entity_id.as_deref(), Some(c.id.as_str()));

    // Import from link into the current project (new ids).
    let public = SyncClient::fetch_public_share(&share.url).await.unwrap();
    assert_eq!(public.kind, ShareKind::Collection);
    assert!(!public.snapshot.to_string().contains("Bearer real"));
    let tree = import_snapshot(&mut a, &pid, public.kind, &public.snapshot).unwrap();
    assert_eq!(tree.collections.len(), 2);

    // Update keeps the URL.
    let mut renamed = a.collection(&c.id).unwrap().clone();
    renamed.name = "Renamed API".into();
    a.update_collection(&renamed).unwrap();
    let (snap2, _) =
        build_snapshot(&a, ShareKind::Collection, &c.id, &ShareOptions::default()).unwrap();
    let updated = client.update_share(&share.share_id, &snap2).await.unwrap();
    assert_eq!(updated.kind, Some(ShareKind::Collection));
    assert_eq!(updated.entity_id.as_deref(), Some(c.id.as_str()));
    assert_eq!(updated.name.as_deref(), Some("Renamed API"));
    let public = SyncClient::fetch_public_share(&share.url).await.unwrap();
    assert_eq!(public.snapshot["collection"]["name"], "Renamed API");

    // Revoke -> 410 Gone.
    client.revoke_share(&share.share_id).await.unwrap();
    assert!(matches!(
        SyncClient::fetch_public_share(&share.url).await,
        Err(SyncError::Gone(_))
    ));
    assert!(client.list_shares().await.unwrap().is_empty());
}

/// Insert many requests with a single save (fast test setup).
fn bulk_requests(s: &mut Store, pid: &str, cid: &str, n: usize, kb: usize) {
    let tree = s.projects.get_mut(pid).unwrap();
    let now = now_ms();
    for i in 0..n {
        let mut r = big_request(&format!("r{i}"), kb);
        r.collection_id = cid.to_string();
        r.sort_order = i as i64;
        r.created_at = now;
        r.updated_at = now;
        tree.requests.push(r);
    }
    s.save_project(pid).unwrap();
}

fn big_request(name: &str, kb: usize) -> Request {
    Request {
        name: name.into(),
        method: "POST".into(),
        url: "https://x.dev".into(),
        body: Body::json(format!("{{\"blob\":\"{}\"}}", "x".repeat(kb * 1024))),
        ..Request::default()
    }
}

/// Contract C1 / QA-R1-004: batches are bounded by bytes, not just count.
#[tokio::test]
async fn qa_r1_004_push_batches_stay_under_body_limit() {
    let (server, w) = worker().await;
    let (_d, mut a, _t) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "Big").unwrap();
    bulk_requests(&mut a, &pid, &c.id, 500, 12);
    let rep = sync_now(&mut a).await;
    assert!(rep.pushed >= 502, "{rep:?}");
    assert!(sync::collect_push(&a).is_empty());
    let max = w.0.lock().unwrap().max_body_seen;
    assert!(max <= sync::MAX_PUSH_BYTES, "largest push body {max}");
}

/// Contract C1: a server 413 (smaller limit than ours) splits the batch.
#[tokio::test]
async fn qa_r1_004_server_413_splits_batches() {
    let (server, w) = worker().await;
    let (_d, mut a, _t) = create_and_connect(&server).await;
    w.0.lock().unwrap().body_limit = 200 * 1024;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    bulk_requests(&mut a, &pid, &c.id, 40, 20);
    let rep = sync_now(&mut a).await;
    assert!(rep.pushed >= 42, "{rep:?}");
    assert!(rep.too_large.is_empty());
    // A single change bigger than the body limit is reported, not looped on.
    a.create_request(&c.id, None, big_request("huge", 300))
        .unwrap();
    let rep = sync_now(&mut a).await;
    assert_eq!(rep.too_large.len(), 1, "{rep:?}");
    assert!(
        sync::collect_push(&a).is_empty(),
        "rejected change must not be resent"
    );
}

/// Contract C2 / QA-R1-005: an oversized entity is skipped locally and
/// reported; everything else still syncs; no retry loop.
#[tokio::test]
async fn qa_r1_005_oversized_entity_skipped_others_sync() {
    let (server, w) = worker().await;
    let (_d, mut a, _t) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "Mixed").unwrap();
    let small = a
        .create_request(
            &c.id,
            None,
            Request {
                name: "small".into(),
                ..Request::default()
            },
        )
        .unwrap();
    let big = a
        .create_request(&c.id, None, big_request("big", 600))
        .unwrap();
    let rep = sync_now(&mut a).await;
    assert_eq!(rep.too_large.len(), 1, "{rep:?}");
    assert_eq!(rep.too_large[0].id, big.id);
    assert_eq!(rep.too_large[0].reason, "too_large");
    {
        let st = w.0.lock().unwrap();
        assert!(
            st.entities
                .contains_key(&("request".into(), small.id.clone()))
        );
        assert!(
            !st.entities
                .contains_key(&("request".into(), big.id.clone())),
            "never pre-sent"
        );
    }
    assert!(sync::collect_push(&a).is_empty() || sync::plan_push(&mut a).is_empty());
    assert_eq!(a.sync_state.sync_skipped.len(), 1);
    // Shrinking the request makes it sync again.
    let mut shrunk = a.request(&big.id).unwrap().clone();
    shrunk.body = Body::json("{}");
    a.save_request(&shrunk).unwrap();
    let rep = sync_now(&mut a).await;
    assert!(rep.too_large.is_empty(), "{rep:?}");
    assert!(
        w.0.lock()
            .unwrap()
            .entities
            .contains_key(&("request".into(), big.id.clone()))
    );
}

/// Contract C2: a server-side per-entity `rejected` result is handled like a
/// local skip and never retried in a loop.
#[tokio::test]
async fn qa_r1_005_server_rejected_result_marks_skipped() {
    let (server, w) = worker().await;
    let (_d, mut a, _t) = create_and_connect(&server).await;
    w.0.lock().unwrap().entity_limit = 50 * 1024;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    let mid = a
        .create_request(&c.id, None, big_request("mid", 100))
        .unwrap();
    let ok = a.create_request(&c.id, None, Request::default()).unwrap();
    let rep = sync_now(&mut a).await;
    assert_eq!(
        rep.too_large
            .iter()
            .map(|s| s.id.clone())
            .collect::<Vec<_>>(),
        vec![mid.id.clone()]
    );
    assert!(
        w.0.lock()
            .unwrap()
            .entities
            .contains_key(&("request".into(), ok.id.clone()))
    );
    assert!(sync::collect_push(&a).is_empty());
}

/// Contract C3 / QA-R1-026: equal updatedAt converges on the greater deviceId.
#[tokio::test]
async fn qa_r1_026_equal_updated_at_converges_by_device_id() {
    let (server, _w) = worker().await;
    let (_da, mut a, token) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "Tie").unwrap();
    let r = a
        .create_request(
            &c.id,
            None,
            Request {
                name: "tie".into(),
                ..Request::default()
            },
        )
        .unwrap();
    sync_now(&mut a).await;
    let (_db, mut b) = device();
    sync::connect(&mut b, &server.uri(), &token).await.unwrap();
    sync_now(&mut b).await;
    assert_ne!(a.sync_state.device_id, b.sync_state.device_id);
    let ts = now_ms() + 10_000;
    for (store, name) in [(&mut a, "from A"), (&mut b, "from B")] {
        let tree = store.projects.get_mut(&pid).unwrap();
        let q = tree.requests.iter_mut().find(|x| x.id == r.id).unwrap();
        q.name = name.into();
        q.updated_at = ts;
        store.save_project(&pid).unwrap();
    }
    sync_now(&mut a).await;
    sync_now(&mut b).await;
    sync_now(&mut a).await;
    let na = a.request(&r.id).unwrap().name.clone();
    let nb = b.request(&r.id).unwrap().name.clone();
    assert_eq!(na, nb);
    let expected = if a.sync_state.device_id > b.sync_state.device_id {
        "from A"
    } else {
        "from B"
    };
    assert_eq!(na, expected);
}

/// QA-R1-028: a corrupt sync-state.json falls back to its backup, keeping
/// pending tombstones and the device id.
#[tokio::test]
async fn qa_r1_028_corrupt_sync_state_keeps_tombstones() {
    let (server, _w) = worker().await;
    let (dir, mut a, _t) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    sync_now(&mut a).await;
    a.delete_collection(&c.id).unwrap();
    let device = a.sync_state.device_id.clone();
    assert_eq!(a.sync_state.tombstones.len(), 1);
    drop(a);
    std::fs::write(dir.path().join("sync-state.json"), b"{ corrupt").unwrap();
    let a = Store::open(dir.path()).unwrap();
    assert_eq!(a.sync_state.tombstones.len(), 1);
    assert_eq!(a.sync_state.device_id, device);
    assert_eq!(a.corrupt.len(), 1);
}

/// Worker 413 `quota_exceeded` stops the sync without splitting/retrying.
#[tokio::test]
async fn quota_exceeded_is_not_split_or_retried() {
    let (server, w) = worker().await;
    let (_d, mut a, _t) = create_and_connect(&server).await;
    let pid = first_project(&a);
    let c = a.create_collection(&pid, "C").unwrap();
    bulk_requests(&mut a, &pid, &c.id, 20, 1);
    w.0.lock().unwrap().quota_full = true;
    let client = sync::client_for(&a).unwrap();
    let e = sync::sync_now(&client, &mut a).await.unwrap_err();
    assert!(matches!(e, SyncError::QuotaExceeded(_)), "{e:?}");
    assert!(!e.is_retryable());
    assert_eq!(
        w.0.lock().unwrap().push_calls,
        1,
        "no splitting on quota errors"
    );
    assert!(!sync::collect_push(&a).is_empty(), "changes stay queued");
}

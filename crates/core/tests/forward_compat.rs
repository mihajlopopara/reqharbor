//! Forward compatibility (accounts plan M0; product AC-FWD-1..5, tech §9):
//! unknown JSON fields survive load -> edit -> save at every level, unknown
//! request kinds / body types are read-only and never sent or pushed when
//! modified, and pulled changes of unknown sync types are kept, not fatal.

use proptest::prelude::*;
use reqharbor_core::model::*;
use reqharbor_core::prepare::{PrepareError, prepare};
use reqharbor_core::share::{ShareOptions, build_snapshot, import_snapshot};
use reqharbor_core::storage::{NodeKind, Store, StoreError, sync_key};
use reqharbor_core::sync::{self, ShareKind, SyncClient};
use reqharbor_core::vars::VarScope;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, query_param};
use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate};

fn read_json(path: &std::path::Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn write_json(path: &std::path::Path, v: &Value) {
    std::fs::write(path, serde_json::to_vec_pretty(v).unwrap()).unwrap();
}

/// Remove every key a pre-M0 UI would not know about, recursively, the way
/// a UI that rebuilds objects from its own types would.
fn strip_extras(r: &mut Request) {
    r.extra.clear();
    r.auth.extra.clear();
    if let Some(k) = r.auth.api_key.as_mut() {
        k.extra.clear();
    }
    r.settings.extra.clear();
    for kv in r.params.iter_mut().chain(r.headers.iter_mut()) {
        kv.extra.clear();
    }
    r.body.extra.clear();
    if let Some(g) = r.body.graphql.as_mut() {
        g.extra.clear();
    }
    for kv in r.body.form_encoded.iter_mut().flatten() {
        kv.extra.clear();
    }
    for f in r.body.form.iter_mut().flatten() {
        f.extra.clear();
    }
}

/// A request JSON carrying an unknown key at every nested level.
fn rich_request_json(id: &str, collection_id: &str) -> Value {
    json!({
        "id": id,
        "collectionId": collection_id,
        "folderId": null,
        "name": "Rich",
        "method": "POST",
        "url": "https://api.example.com/x?q=1",
        "params": [{"key": "q", "value": "1", "enabled": true, "fxParam": "p"}],
        "headers": [
            {"key": "A", "value": "1", "enabled": true, "fxHeader": {"n": 1}},
            {"key": "B", "value": "2", "enabled": true}
        ],
        "auth": {
            "type": "apikey",
            "apiKey": {"key": "k", "value": "v", "in": "header", "fxApiKey": true},
            "fxAuth": [1, 2]
        },
        "body": {
            "type": "graphql",
            "graphql": {"query": "{a}", "variables": "", "fxOperation": "Op"},
            "form": [{"key": "f", "value": "v", "kind": "text", "enabled": true, "fxForm": 1}],
            "formEncoded": [{"key": "e", "value": "1", "enabled": true, "fxEnc": 2}],
            "fxBody": "b"
        },
        "settings": {"followRedirects": true, "timeoutMs": 1000, "verifyTls": true, "fxRetries": 3},
        "sortOrder": 0,
        "createdAt": 1,
        "updatedAt": 2,
        "fxTopLevel": {"nested": ["kept"]}
    })
}

/// Paths (JSON pointers) of the unknown keys in [`rich_request_json`].
const REQUEST_EXTRA_POINTERS: [&str; 10] = [
    "/fxTopLevel",
    "/params/0/fxParam",
    "/headers/0/fxHeader",
    "/auth/fxAuth",
    "/auth/apiKey/fxApiKey",
    "/body/fxBody",
    "/body/graphql/fxOperation",
    "/body/form/0/fxForm",
    "/body/formEncoded/0/fxEnc",
    "/settings/fxRetries",
];

fn assert_request_extras(v: &Value) {
    for p in REQUEST_EXTRA_POINTERS {
        assert!(v.pointer(p).is_some(), "lost {p} in {v:#}");
    }
    assert_eq!(v["fxTopLevel"], json!({"nested": ["kept"]}));
}

struct Fixture {
    dir: tempfile::TempDir,
    pid: String,
    cid: String,
    fid: String,
    rid: String,
}

/// A store whose files on disk carry unknown keys everywhere.
fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let (pid, cid, fid, rid);
    {
        let mut s = Store::open(dir.path()).unwrap();
        pid = s.project_list()[0].id.clone();
        let c = s.create_collection(&pid, "C").unwrap();
        cid = c.id.clone();
        fid = s.create_folder(&cid, None, "F").unwrap().id;
        rid = new_id();
        let pfile = dir.path().join(format!("projects/{pid}.json"));
        let mut v = read_json(&pfile);
        v["fxProjectFile"] = json!("ignored-ok");
        v["project"]["fxProject"] = json!({"color": "teal"});
        v["collections"][0]["fxCollection"] = json!(7);
        v["collections"][0]["auth"]["fxCollAuth"] = json!("a");
        v["folders"][0]["fxFolder"] = json!(null);
        v["requests"] = json!([rich_request_json(&rid, &cid)]);
        write_json(&pfile, &v);

        let wfile = dir.path().join("workspace.json");
        let mut w = read_json(&wfile);
        w["workspace"]["fxWorkspace"] = json!({"accountHint": "x"});
        w["fxWorkspaceFile"] = json!(1);
        write_json(&wfile, &w);

        s.save_settings().unwrap();
        let setfile = dir.path().join("settings.json");
        let mut st = read_json(&setfile);
        st["fxSettingsFile"] = json!(true);
        write_json(&setfile, &st);

        let sfile = dir.path().join("sync-state.json");
        let mut st = read_json(&sfile);
        st["fxSyncState"] = json!({"spaces": [1, 2]});
        write_json(&sfile, &st);

        let efile = dir.path().join("environments.json");
        let mut e = read_json(&efile);
        e["fxEnvironmentsFile"] = json!("x");
        e["environments"][0]["fxEnv"] = json!("e");
        e["environments"][0]["variables"] =
            json!([{"key": "host", "value": "h", "enabled": true, "secret": false, "fxVar": 1}]);
        write_json(&efile, &e);
    }
    Fixture {
        dir,
        pid,
        cid,
        fid,
        rid,
    }
}

#[test]
fn ac_fwd_1_unknown_fields_survive_load_edit_save_at_every_level() {
    let fx = fixture();
    let d = fx.dir.path();
    {
        let mut s = Store::open(d).unwrap();
        assert!(s.corrupt.is_empty(), "{:?}", s.corrupt);
        // Edit everything through the normal mutators.
        s.rename_project(&fx.pid, "Renamed").unwrap();
        let mut c = s.collection(&fx.cid).unwrap().clone();
        c.name = "C2".into();
        c.extra.clear();
        c.auth.extra.clear();
        s.update_collection(&c).unwrap();
        s.rename_folder(&fx.fid, "F2").unwrap();
        let mut r = s.request(&fx.rid).unwrap().clone();
        strip_extras(&mut r);
        r.url = "https://api.example.com/y".into();
        let saved = s.save_request(&r).unwrap();
        assert_request_extras(&serde_json::to_value(&saved).unwrap());
        let mut g = s.global_environment().unwrap().clone();
        g.extra.clear();
        for v in &mut g.variables {
            v.extra.clear();
        }
        g.variables[0].value = "h2".into();
        s.update_environment(&g).unwrap();
        s.workspace.name = "WS".into();
        s.save_all().unwrap();
        s.settings.sidebar_width = 333;
        s.save_settings().unwrap();
    }
    // Reopen: every unknown key is still on disk after the edits.
    let s = Store::open(d).unwrap();
    assert!(s.corrupt.is_empty());
    let p = read_json(&d.join(format!("projects/{}.json", fx.pid)));
    assert_eq!(
        p["fxProjectFile"], "ignored-ok",
        "file-level key of the project file"
    );
    assert_eq!(p["schemaVersion"], 1);
    let raw_project = std::fs::read_to_string(d.join(format!("projects/{}.json", fx.pid))).unwrap();
    assert_eq!(raw_project.matches("\"schemaVersion\"").count(), 1);
    assert_eq!(p["project"]["name"], "Renamed");
    assert_eq!(p["project"]["fxProject"], json!({"color": "teal"}));
    assert_eq!(p["collections"][0]["name"], "C2");
    assert_eq!(p["collections"][0]["fxCollection"], 7);
    assert_eq!(p["collections"][0]["auth"]["fxCollAuth"], "a");
    assert_eq!(p["folders"][0]["name"], "F2");
    assert!(p["folders"][0].get("fxFolder").is_some());
    assert_eq!(p["requests"][0]["url"], "https://api.example.com/y");
    assert_request_extras(&p["requests"][0]);

    let w = read_json(&d.join("workspace.json"));
    assert_eq!(w["fxWorkspaceFile"], 1, "file-level key of workspace.json");
    assert_eq!(w["workspace"]["fxWorkspace"], json!({"accountHint": "x"}));
    assert_eq!(w["workspace"]["name"], "WS");

    let raw_state = std::fs::read_to_string(d.join("sync-state.json")).unwrap();
    let st: Value = serde_json::from_str(&raw_state).unwrap();
    assert_eq!(st["fxSyncState"], json!({"spaces": [1, 2]}));
    assert_eq!(st["schemaVersion"], 1);
    // The file-level key is consumed by the wrapper, never copied into extra.
    assert_eq!(raw_state.matches("\"schemaVersion\"").count(), 1);

    let st = read_json(&d.join("settings.json"));
    assert_eq!(
        st["fxSettingsFile"], true,
        "file-level key of settings.json"
    );
    let e = read_json(&d.join("environments.json"));
    assert_eq!(
        e["fxEnvironmentsFile"], "x",
        "file-level key of environments.json"
    );
    assert_eq!(e["environments"][0]["fxEnv"], "e");
    assert_eq!(e["environments"][0]["variables"][0]["fxVar"], 1);
    assert_eq!(e["environments"][0]["variables"][0]["value"], "h2");
}

#[test]
fn known_keys_are_never_duplicated_into_extra() {
    let r: Request = serde_json::from_value(rich_request_json("r", "c")).unwrap();
    for k in [
        "id", "kind", "body", "auth", "settings", "params", "headers",
    ] {
        assert!(!r.extra.contains_key(k), "{k} leaked into extra");
    }
    assert!(!r.body.extra.contains_key("type"));
    assert!(!r.auth.extra.contains_key("type"));
    assert!(!r.auth.api_key.as_ref().unwrap().extra.contains_key("in"));
    assert_eq!(r.extra.len(), 1);
    // Serialized output has each key exactly once.
    let text = serde_json::to_string(&r).unwrap();
    assert_eq!(text.matches("\"type\":\"graphql\"").count(), 1);
    assert_eq!(text.matches("\"fxTopLevel\"").count(), 1);
}

#[test]
fn ac_fwd_2_kind_defaults_to_http_and_is_omitted() {
    let r: Request = serde_json::from_str(r#"{"id":"x"}"#).unwrap();
    assert_eq!(r.kind, RequestKind::Http);
    assert!(r.is_editable());
    let v = serde_json::to_value(&r).unwrap();
    assert!(v.get("kind").is_none(), "http kind must be omitted: {v}");
    assert!(v.get("minClientVersion").is_none());
    for (s, k) in [
        ("graphql", RequestKind::Graphql),
        ("grpc", RequestKind::Grpc),
        ("socketio", RequestKind::Other("socketio".into())),
    ] {
        let r: Request = serde_json::from_value(json!({"id": "x", "kind": s})).unwrap();
        assert_eq!(r.kind, k);
        assert!(!r.is_editable(), "{s} must be read-only");
        assert_eq!(serde_json::to_value(&r).unwrap()["kind"], s);
    }
    // WebSocket requests are editable in this build (AC-WS-1).
    let r: Request = serde_json::from_value(json!({"id": "x", "kind": "ws"})).unwrap();
    assert_eq!(r.kind, RequestKind::Ws);
    assert!(r.is_editable());
    assert_eq!(serde_json::to_value(&r).unwrap()["kind"], "ws");
}

#[test]
fn ac_ws_1_ws_config_defaults_round_trip_and_unknown_parts_are_read_only() {
    let raw = json!({
        "id": "x", "kind": "ws", "minClientVersion": WS_MIN_CLIENT_VERSION,
        "ws": {
            "messages": [{"name": "Hi", "kind": "json", "body": "{}", "fxMsg": 1}],
            "socketIo": {"namespace": "/chat", "fxSio": true},
            "fxWs": [1]
        }
    });
    let r: Request = serde_json::from_value(raw).unwrap();
    assert!(r.is_editable());
    let ws = r.ws.as_ref().unwrap();
    assert!(ws.protocols.is_empty() && !ws.auto_reconnect);
    let sio = ws.socket_io.as_ref().unwrap();
    assert_eq!(
        (sio.path.as_str(), sio.namespace.as_str(), sio.version),
        ("/socket.io/", "/chat", 4)
    );
    assert_eq!(ws.messages[0].kind, MessageKind::Json);
    let v = serde_json::to_value(&r).unwrap();
    assert_eq!(v["ws"]["fxWs"], json!([1]));
    assert_eq!(v["ws"]["messages"][0]["fxMsg"], 1);
    assert_eq!(v["ws"]["socketIo"]["fxSio"], true);
    assert!(
        v["ws"]["messages"][0].get("ack").is_none(),
        "false ack omitted"
    );
    // A message kind or Socket.IO version from a newer ReqHarbor: read-only.
    let mut newer = r.clone();
    newer.ws.as_mut().unwrap().messages[0].kind = MessageKind::Other("cbor".into());
    assert!(!newer.is_editable());
    let mut newer = r.clone();
    newer
        .ws
        .as_mut()
        .unwrap()
        .socket_io
        .as_mut()
        .unwrap()
        .version = 5;
    assert!(!newer.is_editable());
    // The stamp: ws requests carry this build's version, http ones nothing.
    let mut stamped: Request = serde_json::from_value(json!({"id": "y", "kind": "ws"})).unwrap();
    stamped.stamp_min_client_version();
    assert_eq!(
        stamped.min_client_version.as_deref(),
        Some(WS_MIN_CLIENT_VERSION)
    );
    let mut kept: Request =
        serde_json::from_value(json!({"id": "y", "kind": "ws", "minClientVersion": "0.0.1"}))
            .unwrap();
    kept.stamp_min_client_version();
    assert_eq!(kept.min_client_version.as_deref(), Some(WS_MIN_CLIENT_VERSION));
    // Never the running version: a newer stamp is kept, the WS one isn't bumped
    // to this build (a later patch build mustn't lock out older WS builds).
    let mut newer: Request =
        serde_json::from_value(json!({"id": "y", "kind": "ws", "minClientVersion": "99.0.0"}))
            .unwrap();
    newer.stamp_min_client_version();
    assert_eq!(newer.min_client_version.as_deref(), Some("99.0.0"));
    assert_eq!(WS_MIN_CLIENT_VERSION, "0.1.0");
    let mut http: Request = serde_json::from_value(json!({"id": "z"})).unwrap();
    http.stamp_min_client_version();
    assert!(http.min_client_version.is_none());
}

#[test]
fn ac_ws_7_session_summary_keeps_unknown_keys() {
    let raw = json!({
        "id": "h", "timestamp": 1, "sourceRequestId": null,
        "request": {"id": "r", "kind": "ws"}, "resolvedUrl": "ws://a/", "method": "WS",
        "ws": {"durationMs": 5, "sent": 1, "received": 2, "fxFrames": {"n": 3}}
    });
    let h: HistoryEntry = serde_json::from_value(raw).unwrap();
    let ws = h.ws.as_ref().unwrap();
    assert_eq!((ws.duration_ms, ws.sent, ws.received), (5, 1, 2));
    let v = serde_json::to_value(&h).unwrap();
    assert_eq!(v["ws"]["fxFrames"], json!({"n": 3}));
    assert!(v["ws"].get("closeCode").is_none());
}

#[test]
fn ac_ws_1_store_stamps_and_keeps_ws_on_save() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let r: Request = serde_json::from_value(json!({
        "id": "w", "kind": "ws", "url": "ws://echo",
        "ws": {"messages": [{"name": "m", "body": "hi", "fxMsg": 2}], "fxWs": 1}
    }))
    .unwrap();
    let created = s.create_request(&c.id, None, r).unwrap();
    assert_eq!(
        created.min_client_version.as_deref(),
        Some(WS_MIN_CLIENT_VERSION)
    );
    // A UI that dropped the unknown keys (or `ws` entirely) loses nothing.
    let mut edited = created.clone();
    edited.url = "ws://echo2".into();
    edited.ws.as_mut().unwrap().extra.clear();
    edited.ws.as_mut().unwrap().messages[0].extra.clear();
    let saved = s.save_request(&edited).unwrap();
    let v = serde_json::to_value(&saved).unwrap();
    assert_eq!(v["ws"]["fxWs"], 1);
    assert_eq!(v["ws"]["messages"][0]["fxMsg"], 2);
    let mut dropped = saved.clone();
    dropped.ws = None;
    let saved = s.save_request(&dropped).unwrap();
    assert_eq!(saved.ws.unwrap().messages[0].body, "hi");
}

#[test]
fn ac_fwd_3_unknown_body_type_round_trips_byte_equal() {
    let raw = r#"{"type":"protobuf","text":"x","fxSchema":{"b":[1,2,{"c":null}]},"aFirst":true}"#;
    let b: Body = serde_json::from_str(raw).unwrap();
    assert_eq!(b.body_type, BodyType::Other("protobuf".into()));
    assert_eq!(serde_json::to_string(&b).unwrap(), raw);
    // Known types keep their camelCase names.
    for t in [
        "none",
        "json",
        "xml",
        "text",
        "form",
        "formEncoded",
        "graphql",
        "binary",
    ] {
        let b: Body = serde_json::from_value(json!({ "type": t })).unwrap();
        assert!(b.body_type.is_known(), "{t}");
        assert_eq!(serde_json::to_value(&b).unwrap()["type"], t);
    }
    let r: Request =
        serde_json::from_value(json!({"id": "x", "body": {"type": "protobuf"}})).unwrap();
    assert!(!r.is_editable());
}

#[test]
fn ac_fwd_4_min_client_version() {
    let with = |v: &str| -> Request {
        serde_json::from_value(json!({"id": "x", "minClientVersion": v})).unwrap()
    };
    assert!(with(CLIENT_VERSION).is_editable());
    assert!(with("0.0.1").is_editable());
    assert!(!with("99.0.0").is_editable());
    assert!(!with("0.1.999").is_editable());
    assert!(
        !with("not-a-version").is_editable(),
        "unparseable counts as newer"
    );
    let r = with("99.0.0");
    assert_eq!(
        serde_json::to_value(&r).unwrap()["minClientVersion"],
        "99.0.0"
    );
}

#[test]
fn non_http_requests_are_never_sent() {
    let mut r: Request = serde_json::from_value(json!({
        "id": "x", "url": "https://a.dev", "kind": "grpc",
        "grpc": {"service": "S", "method": "M"}
    }))
    .unwrap();
    let err = prepare(&r, &VarScope::empty(), None).unwrap_err();
    assert_eq!(err, PrepareError::NeedsNewerVersion);
    assert_eq!(
        err.to_string(),
        "This request needs a newer version of ReqHarbor"
    );
    r.kind = RequestKind::Http;
    r.body.body_type = BodyType::Other("protobuf".into());
    assert_eq!(
        prepare(&r, &VarScope::empty(), None).unwrap_err(),
        PrepareError::NeedsNewerVersion
    );
    r.body.body_type = BodyType::None;
    assert!(prepare(&r, &VarScope::empty(), None).is_ok());
}

fn grpc_request(id: &str, collection_id: &str) -> Value {
    json!({
        "id": id, "collectionId": collection_id, "name": "Say hello",
        "kind": "grpc", "method": "POST", "url": "grpc://svc:443",
        "grpc": {"service": "Greeter", "method": "SayHello", "message": "{}"},
        "updatedAt": 5
    })
}

#[test]
fn store_mutators_refuse_read_only_requests() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let r: Request = serde_json::from_value(grpc_request("g1", &c.id)).unwrap();
    s.projects.get_mut(&pid).unwrap().requests.push(r.clone());
    let ro = |e: StoreError| matches!(e, StoreError::ReadOnlyNewer(_));

    let mut edited = r.clone();
    edited.url = "grpc://other".into();
    assert!(ro(s.save_request(&edited).unwrap_err()));
    // A UI that dropped `kind` cannot bypass the guard either.
    let mut sneaky = edited.clone();
    sneaky.kind = RequestKind::Http;
    sneaky.extra.clear();
    assert!(ro(s.save_request(&sneaky).unwrap_err()));
    assert!(ro(s.rename_request("g1", "New").unwrap_err()));
    assert!(ro(s.duplicate_request("g1").unwrap_err()));
    assert!(ro(s
        .move_node(NodeKind::Request, "g1", Some(&c.id), None, 0)
        .unwrap_err()));
    assert!(ro(s.create_request(&c.id, None, r.clone()).unwrap_err()));
    let msg = s.rename_request("g1", "New").unwrap_err().to_string();
    assert!(msg.contains("newer version of ReqHarbor"), "{msg}");
    // Unchanged, then deletable.
    assert_eq!(s.request("g1").unwrap(), &r);
    s.delete_request("g1").unwrap();
    assert!(s.request("g1").is_none());
}

#[test]
fn duplicate_import_and_share_carry_extra() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let r: Request = serde_json::from_value(rich_request_json("r1", &c.id)).unwrap();
    s.projects.get_mut(&pid).unwrap().requests.push(r);
    s.save_project(&pid).unwrap();

    let dup = s.duplicate_request("r1").unwrap();
    assert_request_extras(&serde_json::to_value(&dup).unwrap());
    let tree = s.duplicate_collection(&c.id).unwrap();
    let copies: Vec<&Request> = tree
        .requests
        .iter()
        .filter(|x| x.collection_id != c.id)
        .collect();
    assert_eq!(copies.len(), 2);
    for x in copies {
        assert_request_extras(&serde_json::to_value(x).unwrap());
    }

    // With credentials included the snapshot carries every unknown key...
    let with_creds = ShareOptions {
        include_credentials: true,
        environment_id: None,
    };
    let (snap, _) = build_snapshot(&s, ShareKind::Collection, &c.id, &with_creds).unwrap();
    assert!(
        snap["collection"]["requests"][0]
            .get("fxTopLevel")
            .is_some()
    );
    // ...without them, none: this build can't tell whether they are secret.
    let (bare, preview) =
        build_snapshot(&s, ShareKind::Collection, &c.id, &ShareOptions::default()).unwrap();
    for p in REQUEST_EXTRA_POINTERS {
        assert!(
            bare["collection"]["requests"][0].pointer(p).is_none(),
            "{p} leaked into a share without credentials"
        );
    }
    assert!(preview.credentials_replaced >= 2 * REQUEST_EXTRA_POINTERS.len());
    let before = s.project(&pid).unwrap().requests.len();
    let tree = import_snapshot(&mut s, &pid, ShareKind::Collection, &snap).unwrap();
    assert_eq!(tree.requests.len(), before + 2);
    let imported = tree.requests.last().unwrap();
    assert_request_extras(&serde_json::to_value(imported).unwrap());
    // Unknown collection fields of the snapshot survive too, and the
    // snapshot's folders/requests arrays are not copied into extra.
    let mut snap2 = snap.clone();
    snap2["collection"]["fxShared"] = json!(1);
    let tree = import_snapshot(&mut s, &pid, ShareKind::Collection, &snap2).unwrap();
    let col = tree.collections.last().unwrap();
    assert_eq!(col.extra.get("fxShared"), Some(&json!(1)));
    assert!(!col.extra.contains_key("folders") && !col.extra.contains_key("requests"));
}

/// A store with collection `c`, folder `f` (in `c`) holding the gRPC
/// request `g1` plus an editable request `a1`, and a second collection `c2`.
fn guarded_store() -> (tempfile::TempDir, Store, String, String, String, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap().id;
    let c2 = s.create_collection(&pid, "C2").unwrap().id;
    let f = s.create_folder(&c, None, "F").unwrap().id;
    let mut g: Request = serde_json::from_value(grpc_request("g1", &c)).unwrap();
    g.folder_id = Some(f.clone());
    g.sort_order = 2;
    s.projects.get_mut(&pid).unwrap().requests.push(g);
    s.save_project(&pid).unwrap();
    s.create_request(&c, Some(&f), Request::default()).unwrap();
    (dir, s, pid, c, c2, f)
}

fn is_ro(e: StoreError) -> bool {
    matches!(e, StoreError::ReadOnlyNewer(_))
}

#[test]
fn container_mutators_never_rewrite_read_only_requests() {
    let (_d, mut s, pid, c, c2, f) = guarded_store();
    let g = s.request("g1").unwrap().clone();
    let snapshot = s.project(&pid).unwrap().clone();

    // Moves that would re-home it, cascade deletes and copies are refused
    // and leave the tree untouched.
    let e = s
        .move_node(NodeKind::Folder, &f, Some(&c2), None, 0)
        .unwrap_err();
    assert!(is_ro(e.clone()), "{e:?}");
    assert!(e.to_string().contains("Say hello"), "{e}");
    assert!(is_ro(s.delete_folder(&f).unwrap_err()));
    let e = s.delete_collection(&c).unwrap_err();
    assert!(
        e.to_string()
            .contains("Delete \u{201c}Say hello\u{201d} on its own first"),
        "{e}"
    );
    assert!(is_ro(s.duplicate_folder(&f).unwrap_err()));
    assert!(is_ro(s.duplicate_collection(&c).unwrap_err()));
    assert!(is_ro(s.duplicate_project(&pid).unwrap_err()));
    s.create_project("Other").unwrap();
    assert!(is_ro(s.delete_project(&pid).unwrap_err()));
    assert_eq!(s.project(&pid).unwrap(), &snapshot);
    assert!(
        s.sync_state.tombstones.is_empty(),
        "a refused delete leaves no tombstones"
    );

    // Moves that don't touch it are fine: re-parenting the folder inside the
    // same collection, reordering collections.
    let inner = s.create_folder(&c, None, "Inner").unwrap().id;
    s.move_node(NodeKind::Folder, &f, Some(&c), Some(&inner), 0)
        .unwrap();
    s.move_node(NodeKind::Collection, &c, None, None, 1)
        .unwrap();
    assert_eq!(s.request("g1").unwrap(), &g);

    // Deleting the request itself is allowed; then the folder can go.
    s.delete_request("g1").unwrap();
    s.delete_folder(&f).unwrap();
}

#[test]
fn reordering_siblings_keeps_read_only_sort_orders() {
    let (_d, mut s, pid, c, _c2, f) = guarded_store();
    let b = s
        .create_request(&c, Some(&f), Request::default())
        .unwrap()
        .id;
    let a = s
        .project(&pid)
        .unwrap()
        .requests
        .iter()
        .find(|r| r.id != "g1" && r.id != b)
        .unwrap()
        .id
        .clone();
    for r in &mut s.projects.get_mut(&pid).unwrap().requests {
        if r.id == a {
            r.sort_order = 0;
        } else if r.id == b {
            r.sort_order = 1;
        }
    }
    let g = s.request("g1").unwrap().clone();
    // Siblings sorted: a(0), b(1), g1(2). Move `a` to the end.
    let t = s
        .move_node(NodeKind::Request, &a, Some(&c), Some(&f), 2)
        .unwrap();
    assert_eq!(s.request("g1").unwrap(), &g, "read-only sibling untouched");
    let mut order: Vec<(i64, String)> = t
        .requests
        .iter()
        .filter(|r| r.folder_id.as_deref() == Some(f.as_str()))
        .map(|r| (r.sort_order, r.id.clone()))
        .collect();
    order.sort();
    let ids: Vec<&str> = order.iter().map(|x| x.1.as_str()).collect();
    assert_eq!(ids, vec![b.as_str(), "g1", a.as_str()]);
    // Moving it to the front works too (numbers below the fixed one).
    s.move_node(NodeKind::Request, &a, Some(&c), Some(&f), 0)
        .unwrap();
    assert_eq!(s.request("g1").unwrap(), &g);

    // Two adjacent read-only siblings leave no gap: moving between them is
    // refused and nothing changes.
    let mut g2: Request = serde_json::from_value(grpc_request("g2", &c)).unwrap();
    g2.folder_id = Some(f.clone());
    g2.sort_order = 3;
    s.projects.get_mut(&pid).unwrap().requests.push(g2);
    let before = s.project(&pid).unwrap().clone();
    // Sorted now: a, b, g1(2), g2(3). Without `a`: b, g1, g2, so index 2
    // is between g1 and g2.
    let e = s
        .move_node(NodeKind::Request, &a, Some(&c), Some(&f), 2)
        .unwrap_err();
    assert!(is_ro(e));
    assert_eq!(s.project(&pid).unwrap(), &before);
    // Moving a request into another collection's list is unaffected.
    let c3 = s.create_collection(&pid, "C3").unwrap().id;
    s.move_node(NodeKind::Request, &b, Some(&c3), None, 0)
        .unwrap();
}

#[test]
fn unknown_auth_api_key_and_form_kinds_round_trip_and_are_read_only() {
    let cases = [
        json!({"type": "oauth2", "fxFlow": {"tokenUrl": "https://id/token"}}),
        json!({"type": "apikey", "apiKey": {"key": "k", "value": "v", "in": "cookie"}}),
    ];
    for auth in cases {
        let v = json!({"id": "x", "url": "https://a.dev", "auth": auth});
        let r: Request = serde_json::from_value(v.clone()).unwrap();
        assert!(!r.is_editable(), "{auth}");
        assert_eq!(serde_json::to_value(&r).unwrap()["auth"], auth);
        assert_eq!(
            prepare(&r, &VarScope::empty(), None).unwrap_err(),
            PrepareError::NeedsNewerVersion
        );
    }
    let body = json!({"type": "form", "form": [
        {"key": "a", "value": "1", "kind": "text", "enabled": true},
        {"key": "b", "value": "s", "kind": "stream", "enabled": true}
    ]});
    let r: Request =
        serde_json::from_value(json!({"id": "x", "url": "https://a.dev", "body": body})).unwrap();
    assert!(!r.is_editable());
    assert_eq!(serde_json::to_value(&r).unwrap()["body"], body);
    assert_eq!(
        r.body.form.as_ref().unwrap()[1].kind,
        FormFieldKind::Other("stream".into())
    );

    // A project file carrying such values loads without being quarantined.
    let dir = tempfile::tempdir().unwrap();
    let pid;
    {
        let mut s = Store::open(dir.path()).unwrap();
        pid = s.project_list()[0].id.clone();
        let c = s.create_collection(&pid, "C").unwrap();
        let pfile = dir.path().join(format!("projects/{pid}.json"));
        let mut v = read_json(&pfile);
        v["collections"][0]["auth"] = json!({"type": "oauth2", "fxFlow": 1});
        v["requests"] = json!([{"id": "r1", "collectionId": c.id, "name": "R",
            "auth": {"type": "inherit"}, "body": body}]);
        write_json(&pfile, &v);
    }
    let mut s = Store::open(dir.path()).unwrap();
    assert!(s.corrupt.is_empty(), "{:?}", s.corrupt);
    let col = s.project(&pid).unwrap().collections[0].clone();
    assert_eq!(col.auth.auth_type, AuthType::Other("oauth2".into()));
    assert!(!col.auth_is_known());
    assert!(!s.request("r1").unwrap().is_editable());

    // An inheriting editable request can't use the unknown collection auth.
    let inheriting = Request {
        url: "https://a.dev".into(),
        auth: Auth::inherit(),
        ..Request::default()
    };
    assert_eq!(
        prepare(&inheriting, &VarScope::empty(), Some(&col.auth)).unwrap_err(),
        PrepareError::InheritedAuthNeedsNewerVersion
    );

    // The collection can be renamed, but its unknown auth can't be changed.
    let mut renamed = col.clone();
    renamed.name = "Renamed".into();
    assert_eq!(s.update_collection(&renamed).unwrap().name, "Renamed");
    let mut reauth = renamed.clone();
    reauth.auth = Auth::bearer("t");
    assert!(is_ro(s.update_collection(&reauth).unwrap_err()));
    // ... and this build never writes an unknown auth itself.
    let mut c2 = s.create_collection(&pid, "Plain").unwrap();
    c2.auth = col.auth.clone();
    assert!(is_ro(s.update_collection(&c2).unwrap_err()));
    let saved = read_json(&dir.path().join(format!("projects/{pid}.json")));
    assert_eq!(
        saved["collections"][0]["auth"],
        json!({"type": "oauth2", "fxFlow": 1})
    );
}

#[test]
fn share_sanitizes_unknown_auth_schemes() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let mut col = c.clone();
    col.auth = serde_json::from_value(json!({"type": "oauth2", "clientSecret": "s3cret"})).unwrap();
    s.projects.get_mut(&pid).unwrap().collections[0] = col;
    let (snap, _) =
        build_snapshot(&s, ShareKind::Collection, &c.id, &ShareOptions::default()).unwrap();
    let text = snap.to_string();
    assert!(!text.contains("s3cret"), "{text}");
    assert_eq!(snap["collection"]["auth"]["type"], "oauth2");
}

#[test]
fn tombstones_of_unknown_types_are_kept_and_never_pushed() {
    let dir = tempfile::tempdir().unwrap();
    {
        let s = Store::open(dir.path()).unwrap();
        let sfile = dir.path().join("sync-state.json");
        let mut st = read_json(&sfile);
        st["tombstones"] = json!([
            {"id": "pf1", "type": "protoFile", "deletedAt": 5, "fxT": 1},
            {"id": "r1", "type": "request", "deletedAt": 6, "fxKnown": "k"}
        ]);
        write_json(&sfile, &st);
        drop(s);
    }
    let s = Store::open(dir.path()).unwrap();
    assert!(s.corrupt.is_empty(), "{:?}", s.corrupt);
    assert_eq!(s.sync_state.tombstones.len(), 1);
    assert_eq!(s.sync_state.unknown_tombstones.len(), 1);
    let push = sync::collect_push(&s);
    assert_eq!(push.len(), s.projects.len() + s.environments.len() + 1);
    assert!(push.iter().any(|c| c.id == "r1" && c.deleted));
    assert!(!push.iter().any(|c| c.id == "pf1"));
    s.save_sync_state().unwrap();
    let st = read_json(&dir.path().join("sync-state.json"));
    let ts = st["tombstones"].as_array().unwrap();
    assert_eq!(ts.len(), 2);
    assert!(ts.contains(&json!({"id": "pf1", "type": "protoFile", "deletedAt": 5, "fxT": 1})));
    assert!(ts.contains(&json!({"id": "r1", "type": "request", "deletedAt": 6, "fxKnown": "k"})));
}

#[test]
fn unknown_pulled_changes_are_capped() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let n = sync::MAX_UNKNOWN_CHANGES + 10;
    let many: Vec<Value> = (0..n)
        .map(|i| json!({"type": "protoFile", "id": format!("p{i}"), "rev": i as u64 + 1}))
        .collect();
    assert_eq!(sync::remember_unknown(&mut s, many), n);
    let list = &s.sync_state.unknown_changes;
    assert_eq!(list.len(), sync::MAX_UNKNOWN_CHANGES);
    assert!(s.sync_state.unknown_changes_truncated);
    // The oldest (lowest rev) were dropped.
    assert!(!list.iter().any(|v| v["rev"] == 1));
    assert!(list.iter().any(|v| v["rev"] == n as u64));
    // Bytes are capped too.
    let mut s = Store::open(tempfile::tempdir().unwrap().path()).unwrap();
    let big = "x".repeat(1024 * 1024);
    let fat: Vec<Value> = (0..12)
        .map(|i| json!({"type": "blob", "id": format!("b{i}"), "rev": i + 1, "data": big}))
        .collect();
    sync::remember_unknown(&mut s, fat);
    let bytes: usize = s
        .sync_state
        .unknown_changes
        .iter()
        .map(|v| serde_json::to_vec(v).unwrap().len())
        .sum();
    assert!(bytes <= sync::MAX_UNKNOWN_BYTES, "{bytes}");
    assert!(s.sync_state.unknown_changes.len() < 12);
}

// ------------------------------------------------------------------ sync

fn change(t: &str, id: &str, rev: u64, updated_at: i64, data: Value) -> Value {
    json!({"type": t, "id": id, "updatedAt": updated_at, "deleted": false,
           "data": data, "rev": rev, "deviceId": "other-device"})
}

fn connected_store(dir: &std::path::Path, base: &str) -> Store {
    let mut s = Store::open(dir).unwrap();
    s.workspace.sync = Some(SyncConfig {
        worker_url: base.to_string(),
        remote_workspace_id: "remote".into(),
        token: "tok".into(),
        cursor: 0,
    });
    s.save_workspace().unwrap();
    s
}

async fn serve_pull(server: &MockServer, changes: Vec<Value>, cursor: u64) {
    Mock::given(method("GET"))
        .and(path("/v1/sync"))
        .and(header("x-reqharbor-version", CLIENT_VERSION))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"changes": changes, "cursor": cursor, "hasMore": false})),
        )
        .mount(server)
        .await;
}

#[tokio::test]
async fn ac_fwd_5_unknown_pull_types_are_skipped_and_kept() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let ws = s.workspace.id.clone();
    let proto = change(
        "protoFile",
        "pf1",
        1,
        10,
        json!({"id": "pf1", "proto": "syntax = \"proto3\";"}),
    );
    serve_pull(
        &server,
        vec![
            proto.clone(),
            change(
                "project",
                "p1",
                2,
                10,
                json!({"id": "p1", "workspaceId": ws, "name": "Remote", "fxP": 1}),
            ),
            change(
                "collection",
                "c1",
                3,
                10,
                json!({"id": "c1", "projectId": "p1", "name": "Col"}),
            ),
            change("request", "g1", 4, 10, grpc_request("g1", "c1")),
            change(
                "protoFile",
                "pf1",
                5,
                11,
                json!({"id": "pf1", "proto": "v2"}),
            ),
        ],
        5,
    )
    .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::pull(&client, &mut s).await.unwrap();
    assert_eq!(report.applied, 3, "{report:?}");
    assert_eq!(report.unknown, 2);
    assert!(report.skipped.is_empty(), "{report:?}");
    assert_eq!(s.workspace.sync.as_ref().unwrap().cursor, 5);
    assert_eq!(s.projects["p1"].project.extra.get("fxP"), Some(&json!(1)));
    let g = s.request("g1").unwrap();
    assert_eq!(g.kind, RequestKind::Grpc);
    assert!(g.extra.contains_key("grpc"));
    // Latest version per (type, id) is kept and persisted.
    assert_eq!(s.sync_state.unknown_changes.len(), 1);
    assert_eq!(s.sync_state.unknown_changes[0]["rev"], 5);
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.sync_state.unknown_changes[0]["data"]["proto"], "v2");
}

#[tokio::test]
async fn read_only_requests_are_never_pushed_when_modified_locally() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let ws = s.workspace.id.clone();
    let pulled = vec![
        change(
            "project",
            "p1",
            1,
            10,
            json!({"id": "p1", "workspaceId": ws, "name": "Remote"}),
        ),
        change(
            "collection",
            "c1",
            2,
            10,
            json!({"id": "c1", "projectId": "p1", "name": "Col"}),
        ),
        change(
            "folder",
            "f1",
            3,
            10,
            json!({"id": "f1", "collectionId": "c1", "name": "F"}),
        ),
        change("request", "g1", 4, 10, {
            let mut g = grpc_request("g1", "c1");
            g["folderId"] = json!("f1");
            g
        }),
        change("request", "h1", 5, 10, rich_request_json("h1", "c1")),
    ];
    serve_pull(&server, pulled, 5).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    sync::pull(&client, &mut s).await.unwrap();
    assert!(
        sync::collect_push(&s)
            .iter()
            .all(|c| c.entity_type != EntityType::Request),
        "pulled requests are clean"
    );

    // Moving the parent folder to another collection would re-home the
    // gRPC request: refused, nothing changes ...
    let c2 = s.create_collection("p1", "Other").unwrap();
    let before = s.project("p1").unwrap().clone();
    let err = s
        .move_node(NodeKind::Folder, "f1", Some(&c2.id), None, 0)
        .unwrap_err();
    assert!(matches!(err, StoreError::ReadOnlyNewer(_)), "{err:?}");
    assert_eq!(s.project("p1").unwrap(), &before);
    // ... even if the request was touched behind the store's back (e.g. by
    // an older build), it is never pushed ...
    s.projects
        .get_mut("p1")
        .unwrap()
        .requests
        .iter_mut()
        .find(|r| r.id == "g1")
        .unwrap()
        .collection_id = c2.id.clone();
    // ... and an editable request with unknown fields is edited.
    s.rename_request("h1", "Renamed").unwrap();
    let push = sync::collect_push(&s);
    assert!(
        !push.iter().any(|c| c.id == "g1"),
        "a locally modified read-only request must never be pushed"
    );
    let h1 = push.iter().find(|c| c.id == "h1").expect("h1 is pushed");
    assert_request_extras(h1.data.as_ref().unwrap());
    assert_eq!(h1.data.as_ref().unwrap()["name"], "Renamed");

    // A remote version always replaces the local read-only one: no
    // conflict copy, even though the local copy was touched later.
    let report = sync::apply_pull(
        &mut s,
        vec![
            serde_json::from_value(change("request", "g1", 6, 11, grpc_request("g1", "c1")))
                .unwrap(),
        ],
        6,
    );
    assert!(report.conflicts.is_empty(), "{report:?}");
    assert_eq!(s.request("g1").unwrap().collection_id, "c1");
    assert_eq!(
        s.sync_state
            .synced
            .get(&sync_key(EntityType::Request, "g1")),
        Some(&11)
    );
    // Deleting it is allowed and pushes a tombstone.
    s.delete_request("g1").unwrap();
    assert!(
        sync::collect_push(&s)
            .iter()
            .any(|c| c.id == "g1" && c.deleted)
    );
}

#[tokio::test]
async fn conflict_copy_keeps_extra() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let ws = s.workspace.id.clone();
    serve_pull(
        &server,
        vec![
            change(
                "project",
                "p1",
                1,
                10,
                json!({"id": "p1", "workspaceId": ws, "name": "Remote"}),
            ),
            change(
                "collection",
                "c1",
                2,
                10,
                json!({"id": "c1", "projectId": "p1", "name": "Col"}),
            ),
            change("request", "h1", 3, 10, rich_request_json("h1", "c1")),
        ],
        3,
    )
    .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    sync::pull(&client, &mut s).await.unwrap();
    s.rename_request("h1", "Local edit").unwrap();
    let mut remote = rich_request_json("h1", "c1");
    remote["name"] = json!("Remote edit");
    let far_future = now_ms() + 1_000_000;
    let report = sync::apply_pull(
        &mut s,
        vec![serde_json::from_value(change("request", "h1", 4, far_future, remote)).unwrap()],
        4,
    );
    assert_eq!(report.conflicts.len(), 1);
    let copy = s.request(&report.conflicts[0].copy_id).unwrap();
    assert_eq!(copy.name, "Local edit (conflict)");
    assert_request_extras(&serde_json::to_value(copy).unwrap());
}

#[tokio::test]
async fn sync_calls_send_the_version_header() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/workspace"))
        .and(header("x-reqharbor-version", CLIENT_VERSION))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"workspaceId": "w", "name": "W"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    assert_eq!(client.workspace().await.unwrap().workspace_id, "w");
}

#[test]
fn pull_page_with_only_unknown_types_parses() {
    let page: sync::PullPage = serde_json::from_value(json!({
        "changes": [{"type": "socketCollection", "id": "x", "anything": [1]}],
        "cursor": 9
    }))
    .unwrap();
    assert!(page.changes.is_empty());
    assert_eq!(page.unknown.len(), 1);
    assert_eq!(page.cursor, 9);
    // A malformed change of a known type is still an error.
    assert!(
        serde_json::from_value::<sync::PullPage>(json!({
            "changes": [{"type": "request", "id": 5}], "cursor": 1
        }))
        .is_err()
    );
}

// ------------------------------------------------- restore detection (§9)

/// `GET /v1/sync?since=<since>` answers `body` (most specific mocks first).
async fn serve_pull_since(server: &MockServer, since: u64, body: Value) {
    Mock::given(method("GET"))
        .and(path("/v1/sync"))
        .and(query_param("since", since.to_string()))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(server)
        .await;
}

/// `POST /v1/sync`: every change is applied, except types in `reject`,
/// which get a per-change `rejected` / `unsupported_type`.
struct PushEcho {
    reject: Vec<&'static str>,
}

impl Respond for PushEcho {
    fn respond(&self, req: &WmRequest) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let results: Vec<Value> = body["changes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| {
                if self.reject.contains(&c["type"].as_str().unwrap()) {
                    json!({"type": c["type"], "id": c["id"], "status": "rejected", "reason": "unsupported_type"})
                } else {
                    json!({"type": c["type"], "id": c["id"], "status": "applied", "rev": 100})
                }
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(json!({"results": results, "cursor": 100}))
    }
}

async fn serve_push(server: &MockServer, reject: Vec<&'static str>) {
    Mock::given(method("POST"))
        .and(path("/v1/sync"))
        .respond_with(PushEcho { reject })
        .mount(server)
        .await;
}

/// Changes pushed so far, from the mock's request log.
async fn pushed_changes(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method == wiremock::http::Method::POST && r.url.path() == "/v1/sync")
        .flat_map(|r| {
            let v: Value = serde_json::from_slice(&r.body).unwrap();
            v["changes"].as_array().cloned().unwrap_or_default()
        })
        .collect()
}

fn remote_tree(ws: &str) -> Vec<Value> {
    vec![
        change(
            "project",
            "p1",
            1,
            10,
            json!({"id": "p1", "workspaceId": ws, "name": "Remote"}),
        ),
        change(
            "collection",
            "c1",
            2,
            10,
            json!({"id": "c1", "projectId": "p1", "name": "Col"}),
        ),
        change(
            "request",
            "r1",
            3,
            10,
            json!({"id": "r1", "collectionId": "c1", "name": "One"}),
        ),
        change(
            "request",
            "r2",
            4,
            20,
            json!({"id": "r2", "collectionId": "c1", "name": "Two v2"}),
        ),
    ]
}

/// A connected store that pulled [`remote_tree`] (cursor 4) and pushed its
/// own Global environment, so everything is clean.
fn synced_store(dir: &std::path::Path, base: &str, epoch: Option<&str>) -> Store {
    let mut s = connected_store(dir, base);
    let ws = s.workspace.id.clone();
    let changes = remote_tree(&ws)
        .into_iter()
        .map(|v| serde_json::from_value(v).unwrap())
        .collect();
    sync::apply_pull(&mut s, changes, 4);
    for c in sync::collect_push(&s) {
        s.sync_state
            .synced
            .insert(sync_key(c.entity_type, &c.id), c.updated_at);
    }
    s.sync_state.sync_epoch = epoch.map(str::to_string);
    assert!(sync::collect_push(&s).is_empty());
    s
}

#[tokio::test]
async fn restore_by_epoch_change_resets_cursor_and_resends_everything() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = synced_store(dir.path(), &server.uri(), Some("e1"));
    let ws = s.workspace.id.clone();
    // The restored server (epoch e2) has r1 as it was, but only the older
    // version of r2, and not the Global environment.
    let mut restored = remote_tree(&ws);
    restored[3] = change(
        "request",
        "r2",
        4,
        15,
        json!({"id": "r2", "collectionId": "c1", "name": "Two v1"}),
    );
    serve_pull_since(
        &server,
        4,
        json!({"changes": [], "cursor": 4, "hasMore": false, "syncEpoch": "e2"}),
    )
    .await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": restored, "cursor": 4, "hasMore": false, "syncEpoch": "e2"}),
    )
    .await;
    serve_push(&server, vec![]).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::sync_now(&client, &mut s).await.unwrap();

    assert!(report.restored, "{report:?}");
    assert!(
        report.conflicts.is_empty(),
        "re-pulled equal versions are no conflict: {report:?}"
    );
    assert_eq!(s.sync_state.sync_epoch.as_deref(), Some("e2"));
    assert_eq!(s.workspace.sync.as_ref().unwrap().cursor, 4);
    // The older restored r2 never overwrites the local newer one ...
    let r2 = s.request("r2").unwrap();
    assert_eq!((r2.name.as_str(), r2.updated_at), ("Two v2", 20));
    // ... which is re-sent with the device id of the version it is.
    let pushed = pushed_changes(&server).await;
    let r2p = pushed.iter().find(|c| c["id"] == "r2").expect("r2 re-sent");
    assert_eq!(r2p["deviceId"], "other-device");
    assert_eq!(r2p["updatedAt"], 20);
    assert!(
        pushed.iter().any(|c| c["type"] == "environment"),
        "env re-sent"
    );
    assert!(
        !pushed.iter().any(|c| c["id"] == "r1"),
        "r1 is on the server"
    );
    assert!(sync::collect_push(&s).is_empty(), "clean again");
    assert!(s.sync_state.resend.is_empty());
}

#[tokio::test]
async fn restore_by_cursor_below_since_resets_and_marks_dirty() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = synced_store(dir.path(), &server.uri(), None);
    s.workspace.sync.as_mut().unwrap().cursor = 50;
    let ws = s.workspace.id.clone();
    // No syncEpoch at all (the Worker), but the cursor went backwards.
    serve_pull_since(
        &server,
        50,
        json!({"changes": [], "cursor": 7, "hasMore": false}),
    )
    .await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": remote_tree(&ws)[..2], "cursor": 2, "hasMore": false}),
    )
    .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::pull(&client, &mut s).await.unwrap();
    assert!(report.restored);
    assert_eq!(s.workspace.sync.as_ref().unwrap().cursor, 2);
    // Every entity the server didn't send back is dirty again.
    let push = sync::collect_push(&s);
    for id in ["r1", "r2"] {
        assert!(push.iter().any(|c| c.id == id), "{id} must be re-sent");
    }
    assert!(
        push.iter()
            .any(|c| c.entity_type == EntityType::Environment)
    );
    assert!(
        !push.iter().any(|c| c.id == "p1" || c.id == "c1"),
        "equal versions are clean"
    );
    assert_eq!(s.sync_state.sync_epoch, None);
}

#[tokio::test]
async fn first_pull_only_stores_the_epoch_and_the_worker_never_resets_on_epoch() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let ws = s.workspace.id.clone();
    serve_pull_since(
        &server,
        0,
        json!({"changes": remote_tree(&ws), "cursor": 4, "hasMore": false, "syncEpoch": "e1"}),
    )
    .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::pull(&client, &mut s).await.unwrap();
    assert!(!report.restored);
    assert_eq!(s.sync_state.sync_epoch.as_deref(), Some("e1"));
    assert!(
        !sync::collect_push(&s)
            .iter()
            .any(|c| c.entity_type == EntityType::Request)
    );

    // A Worker page (no syncEpoch, cursor not below since) never resets,
    // and the stored epoch is kept.
    serve_pull_since(
        &server,
        4,
        json!({"changes": [], "cursor": 4, "hasMore": false}),
    )
    .await;
    let report = sync::pull(&client, &mut s).await.unwrap();
    assert!(!report.restored);
    assert_eq!(s.sync_state.sync_epoch.as_deref(), Some("e1"));
    assert_eq!(s.workspace.sync.as_ref().unwrap().cursor, 4);
    // ... and the restore state persists across a restart.
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.sync_state.sync_epoch.as_deref(), Some("e1"));
}

#[tokio::test]
async fn restore_by_server_info_epoch_pulls_from_zero() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = synced_store(dir.path(), &server.uri(), Some("e1"));
    let ws = s.workspace.id.clone();
    Mock::given(method("GET"))
        .and(path("/v1/server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "software": "reqharbor-server", "apiVersion": 1, "syncEpoch": "e9",
            "sync": {"types": BASE_TYPES, "requestKinds": ["http"]}, "futureField": true
        })))
        .mount(&server)
        .await;
    // The pull itself would not reveal it (the cursor is still 4).
    serve_pull_since(
        &server,
        0,
        json!({"changes": remote_tree(&ws), "cursor": 4, "hasMore": false, "syncEpoch": "e9"}),
    )
    .await;
    serve_push(&server, vec![]).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::sync_now(&client, &mut s).await.unwrap();
    assert!(report.restored);
    assert!(report.conflicts.is_empty());
    assert_eq!(s.sync_state.sync_epoch.as_deref(), Some("e9"));
    let pushed = pushed_changes(&server).await;
    assert_eq!(
        pushed.len(),
        1,
        "only the Global environment was missing: {pushed:?}"
    );
}

#[test]
fn pull_page_reads_sync_epoch() {
    let page: sync::PullPage =
        serde_json::from_value(json!({"changes": [], "cursor": 3, "syncEpoch": "ep"})).unwrap();
    assert_eq!(page.sync_epoch.as_deref(), Some("ep"));
    let page: sync::PullPage =
        serde_json::from_value(json!({"changes": [], "cursor": 3, "syncEpoch": ""})).unwrap();
    assert_eq!(page.sync_epoch, None, "an empty epoch counts as none");
}

// ------------------------------------------- push filtering by sync.types

const BASE_TYPES: [&str; 5] = ["project", "collection", "folder", "request", "environment"];

async fn serve_server_info(server: &MockServer, types: &[&str]) {
    Mock::given(method("GET"))
        .and(path("/v1/server"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "software": "reqharbor-server", "apiVersion": 1, "instanceId": "i1",
            "syncEpoch": "e1", "sync": {"types": types, "requestKinds": ["http"]}
        })))
        .mount(server)
        .await;
}

#[tokio::test]
async fn push_only_sends_types_the_server_lists() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let pid = s.project_list()[0].id.clone();
    s.create_collection(&pid, "Mine").unwrap();
    // An older server that doesn't take environments.
    serve_server_info(&server, &["project", "collection", "folder", "request"]).await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": [], "cursor": 0, "hasMore": false, "syncEpoch": "e1"}),
    )
    .await;
    serve_push(&server, vec![]).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let report = sync::sync_now(&client, &mut s).await.unwrap();

    let pushed = pushed_changes(&server).await;
    assert!(pushed.iter().any(|c| c["type"] == "collection"));
    assert!(
        !pushed.iter().any(|c| c["type"] == "environment"),
        "{pushed:?}"
    );
    assert_eq!(report.unsupported.len(), 1, "{report:?}");
    assert_eq!(report.unsupported[0].entity_type, EntityType::Environment);
    assert_eq!(report.unsupported[0].reason, "unsupported_type");
    assert_eq!(
        sync::unsupported_notice(3).as_deref(),
        Some(
            "3 items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server."
        )
    );
    // It stays dirty (local-only), not marked as skipped.
    assert!(
        sync::collect_push(&s)
            .iter()
            .any(|c| c.entity_type == EntityType::Environment)
    );
    assert!(sync::skipped_entities(&mut s).is_empty());
    assert!(
        sync::server_info_due(&s, now_ms()),
        "held-back items re-check the server"
    );

    // The server is updated: the next run asks again and sends it.
    server.reset().await;
    serve_server_info(&server, &BASE_TYPES).await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": [], "cursor": 0, "hasMore": false, "syncEpoch": "e1"}),
    )
    .await;
    serve_push(&server, vec![]).await;
    let report = sync::sync_now(&client, &mut s).await.unwrap();
    assert!(report.unsupported.is_empty());
    let pushed = pushed_changes(&server).await;
    assert!(pushed.iter().any(|c| c["type"] == "environment"));
    assert!(sync::collect_push(&s).is_empty());
    assert!(!sync::server_info_due(&s, now_ms()));
}

#[tokio::test]
async fn missing_server_info_means_the_base_types() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    // The Worker: GET /v1/server is a 404 `not_found`.
    Mock::given(method("GET"))
        .and(path("/v1/server"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "not_found", "message": "Not found"}})),
        )
        .expect(2)
        .mount(&server)
        .await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": [], "cursor": 0, "hasMore": false}),
    )
    .await;
    serve_push(&server, vec![]).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    assert_eq!(client.server_info().await.unwrap(), None);
    let report = sync::sync_now(&client, &mut s).await.unwrap();
    assert!(report.unsupported.is_empty());
    assert!(
        sync::collect_push(&s).is_empty(),
        "every base type was pushed"
    );
    assert_eq!(s.sync_state.server_types, None);
    assert!(s.sync_state.server_checked_at.is_some());
}

#[tokio::test]
async fn per_change_unsupported_type_keeps_the_entity_local() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    serve_server_info(&server, &BASE_TYPES).await;
    serve_pull_since(
        &server,
        0,
        json!({"changes": [], "cursor": 0, "hasMore": false, "syncEpoch": "e1"}),
    )
    .await;
    // The server changed between discovery and push.
    serve_push(&server, vec!["environment"]).await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    sync::sync_now(&client, &mut s).await.unwrap();
    assert!(
        sync::collect_push(&s)
            .iter()
            .any(|c| c.entity_type == EntityType::Environment)
    );
    assert!(sync::skipped_entities(&mut s).is_empty(), "not 'too large'");
    assert!(!sync::server_types(&s).iter().any(|t| t == "environment"));
    assert_eq!(
        s.sync_state.server_checked_at, None,
        "rediscovery decides next time"
    );
    assert_eq!(sync::unsupported_entities(&s).len(), 1);
}

// ------------------------------------------------------------- proptest

fn unknown_key() -> impl Strategy<Value = String> {
    // Prefix keeps generated keys from colliding with declared field names.
    "[a-z][a-zA-Z0-9]{0,8}".prop_map(|s| format!("fx_{s}"))
}

fn json_leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(|n| json!(n)),
        "[ -~]{0,12}".prop_map(Value::String),
        prop::collection::vec(any::<u8>().prop_map(|n| json!(n)), 0..3).prop_map(Value::Array),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Unknown keys injected at any level survive deserialise -> edit ->
    /// save_request (from an object stripped of them) -> serialise.
    #[test]
    fn prop_unknown_fields_survive_every_level(
        entries in prop::collection::vec((0usize..REQUEST_EXTRA_POINTERS.len(), unknown_key(), json_leaf()), 1..8)
    ) {
        let mut v = rich_request_json("r1", "c1");
        let mut expected = Vec::new();
        for (slot, key, val) in entries {
            let parent = REQUEST_EXTRA_POINTERS[slot].rsplit_once('/').unwrap().0;
            let obj = if parent.is_empty() { &mut v } else { v.pointer_mut(parent).unwrap() };
            obj.as_object_mut().unwrap().insert(key.clone(), val.clone());
            expected.retain(|(p, k, _): &(String, String, Value)| !(p == parent && *k == key));
            expected.push((parent.to_string(), key, val));
        }
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::open(dir.path()).unwrap();
        let pid = s.project_list()[0].id.clone();
        let mut c = s.create_collection(&pid, "C").unwrap();
        c.id = "c1".into();
        s.projects.get_mut(&pid).unwrap().collections = vec![c];
        let r: Request = serde_json::from_value(v).unwrap();
        s.projects.get_mut(&pid).unwrap().requests.push(r.clone());
        let mut edit = r;
        strip_extras(&mut edit);
        edit.name = "edited".into();
        s.save_request(&edit).unwrap();
        s.save_project(&pid).unwrap();
        let s = Store::open(dir.path()).unwrap();
        let out = serde_json::to_value(s.request("r1").unwrap()).unwrap();
        prop_assert_eq!(&out["name"], &json!("edited"));
        for (parent, key, val) in expected {
            let obj = if parent.is_empty() { &out } else { out.pointer(&parent).unwrap() };
            prop_assert_eq!(obj.get(&key), Some(&val), "{}/{}", parent, key);
        }
    }
}

// ------------------------------------------------- M0 QA round 2

/// A gRPC request whose metadata holds a bearer token (a field this build
/// doesn't know), inside collection `c`.
fn grpc_with_token(id: &str, collection_id: &str) -> Value {
    json!({
        "id": id, "collectionId": collection_id, "name": "Say hello",
        "kind": "grpc", "method": "POST", "url": "grpc://svc:443",
        "grpc": {"service": "Greeter", "method": "SayHello",
                 "metadata": [{"key": "authorization", "value": "Bearer grpc-s3cret"}]},
        "updatedAt": 5
    })
}

#[test]
fn share_without_credentials_never_leaks_unknown_fields() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let g: Request = serde_json::from_value(grpc_with_token("g1", &c.id)).unwrap();
    s.projects.get_mut(&pid).unwrap().requests.push(g);
    let off = ShareOptions::default();
    let on = ShareOptions {
        include_credentials: true,
        environment_id: None,
    };

    // Credentials off: a collection or request share holding a request
    // from a newer ReqHarbor is refused, so the token never leaves.
    for (kind, id) in [
        (ShareKind::Collection, c.id.as_str()),
        (ShareKind::Request, "g1"),
    ] {
        let e = build_snapshot(&s, kind, id, &off).unwrap_err();
        assert!(is_ro(e.clone()), "{e:?}");
        let msg = e.to_string();
        assert!(
            msg.contains("Say hello") && msg.contains("Include credentials"),
            "{msg}"
        );
    }
    // Credentials on: shared as it is, and the preview counts the unknown
    // block as a credential field.
    let (snap, preview) = build_snapshot(&s, ShareKind::Collection, &c.id, &on).unwrap();
    assert!(snap.to_string().contains("grpc-s3cret"));
    assert!(preview.credential_fields >= 1);
    assert_eq!(preview.credentials_replaced, 0);

    // An editable HTTP request with unknown keys under a known auth scheme,
    // a header and the request itself: dropped and counted without
    // credentials, kept with them.
    s.delete_request("g1").unwrap();
    let r: Request = serde_json::from_value(json!({
        "id": "h1", "collectionId": c.id, "name": "Plain", "method": "GET",
        "url": "https://api.example.com",
        "headers": [{"key": "X-Trace", "value": "1", "enabled": true,
                     "secretRef": "hdr-s3cret"}],
        "auth": {"type": "bearer", "token": "tok", "refreshToken": "rt-s3cret"},
        "clientCert": {"passphrase": "cert-s3cret"},
        "updatedAt": 6
    }))
    .unwrap();
    s.projects.get_mut(&pid).unwrap().requests.push(r);
    let mut col = s.collection(&c.id).unwrap().clone();
    col.auth = serde_json::from_value(
        json!({"type": "apikey", "apiKey": {"key": "k", "value": "v", "in": "header", "salt": "ak-s3cret"}}),
    )
    .unwrap();
    col.extra.insert("vault".into(), json!("col-s3cret"));
    s.projects.get_mut(&pid).unwrap().collections[0] = col;
    let (snap, preview) = build_snapshot(&s, ShareKind::Collection, &c.id, &off).unwrap();
    let text = snap.to_string();
    for secret in [
        "hdr-s3cret",
        "rt-s3cret",
        "cert-s3cret",
        "ak-s3cret",
        "col-s3cret",
    ] {
        assert!(!text.contains(secret), "{secret} leaked: {text}");
    }
    // token + apiKey value + 5 unknown blocks.
    assert_eq!(preview.credential_fields, 7, "{preview:?}");
    assert_eq!(preview.credentials_replaced, 7);
    let (snap, preview) = build_snapshot(&s, ShareKind::Request, "h1", &off).unwrap();
    assert!(!snap.to_string().contains("s3cret"), "{snap}");
    assert_eq!(preview.credentials_replaced, 4, "{preview:?}");
    let (snap, _) = build_snapshot(&s, ShareKind::Collection, &c.id, &on).unwrap();
    let text = snap.to_string();
    for secret in [
        "hdr-s3cret",
        "rt-s3cret",
        "cert-s3cret",
        "ak-s3cret",
        "col-s3cret",
    ] {
        assert!(
            text.contains(secret),
            "{secret} dropped with credentials on"
        );
    }
}

#[test]
fn share_refuses_containers_from_a_newer_reqharbor_without_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap();
    let f = s.create_folder(&c.id, None, "Future folder").unwrap();
    s.projects.get_mut(&pid).unwrap().folders[0]
        .extra
        .insert("minClientVersion".into(), json!("99.0.0"));
    let e = build_snapshot(&s, ShareKind::Collection, &c.id, &ShareOptions::default()).unwrap_err();
    assert!(e.to_string().contains("Future folder"), "{e}");
    let on = ShareOptions {
        include_credentials: true,
        environment_id: None,
    };
    let (snap, _) = build_snapshot(&s, ShareKind::Collection, &c.id, &on).unwrap();
    assert_eq!(snap["collection"]["folders"][0]["id"], json!(f.id));
}

/// A store whose collection `cr` (and folder `fr` in collection `c`, and
/// the project, and an environment) carry a `minClientVersion` above this
/// build. Returns (dir, store, pid, c, cr, fr, env).
fn newer_containers() -> (
    tempfile::TempDir,
    Store,
    String,
    String,
    String,
    String,
    String,
) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "C").unwrap().id;
    let cr = s.create_collection(&pid, "Future collection").unwrap().id;
    let fr = s.create_folder(&c, None, "Future folder").unwrap().id;
    let env = s.create_environment(Some(&pid), "Future env").unwrap().id;
    let newer = || json!("99.0.0");
    {
        let t = s.projects.get_mut(&pid).unwrap();
        t.collections
            .iter_mut()
            .find(|x| x.id == cr)
            .unwrap()
            .extra
            .insert("minClientVersion".into(), newer());
        t.folders[0]
            .extra
            .insert("minClientVersion".into(), newer());
    }
    s.environments
        .iter_mut()
        .find(|e| e.id == env)
        .unwrap()
        .extra
        .insert("minClientVersion".into(), newer());
    s.save_all().unwrap();
    (dir, s, pid, c, cr, fr, env)
}

#[test]
fn ac_fwd_4_containers_with_a_newer_min_client_version_are_read_only() {
    let (_d, mut s, pid, c, cr, fr, env) = newer_containers();
    let mut col = s.collection(&cr).unwrap().clone();
    assert!(!col.is_editable());
    assert!(s.collection(&c).unwrap().is_editable());
    let before = s.project(&pid).unwrap().clone();

    col.name = "Renamed".into();
    assert!(is_ro(s.update_collection(&col).unwrap_err()));
    assert!(is_ro(s.duplicate_collection(&cr).unwrap_err()));
    assert!(is_ro(
        s.move_node(NodeKind::Collection, &cr, None, None, 0)
            .unwrap_err()
    ));
    assert!(is_ro(s.rename_folder(&fr, "X").unwrap_err()));
    assert!(is_ro(s.duplicate_folder(&fr).unwrap_err()));
    assert!(is_ro(
        s.move_node(NodeKind::Folder, &fr, Some(&cr), None, 0)
            .unwrap_err()
    ));
    // Its parent can't be deleted or copied with it.
    let e = s.delete_collection(&c).unwrap_err();
    assert!(e.to_string().contains("Future folder"), "{e}");
    assert!(is_ro(s.duplicate_project(&pid).unwrap_err()));
    let mut e2 = s.environment(&env).unwrap().clone();
    e2.name = "Y".into();
    assert!(is_ro(s.update_environment(&e2).unwrap_err()));
    assert!(is_ro(s.duplicate_environment(&env).unwrap_err()));
    assert_eq!(s.project(&pid).unwrap(), &before, "nothing changed");

    // Reordering collections around it keeps its sort order.
    let fixed = s.collection(&cr).unwrap().clone();
    let c3 = s.create_collection(&pid, "C3").unwrap().id;
    s.move_node(NodeKind::Collection, &c3, None, None, 0)
        .unwrap();
    assert_eq!(s.collection(&cr).unwrap(), &fixed);
    // Reordering sibling folders around the read-only folder too.
    let fixed_f = s.project(&pid).unwrap().folders[0].clone();
    let f2 = s.create_folder(&c, None, "F2").unwrap().id;
    s.move_node(NodeKind::Folder, &f2, Some(&c), None, 0)
        .unwrap();
    let after = s.project(&pid).unwrap();
    assert_eq!(after.folders.iter().find(|f| f.id == fr).unwrap(), &fixed_f);

    // Children can still be added, and the containers can be deleted on
    // their own.
    s.create_request(&cr, None, Request::default()).unwrap();
    s.delete_folder(&fr).unwrap();
    s.delete_collection(&cr).unwrap();
    s.delete_environment(&env).unwrap();
}

#[test]
fn ac_fwd_4_a_newer_project_is_read_only_and_kept_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let p0 = s.project_list()[0].id.clone();
    let p1 = s.create_project("Future").unwrap().project.id;
    let p2 = s.create_project("Mine").unwrap().project.id;
    s.projects
        .get_mut(&p1)
        .unwrap()
        .project
        .extra
        .insert("minClientVersion".into(), json!("99.0.0"));
    let fixed = s.project(&p1).unwrap().project.clone();
    assert!(is_ro(s.rename_project(&p1, "X").unwrap_err()));
    assert!(is_ro(s.move_project(&p1, 0).unwrap_err()));
    assert!(is_ro(s.duplicate_project(&p1).unwrap_err()));
    let env = s.create_environment(Some(&p1), "E").unwrap().id;
    assert!(is_ro(
        s.set_active_environment(&p1, Some(&env)).unwrap_err()
    ));
    // Moving another project around it leaves it alone.
    s.move_project(&p2, 0).unwrap();
    assert_eq!(s.project(&p1).unwrap().project, fixed);
    let order: Vec<String> = s.project_list().into_iter().map(|p| p.id).collect();
    assert_eq!(order, vec![p2.clone(), p0.clone(), p1.clone()]);
    s.delete_project(&p1).unwrap();
}

#[test]
fn read_only_containers_are_never_pushed_when_modified_locally() {
    let (_d, mut s, _pid, _c, cr, _fr, env) = newer_containers();
    for (t, id) in [
        (EntityType::Collection, &cr),
        (EntityType::Environment, &env),
    ] {
        s.sync_state.synced.insert(sync_key(t, id), 1);
    }
    let push = sync::collect_push(&s);
    assert!(!push.iter().any(|c| c.id == cr), "{push:?}");
    assert!(!push.iter().any(|c| c.id == env));
    assert!(sync::read_only_entity(&s, EntityType::Collection, &cr));
    assert!(sync::read_only_entity(&s, EntityType::Environment, &env));
}

fn result(t: &str, id: &str, status: &str, reason: Option<&str>) -> sync::PushResult {
    serde_json::from_value(json!({"type": t, "id": id, "status": status, "reason": reason}))
        .unwrap()
}

#[test]
fn rejected_push_results_are_handled_per_reason() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let mk = |s: &mut Store, name: &str| s.create_collection(&pid, name).unwrap().id;
    let (skew, odd, viewer, big, future, unknown_status) = (
        mk(&mut s, "Skew"),
        mk(&mut s, "Odd"),
        mk(&mut s, "Viewer"),
        mk(&mut s, "Big"),
        mk(&mut s, "Future"),
        mk(&mut s, "Unknown"),
    );
    // Two synced collections deleted locally: one delete is rejected, one
    // applied.
    let gone_rejected = mk(&mut s, "Gone1");
    let gone_applied = mk(&mut s, "Gone2");
    for id in [&gone_rejected, &gone_applied] {
        s.sync_state
            .synced
            .insert(sync_key(EntityType::Collection, id), 1);
        s.delete_collection(id).unwrap();
    }
    assert_eq!(s.sync_state.tombstones.len(), 2);
    let pushed = sync::collect_push(&s);
    let results = vec![
        result("collection", &skew, "rejected", Some("clock_skew")),
        result("collection", &odd, "rejected", Some("invalid_json_value")),
        result("collection", &viewer, "rejected", Some("forbidden_role")),
        result("collection", &big, "rejected", Some("entity_too_large")),
        result("collection", &future, "rejected", None),
        result("collection", &unknown_status, "deferred", None),
        result(
            "collection",
            &gone_rejected,
            "rejected",
            Some("forbidden_role"),
        ),
        result("collection", &gone_applied, "applied", None),
    ];
    assert_eq!(results[5].status, sync::PushStatus::Unknown);
    let out = sync::apply_push_results_detailed(&mut s, &pushed, &results);
    assert_eq!(out.rejected, 6);
    assert_eq!(out.clock_skew, 1);
    assert_eq!(out.unknown, 1);
    assert_eq!(out.applied, 1);

    // The rejected delete keeps its tombstone and is sent again.
    assert_eq!(s.sync_state.tombstones.len(), 1);
    assert_eq!(s.sync_state.tombstones[0].id, gone_rejected);
    let again = sync::collect_push(&s);
    assert!(again.iter().any(|c| c.id == gone_rejected && c.deleted));
    assert!(!again.iter().any(|c| c.id == gone_applied));
    // clock_skew and the unknown status stay dirty, never skipped.
    assert!(again.iter().any(|c| c.id == skew));
    assert!(again.iter().any(|c| c.id == unknown_status));
    // The others are skipped with the server's reason.
    let skipped = sync::skipped_entities(&mut s);
    let reason = |id: &str| {
        skipped
            .iter()
            .find(|x| x.id == id)
            .map(|x| x.reason.clone())
    };
    assert_eq!(reason(&odd).as_deref(), Some("invalid_json_value"));
    assert_eq!(reason(&viewer).as_deref(), Some("forbidden_role"));
    assert_eq!(reason(&big).as_deref(), Some("too_large"));
    assert_eq!(reason(&future).as_deref(), Some("rejected"));
    assert_eq!(reason(&skew), None);
    assert_eq!(skipped.len(), 4, "{skipped:?}");
    for id in [&odd, &viewer, &big, &future] {
        assert!(!again.iter().any(|c| &c.id == id));
    }
}

#[test]
fn push_responses_tolerate_unknown_statuses_and_types() {
    let resp: sync::PushResponse = serde_json::from_value(json!({
        "results": [
            {"type": "request", "id": "a", "status": "applied", "rev": 3},
            {"type": "request", "id": "b", "status": "queued"},
            {"type": "protoFile", "id": "c", "status": "applied", "rev": 4},
            {"type": "request", "id": "d", "status": "stale", "rev": 5, "fxNew": 1}
        ],
        "cursor": 5
    }))
    .unwrap();
    let ids: Vec<(&str, sync::PushStatus)> = resp
        .results
        .iter()
        .map(|r| (r.id.as_str(), r.status))
        .collect();
    assert_eq!(
        ids,
        vec![
            ("a", sync::PushStatus::Applied),
            ("b", sync::PushStatus::Unknown),
            ("d", sync::PushStatus::Stale)
        ]
    );
}

#[tokio::test]
async fn an_unknown_push_status_leaves_the_change_dirty() {
    let server = MockServer::start().await;
    let dir = tempfile::tempdir().unwrap();
    let mut s = connected_store(dir.path(), &server.uri());
    let pid = s.project_list()[0].id.clone();
    s.create_collection(&pid, "Mine").unwrap();
    serve_pull(&server, vec![], 0).await;
    Mock::given(method("POST"))
        .and(path("/v1/sync"))
        .respond_with(move |req: &WmRequest| {
            let body: Value = serde_json::from_slice(&req.body).unwrap();
            let results: Vec<Value> = body["changes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|c| json!({"type": c["type"], "id": c["id"], "status": "deferred"}))
                .collect();
            ResponseTemplate::new(200).set_body_json(json!({"results": results, "cursor": 0}))
        })
        .mount(&server)
        .await;
    let client = SyncClient::new(&server.uri(), Some("tok")).unwrap();
    let before = sync::collect_push(&s).len();
    let report = sync::sync_now(&client, &mut s).await.unwrap();
    assert_eq!(report.pushed, 0);
    assert_eq!(sync::collect_push(&s).len(), before, "still pending");
    assert!(sync::skipped_entities(&mut s).is_empty());
}

#[test]
fn pending_count_leaves_out_types_the_server_does_not_take() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    s.create_collection(&pid, "C").unwrap();
    let all = sync::collect_push(&s).len();
    assert_eq!(sync::pending_count(&s), all);
    s.sync_state.server_types = Some(
        ["project", "collection", "folder", "request"]
            .iter()
            .map(|t| t.to_string())
            .collect(),
    );
    let envs = s.environments.len();
    assert_eq!(sync::unsupported_entities(&s).len(), envs);
    assert_eq!(sync::pending_count(&s), all - envs);
}

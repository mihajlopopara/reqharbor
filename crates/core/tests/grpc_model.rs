//! gRPC requests in the data model (AC-GRPC-1, AC-GRPC-4, AC-GRPC-10,
//! AC-GRPC-12): the `grpc` sub-object's defaults and forward compatibility,
//! `minClientVersion` stamping, save merging, share / team-push path
//! stripping and the history summary.

use reqharbor_core::history::HistorySummary;
use reqharbor_core::model::*;
use reqharbor_core::share::{ShareOptions, build_snapshot, strip_file_references};
use reqharbor_core::storage::Store;
use reqharbor_core::sync::{ShareKind, strip_device_local_paths};
use serde_json::json;

fn grpc_json() -> serde_json::Value {
    json!({
        "id": "g1", "kind": "grpc", "url": "grpc://localhost:50051",
        "headers": [{"key": "authorization", "value": "Bearer grpc-s3cret", "enabled": true}],
        "grpc": {
            "service": "demo.Greeter", "method": "SayHello", "message": "{\"name\":\"x\"}",
            "protoSource": "files",
            "protoFiles": ["/home/me/protos/greeter.proto"],
            "importPaths": ["/home/me/protos"],
            "messages": [{"name": "Hi", "body": "{}", "fxMsg": 1}],
            "fxGrpc": {"newer": true}
        }
    })
}

#[test]
fn ac_grpc_1_defaults_and_unknown_keys_round_trip() {
    let r: Request =
        serde_json::from_value(json!({"id": "x", "kind": "grpc", "grpc": {}})).unwrap();
    let g = r.grpc.as_ref().unwrap();
    assert_eq!(g.message, "{}");
    assert_eq!(g.proto_source, ProtoSource::Reflection);
    assert!(g.proto_files.is_empty() && g.import_paths.is_empty() && g.messages.is_empty());
    assert!(r.is_editable());

    let r: Request = serde_json::from_value(grpc_json()).unwrap();
    let back = serde_json::to_value(&r).unwrap();
    assert_eq!(back["grpc"]["fxGrpc"], json!({"newer": true}));
    assert_eq!(back["grpc"]["messages"][0]["fxMsg"], 1);
    assert_eq!(back["grpc"]["protoSource"], "files");
    assert!(r.is_editable());
}

#[test]
fn ac_grpc_1_unknown_proto_source_is_read_only() {
    let mut v = grpc_json();
    v["grpc"]["protoSource"] = json!("registry");
    let r: Request = serde_json::from_value(v).unwrap();
    assert!(!r.is_editable());
    assert_eq!(
        serde_json::to_value(&r).unwrap()["grpc"]["protoSource"],
        "registry"
    );
}

#[test]
fn ac_grpc_1_min_client_version_is_stamped_and_never_lowered() {
    let mut r: Request = serde_json::from_value(grpc_json()).unwrap();
    r.stamp_min_client_version();
    assert_eq!(
        r.min_client_version.as_deref(),
        Some(GRPC_MIN_CLIENT_VERSION)
    );
    assert_eq!(GRPC_MIN_CLIENT_VERSION, "0.1.0");
    r.min_client_version = Some("0.0.5".into());
    r.stamp_min_client_version();
    assert_eq!(r.min_client_version.as_deref(), Some("0.1.0"));
    r.min_client_version = Some("0.1.0".into());
    r.stamp_min_client_version();
    assert_eq!(r.min_client_version.as_deref(), Some("0.1.0"));
    // An HTTP request is never stamped.
    let mut h = Request::default();
    h.stamp_min_client_version();
    assert_eq!(h.min_client_version, None);
}

fn store_with_collection() -> (tempfile::TempDir, Store, String) {
    let dir = tempfile::tempdir().unwrap();
    let mut s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let c = s.create_collection(&pid, "API").unwrap();
    (dir, s, c.id)
}

#[test]
fn ac_grpc_1_create_and_save_stamp_and_keep_grpc() {
    let (_d, mut s, cid) = store_with_collection();
    let template: Request = serde_json::from_value(grpc_json()).unwrap();
    let r = s.create_request(&cid, None, template).unwrap();
    assert_eq!(
        r.min_client_version.as_deref(),
        Some(GRPC_MIN_CLIENT_VERSION)
    );
    assert_eq!(r.kind, RequestKind::Grpc);
    // A UI that rebuilt the request without `grpc` (or its unknown keys) loses nothing.
    let mut edited = r.clone();
    edited.grpc = None;
    edited.min_client_version = None;
    edited.name = "Renamed".into();
    let saved = s.save_request(&edited).unwrap();
    assert_eq!(saved.grpc, r.grpc);
    assert_eq!(saved.min_client_version.as_deref(), Some("0.1.0"));
    let mut edited = saved.clone();
    let g = edited.grpc.as_mut().unwrap();
    g.extra.clear();
    g.messages[0].extra.clear();
    g.method = "LotsOfReplies".into();
    let saved = s.save_request(&edited).unwrap();
    let g = saved.grpc.unwrap();
    assert_eq!(g.method, "LotsOfReplies");
    assert!(g.extra.contains_key("fxGrpc"));
    assert!(g.messages[0].extra.contains_key("fxMsg"));
    // Switching back to HTTP keeps `grpc` for later.
    let mut http = saved_clone(&s, &r.id);
    http.kind = RequestKind::Http;
    let saved = s.save_request(&http).unwrap();
    assert!(saved.grpc.is_some());
}

fn saved_clone(s: &Store, id: &str) -> Request {
    s.request(id).cloned().unwrap()
}

#[test]
fn ac_grpc_12_shares_empty_proto_paths_and_handle_metadata_credentials() {
    let (_d, mut s, cid) = store_with_collection();
    let r = s
        .create_request(&cid, None, serde_json::from_value(grpc_json()).unwrap())
        .unwrap();
    for include in [false, true] {
        let opts = ShareOptions {
            include_credentials: include,
            environment_id: None,
        };
        let (snap, _) = build_snapshot(&s, ShareKind::Request, &r.id, &opts).unwrap();
        let g = &snap["request"]["grpc"];
        assert_eq!(g["protoFiles"], json!([]), "include={include}");
        assert_eq!(g["importPaths"], json!([]));
        assert_eq!(g["service"], "demo.Greeter");
        assert_eq!(g["method"], "SayHello");
        assert_eq!(g["message"], "{\"name\":\"x\"}");
        assert_eq!(snap.to_string().contains("grpc-s3cret"), include);
    }
    // Importing a crafted share clears paths with a warning.
    let mut crafted: Request = serde_json::from_value(grpc_json()).unwrap();
    let mut warnings = Vec::new();
    strip_file_references(&mut crafted, &mut warnings);
    let g = crafted.grpc.unwrap();
    assert!(g.proto_files.is_empty() && g.import_paths.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains(".proto"), "{warnings:?}");
    // A share carries no paths at all: a request that uses `.proto` files
    // still warns on import, one that uses reflection doesn't.
    let mut v = grpc_json();
    strip_device_local_paths(&mut v);
    let mut shared: Request = serde_json::from_value(v.clone()).unwrap();
    assert!(shared.grpc.as_ref().unwrap().proto_files.is_empty());
    let mut warnings = Vec::new();
    strip_file_references(&mut shared, &mut warnings);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains(".proto"), "{warnings:?}");
    v["grpc"]["protoSource"] = json!("reflection");
    let mut reflected: Request = serde_json::from_value(v).unwrap();
    let mut warnings = Vec::new();
    strip_file_references(&mut reflected, &mut warnings);
    assert!(warnings.is_empty(), "{warnings:?}");
}

#[test]
fn ac_grpc_4_team_pushes_leave_proto_paths_out() {
    let mut v = grpc_json();
    strip_device_local_paths(&mut v);
    assert_eq!(v["grpc"]["protoFiles"], json!([]));
    assert_eq!(v["grpc"]["importPaths"], json!([]));
    assert_eq!(v["grpc"]["service"], "demo.Greeter");
}

#[test]
fn ac_grpc_10_history_summary_leaves_bodies_out() {
    let entry = HistoryEntry {
        id: "h".into(),
        timestamp: 1,
        source_request_id: None,
        request: Request::default(),
        resolved_url: "grpc://localhost:1/demo.Greeter/SayHello".into(),
        method: "GRPC".into(),
        response: None,
        error: None,
        ws: None,
        grpc: Some(GrpcCallSummary {
            service: "demo.Greeter".into(),
            method: "SayHello".into(),
            call_kind: GrpcCallKind::Unary,
            duration_ms: 5,
            sent: 1,
            received: 1,
            status: Some(GrpcStatus {
                code: 5,
                name: "NOT_FOUND".into(),
                message: "nope".into(),
                details: vec![GrpcStatusDetail {
                    type_url: "t".into(),
                    json: None,
                    base64: "".into(),
                }],
            }),
            response: Some("{}".into()),
            response_truncated: false,
            headers: Some(vec![("a".into(), "b".into())]),
            trailers: Some(vec![]),
            extra: Default::default(),
        }),
    };
    let v = serde_json::to_value(&entry).unwrap();
    assert_eq!(v["grpc"]["callKind"], "unary");
    let back: HistoryEntry = serde_json::from_value(v).unwrap();
    assert_eq!(back, entry);
    let s = HistorySummary::from_entry(&entry);
    let g = s.grpc.unwrap();
    assert!(g.response.is_none() && g.headers.is_none() && g.trailers.is_none());
    let st = g.status.unwrap();
    assert_eq!(st.code, 5);
    assert!(st.details.is_empty());
}

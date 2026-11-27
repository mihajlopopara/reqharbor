//! QA round 1 — variables (AC-ENV-1/2/3/7), storage (AC-COL-6) and history
//! (AC-HIST-1) adversarial tests.
//!
//! Tests whose name starts with `bug_` are EXPECTED TO FAIL until the
//! referenced QA-R1-xxx bug is fixed — they are the repro.
//!
//! Run: docker compose run --rm dev cargo test -p reqharbor-core --test qa_store_vars

use reqharbor_core::CancellationToken;
use reqharbor_core::history::{HISTORY_LIMIT, History};
use reqharbor_core::http::HttpEngine;
use reqharbor_core::model::*;
use reqharbor_core::prepare::prepare;
use reqharbor_core::send::send_request;
use reqharbor_core::storage::{Store, atomic_write};
use reqharbor_core::vars::VarScope;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn env(id: &str, project: Option<&str>, vars: Vec<Variable>) -> Environment {
    Environment {
        id: id.into(),
        workspace_id: "w".into(),
        project_id: project.map(str::to_string),
        name: id.into(),
        variables: vars,
        sort_order: 0,
        created_at: 0,
        updated_at: 0,
        extra: Default::default(),
    }
}

fn disabled(mut v: Variable) -> Variable {
    v.enabled = false;
    v
}

// ---------------------------------------------------------------- variables

#[test]
fn precedence_project_then_global_and_disabled_falls_through() {
    let project = env(
        "p",
        Some("P"),
        vec![
            Variable::new("host", "project.dev"),
            disabled(Variable::new("port", "1111")),
        ],
    );
    let global = env(
        "g",
        None,
        vec![
            Variable::new("host", "global.dev"),
            Variable::new("port", "2222"),
            Variable::new("only", "g"),
        ],
    );
    let s = VarScope::new(Some(&project), Some(&global));
    let (out, unresolved) = s.resolve_simple("{{host}}:{{ port }}/{{only}}/{{nope}}");
    assert_eq!(out, "project.dev:2222/g/{{nope}}");
    assert_eq!(unresolved, vec!["nope"]);
}

#[test]
fn cycles_self_reference_and_depth() {
    let g = env(
        "g",
        None,
        vec![
            Variable::new("self", "x{{self}}"),
            Variable::new("a", "A{{b}}"),
            Variable::new("b", "B{{a}}"),
            Variable::new("d1", "{{d2}}"),
            Variable::new("d2", "{{d3}}"),
            Variable::new("d3", "{{d4}}"),
            Variable::new("d4", "deep"),
            Variable::new("e1", "{{e2}}"),
            Variable::new("e2", "{{e3}}"),
            Variable::new("e3", "{{e4}}"),
            Variable::new("e4", "{{e5}}"),
            Variable::new("e5", "too-deep"),
        ],
    );
    let s = VarScope::new(None, Some(&g));
    assert_eq!(s.resolve_simple("[{{self}}]").0, "[{{self}}]");
    assert_eq!(s.resolve_simple("[{{a}}]").0, "[{{a}}]");
    // AC-ENV-2: values may reference other variables up to depth 3
    assert_eq!(s.resolve_simple("{{d1}}").0, "deep");
    assert_ne!(s.resolve_simple("{{e1}}").0, "too-deep");
}

#[test]
fn malformed_references() {
    let g = env(
        "g",
        None,
        vec![Variable::new("a", "1"), Variable::new("a.b-c_d", "2")],
    );
    let s = VarScope::new(None, Some(&g));
    assert_eq!(s.resolve_simple("{{{a}}}").0, "{1}");
    assert_eq!(s.resolve_simple("{{a b}}").0, "{{a b}}");
    assert_eq!(s.resolve_simple("{{}}").0, "{{}}");
    assert_eq!(s.resolve_simple("{{a").0, "{{a");
    assert_eq!(s.resolve_simple("{{ a.b-c_d }}").0, "2");
    assert_eq!(s.resolve_simple("{{\ta\n}}").0, "1");
    assert_eq!(s.resolve_simple("{{é}}").0, "{{é}}");
}

/// AC-ENV-1: variables resolve in URL, params, headers (name and value), auth
/// fields, body text, form values and file paths.
#[test]
fn ac_env_1_resolution_everywhere() {
    let g = env(
        "g",
        None,
        vec![
            Variable::new("h", "api.x.dev"),
            Variable::new("hn", "X-Trace"),
            Variable::new("v", "val"),
            Variable::new("dir", "/tmp"),
        ],
    );
    let s = VarScope::new(None, Some(&g));
    let mut r = Request {
        method: "POST".into(),
        url: "{{h}}/a".into(),
        params: vec![KeyValue::new("q", "{{v}}")],
        headers: vec![KeyValue::new("{{hn}}", "{{v}}")],
        auth: Auth::basic("{{v}}", "{{v}}"),
        ..Request::default()
    };
    r.body = Body::form(vec![
        FormField::text("t", "{{v}}"),
        FormField::file("f", "{{dir}}/x.bin"),
    ]);
    let p = prepare(&r, &s, None).unwrap();
    assert_eq!(p.url, "https://api.x.dev/a?q=val");
    assert_eq!(p.header("X-Trace"), Some("val"));
    assert!(p.unresolved.is_empty(), "{:?}", p.unresolved);
    match p.body.unwrap() {
        reqharbor_core::prepare::PreparedBody::Multipart { parts, .. } => {
            assert_eq!(parts[0].value, "val");
            assert_eq!(parts[1].value, "/tmp/x.bin");
        }
        other => panic!("{other:?}"),
    }
}

/// AC-ENV-7: secret values are never written to history. The resolved URL is
/// masked (`{{token}}`), but when the send fails the reqwest error message —
/// which embeds the full resolved URL — is stored verbatim in
/// `history.jsonl` (`error.message`), leaking the secret to disk.
#[tokio::test]
async fn bug_qa_r1_001_secret_leaks_into_history_via_error_message() {
    let g = env(
        "g",
        None,
        vec![Variable::new("token", "TOPSECRET123").secret()],
    );
    let s = VarScope::new(None, Some(&g));
    // Port 9 on localhost: connection refused -> reqwest error with URL.
    let r = Request {
        method: "GET".into(),
        url: "http://127.0.0.1:9/v1/items?api_key={{token}}".into(),
        ..Request::default()
    };
    let engine = HttpEngine::new().unwrap();
    let out = send_request(&engine, &r, &s, None, &CancellationToken::new())
        .await
        .unwrap();
    assert!(out.error.is_some());
    let line = serde_json::to_string(&out.history).unwrap();
    assert!(
        line.contains("{{token}}"),
        "resolvedUrl should keep the placeholder"
    );
    assert!(
        !line.contains("TOPSECRET123"),
        "secret value written to history: {}",
        out.history.error.as_ref().unwrap().message
    );
}

/// Same leak through an invalid-URL error (secret in the host part).
#[tokio::test]
async fn bug_qa_r1_001b_secret_leaks_via_invalid_url_error() {
    let g = env("g", None, vec![Variable::new("tok", "SECRET 2").secret()]);
    let s = VarScope::new(None, Some(&g));
    let r = Request {
        method: "GET".into(),
        url: "http://exa mple.com/{{tok}}".into(),
        ..Request::default()
    };
    let engine = HttpEngine::new().unwrap();
    let out = send_request(&engine, &r, &s, None, &CancellationToken::new())
        .await
        .unwrap();
    let line = serde_json::to_string(&out.history).unwrap();
    assert!(!line.contains("SECRET 2"), "{line}");
}

// ------------------------------------------------------------------ storage

#[test]
fn ac_col_6_corrupt_files_quarantined_and_rest_survives() {
    let dir = tempfile::tempdir().unwrap();
    let pid;
    {
        let mut s = Store::open(dir.path()).unwrap();
        let p2 = s.create_project("Second").unwrap();
        pid = p2.project.id.clone();
        let c = s.create_collection(&pid, "C").unwrap();
        s.create_request(&c.id, None, Request::default()).unwrap();
        let mut g = s.global_environment().unwrap().clone();
        g.variables.push(Variable::new("k", "v"));
        s.update_environment(&g).unwrap();
    }
    // truncate the second project half-way, corrupt environments with invalid UTF-8
    let pfile = dir.path().join("projects").join(format!("{pid}.json"));
    let bytes = std::fs::read(&pfile).unwrap();
    std::fs::write(&pfile, &bytes[..bytes.len() / 2]).unwrap();
    std::fs::write(dir.path().join("environments.json"), b"\xff\xfe{").unwrap();

    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.corrupt.len(), 2, "{:?}", s.corrupt);
    for c in &s.corrupt {
        assert!(std::path::Path::new(&c.renamed_to).exists(), "{c:?}");
    }
    // the healthy Default project is still there; the workspace was not reset
    assert_eq!(s.projects.len(), 1);
    assert!(s.global_environment().is_some());
}

/// Corrupt `workspace.json` silently disconnects sync and mints a new
/// workspace id (reported in corruptFiles, but the remote link is lost).
#[test]
fn corrupt_workspace_json_keeps_projects() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut s = Store::open(dir.path()).unwrap();
        s.create_project("Mine").unwrap();
    }
    std::fs::write(dir.path().join("workspace.json"), b"{\"schemaVersion\":1,").unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert_eq!(s.corrupt.len(), 1);
    assert_eq!(
        s.projects.len(),
        2,
        "projects must survive a corrupt workspace.json"
    );
}

/// Concurrent atomic writes to one path from many threads while a reader
/// parses it: the reader must never observe a partial file.
#[test]
fn atomic_write_under_concurrency_never_exposes_partial_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = Arc::new(dir.path().join("x.json"));
    atomic_write(&path, b"{\"n\":0}").unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader = {
        let path = path.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut reads = 0;
            while !stop.load(Ordering::Relaxed) {
                let b = std::fs::read(&*path).unwrap();
                serde_json::from_slice::<serde_json::Value>(&b)
                    .unwrap_or_else(|e| panic!("partial read after {reads}: {e}"));
                reads += 1;
            }
            reads
        })
    };
    let writers: Vec<_> = (0..8)
        .map(|t| {
            let path = path.clone();
            std::thread::spawn(move || {
                for i in 0..40 {
                    let big = "x".repeat(10_000 * (t + 1));
                    let v = serde_json::json!({ "t": t, "i": i, "pad": big });
                    atomic_write(&path, serde_json::to_vec(&v).unwrap().as_slice()).unwrap();
                }
            })
        })
        .collect();
    for w in writers {
        w.join().unwrap();
    }
    stop.store(true, Ordering::Relaxed);
    assert!(reader.join().unwrap() > 0);
    // no temp files left behind
    let leftovers: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(leftovers.is_empty());
}

/// Two Store instances on one data dir (e.g. the app started twice, or a
/// sync pull racing a save): last writer wins; the store must still open.
#[test]
fn two_stores_same_dir_last_writer_wins_but_valid() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = Store::open(dir.path()).unwrap();
    let mut b = Store::open(dir.path()).unwrap();
    let pid = a.project_list()[0].id.clone();
    a.create_collection(&pid, "from A").unwrap();
    b.create_collection(&pid, "from B").unwrap();
    let s = Store::open(dir.path()).unwrap();
    assert!(s.corrupt.is_empty());
    let names: Vec<String> = s
        .project(&pid)
        .unwrap()
        .collections
        .iter()
        .map(|c| c.name.clone())
        .collect();
    // documents the lost update (A's collection is gone)
    assert_eq!(names, vec!["from B".to_string()]);
}

/// `save_all` (run after every sync pull) deletes any `projects/*.json`
/// whose stem is not a loaded project id — including user backups or a
/// second file carrying the same project id, without quarantine or report.
#[test]
fn bug_qa_r1_021_save_all_deletes_unrecognised_project_files() {
    let dir = tempfile::tempdir().unwrap();
    let s = Store::open(dir.path()).unwrap();
    let pid = s.project_list()[0].id.clone();
    let src = dir.path().join("projects").join(format!("{pid}.json"));
    let backup = dir
        .path()
        .join("projects")
        .join("backup-before-upgrade.json");
    std::fs::copy(&src, &backup).unwrap();
    drop(s);
    let s = Store::open(dir.path()).unwrap();
    s.save_all().unwrap();
    assert!(
        backup.exists(),
        "user file in projects/ was deleted by save_all"
    );
}

// ------------------------------------------------------------------ history

fn entry(i: usize) -> HistoryEntry {
    HistoryEntry {
        id: format!("h{i:05}"),
        timestamp: i as i64,
        source_request_id: None,
        request: Request::default(),
        resolved_url: format!("https://x.dev/{i}"),
        method: "GET".into(),
        response: None,
        error: None,
        ws: None,
        grpc: None,
    }
}

#[test]
fn ac_hist_1_cap_500_newest_kept_across_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    {
        let mut h = History::open(path.clone()).unwrap();
        for i in 0..1234 {
            h.append(&entry(i)).unwrap();
        }
        let list = h.list();
        assert_eq!(list.len(), HISTORY_LIMIT);
        assert_eq!(list[0].id, "h01233");
        assert_eq!(list.last().unwrap().id, "h00734");
    }
    let lines = std::fs::read_to_string(&path).unwrap().lines().count();
    assert!(lines <= HISTORY_LIMIT + 50, "{lines}");
    let h = History::open(path.clone()).unwrap();
    assert_eq!(h.list().len(), HISTORY_LIMIT);
    let lines = std::fs::read_to_string(&path).unwrap().lines().count();
    assert_eq!(lines, HISTORY_LIMIT, "compacted to newest 500 on open");
    assert_eq!(h.get("h01233").unwrap().resolved_url, "https://x.dev/1233");
    assert!(h.get("h00001").is_err());
}

#[test]
fn history_survives_torn_last_line_and_garbage_lines() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.jsonl");
    {
        let mut h = History::open(path.clone()).unwrap();
        for i in 0..5 {
            h.append(&entry(i)).unwrap();
        }
    }
    let mut data = std::fs::read(&path).unwrap();
    data.extend_from_slice(b"garbage\n\xff\xfe\n{\"id\":\"torn\",\"timest");
    std::fs::write(&path, data).unwrap();
    let mut h = History::open(path.clone()).unwrap();
    assert_eq!(h.list().len(), 5);
    h.append(&entry(99)).unwrap();
    assert_eq!(h.get("h00099").unwrap().method, "GET");
    h.delete("h00002").unwrap();
    assert_eq!(h.list().len(), 5);
    assert!(h.get("h00004").is_ok());
    h.clear().unwrap();
    assert!(h.list().is_empty());
}

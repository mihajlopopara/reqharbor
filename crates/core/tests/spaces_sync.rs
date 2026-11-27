//! Personal sync and bindings (04 §12 M1d, 06-go-server §10 C2) against a
//! wiremock model of the Go server: `/v1/server`, `/v1/me/spaces`,
//! `/v1/spaces/{id}/sync` (LWW, revs, `updatedBy`, `serverTime`, per-change
//! `clock_skew` and `unsupported_type`, `syncEpoch`), the token endpoint and
//! legacy `/v1/sync`.

use reqharbor_core::auth::{AuthClient, MySpace, MySpaces, Session, Tokens};
use reqharbor_core::model::*;
use reqharbor_core::secrets::{MemorySecretStore, SecretStore};
use reqharbor_core::space_sync::{self, RUN_BUDGET, StoreAccess};
use reqharbor_core::spaces::{
    AccountIdent, BindingState, LEGACY_SPACE, LegacyMode, Role, Scope, SpaceBinding, SpaceKind,
    legacy_key,
};
use reqharbor_core::storage::{Store, sync_key};
use reqharbor_core::sync::{self, SyncClient, SyncError};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use wiremock::{Mock, MockServer, Request as WmRequest, Respond, ResponseTemplate, matchers::any};

const USER: &str = "01JUSERPRIYA0000000000000";
const PERSONAL: &str = "01JSPACEPERSONAL000000000";
const TEAM: &str = "01JSPACETEAM0000000000000";
const INSTANCE: &str = "01JDINSTANCE0000000000000";
const SKEW_MS: i64 = 300_000;

#[derive(Default, Clone)]
struct Ent {
    rev: u64,
    updated_at: i64,
    device_id: String,
    deleted: bool,
    data: Option<Value>,
}

#[derive(Default)]
struct Space {
    rev: u64,
    entities: BTreeMap<(String, String), Ent>,
}

impl Space {
    fn live(&self, t: &str) -> Vec<String> {
        self.entities
            .iter()
            .filter(|((ty, _), e)| ty == t && !e.deleted)
            .map(|((_, id), _)| id.clone())
            .collect()
    }
}

struct State {
    instance_id: String,
    epoch: String,
    types: Vec<String>,
    /// `/v1/me/spaces` rows; `None` makes it answer 503.
    listed: Option<Vec<Value>>,
    spaces: BTreeMap<String, Space>,
    /// Space id → (status, code) answered on every sync call.
    fail: BTreeMap<String, (u16, String)>,
    access: String,
    refresh: String,
    refreshes: usize,
    /// Added to the real clock for `serverTime` and the skew check.
    clock_offset_ms: i64,
    legacy_token: String,
    legacy_disabled: bool,
    /// (space or "legacy", pushed changes).
    pushes: Vec<(String, Vec<Value>)>,
    /// Space id → the caller's role there, enforced like the Go server
    /// (06 §10 M2): a viewer push is refused whole with 403
    /// `forbidden_role`; a project tombstone below admin is refused per
    /// change. No entry: no enforcement.
    roles: BTreeMap<String, String>,
    /// Spaces whose push applies but whose response is lost (503).
    lose_push_response: std::collections::BTreeSet<String>,
}

impl Default for State {
    fn default() -> Self {
        let mut spaces = BTreeMap::new();
        spaces.insert(PERSONAL.to_string(), Space::default());
        spaces.insert(LEGACY_SPACE.to_string(), Space::default());
        State {
            instance_id: INSTANCE.into(),
            epoch: "EPOCH1".into(),
            types: ["project", "collection", "folder", "request", "environment"]
                .map(String::from)
                .to_vec(),
            listed: Some(vec![personal_row()]),
            spaces,
            fail: BTreeMap::new(),
            access: "rha_ACCESS1".into(),
            refresh: "rhr_REFRESH1".into(),
            refreshes: 0,
            clock_offset_ms: 0,
            legacy_token: "rh_LEGACY".into(),
            legacy_disabled: false,
            pushes: Vec::new(),
            roles: BTreeMap::new(),
            lose_push_response: Default::default(),
        }
    }
}

fn personal_row() -> Value {
    json!({"spaceId": PERSONAL, "kind": "personal", "teamId": null, "teamName": null, "role": "owner",
           "bytesUsed": 1024, "quotaBytes": 104857600})
}

fn team_row(role: &str) -> Value {
    json!({"spaceId": TEAM, "kind": "team", "teamId": "T1", "teamName": "Acme", "role": role,
           "bytesUsed": 0, "quotaBytes": 262144000})
}

#[derive(Clone, Default)]
struct Go(Arc<Mutex<State>>);

fn err(status: u16, code: &str) -> ResponseTemplate {
    ResponseTemplate::new(status).set_body_json(json!({"error": {"code": code, "message": code}}))
}

fn pull(space: &Space, since: u64, epoch: &str) -> Value {
    let mut rows: Vec<(&(String, String), &Ent)> = space
        .entities
        .iter()
        .filter(|(_, e)| e.rev > since)
        .collect();
    rows.sort_by_key(|(_, e)| e.rev);
    let has_more = rows.len() > 500;
    rows.truncate(500);
    let cursor = rows.last().map(|(_, e)| e.rev).unwrap_or(space.rev);
    let changes: Vec<Value> = rows
        .iter()
        .map(|((t, id), e)| {
            json!({"type": t, "id": id, "updatedAt": e.updated_at, "deleted": e.deleted, "data": e.data,
                   "rev": e.rev, "deviceId": e.device_id, "updatedBy": {"id": USER, "name": "Priya Shah"}})
        })
        .collect();
    json!({"changes": changes, "cursor": cursor, "hasMore": has_more, "syncEpoch": epoch})
}

fn push(
    st: &mut State,
    space_id: &str,
    body: &Value,
    per_change_types: bool,
) -> Result<Value, Box<ResponseTemplate>> {
    let now = now_ms() + st.clock_offset_ms;
    let types = st.types.clone();
    let changes = body["changes"].as_array().cloned().unwrap_or_default();
    st.pushes.push((space_id.to_string(), changes.clone()));
    let below_admin = st
        .roles
        .get(space_id)
        .is_some_and(|r| r != "admin" && r != "owner");
    let space = st.spaces.entry(space_id.to_string()).or_default();
    let mut results = Vec::new();
    for c in changes {
        let t = c["type"].as_str().unwrap().to_string();
        let id = c["id"].as_str().unwrap().to_string();
        if !types.contains(&t) {
            if per_change_types {
                results.push(json!({"type": t, "id": id, "status": "rejected", "reason": "unsupported_type"}));
                continue;
            }
            return Err(Box::new(err(400, "unsupported_type")));
        }
        let updated = c["updatedAt"].as_i64().unwrap();
        if per_change_types && updated > now + SKEW_MS {
            results
                .push(json!({"type": t, "id": id, "status": "rejected", "reason": "clock_skew"}));
            continue;
        }
        let deleted = c["deleted"].as_bool().unwrap();
        if below_admin && deleted && t == "project" {
            results.push(
                json!({"type": t, "id": id, "status": "rejected", "reason": "forbidden_role"}),
            );
            continue;
        }
        let device = c["deviceId"].as_str().unwrap_or("").to_string();
        space.rev += 1;
        let rev = space.rev;
        let e = space.entities.entry((t.clone(), id.clone())).or_default();
        let status =
            if e.rev > 0 && (e.updated_at, e.device_id.as_str()) > (updated, device.as_str()) {
                "stale"
            } else {
                e.updated_at = updated;
                e.device_id = device;
                e.deleted = deleted;
                e.data = if deleted {
                    None
                } else {
                    Some(c["data"].clone())
                };
                "applied"
            };
        e.rev = rev;
        results.push(json!({"type": t, "id": id, "status": status, "rev": rev}));
    }
    let mut out = json!({"results": results, "cursor": space.rev});
    if per_change_types {
        out["serverTime"] = json!(now);
    }
    Ok(out)
}

impl Respond for Go {
    fn respond(&self, req: &WmRequest) -> ResponseTemplate {
        let mut st = self.0.lock().unwrap();
        let path = req.url.path().to_string();
        let m = req.method.as_str().to_string();
        let q: BTreeMap<String, String> = req.url.query_pairs().into_owned().collect();
        let bearer = req
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .trim_start_matches("Bearer ")
            .to_string();
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        if (m.as_str(), path.as_str()) == ("GET", "/v1/server") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "software": "reqharbor-server", "version": "0.2.0", "apiVersion": 1,
                "instanceId": st.instance_id, "syncEpoch": st.epoch, "name": "Acme",
                "publicUrl": "http://x", "minClientVersion": null,
                "auth": {"methods": [{"id": "password", "label": "Email"}], "signup": "invite", "passwordReset": "admin", "setupRequired": false},
                "features": {"accounts": true, "teams": false, "legacyWorkspaces": "existing"},
                "sync": {"types": st.types, "requestKinds": ["http"]}
            }));
        }
        if (m.as_str(), path.as_str()) == ("POST", "/v1/auth/token") {
            let form: BTreeMap<String, String> = url::form_urlencoded::parse(&req.body)
                .into_owned()
                .collect();
            if form.get("grant_type").map(String::as_str) == Some("refresh_token")
                && form.get("refresh_token") == Some(&st.refresh)
            {
                st.refreshes += 1;
                st.access = format!("rha_ACCESS{}", st.refreshes + 1);
                st.refresh = format!("rhr_REFRESH{}", st.refreshes + 1);
                return ResponseTemplate::new(200).set_body_json(json!({
                    "access_token": st.access, "refresh_token": st.refresh, "token_type": "Bearer",
                    "expires_in": 3600, "session_id": "S1"
                }));
            }
            return err(400, "invalid_grant");
        }
        // Legacy token routes.
        if path == "/v1/sync" || path == "/v1/workspace" {
            if bearer != st.legacy_token {
                return err(401, "unauthorized");
            }
            if st.legacy_disabled {
                return err(403, "legacy_disabled");
            }
            return match (m.as_str(), path.as_str()) {
                ("GET", "/v1/workspace") => ResponseTemplate::new(200)
                    .set_body_json(json!({"workspaceId": "LEGACYWS", "name": "W", "createdAt": 1, "entityCount": 0})),
                ("GET", "/v1/sync") => {
                    let since = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
                    ResponseTemplate::new(200).set_body_json(pull(&st.spaces[LEGACY_SPACE], since, ""))
                }
                ("POST", "/v1/sync") => match push(&mut st, LEGACY_SPACE, &body, false) {
                    Ok(v) => ResponseTemplate::new(200).set_body_json(v),
                    Err(r) => *r,
                },
                _ => err(405, "method_not_allowed"),
            };
        }
        // Account routes.
        if bearer != st.access {
            return err(401, "auth_required");
        }
        if (m.as_str(), path.as_str()) == ("GET", "/v1/me/spaces") {
            return match &st.listed {
                Some(rows) => ResponseTemplate::new(200)
                    .set_body_json(json!({"spaces": rows, "departed": []})),
                None => err(503, "starting"),
            };
        }
        if let Some(rest) = path.strip_prefix("/v1/spaces/")
            && let Some(id) = rest.strip_suffix("/sync")
        {
            if let Some((status, code)) = st.fail.get(id).cloned() {
                return err(status, &code);
            }
            if !st.spaces.contains_key(id) {
                return err(404, "space_not_found");
            }
            let epoch = st.epoch.clone();
            return match m.as_str() {
                "GET" => {
                    let since = q.get("since").and_then(|s| s.parse().ok()).unwrap_or(0);
                    ResponseTemplate::new(200).set_body_json(pull(&st.spaces[id], since, &epoch))
                }
                "POST" => {
                    if st.roles.get(id).is_some_and(|r| r == "viewer") {
                        return err(403, "forbidden_role");
                    }
                    let lost = st.lose_push_response.contains(id);
                    match push(&mut st, id, &body, true) {
                        Ok(_) if lost => err(503, "starting"),
                        Ok(v) => ResponseTemplate::new(200).set_body_json(v),
                        Err(r) => *r,
                    }
                }
                _ => err(405, "method_not_allowed"),
            };
        }
        err(404, "not_found")
    }
}

async fn go() -> (MockServer, Go) {
    let server = MockServer::start().await;
    let g = Go::default();
    Mock::given(any())
        .respond_with(g.clone())
        .mount(&server)
        .await;
    (server, g)
}

fn http_req(method: &str, url: &str) -> Request {
    Request {
        method: method.into(),
        url: url.into(),
        ..Request::default()
    }
}

fn ident(server: &MockServer) -> AccountIdent {
    AccountIdent {
        server_url: server.uri(),
        server_instance_id: INSTANCE.into(),
        user_id: USER.into(),
        email: "priya@acme.dev".into(),
    }
}

fn session(server: &MockServer, g: &Go) -> Arc<Session> {
    let (access, refresh) = {
        let st = g.0.lock().unwrap();
        (st.access.clone(), st.refresh.clone())
    };
    Arc::new(Session::new(
        AuthClient::new(&server.uri()).unwrap(),
        &server.uri(),
        USER,
        Arc::new(MemorySecretStore::new()),
        Tokens {
            access_token: access,
            access_expires_at: now_ms() + 3_600_000,
            refresh_token: refresh,
            session_id: "S1".into(),
        },
    ))
}

struct Device {
    _dir: tempfile::TempDir,
    store: Mutex<Store>,
    secrets: MemorySecretStore,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        Device {
            _dir: dir,
            store: Mutex::new(store),
            secrets: MemorySecretStore::new(),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        self.store.with(f)
    }

    fn sign_in(&self, server: &MockServer) {
        let who = ident(server);
        self.with(|s| s.sign_in_migrate(who, &self.secrets).unwrap());
    }

    async fn run(&self, server: &MockServer, g: &Go) -> space_sync::AccountRun {
        space_sync::run(&self.store, Some(session(server, g)), RUN_BUDGET).await
    }

    fn reopen(&self) -> Store {
        Store::open(self.with(|s| s.dir().to_path_buf())).unwrap()
    }

    fn project_named(&self, name: &str) -> Option<String> {
        self.with(|s| {
            s.project_list()
                .into_iter()
                .find(|p| p.name == name)
                .map(|p| p.id)
        })
    }
}

/// A device with one personal project "Work" (a collection and a request).
async fn personal_device(server: &MockServer, g: &Go) -> (Device, String) {
    let d = Device::new();
    d.sign_in(server);
    d.run(server, g).await; // reconcile: personal binding, Default → Personal
    let pid = d.with(|s| {
        let p = s.create_project("Work").unwrap().project.id;
        let c = s.create_collection(&p, "API").unwrap();
        s.create_request(&c.id, None, http_req("GET", "https://x/a"))
            .unwrap();
        p
    });
    (d, pid)
}

fn bind_team(d: &Device, server: &MockServer, role: Role) {
    d.with(|s| {
        if let Some(b) = s.binding_mut(TEAM) {
            b.role = Some(role);
            s.save_bindings().unwrap();
            return;
        }
        s.bindings.spaces.push(SpaceBinding {
            space_id: TEAM.into(),
            kind: SpaceKind::Team,
            team_id: Some("T1".into()),
            team_name: Some("Acme".into()),
            role: Some(role),
            server_origin: server.uri(),
            server_instance_id: INSTANCE.into(),
            user_id: USER.into(),
            user_email: "priya@acme.dev".into(),
            state: BindingState::Active,
            bytes_used: None,
            quota_bytes: None,
            decide_later_at: None,
            member_count: None,
            extra: Extra::new(),
        });
        s.save_bindings().unwrap();
    });
}

fn seed(g: &Go, space: &str, t: &str, id: &str, updated_at: i64, data: Value) {
    let mut st = g.0.lock().unwrap();
    let sp = st.spaces.entry(space.to_string()).or_default();
    sp.rev += 1;
    let rev = sp.rev;
    sp.entities.insert(
        (t.into(), id.into()),
        Ent {
            rev,
            updated_at,
            device_id: "other-device".into(),
            deleted: false,
            data: Some(data),
        },
    );
}

fn tombstone(g: &Go, space: &str, t: &str, id: &str, at: i64) {
    let mut st = g.0.lock().unwrap();
    let sp = st.spaces.entry(space.to_string()).or_default();
    sp.rev += 1;
    let rev = sp.rev;
    let e = sp.entities.entry((t.into(), id.into())).or_default();
    e.rev = rev;
    e.updated_at = at;
    e.deleted = true;
    e.data = None;
    e.device_id = "other-device".into();
}

fn pushed_ids(g: &Go, space: &str) -> Vec<String> {
    g.0.lock()
        .unwrap()
        .pushes
        .iter()
        .filter(|(s, _)| s == space)
        .flat_map(|(_, c)| c.iter().map(|x| x["id"].as_str().unwrap().to_string()))
        .collect()
}

// ───────────────────────────── migration ─────────────────────────────

#[tokio::test]
async fn sign_in_migration_moves_token_sync_out_of_workspace_json_and_keeps_the_cursor() {
    let (server, g) = go().await;
    let d = Device::new();
    let pid = d.with(|s| s.project_list()[0].id.clone());
    // Token sync with some history: cursor, a synced key and a tombstone.
    d.with(|s| {
        s.workspace.sync = Some(SyncConfig {
            worker_url: server.uri(),
            remote_workspace_id: "LEGACYWS".into(),
            token: "rh_LEGACY".into(),
            cursor: 42,
        });
        s.sync_state
            .synced
            .insert(sync_key(EntityType::Project, &pid), 7);
        s.sync_state.tombstones.push(Tombstone {
            id: "gone".into(),
            entity_type: EntityType::Request,
            deleted_at: 9,
            extra: Extra::new(),
        });
        s.save_workspace().unwrap();
        s.save_sync_state().unwrap();
    });
    let device_id = d.with(|s| s.device_id().to_string());
    let report = d.with(|s| s.sign_in_migrate(ident(&server), &d.secrets).unwrap());
    assert!(report.migrated && report.legacy);

    let s = d.reopen();
    assert!(s.account_mode());
    assert!(
        s.workspace.sync.is_none(),
        "sync is gone from workspace.json"
    );
    let raw = std::fs::read_to_string(s.dir().join("workspace.json")).unwrap();
    assert!(
        !raw.contains("rh_LEGACY") && !raw.contains("\"sync\""),
        "{raw}"
    );
    // The cursor and bookkeeping moved into sync-spaces.json["legacy"].
    let legacy = s.space_sync(&Scope::Legacy);
    assert_eq!(legacy.cursor, 42);
    assert_eq!(
        legacy
            .state
            .synced
            .get(&sync_key(EntityType::Project, &pid)),
        Some(&7)
    );
    assert_eq!(legacy.state.tombstones.len(), 1);
    // sync-state.json keeps only the device id.
    assert_eq!(s.sync_state.device_id, device_id);
    assert!(s.sync_state.synced.is_empty() && s.sync_state.tombstones.is_empty());
    // Every project and the Global are bound to legacy; the token is a secret.
    assert_eq!(s.bindings.project_spaces[&pid], LEGACY_SPACE);
    assert_eq!(s.bindings.global_space.as_deref(), Some(LEGACY_SPACE));
    assert_eq!(s.bindings.legacy.as_ref().unwrap().mode, LegacyMode::Sync);
    assert_eq!(
        d.secrets.get(&legacy_key("LEGACYWS")).unwrap().as_deref(),
        Some("rh_LEGACY")
    );
    let account = std::fs::read_to_string(s.dir().join("account.json")).unwrap();
    assert!(!account.contains("rh_LEGACY"), "no token in account.json");
    // Nothing to ask: every project is legacy-bound.
    assert!(s.first_sync_candidates().is_empty());

    // Token sync keeps working in account mode (shares-capable legacy scope).
    d.with(|s| s.legacy_token = Some("rh_LEGACY".into()));
    let run = d.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    assert!(pushed_ids(&g, LEGACY_SPACE).contains(&pid));
    assert!(
        !pushed_ids(&g, PERSONAL).contains(&pid),
        "nothing leaks into Personal"
    );
}

#[tokio::test]
async fn a_failed_secret_write_leaves_token_sync_untouched() {
    let (server, _g) = go().await;
    let d = Device::new();
    d.with(|s| {
        s.workspace.sync = Some(SyncConfig {
            worker_url: server.uri(),
            remote_workspace_id: "W".into(),
            token: "rh_T".into(),
            cursor: 3,
        });
        s.save_workspace().unwrap();
    });
    d.secrets.set_fail_writes(true);
    let e = d.with(|s| s.sign_in_migrate(ident(&server), &d.secrets));
    assert!(e.is_err());
    let s = d.reopen();
    assert!(!s.account_mode());
    assert_eq!(s.workspace.sync.as_ref().unwrap().cursor, 3);
}

#[tokio::test]
async fn first_sync_binds_only_the_checked_projects() {
    let (server, g) = go().await;
    let d = Device::new();
    let (a, b) = d.with(|s| {
        let a = s.create_project("Payments API").unwrap().project.id;
        let b = s.create_project("Client X (NDA)").unwrap().project.id;
        (a, b)
    });
    d.sign_in(&server);
    d.run(&server, &g).await;
    let offered: Vec<String> = d.with(|s| {
        s.first_sync_candidates()
            .into_iter()
            .map(|c| c.project_id)
            .collect()
    });
    assert!(offered.contains(&a) && offered.contains(&b));
    // Nothing uploads before the answer, not even the Global.
    let global = d.with(|s| s.global_environment().unwrap().id.clone());
    d.run(&server, &g).await;
    assert!(
        pushed_ids(&g, PERSONAL)
            .iter()
            .all(|id| *id != a && *id != b && *id != global),
        "{:?}",
        pushed_ids(&g, PERSONAL)
    );
    assert_eq!(d.with(|s| s.global_scope()), None);

    d.with(|s| s.first_sync_apply(std::slice::from_ref(&a)).unwrap());
    d.run(&server, &g).await;
    let pushed = pushed_ids(&g, PERSONAL);
    assert!(pushed.contains(&a));
    assert!(
        pushed.contains(&global),
        "the Global goes with a checked project"
    );
    assert!(
        !pushed.contains(&b),
        "an unchecked project never leaves the device"
    );
    assert!(d.with(|s| s.first_sync_candidates()).is_empty());
    assert_eq!(d.with(|s| s.location_of(&b).kind), "local");
}

#[tokio::test]
async fn keep_all_on_this_device_is_never_overridden_by_a_later_run() {
    let (server, g) = go().await;
    let d = Device::new();
    // An untouched Default plus real work: the checklist is asked.
    let work = d.with(|s| s.create_project("Work").unwrap().project.id);
    d.with(|s| {
        let c = s.create_collection(&work, "API").unwrap();
        s.create_request(&c.id, None, http_req("GET", "https://x"))
            .unwrap();
    });
    d.sign_in(&server);
    d.run(&server, &g).await;
    assert!(!d.with(|s| s.first_sync_candidates()).is_empty());
    d.with(|s| s.first_sync_apply(&[]).unwrap());
    d.run(&server, &g).await;
    d.run(&server, &g).await;
    d.with(|s| {
        assert!(s.projects.keys().all(|p| s.location_of(p).kind == "local"));
    });
    assert!(
        pushed_ids(&g, PERSONAL)
            .iter()
            .all(|id| !d.with(|s| s.projects.contains_key(id)))
    );
    // "Keep all on this device" (and ×) keeps the Global here too.
    let global = d.with(|s| s.global_environment().unwrap().id.clone());
    assert!(!pushed_ids(&g, PERSONAL).contains(&global));
    assert_eq!(d.with(|s| s.global_scope()), None);
    assert_eq!(d.reopen().global_scope(), None, "persisted");
}

#[tokio::test]
async fn a_fresh_device_syncs_its_untouched_default_and_then_adopts_the_account() {
    let (server, g) = go().await;
    // Device A has real work in Personal.
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    // Device B is fresh: no dialog, its untouched Default goes away.
    let b = Device::new();
    let report = b.with(|s| s.sign_in_migrate(ident(&server), &b.secrets).unwrap());
    assert!(report.fresh_device && !report.first_sync);
    assert!(b.with(|s| s.first_sync_candidates()).is_empty());
    b.run(&server, &g).await;
    let names: Vec<String> = b.with(|s| s.project_list().into_iter().map(|p| p.name).collect());
    assert!(names.contains(&"Work".to_string()), "{names:?}");
    assert_eq!(b.with(|s| s.location_of(&pid).kind), "personal");
    // The fresh-device toast (UX §10) counts what syncs now.
    let n = b.with(|s| s.projects_in(PERSONAL).len());
    assert!(n >= 1);
    assert_eq!(
        b.with(|s| s.fresh_device_notice()),
        format!(
            "Signed in · syncing {n} project{} from your account.",
            if n == 1 { "" } else { "s" }
        )
    );
}

// ───────────────────────────── bindings B1–B7 ─────────────────────────────

#[tokio::test]
async fn a_local_only_project_produces_no_traffic() {
    let (server, g) = go().await;
    let (d, work) = personal_device(&server, &g).await;
    let local = d.with(|s| {
        let p = s.create_project_at("Scratch", None).unwrap().project.id;
        let c = s.create_collection(&p, "Secret stuff").unwrap();
        s.create_request(&c.id, None, http_req("GET", "https://intranet/x"))
            .unwrap();
        p
    });
    d.run(&server, &g).await;
    let pushed = pushed_ids(&g, PERSONAL);
    assert!(pushed.contains(&work));
    assert!(!pushed.contains(&local));
    let all: Vec<Value> =
        g.0.lock()
            .unwrap()
            .pushes
            .iter()
            .flat_map(|(_, c)| c.clone())
            .collect();
    assert!(
        !serde_json::to_string(&all).unwrap().contains("intranet"),
        "nothing of the local project is ever sent"
    );
    // Deleting it records no tombstone anywhere.
    d.with(|s| s.delete_project(&local).unwrap());
    assert!(
        d.with(|s| sync::collect_push_in(s, &Scope::Space(PERSONAL.into())))
            .is_empty()
    );
}

#[tokio::test]
async fn the_global_environment_syncs_only_with_its_space() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    g.0.lock().unwrap().spaces.entry(TEAM.into()).or_default();
    let global = d.with(|s| s.global_environment().unwrap().id.clone());
    // A team pushes a Global of its own.
    seed(
        &g,
        TEAM,
        "environment",
        "TEAMGLOBAL",
        5,
        json!({"id": "TEAMGLOBAL", "workspaceId": "x", "projectId": null,
        "name": "Global", "variables": [{"key": "stolen", "value": "1", "enabled": true, "secret": false}], "sortOrder": 0, "createdAt": 1, "updatedAt": 5}),
    );
    d.run(&server, &g).await;
    assert!(pushed_ids(&g, PERSONAL).contains(&global));
    assert!(
        !pushed_ids(&g, TEAM).contains(&global),
        "the Global never goes to a team"
    );
    d.with(|s| {
        assert!(s.environment("TEAMGLOBAL").is_none());
        assert_eq!(s.global_environment().unwrap().id, global);
    });
}

#[tokio::test]
async fn b2_a_known_project_id_from_another_space_is_ignored() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    // A malicious teammate re-pushes the personal project's id into the team.
    seed(
        &g,
        TEAM,
        "project",
        &pid,
        now_ms() + 10_000,
        json!({"id": pid, "workspaceId": "x", "name": "Hijacked",
        "sortOrder": 0, "createdAt": 1, "updatedAt": 1}),
    );
    let run = d.run(&server, &g).await;
    d.with(|s| {
        assert_eq!(s.project(&pid).unwrap().project.name, "Work");
        assert_eq!(
            s.bindings.project_spaces[&pid], PERSONAL,
            "binding unchanged"
        );
        assert_eq!(s.space_sync(&Scope::Space(TEAM.into())).foreign, 1);
    });
    let team = run.scopes.iter().find(|r| r.space_id == TEAM).unwrap();
    assert_eq!(team.report.pulled, 0);
}

#[tokio::test]
async fn b3_children_with_a_foreign_parent_or_id_are_not_relocated() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let (cid, rid) = d.with(|s| {
        let t = s.project(&pid).unwrap();
        (t.collections[0].id.clone(), t.requests[0].id.clone())
    });
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    let later = now_ms() + 10_000;
    seed(
        &g,
        TEAM,
        "project",
        "TEAMPROJ",
        1,
        json!({"id": "TEAMPROJ", "workspaceId": "x", "name": "Team project",
        "sortOrder": 0, "createdAt": 1, "updatedAt": 1}),
    );
    seed(
        &g,
        TEAM,
        "collection",
        "TEAMCOL",
        1,
        json!({"id": "TEAMCOL", "projectId": "TEAMPROJ", "name": "Team col",
        "description": "", "auth": {"type": "none"}, "sortOrder": 0, "createdAt": 1, "updatedAt": 1}),
    );
    // The personal collection's id, claimed under the team project.
    seed(
        &g,
        TEAM,
        "collection",
        &cid,
        later,
        json!({"id": cid, "projectId": "TEAMPROJ", "name": "Moved",
        "description": "", "auth": {"type": "none"}, "sortOrder": 0, "createdAt": 1, "updatedAt": later}),
    );
    // The personal request's id, re-parented under the team collection.
    let mut r = http_req("DELETE", "https://evil");
    r.id = rid.clone();
    r.collection_id = "TEAMCOL".into();
    r.updated_at = later;
    seed(
        &g,
        TEAM,
        "request",
        &rid,
        later,
        serde_json::to_value(&r).unwrap(),
    );
    d.run(&server, &g).await;
    d.with(|s| {
        let t = s.project(&pid).unwrap();
        assert!(t.collections.iter().any(|c| c.id == cid && c.name == "API"));
        assert!(t.requests.iter().any(|x| x.id == rid && x.method == "GET"));
        // The team project arrived (new to this device) with its own child only.
        let team = s.project("TEAMPROJ").unwrap();
        assert_eq!(team.collections.len(), 1);
        assert_eq!(team.collections[0].id, "TEAMCOL");
        assert!(team.requests.is_empty());
        assert_eq!(s.bindings.project_spaces["TEAMPROJ"], TEAM);
    });
}

#[tokio::test]
async fn b5_a_team_tombstone_never_deletes_a_personal_entity() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let rid = d.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    tombstone(&g, TEAM, "request", &rid, now_ms() + 10_000);
    tombstone(&g, TEAM, "project", &pid, now_ms() + 10_000);
    d.run(&server, &g).await;
    d.with(|s| {
        assert!(s.project(&pid).is_ok());
        assert!(s.request(&rid).is_some());
    });
}

#[tokio::test]
async fn b7_a_partial_run_defers_deletes_and_does_not_flip() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    let other = d.with(|s| s.create_project("Other").unwrap().project.id);
    d.run(&server, &g).await;
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    g.0.lock().unwrap().spaces.entry(TEAM.into()).or_default();
    // Another device deleted "Other" in Personal; the team is down.
    tombstone(&g, PERSONAL, "project", &other, now_ms() + 1_000);
    g.0.lock()
        .unwrap()
        .fail
        .insert(TEAM.into(), (503, "starting".into()));
    let cursor_before = d.with(|s| s.space_sync(&Scope::Space(PERSONAL.into())).cursor);
    let run = d.run(&server, &g).await;
    assert!(run.error().is_some_and(SyncError::is_retryable));
    d.with(|s| {
        assert!(s.project(&other).is_ok(), "the delete waits");
        assert_eq!(
            s.space_sync(&Scope::Space(PERSONAL.into())).cursor,
            cursor_before,
            "the cursor stays so the delete is pulled again"
        );
        assert_eq!(
            s.space_sync(&Scope::Space(PERSONAL.into())).deferred_runs,
            1
        );
        assert!(s.project(&pid).is_ok());
    });
    // The team is back: the delete applies.
    g.0.lock().unwrap().fail.clear();
    let run = d.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    d.with(|s| {
        assert!(s.project(&other).is_err());
        assert_eq!(
            s.space_sync(&Scope::Space(PERSONAL.into())).deferred_runs,
            0
        );
    });
}

#[tokio::test]
async fn b7_deletes_apply_after_three_deferred_runs() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    let other = d.with(|s| s.create_project("Other").unwrap().project.id);
    d.run(&server, &g).await;
    bind_team(&d, &server, Role::Editor);
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    tombstone(&g, PERSONAL, "project", &other, now_ms() + 1_000);
    g.0.lock()
        .unwrap()
        .fail
        .insert(TEAM.into(), (503, "starting".into()));
    for _ in 0..3 {
        d.run(&server, &g).await;
        assert!(d.with(|s| s.project(&other).is_ok()));
    }
    d.run(&server, &g).await;
    assert!(d.with(|s| s.project(&other).is_err()));
}

// ───────────────────────── foreign servers and removal ─────────────────────────

#[tokio::test]
async fn a_binding_of_another_origin_or_instance_never_pushes() {
    let (server_a, ga) = go().await;
    let (server_b, gb) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    // Sign in to server B: A's personal binding pauses.
    d.with(|s| {
        s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap();
        assert!(matches!(
            s.binding(PERSONAL).unwrap().state,
            BindingState::Paused { .. }
        ));
        assert!(!s.scope_syncable(&Scope::Space(PERSONAL.into())));
        s.rename_project(&pid, "Edited while on B").unwrap();
    });
    // B lists a personal space with the same id (another server's db).
    d.run(&server_b, &gb).await;
    assert!(
        !pushed_ids(&gb, PERSONAL).contains(&pid),
        "A's project never reaches B"
    );
    assert!(
        d.with(|s| sync::collect_push_in(s, &Scope::Space(PERSONAL.into())))
            .is_empty()
    );

    // Same origin, new instance id (the admin reset the database).
    let (server_c, gc) = go().await;
    let (d2, pid2) = personal_device(&server_c, &gc).await;
    d2.run(&server_c, &gc).await;
    let mut who = ident(&server_c);
    who.server_instance_id = "01JDNEWINSTANCE00000000000".into();
    d2.with(|s| {
        s.set_account(Some(who));
        s.rename_project(&pid2, "x").unwrap();
    });
    let before = gc.0.lock().unwrap().pushes.len();
    d2.run(&server_c, &gc).await;
    assert_eq!(gc.0.lock().unwrap().pushes.len(), before, "nothing pushed");
    assert_eq!(d2.with(|s| s.location_of(&pid2).state.unwrap()), "paused");
}

#[tokio::test]
async fn keep_or_remove_resolves_a_paused_space() {
    let (server_a, ga) = go().await;
    let (server_b, _) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    d.with(|s| s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap());
    let kept = d.with(|s| s.space_notice_resolve(PERSONAL, true).unwrap());
    assert!(kept.contains(&pid));
    d.with(|s| {
        assert!(s.binding(PERSONAL).is_none());
        assert_eq!(s.location_of(&pid).kind, "local");
        assert_eq!(s.project(&pid).unwrap().project.name, "Work");
    });

    let (d2, pid2) = personal_device(&server_a, &ga).await;
    d2.with(|s| s.create_project_at("Mine", None).unwrap());
    d2.with(|s| s.sign_in_migrate(ident(&server_b), &d2.secrets).unwrap());
    d2.with(|s| s.space_notice_resolve(PERSONAL, false).unwrap());
    d2.with(|s| assert!(s.project(&pid2).is_err(), "removed from this device"));
    assert!(d2.project_named("Mine").is_some());
}

#[tokio::test]
async fn removal_needs_an_authoritative_list_and_a_stray_404_changes_nothing() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    g.0.lock().unwrap().spaces.entry(TEAM.into()).or_default();
    seed(
        &g,
        TEAM,
        "project",
        "TEAMPROJ",
        1,
        json!({"id": "TEAMPROJ", "workspaceId": "x", "name": "Payments API",
        "sortOrder": 0, "createdAt": 1, "updatedAt": 1}),
    );
    d.run(&server, &g).await;
    assert!(d.with(|s| s.project("TEAMPROJ").is_ok()));

    // A stray 404 on the team's sync route: Unavailable, data untouched.
    g.0.lock()
        .unwrap()
        .fail
        .insert(TEAM.into(), (404, "space_not_found".into()));
    let run = d.run(&server, &g).await;
    assert!(matches!(
        run.scopes
            .iter()
            .find(|r| r.space_id == TEAM)
            .unwrap()
            .error,
        Some(SyncError::SpaceGone(_))
    ));
    d.with(|s| {
        assert!(matches!(
            s.binding(TEAM).unwrap().state,
            BindingState::Unavailable { .. }
        ));
        assert!(s.project("TEAMPROJ").is_ok());
    });
    // /v1/me/spaces failing (503) is not authoritative either.
    g.0.lock().unwrap().listed = None;
    d.run(&server, &g).await;
    assert!(matches!(
        d.with(|s| s.binding(TEAM).unwrap().state.clone()),
        BindingState::Unavailable { .. }
    ));
    // The space is listed again: active again.
    g.0.lock().unwrap().fail.clear();
    g.0.lock().unwrap().listed = Some(vec![personal_row(), team_row("editor")]);
    d.run(&server, &g).await;
    assert_eq!(
        d.with(|s| s.binding(TEAM).unwrap().state.clone()),
        BindingState::Active
    );
    // A 200 list without it: Removed, and still no local change.
    g.0.lock().unwrap().listed = Some(vec![personal_row()]);
    d.run(&server, &g).await;
    d.with(|s| {
        assert!(matches!(
            s.binding(TEAM).unwrap().state,
            BindingState::Removed { .. }
        ));
        assert!(s.project("TEAMPROJ").is_ok());
    });
}

// ───────────────────────────── merge ─────────────────────────────

fn legacy_device(server: &MockServer) -> Device {
    let d = Device::new();
    d.with(|s| {
        s.workspace.sync = Some(SyncConfig {
            worker_url: server.uri(),
            remote_workspace_id: "LEGACYWS".into(),
            token: "rh_LEGACY".into(),
            cursor: 0,
        });
        s.save_workspace().unwrap();
    });
    d
}

#[tokio::test]
async fn merge_converges_across_two_devices_with_no_duplicates() {
    let (server, g) = go().await;
    // Device A builds a token-synced workspace; device B joins it.
    let a = legacy_device(&server);
    a.with(|s| {
        let p = s.create_project("Shared").unwrap().project.id;
        let c = s.create_collection(&p, "API").unwrap();
        s.create_request(&c.id, None, http_req("GET", "https://x"))
            .unwrap();
    });
    // Pure-legacy runs: no account, only the token scope.
    let run = space_sync::run(&a.store, None, RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    let b = legacy_device(&server);
    let run = space_sync::run(&b.store, None, RUN_BUDGET).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    let count = |d: &Device| d.with(|s| s.projects.len());
    assert_eq!(count(&a), count(&b));

    // Both sign in and merge, A first.
    for d in [&a, &b] {
        d.sign_in(&server);
        let token = d.secrets.get(&legacy_key("LEGACYWS")).unwrap();
        d.with(|s| s.legacy_token = token);
        d.run(&server, &g).await;
    }
    a.with(|s| s.legacy_merge().unwrap());
    let run = a.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    b.with(|s| s.legacy_merge().unwrap());
    let run = b.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    assert!(
        run.report.conflicts.is_empty(),
        "{:?}",
        run.report.conflicts
    );
    a.run(&server, &g).await;

    let personal_projects = g.0.lock().unwrap().spaces[PERSONAL].live("project");
    assert_eq!(
        personal_projects.len(),
        count(&a),
        "same ids: no duplicates"
    );
    for d in [&a, &b] {
        d.with(|s| {
            assert_eq!(
                s.bindings.legacy.as_ref().unwrap().mode,
                LegacyMode::SharesOnly
            );
            assert!(
                s.project_list()
                    .iter()
                    .all(|p| !p.name.contains("(conflict)"))
            );
            for p in s.project_list() {
                assert_eq!(s.bindings.project_spaces[&p.id], PERSONAL);
            }
            assert!(!s.scope_syncable(&Scope::Legacy), "legacy stops syncing");
        });
    }
    assert_eq!(count(&a), count(&b));
    // Merging again is a no-op.
    assert_eq!(a.with(|s| s.legacy_merge().unwrap()), 0);
}

#[tokio::test]
async fn legacy_disabled_gets_the_sign_in_copy() {
    let (server, g) = go().await;
    g.0.lock().unwrap().legacy_disabled = true;
    let client = SyncClient::new(&server.uri(), Some("rh_LEGACY")).unwrap();
    let e = client.pull_page(0).await.unwrap_err();
    assert_eq!(
        e,
        SyncError::Rejected("This server no longer accepts token sync. Sign in instead.".into())
    );
    // In account mode it becomes the legacy space's issue.
    let d = legacy_device(&server);
    d.sign_in(&server);
    d.with(|s| s.legacy_token = Some("rh_LEGACY".into()));
    d.run(&server, &g).await;
    let issue = d
        .with(|s| s.space_sync(&Scope::Legacy).issue.clone())
        .unwrap();
    assert_eq!(issue.code, "legacyDisabled");
}

// ───────────────────────── restore, types, clock, session ─────────────────────────

#[tokio::test]
async fn restore_detection_on_a_space_resets_the_cursor_and_resends_everything() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let scope = Scope::Space(PERSONAL.into());
    assert_eq!(
        d.with(|s| s.space_state(&scope).sync_epoch.clone())
            .as_deref(),
        Some("EPOCH1")
    );
    // The admin restored a backup from before our push: new epoch, data gone.
    {
        let mut st = g.0.lock().unwrap();
        st.epoch = "EPOCH2".into();
        st.spaces.insert(PERSONAL.into(), Space::default());
        st.pushes.clear();
    }
    let run = d.run(&server, &g).await;
    assert!(run.report.restored);
    assert!(
        pushed_ids(&g, PERSONAL).contains(&pid),
        "everything re-sent"
    );
    assert_eq!(
        d.with(|s| s.space_state(&scope).sync_epoch.clone())
            .as_deref(),
        Some("EPOCH2")
    );
    assert!(
        g.0.lock().unwrap().spaces[PERSONAL]
            .live("project")
            .contains(&pid)
    );
    assert_eq!(d.with(|s| sync::pending_count_in(s, &scope)), 0);
}

#[tokio::test]
async fn an_unlisted_type_stays_local_only_with_the_notice() {
    let (server, g) = go().await;
    g.0.lock().unwrap().types = ["project", "collection", "folder", "request"]
        .map(String::from)
        .to_vec();
    let (d, pid) = personal_device(&server, &g).await;
    let env = d.with(|s| s.create_environment(Some(&pid), "Staging").unwrap().id);
    d.run(&server, &g).await;
    let scope = Scope::Space(PERSONAL.into());
    assert!(!pushed_ids(&g, PERSONAL).contains(&env));
    let held = d.with(|s| sync::unsupported_entities_in(s, &scope));
    assert!(held.iter().any(|e| e.id == env));
    assert_eq!(
        sync::unsupported_notice(held.len()).unwrap(),
        format!(
            "{} items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server.",
            held.len()
        )
    );
    let status = d.with(|s| space_sync::space_statuses(s));
    assert_eq!(status[0].unsupported, held.len());
    // The server is updated: the next run sends them and the notice clears.
    g.0.lock().unwrap().types.push("environment".into());
    d.run(&server, &g).await;
    assert!(pushed_ids(&g, PERSONAL).contains(&env));
    assert!(
        d.with(|s| sync::unsupported_entities_in(s, &scope))
            .is_empty()
    );
}

#[tokio::test]
async fn a_change_rejected_per_change_as_unsupported_type_stays_dirty() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    // The server dropped `environment` after discovery.
    g.0.lock().unwrap().types.retain(|t| t != "environment");
    let env = d.with(|s| s.create_environment(Some(&pid), "Dev").unwrap().id);
    let run = d.run(&server, &g).await;
    assert!(run.error().is_none());
    let scope = Scope::Space(PERSONAL.into());
    assert!(
        d.with(|s| sync::unsupported_entities_in(s, &scope))
            .iter()
            .any(|e| e.id == env)
    );
}

#[tokio::test]
async fn clock_skew_restamps_once_the_clock_is_fixed() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let scope = Scope::Space(PERSONAL.into());
    // This device's clock ran 7 minutes ahead when the edit was made.
    let ahead = now_ms() + 7 * 60_000;
    d.with(|s| {
        let t = s.projects.get_mut(&pid).unwrap();
        t.requests[0].name = "Edited ahead".into();
        t.requests[0].updated_at = ahead;
        s.save_project(&pid).unwrap();
    });
    let rid = d.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    // The server sees the clock as wrong: its time is behind ours.
    g.0.lock().unwrap().clock_offset_ms = -7 * 60_000;
    d.run(&server, &g).await;
    d.with(|s| {
        let ss = s.space_sync(&scope);
        assert!(ss.skewed.contains_key(&sync_key(EntityType::Request, &rid)));
        assert_eq!(ss.issue.as_ref().unwrap().code, "clockSkew");
        assert!(
            ss.issue
                .as_ref()
                .unwrap()
                .message
                .contains("about 14 minutes")
                || ss
                    .issue
                    .as_ref()
                    .unwrap()
                    .message
                    .contains("ahead by about")
        );
        assert!(sync::pending_count_in(s, &scope) > 0, "it stays dirty");
    });
    // The clock is fixed: the next run restamps and the push is applied.
    g.0.lock().unwrap().clock_offset_ms = 0;
    let run = d.run(&server, &g).await;
    assert!(run.error().is_none());
    d.with(|s| {
        let ss = s.space_sync(&scope);
        assert!(ss.skewed.is_empty());
        assert_eq!(ss.issue.as_ref().unwrap().code, "clockFixed");
        assert_eq!(
            ss.issue.as_ref().unwrap().message,
            "Your clock was corrected; 1 change will sync now."
        );
        let r = s.request(&rid).unwrap();
        assert!(r.updated_at < ahead, "restamped to a sane time");
        assert_eq!(r.name, "Edited ahead");
        assert_eq!(sync::pending_count_in(s, &scope), 0);
    });
    let server_name = g.0.lock().unwrap().spaces[PERSONAL].entities
        [&("request".to_string(), rid.clone())]
        .data
        .clone()
        .unwrap()["name"]
        .clone();
    assert_eq!(server_name, "Edited ahead");
}

#[tokio::test]
async fn a_skewed_edit_whose_remote_changed_becomes_a_conflict_copy() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let scope = Scope::Space(PERSONAL.into());
    let rid = d.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    let ahead = now_ms() + 20 * 60_000;
    d.with(|s| {
        let t = s.projects.get_mut(&pid).unwrap();
        t.requests[0].name = "Mine (future clock)".into();
        t.requests[0].updated_at = ahead;
        s.save_project(&pid).unwrap();
    });
    g.0.lock().unwrap().clock_offset_ms = -20 * 60_000;
    d.run(&server, &g).await;
    assert!(d.with(|s| !s.space_sync(&scope).skewed.is_empty()));
    // Meanwhile another device changed the same request on the server.
    let mut theirs = d.with(|s| s.request(&rid).unwrap().clone());
    theirs.name = "Theirs".into();
    theirs.updated_at = now_ms();
    seed(
        &g,
        PERSONAL,
        "request",
        &rid,
        theirs.updated_at,
        serde_json::to_value(&theirs).unwrap(),
    );
    g.0.lock().unwrap().clock_offset_ms = 0;
    let run = d.run(&server, &g).await;
    assert_eq!(run.report.conflicts.len(), 1, "{:?}", run.report);
    d.with(|s| {
        assert_eq!(
            s.request(&rid).unwrap().name,
            "Theirs",
            "the future stamp doesn't win"
        );
        let t = s.project(&pid).unwrap();
        assert!(
            t.requests
                .iter()
                .any(|r| r.name == "Mine (future clock) (conflict)")
        );
        assert!(s.space_sync(&scope).skewed.is_empty());
    });
}

#[tokio::test]
async fn a_401_on_space_sync_refreshes_once_and_a_dead_session_needs_sign_in() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    let sess = session(&server, &g);
    // The server rotated the access token (expired on its side).
    g.0.lock().unwrap().access = "rha_ROTATED".into();
    let run = space_sync::run(&d.store, Some(sess.clone()), RUN_BUDGET).await;
    assert!(!run.sign_in_required);
    assert_eq!(g.0.lock().unwrap().refreshes, 1);
    assert!(run.error().is_none(), "{:?}", run.error());

    // The refresh token is dead too: sign in again, data untouched.
    {
        let mut st = g.0.lock().unwrap();
        st.access = "rha_GONE".into();
        st.refresh = "rhr_GONE".into();
    }
    let before = d.with(|s| s.projects.len());
    let run = space_sync::run(&d.store, Some(sess.clone()), RUN_BUDGET).await;
    assert!(run.sign_in_required);
    assert!(sess.sign_in_required());
    assert_eq!(d.with(|s| s.projects.len()), before);
    assert!(
        d.with(|s| s.binding(PERSONAL).unwrap().is_active()),
        "bindings untouched"
    );
}

#[tokio::test]
async fn pulled_changes_carry_attribution_in_the_space_not_the_entity() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    seed(
        &g,
        PERSONAL,
        "project",
        "FROMMAC",
        5,
        json!({"id": "FROMMAC", "workspaceId": "x", "name": "From my Mac",
        "sortOrder": 9, "createdAt": 1, "updatedAt": 5}),
    );
    d.run(&server, &g).await;
    d.with(|s| {
        let a = &s.space_sync(&Scope::Space(PERSONAL.into())).attribution;
        assert_eq!(
            a[&sync_key(EntityType::Project, "FROMMAC")].name,
            "Priya Shah"
        );
        assert!(s.project("FROMMAC").unwrap().project.extra.is_empty());
    });
    let raw = std::fs::read_to_string(d.with(|s| s.dir().join("projects/FROMMAC.json"))).unwrap();
    assert!(!raw.contains("updatedBy"));
}

// ───────────────────────────── local actions ─────────────────────────────

#[tokio::test]
async fn sync_location_moves_between_personal_and_this_device() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let scope = Scope::Space(PERSONAL.into());
    // Personal → this device: tombstones for what the space holds.
    d.with(|s| s.set_project_location(&pid, None).unwrap());
    let tomb = d.with(|s| s.space_state(&scope).tombstones.len());
    assert_eq!(tomb, 3, "project, collection and request");
    d.run(&server, &g).await;
    assert!(
        !g.0.lock().unwrap().spaces[PERSONAL]
            .live("project")
            .contains(&pid)
    );
    assert!(d.with(|s| s.project(&pid).is_ok()), "this copy stays");
    // … and back: stamped newer than the tombstones, so it returns.
    d.with(|s| s.set_project_location(&pid, Some(PERSONAL)).unwrap());
    d.run(&server, &g).await;
    assert!(
        g.0.lock().unwrap().spaces[PERSONAL]
            .live("project")
            .contains(&pid)
    );
    assert_eq!(d.with(|s| sync::pending_count_in(s, &scope)), 0);
}

#[tokio::test]
async fn the_viewer_guard_blocks_mutators() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    bind_team(&d, &server, Role::Viewer);
    let tp = d.with(|s| {
        let p = s.create_project("Team thing").unwrap().project.id;
        s.bindings.project_spaces.insert(p.clone(), TEAM.into());
        p
    });
    d.with(|s| {
        let e = s.rename_project(&tp, "x").unwrap_err();
        assert!(matches!(
            e,
            reqharbor_core::storage::StoreError::ReadOnlyRole(_)
        ));
        assert!(s.create_collection(&tp, "c").is_err());
        assert!(s.create_environment(Some(&tp), "e").is_err());
    });
}

#[tokio::test]
async fn new_projects_go_to_personal_when_signed_in_and_stay_local_when_signed_out() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    assert_eq!(d.with(|s| s.location_of(&pid).kind), "personal");
    d.with(|s| s.set_account(None));
    let local = d.with(|s| s.create_project("Offline idea").unwrap().project.id);
    assert_eq!(d.with(|s| s.location_of(&local).kind), "local");
    assert_eq!(d.with(|s| s.location_of(&pid).state.unwrap()), "signedOut");
}

#[test]
fn my_spaces_parse_the_final_m1_shape() {
    let list: MySpaces =
        serde_json::from_value(json!({"spaces": [personal_row()], "departed": []})).unwrap();
    assert_eq!(
        list.spaces[0],
        MySpace {
            space_id: PERSONAL.into(),
            kind: "personal".into(),
            team_id: None,
            team_name: None,
            role: Some("owner".into()),
            bytes_used: Some(1024),
            quota_bytes: Some(104857600),
            member_count: None,
        }
    );
    // M2: team rows carry memberCount; departed rows carry team, reason and time.
    let m2: MySpaces = serde_json::from_value(json!({
        "spaces": [personal_row(), {"spaceId": TEAM, "kind": "team", "teamId": "T1", "teamName": "Acme",
                    "role": "viewer", "bytesUsed": 5, "quotaBytes": null, "memberCount": 8}],
        "departed": [{"spaceId": "S9", "teamId": "T9", "teamName": "Globex", "reason": "deleted", "at": 1700000000000i64}]
    }))
    .unwrap();
    assert_eq!(m2.spaces[1].member_count, Some(8));
    assert_eq!(m2.departed[0].team_id.as_deref(), Some("T9"));
    assert_eq!(m2.departed[0].at, Some(1_700_000_000_000));
}

#[test]
fn updated_by_parses_leniently_and_is_never_pushed() {
    let c: sync::Change = serde_json::from_value(
        json!({"type": "project", "id": "p", "updatedAt": 1, "deleted": false,
        "data": {}, "updatedBy": "u1"}),
    )
    .unwrap();
    assert_eq!(c.updated_by.as_ref().unwrap().user_id, "u1");
    let c: sync::Change = serde_json::from_value(
        json!({"type": "project", "id": "p", "updatedAt": 1, "deleted": false,
        "data": {}, "updatedBy": {"id": "u2", "name": "Sam"}}),
    )
    .unwrap();
    assert_eq!(c.updated_by.as_ref().unwrap().name, "Sam");
    assert!(!serde_json::to_string(&c).unwrap().contains("updatedBy"));
    let c: sync::Change = serde_json::from_value(
        json!({"type": "project", "id": "p", "updatedAt": 1, "deleted": false,
        "data": {}, "updatedBy": 5}),
    )
    .unwrap();
    assert!(c.updated_by.is_none());
}

// ───────────────────────────── QA round (C2) ─────────────────────────────

#[tokio::test]
async fn a_paused_foreign_binding_gets_no_client_and_sends_nothing_to_its_server() {
    let (server_a, ga) = go().await;
    let (server_b, gb) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    d.with(|s| s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap());
    let before = server_a.received_requests().await.unwrap().len();
    let sess_b = session(&server_b, &gb);
    // Sharing from the paused project: no client, so no request with B's
    // token to A's origin (and no refresh against B on A's 401).
    let client = d.with(|s| {
        assert_eq!(
            s.scope_of_project(&pid),
            Some(Scope::Space(PERSONAL.into()))
        );
        space_sync::space_client(s, PERSONAL, &sess_b)
    });
    assert!(matches!(client, Err(SyncError::NotConnected)));
    if let Ok(c) = client {
        let _ = c
            .create_share(sync::ShareKind::Collection, "x", &json!({}))
            .await;
    }
    d.run(&server_b, &gb).await;
    let after = server_a.received_requests().await.unwrap();
    assert_eq!(after.len(), before, "nothing reached A's origin");

    // An active binding of the account, but a session of another origin.
    let (d2, _) = personal_device(&server_a, &ga).await;
    let wrong = d2.with(|s| space_sync::space_client(s, PERSONAL, &sess_b));
    assert!(matches!(wrong, Err(SyncError::NotConnected)));
    let right = d2.with(|s| space_sync::space_client(s, PERSONAL, &session(&server_a, &ga)));
    assert!(right.is_ok());
}

#[tokio::test]
async fn a_foreign_keep_leaves_the_global_on_this_device() {
    let (server_a, ga) = go().await;
    let (server_b, gb) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    let global = d.with(|s| s.global_environment().unwrap().id.clone());
    assert!(pushed_ids(&ga, PERSONAL).contains(&global));
    d.with(|s| s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap());
    d.run(&server_b, &gb).await;
    d.with(|s| s.space_notice_resolve(PERSONAL, true).unwrap());
    // B's personal space binds now; A's Global never follows it.
    d.run(&server_b, &gb).await;
    d.run(&server_b, &gb).await;
    d.with(|s| {
        assert!(s.personal_space().is_some(), "B's personal space is bound");
        assert_eq!(s.global_scope(), None);
        assert_eq!(s.location_of(&pid).kind, "local");
    });
    assert!(!pushed_ids(&gb, PERSONAL).contains(&global));
    assert!(!pushed_ids(&gb, PERSONAL).contains(&pid));
}

#[tokio::test]
async fn signing_back_in_as_the_original_account_resumes_its_paused_bindings() {
    let (server_a, ga) = go().await;
    let (server_b, _) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    // A → B: paused and saved.
    d.with(|s| s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap());
    assert!(d.reopen().binding(PERSONAL).unwrap().paused_foreign());
    // Cancel sign-in (B signs out), then A signs in again.
    d.with(|s| s.set_account(None));
    d.with(|s| s.sign_in_migrate(ident(&server_a), &d.secrets).unwrap());
    d.with(|s| {
        assert_eq!(s.binding(PERSONAL).unwrap().state, BindingState::Active);
        assert_eq!(s.location_of(&pid).state.as_deref(), Some("active"));
        s.rename_project(&pid, "Back on A").unwrap();
    });
    assert_eq!(
        d.reopen().binding(PERSONAL).unwrap().state,
        BindingState::Active,
        "persisted"
    );
    let before = pushed_ids(&ga, PERSONAL).len();
    d.run(&server_a, &ga).await;
    let pushed = pushed_ids(&ga, PERSONAL);
    assert!(
        pushed.len() > before && pushed[before..].contains(&pid),
        "pushes resume"
    );
    // Also after a restart with A attached from account.json.
    let mut s = d.reopen();
    s.set_account(Some(ident(&server_b)));
    s.save_bindings().unwrap();
    let mut s = d.reopen();
    s.set_account(Some(ident(&server_a)));
    assert!(s.scope_syncable(&Scope::Space(PERSONAL.into())));
}

#[tokio::test]
async fn a_legacy_project_never_moves_to_personal_by_sync_location() {
    let (server, g) = go().await;
    let d = legacy_device(&server);
    let pid = d.with(|s| s.project_list()[0].id.clone());
    d.with(|s| {
        let c = s.create_collection(&pid, "API").unwrap();
        s.create_request(&c.id, None, http_req("GET", "https://x/a"))
            .unwrap();
    });
    d.sign_in(&server);
    d.with(|s| s.legacy_token = Some("rh_LEGACY".into()));
    d.run(&server, &g).await;
    assert!(pushed_ids(&g, LEGACY_SPACE).contains(&pid));
    let err = d.with(|s| s.set_project_location(&pid, Some(PERSONAL)));
    assert!(err.is_err(), "B6: only the merge moves legacy → personal");
    d.with(|s| {
        assert!(
            s.space_state(&Scope::Legacy).tombstones.is_empty(),
            "no legacy tombstone"
        );
        assert_eq!(s.location_of(&pid).kind, "legacy");
    });
    let pushes = g.0.lock().unwrap().pushes.len();
    d.run(&server, &g).await;
    let deleted: Vec<Value> = g.0.lock().unwrap().pushes[pushes..]
        .iter()
        .flat_map(|(_, c)| c.clone())
        .filter(|c| c["deleted"] == json!(true))
        .collect();
    assert!(deleted.is_empty(), "{deleted:?}");
    assert!(
        g.0.lock().unwrap().spaces[LEGACY_SPACE]
            .live("project")
            .contains(&pid)
    );
    // Legacy → this device stays possible (with the UI's warning).
    d.with(|s| s.set_project_location(&pid, None).unwrap());
    assert!(!d.with(|s| s.space_state(&Scope::Legacy).tombstones.is_empty()));
}

#[tokio::test]
async fn a_token_reconnected_by_an_older_build_is_kept_until_it_is_a_secret() {
    let (server, g) = go().await;
    let (d, _) = personal_device(&server, &g).await;
    // A downgraded client connected token sync to another workspace.
    let path = d.with(|s| s.dir().join("workspace.json"));
    let mut raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    raw["workspace"]["sync"] = serde_json::to_value(SyncConfig {
        worker_url: server.uri(),
        remote_workspace_id: "OTHERWS".into(),
        token: "rh_OTHER".into(),
        cursor: 5,
    })
    .unwrap();
    std::fs::write(&path, serde_json::to_vec(&raw).unwrap()).unwrap();

    let mut s = d.reopen();
    assert!(
        s.workspace.sync.is_none(),
        "never used for sync in account mode"
    );
    assert!(s.stray_sync.is_some());
    s.save_workspace().unwrap();
    assert!(
        std::fs::read_to_string(&path).unwrap().contains("rh_OTHER"),
        "still on disk until it is a secret"
    );
    // The secret write fails: the file stays as it is.
    let failing = MemorySecretStore::new();
    failing.set_fail_writes(true);
    assert!(s.secure_stray_sync(&failing).is_err());
    assert!(std::fs::read_to_string(&path).unwrap().contains("rh_OTHER"));
    // It succeeds: the token is a secret, then leaves workspace.json.
    let secrets = MemorySecretStore::new();
    assert_eq!(
        s.secure_stray_sync(&secrets).unwrap().as_deref(),
        Some("OTHERWS")
    );
    assert_eq!(
        secrets.get(&legacy_key("OTHERWS")).unwrap().as_deref(),
        Some("rh_OTHER")
    );
    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        !after.contains("rh_OTHER") && !after.contains("\"sync\""),
        "{after}"
    );
}

#[tokio::test]
async fn an_account_with_an_unknown_instance_binds_and_pushes_nothing() {
    let (server, g) = go().await;
    let (d, pid) = personal_device(&server, &g).await;
    d.run(&server, &g).await;
    let mut who = ident(&server);
    who.server_instance_id = String::new();
    d.with(|s| {
        s.set_account(Some(who.clone()));
        // Not foreign (no notice), but not ours either.
        assert_eq!(s.binding(PERSONAL).unwrap().state, BindingState::Active);
        assert!(!s.scope_syncable(&Scope::Space(PERSONAL.into())));
        s.rename_project(&pid, "Edited").unwrap();
    });
    let before = g.0.lock().unwrap().pushes.len();
    d.run(&server, &g).await;
    assert_eq!(g.0.lock().unwrap().pushes.len(), before, "nothing pushed");
    // A rebuilt server (new instance id) under an empty account instance is
    // never adopted: no binding is added for it.
    let fresh = Device::new();
    fresh.with(|s| s.sign_in_migrate(who.clone(), &fresh.secrets).unwrap());
    fresh.run(&server, &g).await;
    assert!(fresh.with(|s| s.bindings.spaces.is_empty()));
    // Discovery fills it in: pushes resume.
    d.with(|s| s.set_account(Some(ident(&server))));
    d.run(&server, &g).await;
    assert!(g.0.lock().unwrap().pushes.len() > before);
}

#[tokio::test]
async fn a_notice_closed_with_x_keeps_the_projects_after_seven_days() {
    let (server_a, ga) = go().await;
    let (server_b, _) = go().await;
    let (d, pid) = personal_device(&server_a, &ga).await;
    d.run(&server_a, &ga).await;
    d.with(|s| s.sign_in_migrate(ident(&server_b), &d.secrets).unwrap());
    let now = now_ms();
    let day = 24 * 3_600_000;
    d.with(|s| {
        s.space_notice_later(&[PERSONAL.to_string()], now - 6 * day)
            .unwrap()
    });
    // A later × doesn't restart the clock.
    d.with(|s| s.space_notice_later(&[PERSONAL.to_string()], now).unwrap());
    assert_eq!(
        d.reopen().binding(PERSONAL).unwrap().decide_later_at,
        Some(now - 6 * day)
    );
    assert!(
        d.with(|s| s.apply_overdue_notices(now)).is_empty(),
        "not yet"
    );
    // Signed out: never resolved behind the user's back.
    d.with(|s| s.set_account(None));
    assert!(
        d.with(|s| s.apply_overdue_notices(now + 2 * day))
            .is_empty()
    );
    d.with(|s| s.set_account(Some(ident(&server_b))));
    assert_eq!(
        d.with(|s| s.apply_overdue_notices(now + 2 * day)),
        vec![PERSONAL.to_string()]
    );
    d.with(|s| {
        assert!(s.binding(PERSONAL).is_none());
        assert_eq!(s.location_of(&pid).kind, "local");
        assert!(s.project(&pid).is_ok(), "Keep: nothing is deleted");
        assert_eq!(s.global_scope(), None);
    });
}

// ───────────────────────────── M2: teams (04 §12 M2b) ─────────────────────────────

use reqharbor_core::transfer;

fn server_ent(g: &Go, space: &str, t: &str, id: &str) -> Option<Ent> {
    g.0.lock()
        .unwrap()
        .spaces
        .get(space)
        .and_then(|s| s.entities.get(&(t.to_string(), id.to_string())).cloned())
}

/// A second device of the same account (Personal bound, first run done).
async fn second_device(server: &MockServer, g: &Go) -> Device {
    let d = Device::new();
    d.sign_in(server);
    d.run(server, g).await;
    d
}

/// Make the team exist on the server and in `/v1/me/spaces` with `role`.
fn team_exists(g: &Go, role: &str) {
    let mut st = g.0.lock().unwrap();
    st.spaces.entry(TEAM.into()).or_default();
    st.listed = Some(vec![personal_row(), team_row(role)]);
}

fn no_progress(_: usize, _: usize) {}

/// A project in the team with a collection and a request, pushed.
async fn team_project(d: &Device, server: &MockServer, g: &Go) -> (String, String, String) {
    let ids = d.with(|s| {
        let p = s
            .create_project_at("Payments", Some(TEAM.into()))
            .unwrap()
            .project
            .id;
        let c = s.create_collection(&p, "Orders").unwrap().id;
        let r = s
            .create_request(&c, None, http_req("POST", "https://api/orders"))
            .unwrap()
            .id;
        (p, c, r)
    });
    let run = d.run(server, g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    ids
}

#[tokio::test]
async fn m2_a_malicious_editor_repush_of_old_ids_leaves_the_personal_copy_untouched() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Admin);
    team_exists(&g, "admin");
    let (old_p, _, old_r) = team_project(&a, &server, &g).await;
    // An admin moves the project out of the team into Personal (new ids).
    let moved = transfer::move_project(
        &a.store,
        Some(session(&server, &g)),
        &old_p,
        Some(PERSONAL),
        &no_progress,
    )
    .await
    .unwrap();
    a.run(&server, &g).await;
    let new_p = moved.project_id.clone();
    assert_ne!(new_p, old_p);
    let new_r = a.with(|s| s.project(&new_p).unwrap().requests[0].id.clone());
    assert_ne!(new_r, old_r);
    // A teammate re-pushes the old ids (and the new project's id) into the team.
    let later = now_ms() + 10_000;
    for (pid, name) in [(&old_p, "Resurrected"), (&new_p, "Hijacked")] {
        seed(
            &g,
            TEAM,
            "project",
            pid,
            later,
            json!({"id": pid, "workspaceId": "x", "name": name, "sortOrder": 0, "createdAt": 1, "updatedAt": later}),
        );
    }
    let mut evil = http_req("DELETE", "https://evil");
    evil.id = new_r.clone();
    evil.collection_id = a.with(|s| s.project(&new_p).unwrap().collections[0].id.clone());
    evil.updated_at = later;
    seed(&g, TEAM, "request", &new_r, later, serde_json::to_value(&evil).unwrap());
    a.run(&server, &g).await;
    a.with(|s| {
        let t = s.project(&new_p).unwrap();
        assert_eq!(t.project.name, "Payments");
        assert_eq!(s.bindings.project_spaces[&new_p], PERSONAL, "binding unchanged");
        assert!(t.requests.iter().any(|r| r.id == new_r && r.method == "POST"));
        // The resurrected old id is just a team project of its own.
        assert_eq!(s.bindings.project_spaces.get(&old_p).map(String::as_str), Some(TEAM));
        assert!(s.space_sync(&Scope::Space(TEAM.into())).foreign >= 1);
    });
}

#[tokio::test]
/// 04 §12 M2b asked for "the team gets the resurrection"; 06 §10 M2 (never
/// push orphans) wins: the newer offline edit survives here, held and
/// frozen, and is offered as a personal copy (04 §12 "M2b client as built").
async fn m2_a_newer_offline_edit_under_a_moved_out_project_is_held_frozen_and_saved_as_a_personal_copy() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Admin);
    team_exists(&g, "admin");
    let (old_p, old_c, old_r) = team_project(&a, &server, &g).await;
    // B, another device of the same admin, has the team project too.
    let b = second_device(&server, &g).await;
    bind_team(&b, &server, Role::Admin);
    b.run(&server, &g).await;
    assert!(b.with(|s| s.request(&old_r).is_some()));
    // A moves it to Personal; B, offline, edits the request afterwards.
    let moved = transfer::move_project(
        &a.store,
        Some(session(&server, &g)),
        &old_p,
        Some(PERSONAL),
        &no_progress,
    )
    .await
    .unwrap();
    a.run(&server, &g).await;
    assert!(server_ent(&g, TEAM, "project", &old_p).unwrap().deleted);
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.with(|s| {
        let mut r = s.request(&old_r).unwrap().clone();
        r.url = "https://api/orders/v2".into();
        s.save_request(&r).unwrap();
    });
    let pushes_before = pushed_ids(&g, TEAM).len();
    b.run(&server, &g).await;
    b.with(|s| {
        // The newer offline edit survives; its containers wait for a decision.
        assert_eq!(s.request(&old_r).unwrap().url, "https://api/orders/v2");
        assert!(s.project(&old_p).is_ok());
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert!(ss.frozen.contains(&sync_key(EntityType::Request, &old_r)));
        assert!(ss.held.contains(&sync_key(EntityType::Project, &old_p)));
        assert!(ss.held.contains(&sync_key(EntityType::Collection, &old_c)));
        assert_eq!(ss.issue.as_ref().unwrap().code, "heldDelete");
        // The personal copy arrived untouched.
        let copy = s.project(&moved.project_id).unwrap();
        assert_eq!(s.bindings.project_spaces[&moved.project_id], PERSONAL);
        assert_eq!(copy.requests[0].url, "https://api/orders");
    });
    // Nothing was pushed to the team: no orphans under a deleted project.
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before);
    assert!(server_ent(&g, TEAM, "request", &old_r).unwrap().deleted);
    // Save as personal copy: the edit lands in Personal under new ids.
    let copies = b.with(|s| s.resolve_frozen(TEAM, true).unwrap());
    assert_eq!(copies.len(), 1);
    b.run(&server, &g).await;
    b.with(|s| {
        assert!(s.project(&old_p).is_err(), "the held delete applied");
        let saved = s.project(&copies[0]).unwrap();
        assert_eq!(saved.project.name, "Payments (my changes)");
        assert_eq!(saved.requests[0].url, "https://api/orders/v2");
        assert_eq!(s.bindings.project_spaces[&copies[0]], PERSONAL);
        assert!(s.space_sync(&Scope::Space(TEAM.into())).frozen.is_empty());
    });
    assert!(pushed_ids(&g, PERSONAL).contains(&copies[0]));
    // A's personal copy is still the original.
    a.run(&server, &g).await;
    a.with(|s| {
        assert_eq!(
            s.project(&moved.project_id).unwrap().requests[0].url,
            "https://api/orders"
        )
    });
}

#[tokio::test]
async fn m2_a_partial_run_during_a_move_does_not_flip_and_converges() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let b = second_device(&server, &g).await;
    bind_team(&b, &server, Role::Editor);
    b.run(&server, &g).await;
    assert!(b.with(|s| s.project(&pid).is_ok()));
    let moved = transfer::move_project(
        &a.store,
        Some(session(&server, &g)),
        &pid,
        Some(TEAM),
        &no_progress,
    )
    .await
    .unwrap();
    a.run(&server, &g).await;
    assert!(server_ent(&g, PERSONAL, "project", &pid).unwrap().deleted);
    // B: Personal answers (with the tombstones), the team is down.
    g.0.lock()
        .unwrap()
        .fail
        .insert(TEAM.into(), (503, "starting".into()));
    b.run(&server, &g).await;
    b.with(|s| {
        assert!(s.project(&pid).is_ok(), "no flip: the delete waits");
        assert!(s.project(&moved.project_id).is_err());
    });
    g.0.lock().unwrap().fail.clear();
    let run = b.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    b.with(|s| {
        assert!(s.project(&pid).is_err());
        assert_eq!(s.bindings.project_spaces[&moved.project_id], TEAM);
        assert_eq!(
            s.project(&moved.project_id).unwrap().requests.len(),
            1,
            "the copy arrived whole"
        );
    });
}

#[tokio::test]
async fn m2_a_move_resumes_after_a_crash_with_the_same_ids() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    // The push lands but its response is lost: the move fails, nothing moved.
    g.0.lock()
        .unwrap()
        .lose_push_response
        .insert(TEAM.into());
    let err = transfer::move_project(
        &a.store,
        Some(session(&server, &g)),
        &pid,
        Some(TEAM),
        &no_progress,
    )
    .await
    .unwrap_err();
    assert!(matches!(err, transfer::TransferError::Sync(SyncError::Server(_))), "{err:?}");
    let plan = a.with(|s| {
        assert!(s.project(&pid).is_ok(), "nothing was moved");
        assert_eq!(s.bindings.project_spaces[&pid], PERSONAL);
        s.pending_moves().pop().unwrap()
    });
    let landed = g.0.lock().unwrap().spaces[TEAM].live("project");
    assert_eq!(landed, vec![plan.id_map[&pid].clone()]);
    // "Crash": a fresh store from disk keeps the plan.
    let reopened = a.reopen();
    assert_eq!(reopened.pending_moves(), vec![plan.clone()]);
    *a.store.lock().unwrap() = reopened;
    a.with(|s| {
        s.set_account(Some(ident(&server)));
    });
    g.0.lock().unwrap().lose_push_response.clear();
    let done = transfer::move_project(
        &a.store,
        Some(session(&server, &g)),
        &pid,
        Some(TEAM),
        &no_progress,
    )
    .await
    .unwrap();
    assert_eq!(done.project_id, plan.id_map[&pid], "same ids after the retry");
    assert_eq!(done.move_id, plan.move_id);
    assert_eq!(
        g.0.lock().unwrap().spaces[TEAM].live("project"),
        vec![done.project_id.clone()],
        "no duplicate in the team"
    );
    a.run(&server, &g).await;
    a.with(|s| {
        assert!(s.project(&pid).is_err());
        assert!(s.pending_moves().is_empty());
        assert_eq!(s.bindings.project_spaces[&done.project_id], TEAM);
        assert_eq!(
            space_sync::pending_total(s),
            0,
            "the copy is synced in the team"
        );
    });
    assert!(server_ent(&g, PERSONAL, "project", &pid).unwrap().deleted);
}

#[tokio::test]
async fn m2_a_failed_move_changes_nothing_and_cancel_tombstones_what_landed() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    g.0.lock()
        .unwrap()
        .lose_push_response
        .insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    let mut plan = a.with(|s| s.pending_moves().pop().unwrap());
    // Pretend the first batch was acknowledged, then cancel.
    plan.applied.insert(sync_key(EntityType::Project, &plan.id_map[&pid]), 1);
    a.with(|s| s.record_move_progress(&plan).unwrap());
    g.0.lock().unwrap().lose_push_response.clear();
    transfer::cancel_move(&a.store, Some(session(&server, &g)), &plan.move_id)
        .await
        .unwrap();
    assert!(server_ent(&g, TEAM, "project", &plan.id_map[&pid]).unwrap().deleted);
    a.with(|s| {
        assert!(s.pending_moves().is_empty());
        assert_eq!(s.bindings.project_spaces[&pid], PERSONAL);
    });
}

#[tokio::test]
async fn m2_only_admins_move_out_and_viewers_cant_move_in() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, _, _) = team_project(&a, &server, &g).await;
    let e = a.with(|s| s.move_check(&tp, Some(PERSONAL)).unwrap_err());
    assert!(e.to_string().contains("Only admins can move a project out of Acme"), "{e}");
    let e = a.with(|s| s.delete_project(&tp).unwrap_err());
    assert!(e.to_string().contains("Only admins can delete team projects"), "{e}");
    a.with(|s| {
        s.binding_mut(TEAM).unwrap().role = Some(Role::Viewer);
        assert!(matches!(
            s.move_check(&pid, Some(TEAM)).unwrap_err(),
            reqharbor_core::storage::StoreError::ReadOnlyRole(_)
        ));
        assert!(s.create_project_at("x", Some(TEAM.into())).is_err());
    });
}

#[tokio::test]
async fn m2_the_viewer_guard_blocks_mutators_and_makes_no_conflict_copies() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, tc, tr) = team_project(&a, &server, &g).await;
    // V is a viewer on another device.
    let v = second_device(&server, &g).await;
    bind_team(&v, &server, Role::Viewer);
    team_exists(&g, "viewer");
    v.run(&server, &g).await;
    v.with(|s| {
        assert!(s.rename_project(&tp, "x").is_err());
        assert!(s.create_collection(&tp, "c").is_err());
        assert!(s.delete_collection(&tc).is_err());
        let mut r = s.request(&tr).unwrap().clone();
        r.url = "https://mine".into();
        assert!(matches!(
            s.save_request(&r).unwrap_err(),
            reqharbor_core::storage::StoreError::ReadOnlyRole(_)
        ));
        assert!(s.move_node(reqharbor_core::storage::NodeKind::Request, &tr, Some(&tc), None, 0).is_err());
        assert!(s.delete_project(&tp).is_err());
        // Copying out is allowed: Duplicate to personal.
        let copy = s.duplicate_to_space(&tp, Some(PERSONAL), Some("Payments copy")).unwrap();
        assert_eq!(s.bindings.project_spaces[&copy.project.id], PERSONAL);
        // Make the request look locally changed (e.g. written by an older build).
        let k = sync_key(EntityType::Request, &tr);
        s.space_state_mut(&Scope::Space(TEAM.into()))
            .synced
            .insert(k, 1);
    });
    // An editor changes it; the viewer's pull takes it without a conflict copy.
    let mut theirs = a.with(|s| s.request(&tr).unwrap().clone());
    theirs.url = "https://api/orders/v3".into();
    theirs.updated_at = now_ms() + 1_000;
    seed(&g, TEAM, "request", &tr, theirs.updated_at, serde_json::to_value(&theirs).unwrap());
    let pushes_before = pushed_ids(&g, TEAM).len();
    let run = v.run(&server, &g).await;
    assert!(run.report.conflicts.is_empty());
    v.with(|s| {
        assert_eq!(s.request(&tr).unwrap().url, "https://api/orders/v3");
        let t = s.project(&tp).unwrap();
        assert!(!t.requests.iter().any(|r| r.name.contains("(conflict)")));
        assert_eq!(sync::collect_push_in(s, &Scope::Space(TEAM.into())).len(), 0);
    });
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before, "a viewer pushes nothing");
}

#[tokio::test]
async fn m2_a_role_downgrade_freezes_dirty_keys() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, _, tr) = team_project(&a, &server, &g).await;
    // An unsynced edit, then an admin makes us a viewer and edits the same request.
    a.with(|s| {
        let mut r = s.request(&tr).unwrap().clone();
        r.url = "https://mine".into();
        s.save_request(&r).unwrap();
    });
    let mut theirs = a.with(|s| s.request(&tr).unwrap().clone());
    theirs.url = "https://theirs".into();
    theirs.updated_at = now_ms() + 1_000;
    seed(&g, TEAM, "request", &tr, theirs.updated_at, serde_json::to_value(&theirs).unwrap());
    team_exists(&g, "viewer");
    g.0.lock().unwrap().roles.insert(TEAM.into(), "viewer".into());
    let pushes_before = pushed_ids(&g, TEAM).len();
    let run = a.run(&server, &g).await;
    assert!(run.error().is_none(), "{:?}", run.error());
    assert!(run.report.conflicts.is_empty());
    let key = sync_key(EntityType::Request, &tr);
    a.with(|s| {
        assert_eq!(s.binding(TEAM).unwrap().role, Some(Role::Viewer));
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert_eq!(ss.frozen, vec![key.clone()]);
        let issue = ss.issue.clone().unwrap();
        assert_eq!(issue.code, "roleChanged");
        assert_eq!(
            issue.message,
            "Your role in Acme changed to Viewer. You have 1 unsynced change."
        );
        assert_eq!(s.request(&tr).unwrap().url, "https://mine", "frozen: not overwritten");
        let st = space_sync::space_statuses(s);
        assert_eq!(st.iter().find(|x| x.space_id == TEAM).unwrap().frozen, 1);
    });
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before);
    // Discard: unfreeze and take the server's version.
    a.with(|s| assert!(s.resolve_frozen(TEAM, false).unwrap().is_empty()));
    a.run(&server, &g).await;
    a.with(|s| {
        assert_eq!(s.request(&tr).unwrap().url, "https://theirs");
        assert!(s.project(&tp).is_ok());
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert!(ss.frozen.is_empty() && ss.issue.is_none());
    });
}

#[tokio::test]
async fn m2_a_viewer_push_refused_as_a_whole_freezes_and_shows_the_banner() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (_, _, tr) = team_project(&a, &server, &g).await;
    a.with(|s| {
        let mut r = s.request(&tr).unwrap().clone();
        r.name = "Renamed".into();
        s.save_request(&r).unwrap();
    });
    // The server already enforces viewer; /v1/me/spaces still says editor.
    g.0.lock().unwrap().roles.insert(TEAM.into(), "viewer".into());
    let run = a.run(&server, &g).await;
    assert!(run.error().is_none(), "a refused push is the banner, not an error");
    a.with(|s| {
        assert_eq!(s.binding(TEAM).unwrap().role, Some(Role::Viewer));
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert_eq!(ss.frozen, vec![sync_key(EntityType::Request, &tr)]);
        assert_eq!(ss.issue.as_ref().unwrap().code, "roleChanged");
    });
    // Save as personal copy keeps the edit under new ids in Personal.
    let copies = a.with(|s| s.resolve_frozen(TEAM, true).unwrap());
    a.with(|s| {
        let t = s.project(&copies[0]).unwrap();
        assert_eq!(t.requests[0].name, "Renamed");
        assert_ne!(t.requests[0].id, tr);
    });
}

#[tokio::test]
async fn m2_a_project_delete_refused_with_forbidden_role_is_discarded_back() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Admin);
    team_exists(&g, "admin");
    let (tp, _, tr) = team_project(&a, &server, &g).await;
    a.with(|s| s.delete_project(&tp).unwrap());
    // Demoted to editor before the push: the project tombstone is refused.
    team_exists(&g, "editor");
    g.0.lock().unwrap().roles.insert(TEAM.into(), "editor".into());
    a.run(&server, &g).await;
    let pk = sync_key(EntityType::Project, &tp);
    a.with(|s| {
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert_eq!(ss.frozen, vec![pk.clone()]);
        assert!(ss.issue.as_ref().unwrap().message.contains("Only admins can delete projects in Acme"));
        assert!(
            s.space_state(&Scope::Space(TEAM.into()))
                .tombstones
                .iter()
                .any(|t| t.id == tp),
            "the tombstone is kept"
        );
    });
    assert!(!server_ent(&g, TEAM, "project", &tp).unwrap().deleted);
    assert!(
        !server_ent(&g, TEAM, "request", &tr).unwrap().deleted,
        "the children's tombstones wait with the refused project tombstone"
    );
    a.with(|s| s.resolve_frozen(TEAM, false).unwrap());
    a.run(&server, &g).await;
    a.with(|s| {
        let t = s.project(&tp).expect("Discard brings the project back");
        assert_eq!(s.bindings.project_spaces[&tp], TEAM);
        assert!(t.requests.iter().any(|r| r.id == tr), "with its content");
        assert!(s.space_state(&Scope::Space(TEAM.into())).tombstones.is_empty());
    });
    assert!(!server_ent(&g, TEAM, "request", &tr).unwrap().deleted);
}

#[tokio::test]
async fn m2_team_pushes_leave_device_local_paths_here() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (_, c, _) = team_project(&a, &server, &g).await;
    let rid = a.with(|s| {
        let mut r = http_req("POST", "https://api/upload");
        r.body.body_type = BodyType::Binary;
        r.body.binary_path = Some("/Users/priya/secret.bin".into());
        s.create_request(&c, None, r).unwrap().id
    });
    a.run(&server, &g).await;
    let pushed = server_ent(&g, TEAM, "request", &rid).unwrap().data.unwrap();
    assert_eq!(pushed["body"]["binaryPath"], "");
    // A pulled version keeps this device's path.
    let mut theirs: Request = serde_json::from_value(pushed).unwrap();
    theirs.name = "Upload v2".into();
    theirs.updated_at = now_ms() + 1_000;
    seed(&g, TEAM, "request", &rid, theirs.updated_at, serde_json::to_value(&theirs).unwrap());
    a.run(&server, &g).await;
    a.with(|s| {
        let r = s.request(&rid).unwrap();
        assert_eq!(r.name, "Upload v2");
        assert_eq!(r.body.binary_path.as_deref(), Some("/Users/priya/secret.bin"));
    });
}

#[tokio::test]
async fn m2_copy_to_project_and_duplicate_use_new_ids_and_leave_the_source() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, tc, tr) = team_project(&a, &server, &g).await;
    let (cid, rid) = a.with(|s| {
        let t = s.project(&pid).unwrap();
        (t.collections[0].id.clone(), t.requests[0].id.clone())
    });
    a.with(|s| {
        let (t, copy) = s.copy_to_project("collection", &tc, &pid, None, None).unwrap();
        assert_ne!(copy, tc);
        let col = t.collections.iter().find(|c| c.id == copy).unwrap();
        assert_eq!(col.name, "Orders");
        assert!(t.requests.iter().any(|r| r.collection_id == copy && r.id != tr));
        let (t, copy) = s
            .copy_to_project("request", &rid, &tp, Some(&tc), None)
            .unwrap();
        assert!(t.requests.iter().any(|r| r.id == copy && r.collection_id == tc));
        // Without a target collection, one named after the source's is made.
        let (t, _) = s.copy_to_project("request", &tr, &pid, None, None).unwrap();
        assert_eq!(t.collections.iter().filter(|c| c.name == "Orders").count(), 2);
        assert!(s.project(&pid).unwrap().collections.iter().any(|c| c.id == cid));
        let p = s.copy_preview("request", &rid, &tp).unwrap();
        assert_eq!(p.target_kind, "team");
    });
    a.run(&server, &g).await;
    assert!(pushed_ids(&g, TEAM).len() >= 4);
}

// ───────────────────────────── M2 QA round 1 regressions ─────────────────────────────

/// QA blocker: with B7 deferring the team's deletes (another space failed),
/// a stale edit under a pulled tombstone was pushed as an orphan and lost on
/// the next run.
#[tokio::test]
async fn qa1_deferred_deletes_still_hold_a_newer_edit_and_push_no_orphan() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Admin);
    team_exists(&g, "admin");
    let (old_p, old_c, old_r) = team_project(&a, &server, &g).await;
    let b = second_device(&server, &g).await;
    bind_team(&b, &server, Role::Admin);
    b.run(&server, &g).await;
    transfer::move_project(&a.store, Some(session(&server, &g)), &old_p, Some(PERSONAL), &no_progress)
        .await
        .unwrap();
    a.run(&server, &g).await;
    std::thread::sleep(std::time::Duration::from_millis(5));
    b.with(|s| {
        let mut r = s.request(&old_r).unwrap().clone();
        r.url = "https://api/orders/v2".into();
        s.save_request(&r).unwrap();
    });
    // Personal is briefly down: B7 defers the team's deletes.
    g.0.lock().unwrap().fail.insert(PERSONAL.into(), (503, "starting".into()));
    let pushes_before = pushed_ids(&g, TEAM).len();
    b.run(&server, &g).await;
    g.0.lock().unwrap().fail.clear();
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before, "nothing pushed to the team");
    assert!(server_ent(&g, TEAM, "request", &old_r).unwrap().deleted);
    let rk = sync_key(EntityType::Request, &old_r);
    b.with(|s| {
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert!(ss.frozen.contains(&rk), "the edit is frozen at once");
        assert!(ss.held.contains(&sync_key(EntityType::Project, &old_p)));
        assert!(ss.held.contains(&sync_key(EntityType::Collection, &old_c)));
    });
    // The next run applies the deletes: the edit is still held, not lost.
    b.run(&server, &g).await;
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before);
    b.with(|s| {
        assert_eq!(s.request(&old_r).unwrap().url, "https://api/orders/v2");
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert!(ss.frozen.contains(&rk));
        assert!(ss.held.contains(&sync_key(EntityType::Project, &old_p)));
        assert!(ss.deferred_deletes.is_empty());
    });
}

/// QA blocker, the other half: an older dirty edit under a deferred
/// tombstone isn't pushed either (it would land as an orphan).
#[tokio::test]
async fn qa1_nothing_under_a_deferred_tombstone_is_pushed() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Admin);
    team_exists(&g, "admin");
    let (tp, _, tr) = team_project(&a, &server, &g).await;
    // An older local edit, then the project is deleted on the server.
    a.with(|s| {
        let mut r = s.request(&tr).unwrap().clone();
        r.url = "https://older".into();
        s.save_request(&r).unwrap();
    });
    std::thread::sleep(std::time::Duration::from_millis(5));
    let later = now_ms() + 1_000;
    tombstone(&g, TEAM, "request", &tr, later);
    tombstone(&g, TEAM, "project", &tp, later);
    g.0.lock().unwrap().fail.insert(PERSONAL.into(), (503, "starting".into()));
    let pushes_before = pushed_ids(&g, TEAM).len();
    a.run(&server, &g).await;
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before);
    a.with(|s| {
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert!(ss.deferred_deletes.contains(&sync_key(EntityType::Project, &tp)));
        assert!(sync::collect_push_in(s, &Scope::Space(TEAM.into())).is_empty());
    });
    g.0.lock().unwrap().fail.clear();
    a.run(&server, &g).await;
    a.with(|s| assert!(s.project(&tp).is_err(), "the older edit loses to the delete"));
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before);
}

/// QA major: a sync between a failed move (push landed, answer lost) and
/// its resume pulled the partial copy as a second project; the resume then
/// duplicated the environment.
#[tokio::test]
async fn qa1_a_sync_between_a_failed_move_and_its_resume_makes_no_duplicates() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    let env_id = a.with(|s| s.create_environment(Some(&pid), "Dev").unwrap().id);
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    g.0.lock().unwrap().lose_push_response.insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    g.0.lock().unwrap().lose_push_response.clear();
    let projects_before = a.with(|s| s.project_list().len());
    a.run(&server, &g).await;
    let plan = a.with(|s| s.pending_moves().pop().unwrap());
    let copy_pid = plan.id_map[&pid].clone();
    let copy_env = plan.id_map[&env_id].clone();
    a.with(|s| {
        assert_eq!(s.project_list().len(), projects_before, "no partial copy pulled");
        assert!(s.project(&copy_pid).is_err());
        assert_eq!(s.pending_move_list()[0].project_name, "Work");
    });
    let done = transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    assert_eq!(done.project_id, copy_pid);
    a.run(&server, &g).await;
    a.with(|s| {
        assert_eq!(s.environments.iter().filter(|e| e.id == copy_env).count(), 1);
        assert_eq!(s.project_list().iter().filter(|p| p.name == "Work").count(), 1);
    });
    let reopened = a.reopen();
    assert_eq!(reopened.environments.iter().filter(|e| e.id == copy_env).count(), 1);
}

/// QA major: a move to another target first undoes the older plan.
#[tokio::test]
async fn qa1_a_move_to_another_target_cancels_the_older_plan() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    g.0.lock().unwrap().lose_push_response.insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    g.0.lock().unwrap().lose_push_response.clear();
    let mut plan = a.with(|s| s.pending_moves().pop().unwrap());
    // The project landed; pretend its acknowledgement was recorded.
    let copy_key = sync_key(EntityType::Project, &plan.id_map[&pid]);
    plan.applied.insert(copy_key, 1);
    a.with(|s| s.record_move_progress(&plan).unwrap());
    // Now to this device only instead.
    let moved = transfer::move_project(&a.store, Some(session(&server, &g)), &pid, None, &no_progress)
        .await
        .unwrap();
    assert!(server_ent(&g, TEAM, "project", &plan.id_map[&pid]).unwrap().deleted, "the older plan was undone");
    a.with(|s| {
        assert!(s.pending_moves().is_empty());
        assert!(!s.bindings.project_spaces.contains_key(&moved.project_id));
    });
}

/// QA major: the copy is saved before the source is removed, and a resume
/// after a crash between the two steps finishes without losing anything.
#[tokio::test]
async fn qa1_a_crash_between_saving_the_copy_and_removing_the_source_loses_nothing() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.with(|s| {
        let env = s.create_environment(Some(&pid), "Dev").unwrap();
        let mut e = env.clone();
        e.variables = vec![serde_json::from_value(json!({"key": "token", "value": "s3cret", "enabled": true, "secret": true})).unwrap()];
        s.update_environment(&e).unwrap();
    });
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    // Push the copy, then "crash" right after step one of the local step.
    g.0.lock().unwrap().lose_push_response.insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    g.0.lock().unwrap().lose_push_response.clear();
    let plan = a.with(|s| s.pending_moves().pop().unwrap());
    let copy = a.with(|s| s.finish_move_copy(&plan).unwrap());
    let reopened = a.reopen();
    assert!(reopened.project(&pid).is_ok(), "the source is still here");
    assert!(reopened.project(&copy).is_ok(), "and so is the saved copy");
    assert!(
        reopened
            .environments
            .iter()
            .any(|e| e.project_id.as_deref() == Some(copy.as_str()) && e.variables[0].value == "s3cret"),
        "with its secret value"
    );
    *a.store.lock().unwrap() = reopened;
    a.with(|s| {
        s.set_account(Some(ident(&server)));
    });
    let done = transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    assert_eq!(done.project_id, copy);
    a.with(|s| {
        assert!(s.project(&pid).is_err());
        assert_eq!(s.project_list().iter().filter(|p| p.name == "Work").count(), 1);
        assert_eq!(s.environments.iter().filter(|e| e.project_id.as_deref() == Some(copy.as_str())).count(), 1);
        assert!(s.pending_moves().is_empty());
    });
    // Idempotent: finishing the same plan again only drops it.
    a.with(|s| {
        s.record_move_progress(&plan).unwrap();
        assert_eq!(s.finish_move(&plan).unwrap(), copy);
        assert!(s.pending_moves().is_empty());
        assert_eq!(s.project_list().iter().filter(|p| p.name == "Work").count(), 1);
    });
}

/// QA minor 13: an entity deleted between a failed move and its resume is
/// removed from the target too.
#[tokio::test]
async fn qa1_an_entity_deleted_before_the_resume_is_removed_from_the_target() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let rid = a.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    g.0.lock().unwrap().lose_push_response.insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    g.0.lock().unwrap().lose_push_response.clear();
    let mut plan = a.with(|s| s.pending_moves().pop().unwrap());
    let copy_r = plan.id_map[&rid].clone();
    // Record what landed (as a lost answer would have).
    let landed: Vec<(String, i64)> = {
        let st = g.0.lock().unwrap();
        st.spaces[TEAM].entities.iter().map(|((t, id), e)| (format!("{t}:{id}"), e.updated_at)).collect()
    };
    plan.applied.extend(landed);
    a.with(|s| s.record_move_progress(&plan).unwrap());
    a.with(|s| s.delete_request(&rid).unwrap());
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    assert!(server_ent(&g, TEAM, "request", &copy_r).unwrap().deleted);
    a.run(&server, &g).await;
    a.with(|s| assert!(s.request(&copy_r).is_none(), "not pulled back"));
}

/// QA minor 8: a viewer may pick an environment, locally only.
#[tokio::test]
async fn qa1_a_viewer_choosing_an_environment_changes_nothing_to_sync() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, _, _) = team_project(&a, &server, &g).await;
    let env = a.with(|s| s.create_environment(Some(&tp), "Prod").unwrap().id);
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Viewer);
    a.with(|s| {
        let before = s.project(&tp).unwrap().project.updated_at;
        let p = s.set_active_environment(&tp, Some(&env)).unwrap();
        assert_eq!(p.active_environment_id.as_deref(), Some(env.as_str()));
        assert_eq!(p.updated_at, before, "not marked changed");
        assert!(sync::dirty_keys_in(s, &Scope::Space(TEAM.into())).is_empty());
    });
}

/// QA minor 12: a held collection is saved on its own, not with the whole
/// team project.
#[tokio::test]
async fn qa1_saving_a_held_collection_copies_only_that_collection() {
    let (server, g) = go().await;
    let (a, _) = personal_device(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let (tp, tc, tr) = team_project(&a, &server, &g).await;
    let other = a.with(|s| s.create_collection(&tp, "Billing").unwrap().id);
    a.run(&server, &g).await;
    std::thread::sleep(std::time::Duration::from_millis(5));
    // An older delete on the server, a newer edit here.
    let deleted_at = now_ms() - 1;
    a.with(|s| {
        let mut r = s.request(&tr).unwrap().clone();
        r.url = "https://mine".into();
        s.save_request(&r).unwrap();
    });
    tombstone(&g, TEAM, "request", &tr, deleted_at);
    tombstone(&g, TEAM, "collection", &tc, deleted_at);
    a.run(&server, &g).await;
    a.with(|s| {
        let ss = s.space_sync(&Scope::Space(TEAM.into()));
        assert_eq!(ss.held, vec![sync_key(EntityType::Collection, &tc)]);
    });
    let copies = a.with(|s| s.resolve_frozen(TEAM, true).unwrap());
    a.with(|s| {
        let t = s.project(&copies[0]).unwrap();
        assert_eq!(t.project.name, "Payments (my changes)");
        assert_eq!(t.collections.len(), 1, "only the held collection");
        assert_eq!(t.collections[0].name, "Orders");
        assert_eq!(t.requests[0].url, "https://mine");
        assert_eq!(s.bindings.project_spaces[&copies[0]], PERSONAL);
        assert!(s.project(&tp).unwrap().collections.iter().any(|c| c.id == other));
    });
}

// ───────────────────────────── M2 QA round 2 regressions ─────────────────────────────

/// A failed move whose batches were all acknowledged: the plan holds them.
async fn acknowledged_failed_move(server: &MockServer, g: &Go, a: &Device, pid: &str) -> transfer::MovePlan {
    g.0.lock().unwrap().lose_push_response.insert(TEAM.into());
    transfer::move_project(&a.store, Some(session(server, g)), pid, Some(TEAM), &no_progress)
        .await
        .unwrap_err();
    g.0.lock().unwrap().lose_push_response.clear();
    let mut plan = a.with(|s| s.pending_moves().pop().unwrap());
    let changes = a.with(|s| s.move_changes(&mut plan.clone()).unwrap());
    for c in &changes {
        plan.applied.insert(sync_key(c.entity_type, &c.id), c.updated_at);
    }
    a.with(|s| s.record_move_progress(&plan).unwrap());
    plan
}

/// QA2 major (the probe): a teammate's edit of the partial copy, skipped by
/// pulls while the move was pending, applies once the move finishes.
#[tokio::test]
async fn qa2_a_teammate_edit_of_an_acknowledged_pending_copy_applies_after_the_move() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let plan = acknowledged_failed_move(&server, &g, &a, &pid).await;
    let rid = a.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    let new_r = plan.id_map[&rid].clone();
    let mut theirs: Request =
        serde_json::from_value(server_ent(&g, TEAM, "request", &new_r).unwrap().data.unwrap()).unwrap();
    theirs.url = "https://teammate".into();
    theirs.updated_at = now_ms() + 1_000;
    seed(&g, TEAM, "request", &new_r, theirs.updated_at, serde_json::to_value(&theirs).unwrap());
    a.run(&server, &g).await; // skipped: the copy isn't here yet
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    a.run(&server, &g).await;
    assert_eq!(a.with(|s| s.request(&new_r).map(|r| r.url.clone())).as_deref(), Some("https://teammate"));
    // And a later local edit doesn't overwrite it blindly: it builds on it.
    let srv = server_ent(&g, TEAM, "request", &new_r).unwrap().data.unwrap()["url"].clone();
    assert_eq!(srv, "https://teammate");
}

/// QA2 major: a teammate's delete in the partial copy applies too.
#[tokio::test]
async fn qa2_a_teammate_delete_in_a_pending_copy_applies_after_the_move() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let plan = acknowledged_failed_move(&server, &g, &a, &pid).await;
    let rid = a.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    let new_r = plan.id_map[&rid].clone();
    tombstone(&g, TEAM, "request", &new_r, now_ms() + 1_000);
    a.run(&server, &g).await;
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    a.run(&server, &g).await;
    a.with(|s| {
        assert!(s.request(&new_r).is_none(), "the teammate's delete applied");
        assert!(s.project(&plan.id_map[&pid]).is_ok());
    });
    assert!(server_ent(&g, TEAM, "request", &new_r).unwrap().deleted, "not resurrected");
}

/// QA3 minor: a crash after the source's removal but before the cursor
/// rewind was saved still rewinds on resume, so the teammate's edit applies.
#[tokio::test]
async fn qa3_a_crash_after_removing_the_source_still_rewinds_the_target() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let plan = acknowledged_failed_move(&server, &g, &a, &pid).await;
    let rid = a.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    let new_r = plan.id_map[&rid].clone();
    let mut theirs: Request =
        serde_json::from_value(server_ent(&g, TEAM, "request", &new_r).unwrap().data.unwrap()).unwrap();
    theirs.url = "https://teammate".into();
    theirs.updated_at = now_ms() + 1_000;
    seed(&g, TEAM, "request", &new_r, theirs.updated_at, serde_json::to_value(&theirs).unwrap());
    a.run(&server, &g).await; // skipped: the copy isn't here yet
    // The interrupted finish: copy saved, source removed, plan still there,
    // cursor never rewound.
    a.with(|s| {
        s.finish_move_copy(&plan).unwrap();
        s.delete_project(&pid).unwrap();
        assert!(s.move_source_gone(&plan));
        assert_eq!(s.pending_move_list().len(), 1);
    });
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    a.with(|s| assert!(s.pending_move_list().is_empty()));
    a.run(&server, &g).await;
    assert_eq!(a.with(|s| s.request(&new_r).map(|r| r.url.clone())).as_deref(), Some("https://teammate"));
}

/// QA2 minor 2: finishing a move whose copy was already saved keeps the
/// copy's later edits (it isn't rebuilt from the source).
#[tokio::test]
async fn qa2_finishing_keeps_edits_made_to_an_already_saved_copy() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let plan = acknowledged_failed_move(&server, &g, &a, &pid).await;
    let copy = a.with(|s| s.finish_move_copy(&plan).unwrap());
    let rid = a.with(|s| s.project(&pid).unwrap().requests[0].id.clone());
    let new_r = plan.id_map[&rid].clone();
    a.with(|s| {
        let mut r = s.request(&new_r).unwrap().clone();
        r.url = "https://edited-copy".into();
        s.save_request(&r).unwrap();
    });
    let pushes_before = pushed_ids(&g, TEAM).len();
    let done = transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    assert_eq!(done.project_id, copy);
    assert_eq!(pushed_ids(&g, TEAM).len(), pushes_before, "the source isn't pushed over the copy");
    a.with(|s| {
        assert!(s.project(&pid).is_err());
        assert_eq!(s.request(&new_r).unwrap().url, "https://edited-copy");
    });
    a.run(&server, &g).await;
    assert_eq!(server_ent(&g, TEAM, "request", &new_r).unwrap().data.unwrap()["url"], "https://edited-copy");
}

/// QA2 minor 3: a move whose copy is already saved can be finished, not
/// cancelled, and a move to another target is refused until it is.
#[tokio::test]
async fn qa2_a_move_with_a_saved_copy_must_be_finished_first() {
    let (server, g) = go().await;
    let (a, pid) = personal_device(&server, &g).await;
    a.run(&server, &g).await;
    bind_team(&a, &server, Role::Editor);
    team_exists(&g, "editor");
    let plan = acknowledged_failed_move(&server, &g, &a, &pid).await;
    a.with(|s| s.finish_move_copy(&plan).unwrap());
    a.with(|s| {
        let p = &s.pending_move_list()[0];
        assert!(p.copy_saved);
        assert_eq!(p.target_name, "Acme");
    });
    let err = transfer::cancel_move(&a.store, Some(session(&server, &g)), &plan.move_id)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Finish it instead"), "{err}");
    let err = transfer::move_project(&a.store, Some(session(&server, &g)), &pid, None, &no_progress)
        .await
        .unwrap_err();
    assert_eq!(err.to_string(), "Finish the move to Acme first: its copy is already saved here.");
    a.with(|s| {
        assert!(s.project(&pid).is_ok() && s.project(&plan.id_map[&pid]).is_ok(), "nothing changed");
        assert_eq!(s.pending_moves().len(), 1);
    });
    transfer::move_project(&a.store, Some(session(&server, &g)), &pid, Some(TEAM), &no_progress)
        .await
        .unwrap();
    a.with(|s| assert!(s.pending_moves().is_empty()));
}

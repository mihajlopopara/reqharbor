//! Thin `#[tauri::command]` wrappers over `reqharbor-core`.
//! Every command is documented in docs/API_CONTRACT.md; keep them in sync.

use crate::auth_rt::{self, AuthState, AuthStatus};
use crate::error::{CmdResult, CommandError};
use crate::state::{AppState, SyncStatus};
use crate::sync_rt::{run_sync, schedule_push};
use reqharbor_core::auth::SessionInfo;
use reqharbor_core::curl::{self, CurlImport, ExportOptions};
use reqharbor_core::history::HistorySummary;
use reqharbor_core::model::*;
use reqharbor_core::openapi::{self, ImportReport, SpecInfo};
use reqharbor_core::prepare::PrepareError;
use reqharbor_core::send::{ResponseMeta, send_request as core_send};
use reqharbor_core::server_info::Discovery;
use reqharbor_core::share::{self, ShareOptions, SharePreview};
use reqharbor_core::spaces::{FirstSyncCandidate, LegacyLink, LegacyMode, ProjectLocation, Scope};
use reqharbor_core::storage::{ChildCounts, CorruptFile, NodeKind, Store};
use reqharbor_core::sync::{
    self, CreatedWorkspace, RemoteWorkspace, ShareInfo, ShareKind, SyncClient, SyncReport,
};
use reqharbor_core::vars::VariableInfo;
use reqharbor_core::{format, urlx};
use serde::Serialize;
use tauri::{AppHandle, Manager, State};

type S<'a> = State<'a, AppState>;

// ------------------------------------------------------------------ bootstrap

/// Environments with Global first, then by project and sortOrder.
fn sorted_envs(store: &reqharbor_core::storage::Store) -> Vec<Environment> {
    let mut envs = store.environments.clone();
    envs.sort_by(|a, b| {
        (a.project_id.is_some(), &a.project_id, a.sort_order, &a.name).cmp(&(
            b.project_id.is_some(),
            &b.project_id,
            b.sort_order,
            &b.name,
        ))
    });
    envs
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Bootstrap {
    pub workspace: WorkspaceInfo,
    pub projects: Vec<Project>,
    pub environments: Vec<Environment>,
    pub settings: Settings,
    pub corrupt_files: Vec<CorruptFile>,
    pub sync: SyncStatus,
    pub platform: String,
    pub data_dir: String,
    /// This build's version; requests whose `minClientVersion` is newer are
    /// read-only (AC-FWD-4).
    pub app_version: String,
    /// The `accounts` flag: show Settings › Account and sign-in.
    pub accounts: bool,
}

#[tauri::command]
pub fn bootstrap(state: S<'_>) -> CmdResult<Bootstrap> {
    let store = state.store();
    Ok(Bootstrap {
        workspace: WorkspaceInfo {
            id: store.workspace.id.clone(),
            name: store.workspace.name.clone(),
        },
        projects: store.project_list(),
        environments: sorted_envs(&store),
        settings: store.settings.clone(),
        corrupt_files: store.corrupt.clone(),
        sync: state.sync_status(),
        platform: std::env::consts::OS.to_string(),
        data_dir: store.dir().to_string_lossy().into_owned(),
        app_version: CLIENT_VERSION.to_string(),
        accounts: crate::auth_rt::accounts_enabled(),
    })
}

#[tauri::command]
pub fn get_settings(state: S<'_>) -> Settings {
    state.store().settings.clone()
}

#[tauri::command]
pub fn save_settings(state: S<'_>, settings: Settings) -> CmdResult<Settings> {
    let mut store = state.store();
    store.settings = settings;
    store.save_settings()?;
    Ok(store.settings.clone())
}

// ------------------------------------------------------------------- projects

#[tauri::command]
pub fn list_projects(state: S<'_>) -> Vec<Project> {
    state.store().project_list()
}

#[tauri::command]
pub fn get_project_tree(state: S<'_>, project_id: String) -> CmdResult<ProjectTree> {
    Ok(state.store().project(&project_id)?.clone())
}

/// Run a mutation, then schedule the debounced sync push.
fn mutate<T>(
    app: &AppHandle,
    f: impl FnOnce(&mut reqharbor_core::storage::Store) -> CmdResult<T>,
) -> CmdResult<T> {
    let out = {
        let state = app.state::<AppState>();
        let mut store = state.store();
        f(&mut store)?
    };
    schedule_push(app);
    Ok(out)
}

/// `location`: omitted or null for the default (Personal when signed in),
/// `"local"` for this device only, or a space id (UX §5.2).
#[tauri::command]
pub fn create_project(
    app: AppHandle,
    name: String,
    location: Option<String>,
) -> CmdResult<ProjectTree> {
    mutate(&app, |s| {
        Ok(match location.as_deref() {
            None | Some("") => s.create_project(&name)?,
            Some("local") => s.create_project_at(&name, None)?,
            Some(space) => s.create_project_at(&name, Some(space.to_string()))?,
        })
    })
}

#[tauri::command]
pub fn rename_project(app: AppHandle, project_id: String, name: String) -> CmdResult<Project> {
    mutate(&app, |s| Ok(s.rename_project(&project_id, &name)?))
}

#[tauri::command]
pub fn duplicate_project(app: AppHandle, project_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.duplicate_project(&project_id)?))
}

#[tauri::command]
pub fn delete_project(app: AppHandle, project_id: String) -> CmdResult<Vec<Project>> {
    mutate(&app, |s| {
        s.delete_project(&project_id)?;
        Ok(s.project_list())
    })
}

#[tauri::command]
pub fn move_project(app: AppHandle, project_id: String, index: usize) -> CmdResult<Vec<Project>> {
    mutate(&app, |s| Ok(s.move_project(&project_id, index)?))
}

#[tauri::command]
pub fn set_active_environment(
    app: AppHandle,
    project_id: String,
    environment_id: Option<String>,
) -> CmdResult<Project> {
    mutate(&app, |s| {
        Ok(s.set_active_environment(&project_id, environment_id.as_deref())?)
    })
}

#[tauri::command]
pub fn child_counts(state: S<'_>, kind: String, id: String) -> CmdResult<ChildCounts> {
    Ok(state.store().child_counts(&kind, &id)?)
}

// ---------------------------------------------------------------- collections

#[tauri::command]
pub fn create_collection(
    app: AppHandle,
    project_id: String,
    name: String,
) -> CmdResult<Collection> {
    mutate(&app, |s| Ok(s.create_collection(&project_id, &name)?))
}

#[tauri::command]
pub fn update_collection(app: AppHandle, collection: Collection) -> CmdResult<Collection> {
    mutate(&app, |s| Ok(s.update_collection(&collection)?))
}

#[tauri::command]
pub fn duplicate_collection(app: AppHandle, collection_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.duplicate_collection(&collection_id)?))
}

#[tauri::command]
pub fn delete_collection(app: AppHandle, collection_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.delete_collection(&collection_id)?))
}

// -------------------------------------------------------------------- folders

#[tauri::command]
pub fn create_folder(
    app: AppHandle,
    collection_id: String,
    parent_folder_id: Option<String>,
    name: String,
) -> CmdResult<Folder> {
    mutate(&app, |s| {
        Ok(s.create_folder(&collection_id, parent_folder_id.as_deref(), &name)?)
    })
}

#[tauri::command]
pub fn rename_folder(app: AppHandle, folder_id: String, name: String) -> CmdResult<Folder> {
    mutate(&app, |s| Ok(s.rename_folder(&folder_id, &name)?))
}

#[tauri::command]
pub fn duplicate_folder(app: AppHandle, folder_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.duplicate_folder(&folder_id)?))
}

#[tauri::command]
pub fn delete_folder(app: AppHandle, folder_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.delete_folder(&folder_id)?))
}

// ------------------------------------------------------------------- requests

#[tauri::command]
pub fn create_request(
    app: AppHandle,
    collection_id: String,
    folder_id: Option<String>,
    request: Option<Request>,
) -> CmdResult<Request> {
    mutate(&app, |s| {
        Ok(s.create_request(
            &collection_id,
            folder_id.as_deref(),
            request.unwrap_or_default(),
        )?)
    })
}

#[tauri::command]
pub fn save_request(app: AppHandle, request: Request) -> CmdResult<Request> {
    mutate(&app, |s| Ok(s.save_request(&request)?))
}

#[tauri::command]
pub fn rename_request(app: AppHandle, request_id: String, name: String) -> CmdResult<Request> {
    mutate(&app, |s| Ok(s.rename_request(&request_id, &name)?))
}

#[tauri::command]
pub fn duplicate_request(app: AppHandle, request_id: String) -> CmdResult<Request> {
    mutate(&app, |s| Ok(s.duplicate_request(&request_id)?))
}

#[tauri::command]
pub fn delete_request(app: AppHandle, request_id: String) -> CmdResult<ProjectTree> {
    mutate(&app, |s| Ok(s.delete_request(&request_id)?))
}

#[tauri::command]
pub fn move_node(
    app: AppHandle,
    kind: NodeKind,
    id: String,
    target_collection_id: Option<String>,
    target_folder_id: Option<String>,
    index: usize,
) -> CmdResult<ProjectTree> {
    mutate(&app, |s| {
        Ok(s.move_node(
            kind,
            &id,
            target_collection_id.as_deref(),
            target_folder_id.as_deref(),
            index,
        )?)
    })
}

// --------------------------------------------------------------- environments

#[tauri::command]
pub fn list_environments(state: S<'_>) -> Vec<Environment> {
    sorted_envs(&state.store())
}

#[tauri::command]
pub fn create_environment(
    app: AppHandle,
    project_id: String,
    name: String,
) -> CmdResult<Environment> {
    mutate(
        &app,
        |s| Ok(s.create_environment(Some(&project_id), &name)?),
    )
}

#[tauri::command]
pub fn update_environment(app: AppHandle, environment: Environment) -> CmdResult<Environment> {
    mutate(&app, |s| Ok(s.update_environment(&environment)?))
}

#[tauri::command]
pub fn duplicate_environment(app: AppHandle, environment_id: String) -> CmdResult<Environment> {
    mutate(&app, |s| Ok(s.duplicate_environment(&environment_id)?))
}

#[tauri::command]
pub fn delete_environment(app: AppHandle, environment_id: String) -> CmdResult<Vec<Environment>> {
    mutate(&app, |s| {
        s.delete_environment(&environment_id)?;
        Ok(sorted_envs(s))
    })
}

/// Variables visible to a project (active env, then Global) with their source
/// (AC-ENV-4). Secret values are included; the UI masks them.
#[tauri::command]
pub fn get_variables(state: S<'_>, project_id: Option<String>) -> Vec<VariableInfo> {
    state.store().var_scope(project_id.as_deref()).visible()
}

/// The dynamic-variable catalogue (`{{$uuid}}`, `{{$randomEmail}}`, …) for
/// highlighting, hover and autocomplete (AC-ENV-8..10).
#[tauri::command]
pub fn get_dynamic_variables() -> Vec<reqharbor_core::dynamic::DynamicVariableInfo> {
    reqharbor_core::dynamic::catalogue()
}

// -------------------------------------------------------------------- history

#[tauri::command]
pub fn list_history(state: S<'_>) -> Vec<HistorySummary> {
    state.store().history.list()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenedHistory {
    /// The entry; `entry.response.body` is emptied (fetch it via `get_response_body`).
    pub entry: HistoryEntry,
    pub response: Option<ResponseWithId>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseWithId {
    pub response_id: String,
    #[serde(flatten)]
    pub meta: ResponseMeta,
}

/// AC-HIST-4: restore a history entry without re-sending.
#[tauri::command]
pub fn open_history_entry(state: S<'_>, history_id: String) -> CmdResult<OpenedHistory> {
    let mut entry = state.store().history.get(&history_id)?;
    let response = entry.response.as_mut().map(|h| {
        let (meta, body) = ResponseMeta::from_history(h);
        h.body.clear();
        let response_id = format!("history:{history_id}");
        state.put_response(response_id.clone(), body);
        ResponseWithId { response_id, meta }
    });
    Ok(OpenedHistory { entry, response })
}

#[tauri::command]
pub fn delete_history_entry(state: S<'_>, history_id: String) -> CmdResult<()> {
    Ok(state.store().history.delete(&history_id)?)
}

#[tauri::command]
pub fn clear_history(state: S<'_>) -> CmdResult<()> {
    Ok(state.store().history.clear()?)
}

// ----------------------------------------------------------------------- send

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SendResult {
    pub execution_id: String,
    pub history_id: String,
    pub method: String,
    /// URL as sent, with secret variables shown as `{{name}}`.
    pub resolved_url: String,
    /// AC-ENV-3: variables that were not defined.
    pub unresolved_variables: Vec<String>,
    pub response: Option<ResponseWithId>,
    pub error: Option<SendError>,
}

/// Send a request (AC-REQ-8/9). Network failures resolve with `error` set;
/// only validation problems reject (code "validation").
#[tauri::command]
pub async fn send_request(
    state: S<'_>,
    execution_id: String,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<SendResult> {
    let (scope, collection_auth) = {
        let store = state.store();
        // AC-FWD-3/4: a stored request from a newer ReqHarbor is never sent,
        // even if the webview dropped its `kind` / `body.type`.
        if store.request(&request.id).is_some_and(|r| !r.is_editable()) {
            return Err(PrepareError::NeedsNewerVersion.into());
        }
        let pid = project_id.or_else(|| {
            store
                .collection(&request.collection_id)
                .map(|c| c.project_id.clone())
        });
        let auth = store
            .collection(&request.collection_id)
            .map(|c| c.auth.clone());
        (store.var_scope(pid.as_deref()), auth)
    };
    let handle = state.inflight.register(&execution_id);
    let outcome = core_send(
        &state.engine,
        &request,
        &scope,
        collection_auth.as_ref(),
        &handle.token,
    )
    .await;
    state.inflight.finish(&execution_id, &handle);
    let outcome = outcome?;
    let history_id = outcome.history.id.clone();
    // History write failures must not lose the response.
    let _ = state.store().history.append(&outcome.history);
    let response = outcome.response.map(|r| {
        let meta = ResponseMeta::from_response(&r);
        let response_id = execution_id.clone();
        state.put_response(response_id.clone(), r.body);
        ResponseWithId { response_id, meta }
    });
    Ok(SendResult {
        execution_id,
        history_id,
        method: outcome.prepared.method,
        resolved_url: outcome.prepared.display_url,
        unresolved_variables: outcome.prepared.unresolved,
        response,
        error: outcome.error,
    })
}

// ------------------------------------------------------------------ websocket

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsConnectResult {
    pub connection_id: String,
    /// The history entry written when the session ends.
    pub history_id: String,
    /// "WS" or "SIO".
    pub method: String,
    /// URL being opened, secret variables shown as `{{name}}`.
    pub resolved_url: String,
    pub unresolved_variables: Vec<String>,
}

/// Open a WebSocket / Socket.IO connection (AC-WS-2/3). Resolves as soon as
/// the connection is being opened; progress, every frame and the final
/// close arrive on `onEvent`, in order. Validation problems reject.
#[tauri::command]
pub fn ws_connect(
    app: AppHandle,
    state: S<'_>,
    connection_id: String,
    request: Request,
    project_id: Option<String>,
    on_event: tauri::ipc::Channel<reqharbor_core::ws::WsFrame>,
) -> CmdResult<WsConnectResult> {
    use reqharbor_core::ws;
    let (scope, collection_auth, pid) = {
        let store = state.store();
        if store.request(&request.id).is_some_and(|r| !r.is_editable()) {
            return Err(PrepareError::NeedsNewerVersion.into());
        }
        let pid = project_id.or_else(|| {
            store
                .collection(&request.collection_id)
                .map(|c| c.project_id.clone())
        });
        let auth = store
            .collection(&request.collection_id)
            .map(|c| c.auth.clone());
        (store.var_scope(pid.as_deref()), auth, pid)
    };
    let prepared = ws::prepare_ws(&request, &scope, collection_auth.as_ref())?;
    let reg = state
        .ws
        .register(&connection_id, pid, prepared.socket_io.is_some());
    let history_id = ws::new_history_id();
    let started_at = now_ms();
    let result = WsConnectResult {
        connection_id: connection_id.clone(),
        history_id: history_id.clone(),
        method: prepared.method().to_string(),
        resolved_url: prepared.display_url.clone(),
        unresolved_variables: prepared.unresolved.clone(),
    };
    let serial = reg.serial;
    // A channel that can't deliver any more (webview reloaded or closed)
    // ends the session rather than leaving it running without a UI.
    let emit = crate::ws_rt::channel_emitter(move |f| on_event.send(f), reg.cancel.clone());
    tauri::async_runtime::spawn(async move {
        let stats = ws::run(prepared.clone(), reg.rx, emit, reg.cancel).await;
        let state = app.state::<AppState>();
        let redactor = reqharbor_core::redact::Redactor::from_scope(&scope);
        let entry = ws::history_entry(
            &history_id,
            started_at,
            &request,
            &prepared,
            &stats,
            &redactor,
        );
        // History write failures must not break the session teardown.
        let _ = state.store().history.append(&entry);
        state.ws.finish(&connection_id, serial);
    });
    Ok(result)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WsSendResult {
    /// Socket.IO acknowledgement id when `ack` was requested.
    pub ack_id: Option<u64>,
}

/// Send a message on an open connection (AC-WS-4). Variables are resolved
/// with the project's current environment; `{{$…}}` get fresh values.
#[tauri::command]
pub async fn ws_send(
    state: S<'_>,
    connection_id: String,
    message: SavedMessage,
) -> CmdResult<WsSendResult> {
    use crate::ws_rt::SendRefused;
    let (pid, sio) = state
        .ws
        .info(&connection_id)
        .ok_or_else(|| CommandError::new("ws_not_connected", "Not connected"))?;
    let scope = state.store().var_scope(pid.as_deref());
    let out = reqharbor_core::ws::resolve_message(&message, &scope, sio)
        .map_err(|m| CommandError::new("ws_invalid_message", m))?;
    match state.ws.send(&connection_id, out).await {
        Ok(ack_id) => Ok(WsSendResult { ack_id }),
        Err(SendRefused::NotConnected) => {
            Err(CommandError::new("ws_not_connected", "Not connected"))
        }
        Err(SendRefused::Session(m)) => Err(CommandError::new("ws_not_connected", m)),
    }
}

/// Send a WebSocket ping frame (AC-WS-4). False when not connected.
#[tauri::command]
pub fn ws_ping(state: S<'_>, connection_id: String) -> bool {
    state.ws.ping(&connection_id, Vec::new())
}

/// Close a connection (or stop connecting) with an optional code (1000 or
/// 3000–4999) and reason (AC-WS-3). False when nothing was open.
#[tauri::command]
pub fn ws_close(
    state: S<'_>,
    connection_id: String,
    code: Option<u16>,
    reason: Option<String>,
) -> CmdResult<bool> {
    if let Some(c) = code
        && c != 1000
        && !(3000..=4999).contains(&c)
    {
        return Err(CommandError::new(
            "invalid",
            format!("Close code {c} isn't allowed: use 1000 or 3000–4999"),
        ));
    }
    let reason = reason.unwrap_or_default();
    if reason.len() > 123 {
        return Err(CommandError::new(
            "invalid",
            "The close reason can be at most 123 bytes",
        ));
    }
    Ok(state.ws.close(&connection_id, code, reason))
}

/// Cancel an in-flight send (AC-REQ-8). Returns false when nothing matched.
#[tauri::command]
pub fn cancel_request(state: S<'_>, execution_id: String) -> bool {
    state.inflight.cancel(&execution_id)
}

/// Raw response body bytes (ArrayBuffer in JS). `limit` truncates (AC-RES-3).
#[tauri::command]
pub fn get_response_body(
    state: S<'_>,
    response_id: String,
    limit: Option<usize>,
) -> CmdResult<tauri::ipc::Response> {
    Ok(tauri::ipc::Response::new(
        state.response_slice(&response_id, limit)?,
    ))
}

/// Write the full response body to `path` (Save to file).
#[tauri::command]
pub fn save_response_body(state: S<'_>, response_id: String, path: String) -> CmdResult<()> {
    let body = state.response(&response_id)?;
    std::fs::write(&path, body.as_slice())?;
    Ok(())
}

// ------------------------------------------------------------------- helpers

/// AC-REQ-6: `kind` is "json" or "xml". Rejects with code "format_error" and
/// `details: { message, line, column }`.
#[tauri::command]
pub fn format_body(kind: String, text: String) -> CmdResult<String> {
    match kind.as_str() {
        "json" => Ok(format::format_json(&text)?),
        "xml" => Ok(format::format_xml(&text)?),
        other => Err(CommandError::new(
            "invalid",
            format!("Cannot format {other}"),
        )),
    }
}

/// AC-REQ-2: URL -> Query rows (keeps disabled rows of `params`).
#[tauri::command]
pub fn url_to_params(url: String, params: Vec<KeyValue>) -> Vec<KeyValue> {
    urlx::url_to_params(&url, &params)
}

/// AC-REQ-2: Query rows -> URL.
#[tauri::command]
pub fn params_to_url(url: String, params: Vec<KeyValue>) -> String {
    urlx::params_to_url(&url, &params)
}

// ----------------------------------------------------------------------- curl

#[tauri::command]
pub fn import_curl(text: String) -> CmdResult<CurlImport> {
    Ok(curl::parse_curl(&text)?)
}

#[tauri::command]
pub fn looks_like_curl(text: String) -> bool {
    curl::looks_like_curl(&text)
}

/// AC-CEXP-5: resolves variables against the project's active environment
/// unless `keepVariables` is true.
#[tauri::command]
pub fn export_curl(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    keep_variables: bool,
) -> CmdResult<String> {
    let store = state.store();
    let pid = project_id.or_else(|| {
        store
            .collection(&request.collection_id)
            .map(|c| c.project_id.clone())
    });
    let scope = store.var_scope(pid.as_deref());
    let collection_auth = store
        .collection(&request.collection_id)
        .map(|c| c.auth.clone());
    Ok(curl::export_curl(
        &request,
        &scope,
        collection_auth.as_ref(),
        ExportOptions { keep_variables },
    )?)
}

// -------------------------------------------------------------------- openapi

fn openapi_text(text: Option<String>, path: Option<String>) -> CmdResult<String> {
    match (text, path) {
        (Some(t), _) if !t.trim().is_empty() => Ok(t),
        (_, Some(p)) => Ok(std::fs::read_to_string(&p)
            .map_err(|e| CommandError::new("io", format!("Cannot read {p}: {e}")))?),
        _ => Err(CommandError::new(
            "invalid",
            "Provide OpenAPI text or a file path",
        )),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApiPreview {
    pub spec: SpecInfo,
    pub report: ImportReport,
}

/// Run CPU-heavy parsing off the main thread (QA-R1-008).
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> CmdResult<T> + Send + 'static,
) -> CmdResult<T> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| CommandError::new("internal", e.to_string()))?
}

/// Detect + dry-run an import for the dialog preview (AC-OAS-1/2/4/7/8).
#[tauri::command]
pub async fn preview_openapi(
    text: Option<String>,
    path: Option<String>,
) -> CmdResult<OpenApiPreview> {
    blocking(move || {
        let text = openapi_text(text, path)?;
        let spec = openapi::detect_spec(&text)?;
        let imported = openapi::import_openapi(&text, "preview", "preview")?;
        Ok(OpenApiPreview {
            spec,
            report: imported.report,
        })
    })
    .await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenApiImportResult {
    pub report: ImportReport,
    pub collection_id: String,
    pub environment: Environment,
    pub tree: ProjectTree,
}

/// Import into `projectId`: one collection + one environment (AC-OAS-2/4).
/// Parsing runs on a blocking thread; only the insert takes the store lock.
#[tauri::command]
pub async fn import_openapi(
    app: AppHandle,
    project_id: String,
    text: Option<String>,
    path: Option<String>,
) -> CmdResult<OpenApiImportResult> {
    let workspace_id = app.state::<AppState>().store().workspace.id.clone();
    let pid = project_id.clone();
    let imported = blocking(move || {
        let text = openapi_text(text, path)?;
        Ok(openapi::import_openapi(&text, &workspace_id, &pid)?)
    })
    .await?;
    mutate(&app, |s| {
        s.project(&project_id)?;
        let collection_id = imported.collection.id.clone();
        let tree = s.insert_collection_tree(
            &project_id,
            imported.collection,
            imported.folders,
            imported.requests,
        )?;
        let mut env = s.create_environment(Some(&project_id), &imported.environment.name)?;
        env.variables = imported.environment.variables;
        let environment = s.update_environment(&env)?;
        Ok(OpenApiImportResult {
            report: imported.report,
            collection_id,
            environment,
            tree,
        })
    })
}

// ----------------------------------------------------------------------- sync

#[tauri::command]
pub fn get_sync_status(state: S<'_>) -> SyncStatus {
    state.sync_status()
}

fn reset_status(state: &AppState) -> SyncStatus {
    let fresh = SyncStatus::from_store(&mut state.store());
    state.update_sync(|s| *s = fresh)
}

/// AC-SYNC-1 "Create workspace": returns the token (shown once) and connects.
#[tauri::command]
pub async fn sync_create_workspace(
    app: AppHandle,
    worker_url: String,
    name: Option<String>,
) -> CmdResult<CreatedWorkspace> {
    let state = app.state::<AppState>();
    let name = name.unwrap_or_else(|| state.store().workspace.name.clone());
    let created = SyncClient::new(&worker_url, None)?
        .create_workspace(&name)
        .await?;
    connect(&app, &worker_url, &created.token).await?;
    Ok(created)
}

/// AC-SYNC-1 "Connect" with an existing token (validated with the Worker).
#[tauri::command]
pub async fn sync_connect(
    app: AppHandle,
    worker_url: String,
    token: String,
) -> CmdResult<SyncStatus> {
    connect(&app, &worker_url, token.trim()).await
}

async fn connect(app: &AppHandle, worker_url: &str, token: &str) -> CmdResult<SyncStatus> {
    let client = SyncClient::new(worker_url, Some(token))?;
    let remote = client.workspace().await?;
    let state = app.state::<AppState>();
    if state.store().account_mode() {
        // Signed in (or once was): token sync becomes the legacy space. Its
        // token goes to the secret store; nothing local is bound to it
        // until projects arrive from it.
        let secrets = app.state::<AuthState>().secrets();
        let key = reqharbor_core::spaces::legacy_key(&remote.workspace_id);
        secrets
            .set(&key, token)
            .map_err(|e| CommandError::from(reqharbor_core::auth::AuthError::from(e)))?;
        {
            let mut store = state.store();
            store.bindings.legacy = Some(LegacyLink {
                worker_url: client.base_url().to_string(),
                remote_workspace_id: remote.workspace_id,
                mode: LegacyMode::Sync,
            });
            store.legacy_token = Some(token.to_string());
            store
                .spaces_state
                .spaces
                .remove(reqharbor_core::spaces::LEGACY_SPACE);
            store.save_bindings()?;
            store.save_spaces_state()?;
        }
        reset_status(&state);
        let _ = run_sync(app).await;
        return Ok(state.sync_status());
    }
    {
        let mut store = state.store();
        store.workspace.sync = Some(SyncConfig {
            worker_url: client.base_url().to_string(),
            remote_workspace_id: remote.workspace_id,
            token: token.to_string(),
            cursor: 0,
        });
        store.sync_state.reset_for_new_remote();
        store.save_workspace()?;
        store.save_sync_state()?;
    }
    reset_status(&state);
    let _ = run_sync(app).await;
    Ok(state.sync_status())
}

#[tauri::command]
pub fn sync_disconnect(app: AppHandle, state: S<'_>) -> CmdResult<SyncStatus> {
    if state.store().account_mode() {
        let secrets = app.state::<AuthState>().secrets();
        state.store().legacy_disconnect(secrets.as_ref())?;
        return Ok(reset_status(&state));
    }
    sync::disconnect(&mut state.store())?;
    state.retry.cancel();
    state.push_debounce.cancel();
    Ok(reset_status(&state))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RotatedToken {
    pub token: String,
}

/// Rotate the workspace token. Serialised with sync runs (contract C4): no
/// sync can use the old token after the Worker invalidated it, and the new
/// token is stored before the lock is released.
#[tauri::command]
pub async fn sync_rotate_token(app: AppHandle, state: S<'_>) -> CmdResult<RotatedToken> {
    let _serial = state.sync_lock.lock().await;
    let client = sync::client_for(&state.store())?;
    let token = client.rotate_token().await?;
    let legacy = state.store().bindings.legacy.clone();
    if let Some(l) = legacy {
        state.store().legacy_token = Some(token.clone());
        let secrets = app.state::<AuthState>().secrets();
        let key = reqharbor_core::spaces::legacy_key(&l.remote_workspace_id);
        if let Err(e) = secrets.set(&key, &token) {
            let mut err = CommandError::from(reqharbor_core::auth::AuthError::from(e));
            err.details = Some(serde_json::json!({ "token": token }));
            return Err(err);
        }
        return Ok(RotatedToken { token });
    }
    let mut store = state.store();
    if let Some(cfg) = store.workspace.sync.as_mut() {
        cfg.token = token.clone();
    }
    if let Err(e) = store.save_workspace() {
        // The new token is live in memory; hand it to the user regardless.
        let mut err = CommandError::from(e);
        err.details = Some(serde_json::json!({ "token": token }));
        return Err(err);
    }
    Ok(RotatedToken { token })
}

/// Remote workspace info incl. storage use (`bytesUsed` / `quotaBytes`).
#[tauri::command]
pub async fn sync_workspace_info(state: S<'_>) -> CmdResult<RemoteWorkspace> {
    let client = sync::client_for(&state.store())?;
    Ok(client.workspace().await?)
}

/// "Sync now" (AC-SYNC-2): pull then push.
#[tauri::command]
pub async fn sync_now(app: AppHandle) -> CmdResult<SyncReport> {
    run_sync(&app).await
}

// --------------------------------------------------------------------- shares

/// Shown when sharing a project that syncs nowhere (UX §12.3).
const SHARE_NEEDS_SYNC: &str =
    "Sharing needs this project to sync. Change its sync location to Personal first.";

/// Shown when sharing from a project whose space is paused or gone.
const SHARE_PAUSED: &str =
    "This project's sync is paused, so it can't be shared. Resolve it in Settings › Sync first.";

/// The client of a scope: token sync, or an account space with the session.
/// A space client exists only for an active binding of the signed-in
/// account on the session's server (06 §9): a project paused for another
/// account never sends this session's tokens to its server.
fn scope_client(app: &AppHandle, store: &Store, scope: &Scope) -> CmdResult<SyncClient> {
    match scope {
        Scope::Legacy => Ok(sync::client_for(store)?),
        Scope::Space(id) => {
            if store.binding(id).is_none() {
                return Err(CommandError::new("sync_not_connected", SHARE_NEEDS_SYNC));
            }
            let session = app.state::<AuthState>().session().ok_or_else(|| {
                CommandError::from(reqharbor_core::auth::AuthError::SignInRequired)
            })?;
            reqharbor_core::space_sync::space_client(store, id, &session)
                .map_err(|_| CommandError::new("sync_not_connected", SHARE_PAUSED))
        }
    }
}

/// The client for sharing `entity_id`: its project's space (04 §10:
/// `share_*` keep their signatures; the space comes from the binding).
fn share_client(
    app: &AppHandle,
    store: &Store,
    kind: ShareKind,
    entity_id: &str,
) -> CmdResult<SyncClient> {
    if !store.account_mode() {
        return Ok(sync::client_for(store)?);
    }
    let node = match kind {
        ShareKind::Collection => NodeKind::Collection,
        ShareKind::Request => NodeKind::Request,
    };
    let pid = store.project_of(node, entity_id)?;
    let scope = store
        .scope_of_project(&pid)
        .ok_or_else(|| CommandError::new("sync_not_connected", SHARE_NEEDS_SYNC))?;
    scope_client(app, store, &scope)
}

/// Clients of every scope that can hold shares: token sync (also in
/// shares-only mode) and each signed-in account space.
fn share_clients(app: &AppHandle, store: &Store) -> CmdResult<Vec<SyncClient>> {
    if !store.account_mode() {
        return Ok(vec![sync::client_for(store)?]);
    }
    let mut out = Vec::new();
    for b in &store.bindings.spaces {
        let scope = Scope::Space(b.space_id.clone());
        if store.scope_syncable(&scope)
            && let Ok(c) = scope_client(app, store, &scope)
        {
            out.push(c);
        }
    }
    if let Ok(c) = sync::client_for(store) {
        out.push(c);
    }
    if out.is_empty() {
        return Err(CommandError::from(sync::SyncError::NotConnected));
    }
    Ok(out)
}

fn share_snapshot(
    app: &AppHandle,
    state: &AppState,
    kind: ShareKind,
    entity_id: &str,
    include_credentials: bool,
    environment_id: Option<String>,
) -> CmdResult<(serde_json::Value, SharePreview, SyncClient)> {
    let store = state.store();
    let client = share_client(app, &store, kind, entity_id)?;
    let opts = ShareOptions {
        include_credentials,
        environment_id,
    };
    let (snap, preview) = share::build_snapshot(&store, kind, entity_id, &opts)?;
    Ok((snap, preview, client))
}

/// What will be stripped (AC-SHARE-2). Works without a sync connection.
#[tauri::command]
pub fn share_preview(
    state: S<'_>,
    kind: ShareKind,
    entity_id: String,
    include_credentials: bool,
    environment_id: Option<String>,
) -> CmdResult<SharePreview> {
    let store = state.store();
    let opts = ShareOptions {
        include_credentials,
        environment_id,
    };
    Ok(share::build_snapshot(&store, kind, &entity_id, &opts)?.1)
}

/// AC-SHARE-1: upload a sanitized snapshot, returns the share URL.
#[tauri::command]
pub async fn share_create(
    app: AppHandle,
    state: S<'_>,
    kind: ShareKind,
    entity_id: String,
    include_credentials: bool,
    environment_id: Option<String>,
) -> CmdResult<ShareInfo> {
    let (snap, _, client) = share_snapshot(
        &app,
        &state,
        kind,
        &entity_id,
        include_credentials,
        environment_id,
    )?;
    let mut info = client.create_share(kind, &entity_id, &snap).await?;
    info.space_id = Some(client.scope().id().to_string());
    Ok(info)
}

/// AC-SHARE-4 "Update share": same URL, new snapshot built with the given
/// options. The returned info always carries `kind`, `entityId` and `name`.
#[tauri::command]
pub async fn share_update(
    app: AppHandle,
    state: S<'_>,
    share_id: String,
    kind: ShareKind,
    entity_id: String,
    include_credentials: bool,
    environment_id: Option<String>,
) -> CmdResult<ShareInfo> {
    let (snap, _, client) = share_snapshot(
        &app,
        &state,
        kind,
        &entity_id,
        include_credentials,
        environment_id,
    )?;
    let mut info = client.update_share(&share_id, &snap).await?;
    info.kind.get_or_insert(kind);
    info.entity_id.get_or_insert(entity_id);
    info.space_id = Some(client.scope().id().to_string());
    Ok(info)
}

/// AC-SHARE-4 "Revoke": the URL then returns 410. In account mode the
/// share is looked for in each space (`spaceId` from `share_list` first).
#[tauri::command]
pub async fn share_revoke(
    app: AppHandle,
    state: S<'_>,
    share_id: String,
    space_id: Option<String>,
) -> CmdResult<()> {
    let mut clients = share_clients(&app, &state.store())?;
    if let Some(sp) = space_id {
        clients.sort_by_key(|c| c.scope().id() != sp);
    }
    let mut last = None;
    for c in clients {
        match c.revoke_share(&share_id).await {
            Ok(()) => return Ok(()),
            Err(e @ sync::SyncError::NotFound(_)) => last = Some(e),
            Err(e) => return Err(e.into()),
        }
    }
    Err(last.unwrap_or(sync::SyncError::NotConnected).into())
}

/// AC-SHARE-6: active shares of this workspace (account mode: of every
/// space, each tagged with its `spaceId`).
#[tauri::command]
pub async fn share_list(app: AppHandle, state: S<'_>) -> CmdResult<Vec<ShareInfo>> {
    let clients = share_clients(&app, &state.store())?;
    let mut out = Vec::new();
    let mut first_err = None;
    let mut any_ok = false;
    for c in &clients {
        match c.list_shares().await {
            Ok(list) => {
                any_ok = true;
                out.extend(list.into_iter().map(|mut s| {
                    s.space_id = Some(c.scope().id().to_string());
                    s
                }));
            }
            // A space route the server doesn't serve yet lists nothing.
            Err(sync::SyncError::NotFound(_)) if clients.len() > 1 => any_ok = true,
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    match (any_ok, first_err) {
        (false, Some(e)) => Err(e.into()),
        _ => Ok(out),
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareImportResult {
    pub tree: ProjectTree,
    /// File references removed from the untrusted share (QA-R1-027).
    pub warnings: Vec<String>,
}

/// AC-SHARE-5 "Import from link": copy into `projectId` with new ids.
/// Binary/Form file paths from the share are cleared (QA-R1-027).
#[tauri::command]
pub async fn import_share(
    app: AppHandle,
    url: String,
    project_id: String,
) -> CmdResult<ShareImportResult> {
    let public = SyncClient::fetch_public_share(&url).await?;
    mutate(&app, |s| {
        let (tree, warnings) =
            share::import_snapshot_with_warnings(s, &project_id, public.kind, &public.snapshot)?;
        Ok(ShareImportResult { tree, warnings })
    })
}

/// Warnings an import of `snapshot` would produce (file fields cleared).
fn share_file_warnings(snapshot: &serde_json::Value) -> Vec<String> {
    let mut warnings = Vec::new();
    let requests: Vec<serde_json::Value> =
        match (snapshot.get("request"), snapshot.get("collection")) {
            (Some(r), _) => vec![r.clone()],
            (None, Some(c)) => c
                .get("requests")
                .and_then(|r| r.as_array())
                .cloned()
                .unwrap_or_default(),
            _ => vec![],
        };
    for raw in requests {
        if let Ok(mut r) = serde_json::from_value::<Request>(raw) {
            share::strip_file_references(&mut r, &mut warnings);
        }
    }
    warnings
}

/// Fetch a share snapshot for the Link tab preview without importing.
#[tauri::command]
pub async fn preview_share(url: String) -> CmdResult<serde_json::Value> {
    let public = SyncClient::fetch_public_share(&url).await?;
    let warnings = share_file_warnings(&public.snapshot);
    Ok(
        serde_json::json!({ "kind": public.kind, "snapshot": public.snapshot, "warnings": warnings }),
    )
}

// ----------------------------------------------------------------- accounts

/// Normalise a typed server address and ask `GET /v1/server` (06 §9).
#[tauri::command]
pub async fn server_discover(app: AppHandle, server_url: String) -> CmdResult<Discovery> {
    auth_rt::discover(&app, &server_url).await
}

#[tauri::command]
pub fn auth_status(auth: State<'_, AuthState>) -> AuthStatus {
    auth.status()
}

/// Start the browser sign-in (04 §2.2). Resolves once the browser is open;
/// progress arrives as `auth://status` events.
#[tauri::command]
pub async fn auth_sign_in_start(
    app: AppHandle,
    server_url: String,
    method: Option<String>,
    insecure_confirmed: Option<bool>,
    invite_ticket: Option<String>,
) -> CmdResult<AuthStatus> {
    auth_rt::sign_in_start(
        &app,
        &server_url,
        method.filter(|m| !m.is_empty()),
        insecure_confirmed.unwrap_or(false),
        invite_ticket,
    )
    .await
}

/// "Open browser again" while waiting.
#[tauri::command]
pub fn auth_sign_in_reopen(app: AppHandle) -> CmdResult<()> {
    auth_rt::sign_in_reopen(&app)
}

#[tauri::command]
pub fn auth_sign_in_cancel(app: AppHandle) -> AuthStatus {
    auth_rt::sign_in_cancel(&app)
}

/// AC-AUTH-11 "Sign out on this device".
#[tauri::command]
pub async fn auth_sign_out(app: AppHandle) -> CmdResult<AuthStatus> {
    auth_rt::sign_out(&app).await
}

/// Refresh the signed-in profile (`GET /v1/me`).
#[tauri::command]
pub async fn auth_me(app: AppHandle) -> CmdResult<AuthStatus> {
    auth_rt::refresh_profile(&app).await
}

/// AC-AUTH-10 Devices.
#[tauri::command]
pub async fn auth_sessions(app: AppHandle) -> CmdResult<Vec<SessionInfo>> {
    auth_rt::sessions(&app).await
}

#[tauri::command]
pub async fn auth_session_revoke(app: AppHandle, session_id: String) -> CmdResult<()> {
    auth_rt::revoke_session(&app, &session_id).await
}

// ------------------------------------------------------------------- spaces

/// Where every project syncs (UX §5.1 grouped switcher).
#[tauri::command]
pub fn project_locations(state: S<'_>) -> Vec<ProjectLocation> {
    state.store().project_locations()
}

/// Sync location… (AC-PROJ-3): a space id, or null for this device only.
#[tauri::command]
pub fn project_set_location(
    app: AppHandle,
    project_id: String,
    space_id: Option<String>,
) -> CmdResult<Vec<ProjectLocation>> {
    let out = mutate(&app, |s| {
        s.set_project_location(&project_id, space_id.as_deref())?;
        Ok(s.project_locations())
    })?;
    crate::sync_rt::refresh_status(&app);
    Ok(out)
}

/// The first-sync checklist (UX §10): projects not bound to any space.
#[tauri::command]
pub fn first_sync_candidates(state: S<'_>) -> Vec<FirstSyncCandidate> {
    state.store().first_sync_candidates()
}

/// Run a sync first when the personal space isn't bound yet (the first
/// `/v1/me/spaces` of a sign-in).
async fn ensure_personal(app: &AppHandle) {
    let missing = app.state::<AppState>().store().personal_space().is_none();
    if missing {
        let _ = run_sync(app).await;
    }
}

/// Answer the first-sync checklist: the checked projects sync to Personal,
/// the rest stay on this device (AC-ONB-5). Nothing uploads before this.
#[tauri::command]
pub async fn first_sync_apply(app: AppHandle, project_ids: Vec<String>) -> CmdResult<SyncStatus> {
    ensure_personal(&app).await;
    {
        let state = app.state::<AppState>();
        let mut store = state.store();
        store.first_sync_apply(&project_ids)?;
    }
    crate::sync_rt::refresh_status(&app);
    let _ = run_sync(&app).await;
    Ok(app.state::<AppState>().sync_status())
}

/// SpaceNoticeDialog: Keep (local-only copies) or Remove from this device,
/// for a space paused for another account or removed (06 §9, 04 §6.7).
#[tauri::command]
pub fn space_notice_resolve(
    app: AppHandle,
    space_id: String,
    keep: bool,
    save_changes: Option<bool>,
) -> CmdResult<SyncStatus> {
    {
        let state = app.state::<AppState>();
        let mut store = state.store();
        // Edge case 3: unsynced edits are saved as a personal copy first.
        if save_changes.unwrap_or(false) {
            store.save_dirty_as_personal(&space_id)?;
        }
        store.space_notice_resolve(&space_id, keep)?;
    }
    crate::sync_rt::schedule_push(&app);
    let status = crate::sync_rt::refresh_status(&app);
    let _ = tauri::Emitter::emit(&app, crate::sync_rt::EVENT_CHANGED, &SyncReport::default());
    Ok(status)
}

/// × on SpaceNoticeDialog: decide later; Keep applies after 7 days (UX §6.9).
#[tauri::command]
pub fn space_notice_later(app: AppHandle, space_ids: Vec<String>) -> CmdResult<SyncStatus> {
    {
        let state = app.state::<AppState>();
        state.store().space_notice_later(&space_ids, now_ms())?;
    }
    Ok(crate::sync_rt::refresh_status(&app))
}

/// What `legacy_merge` did: the merge is local and done once this returns;
/// the follow-up sync is reported on its own (a failed sync is not a
/// failed merge).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyMergeResult {
    /// Projects moved from the legacy workspace to Personal.
    pub merged: usize,
    /// The follow-up sync, when it ran without error.
    pub report: Option<SyncReport>,
    /// Why the follow-up sync failed; the merge stands and syncs later.
    pub sync_error: Option<CommandError>,
}

/// Merge the token-synced workspace into Personal (AC-ONB-3), then sync.
#[tauri::command]
pub async fn legacy_merge(app: AppHandle) -> CmdResult<LegacyMergeResult> {
    ensure_personal(&app).await;
    let merged = {
        let state = app.state::<AppState>();
        let mut store = state.store();
        store.legacy_merge()?
    };
    crate::sync_rt::refresh_status(&app);
    Ok(match run_sync(&app).await {
        Ok(report) => LegacyMergeResult {
            merged,
            report: Some(report),
            sync_error: None,
        },
        Err(e) => LegacyMergeResult {
            merged,
            report: None,
            sync_error: Some(e),
        },
    })
}

/// Forget the legacy token (its share links keep working on the server).
#[tauri::command]
pub fn legacy_disconnect(app: AppHandle) -> CmdResult<SyncStatus> {
    {
        let state = app.state::<AppState>();
        let secrets = app.state::<AuthState>().secrets();
        state.store().legacy_disconnect(secrets.as_ref())?;
    }
    Ok(crate::sync_rt::refresh_status(&app))
}

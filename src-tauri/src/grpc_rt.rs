//! gRPC commands (AC-GRPC-*): descriptors (reflection cache, `.proto`
//! files), unary invokes, streams over an `ipc::Channel`, and grpcurl
//! export. Thin wrappers over `reqharbor_core::grpc`; documented in
//! docs/API_CONTRACT.md §3.16. The registry maps a client-chosen
//! `connectionId` to a running call; it is free of Tauri types so it can be
//! unit-tested.

use crate::error::{CmdResult, CommandError};
use crate::state::AppState;
use reqharbor_core::CancellationToken;
use reqharbor_core::dynamic::DynamicCtx;
use reqharbor_core::grpc::{
    self, CallStats, Channel, DescriptorCache, DescriptorSource, GrpcCommand, GrpcDescriptor,
    GrpcError, GrpcFrame, InputType as MessageDescriptor, Message, Pool as DescriptorPool,
    PreparedGrpc,
};
use reqharbor_core::model::{
    Auth, GrpcCallKind, GrpcStatus, ProtoSource, Request, SendError, new_id, now_ms,
};
use reqharbor_core::prepare::PrepareError;
use reqharbor_core::redact::Redactor;
use reqharbor_core::vars::VarScope;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use tauri::{AppHandle, Manager, State};
use tokio::sync::{mpsc, oneshot};

type S<'a> = State<'a, AppState>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

// ----------------------------------------------------------------- registry

struct Entry {
    serial: u64,
    tx: mpsc::UnboundedSender<GrpcCommand>,
    cancel: CancellationToken,
    /// Project whose variables resolve the messages sent on it.
    project_id: Option<String>,
    /// The method's input type (messages sent on the stream).
    input: MessageDescriptor,
}

/// connectionId -> live call.
#[derive(Default)]
pub struct GrpcRegistry {
    serial: AtomicU64,
    calls: Mutex<HashMap<String, Entry>>,
    /// Calls still connecting or reflecting (not registered yet): cancellable.
    pending: Mutex<HashMap<String, (u64, CancellationToken)>>,
}

/// Handle returned by [`GrpcRegistry::register`].
pub struct Registered {
    pub serial: u64,
    pub rx: mpsc::UnboundedReceiver<GrpcCommand>,
    pub cancel: CancellationToken,
}

impl GrpcRegistry {
    /// Register a call. Re-using an id cancels the previous call.
    pub fn register(
        &self,
        id: &str,
        project_id: Option<String>,
        input: MessageDescriptor,
    ) -> Registered {
        let (tx, rx) = mpsc::unbounded_channel();
        let cancel = CancellationToken::new();
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        let entry = Entry {
            serial,
            tx,
            cancel: cancel.clone(),
            project_id,
            input,
        };
        if let Some(old) = lock(&self.calls).insert(id.to_string(), entry) {
            old.cancel.cancel();
        }
        Registered { serial, rx, cancel }
    }

    /// A call that is still preparing (connecting, reflecting): `cancel` and
    /// `cancel_all` reach its token. Re-using an id cancels the previous one.
    pub fn begin(&self, id: &str) -> (u64, CancellationToken) {
        let token = CancellationToken::new();
        let serial = self.serial.fetch_add(1, Ordering::SeqCst) + 1;
        if let Some((_, old)) = lock(&self.pending).insert(id.to_string(), (serial, token.clone()))
        {
            old.cancel();
        }
        (serial, token)
    }

    /// The call stopped preparing (it registered, or failed).
    pub fn settle(&self, id: &str, serial: u64) {
        let mut map = lock(&self.pending);
        if map.get(id).is_some_and(|(s, _)| *s == serial) {
            map.remove(id);
        }
    }

    /// (project id, input type) of a live call.
    pub fn info(&self, id: &str) -> Option<(Option<String>, MessageDescriptor)> {
        lock(&self.calls)
            .get(id)
            .map(|e| (e.project_id.clone(), e.input.clone()))
    }

    /// Queue a message; resolves once the call took it (or refused it).
    pub async fn send(&self, id: &str, msg: Message, text: String) -> Result<(), String> {
        let (reply, rx) = oneshot::channel();
        {
            let map = lock(&self.calls);
            let e = map.get(id).ok_or_else(|| "Not connected".to_string())?;
            e.tx.send(GrpcCommand::Send(msg, text, reply))
                .map_err(|_| "Not connected".to_string())?;
        }
        rx.await.unwrap_or_else(|_| Err("Not connected".into()))
    }

    /// Half-close. False when nothing is open under `id`.
    pub fn end(&self, id: &str) -> bool {
        lock(&self.calls)
            .get(id)
            .is_some_and(|e| e.tx.send(GrpcCommand::End).is_ok())
    }

    /// Cancel. False when nothing is open under `id`.
    pub fn cancel(&self, id: &str) -> bool {
        let preparing = lock(&self.pending).get(id).is_some_and(|(_, t)| {
            t.cancel();
            true
        });
        let live = lock(&self.calls).get(id).is_some_and(|e| {
            e.cancel.cancel();
            true
        });
        preparing || live
    }

    /// Forget a finished call (unless a newer one re-used the id).
    pub fn finish(&self, id: &str, serial: u64) {
        let mut map = lock(&self.calls);
        if map.get(id).is_some_and(|e| e.serial == serial) {
            map.remove(id);
        }
    }

    /// App exit or webview reload: every call is cancelled.
    pub fn cancel_all(&self) -> usize {
        let pending = lock(&self.pending);
        for (_, t) in pending.values() {
            t.cancel();
        }
        let map = lock(&self.calls);
        for e in map.values() {
            e.cancel.cancel();
        }
        map.len() + pending.len()
    }

    pub fn len(&self) -> usize {
        lock(&self.calls).len()
    }
}

// ------------------------------------------------------------------ context

struct Ctx {
    scope: VarScope,
    auth: Option<Auth>,
    project_id: Option<String>,
    cache: DescriptorCache,
}

/// Variables, collection auth and the descriptor cache for `request`, like
/// `send_request` resolves them. A stored request from a newer ReqHarbor is
/// refused (AC-FWD-3/4).
fn context(state: &AppState, request: &Request, project_id: Option<String>) -> CmdResult<Ctx> {
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
    Ok(Ctx {
        scope: store.var_scope(pid.as_deref()),
        auth,
        project_id: pid,
        cache: DescriptorCache::new(store.dir()),
    })
}

/// A command error whose texts never show a resolved secret.
fn redact_error(mut e: CommandError, redactor: &Redactor) -> CommandError {
    e.message = redactor.redact(&e.message);
    if let Some(d) = e.details.as_mut() {
        redactor.redact_json(d);
    }
    e
}

/// Compile `.proto` files off the async runtime.
async fn compile(p: &PreparedGrpc) -> Result<DescriptorPool, GrpcError> {
    let files = p.proto_files.clone();
    let imports = p.import_paths.clone();
    tokio::task::spawn_blocking(move || grpc::compile_files(&files, &imports))
        .await
        .map_err(|e| GrpcError::Io(e.to_string()))?
}

fn files_descriptor(p: &PreparedGrpc, url: &str, pool: &DescriptorPool) -> GrpcDescriptor {
    grpc::describe(
        pool,
        url,
        DescriptorSource::Files,
        p.proto_files.clone(),
        now_ms(),
    )
}

// ------------------------------------------------------------- descriptors

/// The descriptor the UI already shows.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownDescriptor {
    pub url: String,
    pub fetched_at: i64,
}

/// `grpc_cached_descriptor` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DescriptorLookup {
    pub url: String,
    pub unchanged: bool,
    pub descriptor: Option<GrpcDescriptor>,
}

pub async fn cached_descriptor_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    known: Option<KnownDescriptor>,
) -> CmdResult<DescriptorLookup> {
    let cx = context(state, &request, project_id)?;
    let url = grpc::descriptor_url(&request, &cx.scope, cx.auth.as_ref())?;
    let p = grpc::prepare_grpc(
        &request,
        &cx.scope,
        cx.auth.as_ref(),
        false,
        &DynamicCtx::new(),
    )?;
    if p.proto_source == ProtoSource::Files {
        if p.proto_files.is_empty() {
            return Ok(DescriptorLookup {
                url,
                unchanged: false,
                descriptor: None,
            });
        }
        let pool = compile(&p).await?;
        return Ok(DescriptorLookup {
            descriptor: Some(files_descriptor(&p, &url, &pool)),
            url,
            unchanged: false,
        });
    }
    let cached = cx.cache.get(&url);
    let unchanged = matches!((&cached, &known), (Some(c), Some(k)) if c.url == k.url && c.fetched_at == k.fetched_at);
    let descriptor = match cached {
        Some(c) if !unchanged => Some(grpc::describe(
            &c.pool,
            &url,
            DescriptorSource::Reflection,
            Vec::new(),
            c.fetched_at,
        )),
        _ => None,
    };
    Ok(DescriptorLookup {
        url,
        unchanged,
        descriptor,
    })
}

pub async fn fetch_descriptor_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    execution_id: Option<String>,
) -> CmdResult<GrpcDescriptor> {
    let cx = context(state, &request, project_id)?;
    let url = grpc::descriptor_url(&request, &cx.scope, cx.auth.as_ref())?;
    let p = grpc::prepare_grpc(
        &request,
        &cx.scope,
        cx.auth.as_ref(),
        false,
        &DynamicCtx::new(),
    )?;
    if p.proto_source == ProtoSource::Files {
        let pool = compile(&p).await?;
        return Ok(files_descriptor(&p, &url, &pool));
    }
    let id = execution_id.unwrap_or_else(new_id);
    let handle = state.inflight.register(&id);
    let result = grpc::fetch_reflection(&p, &handle.token).await;
    state.inflight.finish(&id, &handle);
    let pool = result
        .map_err(|e| redact_error(CommandError::from(e), &Redactor::from_scope(&cx.scope)))?;
    let fetched_at = now_ms();
    cx.cache.put(&url, fetched_at, &pool)?;
    Ok(grpc::describe(
        &pool,
        &url,
        DescriptorSource::Reflection,
        Vec::new(),
        fetched_at,
    ))
}

pub fn clear_descriptor_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<bool> {
    let cx = context(state, &request, project_id)?;
    let url = grpc::descriptor_url(&request, &cx.scope, cx.auth.as_ref())?;
    Ok(cx.cache.remove(&url)?)
}

/// The pool a call uses: compiled files, the cached reflection result, or
/// (cache miss, or the method missing from it) a fresh reflection on a new
/// connection, which the call then reuses.
async fn call_pool(
    p: &PreparedGrpc,
    url: &str,
    cache: &DescriptorCache,
    cancel: &CancellationToken,
) -> Result<(DescriptorPool, Option<Channel>), GrpcError> {
    if p.proto_source == ProtoSource::Files {
        return Ok((compile(p).await?, None));
    }
    if let Some(c) = cache.get(url)
        && grpc::find_method(&c.pool, &p.service, &p.method).is_some()
    {
        return Ok((c.pool, None));
    }
    // One time limit for connecting and reflecting (0 = none).
    let work = async {
        let channel = grpc::connect(&p.target, p.verify_tls, 0)
            .await
            .map_err(GrpcError::Network)?;
        let pool = grpc::reflect(&channel, &p.metadata).await?;
        Ok::<_, GrpcError>((pool, channel))
    };
    let limited = async {
        if p.timeout_ms == 0 {
            return work.await;
        }
        match tokio::time::timeout(std::time::Duration::from_millis(p.timeout_ms), work).await {
            Ok(r) => r,
            Err(_) => Err(GrpcError::Network(SendError {
                kind: reqharbor_core::model::ErrorKind::Timeout,
                message: format!("Timed out after {}", grpc::human_ms(p.timeout_ms)),
            })),
        }
    };
    let (pool, channel) = tokio::select! {
        r = limited => r?,
        _ = cancel.cancelled() => {
            return Err(GrpcError::Network(SendError {
                kind: reqharbor_core::model::ErrorKind::Cancelled,
                message: "Cancelled".into(),
            }));
        }
    };
    // Only a descriptor that built is cached (`reflect` checked it).
    let _ = cache.put(url, now_ms(), &pool);
    Ok((pool, Some(channel)))
}

// ------------------------------------------------------------------- invoke

/// `grpc_invoke` result (AC-GRPC-7).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InvokeResult {
    pub execution_id: String,
    pub history_id: String,
    pub call_kind: GrpcCallKind,
    pub resolved_url: String,
    pub unresolved_variables: Vec<String>,
    pub status: Option<GrpcStatus>,
    pub error: Option<SendError>,
    pub response: Option<String>,
    pub headers: Vec<(String, String)>,
    pub trailers: Vec<(String, String)>,
    pub time_ms: u64,
    pub size_bytes: u64,
}

#[allow(clippy::too_many_arguments)]
fn write_history(
    state: &AppState,
    id: &str,
    started_at: i64,
    request: &Request,
    p: &PreparedGrpc,
    kind: GrpcCallKind,
    stats: &CallStats,
    scope: &VarScope,
) {
    let redactor = reqharbor_core::redact::Redactor::from_scope(scope);
    let entry = grpc::history_entry(id, started_at, request, p, kind, stats, &redactor);
    // History write failures must not fail the call.
    let _ = state.store().history.append(&entry);
}

pub async fn invoke_in(
    state: &AppState,
    execution_id: String,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<InvokeResult> {
    let cx = context(state, &request, project_id)?;
    let p = grpc::prepare_grpc(
        &request,
        &cx.scope,
        cx.auth.as_ref(),
        true,
        &DynamicCtx::new(),
    )?;
    let url = grpc::descriptor_url(&request, &cx.scope, cx.auth.as_ref())?;
    let redactor = Redactor::from_scope(&cx.scope);
    let handle = state.inflight.register(&execution_id);
    let started_at = now_ms();
    let started = std::time::Instant::now();
    let history_id = grpc::new_history_id();
    let outcome = async {
        let (pool, channel) = match call_pool(&p, &url, &cx.cache, &handle.token).await {
            Ok(x) => x,
            Err(GrpcError::Network(e)) => {
                // A connect failure is a result (and a history entry), like a send.
                return Ok((
                    GrpcCallKind::Unary,
                    CallStats {
                        error: Some(e),
                        duration_ms: started.elapsed().as_millis() as u64,
                        ..Default::default()
                    },
                ));
            }
            Err(e) => return Err(CommandError::from(e)),
        };
        let plan = grpc::plan(p.clone(), &pool)?;
        let kind = plan.call_kind();
        // A streaming method never runs as a one-shot invoke: a client or
        // bidi call would wait for messages that can't come.
        if kind != GrpcCallKind::Unary {
            return Err(CommandError::from(GrpcError::StreamingMethod(kind)));
        }
        Ok((
            kind,
            grpc::invoke(plan, channel, handle.token.clone()).await,
        ))
    }
    .await;
    state.inflight.finish(&execution_id, &handle);
    let (kind, stats) = outcome.map_err(|e| redact_error(e, &redactor))?;
    write_history(
        state,
        &history_id,
        started_at,
        &request,
        &p,
        kind,
        &stats,
        &cx.scope,
    );
    Ok(InvokeResult {
        execution_id,
        history_id,
        call_kind: kind,
        resolved_url: redactor.redact(&p.display_url),
        unresolved_variables: p.unresolved.clone(),
        error: if stats.status.is_some() {
            None
        } else {
            stats
                .error
                .clone()
                .map(|e| grpc::redact_send_error(&redactor, e))
        },
        status: stats
            .status
            .clone()
            .map(|s| grpc::redact_status(&redactor, s)),
        response: stats.first_response.as_ref().map(|(j, _)| j.clone()),
        size_bytes: stats.first_response.as_ref().map_or(0, |(_, s)| *s),
        headers: stats.headers.clone(),
        trailers: stats.trailers.clone(),
        time_ms: stats.duration_ms,
    })
}

// ------------------------------------------------------------------ streams

/// `grpc_start` result (AC-GRPC-8).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub connection_id: String,
    pub history_id: String,
    pub call_kind: GrpcCallKind,
    pub resolved_url: String,
    pub unresolved_variables: Vec<String>,
}

/// Everything a started call needs; the caller spawns [`run_started`].
pub struct Started {
    pub result: StartResult,
    plan: grpc::CallPlan,
    channel: Option<Channel>,
    reg: Registered,
    request: Request,
    scope: VarScope,
    connection_id: String,
    started_at: i64,
}

pub async fn start_in(
    state: &AppState,
    connection_id: String,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<Started> {
    let cx = context(state, &request, project_id)?;
    let p = grpc::prepare_grpc(
        &request,
        &cx.scope,
        cx.auth.as_ref(),
        true,
        &DynamicCtx::new(),
    )?;
    let url = grpc::descriptor_url(&request, &cx.scope, cx.auth.as_ref())?;
    let redactor = Redactor::from_scope(&cx.scope);
    // Registered while it connects and reflects, so Cancel reaches it.
    let (serial, token) = state.grpc.begin(&connection_id);
    let pooled = call_pool(&p, &url, &cx.cache, &token).await;
    state.grpc.settle(&connection_id, serial);
    let (pool, channel) = pooled
        .map_err(CommandError::from)
        .map_err(|e| redact_error(e, &redactor))?;
    if token.is_cancelled() {
        return Err(redact_error(
            CommandError::from(GrpcError::Network(SendError {
                kind: reqharbor_core::model::ErrorKind::Cancelled,
                message: "Cancelled".into(),
            })),
            &redactor,
        ));
    }
    let plan = grpc::plan(p.clone(), &pool)?;
    let reg = state
        .grpc
        .register(&connection_id, cx.project_id.clone(), plan.method.input());
    let result = StartResult {
        connection_id: connection_id.clone(),
        history_id: grpc::new_history_id(),
        call_kind: plan.call_kind(),
        resolved_url: redactor.redact(&p.display_url),
        unresolved_variables: p.unresolved.clone(),
    };
    Ok(Started {
        result,
        plan,
        channel,
        reg,
        request,
        scope: cx.scope,
        connection_id,
        started_at: now_ms(),
    })
}

/// Run a started call to its end, then write its history entry.
pub async fn run_started(state: &AppState, s: Started, emit: impl FnMut(GrpcFrame)) {
    let kind = s.plan.call_kind();
    let p = s.plan.prepared.clone();
    let redactor = Redactor::from_scope(&s.scope);
    let mut emit = emit;
    let emit = move |f: GrpcFrame| emit(grpc::redact_frame(&redactor, f));
    let stats = grpc::run_call(s.plan, s.channel, s.reg.rx, emit, s.reg.cancel, None).await;
    write_history(
        state,
        &s.result.history_id,
        s.started_at,
        &s.request,
        &p,
        kind,
        &stats,
        &s.scope,
    );
    state.grpc.finish(&s.connection_id, s.reg.serial);
}

pub async fn send_in(state: &AppState, connection_id: &str, message: String) -> CmdResult<()> {
    let (pid, input) = state
        .grpc
        .info(connection_id)
        .ok_or_else(|| CommandError::new("grpc_not_connected", "Not connected"))?;
    let scope = state.store().var_scope(pid.as_deref());
    let (msg, text) = grpc::resolve_stream_message(&message, &scope, &input)
        .map_err(|m| CommandError::new("grpc_invalid_message", m))?;
    state
        .grpc
        .send(connection_id, msg, text)
        .await
        .map_err(|m| CommandError::new("grpc_not_connected", m))
}

// ----------------------------------------------------------------- commands

#[tauri::command]
pub async fn grpc_cached_descriptor(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    known: Option<KnownDescriptor>,
) -> CmdResult<DescriptorLookup> {
    cached_descriptor_in(&state, request, project_id, known).await
}

#[tauri::command]
pub async fn grpc_fetch_descriptor(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    execution_id: Option<String>,
) -> CmdResult<GrpcDescriptor> {
    fetch_descriptor_in(&state, request, project_id, execution_id).await
}

#[tauri::command]
pub fn grpc_clear_descriptor(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<bool> {
    clear_descriptor_in(&state, request, project_id)
}

/// One unary call (AC-GRPC-7); `cancel_request(executionId)` cancels it.
#[tauri::command]
pub async fn grpc_invoke(
    state: S<'_>,
    execution_id: String,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<InvokeResult> {
    invoke_in(&state, execution_id, request, project_id).await
}

/// Start a call (AC-GRPC-8); frames arrive on `onEvent`, in order.
#[tauri::command]
pub async fn grpc_start(
    app: AppHandle,
    state: S<'_>,
    connection_id: String,
    request: Request,
    project_id: Option<String>,
    on_event: tauri::ipc::Channel<GrpcFrame>,
) -> CmdResult<StartResult> {
    let started = start_in(&state, connection_id, request, project_id).await?;
    let result = started.result.clone();
    // A channel that can't deliver any more (webview reloaded or closed)
    // cancels the call rather than leaving it running without a UI.
    let emit = crate::ws_rt::channel_emitter(move |f| on_event.send(f), started.reg.cancel.clone());
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        run_started(&state, started, emit).await;
    });
    Ok(result)
}

#[tauri::command]
pub async fn grpc_send(state: S<'_>, connection_id: String, message: String) -> CmdResult<()> {
    send_in(&state, &connection_id, message).await
}

#[tauri::command]
pub fn grpc_end(state: S<'_>, connection_id: String) -> bool {
    state.grpc.end(&connection_id)
}

#[tauri::command]
pub fn grpc_cancel(state: S<'_>, connection_id: String) -> bool {
    state.grpc.cancel(&connection_id)
}

/// AC-GRPC-11.
#[tauri::command]
pub fn export_grpcurl(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    keep_variables: bool,
) -> CmdResult<String> {
    let cx = context(&state, &request, project_id)?;
    Ok(grpc::export_grpcurl(
        &request,
        &cx.scope,
        cx.auth.as_ref(),
        keep_variables,
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqharbor_core::model::{GrpcConfig, RequestKind};
    use reqharbor_core::storage::Store;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, AppState::new(store).unwrap())
    }

    const PROTO: &str = "syntax = \"proto3\";\npackage demo;\nservice Greeter { rpc SayHello(Req) returns (Req); rpc Chat(stream Req) returns (stream Req); rpc Replies(Req) returns (stream Req); }\nmessage Req { string name = 1; }\n";

    fn files_request(dir: &std::path::Path) -> Request {
        let path = dir.join("greeter.proto");
        std::fs::write(&path, PROTO).unwrap();
        Request {
            kind: RequestKind::Grpc,
            url: "grpc://localhost:1".into(),
            grpc: Some(GrpcConfig {
                service: "demo.Greeter".into(),
                method: "Chat".into(),
                proto_source: ProtoSource::Files,
                proto_files: vec![path.to_string_lossy().into_owned()],
                ..GrpcConfig::default()
            }),
            ..Request::default()
        }
    }

    #[tokio::test]
    async fn ac_grpc_4_files_descriptor_without_network() {
        let (dir, st) = state();
        let r = files_request(dir.path());
        let l = cached_descriptor_in(&st, r.clone(), None, None)
            .await
            .unwrap();
        assert_eq!(l.url, "grpc://localhost:1");
        let d = l.descriptor.unwrap();
        assert_eq!(d.services[0].name, "demo.Greeter");
        // Reflection with nothing cached: no descriptor, nothing sent.
        let mut refl = r.clone();
        refl.grpc.as_mut().unwrap().proto_source = ProtoSource::Reflection;
        let l = cached_descriptor_in(&st, refl.clone(), None, None)
            .await
            .unwrap();
        assert!(l.descriptor.is_none() && !l.unchanged);
        assert!(!clear_descriptor_in(&st, refl, None).unwrap());
        // A broken file is grpc_proto_invalid.
        std::fs::write(dir.path().join("greeter.proto"), "syntax = nope").unwrap();
        let e = cached_descriptor_in(&st, r, None, None).await.unwrap_err();
        assert_eq!(e.code, "grpc_proto_invalid");
    }

    #[tokio::test]
    async fn ac_grpc_8_start_registers_and_routes_commands() {
        let (dir, st) = state();
        let r = files_request(dir.path());
        let started = start_in(&st, "c1".into(), r, None).await.unwrap();
        assert_eq!(started.result.call_kind, GrpcCallKind::BidiStreaming);
        assert_eq!(
            started.result.resolved_url,
            "grpc://localhost:1/demo.Greeter/Chat"
        );
        assert!(st.grpc.info("c1").is_some());
        // An invalid message is refused before reaching the call.
        let e = send_in(&st, "c1", "{\"nmae\":1}".into()).await.unwrap_err();
        assert_eq!(e.code, "grpc_invalid_message");
        let e = send_in(&st, "nope", "{}".into()).await.unwrap_err();
        assert_eq!(e.code, "grpc_not_connected");
        assert!(st.grpc.end("c1"));
        assert!(!st.grpc.end("nope"));
        assert_eq!(st.grpc.cancel_all(), 1);
        // Nothing listens on port 1: the run ends with a connect error.
        let frames = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let f2 = frames.clone();
        run_started(&st, started, move |f| f2.lock().unwrap().push(f)).await;
        assert!(frames.lock().unwrap().last().unwrap().last);
        assert_eq!(st.grpc.len(), 0);
        assert_eq!(st.store().history.len(), 1);
    }

    #[tokio::test]
    async fn ac_grpc_2_invoke_validation_rejects_before_sending() {
        let (dir, st) = state();
        let mut r = files_request(dir.path());
        r.grpc.as_mut().unwrap().method = "SayHello".into();
        r.grpc.as_mut().unwrap().message = "{oops".into();
        let e = invoke_in(&st, "x".into(), r.clone(), None)
            .await
            .unwrap_err();
        assert_eq!(e.code, "validation");
        assert_eq!(e.details.unwrap()["kind"], "invalidGrpcMessage");
        r.grpc.as_mut().unwrap().method = "Missing".into();
        let e = invoke_in(&st, "x".into(), r, None).await.unwrap_err();
        assert_eq!(e.details.unwrap()["kind"], "unknownGrpcMethod");
        assert_eq!(st.store().history.len(), 0);
    }

    /// A server that accepts connections and then says nothing.
    async fn stalled_server() -> u16 {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((s, _)) = l.accept().await {
                held.push(s);
            }
        });
        port
    }

    fn reflection_request(port: u16, timeout_ms: u64) -> Request {
        let mut r = Request {
            kind: RequestKind::Grpc,
            url: format!("grpc://127.0.0.1:{port}"),
            grpc: Some(GrpcConfig {
                service: "demo.Greeter".into(),
                method: "SayHello".into(),
                ..GrpcConfig::default()
            }),
            ..Request::default()
        };
        r.settings.timeout_ms = timeout_ms;
        r
    }

    #[tokio::test]
    async fn ac_grpc_7_invoke_rejects_streaming_methods_without_waiting() {
        let (dir, st) = state();
        for (method, kind) in [("Chat", "bidiStreaming"), ("Replies", "serverStreaming")] {
            let mut r = files_request(dir.path());
            r.grpc.as_mut().unwrap().method = method.into();
            // timeoutMs 0: a call that was sent would wait forever.
            r.settings.timeout_ms = 0;
            let e = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                invoke_in(&st, "x".into(), r, None),
            )
            .await
            .expect("invoke must not hang")
            .unwrap_err();
            assert_eq!(e.code, "validation");
            let d = e.details.unwrap();
            assert_eq!(d["kind"], "grpcStreamingMethod");
            assert_eq!(d["callKind"], kind);
        }
        assert_eq!(st.store().history.len(), 0);
    }

    #[tokio::test]
    async fn ac_grpc_3_stalled_reflection_obeys_the_timeout_for_invoke() {
        let (_dir, st) = state();
        let port = stalled_server().await;
        let t0 = std::time::Instant::now();
        let r = invoke_in(&st, "x".into(), reflection_request(port, 300), None)
            .await
            .unwrap();
        assert!(t0.elapsed() < std::time::Duration::from_secs(5));
        let e = r.error.expect("a timeout result");
        assert_eq!(e.kind, reqharbor_core::model::ErrorKind::Timeout);
        assert!(r.status.is_none());
    }

    #[tokio::test]
    async fn ac_grpc_8_cancel_reaches_a_start_that_is_still_reflecting() {
        let (_dir, st) = state();
        let port = stalled_server().await;
        for all in [false, true] {
            let (res, cancelled) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                tokio::join!(
                    start_in(&st, "c1".into(), reflection_request(port, 0), None),
                    async {
                        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                        if all {
                            st.grpc.cancel_all() == 1
                        } else {
                            st.grpc.cancel("c1")
                        }
                    }
                )
            })
            .await
            .expect("cancel must stop the connecting call");
            assert!(cancelled);
            let e = res.err().expect("a cancelled start");
            assert_eq!(e.code, "grpc_network");
            assert_eq!(e.details.unwrap()["kind"], "cancelled");
        }
        assert_eq!(st.grpc.len(), 0);
        assert!(!st.grpc.cancel("c1"), "nothing is left pending");
    }

    #[tokio::test]
    async fn invalid_url_and_network_errors_never_show_secret_values() {
        let (_dir, st) = state();
        let pid = {
            let mut s = st.store();
            let pid = s.create_project("P").unwrap().project.id;
            let mut env = s.create_environment(Some(&pid), "Dev").unwrap();
            env.variables = vec![
                reqharbor_core::model::Variable::new("host", "secret-host-x1.invalid").secret(),
                reqharbor_core::model::Variable::new("bad", "s3cret-scheme").secret(),
            ];
            s.update_environment(&env).unwrap();
            s.set_active_environment(&pid, Some(&env.id)).unwrap();
            pid
        };
        // A URL that can't be parsed.
        let mut r = reflection_request(1, 2000);
        r.url = "{{bad}}://x".into();
        let e = invoke_in(&st, "x".into(), r.clone(), Some(pid.clone()))
            .await
            .unwrap_err();
        assert!(!e.message.contains("s3cret-scheme"), "{}", e.message);
        let e = start_in(&st, "c".into(), r, Some(pid.clone()))
            .await
            .err()
            .unwrap();
        assert!(!e.message.contains("s3cret-scheme"), "{}", e.message);
        // A host that doesn't resolve: the result and the error frames mask it.
        let mut r = reflection_request(1, 2000);
        r.url = "grpc://{{host}}:1".into();
        let res = invoke_in(&st, "x".into(), r.clone(), Some(pid.clone()))
            .await
            .unwrap();
        let msg = res.error.expect("a network error").message;
        assert!(!msg.contains("secret-host-x1"), "{msg}");
        assert!(msg.contains("{{host}}"), "{msg}");
        let e = start_in(&st, "c2".into(), r, Some(pid))
            .await
            .err()
            .unwrap();
        assert!(!e.message.contains("secret-host-x1"), "{}", e.message);
    }
}

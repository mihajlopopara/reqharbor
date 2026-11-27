//! Sync runtime: debounced push after saves, pull on startup / focus,
//! retry with backoff, status events (AC-SYNC-2/6/7). Decisions live in the
//! pure, unit-tested `sync_policy` module; this file only applies them.
//!
//! No timers run while idle: tasks are only spawned in response to a save,
//! a focus event, a failure (one retry timer) or an explicit "Sync now".

use crate::error::{CmdResult, CommandError};
use crate::state::{AppState, SyncStatus};
use crate::sync_policy::{self, FailureAction};
use reqharbor_core::model::now_ms;
use reqharbor_core::sync::{self, SyncError, SyncReport};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const EVENT_STATUS: &str = "sync://status";
pub const EVENT_CHANGED: &str = "sync://changed";
/// Debounce between a local save and the push (AC-SYNC-2).
pub const PUSH_DEBOUNCE: Duration = Duration::from_secs(2);

fn emit_status(app: &AppHandle, status: &SyncStatus) {
    let _ = app.emit(EVENT_STATUS, status);
}

fn current_token(state: &AppState) -> Option<String> {
    state
        .store()
        .workspace
        .sync
        .as_ref()
        .map(|s| s.token.clone())
}

/// Discovery (when due), pull with restore detection, then push the types
/// the server accepts in size-bounded batches (contracts C1/C2,
/// 06-go-server §9). Holds the store lock only for the local steps.
async fn sync_once(state: &AppState, client: &sync::SyncClient) -> Result<SyncReport, SyncError> {
    let (due, start) = {
        let store = state.store();
        (
            sync::server_info_due(&store, now_ms()),
            sync::pull_start(&store)?,
        )
    };
    let info = sync::refresh_server_info(client, due).await;
    if let Some(info) = &info {
        sync::record_server_info(&mut state.store(), info.as_ref());
    }
    let pulled = sync::pull_from(client, &start, info.as_ref().and_then(Option::as_ref)).await?;
    let report = {
        let mut store = state.store();
        let r = sync::apply_pulled(&mut store, pulled);
        store.save_all()?;
        r
    };
    let batches = sync::plan_push(&mut state.store());
    let mut pushed = 0;
    for batch in batches {
        for (sent, results) in sync::push_batch(client, batch).await? {
            let mut store = state.store();
            pushed += sync::apply_push_results(&mut store, &sent, &results);
        }
    }
    let mut store = state.store();
    store.save_sync_state()?;
    Ok(SyncReport {
        pushed,
        pulled: report.applied,
        deleted: report.deleted,
        conflicts: report.conflicts,
        skipped: report.skipped,
        too_large: sync::skipped_entities(&mut store),
        unsupported: sync::unsupported_entities(&store),
        restored: report.restored,
        cursor: store.workspace.sync.as_ref().map(|s| s.cursor).unwrap_or(0),
    })
}

/// Run one full sync. Serialised with other runs and with token rotation.
pub async fn run_sync(app: &AppHandle) -> CmdResult<SyncReport> {
    let state = app.state::<AppState>();
    let _serial = state.sync_lock.lock().await;
    let (client, token) = {
        let store = state.store();
        let client = sync::client_for(&store)?;
        let token = store.workspace.sync.as_ref().map(|s| s.token.clone());
        (client, token)
    };
    emit_status(app, &state.update_sync(sync_policy::begin));
    // Any pending retry is superseded by this run.
    state.retry.cancel();

    match sync_once(&state, &client).await {
        Ok(report) => {
            let (pending, skipped) = {
                let mut store = state.store();
                (
                    sync::pending_count(&store),
                    sync::skipped_entities(&mut store),
                )
            };
            let unsupported = report.unsupported.clone();
            let status = state.update_sync(|s| {
                sync_policy::on_success(s, now_ms(), pending, skipped, unsupported)
            });
            emit_status(app, &status);
            if report.pulled > 0
                || report.deleted > 0
                || !report.conflicts.is_empty()
                || report.restored
            {
                let _ = app.emit(EVENT_CHANGED, &report);
            }
            Ok(report)
        }
        Err(e) => {
            let rotated = current_token(&state) != token;
            let mut action = FailureAction::Stop;
            let status =
                state.update_sync(|s| action = sync_policy::on_failure(s, &e, now_ms(), rotated));
            match action {
                FailureAction::Disconnect => {
                    // AC-SYNC-7: forget the token, keep local data.
                    let mut store = state.store();
                    let _ = sync::disconnect(&mut store);
                }
                FailureAction::Retry(delay) => schedule_retry(app, delay),
                FailureAction::Stop => {}
            }
            emit_status(app, &status);
            Err(CommandError::from(e))
        }
    }
}

fn schedule_retry(app: &AppHandle, delay: Duration) {
    let state = app.state::<AppState>();
    let generation = state.retry.arm();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(delay).await;
        let state = app.state::<AppState>();
        if state.retry.is_current(generation) && state.sync_status().connected {
            let _ = run_sync(&app).await;
        }
    });
}

/// Called after every local mutation: push within 2 s (debounced).
pub fn schedule_push(app: &AppHandle) {
    let state = app.state::<AppState>();
    let pending = {
        let store = state.store();
        if store.workspace.sync.is_none() {
            return;
        }
        sync::pending_count(&store)
    };
    let status = state.update_sync(|s| s.pending_changes = pending);
    emit_status(app, &status);
    let generation = state.push_debounce.arm();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(PUSH_DEBOUNCE).await;
        let state = app.state::<AppState>();
        if state.push_debounce.is_current(generation) {
            let _ = run_sync(&app).await;
        }
    });
}

/// Pull on window focus, at most once per minute (AC-SYNC-2).
pub fn pull_if_stale(app: &AppHandle) {
    let state = app.state::<AppState>();
    if !sync_policy::should_pull_on_focus(&state.sync_status(), now_ms()) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = run_sync(&app).await;
    });
}

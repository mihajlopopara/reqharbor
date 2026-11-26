//! Shared application state managed by Tauri.

use crate::error::{CmdResult, CommandError};
use crate::registry::{CancelRegistry, Debouncer, ResponseCache};
pub use crate::sync_policy::SyncStatus;
use reqharbor_core::http::HttpEngine;
use reqharbor_core::storage::Store;
use std::sync::{Arc, Mutex, MutexGuard};

/// How many response bodies are kept for `get_response_body`.
pub const RESPONSE_CACHE: usize = 8;

pub struct AppState {
    store: Mutex<Store>,
    pub engine: HttpEngine,
    /// executionId -> cancel token for in-flight sends.
    pub inflight: CancelRegistry,
    responses: ResponseCache,
    pub sync: Mutex<SyncStatus>,
    /// Debounce for the 2 s push-after-save (AC-SYNC-2).
    pub push_debounce: Debouncer,
    /// Scheduled retries (AC-SYNC-6); re-armed or cancelled by each run.
    pub retry: Debouncer,
    /// Serialises sync runs and token rotation (contract C4).
    pub sync_lock: tokio::sync::Mutex<()>,
}

impl AppState {
    pub fn new(mut store: Store) -> CmdResult<Self> {
        let engine = HttpEngine::new().map_err(|e| CommandError::new("internal", e.message))?;
        let sync = SyncStatus::from_store(&mut store);
        Ok(AppState {
            store: Mutex::new(store),
            engine,
            inflight: CancelRegistry::default(),
            responses: ResponseCache::new(RESPONSE_CACHE),
            sync: Mutex::new(sync),
            push_debounce: Debouncer::default(),
            retry: Debouncer::default(),
            sync_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Lock the store. Never hold the guard across an `.await`.
    pub fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn put_response(&self, id: String, body: Vec<u8>) {
        self.responses.put(id, body);
    }

    pub fn response(&self, id: &str) -> CmdResult<Arc<Vec<u8>>> {
        self.responses.get(id).ok_or_else(|| {
            CommandError::new("not_found", format!("Response {id} is no longer available"))
        })
    }

    pub fn response_slice(&self, id: &str, limit: Option<usize>) -> CmdResult<Vec<u8>> {
        self.responses.slice(id, limit).ok_or_else(|| {
            CommandError::new("not_found", format!("Response {id} is no longer available"))
        })
    }

    pub fn sync_status(&self) -> SyncStatus {
        self.sync.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    pub fn update_sync(&self, f: impl FnOnce(&mut SyncStatus)) -> SyncStatus {
        let mut s = self.sync.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut s);
        s.clone()
    }
}

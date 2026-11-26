//! Sync client for the ReqHarbor Worker (PRODUCT §8, AC-SYNC-1..7, AC-ENV-7).
//!
//! * [`SyncClient`] is a thin typed wrapper over the Worker HTTP API.
//! * The engine is split into pure store steps ([`collect_push`],
//!   [`apply_push_results`], [`apply_pull`]) and network steps, so callers can
//!   hold their store lock only for the short local parts. [`sync_now`] runs a
//!   full pull -> push cycle for simple callers and tests.
//!
//! Conflict policy (AC-SYNC-4): last-write-wins by `updatedAt`. When a remote
//! change beats a local *unsynced* edit, the local version is kept as a sibling
//! copy named "<name> (conflict)". Deletes are tombstones (AC-SYNC-5) and
//! cascade to children. Secret variable values are never sent (AC-ENV-7) and
//! local secret values are kept when environments are pulled.

use crate::model::*;
use crate::storage::{Store, StoreError, descendant_folders, sync_key};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

/// Max changes per push / page size per pull (PRODUCT §8).
pub const BATCH: usize = 500;
/// Max serialized size of one push request body (contract C1; the Worker
/// caps bodies at 5 MB).
pub const MAX_PUSH_BYTES: usize = 4 * 1024 * 1024;
/// Max serialized size of one entity's `data` (contract C2; Worker limit).
pub const MAX_ENTITY_BYTES: usize = 512 * 1024;
/// Reason recorded for entities that exceed [`MAX_ENTITY_BYTES`].
pub const TOO_LARGE: &str = "too_large";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum SyncError {
    /// AC-SYNC-1: sync is not configured.
    #[error("Sync is not connected")]
    NotConnected,
    /// AC-SYNC-7: token rejected; the caller disconnects sync.
    #[error("Token rejected (401)")]
    Unauthorized,
    /// AC-SYNC-6: network failure; retry with backoff.
    #[error("Offline: {0}")]
    Offline(String),
    /// AC-SYNC-6: 5xx; retry with backoff.
    #[error("Server error: {0}")]
    Server(String),
    /// Share revoked (410).
    #[error("Gone: {0}")]
    Gone(String),
    #[error("Not found: {0}")]
    NotFound(String),
    /// 413 for a whole request body; the push is split and retried.
    #[error("Payload too large: {0}")]
    TooLarge(String),
    /// 413 `quota_exceeded`: the workspace is full (50 MB). Not retried.
    #[error("Workspace storage quota exceeded: {0}")]
    QuotaExceeded(String),
    /// Other 4xx (validation, rate limit...).
    #[error("Request rejected: {0}")]
    Rejected(String),
    #[error("Unexpected response: {0}")]
    Protocol(String),
    #[error("Local storage error: {0}")]
    Store(String),
}

impl SyncError {
    /// Whether the caller should retry with backoff (AC-SYNC-6).
    pub fn is_retryable(&self) -> bool {
        matches!(self, SyncError::Offline(_) | SyncError::Server(_))
    }
}

impl From<StoreError> for SyncError {
    fn from(e: StoreError) -> Self {
        SyncError::Store(e.to_string())
    }
}

/// Retry delay for attempt `n` (0-based): 5 s doubling, capped at 5 min.
pub fn backoff_delay(attempt: u32) -> Duration {
    let secs = 5u64.saturating_mul(1u64 << attempt.min(16));
    Duration::from_secs(secs.min(300))
}

/// One entity change on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub id: String,
    pub updated_at: i64,
    pub deleted: bool,
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    /// Device that produced this version (contract C3 tie-breaker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullPage {
    pub changes: Vec<Change>,
    pub cursor: u64,
    #[serde(default)]
    pub has_more: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushStatus {
    Applied,
    Stale,
    /// Per-entity rejection, e.g. `reason: "entity_too_large"` (contract C2).
    Rejected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResult {
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub id: String,
    pub status: PushStatus,
    /// Absent for `rejected` results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rev: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushResponse {
    pub results: Vec<PushResult>,
    #[serde(default)]
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatedWorkspace {
    pub workspace_id: String,
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteWorkspace {
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub entity_count: u64,
    /// Storage used by the workspace (bytes), when the Worker reports it.
    #[serde(default)]
    pub bytes_used: Option<u64>,
    /// Storage quota (bytes), when the Worker reports it.
    #[serde(default)]
    pub quota_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShareKind {
    Collection,
    Request,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareInfo {
    #[serde(alias = "id")]
    pub share_id: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub kind: Option<ShareKind>,
    #[serde(default)]
    pub entity_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

impl ShareInfo {
    /// Fill `kind`, `entityId` and `name` from the uploaded snapshot when the
    /// Worker response omits them (QA-R2-003).
    pub fn fill_from_snapshot(&mut self, snapshot: &Value) {
        let (kind, entity) = match (snapshot.get("collection"), snapshot.get("request")) {
            (Some(c), _) if c.is_object() => (ShareKind::Collection, c),
            (_, Some(r)) if r.is_object() => (ShareKind::Request, r),
            _ => return,
        };
        self.kind.get_or_insert(kind);
        if self.entity_id.is_none() {
            self.entity_id = entity.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if self.name.is_none() {
            self.name = entity
                .get("name")
                .and_then(Value::as_str)
                .map(str::to_string);
        }
    }
}

/// `GET /s/:id.json` payload.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicShare {
    #[serde(default)]
    pub share_id: String,
    pub kind: ShareKind,
    pub snapshot: Value,
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}
#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    message: String,
}

/// Typed client for the Worker API.
#[derive(Clone)]
pub struct SyncClient {
    http: reqwest::Client,
    base: String,
    token: Option<String>,
}

impl SyncClient {
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, SyncError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(SyncError::Rejected(
                "Worker URL must start with https://".into(),
            ));
        }
        crate::http::install_crypto_provider();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(crate::http::USER_AGENT)
            .build()
            .map_err(|e| SyncError::Protocol(e.to_string()))?;
        Ok(SyncClient {
            http,
            base,
            token: token.map(str::to_string),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::RequestBuilder, SyncError> {
        let token = self.token.as_deref().ok_or(SyncError::NotConnected)?;
        Ok(rb.bearer_auth(token))
    }

    async fn send<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<T, SyncError> {
        let resp = rb
            .send()
            .await
            .map_err(|e| SyncError::Offline(e.to_string()))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| SyncError::Offline(e.to_string()))?;
        if (200..300).contains(&status) {
            return serde_json::from_slice(&bytes).map_err(|e| SyncError::Protocol(e.to_string()));
        }
        let parsed = serde_json::from_slice::<ErrorBody>(&bytes).ok();
        let code = parsed
            .as_ref()
            .map(|b| b.error.code.clone())
            .unwrap_or_default();
        let message = parsed
            .map(|b| format!("{}: {}", b.error.code, b.error.message))
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(match status {
            401 => SyncError::Unauthorized,
            404 => SyncError::NotFound(message),
            410 => SyncError::Gone(message),
            413 if code == "quota_exceeded" => SyncError::QuotaExceeded(message),
            413 => SyncError::TooLarge(message),
            500..=599 => SyncError::Server(message),
            _ => SyncError::Rejected(message),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// `POST /v1/workspaces` (AC-SYNC-1 "Create workspace").
    pub async fn create_workspace(&self, name: &str) -> Result<CreatedWorkspace, SyncError> {
        self.send(
            self.http
                .post(self.url("/v1/workspaces"))
                .json(&serde_json::json!({ "name": name })),
        )
        .await
    }

    /// `GET /v1/workspace` (AC-SYNC-1 "Connect" validation).
    pub async fn workspace(&self) -> Result<RemoteWorkspace, SyncError> {
        self.send(self.auth(self.http.get(self.url("/v1/workspace")))?)
            .await
    }

    /// `POST /v1/workspace/token/rotate`.
    pub async fn rotate_token(&self) -> Result<String, SyncError> {
        #[derive(Deserialize)]
        struct T {
            token: String,
        }
        let t: T = self
            .send(self.auth(self.http.post(self.url("/v1/workspace/token/rotate")))?)
            .await?;
        Ok(t.token)
    }

    /// `GET /v1/sync?since=&limit=`.
    pub async fn pull_page(&self, since: u64) -> Result<PullPage, SyncError> {
        let url = format!("{}/v1/sync?since={since}&limit={BATCH}", self.base);
        self.send(self.auth(self.http.get(url))?).await
    }

    /// Pull every page after `since`. Returns all changes and the final cursor.
    pub async fn pull_all(&self, since: u64) -> Result<(Vec<Change>, u64), SyncError> {
        let mut all = Vec::new();
        let mut cursor = since;
        loop {
            let page = self.pull_page(cursor).await?;
            let advanced = page.cursor > cursor;
            cursor = cursor.max(page.cursor);
            all.extend(page.changes);
            if !page.has_more || !advanced {
                break;
            }
        }
        Ok((all, cursor))
    }

    /// `POST /v1/sync` for at most [`BATCH`] changes.
    pub async fn push(&self, changes: &[Change]) -> Result<PushResponse, SyncError> {
        self.send(
            self.auth(self.http.post(self.url("/v1/sync")))?
                .json(&serde_json::json!({ "changes": changes })),
        )
        .await
    }

    /// `POST /v1/shares` (AC-SHARE-1).
    pub async fn create_share(
        &self,
        kind: ShareKind,
        entity_id: &str,
        snapshot: &Value,
    ) -> Result<ShareInfo, SyncError> {
        let mut info: ShareInfo =
            self.send(self.auth(self.http.post(self.url("/v1/shares")))?.json(
                &serde_json::json!({
                    "kind": kind, "entityId": entity_id, "snapshot": snapshot
                }),
            ))
            .await?;
        info.kind.get_or_insert(kind);
        info.entity_id.get_or_insert_with(|| entity_id.to_string());
        info.fill_from_snapshot(snapshot);
        Ok(info)
    }

    /// `GET /v1/shares` (AC-SHARE-6). Accepts `{shares: [...]}` or `[...]`.
    pub async fn list_shares(&self) -> Result<Vec<ShareInfo>, SyncError> {
        let v: Value = self
            .send(self.auth(self.http.get(self.url("/v1/shares")))?)
            .await?;
        let list = match v {
            Value::Object(mut o) => o.remove("shares").unwrap_or(Value::Array(vec![])),
            other => other,
        };
        serde_json::from_value(list).map_err(|e| SyncError::Protocol(e.to_string()))
    }

    /// `PUT /v1/shares/:id` (AC-SHARE-4 "Update share").
    pub async fn update_share(
        &self,
        share_id: &str,
        snapshot: &Value,
    ) -> Result<ShareInfo, SyncError> {
        let mut info: ShareInfo = self
            .send(
                self.auth(self.http.put(self.url(&format!("/v1/shares/{share_id}"))))?
                    .json(&serde_json::json!({ "snapshot": snapshot })),
            )
            .await?;
        info.fill_from_snapshot(snapshot);
        Ok(info)
    }

    /// `DELETE /v1/shares/:id` (AC-SHARE-4 "Revoke").
    pub async fn revoke_share(&self, share_id: &str) -> Result<(), SyncError> {
        let _: Value = self
            .send(
                self.auth(
                    self.http
                        .delete(self.url(&format!("/v1/shares/{share_id}"))),
                )?,
            )
            .await?;
        Ok(())
    }

    /// Fetch a public share snapshot from its URL (AC-SHARE-5). Accepts
    /// `https://host/s/<id>` or `https://host/s/<id>.json`. No token needed.
    pub async fn fetch_public_share(share_url: &str) -> Result<PublicShare, SyncError> {
        let json_url = share_json_url(share_url)?;
        let client = SyncClient::new(&json_url, None)?;
        client.send(client.http.get(&json_url)).await
    }
}

/// Normalise a share link to its `.json` endpoint.
pub fn share_json_url(share_url: &str) -> Result<String, SyncError> {
    let u = url::Url::parse(share_url.trim())
        .map_err(|_| SyncError::Rejected("Not a valid share link".into()))?;
    let path = u.path().trim_end_matches('/');
    let Some(idx) = path.rfind("/s/") else {
        return Err(SyncError::Rejected("Not a ReqHarbor share link".into()));
    };
    let id = path[idx + 3..].trim_end_matches(".json");
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(SyncError::Rejected("Not a ReqHarbor share link".into()));
    }
    let mut out = u.clone();
    out.set_path(&format!("{}/s/{id}.json", &path[..idx]));
    out.set_query(None);
    out.set_fragment(None);
    Ok(out.to_string())
}

// ------------------------------------------------------------------ engine

/// Environment data as pushed: secret values blanked (AC-ENV-7).
pub fn strip_env_secrets(env: &Environment) -> Environment {
    let mut e = env.clone();
    for v in &mut e.variables {
        if v.secret {
            v.value.clear();
        }
    }
    e
}

fn to_value<T: Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

/// All local entities as `(type, id, updatedAt, data)`.
fn local_entities(store: &Store) -> Vec<(EntityType, String, i64, Value)> {
    let mut out = Vec::new();
    for t in store.projects.values() {
        out.push((
            EntityType::Project,
            t.project.id.clone(),
            t.project.updated_at,
            to_value(&t.project),
        ));
        for c in &t.collections {
            out.push((
                EntityType::Collection,
                c.id.clone(),
                c.updated_at,
                to_value(c),
            ));
        }
        for f in &t.folders {
            out.push((EntityType::Folder, f.id.clone(), f.updated_at, to_value(f)));
        }
        for r in &t.requests {
            out.push((EntityType::Request, r.id.clone(), r.updated_at, to_value(r)));
        }
    }
    for e in &store.environments {
        out.push((
            EntityType::Environment,
            e.id.clone(),
            e.updated_at,
            to_value(&strip_env_secrets(e)),
        ));
    }
    out
}

fn is_skipped(store: &Store, key: &str, updated_at: i64) -> bool {
    store
        .sync_state
        .sync_skipped
        .get(key)
        .is_some_and(|m| m.updated_at == updated_at)
}

/// Local changes not yet on the server: dirty entities + tombstones, each
/// stamped with this device's id. Entities marked too large are left out
/// until they change again.
pub fn collect_push(store: &Store) -> Vec<Change> {
    let device = store.sync_state.device_id.clone();
    let mut changes: Vec<Change> = local_entities(store)
        .into_iter()
        .filter(|(t, id, updated, _)| {
            let key = sync_key(*t, id);
            store.sync_state.synced.get(&key) != Some(updated) && !is_skipped(store, &key, *updated)
        })
        .map(|(t, id, updated_at, data)| Change {
            entity_type: t,
            id,
            updated_at,
            deleted: false,
            data: Some(data),
            rev: None,
            device_id: Some(device.clone()),
        })
        .collect();
    // Parents first so a partial push never strands children.
    changes.sort_by_key(|c| c.entity_type.depth());
    for t in &store.sync_state.tombstones {
        changes.push(Change {
            entity_type: t.entity_type,
            id: t.id.clone(),
            updated_at: t.deleted_at,
            deleted: true,
            data: None,
            rev: None,
            device_id: Some(device.clone()),
        });
    }
    changes
}

/// An entity excluded from sync (reported in status and [`SyncReport`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedEntity {
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub id: String,
    pub name: String,
    /// `"too_large"`.
    pub reason: String,
}

fn mark_skipped(store: &mut Store, t: EntityType, id: &str, updated_at: i64, reason: &str) {
    store.sync_state.sync_skipped.insert(
        sync_key(t, id),
        crate::storage::SyncSkipped {
            updated_at,
            reason: reason.to_string(),
        },
    );
}

/// Entities currently excluded from sync (their skipped version is still
/// the local one). Stale marks are pruned.
pub fn skipped_entities(store: &mut Store) -> Vec<SkippedEntity> {
    let marks: Vec<(String, crate::storage::SyncSkipped)> = store
        .sync_state
        .sync_skipped
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = Vec::new();
    for (key, mark) in marks {
        let Some((t, id)) = parse_sync_key(&key) else {
            store.sync_state.sync_skipped.remove(&key);
            continue;
        };
        match local_meta(store, t, id) {
            Some((updated, name)) if updated == mark.updated_at => out.push(SkippedEntity {
                entity_type: t,
                id: id.to_string(),
                name,
                reason: mark.reason.clone(),
            }),
            _ => {
                store.sync_state.sync_skipped.remove(&key);
            }
        }
    }
    out
}

fn parse_sync_key(key: &str) -> Option<(EntityType, &str)> {
    let (t, id) = key.split_once(':')?;
    let t = match t {
        "project" => EntityType::Project,
        "collection" => EntityType::Collection,
        "folder" => EntityType::Folder,
        "request" => EntityType::Request,
        "environment" => EntityType::Environment,
        _ => return None,
    };
    Some((t, id))
}

fn data_size(c: &Change) -> usize {
    c.data
        .as_ref()
        .map(|d| serde_json::to_vec(d).map(|v| v.len()).unwrap_or(0))
        .unwrap_or(0)
}

/// Serialized size of a change inside a push body (plus a separator).
fn change_size(c: &Change) -> usize {
    serde_json::to_vec(c).map(|v| v.len()).unwrap_or(0) + 1
}

/// Collect pending changes, mark entities over [`MAX_ENTITY_BYTES`] as
/// skipped (never sent), and split the rest into batches of at most
/// [`BATCH`] changes and [`MAX_PUSH_BYTES`] bytes (contracts C1, C2).
pub fn plan_push(store: &mut Store) -> Vec<Vec<Change>> {
    let mut sendable = Vec::new();
    for c in collect_push(store) {
        if !c.deleted && data_size(&c) > MAX_ENTITY_BYTES {
            mark_skipped(store, c.entity_type, &c.id, c.updated_at, TOO_LARGE);
        } else {
            sendable.push(c);
        }
    }
    batch_changes(sendable, BATCH, MAX_PUSH_BYTES)
}

/// Split changes into batches bounded by count and serialized bytes.
pub fn batch_changes(changes: Vec<Change>, max_count: usize, max_bytes: usize) -> Vec<Vec<Change>> {
    const ENVELOPE: usize = 32; // `{"changes":[` ... `]}`
    let mut batches = Vec::new();
    let mut cur: Vec<Change> = Vec::new();
    let mut cur_bytes = ENVELOPE;
    for c in changes {
        let size = change_size(&c);
        if !cur.is_empty() && (cur.len() >= max_count || cur_bytes + size > max_bytes) {
            batches.push(std::mem::take(&mut cur));
            cur_bytes = ENVELOPE;
        }
        cur_bytes += size;
        cur.push(c);
    }
    if !cur.is_empty() {
        batches.push(cur);
    }
    batches
}

/// Push one batch; on a whole-body 413 split it in halves and retry. A
/// single change that is still too large is reported as rejected
/// (`entity_too_large`) instead of failing the sync (contract C2).
pub async fn push_batch(
    client: &SyncClient,
    batch: Vec<Change>,
) -> Result<Vec<(Vec<Change>, Vec<PushResult>)>, SyncError> {
    let mut out = Vec::new();
    let mut queue = vec![batch];
    while let Some(b) = queue.pop() {
        if b.is_empty() {
            continue;
        }
        match client.push(&b).await {
            Ok(resp) => out.push((b, resp.results)),
            Err(SyncError::TooLarge(_)) if b.len() > 1 => {
                let mut first = b;
                let second = first.split_off(first.len() / 2);
                queue.push(second);
                queue.push(first);
            }
            Err(SyncError::TooLarge(_)) => {
                let results = b
                    .iter()
                    .map(|c| PushResult {
                        entity_type: c.entity_type,
                        id: c.id.clone(),
                        status: PushStatus::Rejected,
                        rev: None,
                        reason: Some("entity_too_large".into()),
                    })
                    .collect();
                out.push((b, results));
            }
            Err(e) => return Err(e),
        }
    }
    Ok(out)
}

/// Outcome of applying push results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PushApplied {
    pub applied: usize,
    /// Changes the server rejected (now marked skipped).
    pub rejected: usize,
}

/// Record push results: applied entities become clean; rejected ones are
/// marked skipped so they are not resent until edited; tombstones are
/// dropped either way (a stale delete means the server copy is newer and
/// will come back on the next pull).
pub fn apply_push_results(store: &mut Store, pushed: &[Change], results: &[PushResult]) -> usize {
    apply_push_results_detailed(store, pushed, results).applied
}

pub fn apply_push_results_detailed(
    store: &mut Store,
    pushed: &[Change],
    results: &[PushResult],
) -> PushApplied {
    let mut out = PushApplied::default();
    for r in results {
        let Some(change) = pushed
            .iter()
            .find(|c| c.id == r.id && c.entity_type == r.entity_type)
        else {
            continue;
        };
        let key = sync_key(change.entity_type, &change.id);
        match (change.deleted, r.status) {
            (true, _) => {
                store
                    .sync_state
                    .tombstones
                    .retain(|t| !(t.id == change.id && t.entity_type == change.entity_type));
                store.sync_state.synced.remove(&key);
                store.sync_state.origins.remove(&key);
            }
            (false, PushStatus::Applied) => {
                store
                    .sync_state
                    .synced
                    .insert(key.clone(), change.updated_at);
                store.sync_state.origins.remove(&key);
                store.sync_state.sync_skipped.remove(&key);
            }
            (false, PushStatus::Rejected) => {
                mark_skipped(
                    store,
                    change.entity_type,
                    &change.id,
                    change.updated_at,
                    TOO_LARGE,
                );
            }
            (false, PushStatus::Stale) => {}
        }
        match r.status {
            PushStatus::Applied => out.applied += 1,
            PushStatus::Rejected => out.rejected += 1,
            PushStatus::Stale => {}
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictCopy {
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub id: String,
    pub name: String,
    pub copy_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullReport {
    pub applied: usize,
    pub deleted: usize,
    pub conflicts: Vec<ConflictCopy>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncReport {
    pub pushed: usize,
    pub pulled: usize,
    pub deleted: usize,
    pub conflicts: Vec<ConflictCopy>,
    /// Remote changes that could not be applied (orphans, malformed).
    pub skipped: Vec<String>,
    /// Local entities not synced because they exceed 512 KB (contract C2).
    pub too_large: Vec<SkippedEntity>,
    pub cursor: u64,
}

fn local_meta(store: &Store, t: EntityType, id: &str) -> Option<(i64, String)> {
    match t {
        EntityType::Project => store
            .projects
            .get(id)
            .map(|p| (p.project.updated_at, p.project.name.clone())),
        EntityType::Collection => store.collection(id).map(|c| (c.updated_at, c.name.clone())),
        EntityType::Folder => store
            .projects
            .values()
            .flat_map(|t| t.folders.iter())
            .find(|f| f.id == id)
            .map(|f| (f.updated_at, f.name.clone())),
        EntityType::Request => store.request(id).map(|r| (r.updated_at, r.name.clone())),
        EntityType::Environment => store
            .environment(id)
            .map(|e| (e.updated_at, e.name.clone())),
    }
}

/// Keep the losing local version as "<name> (conflict)" (AC-SYNC-4).
fn conflict_copy(store: &mut Store, t: EntityType, id: &str) -> Option<String> {
    let now = now_ms();
    let copy_id = new_id();
    let suffix = |n: &str| format!("{n} (conflict)");
    match t {
        EntityType::Project => {
            let src = store.projects.get(id)?.project.clone();
            let p = Project {
                id: copy_id.clone(),
                name: suffix(&src.name),
                updated_at: now,
                created_at: now,
                ..src
            };
            store.projects.insert(
                copy_id.clone(),
                ProjectTree {
                    project: p,
                    collections: vec![],
                    folders: vec![],
                    requests: vec![],
                },
            );
        }
        EntityType::Collection => {
            let pid = store
                .project_of(crate::storage::NodeKind::Collection, id)
                .ok()?;
            let t = store.projects.get_mut(&pid)?;
            let src = t.collections.iter().find(|c| c.id == id)?.clone();
            t.collections.push(Collection {
                id: copy_id.clone(),
                name: suffix(&src.name),
                updated_at: now,
                created_at: now,
                ..src
            });
        }
        EntityType::Folder => {
            let pid = store
                .project_of(crate::storage::NodeKind::Folder, id)
                .ok()?;
            let t = store.projects.get_mut(&pid)?;
            let src = t.folders.iter().find(|f| f.id == id)?.clone();
            t.folders.push(Folder {
                id: copy_id.clone(),
                name: suffix(&src.name),
                updated_at: now,
                created_at: now,
                ..src
            });
        }
        EntityType::Request => {
            let pid = store
                .project_of(crate::storage::NodeKind::Request, id)
                .ok()?;
            let t = store.projects.get_mut(&pid)?;
            let src = t.requests.iter().find(|r| r.id == id)?.clone();
            t.requests.push(Request {
                id: copy_id.clone(),
                name: suffix(&src.name),
                updated_at: now,
                created_at: now,
                ..src
            });
        }
        EntityType::Environment => {
            let src = store.environment(id)?.clone();
            src.project_id.as_ref()?; // only one Global
            store.environments.push(Environment {
                id: copy_id.clone(),
                name: suffix(&src.name),
                updated_at: now,
                created_at: now,
                ..src
            });
        }
    }
    Some(copy_id)
}

fn remove_local(store: &mut Store, t: EntityType, id: &str) -> Vec<(EntityType, String)> {
    let mut removed = Vec::new();
    match t {
        EntityType::Project => {
            if let Some(tree) = store.forget_project(id) {
                removed.push((EntityType::Project, id.to_string()));
                removed.extend(
                    tree.collections
                        .iter()
                        .map(|c| (EntityType::Collection, c.id.clone())),
                );
                removed.extend(
                    tree.folders
                        .iter()
                        .map(|f| (EntityType::Folder, f.id.clone())),
                );
                removed.extend(
                    tree.requests
                        .iter()
                        .map(|r| (EntityType::Request, r.id.clone())),
                );
                let envs: Vec<String> = store
                    .environments
                    .iter()
                    .filter(|e| e.project_id.as_deref() == Some(id))
                    .map(|e| e.id.clone())
                    .collect();
                store
                    .environments
                    .retain(|e| e.project_id.as_deref() != Some(id));
                removed.extend(envs.into_iter().map(|e| (EntityType::Environment, e)));
            }
        }
        EntityType::Collection => {
            for tree in store.projects.values_mut() {
                if tree.collections.iter().any(|c| c.id == id) {
                    removed.push((EntityType::Collection, id.to_string()));
                    removed.extend(
                        tree.folders
                            .iter()
                            .filter(|f| f.collection_id == id)
                            .map(|f| (EntityType::Folder, f.id.clone())),
                    );
                    removed.extend(
                        tree.requests
                            .iter()
                            .filter(|r| r.collection_id == id)
                            .map(|r| (EntityType::Request, r.id.clone())),
                    );
                    tree.collections.retain(|c| c.id != id);
                    tree.folders.retain(|f| f.collection_id != id);
                    tree.requests.retain(|r| r.collection_id != id);
                }
            }
        }
        EntityType::Folder => {
            for tree in store.projects.values_mut() {
                if tree.folders.iter().any(|f| f.id == id) {
                    let ids = descendant_folders(tree, id);
                    removed.extend(ids.iter().map(|f| (EntityType::Folder, f.clone())));
                    removed.extend(
                        tree.requests
                            .iter()
                            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
                            .map(|r| (EntityType::Request, r.id.clone())),
                    );
                    tree.folders.retain(|f| !ids.contains(&f.id));
                    tree.requests
                        .retain(|r| !r.folder_id.as_ref().is_some_and(|f| ids.contains(f)));
                }
            }
        }
        EntityType::Request => {
            for tree in store.projects.values_mut() {
                if tree.requests.iter().any(|r| r.id == id) {
                    removed.push((EntityType::Request, id.to_string()));
                    tree.requests.retain(|r| r.id != id);
                }
            }
        }
        EntityType::Environment => {
            if store.environments.iter().any(|e| e.id == id) {
                removed.push((EntityType::Environment, id.to_string()));
                store.environments.retain(|e| e.id != id);
                for tree in store.projects.values_mut() {
                    if tree.project.active_environment_id.as_deref() == Some(id) {
                        tree.project.active_environment_id = None;
                    }
                }
            }
        }
    }
    removed
}

/// Insert or replace an entity from remote data. Returns false when the
/// parent is missing (orphan) or the data is malformed.
fn upsert_remote(store: &mut Store, change: &Change) -> Result<(), String> {
    let data = change.data.clone().ok_or("missing data")?;
    let ws_id = store.workspace.id.clone();
    let err = |e: serde_json::Error| format!("malformed {}: {e}", change.entity_type.as_str());
    match change.entity_type {
        EntityType::Project => {
            let mut p: Project = serde_json::from_value(data).map_err(err)?;
            p.workspace_id = ws_id;
            p.updated_at = change.updated_at;
            match store.projects.get_mut(&p.id) {
                Some(t) => t.project = p,
                None => {
                    store.projects.insert(
                        p.id.clone(),
                        ProjectTree {
                            project: p,
                            collections: vec![],
                            folders: vec![],
                            requests: vec![],
                        },
                    );
                }
            }
        }
        EntityType::Collection => {
            let mut c: Collection = serde_json::from_value(data).map_err(err)?;
            c.updated_at = change.updated_at;
            if !store.projects.contains_key(&c.project_id) {
                return Err(format!("collection {} has no project", c.id));
            }
            for t in store.projects.values_mut() {
                t.collections.retain(|x| x.id != c.id);
            }
            if let Some(t) = store.projects.get_mut(&c.project_id) {
                t.collections.push(c);
            }
        }
        EntityType::Folder => {
            let mut f: Folder = serde_json::from_value(data).map_err(err)?;
            f.updated_at = change.updated_at;
            let pid = store
                .project_of(crate::storage::NodeKind::Collection, &f.collection_id)
                .map_err(|_| format!("folder {} has no collection", f.id))?;
            for t in store.projects.values_mut() {
                t.folders.retain(|x| x.id != f.id);
            }
            if let Some(t) = store.projects.get_mut(&pid) {
                t.folders.push(f);
            }
        }
        EntityType::Request => {
            let mut r: Request = serde_json::from_value(data).map_err(err)?;
            r.updated_at = change.updated_at;
            let pid = store
                .project_of(crate::storage::NodeKind::Collection, &r.collection_id)
                .map_err(|_| format!("request {} has no collection", r.id))?;
            for t in store.projects.values_mut() {
                t.requests.retain(|x| x.id != r.id);
            }
            if let Some(t) = store.projects.get_mut(&pid) {
                t.requests.push(r);
            }
        }
        EntityType::Environment => {
            let mut e: Environment = serde_json::from_value(data).map_err(err)?;
            e.workspace_id = ws_id;
            e.updated_at = change.updated_at;
            if let Some(pid) = &e.project_id
                && !store.projects.contains_key(pid)
            {
                return Err(format!("environment {} has no project", e.id));
            }
            // Keep local secret values (they are never on the server).
            let local = store
                .environments
                .iter()
                .find(|x| x.id == e.id)
                .or_else(|| {
                    if e.project_id.is_none() {
                        store.global_environment()
                    } else {
                        None
                    }
                })
                .cloned();
            if let Some(local) = &local {
                for v in e.variables.iter_mut().filter(|v| v.secret) {
                    if let Some(lv) = local
                        .variables
                        .iter()
                        .find(|lv| lv.key == v.key && lv.secret)
                    {
                        v.value = lv.value.clone();
                    }
                }
            }
            if e.project_id.is_none() {
                // One Global per workspace: adopt the remote one, keeping
                // local-only variables of an unsynced local Global.
                if let Some(local) = local.filter(|l| l.id != e.id) {
                    let was_synced = store
                        .sync_state
                        .synced
                        .contains_key(&sync_key(EntityType::Environment, &local.id));
                    if !was_synced {
                        let extra: Vec<Variable> = local
                            .variables
                            .iter()
                            .filter(|lv| !e.variables.iter().any(|v| v.key == lv.key))
                            .cloned()
                            .collect();
                        if !extra.is_empty() {
                            e.variables.extend(extra);
                            e.updated_at = now_ms(); // merged result must be pushed
                        }
                    }
                    store.environments.retain(|x| x.id != local.id);
                }
            }
            match store.environments.iter_mut().find(|x| x.id == e.id) {
                Some(slot) => *slot = e,
                None => store.environments.push(e),
            }
        }
    }
    Ok(())
}

/// Apply pulled changes (parents first) with LWW + conflict copies.
pub fn apply_pull(store: &mut Store, mut changes: Vec<Change>, cursor: u64) -> PullReport {
    let mut report = PullReport::default();
    // Latest change per entity wins within a batch.
    changes.sort_by_key(|c| c.rev.unwrap_or(0));
    let mut latest: Vec<Change> = Vec::new();
    for c in changes {
        latest.retain(|x| !(x.id == c.id && x.entity_type == c.entity_type));
        latest.push(c);
    }
    let (mut upserts, mut deletes): (Vec<Change>, Vec<Change>) =
        latest.into_iter().partition(|c| !c.deleted);
    upserts.sort_by_key(|c| c.entity_type.depth());
    deletes.sort_by_key(|c| std::cmp::Reverse(c.entity_type.depth()));
    let had_remote_projects = upserts.iter().any(|c| c.entity_type == EntityType::Project);

    let my_device = store.sync_state.device_id.clone();
    for change in &upserts {
        let key = sync_key(change.entity_type, &change.id);
        let synced_at = store.sync_state.synced.get(&key).copied();
        let remote_device = change.device_id.clone().unwrap_or_default();
        match local_meta(store, change.entity_type, &change.id) {
            Some((local_updated, _)) => {
                let dirty = synced_at != Some(local_updated);
                let local_device = if dirty {
                    my_device.clone()
                } else {
                    store
                        .sync_state
                        .origins
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| my_device.clone())
                };
                // LWW by (updatedAt, deviceId) — contract C3.
                let order = (change.updated_at, remote_device.as_str())
                    .cmp(&(local_updated, local_device.as_str()));
                if order == std::cmp::Ordering::Equal {
                    store.sync_state.synced.insert(key, change.updated_at);
                    continue; // same version
                }
                if dirty && order == std::cmp::Ordering::Less {
                    continue; // local edit wins: keep it, it will be pushed
                }
                if dirty && let Some(copy_id) = conflict_copy(store, change.entity_type, &change.id)
                {
                    let name = local_meta(store, change.entity_type, &copy_id)
                        .map(|m| m.1)
                        .unwrap_or_default();
                    report.conflicts.push(ConflictCopy {
                        entity_type: change.entity_type,
                        id: change.id.clone(),
                        name,
                        copy_id,
                    });
                }
            }
            None => {
                if let Some(t) = store
                    .sync_state
                    .tombstones
                    .iter()
                    .find(|t| t.id == change.id && t.entity_type == change.entity_type)
                {
                    if (t.deleted_at, my_device.as_str())
                        >= (change.updated_at, remote_device.as_str())
                    {
                        continue; // our delete is newer
                    }
                    let id = change.id.clone();
                    store.sync_state.tombstones.retain(|t| t.id != id);
                }
            }
        }
        match upsert_remote(store, change) {
            Ok(()) => {
                // Merged Globals carry a fresh local updatedAt and stay dirty.
                let stored = local_meta(store, change.entity_type, &change.id).map(|m| m.0);
                if stored == Some(change.updated_at) {
                    store
                        .sync_state
                        .synced
                        .insert(key.clone(), change.updated_at);
                    if remote_device.is_empty() || remote_device == my_device {
                        store.sync_state.origins.remove(&key);
                    } else {
                        store
                            .sync_state
                            .origins
                            .insert(key.clone(), remote_device.clone());
                    }
                }
                store.sync_state.sync_skipped.remove(&key);
                report.applied += 1;
            }
            Err(e) => report.skipped.push(e),
        }
    }

    for change in &deletes {
        let key = sync_key(change.entity_type, &change.id);
        let Some((local_updated, _)) = local_meta(store, change.entity_type, &change.id) else {
            store.sync_state.synced.remove(&key);
            store.sync_state.tombstones.retain(|t| t.id != change.id);
            continue;
        };
        let dirty = store.sync_state.synced.get(&key) != Some(&local_updated);
        let remote_device = change.device_id.clone().unwrap_or_default();
        if dirty
            && (local_updated, my_device.as_str()) > (change.updated_at, remote_device.as_str())
        {
            continue; // local edit after the remote delete wins
        }
        for (t, id) in remove_local(store, change.entity_type, &change.id) {
            let k = sync_key(t, &id);
            store.sync_state.synced.remove(&k);
            store.sync_state.origins.remove(&k);
            store.sync_state.sync_skipped.remove(&k);
            store.sync_state.tombstones.retain(|x| x.id != id);
        }
        report.deleted += 1;
    }

    // A fresh device connecting to an existing workspace: drop its empty,
    // never-synced "Default" project in favour of the pulled ones.
    if had_remote_projects {
        let empties: Vec<String> = store
            .projects
            .values()
            .filter(|t| {
                t.collections.is_empty()
                    && t.project.name == "Default"
                    && !store
                        .sync_state
                        .synced
                        .contains_key(&sync_key(EntityType::Project, &t.project.id))
                    && !store
                        .environments
                        .iter()
                        .any(|e| e.project_id.as_deref() == Some(t.project.id.as_str()))
            })
            .map(|t| t.project.id.clone())
            .collect();
        if empties.len() < store.projects.len() {
            for id in empties {
                store.forget_project(&id);
            }
        }
    }

    if let Some(sync) = store.workspace.sync.as_mut() {
        sync.cursor = sync.cursor.max(cursor);
    }
    store.sync_state.last_sync_at = Some(now_ms());
    report
}

/// Push everything dirty in batches of [`BATCH`].
pub async fn push_all(client: &SyncClient, store: &mut Store) -> Result<usize, SyncError> {
    let mut applied = 0;
    for batch in plan_push(store) {
        for (sent, results) in push_batch(client, batch).await? {
            applied += apply_push_results(store, &sent, &results);
        }
    }
    store.save_sync_state()?;
    Ok(applied)
}

/// Pull and apply everything after the stored cursor.
pub async fn pull(client: &SyncClient, store: &mut Store) -> Result<PullReport, SyncError> {
    let since = store
        .workspace
        .sync
        .as_ref()
        .map(|s| s.cursor)
        .ok_or(SyncError::NotConnected)?;
    let (changes, cursor) = client.pull_all(since).await?;
    let report = apply_pull(store, changes, cursor);
    store.save_all()?;
    Ok(report)
}

/// Full cycle: pull (LWW + conflict copies), then push local changes.
pub async fn sync_now(client: &SyncClient, store: &mut Store) -> Result<SyncReport, SyncError> {
    if store.workspace.sync.is_none() {
        return Err(SyncError::NotConnected);
    }
    // Pull first so conflicts are resolved locally (and a freshly connected
    // device adopts the remote tree) before anything is pushed.
    let report = pull(client, store).await?;
    let pushed = push_all(client, store).await?;
    Ok(SyncReport {
        pushed,
        pulled: report.applied,
        deleted: report.deleted,
        conflicts: report.conflicts,
        skipped: report.skipped,
        too_large: skipped_entities(store),
        cursor: store.workspace.sync.as_ref().map(|s| s.cursor).unwrap_or(0),
    })
}

/// Connect the store to a remote workspace (AC-SYNC-1). Validates the token.
pub async fn connect(
    store: &mut Store,
    worker_url: &str,
    token: &str,
) -> Result<RemoteWorkspace, SyncError> {
    let client = SyncClient::new(worker_url, Some(token))?;
    let remote = client.workspace().await?;
    store.workspace.sync = Some(SyncConfig {
        worker_url: client.base_url().to_string(),
        remote_workspace_id: remote.workspace_id.clone(),
        token: token.to_string(),
        cursor: 0,
    });
    // A different remote: everything local is unsynced relative to it.
    store.sync_state.reset_for_new_remote();
    store.save_workspace()?;
    store.save_sync_state()?;
    Ok(remote)
}

/// Forget the sync configuration (AC-SYNC-7 on 401, or user disconnect).
/// Local data is untouched.
pub fn disconnect(store: &mut Store) -> Result<(), SyncError> {
    store.workspace.sync = None;
    store.save_workspace()?;
    Ok(())
}

/// Build a client from the store's sync config.
pub fn client_for(store: &Store) -> Result<SyncClient, SyncError> {
    let cfg = store
        .workspace
        .sync
        .as_ref()
        .ok_or(SyncError::NotConnected)?;
    SyncClient::new(&cfg.worker_url, Some(&cfg.token))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_sync_6_backoff_5s_to_5min() {
        assert_eq!(backoff_delay(0), Duration::from_secs(5));
        assert_eq!(backoff_delay(1), Duration::from_secs(10));
        assert_eq!(backoff_delay(5), Duration::from_secs(160));
        assert_eq!(backoff_delay(6), Duration::from_secs(300));
        assert_eq!(backoff_delay(100), Duration::from_secs(300));
    }

    #[test]
    fn share_url_normalisation() {
        assert_eq!(
            share_json_url("https://w.dev/s/abc123").unwrap(),
            "https://w.dev/s/abc123.json"
        );
        assert_eq!(
            share_json_url("https://w.dev/s/abc123.json?x#y").unwrap(),
            "https://w.dev/s/abc123.json"
        );
        assert_eq!(
            share_json_url("https://w.dev/base/s/AbC/").unwrap(),
            "https://w.dev/base/s/AbC.json"
        );
        assert!(share_json_url("https://w.dev/x/abc").is_err());
        assert!(share_json_url("not a url").is_err());
    }

    #[test]
    fn ac_env_7_secret_values_never_pushed() {
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: None,
            name: "Global".into(),
            variables: vec![
                Variable::new("a", "1"),
                Variable::new("tok", "s3cret").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        let s = strip_env_secrets(&env);
        assert_eq!(s.variables[0].value, "1");
        assert_eq!(s.variables[1].value, "");
        assert!(s.variables[1].secret);
    }
}

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

/// Header carrying this build's version on every sync call, so a server can
/// tell which clients understand forward-compatible data.
pub const VERSION_HEADER: &str = "X-ReqHarbor-Version";

/// Max changes per push / page size per pull (PRODUCT §8).
pub const BATCH: usize = 500;
/// Max serialized size of one push request body (contract C1; the Worker
/// caps bodies at 5 MB).
pub const MAX_PUSH_BYTES: usize = 4 * 1024 * 1024;
/// Max serialized size of one entity's `data` (contract C2; Worker limit).
pub const MAX_ENTITY_BYTES: usize = 512 * 1024;
/// Reason recorded for entities that exceed [`MAX_ENTITY_BYTES`].
pub const TOO_LARGE: &str = "too_large";
/// Reason for local entities of a type the server does not accept
/// (06-go-server §9); also a per-change push `rejected` reason.
pub const UNSUPPORTED_TYPE: &str = "unsupported_type";
/// Per-change push reason: the change's `updatedAt` is too far ahead of the
/// server clock. The change stays dirty (M1 restamps it, 04 §6.9).
pub const CLOCK_SKEW: &str = "clock_skew";
/// Per-change push reason: the caller's role may not change the entity.
pub const FORBIDDEN_ROLE: &str = "forbidden_role";
/// Per-change push reason: the data holds a value the server can't store
/// (e.g. `1e400`, a lone surrogate, nesting over 100 levels).
pub const INVALID_JSON_VALUE: &str = "invalid_json_value";
/// Reason recorded when the server rejects a change without saying why.
pub const REJECTED: &str = "rejected";
/// Sync types every server accepts: assumed when `GET /v1/server` is missing
/// (the Worker, an old server) or does not list `sync.types`.
pub const BASE_SYNC_TYPES: [&str; 5] =
    ["project", "collection", "folder", "request", "environment"];
/// How long a server's `sync.types` are trusted before `GET /v1/server` is
/// asked again (it is also asked again while items are held back).
pub const SERVER_INFO_TTL_MS: i64 = 15 * 60 * 1000;
/// At most this many pulled changes of unknown types are kept (AC-FWD-5).
pub const MAX_UNKNOWN_CHANGES: usize = 5_000;
/// ... and at most this many serialized bytes of them.
pub const MAX_UNKNOWN_BYTES: usize = 8 * 1024 * 1024;

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

/// One `GET /v1/sync` page. Changes whose `type` this build does not know
/// are kept raw in `unknown` instead of failing the page (AC-FWD-5).
#[derive(Debug, Clone)]
pub struct PullPage {
    pub changes: Vec<Change>,
    pub unknown: Vec<Value>,
    pub cursor: u64,
    pub has_more: bool,
    /// Changes when the server's data is restored from a backup (Go server;
    /// absent on the Worker).
    pub sync_epoch: Option<String>,
}

impl<'de> Deserialize<'de> for PullPage {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Raw {
            changes: Vec<Value>,
            cursor: u64,
            #[serde(default)]
            has_more: bool,
            #[serde(default)]
            sync_epoch: Option<String>,
        }
        let raw = Raw::deserialize(d)?;
        let mut changes = Vec::new();
        let mut unknown = Vec::new();
        for v in raw.changes {
            let known = match v.get("type").and_then(Value::as_str) {
                Some(t) => EntityType::parse(t).is_some(),
                None => return Err(D::Error::custom("change without a type")),
            };
            if known {
                changes.push(serde_json::from_value(v).map_err(D::Error::custom)?);
            } else {
                unknown.push(v);
            }
        }
        Ok(PullPage {
            changes,
            unknown,
            cursor: raw.cursor,
            has_more: raw.has_more,
            sync_epoch: raw.sync_epoch.filter(|e| !e.is_empty()),
        })
    }
}

/// Every page after a cursor, merged.
#[derive(Debug, Clone, Default)]
pub struct Pulled {
    pub changes: Vec<Change>,
    /// Raw changes of unknown `type` (AC-FWD-5).
    pub unknown: Vec<Value>,
    pub cursor: u64,
    /// The server's `syncEpoch` (last one seen), if it reports one.
    pub sync_epoch: Option<String>,
    /// The server was restored from a backup: the pull restarted from cursor
    /// 0 and every local entity must be re-sent (06-go-server §9).
    pub restored: bool,
}

/// `GET /v1/server` (Go server discovery, 06-go-server §5.1). Every field is
/// optional so any server version parses; only what sync needs is typed.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    #[serde(default)]
    pub software: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub api_version: Option<u64>,
    #[serde(default)]
    pub instance_id: Option<String>,
    #[serde(default)]
    pub sync_epoch: Option<String>,
    #[serde(default)]
    pub sync: Option<ServerSyncInfo>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSyncInfo {
    /// Sync `type`s the server accepts on push.
    #[serde(default)]
    pub types: Option<Vec<String>>,
    #[serde(default)]
    pub request_kinds: Option<Vec<String>>,
    #[serde(flatten)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PushStatus {
    Applied,
    Stale,
    /// Per-entity rejection, e.g. `reason: "entity_too_large"` (contract C2).
    Rejected,
    /// A status from a newer server that this build doesn't know. Handled
    /// like `stale`: the entity stays dirty and is sent again later.
    #[serde(other)]
    Unknown,
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
    /// Results this build can read. A result it can't parse (say, for a
    /// type it never sent) is dropped: its change simply stays dirty.
    #[serde(deserialize_with = "lenient_results")]
    pub results: Vec<PushResult>,
    #[serde(default)]
    pub cursor: u64,
}

fn lenient_results<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<PushResult>, D::Error> {
    let raw = Vec::<Value>::deserialize(d)?;
    Ok(raw
        .into_iter()
        .filter_map(|v| serde_json::from_value(v).ok())
        .collect())
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
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            VERSION_HEADER,
            reqwest::header::HeaderValue::from_static(CLIENT_VERSION),
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(crate::http::USER_AGENT)
            .default_headers(headers)
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

    /// Pull every page after `since`: all changes (known and unknown types)
    /// and the final cursor. Restore detection uses only the cursor rule; see
    /// [`SyncClient::pull_since`].
    pub async fn pull_all(&self, since: u64) -> Result<Pulled, SyncError> {
        self.pull_since(since, None).await
    }

    /// Pull every page after `since`, detecting a server restored from a
    /// backup (06-go-server §9), checked on every page before the cursor
    /// moves: a `syncEpoch` that differs from `known_epoch` (when both are
    /// present), or a page cursor below the `since` that was sent. Then the
    /// pull starts again from 0 and the result is marked `restored`.
    pub async fn pull_since(
        &self,
        since: u64,
        known_epoch: Option<&str>,
    ) -> Result<Pulled, SyncError> {
        let mut known = known_epoch.filter(|e| !e.is_empty()).map(str::to_string);
        let mut out = Pulled {
            cursor: since,
            sync_epoch: known.clone(),
            ..Pulled::default()
        };
        let mut restarts = 0;
        loop {
            let sent = out.cursor;
            let page = self.pull_page(sent).await?;
            let epoch_changed = matches!(
                (&known, &page.sync_epoch),
                (Some(k), Some(e)) if k != e
            );
            if epoch_changed || page.cursor < sent {
                restarts += 1;
                if restarts > 3 {
                    return Err(SyncError::Protocol(
                        "The server's sync history keeps changing; try again later".into(),
                    ));
                }
                if page.sync_epoch.is_some() {
                    known = page.sync_epoch.clone();
                }
                out = Pulled {
                    sync_epoch: known.clone(),
                    restored: true,
                    ..Pulled::default()
                };
                continue;
            }
            if page.sync_epoch.is_some() {
                known = page.sync_epoch.clone();
                out.sync_epoch = page.sync_epoch.clone();
            }
            let advanced = page.cursor > sent;
            out.cursor = page.cursor;
            out.changes.extend(page.changes);
            out.unknown.extend(page.unknown);
            if !page.has_more || !advanced {
                break;
            }
        }
        Ok(out)
    }

    /// `GET /v1/server` (no token). `Ok(None)` for a server without it (a
    /// 404: the Worker or an old server) or a body that isn't server info.
    pub async fn server_info(&self) -> Result<Option<ServerInfo>, SyncError> {
        match self
            .send::<Value>(self.http.get(self.url("/v1/server")))
            .await
        {
            Ok(v) => Ok(serde_json::from_value(v).ok()),
            Err(SyncError::NotFound(_)) => Ok(None),
            Err(e) => Err(e),
        }
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

/// An entity from a newer ReqHarbor (AC-FWD-3/4) that is already on the
/// server: a local version of it is never pushed, only its deletion.
fn frozen_on_server(store: &Store, t: EntityType, id: &str) -> bool {
    read_only_entity(store, t, id) && store.sync_state.synced.contains_key(&sync_key(t, id))
}

/// A local entity this build cannot edit (a request it can't handle, or any
/// entity whose `minClientVersion` is newer): the remote version always wins.
pub fn read_only_entity(store: &Store, t: EntityType, id: &str) -> bool {
    match t {
        EntityType::Request => store.request(id).is_some_and(|r| !r.is_editable()),
        EntityType::Project => store
            .projects
            .get(id)
            .is_some_and(|p| !p.project.is_editable()),
        EntityType::Collection => store.collection(id).is_some_and(|c| !c.is_editable()),
        EntityType::Folder => store
            .projects
            .values()
            .flat_map(|t| t.folders.iter())
            .any(|f| f.id == id && !f.is_editable()),
        EntityType::Environment => store.environment(id).is_some_and(|e| !e.is_editable()),
    }
}

/// The entity is unchanged since a server restore and only needs re-sending.
fn is_resend(store: &Store, key: &str, updated_at: i64) -> bool {
    store.sync_state.resend.get(key) == Some(&updated_at)
}

/// Sync types the server accepts (its `sync.types`, else the base five).
pub fn server_types(store: &Store) -> Vec<String> {
    match &store.sync_state.server_types {
        Some(t) => t.clone(),
        None => BASE_SYNC_TYPES.iter().map(|s| s.to_string()).collect(),
    }
}

fn type_accepted(types: &[String], t: EntityType) -> bool {
    types.iter().any(|x| x == t.as_str())
}

/// Record a `GET /v1/server` result: `None` (no discovery endpoint) means
/// the base types.
pub fn record_server_info(store: &mut Store, info: Option<&ServerInfo>) {
    store.sync_state.server_types = info
        .and_then(|i| i.sync.as_ref())
        .and_then(|s| s.types.clone());
    store.sync_state.server_checked_at = Some(now_ms());
}

/// Whether `GET /v1/server` should be asked (again) before this sync:
/// never asked, older than [`SERVER_INFO_TTL_MS`], or items are held back
/// (so the notice clears as soon as the server is updated).
pub fn server_info_due(store: &Store, now: i64) -> bool {
    match store.sync_state.server_checked_at {
        None => true,
        Some(at) => now - at >= SERVER_INFO_TTL_MS || !unsupported_entities(store).is_empty(),
    }
}

/// Ask `GET /v1/server` when due and record the result. Network failures
/// keep the cached types (the pull will report the failure).
/// `None` when nothing was asked (or the request failed); `Some(info)` with
/// the server's answer otherwise, `Some(None)` meaning "no discovery".
pub async fn refresh_server_info(client: &SyncClient, due: bool) -> Option<Option<ServerInfo>> {
    if !due {
        return None;
    }
    match client.server_info().await {
        Ok(info) => Some(info),
        // A 200 that isn't JSON: not a ReqHarbor server, so the base types.
        Err(SyncError::Protocol(_)) => Some(None),
        // Offline or failing: keep what is cached (the pull reports errors).
        Err(_) => None,
    }
}

/// Dirty local entities (and tombstones) of a type the server does not
/// accept: they stay local-only (06-go-server §9).
pub fn unsupported_entities(store: &Store) -> Vec<SkippedEntity> {
    let types = server_types(store);
    if BASE_SYNC_TYPES.iter().all(|b| types.iter().any(|t| t == b)) {
        return Vec::new();
    }
    collect_push(store)
        .into_iter()
        .filter(|c| !type_accepted(&types, c.entity_type))
        .map(|c| SkippedEntity {
            name: local_meta(store, c.entity_type, &c.id)
                .map(|m| m.1)
                .unwrap_or_default(),
            entity_type: c.entity_type,
            id: c.id,
            reason: UNSUPPORTED_TYPE.to_string(),
        })
        .collect()
}

/// "3 items aren't synced because this server is older than your app. …"
pub fn unsupported_notice(n: usize) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 item isn't synced because this server is older than your app. Ask its admin to update reqharbor-server.".into()),
        n => Some(format!("{n} items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server.")),
    }
}

/// Changes a push can clear: [`collect_push`] minus the entities of a type
/// the server doesn't accept (those are reported by
/// [`unsupported_entities`] and stay local-only until the server is
/// updated).
pub fn pending_count(store: &Store) -> usize {
    let types = server_types(store);
    collect_push(store)
        .iter()
        .filter(|c| type_accepted(&types, c.entity_type))
        .count()
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
            store.sync_state.synced.get(&key) != Some(updated)
                && !is_skipped(store, &key, *updated)
                && !frozen_on_server(store, *t, id)
        })
        .map(|(t, id, updated_at, data)| {
            // An entity re-sent after a server restore keeps the device id of
            // the version it is, so the push is idempotent (contract C3).
            let key = sync_key(t, &id);
            let device_id = if is_resend(store, &key, updated_at) {
                store
                    .sync_state
                    .origins
                    .get(&key)
                    .unwrap_or(&device)
                    .clone()
            } else {
                device.clone()
            };
            Change {
                entity_type: t,
                id,
                updated_at,
                deleted: false,
                data: Some(data),
                rev: None,
                device_id: Some(device_id),
            }
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
    /// `"too_large"`, `"unsupported_type"`, or the reason the server gave
    /// when it rejected the change (e.g. `"invalid_json_value"`,
    /// `"forbidden_role"`).
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
    Some((EntityType::parse(t)?, id))
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
    let types = server_types(store);
    for c in collect_push(store) {
        if !type_accepted(&types, c.entity_type) {
            continue; // local-only until the server accepts the type
        }
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
    /// Changes the server rejected (upserts are now marked skipped with the
    /// server's reason; rejected deletes and `clock_skew` stay pending).
    pub rejected: usize,
    /// Changes rejected with `unsupported_type`: they stay dirty and local.
    pub unsupported: usize,
    /// Changes rejected with `clock_skew`: they stay dirty (M1 restamps).
    pub clock_skew: usize,
    /// Results with a status this build doesn't know: left dirty.
    pub unknown: usize,
}

/// The reason recorded in `sync_skipped` for a rejected upsert: the
/// server's own reason (`entity_too_large` is stored as [`TOO_LARGE`]).
pub fn skip_reason(reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        None => REJECTED.to_string(),
        Some("entity_too_large") => TOO_LARGE.to_string(),
        Some(r) => r.to_string(),
    }
}

/// Record push results:
/// * applied entities become clean, and applied or stale deletes drop their
///   tombstone (a stale delete means the server copy is newer and will come
///   back on the next pull);
/// * a rejected delete keeps its tombstone, so it is sent again rather than
///   forgotten (the server still has the entity);
/// * a rejected upsert is marked skipped with the server's reason and is not
///   resent until edited, except `clock_skew` (stays dirty) and
///   `unsupported_type` (stays local until the server lists the type);
/// * a status this build doesn't know is treated like `stale`: the change
///   stays pending.
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
        if r.status == PushStatus::Rejected && r.reason.as_deref() == Some(UNSUPPORTED_TYPE) {
            // The server changed since discovery: keep it local, stop sending
            // the type and ask `GET /v1/server` again on the next run.
            let mut types = server_types(store);
            types.retain(|t| t != change.entity_type.as_str());
            store.sync_state.server_types = Some(types);
            store.sync_state.server_checked_at = None;
            out.unsupported += 1;
            continue;
        }
        match (change.deleted, r.status) {
            (true, PushStatus::Applied | PushStatus::Stale) => {
                store
                    .sync_state
                    .tombstones
                    .retain(|t| !(t.id == change.id && t.entity_type == change.entity_type));
                store.sync_state.synced.remove(&key);
                store.sync_state.origins.remove(&key);
            }
            // Kept: the server still has the entity, so the delete is
            // pending until the server accepts it.
            (true, PushStatus::Rejected | PushStatus::Unknown) => {}
            (false, PushStatus::Applied) => {
                store
                    .sync_state
                    .synced
                    .insert(key.clone(), change.updated_at);
                if !is_resend(store, &key, change.updated_at) {
                    store.sync_state.origins.remove(&key);
                }
                store.sync_state.resend.remove(&key);
                store.sync_state.sync_skipped.remove(&key);
            }
            (false, PushStatus::Rejected) if r.reason.as_deref() == Some(CLOCK_SKEW) => {
                // Left dirty for the M1 restamp (04 §6.9), never skipped.
                out.clock_skew += 1;
            }
            (false, PushStatus::Rejected) => {
                mark_skipped(
                    store,
                    change.entity_type,
                    &change.id,
                    change.updated_at,
                    &skip_reason(r.reason.as_deref()),
                );
            }
            (false, PushStatus::Stale | PushStatus::Unknown) => {}
        }
        match r.status {
            PushStatus::Applied => out.applied += 1,
            PushStatus::Rejected => out.rejected += 1,
            PushStatus::Unknown => out.unknown += 1,
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
    /// Changes of an unknown `type`, kept in sync state (AC-FWD-5).
    pub unknown: usize,
    /// The server was restored from a backup; everything is being re-sent.
    pub restored: bool,
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
    /// Local entities of a type this server doesn't accept; they stay
    /// local-only (`reason: "unsupported_type"`, 06-go-server §9).
    pub unsupported: Vec<SkippedEntity>,
    /// The server was restored from a backup: the pull restarted from 0 and
    /// every local entity was re-sent (06-go-server §9).
    pub restored: bool,
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
    if read_only_entity(store, t, id) {
        return None; // never copy an entity from a newer ReqHarbor
    }
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
            store.projects.insert(copy_id.clone(), ProjectTree::new(p));
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
            if !src.is_editable() {
                return None; // never copy a request from a newer ReqHarbor
            }
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
                    store.projects.insert(p.id.clone(), ProjectTree::new(p));
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
                let resend = is_resend(store, &key, local_updated);
                let dirty = synced_at != Some(local_updated)
                    && !resend
                    && !read_only_entity(store, change.entity_type, &change.id);
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
                    store
                        .sync_state
                        .synced
                        .insert(key.clone(), change.updated_at);
                    store.sync_state.resend.remove(&key);
                    continue; // same version
                }
                if (dirty || resend) && order == std::cmp::Ordering::Less {
                    // A local edit wins, or the restored server lost our
                    // newer version: keep it, it will be pushed.
                    continue;
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
                store.sync_state.resend.remove(&key);
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
        let dirty = store.sync_state.synced.get(&key) != Some(&local_updated)
            && !read_only_entity(store, change.entity_type, &change.id);
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
            store.sync_state.resend.remove(&k);
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

    // Restore detection ran before this (see `SyncClient::pull_since`), so
    // the server's cursor is taken as is, even when it went down.
    if let Some(sync) = store.workspace.sync.as_mut() {
        sync.cursor = cursor;
    }
    store.sync_state.last_sync_at = Some(now_ms());
    report
}

/// Keep pulled changes of unknown `type` in sync state (AC-FWD-5), latest
/// per `(type, id)`, so a newer build can apply them later. Returns how many
/// were remembered.
pub fn remember_unknown(store: &mut Store, unknown: Vec<Value>) -> usize {
    let n = unknown.len();
    if n == 0 {
        return 0;
    }
    let ident = |v: &Value| {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        (s("type"), s("id"))
    };
    let rev = |x: &Value| x.get("rev").and_then(Value::as_u64).unwrap_or(0);
    let list = std::mem::take(&mut store.sync_state.unknown_changes);
    let mut slots: Vec<Option<Value>> = list.into_iter().map(Some).collect();
    let mut index: std::collections::HashMap<(String, String), usize> = slots
        .iter()
        .enumerate()
        .filter_map(|(i, v)| v.as_ref().map(|v| (ident(v), i)))
        .collect();
    for v in unknown {
        let id = ident(&v);
        if let Some(&i) = index.get(&id) {
            if slots[i].as_ref().is_some_and(|x| rev(x) > rev(&v)) {
                continue;
            }
            slots[i] = None; // the newer one goes to the end
        }
        index.insert(id, slots.len());
        slots.push(Some(v));
    }
    store.sync_state.unknown_changes = slots.into_iter().flatten().collect();
    cap_unknown(store);
    n
}

/// Keep [`SyncState::unknown_changes`] within [`MAX_UNKNOWN_CHANGES`] and
/// [`MAX_UNKNOWN_BYTES`], dropping the oldest (lowest `rev`) first and
/// noting that something was dropped.
fn cap_unknown(store: &mut Store) {
    let list = &mut store.sync_state.unknown_changes;
    let size = |v: &Value| serde_json::to_vec(v).map(|b| b.len()).unwrap_or(0);
    let mut bytes: usize = list.iter().map(size).sum();
    if list.len() <= MAX_UNKNOWN_CHANGES && bytes <= MAX_UNKNOWN_BYTES {
        return;
    }
    list.sort_by_key(|v| v.get("rev").and_then(Value::as_u64).unwrap_or(0));
    let mut drop = 0;
    while drop < list.len()
        && (list.len() - drop > MAX_UNKNOWN_CHANGES || bytes > MAX_UNKNOWN_BYTES)
    {
        bytes -= size(&list[drop]);
        drop += 1;
    }
    list.drain(..drop);
    store.sync_state.unknown_changes_truncated = true;
}

/// The server was restored from a backup (06-go-server §9): forget what it
/// is known to hold and re-send everything. Entities that were clean are
/// remembered in [`SyncState::resend`] so a re-pulled equal version is not a
/// conflict and an older one never overwrites them. LWW keeps this
/// idempotent.
pub fn mark_restored(store: &mut Store) {
    let clean: Vec<(String, i64)> = local_entities(store)
        .into_iter()
        .map(|(t, id, updated, _)| (sync_key(t, &id), updated))
        .filter(|(key, updated)| {
            store.sync_state.synced.get(key) == Some(updated)
                || store.sync_state.resend.get(key) == Some(updated)
        })
        .collect();
    store.sync_state.resend = clean.into_iter().collect();
    store.sync_state.synced.clear();
    store.sync_state.unknown_changes.clear();
    store.sync_state.unknown_changes_truncated = false;
    if let Some(sync) = store.workspace.sync.as_mut() {
        sync.cursor = 0;
    }
}

/// [`mark_restored`] when the server was restored, the epoch, then
/// [`remember_unknown`] and [`apply_pull`] for one pull.
pub fn apply_pulled(store: &mut Store, pulled: Pulled) -> PullReport {
    if pulled.restored {
        mark_restored(store);
    }
    if let Some(e) = pulled.sync_epoch.filter(|e| !e.is_empty()) {
        store.sync_state.sync_epoch = Some(e);
    }
    let unknown = remember_unknown(store, pulled.unknown);
    let mut report = apply_pull(store, pulled.changes, pulled.cursor);
    report.unknown = unknown;
    report.restored = pulled.restored;
    report
}

/// Where the next pull starts: the stored cursor and `syncEpoch`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PullStart {
    pub since: u64,
    pub epoch: Option<String>,
}

pub fn pull_start(store: &Store) -> Result<PullStart, SyncError> {
    let since = store
        .workspace
        .sync
        .as_ref()
        .map(|s| s.cursor)
        .ok_or(SyncError::NotConnected)?;
    Ok(PullStart {
        since,
        epoch: store.sync_state.sync_epoch.clone(),
    })
}

/// Pull from `start`. A `syncEpoch` from `GET /v1/server` that differs from
/// the stored one is a restore too: the pull starts from 0.
pub async fn pull_from(
    client: &SyncClient,
    start: &PullStart,
    server: Option<&ServerInfo>,
) -> Result<Pulled, SyncError> {
    let stored = start.epoch.as_deref().filter(|e| !e.is_empty());
    let server_epoch = server
        .and_then(|i| i.sync_epoch.as_deref())
        .filter(|e| !e.is_empty());
    if let (Some(s), Some(e)) = (stored, server_epoch)
        && s != e
    {
        let mut p = client.pull_since(0, Some(e)).await?;
        p.restored = true;
        return Ok(p);
    }
    let mut p = client.pull_since(start.since, stored).await?;
    if p.sync_epoch.is_none() {
        p.sync_epoch = server_epoch.map(str::to_string);
    }
    Ok(p)
}

/// Push everything dirty (of a type the server accepts) in batches of
/// [`BATCH`].
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
    pull_with_info(client, store, None).await
}

async fn pull_with_info(
    client: &SyncClient,
    store: &mut Store,
    server: Option<&ServerInfo>,
) -> Result<PullReport, SyncError> {
    let start = pull_start(store)?;
    let pulled = pull_from(client, &start, server).await?;
    let report = apply_pulled(store, pulled);
    store.save_all()?;
    Ok(report)
}

/// Full cycle: discovery (when due), pull (LWW + conflict copies), then
/// push local changes.
pub async fn sync_now(client: &SyncClient, store: &mut Store) -> Result<SyncReport, SyncError> {
    if store.workspace.sync.is_none() {
        return Err(SyncError::NotConnected);
    }
    let info = refresh_server_info(client, server_info_due(store, now_ms())).await;
    if let Some(info) = &info {
        record_server_info(store, info.as_ref());
    }
    // Pull first so conflicts are resolved locally (and a freshly connected
    // device adopts the remote tree) before anything is pushed.
    let report = pull_with_info(client, store, info.as_ref().and_then(Option::as_ref)).await?;
    let pushed = push_all(client, store).await?;
    Ok(SyncReport {
        pushed,
        pulled: report.applied,
        deleted: report.deleted,
        conflicts: report.conflicts,
        skipped: report.skipped,
        too_large: skipped_entities(store),
        unsupported: unsupported_entities(store),
        restored: report.restored,
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
            extra: Extra::new(),
        };
        let s = strip_env_secrets(&env);
        assert_eq!(s.variables[0].value, "1");
        assert_eq!(s.variables[1].value, "");
        assert!(s.variables[1].secret);
    }
}

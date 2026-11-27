//! Sync client for the ReqHarbor Worker (PRODUCT §8, AC-SYNC-1..7, AC-ENV-7).
//!
//! * [`SyncClient`] is a thin typed wrapper over the sync API: token sync
//!   (`/v1/sync` with an `rh_` token, [`Scope::Legacy`]) or an account space
//!   (`/v1/spaces/{id}/sync` with a session, [`Scope::Space`]).
//! * Every engine step takes the [`Scope`] it works on (`*_in`); the
//!   functions without the suffix are the token-sync (legacy) scope. In
//!   account mode a scope sees only the projects bound to it, and pulled data
//!   never rebinds anything (04 §6.3 rules B1–B7).
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

use crate::auth::{AuthError, Session};
use crate::model::*;
pub use crate::spaces::Scope;
use crate::spaces::{Attribution, MAX_DEFERRED_RUNS, SpaceIssue};
use crate::storage::{Store, StoreError, SyncState, descendant_folders, sync_key};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
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
/// Per-change rejection of a Global environment pushed to a team (B4).
pub const GLOBAL_ENV_NOT_ALLOWED: &str = "global_env_not_allowed";
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
    /// Other 4xx (validation...).
    #[error("Request rejected: {0}")]
    Rejected(String),
    #[error("Unexpected response: {0}")]
    Protocol(String),
    #[error("Local storage error: {0}")]
    Store(String),
    /// An account space's session ended (AC-AUTH-9): sign in again. Local
    /// data, bindings and unpushed edits are untouched.
    #[error("Your session ended. Sign in again to keep syncing.")]
    SignInRequired,
    /// 403 `not_a_member` / 404 `space_not_found` on a space call: the space
    /// is temporarily unavailable (04 §6.7); never a removal by itself.
    #[error("This space is temporarily unavailable: {0}")]
    SpaceGone(String),
    /// 403 with another code (e.g. `forbidden_role`).
    #[error("Not allowed: {0}")]
    Forbidden(String),
    /// 429: retried with backoff.
    #[error("Too many requests: {0}")]
    RateLimited(String),
    /// 426 `client_too_old`.
    #[error("This server needs a newer version of ReqHarbor.")]
    ClientTooOld(String),
    /// 507 `server_full`: retried with backoff (06 §9).
    #[error("The sync server is out of space. Your changes are saved on this device.")]
    ServerFull(String),
}

impl SyncError {
    /// Whether the caller should retry with backoff (AC-SYNC-6).
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            SyncError::Offline(_)
                | SyncError::Server(_)
                | SyncError::ServerFull(_)
                | SyncError::RateLimited(_)
        )
    }
}

impl From<AuthError> for SyncError {
    fn from(e: AuthError) -> Self {
        match e {
            AuthError::SignInRequired | AuthError::InvalidGrant | AuthError::Unauthorized(_) => {
                SyncError::SignInRequired
            }
            AuthError::Offline(m) => SyncError::Offline(m),
            AuthError::Server(m) => SyncError::Server(m),
            AuthError::ServerFull => SyncError::ServerFull("server_full".into()),
            AuthError::RateLimited => SyncError::RateLimited("rate_limited".into()),
            AuthError::ClientTooOld => SyncError::ClientTooOld("client_too_old".into()),
            AuthError::NotSignedIn => SyncError::NotConnected,
            other => SyncError::Rejected(other.to_string()),
        }
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
    /// Who made this version (pulled from account spaces; never pushed).
    /// Kept in the space's `attribution`, not in the entity JSON.
    #[serde(default, skip_serializing, deserialize_with = "lenient_updated_by")]
    pub updated_by: Option<Attribution>,
}

/// `updatedBy` as a user id string or `{id|userId, name}`; anything else
/// is ignored.
fn lenient_updated_by<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Attribution>, D::Error> {
    let v = Value::deserialize(d)?;
    let s = |o: &serde_json::Map<String, Value>, k: &str| {
        o.get(k).and_then(Value::as_str).unwrap_or("").to_string()
    };
    Ok(match v {
        Value::String(id) if !id.is_empty() => Some(Attribution {
            user_id: id,
            name: String::new(),
        }),
        Value::Object(o) => {
            let id = Some(s(&o, "userId"))
                .filter(|x| !x.is_empty())
                .unwrap_or_else(|| s(&o, "id"));
            let name = s(&o, "name");
            (!id.is_empty() || !name.is_empty()).then_some(Attribution { user_id: id, name })
        }
        _ => None,
    })
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
    /// The server's clock (Unix ms), on account spaces (04 §6.9).
    #[serde(default)]
    pub server_time: Option<i64>,
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
    /// The space the share lives in (account mode; `legacy` for token
    /// sync). Set by the client, not the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    /// Who created it (space shares, AC-TSHARE-2): `name` is null for a
    /// former member. Absent on legacy shares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_by: Option<crate::teams::UserRef>,
    /// The options it was uploaded with (space shares, AC-TSHARE-3):
    /// `{includeCredentials, environmentId}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
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

/// How a [`SyncClient`] authenticates.
#[derive(Clone)]
pub enum ClientAuth {
    None,
    /// A legacy workspace token (`rh_…`).
    Token(String),
    /// An account session: a 401 refreshes once and retries once.
    Session(Arc<Session>),
}

/// Typed client for the sync API of one scope.
#[derive(Clone)]
pub struct SyncClient {
    http: reqwest::Client,
    base: String,
    auth: ClientAuth,
    scope: Scope,
}

impl SyncClient {
    /// A token-sync (legacy) client.
    pub fn new(base_url: &str, token: Option<&str>) -> Result<Self, SyncError> {
        Self::build(
            base_url,
            match token {
                Some(t) => ClientAuth::Token(t.to_string()),
                None => ClientAuth::None,
            },
            Scope::Legacy,
        )
    }

    /// A client for the account space `space_id` on `base_url`.
    pub fn for_space(
        base_url: &str,
        space_id: &str,
        session: Arc<Session>,
    ) -> Result<Self, SyncError> {
        Self::build(
            base_url,
            ClientAuth::Session(session),
            Scope::Space(space_id.to_string()),
        )
    }

    fn build(base_url: &str, auth: ClientAuth, scope: Scope) -> Result<Self, SyncError> {
        let base = base_url.trim().trim_end_matches('/').to_string();
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err(SyncError::Rejected(
                "Server URL must start with https:// or http://".into(),
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
            auth,
            scope,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    pub fn scope(&self) -> &Scope {
        &self.scope
    }

    /// Send an authenticated request built by `build`. With a session, a
    /// 401 refreshes (single-flight) and retries once; a second 401 means
    /// the session is gone ([`SyncError::SignInRequired`]).
    async fn authed<T: serde::de::DeserializeOwned>(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<T, SyncError> {
        match &self.auth {
            ClientAuth::None => Err(SyncError::NotConnected),
            ClientAuth::Token(t) => self.send(build().bearer_auth(t)).await,
            ClientAuth::Session(s) => {
                let token = s.access_token().await?;
                match self.send(build().bearer_auth(&token)).await {
                    Err(SyncError::Unauthorized) => {
                        let retry = s.refresh_if_current(&token).await?;
                        match self.send(build().bearer_auth(&retry)).await {
                            Err(SyncError::Unauthorized) => {
                                s.mark_sign_in_required();
                                Err(SyncError::SignInRequired)
                            }
                            other => other,
                        }
                    }
                    other => other,
                }
            }
        }
    }

    /// `/v1/sync` or `/v1/spaces/{id}/sync` (and the same for shares).
    fn scoped(&self, rest: &str) -> String {
        format!("{}{}{rest}", self.base, self.scope.prefix())
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
        Err(match (status, code.as_str()) {
            (401, _) => SyncError::Unauthorized,
            (403, "legacy_disabled") => {
                SyncError::Rejected(crate::spaces::LEGACY_DISABLED_MESSAGE.into())
            }
            (403, "not_a_member") | (404, "space_not_found") => SyncError::SpaceGone(message),
            (403, _) if !code.is_empty() => SyncError::Forbidden(message),
            (404, _) => SyncError::NotFound(message),
            (410, _) => SyncError::Gone(message),
            (413, "quota_exceeded") => SyncError::QuotaExceeded(message),
            (413, _) => SyncError::TooLarge(message),
            (426, _) | (_, "client_too_old") => SyncError::ClientTooOld(message),
            (429, _) => SyncError::RateLimited(message),
            (507, _) | (_, "server_full") => SyncError::ServerFull(message),
            (500..=599, _) => SyncError::Server(message),
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
        self.authed(|| self.http.get(self.url("/v1/workspace")))
            .await
    }

    /// `POST /v1/workspace/token/rotate`.
    pub async fn rotate_token(&self) -> Result<String, SyncError> {
        #[derive(Deserialize)]
        struct T {
            token: String,
        }
        let t: T = self
            .authed(|| self.http.post(self.url("/v1/workspace/token/rotate")))
            .await?;
        Ok(t.token)
    }

    /// `GET /v1/sync?since=&limit=` (or the space's sync route).
    pub async fn pull_page(&self, since: u64) -> Result<PullPage, SyncError> {
        let url = self.scoped(&format!("/sync?since={since}&limit={BATCH}"));
        self.authed(|| self.http.get(&url)).await
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

    /// `POST /v1/sync` (or the space's sync route) for at most [`BATCH`]
    /// changes.
    pub async fn push(&self, changes: &[Change]) -> Result<PushResponse, SyncError> {
        let url = self.scoped("/sync");
        let body = serde_json::json!({ "changes": changes });
        self.authed(|| self.http.post(&url).json(&body)).await
    }

    /// `POST /v1/shares` (AC-SHARE-1).
    pub async fn create_share(
        &self,
        kind: ShareKind,
        entity_id: &str,
        snapshot: &Value,
    ) -> Result<ShareInfo, SyncError> {
        let url = self.scoped("/shares");
        let body = serde_json::json!({
            "kind": kind, "entityId": entity_id, "snapshot": snapshot
        });
        let mut info: ShareInfo = self.authed(|| self.http.post(&url).json(&body)).await?;
        info.kind.get_or_insert(kind);
        info.entity_id.get_or_insert_with(|| entity_id.to_string());
        info.fill_from_snapshot(snapshot);
        Ok(info)
    }

    /// `GET /v1/shares` (AC-SHARE-6). Accepts `{shares: [...]}` or `[...]`.
    pub async fn list_shares(&self) -> Result<Vec<ShareInfo>, SyncError> {
        let url = self.scoped("/shares");
        let v: Value = self.authed(|| self.http.get(&url)).await?;
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
        let url = self.scoped(&format!("/shares/{share_id}"));
        let body = serde_json::json!({ "snapshot": snapshot });
        let mut info: ShareInfo = self.authed(|| self.http.put(&url).json(&body)).await?;
        info.fill_from_snapshot(snapshot);
        Ok(info)
    }

    /// `DELETE /v1/shares/:id` (AC-SHARE-4 "Revoke").
    pub async fn revoke_share(&self, share_id: &str) -> Result<(), SyncError> {
        let url = self.scoped(&format!("/shares/{share_id}"));
        let _: Value = self.authed(|| self.http.delete(&url)).await?;
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

const LEGACY: &Scope = &Scope::Legacy;

fn st<'a>(store: &'a Store, scope: &Scope) -> &'a SyncState {
    store.space_state(scope)
}

fn st_mut<'a>(store: &'a mut Store, scope: &Scope) -> &'a mut SyncState {
    store.space_state_mut(scope)
}

/// The local entities `scope` syncs, as `(type, id, updatedAt, data)`: the
/// projects bound to it with their trees and environments, and the Global
/// environment when `scope` holds it (B4).
fn local_entities(store: &Store, scope: &Scope) -> Vec<(EntityType, String, i64, Value)> {
    let mut out = Vec::new();
    for t in store.projects.values() {
        if !store.in_scope(scope, &t.project.id) {
            continue;
        }
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
        if store.scope_of_env(e).as_ref() != Some(scope) {
            continue;
        }
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
fn frozen_on_server(store: &Store, scope: &Scope, t: EntityType, id: &str) -> bool {
    read_only_entity(store, t, id) && st(store, scope).synced.contains_key(&sync_key(t, id))
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
fn is_resend(store: &Store, scope: &Scope, key: &str, updated_at: i64) -> bool {
    st(store, scope).resend.get(key) == Some(&updated_at)
}

/// Sync types the token-sync server accepts (its `sync.types`, else the
/// base five).
pub fn server_types(store: &Store) -> Vec<String> {
    server_types_in(store, LEGACY)
}

pub fn server_types_in(store: &Store, scope: &Scope) -> Vec<String> {
    match &st(store, scope).server_types {
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
    record_server_info_in(store, LEGACY, info)
}

pub fn record_server_info_in(store: &mut Store, scope: &Scope, info: Option<&ServerInfo>) {
    let state = st_mut(store, scope);
    state.server_types = info
        .and_then(|i| i.sync.as_ref())
        .and_then(|s| s.types.clone());
    state.server_checked_at = Some(now_ms());
}

/// Whether `GET /v1/server` should be asked (again) before this sync:
/// never asked, older than [`SERVER_INFO_TTL_MS`], or items are held back
/// (so the notice clears as soon as the server is updated).
pub fn server_info_due(store: &Store, now: i64) -> bool {
    server_info_due_in(store, LEGACY, now)
}

pub fn server_info_due_in(store: &Store, scope: &Scope, now: i64) -> bool {
    match st(store, scope).server_checked_at {
        None => true,
        Some(at) => {
            now - at >= SERVER_INFO_TTL_MS || !unsupported_entities_in(store, scope).is_empty()
        }
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
    unsupported_entities_in(store, LEGACY)
}

pub fn unsupported_entities_in(store: &Store, scope: &Scope) -> Vec<SkippedEntity> {
    let types = server_types_in(store, scope);
    if BASE_SYNC_TYPES.iter().all(|b| types.iter().any(|t| t == b)) {
        return Vec::new();
    }
    collect_push_in(store, scope)
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
    pending_count_in(store, LEGACY)
}

pub fn pending_count_in(store: &Store, scope: &Scope) -> usize {
    let types = server_types_in(store, scope);
    collect_push_in(store, scope)
        .iter()
        .filter(|c| type_accepted(&types, c.entity_type))
        .count()
}

fn is_skipped(store: &Store, scope: &Scope, key: &str, updated_at: i64) -> bool {
    st(store, scope)
        .sync_skipped
        .get(key)
        .is_some_and(|m| m.updated_at == updated_at)
}

/// Local changes not yet on the token-sync server.
pub fn collect_push(store: &Store) -> Vec<Change> {
    collect_push_in(store, LEGACY)
}

/// Local changes not yet in `scope`: dirty entities + tombstones, each
/// stamped with this device's id. Entities marked too large are left out
/// until they change again. In account mode only the projects (and Global)
/// bound to `scope` are considered, and a scope that may not sync (a paused
/// binding of another server or account, a shares-only legacy workspace)
/// sends nothing.
pub fn collect_push_in(store: &Store, scope: &Scope) -> Vec<Change> {
    if store.account_mode() && !store.scope_syncable(scope) {
        return Vec::new();
    }
    // A viewer pushes nothing: the server refuses the whole push (04 §6.2).
    if store.read_only_scope(scope) {
        return Vec::new();
    }
    let mut changes = collect_dirty(store, scope);
    if store.account_mode() {
        let ss = store.space_sync(scope);
        // Children of a project whose own tombstone waits (refused with
        // `forbidden_role`, or one this role can't send): their tombstones
        // stay here, so Discard brings the whole project back.
        let waiting_projects: std::collections::BTreeSet<String> = st(store, scope)
            .tombstones
            .iter()
            .filter(|t| {
                t.entity_type == EntityType::Project
                    && (ss.frozen.contains(&sync_key(EntityType::Project, &t.id))
                        || project_delete_refused(store, scope))
            })
            .map(|t| t.id.clone())
            .collect();
        if !ss.frozen.is_empty()
            || !ss.held.is_empty()
            || !ss.deferred_deletes.is_empty()
            || !waiting_projects.is_empty()
        {
            let frozen: std::collections::BTreeSet<&str> =
                ss.frozen.iter().map(String::as_str).collect();
            let held: std::collections::BTreeSet<&str> = ss
                .held
                .iter()
                .chain(ss.deferred_deletes.iter())
                .map(String::as_str)
                .collect();
            let child_of_waiting = |c: &Change| {
                c.deleted
                    && c.entity_type != EntityType::Project
                    && tombstone_project(store, scope, c)
                        .is_some_and(|p| waiting_projects.contains(p))
            };
            changes.retain(|c| {
                let key = sync_key(c.entity_type, &c.id);
                !frozen.contains(key.as_str())
                    && !held.contains(key.as_str())
                    && !child_of_waiting(c)
                    && !ancestors(store, c.entity_type, &c.id)
                        .iter()
                        .any(|a| held.contains(a.as_str()))
            });
        }
        if is_team_scope(store, scope) {
            for c in &mut changes {
                if c.entity_type == EntityType::Request
                    && let Some(d) = c.data.as_mut()
                {
                    strip_device_local_paths(d);
                }
            }
        }
    }
    changes
}

/// The project a pushed tombstone belonged to (recorded when the project
/// was deleted, `Tombstone.extra.projectId`).
fn tombstone_project<'a>(store: &'a Store, scope: &Scope, c: &Change) -> Option<&'a str> {
    st(store, scope)
        .tombstones
        .iter()
        .find(|t| t.id == c.id && t.entity_type == c.entity_type)
        .and_then(|t| t.extra.get(TOMBSTONE_PROJECT))
        .and_then(Value::as_str)
}

/// The tombstone field naming the project a child was deleted with.
pub const TOMBSTONE_PROJECT: &str = "projectId";

/// This account can't delete projects in `scope` (a team below admin): the
/// server will refuse the project tombstone, so its children's wait.
fn project_delete_refused(store: &Store, scope: &Scope) -> bool {
    is_team_scope(store, scope)
        && !matches!(
            store.scope_role(scope),
            Some(crate::spaces::Role::Admin | crate::spaces::Role::Owner)
        )
}

/// `"type:id"` of every unsynced change of `scope` (entities and
/// tombstones), whatever the role: what a role downgrade freezes.
pub fn dirty_keys_in(store: &Store, scope: &Scope) -> Vec<String> {
    collect_dirty(store, scope)
        .iter()
        .map(|c| sync_key(c.entity_type, &c.id))
        .collect()
}

/// A team space (or one of a kind from a newer server).
fn is_team_scope(store: &Store, scope: &Scope) -> bool {
    match scope {
        Scope::Legacy => false,
        Scope::Space(id) => store.binding(id).is_some_and(|b| {
            matches!(
                b.kind,
                crate::spaces::SpaceKind::Team | crate::spaces::SpaceKind::Unknown
            )
        }),
    }
}

/// Team pushes leave device-local paths out (AC-TSYNC-8): a binary body's
/// path, the value of each form file field and a gRPC request's `.proto`
/// files and import folders (AC-GRPC-4) stay on this device.
pub fn strip_device_local_paths(data: &mut Value) {
    if let Some(grpc) = data.get_mut("grpc").and_then(Value::as_object_mut) {
        for key in ["protoFiles", "importPaths"] {
            if let Some(v) = grpc.get_mut(key)
                && v.is_array()
            {
                *v = Value::Array(Vec::new());
            }
        }
    }
    let Some(body) = data.get_mut("body").and_then(Value::as_object_mut) else {
        return;
    };
    if let Some(p) = body.get_mut("binaryPath")
        && p.is_string()
    {
        *p = Value::String(String::new());
    }
    if let Some(form) = body.get_mut("form").and_then(Value::as_array_mut) {
        for f in form {
            if f.get("kind").and_then(Value::as_str) == Some("file")
                && let Some(v) = f.get_mut("value")
            {
                *v = Value::String(String::new());
            }
        }
    }
}

/// The containers above a local entity, as `"type:id"`: its folders,
/// collection and project (a project environment: its project).
pub fn ancestors(store: &Store, t: EntityType, id: &str) -> Vec<String> {
    let mut out = Vec::new();
    let tree_of = |pid: &str| store.projects.get(pid);
    match t {
        EntityType::Project => {}
        EntityType::Environment => {
            if let Some(pid) = store.environment(id).and_then(|e| e.project_id.clone()) {
                out.push(sync_key(EntityType::Project, &pid));
            }
        }
        EntityType::Collection => {
            if let Some(c) = store.collection(id) {
                out.push(sync_key(EntityType::Project, &c.project_id));
            }
        }
        EntityType::Folder | EntityType::Request => {
            let (collection, mut folder) = if t == EntityType::Request {
                match store.request(id) {
                    Some(r) => (r.collection_id.clone(), r.folder_id.clone()),
                    None => return out,
                }
            } else {
                let Some(f) = store
                    .projects
                    .values()
                    .flat_map(|t| t.folders.iter())
                    .find(|f| f.id == id)
                else {
                    return out;
                };
                (f.collection_id.clone(), f.parent_folder_id.clone())
            };
            let pid = store.collection(&collection).map(|c| c.project_id.clone());
            let mut guard = 0;
            while let Some(fid) = folder.take() {
                out.push(sync_key(EntityType::Folder, &fid));
                guard += 1;
                if guard > 256 {
                    break;
                }
                folder = pid
                    .as_deref()
                    .and_then(tree_of)
                    .and_then(|t| t.folders.iter().find(|f| f.id == fid))
                    .and_then(|f| f.parent_folder_id.clone());
            }
            out.push(sync_key(EntityType::Collection, &collection));
            if let Some(p) = pid {
                out.push(sync_key(EntityType::Project, &p));
            }
        }
    }
    out
}

/// Everything under a container (not the container itself).
pub fn descendants(store: &Store, t: EntityType, id: &str) -> Vec<(EntityType, String)> {
    match t {
        EntityType::Project => store
            .project_entities(id)
            .into_iter()
            .filter(|(et, x)| !(*et == EntityType::Project && x == id))
            .collect(),
        EntityType::Collection => store
            .projects
            .values()
            .find(|tr| tr.collections.iter().any(|c| c.id == id))
            .map(|tr| {
                tr.folders
                    .iter()
                    .filter(|f| f.collection_id == id)
                    .map(|f| (EntityType::Folder, f.id.clone()))
                    .chain(
                        tr.requests
                            .iter()
                            .filter(|r| r.collection_id == id)
                            .map(|r| (EntityType::Request, r.id.clone())),
                    )
                    .collect()
            })
            .unwrap_or_default(),
        EntityType::Folder => store
            .projects
            .values()
            .find(|tr| tr.folders.iter().any(|f| f.id == id))
            .map(|tr| {
                let ids = descendant_folders(tr, id);
                ids.iter()
                    .filter(|f| f.as_str() != id)
                    .map(|f| (EntityType::Folder, f.clone()))
                    .chain(
                        tr.requests
                            .iter()
                            .filter(|r| r.folder_id.as_ref().is_some_and(|f| ids.contains(f)))
                            .map(|r| (EntityType::Request, r.id.clone())),
                    )
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// Dirty entities and tombstones of `scope`, before any role or freeze
/// filtering.
fn collect_dirty(store: &Store, scope: &Scope) -> Vec<Change> {
    let device = store.device_id().to_string();
    let state = st(store, scope);
    let mut changes: Vec<Change> = local_entities(store, scope)
        .into_iter()
        .filter(|(t, id, updated, _)| {
            let key = sync_key(*t, id);
            state.synced.get(&key) != Some(updated)
                && !is_skipped(store, scope, &key, *updated)
                && !frozen_on_server(store, scope, *t, id)
        })
        .map(|(t, id, updated_at, data)| {
            // An entity re-sent after a server restore keeps the device id of
            // the version it is, so the push is idempotent (contract C3).
            let key = sync_key(t, &id);
            let device_id = if is_resend(store, scope, &key, updated_at) {
                state.origins.get(&key).unwrap_or(&device).clone()
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
                updated_by: None,
            }
        })
        .collect();
    // Parents first so a partial push never strands children.
    changes.sort_by_key(|c| c.entity_type.depth());
    for t in &state.tombstones {
        changes.push(Change {
            entity_type: t.entity_type,
            id: t.id.clone(),
            updated_at: t.deleted_at,
            deleted: true,
            data: None,
            rev: None,
            device_id: Some(device.clone()),
            updated_by: None,
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

fn mark_skipped(
    store: &mut Store,
    scope: &Scope,
    t: EntityType,
    id: &str,
    updated_at: i64,
    reason: &str,
) {
    st_mut(store, scope).sync_skipped.insert(
        sync_key(t, id),
        crate::storage::SyncSkipped {
            updated_at,
            reason: reason.to_string(),
        },
    );
}

/// Entities currently excluded from token sync (their skipped version is
/// still the local one). Stale marks are pruned.
pub fn skipped_entities(store: &mut Store) -> Vec<SkippedEntity> {
    skipped_entities_in(store, LEGACY)
}

pub fn skipped_entities_in(store: &mut Store, scope: &Scope) -> Vec<SkippedEntity> {
    let marks: Vec<(String, crate::storage::SyncSkipped)> = st(store, scope)
        .sync_skipped
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = Vec::new();
    for (key, mark) in marks {
        let Some((t, id)) = parse_sync_key(&key) else {
            st_mut(store, scope).sync_skipped.remove(&key);
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
                st_mut(store, scope).sync_skipped.remove(&key);
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
    plan_push_in(store, LEGACY)
}

pub fn plan_push_in(store: &mut Store, scope: &Scope) -> Vec<Vec<Change>> {
    let mut sendable = Vec::new();
    let types = server_types_in(store, scope);
    for c in collect_push_in(store, scope) {
        if !type_accepted(&types, c.entity_type) {
            continue; // local-only until the server accepts the type
        }
        if !c.deleted && data_size(&c) > MAX_ENTITY_BYTES {
            mark_skipped(store, scope, c.entity_type, &c.id, c.updated_at, TOO_LARGE);
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

/// One pushed (sub-)batch with the server's answer.
#[derive(Debug, Clone)]
pub struct Pushed {
    pub sent: Vec<Change>,
    pub results: Vec<PushResult>,
    /// `serverTime` of the response (account spaces).
    pub server_time: Option<i64>,
}

/// Push one batch; on a whole-body 413 split it in halves and retry. A
/// single change that is still too large is reported as rejected
/// (`entity_too_large`) instead of failing the sync (contract C2).
pub async fn push_batch(
    client: &SyncClient,
    batch: Vec<Change>,
) -> Result<Vec<(Vec<Change>, Vec<PushResult>)>, SyncError> {
    Ok(push_batch_timed(client, batch)
        .await?
        .into_iter()
        .map(|p| (p.sent, p.results))
        .collect())
}

/// [`push_batch`], keeping each response's `serverTime`.
pub async fn push_batch_timed(
    client: &SyncClient,
    batch: Vec<Change>,
) -> Result<Vec<Pushed>, SyncError> {
    let mut out = Vec::new();
    let mut queue = vec![batch];
    while let Some(b) = queue.pop() {
        if b.is_empty() {
            continue;
        }
        match client.push(&b).await {
            Ok(resp) => out.push(Pushed {
                sent: b,
                results: resp.results,
                server_time: resp.server_time,
            }),
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
                out.push(Pushed {
                    sent: b,
                    results,
                    server_time: None,
                });
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
    /// Changes rejected with `clock_skew`: they stay dirty (and are
    /// restamped once the clock is fixed, 04 §6.9).
    pub clock_skew: usize,
    /// Results with a status this build doesn't know: left dirty.
    pub unknown: usize,
    /// Changes refused with `forbidden_role`: frozen for the banner.
    pub forbidden: usize,
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
    apply_push_results_in(store, LEGACY, pushed, results, None)
}

/// [`apply_push_results`] for `scope`, with the response's `serverTime`
/// (account spaces record it for the clock-skew restamp, 04 §6.9).
pub fn apply_push_results_in(
    store: &mut Store,
    scope: &Scope,
    pushed: &[Change],
    results: &[PushResult],
    server_time: Option<i64>,
) -> PushApplied {
    let mut out = PushApplied::default();
    let now = now_ms();
    store.record_server_time(scope, server_time, now);
    let mut forbidden_projects = 0;
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
            let mut types = server_types_in(store, scope);
            types.retain(|t| t != change.entity_type.as_str());
            let state = st_mut(store, scope);
            state.server_types = Some(types);
            state.server_checked_at = None;
            out.unsupported += 1;
            continue;
        }
        if r.status == PushStatus::Rejected
            && r.reason.as_deref() == Some(FORBIDDEN_ROLE)
            && store.account_mode()
        {
            // 04 §6.4: the change stays dirty (or the tombstone is kept) and
            // waits for the role-changed banner instead of being re-sent.
            let ss = store.space_sync_mut(scope);
            if !ss.frozen.contains(&key) {
                ss.frozen.push(key.clone());
            }
            if change.deleted && change.entity_type == EntityType::Project {
                forbidden_projects += 1;
            }
            out.forbidden += 1;
            out.rejected += 1;
            continue;
        }
        if r.status == PushStatus::Rejected && r.reason.as_deref() == Some(GLOBAL_ENV_NOT_ALLOWED) {
            // Only a buggy client sends this (B4): dropped silently.
            let state = st_mut(store, scope);
            if change.deleted {
                state
                    .tombstones
                    .retain(|t| !(t.id == change.id && t.entity_type == change.entity_type));
            } else {
                state.synced.insert(key.clone(), change.updated_at);
            }
            out.rejected += 1;
            continue;
        }
        match (change.deleted, r.status) {
            (true, PushStatus::Applied | PushStatus::Stale) => {
                let state = st_mut(store, scope);
                state
                    .tombstones
                    .retain(|t| !(t.id == change.id && t.entity_type == change.entity_type));
                state.synced.remove(&key);
                state.origins.remove(&key);
            }
            // Kept: the server still has the entity, so the delete is
            // pending until the server accepts it.
            (true, PushStatus::Rejected | PushStatus::Unknown) => {}
            (false, PushStatus::Applied) => {
                let resend = is_resend(store, scope, &key, change.updated_at);
                let state = st_mut(store, scope);
                state.synced.insert(key.clone(), change.updated_at);
                if !resend {
                    state.origins.remove(&key);
                }
                state.resend.remove(&key);
                state.sync_skipped.remove(&key);
                if store.account_mode() {
                    store.space_sync_mut(scope).skewed.remove(&key);
                }
            }
            (false, PushStatus::Rejected) if r.reason.as_deref() == Some(CLOCK_SKEW) => {
                // Left dirty and remembered for the restamp (04 §6.9).
                out.clock_skew += 1;
                if store.account_mode() {
                    let ss = store.space_sync_mut(scope);
                    ss.skewed.insert(key.clone(), change.updated_at);
                    // How far this device's clock (or the stamp) is ahead.
                    let ahead = server_time
                        .map(|t| (now - t).max(change.updated_at - t))
                        .unwrap_or(0);
                    ss.issue = Some(SpaceIssue::new(
                        "clockSkew",
                        crate::spaces::clock_skew_message(ahead),
                    ));
                }
            }
            (false, PushStatus::Rejected) => {
                mark_skipped(
                    store,
                    scope,
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
    if out.forbidden > 0
        && let Scope::Space(id) = scope
    {
        let name = store
            .binding(id)
            .map(|b| b.name())
            .unwrap_or_else(|| "this team".into());
        let frozen = store.space_sync(scope).frozen.len();
        let message = if forbidden_projects > 0 {
            format!("Only admins can delete projects in {name}. Discard to bring the project back.")
        } else {
            format!(
                "Your role in {name} doesn't allow {}. Save them as a personal copy or discard them.",
                if frozen == 1 {
                    "this change".to_string()
                } else {
                    format!("these {frozen} changes")
                }
            )
        };
        store.space_sync_mut(scope).issue = Some(SpaceIssue::new("roleChanged", message));
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
    /// Pulled changes ignored because the entity (or its parent) belongs to
    /// another space or to this device only (B2/B3/B5).
    pub foreign: usize,
    /// Deletes held back because another space's pull failed (B7).
    pub deferred: usize,
    /// Pulled changes skipped because their key waits for the role-changed
    /// banner (04 §6.8).
    pub frozen: usize,
    /// Containers deleted on the server that were kept because they hold
    /// newer unsynced edits (06 §10 M2).
    pub held: usize,
}

/// "“Payments API” was deleted in Acme. 2 unsynced changes in it are kept
/// until you decide."
pub fn held_delete_message(name: &str, space: &str, n: usize) -> String {
    let what = if name.is_empty() {
        "An item".to_string()
    } else {
        format!("\u{201c}{name}\u{201d}")
    };
    format!(
        "{what} was deleted in {space}. {} in it {} kept until you decide.",
        if n == 1 {
            "1 unsynced change".to_string()
        } else {
            format!("{n} unsynced changes")
        },
        if n == 1 { "is" } else { "are" }
    )
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

/// The scope a local entity currently syncs with (`None`: missing, or this
/// device only).
fn scope_of_local(store: &Store, t: EntityType, id: &str) -> Option<Scope> {
    use crate::storage::NodeKind;
    match t {
        EntityType::Project => store
            .projects
            .contains_key(id)
            .then(|| store.scope_of_project(id))
            .flatten(),
        EntityType::Collection => store
            .project_of(NodeKind::Collection, id)
            .ok()
            .and_then(|p| store.scope_of_project(&p)),
        EntityType::Folder => store
            .project_of(NodeKind::Folder, id)
            .ok()
            .and_then(|p| store.scope_of_project(&p)),
        EntityType::Request => store
            .project_of(NodeKind::Request, id)
            .ok()
            .and_then(|p| store.scope_of_project(&p)),
        EntityType::Environment => store.environment(id).and_then(|e| store.scope_of_env(e)),
    }
}

/// Whether `change` (an upsert pulled from `scope`) may be applied (B2–B4):
/// the parent named in the incoming data is bound to `scope`, and an entity
/// that already exists here is bound to `scope` too. A project unknown here
/// is accepted (it is new to this device and binds to `scope`), unless this
/// device bound that id elsewhere.
fn accepts_upsert(store: &Store, scope: &Scope, change: &Change) -> bool {
    use crate::storage::NodeKind;
    if !store.account_mode() {
        return true;
    }
    let data = change.data.as_ref();
    let field = |k: &str| data.and_then(|d| d.get(k)).and_then(Value::as_str);
    let parent_ok = |pid: Option<String>| pid.is_some_and(|p| store.in_scope(scope, &p));
    let existing_ok = || match local_meta(store, change.entity_type, &change.id) {
        None => true,
        Some(_) => scope_of_local(store, change.entity_type, &change.id).as_ref() == Some(scope),
    };
    match change.entity_type {
        EntityType::Project => match store.bindings.project_spaces.get(&change.id) {
            Some(s) => Scope::from_id(s) == *scope,
            None => !store.projects.contains_key(&change.id),
        },
        EntityType::Collection => {
            parent_ok(field("projectId").map(str::to_string)) && existing_ok()
        }
        EntityType::Folder | EntityType::Request => {
            let pid =
                field("collectionId").and_then(|c| store.project_of(NodeKind::Collection, c).ok());
            parent_ok(pid) && existing_ok()
        }
        EntityType::Environment => {
            let parent = match field("projectId") {
                Some(p) => store.in_scope(scope, p),
                None => store.global_in_scope(scope),
            };
            parent && existing_ok()
        }
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
            // The copy syncs where the original does.
            if let Some(space) = store.bindings.project_spaces.get(id).cloned() {
                store.bindings.project_spaces.insert(copy_id.clone(), space);
            }
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

pub(crate) fn remove_local(
    store: &mut Store,
    t: EntityType,
    id: &str,
) -> Vec<(EntityType, String)> {
    let mut removed = Vec::new();
    match t {
        EntityType::Project => {
            if let Some(tree) = store.forget_project(id) {
                store.bindings.project_spaces.remove(id);
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

/// Insert or replace an entity from remote data, within `scope`'s projects
/// only (the entity is never relocated out of another space's project, 04
/// §1.3). Returns an error when the parent is missing (orphan) or the data
/// is malformed.
fn upsert_remote(store: &mut Store, scope: &Scope, change: &Change) -> Result<(), String> {
    let data = change.data.clone().ok_or("missing data")?;
    let ws_id = store.workspace.id.clone();
    let err = |e: serde_json::Error| format!("malformed {}: {e}", change.entity_type.as_str());
    // Project ids of `scope` (every project in pure-legacy mode).
    let in_scope: std::collections::BTreeSet<String> = store
        .projects
        .keys()
        .filter(|p| store.in_scope(scope, p))
        .cloned()
        .collect();
    match change.entity_type {
        EntityType::Project => {
            let mut p: Project = serde_json::from_value(data).map_err(err)?;
            p.workspace_id = ws_id;
            p.updated_at = change.updated_at;
            let id = p.id.clone();
            match store.projects.get_mut(&p.id) {
                Some(t) => t.project = p,
                None => {
                    store.projects.insert(p.id.clone(), ProjectTree::new(p));
                    store.removed_projects.remove(&id);
                    // B1: a project new to this device binds to its space.
                    if store.account_mode() {
                        store
                            .bindings
                            .project_spaces
                            .insert(id, scope.id().to_string());
                    }
                }
            }
        }
        EntityType::Collection => {
            let mut c: Collection = serde_json::from_value(data).map_err(err)?;
            c.updated_at = change.updated_at;
            if !store.projects.contains_key(&c.project_id) {
                return Err(format!("collection {} has no project", c.id));
            }
            for (pid, t) in store.projects.iter_mut() {
                if in_scope.contains(pid) {
                    t.collections.retain(|x| x.id != c.id);
                }
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
            for (p, t) in store.projects.iter_mut() {
                if in_scope.contains(p) {
                    t.folders.retain(|x| x.id != f.id);
                }
            }
            if let Some(t) = store.projects.get_mut(&pid) {
                t.folders.push(f);
            }
        }
        EntityType::Request => {
            let mut r: Request = serde_json::from_value(data).map_err(err)?;
            r.updated_at = change.updated_at;
            // Team spaces never carry device-local paths (AC-TSYNC-8): keep
            // this device's file choices, as secrets are kept.
            if is_team_scope(store, scope)
                && let Some(local) = store.request(&r.id)
            {
                keep_local_paths(&mut r, local);
            }
            let pid = store
                .project_of(crate::storage::NodeKind::Collection, &r.collection_id)
                .map_err(|_| format!("request {} has no collection", r.id))?;
            for (p, t) in store.projects.iter_mut() {
                if in_scope.contains(p) {
                    t.requests.retain(|x| x.id != r.id);
                }
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
                    let was_synced = st(store, scope)
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

/// Keep `local`'s binary path and form file values where `r` (pulled from a
/// team space) has them blank.
fn keep_local_paths(r: &mut Request, local: &Request) {
    if let (Some(g), Some(lg)) = (r.grpc.as_mut(), local.grpc.as_ref()) {
        if g.proto_files.is_empty() {
            g.proto_files = lg.proto_files.clone();
        }
        if g.import_paths.is_empty() {
            g.import_paths = lg.import_paths.clone();
        }
    }
    let blank = |v: &Option<String>| v.as_deref().is_none_or(str::is_empty);
    if blank(&r.body.binary_path) && !blank(&local.body.binary_path) {
        r.body.binary_path = local.body.binary_path.clone();
    }
    if let (Some(form), Some(lform)) = (r.body.form.as_mut(), local.body.form.as_ref()) {
        for f in form
            .iter_mut()
            .filter(|f| f.kind == FormFieldKind::File && f.value.is_empty())
        {
            if let Some(l) = lform
                .iter()
                .find(|l| l.kind == FormFieldKind::File && l.key == f.key && !l.value.is_empty())
            {
                f.value = l.value.clone();
            }
        }
    }
}

/// How [`apply_pull_in`] treats one pull.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PullOptions {
    /// Another active space failed this run (B7): apply upserts only and
    /// keep the old cursor when the pull held deletes, so they are pulled
    /// again (after [`MAX_DEFERRED_RUNS`] such runs they apply anyway).
    pub defer_deletes: bool,
}

/// Apply pulled token-sync changes (parents first) with LWW + conflict
/// copies.
pub fn apply_pull(store: &mut Store, changes: Vec<Change>, cursor: u64) -> PullReport {
    apply_pull_in(store, LEGACY, changes, cursor, PullOptions::default())
}

/// Apply changes pulled from `scope` (B2–B5 and B7 in account mode).
pub fn apply_pull_in(
    store: &mut Store,
    scope: &Scope,
    mut changes: Vec<Change>,
    cursor: u64,
    opts: PullOptions,
) -> PullReport {
    let mut report = PullReport::default();
    let account = store.account_mode();
    let first_pull = st(store, scope).last_sync_at.is_none();
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
    // 04 §6.8: in a space this account can only read, the server's version
    // always wins and no conflict copy is made; keys frozen for the
    // role-changed banner (and held deletes) wait for the user's choice.
    let viewer = account && store.read_only_scope(scope);
    let frozen: std::collections::BTreeSet<String> = if account {
        let ss = store.space_sync(scope);
        ss.frozen.iter().chain(ss.held.iter()).cloned().collect()
    } else {
        Default::default()
    };

    let moving = if account {
        store.unfinished_move_ids()
    } else {
        Default::default()
    };
    let my_device = store.device_id().to_string();
    for change in &upserts {
        let key = sync_key(change.entity_type, &change.id);
        if !accepts_upsert(store, scope, change) {
            report.foreign += 1;
            continue;
        }
        if frozen.contains(&key) {
            report.frozen += 1;
            continue;
        }
        // The copy of a move that hasn't finished (its push landed, the
        // answer didn't): it arrives here with the move, not as a pull.
        if moving.contains(&change.id) {
            continue;
        }
        if account && let Some(by) = &change.updated_by {
            store
                .space_sync_mut(scope)
                .attribution
                .insert(key.clone(), by.clone());
        }
        let synced_at = st(store, scope).synced.get(&key).copied();
        let remote_device = change.device_id.clone().unwrap_or_default();
        match local_meta(store, change.entity_type, &change.id) {
            Some((local_updated, _)) => {
                let resend = is_resend(store, scope, &key, local_updated);
                let dirty = !viewer
                    && synced_at != Some(local_updated)
                    && !resend
                    && !read_only_entity(store, change.entity_type, &change.id);
                let local_device = if dirty {
                    my_device.clone()
                } else {
                    st(store, scope)
                        .origins
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| my_device.clone())
                };
                // LWW by (updatedAt, deviceId) — contract C3.
                let order = (change.updated_at, remote_device.as_str())
                    .cmp(&(local_updated, local_device.as_str()));
                if order == std::cmp::Ordering::Equal {
                    let state = st_mut(store, scope);
                    state.synced.insert(key.clone(), change.updated_at);
                    state.resend.remove(&key);
                    continue; // same version
                }
                // A local edit stamped by a clock that ran ahead, while the
                // server's version changed: never let the future timestamp
                // win. The remote version is applied and ours becomes a
                // conflict copy (06 §9 "restamp versus conflict copy").
                let skewed = account
                    && dirty
                    && store.space_sync(scope).skewed.contains_key(&key)
                    && synced_at != Some(change.updated_at);
                if !skewed && (dirty || resend) && order == std::cmp::Ordering::Less {
                    // A local edit wins, or the restored server lost our
                    // newer version: keep it, it will be pushed.
                    continue;
                }
                if skewed {
                    store.space_sync_mut(scope).skewed.remove(&key);
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
                if let Some(t) = st(store, scope)
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
                    st_mut(store, scope).tombstones.retain(|t| t.id != id);
                }
            }
        }
        match upsert_remote(store, scope, change) {
            Ok(()) => {
                // Merged Globals carry a fresh local updatedAt and stay dirty.
                let stored = local_meta(store, change.entity_type, &change.id).map(|m| m.0);
                let state = st_mut(store, scope);
                if stored == Some(change.updated_at) {
                    state.synced.insert(key.clone(), change.updated_at);
                    if remote_device.is_empty() || remote_device == my_device {
                        state.origins.remove(&key);
                    } else {
                        state.origins.insert(key.clone(), remote_device.clone());
                    }
                }
                state.sync_skipped.remove(&key);
                state.resend.remove(&key);
                report.applied += 1;
            }
            Err(e) => report.skipped.push(e),
        }
    }

    // B7: hold deletes back while another space failed this run.
    let defer = opts.defer_deletes
        && !deletes.is_empty()
        && (!account || store.space_sync(scope).deferred_runs < MAX_DEFERRED_RUNS);
    if defer {
        report.deferred = deletes.len();
    }
    // Deferred tombstones of this space's entities: nothing under them is
    // pushed until they apply, so a stale edit never reaches the server as
    // an orphan in the meantime (B7 with 06 §10 M2).
    let mut deferred_keys: Vec<String> = Vec::new();
    for change in &deletes {
        let key = sync_key(change.entity_type, &change.id);
        let Some((local_updated, _)) = local_meta(store, change.entity_type, &change.id) else {
            if !defer {
                let state = st_mut(store, scope);
                state.synced.remove(&key);
                state.tombstones.retain(|t| t.id != change.id);
            }
            continue;
        };
        // B5: a pulled tombstone only touches this space's entities.
        if account && scope_of_local(store, change.entity_type, &change.id).as_ref() != Some(scope)
        {
            if !defer {
                let state = st_mut(store, scope);
                state.synced.remove(&key);
                state.tombstones.retain(|t| t.id != change.id);
                report.foreign += 1;
            }
            continue;
        }
        if frozen.contains(&key) {
            report.frozen += 1;
            continue;
        }
        let dirty = !viewer
            && st(store, scope).synced.get(&key) != Some(&local_updated)
            && !read_only_entity(store, change.entity_type, &change.id);
        let remote_device = change.device_id.clone().unwrap_or_default();
        if dirty
            && (local_updated, my_device.as_str()) > (change.updated_at, remote_device.as_str())
        {
            continue; // local edit after the remote delete wins
        }
        // 06 §10 M2: a container deleted on the server that still holds
        // unsynced edits newer than the delete is kept (and nothing under it
        // is pushed, so no orphans reach the server) until the banner is
        // resolved. Older edits lose to the delete, as before.
        if account && change.entity_type != EntityType::Request {
            let newer: Vec<String> = descendants(store, change.entity_type, &change.id)
                .into_iter()
                .filter_map(|(t, id)| {
                    let k = sync_key(t, &id);
                    if frozen.contains(&k) {
                        return Some(k);
                    }
                    let (updated, _) = local_meta(store, t, &id)?;
                    let kid_dirty = !viewer
                        && st(store, scope).synced.get(&k) != Some(&updated)
                        && !read_only_entity(store, t, &id);
                    (kid_dirty
                        && (updated, my_device.as_str())
                            > (change.updated_at, remote_device.as_str()))
                        .then_some(k)
                })
                .collect();
            if !newer.is_empty() {
                let name = local_meta(store, change.entity_type, &change.id)
                    .map(|m| m.1)
                    .unwrap_or_default();
                let space = match scope {
                    Scope::Space(id) => store.binding(id).map(|b| b.name()),
                    Scope::Legacy => None,
                }
                .unwrap_or_else(|| "the workspace".into());
                let ss = store.space_sync_mut(scope);
                for k in newer {
                    if !ss.frozen.contains(&k) {
                        ss.frozen.push(k);
                    }
                }
                if !ss.held.contains(&key) {
                    ss.held.push(key.clone());
                }
                let n = ss.frozen.len();
                ss.issue = Some(SpaceIssue::new(
                    "heldDelete",
                    held_delete_message(&name, &space, n),
                ));
                report.held += 1;
                continue;
            }
        }
        if defer {
            // Held back (B7): evaluated above, applied on a later run.
            deferred_keys.push(key);
            continue;
        }
        for (t, id) in remove_local(store, change.entity_type, &change.id) {
            let k = sync_key(t, &id);
            let state = st_mut(store, scope);
            state.synced.remove(&k);
            state.origins.remove(&k);
            state.sync_skipped.remove(&k);
            state.resend.remove(&k);
            state.tombstones.retain(|x| x.id != id);
            if account {
                store.space_sync_mut(scope).attribution.remove(&k);
            }
        }
        report.deleted += 1;
    }

    // A fresh device connecting to an existing workspace: drop its empty,
    // never-synced "Default" project in favour of the pulled ones. In
    // account mode only on the first pull of Personal or a legacy space,
    // and only for a Default that is unbound or bound to this space.
    let personal_like = !account
        || *scope == Scope::Legacy
        || store
            .binding(scope.id())
            .is_some_and(|b| b.kind == crate::spaces::SpaceKind::Personal);
    if had_remote_projects && personal_like && (!account || first_pull) {
        let empties: Vec<String> = store
            .projects
            .values()
            .filter(|t| {
                t.collections.is_empty()
                    && t.project.name == "Default"
                    && !st(store, scope)
                        .synced
                        .contains_key(&sync_key(EntityType::Project, &t.project.id))
                    && !store
                        .environments
                        .iter()
                        .any(|e| e.project_id.as_deref() == Some(t.project.id.as_str()))
                    && (!account
                        || match store.bindings.project_spaces.get(&t.project.id) {
                            None => true,
                            Some(s) => Scope::from_id(s) == *scope,
                        })
            })
            .map(|t| t.project.id.clone())
            .collect();
        if empties.len() < store.projects.len() {
            for id in empties {
                store.forget_project(&id);
                store.bindings.project_spaces.remove(&id);
            }
        }
    }

    // Restore detection ran before this (see `SyncClient::pull_since`), so
    // the server's cursor is taken as is, even when it went down. Deferred
    // deletes keep the old cursor so they are pulled again.
    if !defer {
        store.set_scope_cursor(scope, cursor);
    }
    if account {
        let ss = store.space_sync_mut(scope);
        ss.deferred_deletes = deferred_keys;
        ss.deferred_runs = if defer { ss.deferred_runs + 1 } else { 0 };
        ss.foreign += report.foreign as u64;
    }
    st_mut(store, scope).last_sync_at = Some(now_ms());
    report
}

/// Keep pulled changes of unknown `type` in sync state (AC-FWD-5), latest
/// per `(type, id)`, so a newer build can apply them later. Returns how many
/// were remembered.
pub fn remember_unknown(store: &mut Store, unknown: Vec<Value>) -> usize {
    remember_unknown_in(store, LEGACY, unknown)
}

pub fn remember_unknown_in(store: &mut Store, scope: &Scope, unknown: Vec<Value>) -> usize {
    let n = unknown.len();
    if n == 0 {
        return 0;
    }
    let ident = |v: &Value| {
        let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        (s("type"), s("id"))
    };
    let rev = |x: &Value| x.get("rev").and_then(Value::as_u64).unwrap_or(0);
    let state = st_mut(store, scope);
    let list = std::mem::take(&mut state.unknown_changes);
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
    state.unknown_changes = slots.into_iter().flatten().collect();
    cap_unknown(state);
    n
}

/// Keep [`SyncState::unknown_changes`] within [`MAX_UNKNOWN_CHANGES`] and
/// [`MAX_UNKNOWN_BYTES`], dropping the oldest (lowest `rev`) first and
/// noting that something was dropped.
fn cap_unknown(state: &mut SyncState) {
    let list = &mut state.unknown_changes;
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
    state.unknown_changes_truncated = true;
}

/// The server was restored from a backup (06-go-server §9): forget what it
/// is known to hold and re-send everything. Entities that were clean are
/// remembered in [`SyncState::resend`] so a re-pulled equal version is not a
/// conflict and an older one never overwrites them. LWW keeps this
/// idempotent.
pub fn mark_restored(store: &mut Store) {
    mark_restored_in(store, LEGACY)
}

/// [`mark_restored`] for every entity bound to `scope`.
pub fn mark_restored_in(store: &mut Store, scope: &Scope) {
    let state = st(store, scope);
    let clean: Vec<(String, i64)> = local_entities(store, scope)
        .into_iter()
        .map(|(t, id, updated, _)| (sync_key(t, &id), updated))
        .filter(|(key, updated)| {
            state.synced.get(key) == Some(updated) || state.resend.get(key) == Some(updated)
        })
        .collect();
    let state = st_mut(store, scope);
    state.resend = clean.into_iter().collect();
    state.synced.clear();
    state.unknown_changes.clear();
    state.unknown_changes_truncated = false;
    store.set_scope_cursor(scope, 0);
}

/// [`mark_restored`] when the server was restored, the epoch, then
/// [`remember_unknown`] and [`apply_pull`] for one pull.
pub fn apply_pulled(store: &mut Store, pulled: Pulled) -> PullReport {
    apply_pulled_in(store, LEGACY, pulled, PullOptions::default())
}

pub fn apply_pulled_in(
    store: &mut Store,
    scope: &Scope,
    pulled: Pulled,
    opts: PullOptions,
) -> PullReport {
    if pulled.restored {
        mark_restored_in(store, scope);
    }
    if let Some(e) = pulled.sync_epoch.filter(|e| !e.is_empty()) {
        st_mut(store, scope).sync_epoch = Some(e);
    }
    let unknown = remember_unknown_in(store, scope, pulled.unknown);
    let mut report = apply_pull_in(store, scope, pulled.changes, pulled.cursor, opts);
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
    pull_start_in(store, LEGACY)
}

pub fn pull_start_in(store: &Store, scope: &Scope) -> Result<PullStart, SyncError> {
    let since = store.scope_cursor(scope).ok_or(SyncError::NotConnected)?;
    Ok(PullStart {
        since,
        epoch: st(store, scope).sync_epoch.clone(),
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
    let scope = client.scope().clone();
    push_all_in(client, store, &scope).await
}

pub async fn push_all_in(
    client: &SyncClient,
    store: &mut Store,
    scope: &Scope,
) -> Result<usize, SyncError> {
    let mut applied = 0;
    for batch in plan_push_in(store, scope) {
        for p in push_batch_timed(client, batch).await? {
            applied +=
                apply_push_results_in(store, scope, &p.sent, &p.results, p.server_time).applied;
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
    let scope = client.scope().clone();
    let start = pull_start_in(store, &scope)?;
    let pulled = pull_from(client, &start, server).await?;
    let report = apply_pulled_in(store, &scope, pulled, PullOptions::default());
    store.save_all()?;
    Ok(report)
}

/// Full cycle for one scope: discovery (when due), pull (LWW + conflict
/// copies), then push local changes. The client's scope decides which
/// bookkeeping is used.
pub async fn sync_now(client: &SyncClient, store: &mut Store) -> Result<SyncReport, SyncError> {
    let scope = client.scope().clone();
    if store.scope_cursor(&scope).is_none() {
        return Err(SyncError::NotConnected);
    }
    let info = refresh_server_info(client, server_info_due_in(store, &scope, now_ms())).await;
    if let Some(info) = &info {
        record_server_info_in(store, &scope, info.as_ref());
    }
    // Pull first so conflicts are resolved locally (and a freshly connected
    // device adopts the remote tree) before anything is pushed.
    let report = pull_with_info(client, store, info.as_ref().and_then(Option::as_ref)).await?;
    store.restamp_skewed(&scope, now_ms());
    let pushed = push_all_in(client, store, &scope).await?;
    Ok(SyncReport {
        pushed,
        pulled: report.applied,
        deleted: report.deleted,
        conflicts: report.conflicts,
        skipped: report.skipped,
        too_large: skipped_entities_in(store, &scope),
        unsupported: unsupported_entities_in(store, &scope),
        restored: report.restored,
        cursor: store.scope_cursor(&scope).unwrap_or(0),
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

/// The token-sync client: `workspace.sync` in pure-legacy mode, or the
/// legacy binding and its token (from the secret store) in account mode.
pub fn client_for(store: &Store) -> Result<SyncClient, SyncError> {
    if store.account_mode() {
        let link = store
            .bindings
            .legacy
            .as_ref()
            .ok_or(SyncError::NotConnected)?;
        let token = store
            .legacy_token
            .as_deref()
            .ok_or(SyncError::NotConnected)?;
        return SyncClient::new(&link.worker_url, Some(token));
    }
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

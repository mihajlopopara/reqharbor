//! One sync run in account mode (04 §6.6), free of Tauri so it is tested
//! against a mock server:
//!
//! 1. **Reconcile:** `GET /v1/me/spaces` adds bindings for new spaces of the
//!    signed-in account, refreshes roles and usage, and marks a space
//!    Removed only when that authoritative list lacks it. Bindings of another
//!    server, instance or user are paused (06 §9).
//! 2. **Pull** every syncable scope in order (personal → legacy → teams),
//!    within one time budget. Pages are held in memory.
//! 3. **Apply** under the store lock with B2–B7: when any scope failed
//!    transiently, the others apply upserts only and keep their cursor if
//!    the pull held deletes.
//! 4. **Push** each scope that pulled, restamping clock-skew rejects once the
//!    clock is fixed (04 §6.9).
//!
//! The store lock is only held for the local steps, never across a request.

use crate::auth::{AuthError, Session};
use crate::model::now_ms;
use crate::spaces::{
    CLIENT_TOO_OLD_MESSAGE, LEGACY_DISABLED_MESSAGE, LegacyMode, Role, SERVER_FULL_MESSAGE, Scope,
    SpaceIssue, SpaceKind, quota_message,
};
use crate::storage::Store;
use crate::sync::{
    self, PullOptions, Pulled, ServerInfo, SkippedEntity, SyncClient, SyncError, SyncReport,
};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Overall pull budget per run (04 §6.6 step 2, edge case 8).
pub const RUN_BUDGET: Duration = Duration::from_secs(30);

/// Short-lived access to the shared store.
pub trait StoreAccess {
    fn with<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R;
}

impl StoreAccess for Mutex<Store> {
    fn with<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        let mut g = self.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut g)
    }
}

/// The outcome of one scope in a run.
#[derive(Debug, Clone, Default)]
pub struct ScopeRun {
    pub space_id: String,
    pub error: Option<SyncError>,
    pub report: SyncReport,
}

/// The outcome of one account-mode run.
#[derive(Debug, Clone, Default)]
pub struct AccountRun {
    /// Every scope's report, merged.
    pub report: SyncReport,
    pub scopes: Vec<ScopeRun>,
    /// The session ended (AC-AUTH-9): account spaces are skipped until the
    /// user signs in again; token sync keeps going.
    pub sign_in_required: bool,
    /// `/v1/me/spaces` failed (not for sign-in reasons).
    pub reconcile_error: Option<SyncError>,
}

impl AccountRun {
    /// The error that decides the run's state: a retryable one first (so
    /// the run is retried with backoff), then any other.
    pub fn error(&self) -> Option<&SyncError> {
        let all = self
            .reconcile_error
            .iter()
            .chain(self.scopes.iter().filter_map(|s| s.error.as_ref()));
        let mut first = None;
        for e in all {
            if e.is_retryable() {
                return Some(e);
            }
            first.get_or_insert(e);
        }
        first
    }

    /// Any change other devices made arrived.
    pub fn changed(&self) -> bool {
        self.report.pulled > 0
            || self.report.deleted > 0
            || !self.report.conflicts.is_empty()
            || self.report.restored
    }
}

fn client_for_scope(
    store: &Store,
    scope: &Scope,
    session: Option<&Arc<Session>>,
) -> Option<SyncClient> {
    match scope {
        Scope::Legacy => sync::client_for(store).ok(),
        Scope::Space(id) => space_client(store, id, session?).ok(),
    }
}

/// The client of the account space `space_id`, for sync and shares alike.
/// Only an active binding of the signed-in account on the session's own
/// server gets one (06 §9): a project of server A never talks to server B,
/// and B's tokens never go to A. Otherwise [`SyncError::NotConnected`].
pub fn space_client(
    store: &Store,
    space_id: &str,
    session: &Arc<Session>,
) -> Result<SyncClient, SyncError> {
    let b = store.binding(space_id).ok_or(SyncError::NotConnected)?;
    let same_origin = |a: &str, b: &str| a.trim_end_matches('/') == b.trim_end_matches('/');
    if !store.scope_syncable(&Scope::Space(space_id.to_string()))
        || !same_origin(&b.server_origin, session.origin())
    {
        return Err(SyncError::NotConnected);
    }
    SyncClient::for_space(&b.server_origin, space_id, session.clone())
}

/// Record a scope's failure as its issue (and the binding's state).
fn note_error(store: &mut Store, scope: &Scope, err: &SyncError) {
    let kind = match scope {
        Scope::Legacy => SpaceKind::Legacy,
        Scope::Space(id) => store.binding(id).map(|b| b.kind).unwrap_or(SpaceKind::Team),
    };
    let quota = match scope {
        Scope::Space(id) => store.binding(id).and_then(|b| b.quota_bytes),
        Scope::Legacy => None,
    };
    let issue = match err {
        SyncError::SpaceGone(_) => {
            if let Scope::Space(id) = scope {
                store.mark_unavailable(id);
            }
            Some(SpaceIssue::new(
                "unavailable",
                "Temporarily unavailable · Retrying",
            ))
        }
        SyncError::QuotaExceeded(_) => Some(SpaceIssue::new("quota", quota_message(kind, quota))),
        SyncError::ServerFull(_) => Some(SpaceIssue::new("serverFull", SERVER_FULL_MESSAGE)),
        SyncError::ClientTooOld(_) => Some(SpaceIssue::new("clientTooOld", CLIENT_TOO_OLD_MESSAGE)),
        SyncError::Rejected(m) if m == LEGACY_DISABLED_MESSAGE => {
            Some(SpaceIssue::new("legacyDisabled", LEGACY_DISABLED_MESSAGE))
        }
        SyncError::Offline(_) | SyncError::Server(_) | SyncError::RateLimited(_) => None,
        SyncError::SignInRequired => None,
        other => Some(SpaceIssue::new("error", other.to_string())),
    };
    if store.account_mode()
        && let Some(i) = issue
    {
        store.space_sync_mut(scope).issue = Some(i);
    }
}

type PullResult = Result<(Option<Option<ServerInfo>>, Pulled), SyncError>;

/// Discovery (when due) then the pull, for one scope.
async fn pull_scope(client: &SyncClient, due: bool, start: sync::PullStart) -> PullResult {
    let info = sync::refresh_server_info(client, due).await;
    let pulled = sync::pull_from(client, &start, info.as_ref().and_then(Option::as_ref)).await?;
    Ok((info, pulled))
}

/// Push one scope; restamp first when the clock is fixed, and once more
/// when this push showed it fixed while entities were still held back.
async fn push_scope(
    store: &impl StoreAccess,
    scope: &Scope,
    client: &SyncClient,
) -> Result<usize, SyncError> {
    let mut pushed = 0;
    for round in 0..2 {
        let restamped = store.with(|s| s.restamp_skewed(scope, now_ms()));
        if round == 1 && restamped == 0 {
            break;
        }
        let batches = store.with(|s| sync::plan_push_in(s, scope));
        let mut skew = 0;
        for batch in batches {
            for p in sync::push_batch_timed(client, batch).await? {
                store.with(|s| {
                    let r =
                        sync::apply_push_results_in(s, scope, &p.sent, &p.results, p.server_time);
                    pushed += r.applied;
                    skew += r.clock_skew;
                });
            }
        }
        store.with(|s| s.save_sync_state())?;
        if skew == 0 || !store.with(|s| s.clock_ok(scope)) {
            break;
        }
    }
    Ok(pushed)
}

/// One account-mode run (04 §6.6). Never fails as a whole: each scope's
/// error is in [`AccountRun::scopes`].
pub async fn run(
    store: &impl StoreAccess,
    session: Option<Arc<Session>>,
    budget: Duration,
) -> AccountRun {
    let mut out = AccountRun::default();
    let has_account = store.with(|s| {
        s.pause_foreign();
        // Notices closed with × for 7 days get their default (UX §6.9).
        s.apply_overdue_notices(now_ms());
        s.account.is_some()
    });

    // 1. Reconcile.
    let mut account_ok = false;
    if has_account {
        match &session {
            Some(sess) if !sess.sign_in_required() => match sess.spaces().await {
                Ok(list) => {
                    store.with(|s| {
                        if s.apply_my_spaces(&list) {
                            let _ = s.save_bindings();
                        }
                    });
                    account_ok = true;
                }
                Err(AuthError::SignInRequired) => out.sign_in_required = true,
                Err(e) => out.reconcile_error = Some(SyncError::from(e)),
            },
            _ => out.sign_in_required = true,
        }
    }

    // 2. Pull.
    let targets: Vec<(Scope, SyncClient)> = store.with(|s| {
        s.sync_scopes()
            .into_iter()
            .filter(|sc| *sc == Scope::Legacy || account_ok)
            .filter_map(|sc| client_for_scope(s, &sc, session.as_ref()).map(|c| (sc, c)))
            .collect()
    });
    let deadline = tokio::time::Instant::now() + budget;
    let mut pulls: Vec<(Scope, SyncClient, PullResult)> = Vec::new();
    for (scope, client) in targets {
        let (due, start) = store.with(|s| {
            (
                sync::server_info_due_in(s, &scope, now_ms()),
                sync::pull_start_in(s, &scope),
            )
        });
        let res = match start {
            Err(e) => Err(e),
            Ok(start) => {
                match tokio::time::timeout_at(deadline, pull_scope(&client, due, start)).await {
                    Ok(r) => r,
                    Err(_) => Err(SyncError::Offline("timed out".into())),
                }
            }
        };
        pulls.push((scope, client, res));
    }

    // 3. Apply (one lock, no await).
    let transient = out
        .reconcile_error
        .as_ref()
        .is_some_and(SyncError::is_retryable)
        || pulls
            .iter()
            .any(|(_, _, r)| r.as_ref().err().is_some_and(SyncError::is_retryable));
    let mut ok: Vec<(Scope, SyncClient)> = Vec::new();
    store.with(|s| {
        for (scope, client, res) in pulls {
            let mut run = ScopeRun {
                space_id: scope.id().to_string(),
                ..Default::default()
            };
            match res {
                Ok((info, pulled)) => {
                    if let Some(info) = &info {
                        sync::record_server_info_in(s, &scope, info.as_ref());
                    }
                    if s.account_mode() {
                        let ss = s.space_sync_mut(&scope);
                        if ss.skewed.is_empty() {
                            ss.issue = None;
                        }
                    }
                    let r = sync::apply_pulled_in(
                        s,
                        &scope,
                        pulled,
                        PullOptions {
                            defer_deletes: transient,
                        },
                    );
                    run.report.pulled = r.applied;
                    run.report.deleted = r.deleted;
                    run.report.conflicts = r.conflicts;
                    run.report.skipped = r.skipped;
                    run.report.restored = r.restored;
                    ok.push((scope, client));
                }
                Err(e) => {
                    if e == SyncError::SignInRequired {
                        out.sign_in_required = true;
                    }
                    note_error(s, &scope, &e);
                    run.error = Some(e);
                }
            }
            out.scopes.push(run);
        }
        let _ = s.save_all();
    });

    // 4. Push.
    for (scope, client) in ok {
        let result = push_scope(store, &scope, &client).await;
        let idx = out
            .scopes
            .iter()
            .position(|r| r.space_id == scope.id())
            .unwrap_or(0);
        match result {
            Ok(n) => out.scopes[idx].report.pushed = n,
            Err(e) => {
                if e == SyncError::SignInRequired {
                    out.sign_in_required = true;
                }
                store.with(|s| note_error(s, &scope, &e));
                out.scopes[idx].error = Some(e);
            }
        }
        store.with(|s| {
            let r = &mut out.scopes[idx].report;
            r.too_large = sync::skipped_entities_in(s, &scope);
            r.unsupported = sync::unsupported_entities_in(s, &scope);
            r.cursor = s.scope_cursor(&scope).unwrap_or(0);
        });
    }
    store.with(|s| {
        let _ = s.save_sync_state();
    });

    // Merge.
    for r in &out.scopes {
        let m = &mut out.report;
        m.pushed += r.report.pushed;
        m.pulled += r.report.pulled;
        m.deleted += r.report.deleted;
        m.conflicts.extend(r.report.conflicts.iter().cloned());
        m.skipped.extend(r.report.skipped.iter().cloned());
        m.too_large.extend(r.report.too_large.iter().cloned());
        m.unsupported.extend(r.report.unsupported.iter().cloned());
        m.restored |= r.report.restored;
    }
    out.report.cursor = out.scopes.first().map(|r| r.report.cursor).unwrap_or(0);
    out
}

// ───────────────────────────── status ─────────────────────────────

/// One line of the per-space status (UX §6.6 tooltip, §11 Settings › Sync).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceStatus {
    pub space_id: String,
    /// `personal` | `team` | `legacy`.
    pub kind: String,
    pub name: String,
    pub role: Option<Role>,
    /// `active` | `paused` | `unavailable` | `removed` | `signedOut` |
    /// `sharesOnly`.
    pub state: String,
    /// Why it is paused (`foreignAccount`) or removed (`removed`, `left`…).
    pub reason: Option<String>,
    pub issue: Option<SpaceIssue>,
    pub last_sync_at: Option<i64>,
    pub pending_changes: usize,
    pub project_count: usize,
    pub bytes_used: Option<u64>,
    pub quota_bytes: Option<u64>,
    /// Items held back because the server doesn't take their type yet.
    pub unsupported: usize,
    /// The account that bound the space (for "synced by priya@acme.dev").
    pub account_email: String,
}

/// The token-sync workspace in account mode (UX §11 "LEGACY WORKSPACE").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyStatus {
    pub worker_url: String,
    pub remote_workspace_id: String,
    /// `sync` | `sharesOnly`.
    pub mode: String,
    pub project_count: usize,
    /// The token could not be read from the secret store.
    pub token_missing: bool,
}

/// Per-space statuses of an account-mode store, personal first.
pub fn space_statuses(store: &Store) -> Vec<SpaceStatus> {
    if !store.account_mode() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut bindings: Vec<_> = store.bindings.spaces.iter().collect();
    bindings.sort_by_key(|b| (b.kind != SpaceKind::Personal, b.name()));
    for b in bindings {
        let scope = Scope::Space(b.space_id.clone());
        let ss = store.space_sync(&scope);
        let (state, reason) = match (&store.account, &b.state) {
            (None, _) => ("signedOut".to_string(), None),
            (Some(who), _) if !b.belongs_to(who) => (
                "paused".to_string(),
                Some(crate::spaces::PAUSED_FOREIGN.to_string()),
            ),
            (_, crate::spaces::BindingState::Paused { reason }) => {
                ("paused".into(), Some(reason.clone()))
            }
            (_, crate::spaces::BindingState::Removed { reason, .. }) => {
                ("removed".into(), Some(reason.clone()))
            }
            (_, s) => (s.name().to_string(), None),
        };
        let syncable = store.scope_syncable(&scope);
        out.push(SpaceStatus {
            space_id: b.space_id.clone(),
            kind: match b.kind {
                SpaceKind::Personal => "personal".into(),
                _ => "team".into(),
            },
            name: b.name(),
            role: b.role,
            state,
            reason,
            issue: ss.issue.clone(),
            last_sync_at: ss.state.last_sync_at,
            pending_changes: if syncable {
                sync::pending_count_in(store, &scope)
            } else {
                0
            },
            project_count: store.projects_in(&b.space_id).len(),
            bytes_used: b.bytes_used,
            quota_bytes: b.quota_bytes,
            unsupported: if syncable {
                sync::unsupported_entities_in(store, &scope).len()
            } else {
                0
            },
            account_email: b.user_email.clone(),
        });
    }
    if let Some(l) = &store.bindings.legacy {
        let scope = Scope::Legacy;
        let ss = store.space_sync(&scope);
        let syncable = store.scope_syncable(&scope);
        out.push(SpaceStatus {
            space_id: crate::spaces::LEGACY_SPACE.into(),
            kind: "legacy".into(),
            name: "Legacy".into(),
            role: None,
            state: match l.mode {
                LegacyMode::SharesOnly => "sharesOnly".into(),
                LegacyMode::Sync if store.legacy_token.is_none() => "paused".into(),
                LegacyMode::Sync => "active".into(),
            },
            reason: None,
            issue: ss.issue.clone(),
            last_sync_at: ss.state.last_sync_at,
            pending_changes: if syncable {
                sync::pending_count_in(store, &scope)
            } else {
                0
            },
            project_count: store.projects_in(crate::spaces::LEGACY_SPACE).len(),
            bytes_used: None,
            quota_bytes: None,
            unsupported: if syncable {
                sync::unsupported_entities_in(store, &scope).len()
            } else {
                0
            },
            account_email: String::new(),
        });
    }
    out
}

pub fn legacy_status(store: &Store) -> Option<LegacyStatus> {
    let l = store.bindings.legacy.as_ref()?;
    Some(LegacyStatus {
        worker_url: l.worker_url.clone(),
        remote_workspace_id: l.remote_workspace_id.clone(),
        mode: match l.mode {
            LegacyMode::Sync => "sync".into(),
            LegacyMode::SharesOnly => "sharesOnly".into(),
        },
        project_count: store.projects_in(crate::spaces::LEGACY_SPACE).len(),
        token_missing: store.legacy_token.is_none(),
    })
}

/// Changes waiting in every syncable scope.
pub fn pending_total(store: &Store) -> usize {
    store
        .sync_scopes()
        .iter()
        .map(|s| sync::pending_count_in(store, s))
        .sum()
}

/// Too-large / rejected items of every syncable scope.
pub fn skipped_total(store: &mut Store) -> Vec<SkippedEntity> {
    let scopes = store.sync_scopes();
    scopes
        .iter()
        .flat_map(|s| sync::skipped_entities_in(store, s))
        .collect()
}

/// Items held back because a server doesn't take their type yet.
pub fn unsupported_total(store: &Store) -> Vec<SkippedEntity> {
    store
        .sync_scopes()
        .iter()
        .flat_map(|s| sync::unsupported_entities_in(store, s))
        .collect()
}

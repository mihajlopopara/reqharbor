//! Pure sync-status state machine (AC-SYNC-2/6/7, contracts C2/C4), kept
//! free of Tauri types so it is unit-tested. `sync_rt` applies the actions.

use reqharbor_core::storage::Store;
use reqharbor_core::sync::{self, SkippedEntity, SyncError};
use serde::Serialize;
use std::time::Duration;

/// Minimum interval between focus-triggered pulls (AC-SYNC-2).
pub const FOCUS_PULL_INTERVAL_MS: i64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SyncState {
    /// Not configured (sync indicator hidden).
    Disconnected,
    Idle,
    Syncing,
    /// Network failure or 5xx; retrying with backoff.
    Offline,
    /// 401: sync was disconnected; prompt for a new token.
    Unauthorized,
    /// Other failure (validation, protocol); not retried automatically.
    Error,
}

/// Payload of `get_sync_status` and the `sync://status` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncStatus {
    pub state: SyncState,
    pub connected: bool,
    pub worker_url: Option<String>,
    pub remote_workspace_id: Option<String>,
    pub last_sync_at: Option<i64>,
    /// Unix ms of the last successful pull (focus pulls are skipped < 60 s).
    pub last_pull_at: Option<i64>,
    pub last_error: Option<String>,
    /// Unix ms of the next automatic retry, when offline.
    pub next_retry_at: Option<i64>,
    pub retry_attempt: u32,
    /// Local changes waiting to be pushed.
    pub pending_changes: usize,
    /// Entities not synced because they exceed 512 KB (contract C2).
    pub skipped_entities: Vec<SkippedEntity>,
    /// Non-fatal warning for the sync indicator tooltip, e.g. skipped items.
    pub warning: Option<String>,
}

impl SyncStatus {
    pub fn from_store(store: &mut Store) -> Self {
        let cfg = store.workspace.sync.clone();
        let skipped = if cfg.is_some() {
            sync::skipped_entities(store)
        } else {
            Vec::new()
        };
        SyncStatus {
            state: if cfg.is_some() {
                SyncState::Idle
            } else {
                SyncState::Disconnected
            },
            connected: cfg.is_some(),
            worker_url: cfg.as_ref().map(|c| c.worker_url.clone()),
            remote_workspace_id: cfg.as_ref().map(|c| c.remote_workspace_id.clone()),
            last_sync_at: store.sync_state.last_sync_at,
            last_pull_at: None,
            last_error: None,
            next_retry_at: None,
            retry_attempt: 0,
            pending_changes: if cfg.is_some() {
                sync::collect_push(store).len()
            } else {
                0
            },
            warning: skipped_warning(&skipped),
            skipped_entities: skipped,
        }
    }
}

/// "2 items are too large to sync (over 512 KB): Big upload, Fixtures".
pub fn skipped_warning(skipped: &[SkippedEntity]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let names: Vec<&str> = skipped.iter().map(|s| s.name.as_str()).collect();
    let noun = if skipped.len() == 1 {
        "item is"
    } else {
        "items are"
    };
    Some(format!(
        "{} {noun} too large to sync (over 512 KB): {}",
        skipped.len(),
        names.join(", ")
    ))
}

/// Focus pull at most once per minute, never while syncing (AC-SYNC-2).
pub fn should_pull_on_focus(s: &SyncStatus, now: i64) -> bool {
    s.connected
        && s.state != SyncState::Syncing
        && !s
            .last_pull_at
            .is_some_and(|t| now - t < FOCUS_PULL_INTERVAL_MS)
}

pub fn begin(s: &mut SyncStatus) {
    s.state = SyncState::Syncing;
    s.connected = true;
}

pub fn on_success(s: &mut SyncStatus, now: i64, pending: usize, skipped: Vec<SkippedEntity>) {
    s.state = SyncState::Idle;
    s.connected = true;
    s.last_sync_at = Some(now);
    s.last_pull_at = Some(now);
    s.last_error = None;
    s.next_retry_at = None;
    s.retry_attempt = 0;
    s.pending_changes = pending;
    s.warning = skipped_warning(&skipped);
    s.skipped_entities = skipped;
}

/// What the runtime must do after a failed sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureAction {
    /// 401 with the current token: disconnect sync, keep local data (AC-SYNC-7).
    Disconnect,
    /// Schedule one retry after the delay (AC-SYNC-6). `Duration::ZERO` = now.
    Retry(Duration),
    /// Not retried automatically (shown as an error).
    Stop,
}

pub const UNAUTHORIZED_MESSAGE: &str = "Token rejected — sync disconnected. Enter a new token.";

/// Update the status for a failure. `token_rotated` is true when the token
/// in the store changed while this sync ran (a 401 for the old token must not
/// disconnect, contract C4).
pub fn on_failure(
    s: &mut SyncStatus,
    err: &SyncError,
    now: i64,
    token_rotated: bool,
) -> FailureAction {
    match err {
        SyncError::Unauthorized if token_rotated => {
            s.state = SyncState::Idle;
            s.next_retry_at = Some(now);
            FailureAction::Retry(Duration::ZERO)
        }
        SyncError::Unauthorized => {
            s.state = SyncState::Unauthorized;
            s.connected = false;
            s.last_error = Some(UNAUTHORIZED_MESSAGE.into());
            s.next_retry_at = None;
            s.retry_attempt = 0;
            FailureAction::Disconnect
        }
        e if e.is_retryable() => {
            let delay = sync::backoff_delay(s.retry_attempt);
            s.state = SyncState::Offline;
            s.last_error = Some(e.to_string());
            s.next_retry_at = Some(now + delay.as_millis() as i64);
            s.retry_attempt = s.retry_attempt.saturating_add(1);
            FailureAction::Retry(delay)
        }
        e => {
            s.state = SyncState::Error;
            s.last_error = Some(e.to_string());
            s.next_retry_at = None;
            FailureAction::Stop
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqharbor_core::model::EntityType;

    fn connected() -> SyncStatus {
        SyncStatus {
            state: SyncState::Idle,
            connected: true,
            worker_url: Some("https://w".into()),
            remote_workspace_id: Some("r".into()),
            last_sync_at: None,
            last_pull_at: None,
            last_error: None,
            next_retry_at: None,
            retry_attempt: 0,
            pending_changes: 0,
            skipped_entities: vec![],
            warning: None,
        }
    }

    #[test]
    fn ac_sync_2_focus_pull_at_most_once_per_minute() {
        let mut s = connected();
        assert!(should_pull_on_focus(&s, 1_000));
        on_success(&mut s, 1_000, 0, vec![]);
        assert!(!should_pull_on_focus(&s, 30_000));
        assert!(should_pull_on_focus(&s, 61_001));
        begin(&mut s);
        assert!(!should_pull_on_focus(&s, 200_000), "not while syncing");
        let mut off = connected();
        off.connected = false;
        assert!(!should_pull_on_focus(&off, 0));
    }

    #[test]
    fn ac_sync_6_backoff_5s_doubling_to_5min_then_reset() {
        let mut s = connected();
        let err = SyncError::Offline("down".into());
        let mut delays = Vec::new();
        for _ in 0..9 {
            match on_failure(&mut s, &err, 0, false) {
                FailureAction::Retry(d) => delays.push(d.as_secs()),
                other => panic!("{other:?}"),
            }
        }
        assert_eq!(delays, vec![5, 10, 20, 40, 80, 160, 300, 300, 300]);
        assert_eq!(s.state, SyncState::Offline);
        assert_eq!(s.next_retry_at, Some(300_000));
        assert!(matches!(
            on_failure(&mut s, &SyncError::Server("503".into()), 0, false),
            FailureAction::Retry(_)
        ));
        on_success(&mut s, 5, 0, vec![]);
        assert_eq!(s.retry_attempt, 0);
        assert_eq!(s.state, SyncState::Idle);
        assert_eq!(s.next_retry_at, None);
    }

    #[test]
    fn ac_sync_7_401_disconnects_unless_token_was_rotated() {
        let mut s = connected();
        assert_eq!(
            on_failure(&mut s, &SyncError::Unauthorized, 0, false),
            FailureAction::Disconnect
        );
        assert_eq!(s.state, SyncState::Unauthorized);
        assert!(!s.connected);
        assert_eq!(s.last_error.as_deref(), Some(UNAUTHORIZED_MESSAGE));

        // QA-R1-007 / C4: a 401 for a token that was rotated meanwhile retries.
        let mut s = connected();
        assert_eq!(
            on_failure(&mut s, &SyncError::Unauthorized, 7, true),
            FailureAction::Retry(Duration::ZERO)
        );
        assert!(s.connected);
        assert_ne!(s.state, SyncState::Unauthorized);
    }

    #[test]
    fn quota_exceeded_stops_without_retry_loop() {
        let mut s = connected();
        let e = SyncError::QuotaExceeded("quota_exceeded: 50 MB".into());
        assert_eq!(on_failure(&mut s, &e, 0, false), FailureAction::Stop);
        assert_eq!(s.state, SyncState::Error);
        assert!(s.last_error.as_deref().unwrap().contains("quota"));
    }

    #[test]
    fn other_errors_stop_without_retry() {
        let mut s = connected();
        assert_eq!(
            on_failure(&mut s, &SyncError::Rejected("validation".into()), 0, false),
            FailureAction::Stop
        );
        assert_eq!(s.state, SyncState::Error);
    }

    #[test]
    fn c2_skipped_entities_surface_as_warning() {
        let mut s = connected();
        let skipped = vec![SkippedEntity {
            entity_type: EntityType::Request,
            id: "r".into(),
            name: "Big upload".into(),
            reason: "too_large".into(),
        }];
        on_success(&mut s, 1, 0, skipped);
        assert_eq!(s.skipped_entities.len(), 1);
        assert_eq!(
            s.warning.as_deref(),
            Some("1 item is too large to sync (over 512 KB): Big upload")
        );
        on_success(&mut s, 2, 0, vec![]);
        assert!(s.warning.is_none());
    }
}

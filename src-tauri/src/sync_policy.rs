//! Pure sync-status state machine (AC-SYNC-2/6/7, contracts C2/C4), kept
//! free of Tauri types so it is unit-tested. `sync_rt` applies the actions.

use reqharbor_core::space_sync::{self, LegacyStatus, SpaceStatus};
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
    /// Account mode: the session ended (AC-AUTH-9). Account spaces wait for
    /// a new sign-in; token sync keeps going.
    SignInRequired,
    /// Account mode with nothing syncing right now (signed out, or every
    /// space paused); bindings are kept.
    Paused,
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
    /// Local items of a type this server doesn't accept; they stay
    /// local-only until the server is updated (06-go-server §9).
    pub unsupported_entities: Vec<SkippedEntity>,
    /// Non-fatal warning for the sync indicator tooltip, e.g. skipped items.
    pub warning: Option<String>,
    /// `account.json` holds bindings (04 §6.1): projects sync per space.
    pub account_mode: bool,
    /// One entry per bound space, personal first, then teams, then legacy.
    pub spaces: Vec<SpaceStatus>,
    /// The token-synced workspace in account mode.
    pub legacy: Option<LegacyStatus>,
    /// The first-sync checklist is waiting for an answer (UX §10).
    pub first_sync_pending: bool,
}

impl SyncStatus {
    pub fn from_store(store: &mut Store) -> Self {
        if store.account_mode() {
            return Self::from_account_store(store);
        }
        let cfg = store.workspace.sync.clone();
        let skipped = if cfg.is_some() {
            sync::skipped_entities(store)
        } else {
            Vec::new()
        };
        let unsupported = if cfg.is_some() {
            sync::unsupported_entities(store)
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
                sync::pending_count(store)
            } else {
                0
            },
            warning: sync_warning(&skipped, &unsupported),
            skipped_entities: skipped,
            unsupported_entities: unsupported,
            account_mode: false,
            spaces: Vec::new(),
            legacy: None,
            first_sync_pending: false,
        }
    }

    /// Account mode: per-space lines, `connected` while anything can sync.
    fn from_account_store(store: &mut Store) -> Self {
        let skipped = space_sync::skipped_total(store);
        let unsupported = space_sync::unsupported_total(store);
        let connected = store.sync_active();
        let legacy = space_sync::legacy_status(store);
        let mut s = SyncStatus {
            state: SyncState::Idle,
            connected,
            worker_url: legacy.as_ref().map(|l| l.worker_url.clone()),
            remote_workspace_id: legacy.as_ref().map(|l| l.remote_workspace_id.clone()),
            last_sync_at: None,
            last_pull_at: None,
            last_error: None,
            next_retry_at: None,
            retry_attempt: 0,
            pending_changes: space_sync::pending_total(store),
            warning: sync_warning(&skipped, &unsupported),
            skipped_entities: skipped,
            unsupported_entities: unsupported,
            account_mode: true,
            spaces: Vec::new(),
            legacy,
            first_sync_pending: false,
        };
        refresh_spaces(&mut s, store);
        s.last_sync_at = s.spaces.iter().filter_map(|x| x.last_sync_at).max();
        s
    }
}

/// Re-read the per-space part of the status from the store.
pub fn refresh_spaces(s: &mut SyncStatus, store: &Store) {
    if !store.account_mode() {
        return;
    }
    s.account_mode = true;
    s.spaces = space_sync::space_statuses(store);
    s.legacy = space_sync::legacy_status(store);
    s.first_sync_pending =
        store.bindings.first_sync_pending && !store.first_sync_candidates().is_empty();
    s.connected = store.sync_active();
    s.worker_url = s.legacy.as_ref().map(|l| l.worker_url.clone());
    s.remote_workspace_id = s.legacy.as_ref().map(|l| l.remote_workspace_id.clone());
    let syncing = !store.sync_scopes().is_empty();
    if !syncing && matches!(s.state, SyncState::Idle | SyncState::Disconnected) {
        s.state = if s.spaces.is_empty() && s.legacy.is_none() {
            SyncState::Disconnected
        } else {
            SyncState::Paused
        };
    } else if syncing && s.state == SyncState::Paused {
        s.state = SyncState::Idle;
    }
}

/// One sentence (no final period) per skip reason, in a fixed order:
/// "2 items are too large to sync (over 512 KB): Big upload, Fixtures".
/// The reason is the one the server gave (`invalid_json_value`,
/// `forbidden_role`, …) or `too_large` for entities over 512 KB.
pub fn skipped_warning(skipped: &[SkippedEntity]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let rank = |r: &str| match r {
        sync::TOO_LARGE => 0,
        sync::INVALID_JSON_VALUE => 1,
        sync::FORBIDDEN_ROLE => 2,
        _ => 3,
    };
    let mut reasons: Vec<&str> = Vec::new();
    for s in skipped {
        if !reasons.contains(&s.reason.as_str()) {
            reasons.push(&s.reason);
        }
    }
    reasons.sort_by_key(|r| (rank(r), *r));
    let parts: Vec<String> = reasons
        .into_iter()
        .map(|reason| {
            let items: Vec<&SkippedEntity> =
                skipped.iter().filter(|s| s.reason == reason).collect();
            let names: Vec<&str> = items.iter().map(|s| s.name.as_str()).collect();
            let n = items.len();
            let one = n == 1;
            let head = match reason {
                sync::TOO_LARGE => format!(
                    "{n} {} too large to sync (over 512 KB)",
                    if one { "item is" } else { "items are" }
                ),
                sync::INVALID_JSON_VALUE => format!(
                    "{n} {} synced because the server can't store one of {} values",
                    if one { "item isn't" } else { "items aren't" },
                    if one { "its" } else { "their" }
                ),
                sync::FORBIDDEN_ROLE => format!(
                    "{n} {} synced because your role can't change {} on the server",
                    if one { "item isn't" } else { "items aren't" },
                    if one { "it" } else { "them" }
                ),
                other => format!(
                    "{n} {} rejected by the server ({other})",
                    if one { "item was" } else { "items were" }
                ),
            };
            format!("{head}: {}", names.join(", "))
        })
        .collect();
    Some(parts.join(". "))
}

/// Both warnings for the sync indicator, joined into one sentence block.
pub fn sync_warning(skipped: &[SkippedEntity], unsupported: &[SkippedEntity]) -> Option<String> {
    let parts: Vec<String> = [
        skipped_warning(skipped),
        sync::unsupported_notice(unsupported.len()),
    ]
    .into_iter()
    .flatten()
    .collect();
    // The too-large sentence has no final period; the notice has one.
    (!parts.is_empty()).then(|| parts.join(". "))
}

/// Focus pull at most once per minute, never while syncing (AC-SYNC-2).
pub fn should_pull_on_focus(s: &SyncStatus, now: i64) -> bool {
    s.connected
        && s.state != SyncState::Syncing
        && s.state != SyncState::SignInRequired
        && !s
            .last_pull_at
            .is_some_and(|t| now - t < FOCUS_PULL_INTERVAL_MS)
}

pub fn begin(s: &mut SyncStatus) {
    s.state = SyncState::Syncing;
    s.connected = true;
}

pub fn on_success(
    s: &mut SyncStatus,
    now: i64,
    pending: usize,
    skipped: Vec<SkippedEntity>,
    unsupported: Vec<SkippedEntity>,
) {
    s.state = SyncState::Idle;
    s.connected = true;
    s.last_sync_at = Some(now);
    s.last_pull_at = Some(now);
    s.last_error = None;
    s.next_retry_at = None;
    s.retry_attempt = 0;
    s.pending_changes = pending;
    s.warning = sync_warning(&skipped, &unsupported);
    s.skipped_entities = skipped;
    s.unsupported_entities = unsupported;
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
///
/// Only a legacy token's 401 disconnects ([`SyncError::Unauthorized`] comes
/// only from token clients; session clients refresh and end in
/// [`SyncError::SignInRequired`], which never touches bindings or data).
pub fn on_failure(
    s: &mut SyncStatus,
    err: &SyncError,
    now: i64,
    token_rotated: bool,
) -> FailureAction {
    match err {
        SyncError::SignInRequired => {
            s.state = SyncState::SignInRequired;
            s.last_error = Some(err.to_string());
            s.next_retry_at = None;
            s.retry_attempt = 0;
            FailureAction::Stop
        }
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

/// Fold an account-mode run into the status (04 §6.6 step 5). The worst
/// state wins: a session that ended, then a retryable failure (offline),
/// then any other error; a legacy 401 only disconnects token sync.
pub fn on_account_run(
    s: &mut SyncStatus,
    run: &space_sync::AccountRun,
    now: i64,
    token_rotated: bool,
) -> FailureAction {
    s.connected = true;
    s.last_pull_at = Some(now);
    if run.sign_in_required {
        s.state = SyncState::SignInRequired;
        s.last_error = Some(SyncError::SignInRequired.to_string());
        s.next_retry_at = None;
        // Token sync may still have failed and need its own handling.
        if let Some(e @ SyncError::Unauthorized) = run.error() {
            let mut scratch = s.clone();
            let a = on_failure(&mut scratch, e, now, token_rotated);
            if a == FailureAction::Disconnect {
                return a;
            }
        }
        return FailureAction::Stop;
    }
    match run.error() {
        None => {
            s.state = SyncState::Idle;
            s.last_sync_at = Some(now);
            s.last_error = None;
            s.next_retry_at = None;
            s.retry_attempt = 0;
            FailureAction::Stop
        }
        Some(e) => on_failure(s, e, now, token_rotated),
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
            unsupported_entities: vec![],
            warning: None,
            account_mode: false,
            spaces: vec![],
            legacy: None,
            first_sync_pending: false,
        }
    }

    #[test]
    fn only_a_legacy_401_disconnects() {
        let mut s = connected();
        assert_eq!(
            on_failure(&mut s, &SyncError::SignInRequired, 0, false),
            FailureAction::Stop
        );
        assert_eq!(s.state, SyncState::SignInRequired);
        assert!(s.connected, "bindings and data stay; nothing disconnects");
        for e in [
            SyncError::SpaceGone("space_not_found".into()),
            SyncError::Forbidden("forbidden_role".into()),
            SyncError::ClientTooOld("client_too_old".into()),
        ] {
            let mut s = connected();
            assert_ne!(on_failure(&mut s, &e, 0, false), FailureAction::Disconnect);
            assert!(s.connected);
        }
        let mut s = connected();
        assert_eq!(
            on_failure(&mut s, &SyncError::Unauthorized, 0, false),
            FailureAction::Disconnect
        );
        // 507 and 429 back off like other transient failures (06 §9).
        for e in [
            SyncError::ServerFull("server_full".into()),
            SyncError::RateLimited("rate_limited".into()),
        ] {
            let mut s = connected();
            assert!(matches!(
                on_failure(&mut s, &e, 0, false),
                FailureAction::Retry(_)
            ));
            assert_eq!(s.state, SyncState::Offline);
        }
    }

    #[test]
    fn an_account_run_folds_into_the_worst_state() {
        use reqharbor_core::space_sync::{AccountRun, ScopeRun};
        let ok = AccountRun::default();
        let mut s = connected();
        assert_eq!(on_account_run(&mut s, &ok, 5, false), FailureAction::Stop);
        assert_eq!(s.state, SyncState::Idle);
        assert_eq!(s.last_sync_at, Some(5));

        let offline = AccountRun {
            scopes: vec![
                ScopeRun {
                    space_id: "p".into(),
                    error: Some(SyncError::QuotaExceeded("q".into())),
                    ..Default::default()
                },
                ScopeRun {
                    space_id: "t".into(),
                    error: Some(SyncError::Offline("down".into())),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut s = connected();
        assert!(matches!(
            on_account_run(&mut s, &offline, 5, false),
            FailureAction::Retry(_)
        ));
        assert_eq!(s.state, SyncState::Offline);

        let ended = AccountRun {
            sign_in_required: true,
            ..Default::default()
        };
        let mut s = connected();
        assert_eq!(
            on_account_run(&mut s, &ended, 5, false),
            FailureAction::Stop
        );
        assert_eq!(s.state, SyncState::SignInRequired);

        // The session ended and the legacy token was rejected: token sync
        // still disconnects (AC-SYNC-7).
        let both = AccountRun {
            sign_in_required: true,
            scopes: vec![ScopeRun {
                space_id: "legacy".into(),
                error: Some(SyncError::Unauthorized),
                ..Default::default()
            }],
            ..Default::default()
        };
        let mut s = connected();
        assert_eq!(
            on_account_run(&mut s, &both, 5, false),
            FailureAction::Disconnect
        );
    }

    #[test]
    fn ac_sync_2_focus_pull_at_most_once_per_minute() {
        let mut s = connected();
        assert!(should_pull_on_focus(&s, 1_000));
        on_success(&mut s, 1_000, 0, vec![], vec![]);
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
        on_success(&mut s, 5, 0, vec![], vec![]);
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
        on_success(&mut s, 1, 0, skipped, vec![]);
        assert_eq!(s.skipped_entities.len(), 1);
        assert_eq!(
            s.warning.as_deref(),
            Some("1 item is too large to sync (over 512 KB): Big upload")
        );
        on_success(&mut s, 2, 0, vec![], vec![]);
        assert!(s.warning.is_none());
    }

    #[test]
    fn unsupported_types_surface_as_a_notice() {
        let item = |t, name: &str, reason: &str| SkippedEntity {
            entity_type: t,
            id: name.into(),
            name: name.into(),
            reason: reason.into(),
        };
        let mut s = connected();
        let unsupported = vec![
            item(EntityType::Environment, "Global", "unsupported_type"),
            item(EntityType::Environment, "Dev", "unsupported_type"),
        ];
        on_success(&mut s, 1, 2, vec![], unsupported.clone());
        assert_eq!(s.unsupported_entities.len(), 2);
        assert_eq!(
            s.warning.as_deref(),
            Some(
                "2 items aren't synced because this server is older than your app. Ask its admin to update reqharbor-server."
            )
        );
        let big = vec![item(EntityType::Request, "Big", "too_large")];
        on_success(&mut s, 2, 2, big, unsupported[..1].to_vec());
        assert_eq!(
            s.warning.as_deref(),
            Some(
                "1 item is too large to sync (over 512 KB): Big. 1 item isn't synced because this server is older than your app. Ask its admin to update reqharbor-server."
            )
        );
        on_success(&mut s, 3, 0, vec![], vec![]);
        assert!(s.warning.is_none() && s.unsupported_entities.is_empty());
    }

    #[test]
    fn skipped_warnings_use_the_servers_reason() {
        let item = |name: &str, reason: &str| SkippedEntity {
            entity_type: EntityType::Request,
            id: name.into(),
            name: name.into(),
            reason: reason.into(),
        };
        let w = skipped_warning(&[
            item("Odd", "invalid_json_value"),
            item("Big", "too_large"),
            item("Mine", "forbidden_role"),
            item("X", "future_reason"),
            item("Odd2", "invalid_json_value"),
        ])
        .unwrap();
        assert_eq!(
            w,
            "1 item is too large to sync (over 512 KB): Big. \
             2 items aren't synced because the server can't store one of their values: Odd, Odd2. \
             1 item isn't synced because your role can't change it on the server: Mine. \
             1 item was rejected by the server (future_reason): X"
        );
        assert!(!w.contains("too large to sync (over 512 KB): Odd"));
    }
}

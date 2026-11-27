//! Sign-in runtime (04 §2.2, §12 M1c; 06-go-server §5.2, §9).
//!
//! Owns the signed-in [`Session`], the pending browser sign-in and the
//! cached discovery of the account's server, and reports every change as
//! one [`AuthStatus`] on `auth://status`. A finished sign-in runs the
//! store's one-time migration (04 §7) and starts a sync; signing out pauses
//! the account's spaces (their bindings stay).

use crate::error::{CmdResult, CommandError};
use crate::secret_store::AppSecrets;
use reqharbor_core::CancellationToken;
use reqharbor_core::auth::{
    self, Account, AccountFile, AuthClient, AuthError, CodeReceived, LoopbackConfig, PendingSignIn,
    ServerIdentity, Session, SessionInfo, TokenResponse,
};
use reqharbor_core::model::now_ms;
use reqharbor_core::secrets::{SecretError, SecretStore, SecretStoreKind};
use reqharbor_core::server_info::{self, Discovery, DiscoveryStatus};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};

pub const EVENT_AUTH: &str = "auth://status";
/// Sign-out never waits longer than this for the server (AC-AUTH-11: best
/// effort when offline).
const LOGOUT_TIMEOUT: Duration = Duration::from_secs(5);

/// The `accounts` flag (04 §12 M1c): on in debug builds, off in release
/// builds until personal sync ships, and `REQHARBOR_ACCOUNTS=1|0` overrides.
pub fn accounts_enabled() -> bool {
    match std::env::var("REQHARBOR_ACCOUNTS").as_deref() {
        Ok("1") | Ok("true") => true,
        Ok("0") | Ok("false") => false,
        _ => cfg!(debug_assertions) || option_env!("REQHARBOR_ACCOUNTS") == Some("1"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SignInState {
    Idle,
    /// Asking the server (`GET /v1/server`).
    Checking,
    /// The browser is open and the loopback is listening.
    Waiting,
    /// The browser came back; exchanging the code.
    Exchanging,
    Success,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInStatus {
    pub state: SignInState,
    pub server_url: Option<String>,
    pub method: Option<String>,
    /// Unix ms when the browser was opened (the "Waiting 0:42" timer).
    pub started_at: Option<i64>,
    /// A `CommandErrorCode` when `state` is `error`.
    pub error: Option<String>,
    /// The error's copy, or, while `waiting`, [`BROWSER_FAILED`].
    pub message: Option<String>,
    /// While `waiting`, when the browser couldn't be opened: the authorize
    /// URL for "Copy link" (it holds no secret: PKCE sends only the challenge).
    pub authorize_url: Option<String>,
}

impl SignInStatus {
    fn idle() -> Self {
        SignInStatus {
            state: SignInState::Idle,
            server_url: None,
            method: None,
            started_at: None,
            error: None,
            message: None,
            authorize_url: None,
        }
    }
}

/// Shown while waiting when the system browser couldn't be opened.
pub const BROWSER_FAILED: &str =
    "Couldn't open your browser. Copy the sign-in link and open it in your browser.";

/// Record whether the browser opened for `url` (QA: a failed open used to
/// leave "Waiting…" with no hint until the 10-minute timeout).
fn note_browser(s: &mut SignInStatus, opened: bool, url: &str) {
    if s.state != SignInState::Waiting {
        return;
    }
    if opened {
        s.message = None;
        s.authorize_url = None;
    } else {
        s.message = Some(BROWSER_FAILED.to_string());
        s.authorize_url = Some(url.to_string());
    }
}

/// Payload of `auth_status` and the `auth://status` event.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthStatus {
    /// The `accounts` flag: the UI shows Settings › Account only when set.
    pub enabled: bool,
    pub account: Option<Account>,
    /// AC-AUTH-9: the session can't be refreshed; sign in again.
    pub sign_in_required: bool,
    pub keychain: SecretStoreKind,
    /// "Your sign-in is stored in a file because …", when the file store is used.
    pub keychain_note: Option<String>,
    /// The keychain refused to give the session back (edge case 7).
    pub keychain_unreadable: bool,
    /// Last discovery of the account's server (cached up to 24 h offline).
    pub server: Option<Discovery>,
    /// The last profile refresh could not reach the server.
    pub offline: bool,
    pub sign_in: SignInStatus,
}

struct Pending {
    cancel: CancellationToken,
    authorize_url: String,
}

struct Inner {
    account: Option<Account>,
    session: Option<Arc<Session>>,
    sign_in_required: bool,
    keychain_unreadable: bool,
    server: Option<Discovery>,
    offline: bool,
    sign_in: SignInStatus,
    pending: Option<Pending>,
    /// Bumped by every start/cancel; a finishing task from an older
    /// generation changes nothing.
    generation: u64,
}

pub struct AuthState {
    pub enabled: bool,
    /// Probed on first use only (QA: no keychain prompt at a plain start).
    store: Arc<AppSecrets>,
    secrets: Arc<dyn SecretStore>,
    dir: PathBuf,
    inner: Mutex<Inner>,
}

/// Read the session from the keychain, retrying a denied read once (04 §3.3).
fn read_session(
    account: &Account,
    secrets: &Arc<dyn SecretStore>,
) -> Result<Option<auth::Tokens>, SecretError> {
    let key = auth::session_key(&account.server_url, &account.user_id);
    let read = || -> Result<Option<auth::Tokens>, SecretError> {
        Ok(secrets
            .get(&key)?
            .and_then(|s| serde_json::from_str(&s).ok()))
    };
    match read() {
        Err(SecretError::Denied(_)) => read(),
        other => other,
    }
}

impl AuthState {
    /// Load `account.json`, and the session from the secret store. The
    /// store (and so the keychain probe) is touched only when the accounts
    /// flag is on and `account.json` has an account: a plain start of a
    /// build without accounts never reaches the keychain.
    pub fn load(dir: &Path, store: Arc<AppSecrets>, enabled: bool) -> Self {
        let secrets: Arc<dyn SecretStore> = store.clone();
        let file = AccountFile::load(dir).ok().flatten().unwrap_or_default();
        let mut inner = Inner {
            account: file.account.clone(),
            session: None,
            sign_in_required: file.sign_in_required,
            keychain_unreadable: false,
            server: file.server.clone(),
            offline: false,
            sign_in: SignInStatus::idle(),
            pending: None,
            generation: 0,
        };
        if let Some(account) = file.account.as_ref().filter(|_| enabled) {
            match read_session(account, &secrets) {
                Ok(Some(tokens)) => match AuthClient::new(&account.server_url) {
                    Ok(client) => {
                        inner.session = Some(Arc::new(Session::new(
                            client,
                            &account.server_url,
                            &account.user_id,
                            secrets.clone(),
                            tokens,
                        )))
                    }
                    Err(_) => inner.sign_in_required = true,
                },
                // The entry is gone (another build, a wiped keychain).
                Ok(None) => inner.sign_in_required = true,
                Err(_) => inner.keychain_unreadable = true,
            }
        }
        AuthState {
            enabled,
            store,
            secrets,
            dir: dir.to_path_buf(),
            inner: Mutex::new(inner),
        }
    }

    /// Whether `generation` is still the current sign-in.
    fn is_current(&self, generation: u64) -> bool {
        self.lock().generation == generation
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn status(&self) -> AuthStatus {
        let i = self.lock();
        AuthStatus {
            enabled: self.enabled,
            // Edge case 7: an unreadable keychain renders the signed-out view.
            account: if i.keychain_unreadable {
                None
            } else {
                i.account.clone()
            },
            sign_in_required: i.sign_in_required && !i.keychain_unreadable,
            keychain: self.store.kind(),
            keychain_note: self.store.note().map(str::to_string),
            keychain_unreadable: i.keychain_unreadable,
            server: i.server.clone(),
            offline: i.offline,
            sign_in: i.sign_in.clone(),
        }
    }

    pub fn session(&self) -> Option<Arc<Session>> {
        let i = self.lock();
        if i.keychain_unreadable {
            return None;
        }
        i.session.clone()
    }

    /// The signed-in account's origin, when there is one.
    pub fn account_origin(&self) -> Option<String> {
        self.lock().account.as_ref().map(|a| a.server_url.clone())
    }

    /// The signed-in account (none while the keychain is unreadable).
    pub fn account(&self) -> Option<Account> {
        let i = self.lock();
        if i.keychain_unreadable {
            None
        } else {
            i.account.clone()
        }
    }

    /// The secret store (keychain or 0600 file), probed on first use.
    pub fn secrets(&self) -> Arc<dyn SecretStore> {
        self.secrets.clone()
    }

    fn persist_flags(&self) {
        let (required, server) = {
            let i = self.lock();
            (i.sign_in_required, i.server.clone())
        };
        let _ = AccountFile::update(&self.dir, |file| {
            if file.account.is_some() {
                file.sign_in_required = required;
                if server.is_some() {
                    file.server = server;
                }
            }
        });
    }
}

/// After a sign-in: run the store's migration (or pause foreign bindings),
/// refresh the sync status and start a sync (04 §7, 06 §9).
pub fn on_signed_in(app: &AppHandle) {
    let auth = app.state::<AuthState>();
    let Some(account) = auth.account() else {
        return;
    };
    let secrets = auth.secrets();
    let state = app.state::<crate::state::AppState>();
    let migrated = {
        let mut store = state.store();
        let r = store.sign_in_migrate((&account).into(), secrets.as_ref());
        let fresh = crate::state::SyncStatus::from_store(&mut store);
        state.update_sync(|s| *s = fresh);
        r
    };
    let fresh = match migrated {
        Ok(r) => r.fresh_device,
        Err(e) => {
            // Token sync stays exactly as it was; the account is still signed in.
            let _ = app.emit(
                crate::sync_rt::EVENT_STATUS,
                &state.update_sync(|s| s.last_error = Some(e.to_string())),
            );
            return;
        }
    };
    crate::sync_rt::refresh_status(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let _ = crate::sync_rt::run_sync(&app).await;
        if fresh {
            // UX §10 "Fresh device": no checklist, just say what syncs.
            let message = app
                .state::<crate::state::AppState>()
                .store()
                .fresh_device_notice();
            let _ = app.emit(
                crate::sync_rt::EVENT_NOTICE,
                &crate::sync_rt::SyncNotice {
                    kind: "success",
                    message,
                },
            );
        }
    });
}

/// App start: line the store up with `account.json` and the secret store:
/// the signed-in account, the legacy token in account mode, and the
/// migration for an account signed in by a build without bindings.
pub fn attach_store(app: &AppHandle) {
    let auth = app.state::<AuthState>();
    let account = if auth.enabled { auth.account() } else { None };
    let secrets = auth.secrets();
    let state = app.state::<crate::state::AppState>();
    let mut store = state.store();
    if let Some(a) = &account
        && !store.account_mode()
    {
        let _ = store.sign_in_migrate(a.into(), secrets.as_ref());
    }
    if !store.account_mode() {
        return;
    }
    store.set_account(account.as_ref().map(Into::into));
    // Token sync reconnected by an older build: keep its token safe in the
    // secret store before it leaves workspace.json (stays there on failure).
    let _ = store.secure_stray_sync(secrets.as_ref());
    if store.legacy_token.is_none()
        && let Some(l) = store.bindings.legacy.clone()
    {
        store.legacy_token = secrets
            .get(&reqharbor_core::spaces::legacy_key(&l.remote_workspace_id))
            .ok()
            .flatten();
    }
    let fresh = crate::state::SyncStatus::from_store(&mut store);
    state.update_sync(|s| *s = fresh);
}

fn emit(app: &AppHandle) -> AuthStatus {
    let status = app.state::<AuthState>().status();
    let _ = app.emit(EVENT_AUTH, &status);
    status
}

/// Error code and copy for a discovery that can't be signed in to.
pub fn discovery_error(d: &Discovery) -> Option<CommandError> {
    let msg = d.message.clone().unwrap_or_default();
    let code = match d.status {
        DiscoveryStatus::Ok if d.can_sign_in => return None,
        DiscoveryStatus::Ok => "no_sign_in_methods",
        DiscoveryStatus::Unreachable => "server_unreachable",
        DiscoveryStatus::TokenSyncOnly => "token_sync_only",
        DiscoveryStatus::NotReqharbor => "not_reqharbor",
        DiscoveryStatus::ClientTooOld => "client_too_old",
    };
    Some(CommandError::new(code, msg))
}

pub const INSECURE_MESSAGE: &str = "This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.";

/// Whether `settings.json` records the plain-http acknowledgement for
/// `origin` (`knownServers[].insecureAck`, written by the UI).
pub fn insecure_acknowledged(settings: &reqharbor_core::model::Settings, origin: &str) -> bool {
    settings
        .extra
        .get("knownServers")
        .and_then(|v| v.as_array())
        .is_some_and(|list| {
            list.iter().any(|s| {
                s.get("url").and_then(|u| u.as_str()) == Some(origin)
                    && s.get("insecureAck").and_then(|a| a.as_bool()) == Some(true)
            })
        })
}

/// `server_discover`: normalise and ask the server; for the signed-in
/// account's server an unreachable answer falls back to the 24 h cache.
pub async fn discover(app: &AppHandle, server_url: &str) -> CmdResult<Discovery> {
    let origin = server_info::normalize_server_url(server_url)
        .map_err(|e| CommandError::new("invalid", e.to_string()))?;
    let fresh = server_info::discover_origin(&origin).await;
    let auth = app.state::<AuthState>();
    if auth.account_origin().as_deref() != Some(origin.as_str()) {
        return Ok(fresh);
    }
    let cached = auth.lock().server.clone();
    let d = Discovery::or_cached(fresh, cached.as_ref(), now_ms());
    if !d.cached {
        if let Some(filled) = note_account_discovery(&auth, &d) {
            // The account's instance is known now: its bindings can sync.
            let state = app.state::<crate::state::AppState>();
            let mut store = state.store();
            if store.account_mode() {
                store.set_account(Some((&filled).into()));
                let _ = store.save_bindings();
            }
        }
        emit(app);
    }
    Ok(d)
}

/// Record a fresh discovery of the account's server: a changed `instanceId`
/// for the same origin is a different server (06 §9), whose sessions this
/// one's tokens can't use. An account signed in without a known instance
/// adopts the discovered one; it is returned then.
fn note_account_discovery(auth: &AuthState, d: &Discovery) -> Option<Account> {
    let info = d.info.as_ref()?;
    let filled = {
        let mut i = auth.lock();
        let changed = i.account.as_ref().is_some_and(|a| {
            !a.server_instance_id.is_empty()
                && !info.instance_id.is_empty()
                && a.server_instance_id != info.instance_id
        });
        if changed {
            i.sign_in_required = true;
        }
        i.server = Some(d.clone());
        match i.account.as_mut() {
            Some(a) if a.server_instance_id.is_empty() && !info.instance_id.is_empty() => {
                a.server_instance_id = info.instance_id.clone();
                Some(a.clone())
            }
            _ => None,
        }
    };
    auth.persist_flags();
    if let Some(a) = &filled {
        let a = a.clone();
        let _ = AccountFile::update(&auth.dir, move |file| {
            if let Some(acc) = file.account.as_mut()
                && acc.server_url == a.server_url
                && acc.user_id == a.user_id
            {
                acc.server_instance_id = a.server_instance_id.clone();
            }
        });
    }
    filled
}

/// App start: rediscover the account's server (06 §9 "Rediscovery").
pub async fn rediscover_on_start(app: &AppHandle) {
    let Some(origin) = app.state::<AuthState>().account_origin() else {
        return;
    };
    let _ = discover(app, &origin).await;
}

fn set_sign_in(
    app: &AppHandle,
    generation: Option<u64>,
    f: impl FnOnce(&mut SignInStatus),
) -> bool {
    let auth = app.state::<AuthState>();
    let mut i = auth.lock();
    if generation.is_some_and(|g| g != i.generation) {
        return false;
    }
    f(&mut i.sign_in);
    true
}

fn fail(app: &AppHandle, generation: Option<u64>, err: &CommandError) {
    let applied = set_sign_in(app, generation, |s| {
        s.state = SignInState::Error;
        s.error = Some(err.code.clone());
        s.message = Some(err.message.clone());
        s.authorize_url = None;
    });
    if applied {
        let auth = app.state::<AuthState>();
        auth.lock().pending = None;
        emit(app);
    }
}

fn open_browser(app: &AppHandle, url: &str) -> bool {
    use tauri_plugin_opener::OpenerExt;
    app.opener().open_url(url, None::<&str>).is_ok()
}

fn focus_main(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// `auth_sign_in_start`: discovery, the plain-http check, the loopback, then
/// the system browser. Returns once the browser is open; the rest arrives
/// as `auth://status` events.
pub async fn sign_in_start(
    app: &AppHandle,
    server_url: &str,
    method: Option<String>,
    insecure_confirmed: bool,
    invite_ticket: Option<String>,
) -> CmdResult<AuthStatus> {
    let generation = {
        let auth = app.state::<AuthState>();
        let mut i = auth.lock();
        // Once the code is being exchanged the sign-in finishes as one unit
        // (its tokens may already be on disk).
        if i.sign_in.state == SignInState::Exchanging {
            return Err(CommandError::new("invalid", FINISHING_MESSAGE));
        }
        if let Some(p) = i.pending.take() {
            p.cancel.cancel();
        }
        i.generation += 1;
        i.sign_in = SignInStatus {
            state: SignInState::Checking,
            server_url: Some(server_url.trim().to_string()),
            method: method.clone(),
            ..SignInStatus::idle()
        };
        i.generation
    };
    emit(app);
    let result = prepare(app, server_url, method.clone(), insecure_confirmed)
        .await
        .map(|(p, d)| (p.with_ticket(invite_ticket.as_deref()), d));
    let (pending, discovery) = match result {
        Ok(v) => v,
        Err(e) => {
            fail(app, Some(generation), &e);
            return Err(e);
        }
    };
    let cancel = CancellationToken::new();
    let url = pending.authorize_url.clone();
    let origin = pending.server_url.clone();
    let current = {
        let auth = app.state::<AuthState>();
        let mut i = auth.lock();
        if i.generation == generation {
            i.pending = Some(Pending {
                cancel: cancel.clone(),
                authorize_url: url.clone(),
            });
            i.sign_in = SignInStatus {
                state: SignInState::Waiting,
                server_url: Some(origin),
                method: method.clone(),
                started_at: Some(now_ms()),
                ..SignInStatus::idle()
            };
            true
        } else {
            false
        }
    };
    if !current {
        return Err(CommandError::new(
            "sign_in_cancelled",
            AuthError::Cancelled.to_string(),
        ));
    }
    let opened = open_browser(app, &url);
    set_sign_in(app, Some(generation), |s| note_browser(s, opened, &url));
    let status = emit(app);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        finish(&app, generation, pending, discovery, method, cancel).await;
    });
    Ok(status)
}

async fn prepare(
    app: &AppHandle,
    server_url: &str,
    method: Option<String>,
    insecure_confirmed: bool,
) -> CmdResult<(PendingSignIn, Discovery)> {
    let origin = server_info::normalize_server_url(server_url)
        .map_err(|e| CommandError::new("invalid", e.to_string()))?;
    // Rediscover before every sign-in (06 §9).
    let discovery = server_info::discover_origin(&origin).await;
    if let Some(e) = discovery_error(&discovery) {
        return Err(e);
    }
    if discovery.insecure && !insecure_confirmed {
        let acked = {
            let state = app.state::<crate::state::AppState>();
            insecure_acknowledged(&state.store().settings, &origin)
        };
        if !acked {
            return Err(CommandError::new("insecure_unconfirmed", INSECURE_MESSAGE));
        }
    }
    let pending = PendingSignIn::start(&origin, method.as_deref(), &auth::device_name())
        .await
        .map_err(CommandError::from)?;
    Ok((pending, discovery))
}

async fn finish(
    app: &AppHandle,
    generation: u64,
    pending: PendingSignIn,
    discovery: Discovery,
    method: Option<String>,
    cancel: CancellationToken,
) {
    let origin = pending.server_url.clone();
    let code = match pending.wait(LoopbackConfig::default(), cancel).await {
        Ok(c) => c,
        // Cancel already reset the status.
        Err(AuthError::Cancelled) => return,
        Err(e) => return fail(app, Some(generation), &CommandError::from(e)),
    };
    focus_main(app);
    if !set_sign_in(app, Some(generation), |s| {
        s.state = SignInState::Exchanging;
        s.message = None;
        s.authorize_url = None;
    }) {
        return;
    }
    emit(app);
    let auth = app.state::<AuthState>();
    match exchange_and_commit(
        &auth,
        generation,
        &origin,
        &code,
        &discovery,
        method.as_deref(),
    )
    .await
    {
        Ok(true) => {
            emit(app);
            on_signed_in(app);
        }
        // Superseded (signed out while exchanging): nothing was kept.
        Ok(false) => {}
        Err(e) => fail(app, Some(generation), &CommandError::from(e)),
    }
}

/// Shown when a sign-in is started while another one is finishing.
const FINISHING_MESSAGE: &str = "Sign-in is finishing. Try again in a moment.";

/// Revoke a session the exchange created but nothing will use.
async fn revoke_unused(client: &AuthClient, resp: &TokenResponse) {
    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, client.logout(&resp.access_token)).await;
}

/// Exchange the code, store the session and make it the account. Returns
/// `Ok(false)` when the sign-in was superseded on the way (a sign-out while
/// exchanging; Cancel and a new start are refused then): the new session is
/// revoked and nothing of it stays on disk or in memory, and the previous
/// session is left alone unless this sign-in is still current.
async fn exchange_and_commit(
    auth: &AuthState,
    generation: u64,
    origin: &str,
    code: &CodeReceived,
    discovery: &Discovery,
    method: Option<&str>,
) -> Result<bool, AuthError> {
    let client = AuthClient::new(origin)?;
    let resp = code.exchange(&client).await?;
    let info = discovery.info.as_ref();
    let identity = ServerIdentity {
        instance_id: info.map(|i| i.instance_id.clone()).unwrap_or_default(),
        name: info
            .map(|i| i.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| server_info::display_host(origin)),
    };
    if !auth.is_current(generation) {
        revoke_unused(&client, &resp).await;
        return Ok(false);
    }
    // Signing in again replaces the previous session: revoke it on the
    // server first (best effort).
    if let Some(old) = auth.session() {
        let _ = tokio::time::timeout(LOGOUT_TIMEOUT, old.logout()).await;
    }
    if !auth.is_current(generation) {
        revoke_unused(&client, &resp).await;
        return Ok(false);
    }
    let (account, session) = auth::complete_sign_in(
        &auth.dir,
        auth.secrets.clone(),
        &client,
        &identity,
        Some(discovery.clone()),
        method,
        resp,
    )
    .await?;
    Ok(commit_sign_in(auth, generation, account, session, discovery.clone()).await)
}

/// Make a stored sign-in the account, or, when `generation` is stale, undo
/// it: revoke it, delete its tokens and put `account.json` back in line
/// with memory. Disk and memory never disagree afterwards.
async fn commit_sign_in(
    auth: &AuthState,
    generation: u64,
    account: Account,
    session: Session,
    discovery: Discovery,
) -> bool {
    {
        let mut i = auth.lock();
        if i.generation == generation {
            i.account = Some(account);
            i.session = Some(Arc::new(session));
            i.sign_in_required = false;
            i.keychain_unreadable = false;
            i.offline = false;
            i.server = Some(discovery);
            i.pending = None;
            i.sign_in.state = SignInState::Success;
            i.sign_in.error = None;
            i.sign_in.message = None;
            i.sign_in.authorize_url = None;
            return true;
        }
    }
    let _ = tokio::time::timeout(LOGOUT_TIMEOUT, session.logout()).await;
    let _ = session.forget().await;
    auth.restore_account_file(&account);
    false
}

impl AuthState {
    /// `account.json` was written for `written` by a sign-in that lost: put
    /// back what memory holds (or remove the file when nothing is signed in).
    fn restore_account_file(&self, written: &Account) {
        let (account, server, required) = {
            let i = self.lock();
            (i.account.clone(), i.server.clone(), i.sign_in_required)
        };
        let _ = AccountFile::update(&self.dir, |file| {
            if file.account.as_ref() != Some(written) {
                return;
            }
            file.server = if account.is_some() { server } else { None };
            file.sign_in_required = account.is_some() && required;
            file.account = account;
        });
    }
}

/// "Open browser again": the same authorize URL.
pub fn sign_in_reopen(app: &AppHandle) -> CmdResult<()> {
    let (url, generation) = {
        let auth = app.state::<AuthState>();
        let i = auth.lock();
        let url = i
            .pending
            .as_ref()
            .map(|p| p.authorize_url.clone())
            .ok_or_else(|| CommandError::new("not_found", "No sign-in is in progress"))?;
        (url, i.generation)
    };
    let opened = open_browser(app, &url);
    if set_sign_in(app, Some(generation), |s| note_browser(s, opened, &url)) {
        emit(app);
    }
    Ok(())
}

/// Cancel closes the listener and drops the verifier (UX §3.2). Once the
/// code is being exchanged it does nothing: the sign-in finishes as a unit.
fn cancel_pending(auth: &AuthState) -> bool {
    let mut i = auth.lock();
    if i.sign_in.state == SignInState::Exchanging {
        return false;
    }
    if let Some(p) = i.pending.take() {
        p.cancel.cancel();
    }
    i.generation += 1;
    i.sign_in = SignInStatus::idle();
    true
}

pub fn sign_in_cancel(app: &AppHandle) -> AuthStatus {
    cancel_pending(&app.state::<AuthState>());
    emit(app)
}

/// AC-AUTH-11: revoke on the server (best effort), clear the keychain entry,
/// forget the account. Local projects are untouched.
pub async fn sign_out(app: &AppHandle) -> CmdResult<AuthStatus> {
    let auth = app.state::<AuthState>();
    let (session, account) = {
        let mut i = auth.lock();
        if let Some(p) = i.pending.take() {
            p.cancel.cancel();
        }
        i.generation += 1;
        (i.session.clone(), i.account.clone())
    };
    if let Some(s) = &session
        && !s.sign_in_required()
    {
        let _ = tokio::time::timeout(LOGOUT_TIMEOUT, s.logout()).await;
    }
    if let Some(a) = &account {
        let key = auth::session_key(&a.server_url, &a.user_id);
        auth::delete_tokens(auth.secrets.clone(), key)
            .await
            .map_err(|e| CommandError::from(AuthError::from(e)))?;
    }
    AccountFile::update(&auth.dir, |file| {
        file.account = None;
        file.server = None;
        file.sign_in_required = false;
    })
    .map_err(CommandError::from)?;
    {
        let mut i = auth.lock();
        i.account = None;
        i.session = None;
        i.sign_in_required = false;
        i.keychain_unreadable = false;
        i.server = None;
        i.offline = false;
        i.sign_in = SignInStatus::idle();
    }
    // The account's spaces pause; their bindings and data stay (UX §4.3).
    if let Some(state) = app.try_state::<crate::state::AppState>() {
        state.store().set_account(None);
        crate::sync_rt::refresh_status(app);
    }
    Ok(emit(app))
}

/// Mark the session as needing a new sign-in (AC-AUTH-9) and tell the UI.
pub(crate) fn signed_out_by_server(app: &AppHandle) {
    let auth = app.state::<AuthState>();
    auth.lock().sign_in_required = true;
    auth.persist_flags();
    emit(app);
}

fn require_session(app: &AppHandle) -> CmdResult<Arc<Session>> {
    let auth = app.state::<AuthState>();
    let i = auth.lock();
    if i.account.is_none() || i.keychain_unreadable {
        return Err(CommandError::from(AuthError::NotSignedIn));
    }
    if i.sign_in_required {
        return Err(CommandError::from(AuthError::SignInRequired));
    }
    i.session
        .clone()
        .ok_or_else(|| CommandError::from(AuthError::SignInRequired))
}

/// Map a session call's error, handling SignInRequired centrally.
fn session_err(app: &AppHandle, e: AuthError) -> CommandError {
    match e {
        AuthError::SignInRequired => signed_out_by_server(app),
        AuthError::Offline(_) => {
            app.state::<AuthState>().lock().offline = true;
            emit(app);
        }
        _ => {}
    }
    CommandError::from(e)
}

/// `auth_me`: refresh the profile shown in Settings › Account.
pub async fn refresh_profile(app: &AppHandle) -> CmdResult<AuthStatus> {
    let session = require_session(app)?;
    let me = match session.me().await {
        Ok(me) => me,
        Err(e @ AuthError::Offline(_)) => {
            let _ = session_err(app, e);
            return Ok(app.state::<AuthState>().status());
        }
        Err(e) => return Err(session_err(app, e)),
    };
    let auth = app.state::<AuthState>();
    {
        let mut i = auth.lock();
        i.offline = false;
        if let Some(a) = i.account.as_mut()
            && a.user_id == me.user.id
        {
            a.email = me.user.email.clone();
            a.name = me.user.name.clone();
            if !me.method.is_empty() {
                a.method = me.method.clone();
            }
        }
    }
    let account = auth.lock().account.clone();
    let _ = AccountFile::update(&auth.dir, |file| {
        if file.account.is_some() {
            file.account = account;
        }
    });
    Ok(emit(app))
}

pub async fn sessions(app: &AppHandle) -> CmdResult<Vec<SessionInfo>> {
    let session = require_session(app)?;
    session.sessions().await.map_err(|e| session_err(app, e))
}

pub async fn revoke_session(app: &AppHandle, session_id: &str) -> CmdResult<()> {
    let session = require_session(app)?;
    if session.session_id().await == session_id {
        return Err(CommandError::new(
            "invalid",
            "Use Sign out to sign this device out.",
        ));
    }
    session
        .revoke_session(session_id)
        .await
        .map_err(|e| session_err(app, e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secret_store::Chosen;
    use reqharbor_core::model::Settings;
    use reqharbor_core::secrets::MemorySecretStore;
    use serde_json::json;

    fn discovery(status: DiscoveryStatus, can: bool) -> Discovery {
        Discovery {
            server_url: "https://rh.acme.dev".into(),
            status,
            message: Some("msg".into()),
            info: None,
            canonical_url: None,
            insecure: false,
            can_sign_in: can,
            fetched_at: 0,
            cached: false,
        }
    }

    #[test]
    fn discovery_statuses_map_to_error_codes() {
        use DiscoveryStatus as S;
        assert!(discovery_error(&discovery(S::Ok, true)).is_none());
        for (s, can, code) in [
            (S::Ok, false, "no_sign_in_methods"),
            (S::Unreachable, false, "server_unreachable"),
            (S::TokenSyncOnly, false, "token_sync_only"),
            (S::NotReqharbor, false, "not_reqharbor"),
            (S::ClientTooOld, false, "client_too_old"),
        ] {
            let e = discovery_error(&discovery(s, can)).unwrap();
            assert_eq!(e.code, code);
            assert_eq!(e.message, "msg");
        }
    }

    #[test]
    fn insecure_ack_is_read_per_origin_from_known_servers() {
        let mut s: Settings = serde_json::from_value(json!({})).unwrap();
        assert!(!insecure_acknowledged(&s, "http://192.168.1.5:8080"));
        s.extra.insert(
            "knownServers".into(),
            json!([
                {"url": "http://192.168.1.5:8080", "insecureAck": true},
                {"url": "http://10.0.0.2", "insecureAck": false}
            ]),
        );
        assert!(insecure_acknowledged(&s, "http://192.168.1.5:8080"));
        assert!(!insecure_acknowledged(&s, "http://10.0.0.2"));
        assert!(!insecure_acknowledged(&s, "http://192.168.1.6:8080"));
    }

    fn chosen(store: Arc<MemorySecretStore>) -> Arc<AppSecrets> {
        Arc::new(AppSecrets::fixed(Chosen {
            store,
            kind: SecretStoreKind::System,
            note: None,
        }))
    }

    fn tokens() -> String {
        serde_json::to_string(&auth::Tokens {
            access_token: "rha_a".into(),
            access_expires_at: now_ms() + 3_600_000,
            refresh_token: "rhr_r".into(),
            session_id: "s1".into(),
        })
        .unwrap()
    }

    fn signed_in_dir() -> (tempfile::TempDir, Arc<MemorySecretStore>) {
        let dir = tempfile::tempdir().unwrap();
        AccountFile {
            account: Some(account()),
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        mem.set("session:https://rh.acme.dev:u1", &tokens())
            .unwrap();
        (dir, mem)
    }

    #[test]
    fn the_keychain_is_not_probed_while_accounts_are_off() {
        use crate::secret_store::Preference;
        // Flag off: even with a saved account, nothing touches the store.
        let (dir, mem) = signed_in_dir();
        let before = mem.log();
        let store = Arc::new(AppSecrets::new(
            dir.path(),
            Preference::Keychain,
            mem.clone(),
        ));
        let state = AuthState::load(dir.path(), store.clone(), false);
        let st = state.status();
        assert!(!st.enabled);
        assert_eq!(st.keychain, SecretStoreKind::System);
        assert!(state.session().is_none());
        assert_eq!(store.probe_count(), 0);
        assert_eq!(mem.log(), before);

        // Flag on but signed out: still no probe at start.
        let empty = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        let store = Arc::new(AppSecrets::new(
            empty.path(),
            Preference::Keychain,
            mem.clone(),
        ));
        let state = AuthState::load(empty.path(), store.clone(), true);
        assert!(state.status().account.is_none());
        assert_eq!(store.probe_count(), 0);
        assert!(mem.log().is_empty());

        // Flag on with an account: the probe runs once, then the session loads.
        let (dir, mem) = signed_in_dir();
        let store = Arc::new(AppSecrets::new(
            dir.path(),
            Preference::Keychain,
            mem.clone(),
        ));
        let state = AuthState::load(dir.path(), store.clone(), true);
        assert_eq!(store.probe_count(), 1);
        assert!(mem.log().contains(&"set:probe:reqharbor".to_string()));
        assert!(state.session().is_some());
    }

    #[test]
    fn cancel_is_a_no_op_while_exchanging() {
        let dir = tempfile::tempdir().unwrap();
        let state = AuthState::load(dir.path(), chosen(Arc::new(MemorySecretStore::new())), true);
        state.lock().sign_in.state = SignInState::Waiting;
        let g = state.lock().generation;
        assert!(cancel_pending(&state));
        assert_eq!(state.lock().sign_in.state, SignInState::Idle);
        assert_eq!(state.lock().generation, g + 1);

        state.lock().sign_in.state = SignInState::Exchanging;
        assert!(!cancel_pending(&state));
        assert_eq!(state.lock().sign_in.state, SignInState::Exchanging);
        assert_eq!(state.lock().generation, g + 1);
    }

    /// A sign-in whose generation went stale after it was stored (a sign-out
    /// while exchanging) is undone on disk too, not only in memory.
    #[tokio::test]
    async fn a_stale_sign_in_is_undone_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        let state = AuthState::load(dir.path(), chosen(mem.clone()), true);
        let generation = state.lock().generation;
        // What complete_sign_in left behind: tokens, then account.json.
        let mut new_account = account();
        new_account.user_id = "u2".into();
        let key = auth::session_key("https://rh.acme.dev", "u2");
        mem.set(&key, &tokens()).unwrap();
        AccountFile {
            account: Some(new_account.clone()),
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        // Meanwhile the user signed out.
        state.lock().generation += 1;
        // Logout goes to a closed port: refused at once, best effort.
        let client = AuthClient::new("http://127.0.0.1:9").unwrap();
        let session = Session::new(
            client,
            "https://rh.acme.dev",
            "u2",
            mem.clone(),
            serde_json::from_str(&tokens()).unwrap(),
        );
        let applied = commit_sign_in(
            &state,
            generation,
            new_account,
            session,
            discovery(DiscoveryStatus::Ok, true),
        )
        .await;
        assert!(!applied);
        assert!(state.status().account.is_none());
        assert!(mem.keys().is_empty(), "{:?}", mem.keys());
        assert!(AccountFile::load(dir.path()).unwrap().is_none());
    }

    #[tokio::test]
    async fn a_current_sign_in_is_applied() {
        let dir = tempfile::tempdir().unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        let state = AuthState::load(dir.path(), chosen(mem.clone()), true);
        let generation = state.lock().generation;
        state.lock().sign_in.state = SignInState::Exchanging;
        let session = Session::new(
            AuthClient::new("http://127.0.0.1:9").unwrap(),
            "https://rh.acme.dev",
            "u1",
            mem.clone(),
            serde_json::from_str(&tokens()).unwrap(),
        );
        let ok = commit_sign_in(
            &state,
            generation,
            account(),
            session,
            discovery(DiscoveryStatus::Ok, true),
        )
        .await;
        assert!(ok);
        let st = state.status();
        assert_eq!(st.account.unwrap().user_id, "u1");
        assert_eq!(st.sign_in.state, SignInState::Success);
    }

    #[test]
    fn a_browser_that_does_not_open_leaves_a_copyable_link() {
        let mut s = SignInStatus {
            state: SignInState::Waiting,
            ..SignInStatus::idle()
        };
        note_browser(&mut s, false, "https://rh.acme.dev/auth/authorize?x=1");
        assert_eq!(s.state, SignInState::Waiting);
        assert_eq!(s.message.as_deref(), Some(BROWSER_FAILED));
        assert_eq!(
            s.authorize_url.as_deref(),
            Some("https://rh.acme.dev/auth/authorize?x=1")
        );
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(
            json["authorizeUrl"],
            "https://rh.acme.dev/auth/authorize?x=1"
        );
        // "Open browser again" worked: the hint goes away.
        note_browser(&mut s, true, "https://rh.acme.dev/auth/authorize?x=1");
        assert!(s.message.is_none() && s.authorize_url.is_none());
        // Never on top of another state.
        let mut e = SignInStatus {
            state: SignInState::Error,
            message: Some("boom".into()),
            ..SignInStatus::idle()
        };
        note_browser(&mut e, false, "u");
        assert_eq!(e.message.as_deref(), Some("boom"));
        assert!(e.authorize_url.is_none());
    }

    fn account() -> Account {
        Account {
            server_url: "https://rh.acme.dev".into(),
            server_instance_id: "I1".into(),
            server_name: "Acme".into(),
            user_id: "u1".into(),
            email: "priya@acme.dev".into(),
            name: "Priya Shah".into(),
            method: "github".into(),
            session_id: "s1".into(),
        }
    }

    #[test]
    fn startup_states() {
        let dir = tempfile::tempdir().unwrap();
        // Signed out.
        let st =
            AuthState::load(dir.path(), chosen(Arc::new(MemorySecretStore::new())), true).status();
        assert!(st.account.is_none() && !st.sign_in_required);

        // Signed in, tokens present.
        let file = AccountFile {
            account: Some(account()),
            ..Default::default()
        };
        file.save(dir.path()).unwrap();
        let mem = Arc::new(MemorySecretStore::new());
        mem.set(
            "session:https://rh.acme.dev:u1",
            &serde_json::to_string(&auth::Tokens {
                access_token: "rha_a".into(),
                access_expires_at: now_ms() + 3_600_000,
                refresh_token: "rhr_r".into(),
                session_id: "s1".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let state = AuthState::load(dir.path(), chosen(mem.clone()), true);
        let st = state.status();
        assert_eq!(st.account.as_ref().map(|a| a.user_id.as_str()), Some("u1"));
        assert!(!st.sign_in_required);
        assert!(state.session().is_some());
        let json = serde_json::to_string(&st).unwrap();
        assert!(!json.contains("rha_") && !json.contains("rhr_"), "{json}");

        // Tokens missing: sign in again, account kept.
        let st =
            AuthState::load(dir.path(), chosen(Arc::new(MemorySecretStore::new())), true).status();
        assert!(st.account.is_some() && st.sign_in_required);

        // Keychain unreadable (denied twice): signed-out view with a banner.
        mem.set_fail_reads(true);
        let state = AuthState::load(dir.path(), chosen(mem), true);
        let st = state.status();
        assert!(st.keychain_unreadable);
        assert!(st.account.is_none());
        assert!(state.session().is_none());
    }

    #[test]
    fn a_new_instance_id_needs_a_new_sign_in() {
        let dir = tempfile::tempdir().unwrap();
        AccountFile {
            account: Some(account()),
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let state = AuthState::load(dir.path(), chosen(Arc::new(MemorySecretStore::new())), true);
        state.lock().sign_in_required = false;
        let body = json!({
            "software": "reqharbor-server", "apiVersion": 1, "instanceId": "I2",
            "auth": {"methods": [{"id": "password", "label": "Email"}]},
            "features": {"accounts": true}
        });
        let d = server_info::classify(
            "https://rh.acme.dev",
            Some((200, body.to_string().as_bytes())),
        );
        assert!(note_account_discovery(&state, &d).is_none());
        assert!(state.status().sign_in_required);
        let saved = AccountFile::load(dir.path()).unwrap().unwrap();
        assert!(saved.sign_in_required);
        assert_eq!(saved.server.unwrap().info.unwrap().instance_id, "I2");
    }

    #[test]
    fn an_account_without_an_instance_adopts_the_discovered_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = account();
        a.server_instance_id = String::new();
        AccountFile {
            account: Some(a),
            ..Default::default()
        }
        .save(dir.path())
        .unwrap();
        let state = AuthState::load(dir.path(), chosen(Arc::new(MemorySecretStore::new())), true);
        state.lock().sign_in_required = false;
        let body = json!({
            "software": "reqharbor-server", "apiVersion": 1, "instanceId": "I9",
            "auth": {"methods": [{"id": "password", "label": "Email"}]},
            "features": {"accounts": true}
        });
        let d = server_info::classify(
            "https://rh.acme.dev",
            Some((200, body.to_string().as_bytes())),
        );
        let filled = note_account_discovery(&state, &d).expect("instance adopted");
        assert_eq!(filled.server_instance_id, "I9");
        assert!(!state.status().sign_in_required, "not a different server");
        let saved = AccountFile::load(dir.path()).unwrap().unwrap();
        assert_eq!(saved.account.unwrap().server_instance_id, "I9");
    }
}

//! Desktop sign-in and sessions (04 §2.2, §3.2; 06-go-server §5.2, §5.8, §9).
//!
//! * [`PendingSignIn`]: PKCE S256, a random `state` and a `127.0.0.1`
//!   loopback listener ([`Loopback`]); the caller opens
//!   [`PendingSignIn::authorize_url`] in the system browser and waits for the
//!   matching `/cb`.
//! * [`AuthClient`]: the JSON calls (`/v1/auth/token` with RFC 6749 form
//!   fields, `/v1/auth/logout`, `/v1/me`, `/v1/me/sessions`).
//! * [`Session`]: the signed-in tokens. Access tokens are refreshed
//!   single-flight; a 401 refreshes once and retries; a failed refresh
//!   becomes [`AuthError::SignInRequired`] and never touches local data.
//!   New tokens are written to the [`SecretStore`] before they are used.
//! * [`AccountFile`]: `account.json` (non-secret account facts).
//!
//! No token ever appears in an error message, a log line, `account.json` or
//! `settings.json`.

use crate::model::{CLIENT_VERSION, Extra, now_ms};
use crate::secrets::{SecretError, SecretStore};
use crate::server_info::Discovery;
use crate::storage::{StoreError, atomic_write};
use crate::sync::VERSION_HEADER;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, mpsc};
use tokio_util::sync::CancellationToken;

/// The single public OAuth client (06 §5.2).
pub const CLIENT_ID: &str = "reqharbor-desktop";
/// A pending sign-in expires after 10 minutes (AC-AUTH-3).
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Refresh this long before the access token expires (04 §3.2).
pub const REFRESH_MARGIN_MS: i64 = 60_000;
/// Loopback limits (QA 9b, 06 §9): concurrent connections and the read
/// timeout of each.
pub const LOOPBACK_MAX_CONNS: usize = 8;
pub const LOOPBACK_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest request head the loopback reads.
const LOOPBACK_MAX_HEAD: usize = 8 * 1024;
/// HTTP timeout for the account calls.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
pub const ACCOUNT_FILE: &str = "account.json";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum AuthError {
    /// Network failure: the server could not be reached.
    #[error("Can't reach the server. Check your connection and try again.")]
    Offline(String),
    #[error("Sign-in was cancelled.")]
    Cancelled,
    #[error("Sign-in timed out. Try again.")]
    Timeout,
    /// The browser came back with `?error=` (e.g. `access_denied`).
    #[error("Sign-in was cancelled in the browser.")]
    Denied(String),
    #[error(
        "ReqHarbor couldn't start sign-in on this computer. You can still sync with a token under Sync › Advanced."
    )]
    LoopbackUnavailable(String),
    /// 400 `invalid_grant` from the token endpoint.
    #[error("Sign-in didn't finish. Start again from ReqHarbor.")]
    InvalidGrant,
    /// The session can no longer be refreshed (AC-AUTH-9).
    #[error("Your session ended. Sign in again to keep syncing.")]
    SignInRequired,
    /// A 401 from an authenticated call, with the server's code. Session
    /// callers refresh and retry; it never reaches the UI unhandled.
    #[error("The server did not accept the sign-in ({0}).")]
    Unauthorized(String),
    #[error("This server needs a newer version of ReqHarbor.")]
    ClientTooOld,
    #[error("Too many sign-in attempts. Try again in a minute.")]
    RateLimited,
    #[error("This server isn't accepting new accounts right now.")]
    ServerFull,
    #[error("Server error: {0}")]
    Server(String),
    #[error("Unexpected response from the server: {0}")]
    Protocol(String),
    #[error("{0}")]
    Rejected(String),
    #[error("ReqHarbor couldn't save your sign-in to the system keychain.")]
    Keychain(String),
    #[error("You're not signed in.")]
    NotSignedIn,
    #[error("Local storage error: {0}")]
    Store(String),
    /// A 4xx from a team or invite endpoint with the server's error code
    /// (`forbidden_role`, `last_owner`, `transfer_required`, `limit_teams`,
    /// `invite_invalid`, …): the UI words it by code.
    #[error("{message}")]
    Refused { code: String, message: String },
}

impl AuthError {
    /// Worth retrying later without user action.
    pub fn is_retryable(&self) -> bool {
        matches!(self, AuthError::Offline(_) | AuthError::Server(_))
    }
}

impl From<SecretError> for AuthError {
    fn from(e: SecretError) -> Self {
        AuthError::Keychain(e.to_string())
    }
}

impl From<StoreError> for AuthError {
    fn from(e: StoreError) -> Self {
        AuthError::Store(e.to_string())
    }
}

// ───────────────────────────── PKCE and randomness ─────────────────────────────

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// `n` random bytes from the OS CSPRNG, base64url without padding.
pub fn random_b64url(n: usize) -> String {
    use ring::rand::SecureRandom as _;
    let mut buf = vec![0u8; n];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .expect("the OS random generator failed");
    b64url(&buf)
}

/// `b64url(SHA256(verifier))` (RFC 7636 S256).
pub fn challenge_for(verifier: &str) -> String {
    b64url(ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes()).as_ref())
}

#[derive(Clone)]
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
}

impl Pkce {
    /// A 32-byte verifier (43 characters) and its S256 challenge.
    pub fn new() -> Self {
        let verifier = random_b64url(32);
        Pkce {
            challenge: challenge_for(&verifier),
            verifier,
        }
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for Pkce {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pkce")
            .field("challenge", &self.challenge)
            .finish_non_exhaustive()
    }
}

// ───────────────────────────── device name ─────────────────────────────

fn os_label() -> &'static str {
    match std::env::consts::OS {
        "macos" => "macOS",
        "windows" => "Windows",
        "linux" => "Linux",
        other => other,
    }
}

/// A character in Unicode general category L* or N*, exactly what the
/// server's `\p{L}\p{N}` accepts. `char::is_alphanumeric` is not enough: the
/// Alphabetic property also covers combining marks (U+093E DEVANAGARI VOWEL
/// SIGN AA, U+0E31 THAI MAI HAN-AKAT) and some symbols (Ⓐ), which the server
/// refuses.
pub fn is_letter_or_number(c: char) -> bool {
    use icu_properties::CodePointMapData;
    use icu_properties::props::{GeneralCategory, GeneralCategoryGroup};
    let gc = CodePointMapData::<GeneralCategory>::new().get(c);
    GeneralCategoryGroup::Letter.contains(gc) || GeneralCategoryGroup::Number.contains(gc)
}

/// Keep `[\p{L}\p{N} ()._-]`, collapse spaces, at most `max` characters.
pub fn sanitize_device_part(raw: &str, max: usize) -> String {
    let kept: String = raw
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|&c| is_letter_or_number(c) || matches!(c, ' ' | '(' | ')' | '.' | '_' | '-'))
        .collect();
    let collapsed = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .chars()
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

/// "MacBook Pro (macOS)": the computer name plus the OS, at most 64
/// characters from the class the server accepts (06 §5.2).
pub fn device_name_from(computer: &str) -> String {
    let os = os_label();
    let suffix = format!(" ({os})");
    let room = 64usize.saturating_sub(suffix.chars().count());
    let name = sanitize_device_part(computer, room);
    if name.is_empty() {
        format!("ReqHarbor{suffix}")
    } else {
        format!("{name}{suffix}")
    }
}

fn command_output(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// This computer's name for the Devices list (AC-AUTH-10).
pub fn device_name() -> String {
    let computer = if cfg!(target_os = "macos") {
        command_output("scutil", &["--get", "ComputerName"])
    } else {
        None
    }
    .or_else(|| std::env::var("COMPUTERNAME").ok())
    .or_else(|| {
        std::fs::read_to_string("/etc/hostname")
            .ok()
            .map(|s| s.trim().to_string())
    })
    .or_else(|| command_output("hostname", &[]))
    .unwrap_or_default();
    device_name_from(&computer)
}

// ───────────────────────────── authorize URL ─────────────────────────────

/// `{server}/auth/authorize?…` (06 §5.2 step 3).
pub fn authorize_url(
    server: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
    device_name: &str,
    method: Option<&str>,
) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    q.append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", state)
        .append_pair("device_name", device_name);
    if let Some(m) = method.filter(|m| !m.is_empty()) {
        q.append_pair("method", m);
    }
    format!("{server}/auth/authorize?{}", q.finish())
}

// ───────────────────────────── loopback listener ─────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct LoopbackConfig {
    pub max_conns: usize,
    pub read_timeout: Duration,
    pub timeout: Duration,
}

impl Default for LoopbackConfig {
    fn default() -> Self {
        LoopbackConfig {
            max_conns: LOOPBACK_MAX_CONNS,
            read_timeout: LOOPBACK_READ_TIMEOUT,
            timeout: SIGN_IN_TIMEOUT,
        }
    }
}

/// The `127.0.0.1:<ephemeral>` listener that receives the browser redirect
/// (RFC 8252). It keeps accepting until a `GET /cb` with the expected
/// `state` arrives, the timeout passes or the sign-in is cancelled; stray
/// requests get 404, a wrong `state` gets 400, and neither stops it.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
    peak: Arc<AtomicUsize>,
}

/// The address every sign-in listens on: an IPv4 loopback literal and an
/// ephemeral port (the server only accepts `http://127.0.0.1:<port>/cb`).
pub const LOOPBACK_ADDR: (std::net::Ipv4Addr, u16) = (std::net::Ipv4Addr::LOCALHOST, 0);

/// A failed bind (or `local_addr`) is [`AuthError::LoopbackUnavailable`]
/// with only the error kind, never a path or address.
pub fn loopback_error(e: &std::io::Error) -> AuthError {
    AuthError::LoopbackUnavailable(e.kind().to_string())
}

impl Loopback {
    pub async fn bind() -> Result<Self, AuthError> {
        Self::bind_on(LOOPBACK_ADDR).await
    }

    /// Bind a specific address (tests use a taken port to make it fail).
    pub async fn bind_on(addr: impl tokio::net::ToSocketAddrs) -> Result<Self, AuthError> {
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| loopback_error(&e))?;
        let port = listener
            .local_addr()
            .map_err(|e| loopback_error(&e))?
            .port();
        Ok(Loopback {
            listener,
            port,
            peak: Arc::new(AtomicUsize::new(0)),
        })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/cb", self.port)
    }

    /// The highest number of connections handled at once (for tests).
    pub fn peak_concurrency(&self) -> Arc<AtomicUsize> {
        self.peak.clone()
    }

    /// Wait for the matching callback. A hit answers 302 to `done_url`
    /// (the server's "You're signed in" page) and returns the code.
    pub async fn wait(
        self,
        state: &str,
        done_url: &str,
        cfg: LoopbackConfig,
        cancel: CancellationToken,
    ) -> Result<String, AuthError> {
        let sem = Arc::new(Semaphore::new(cfg.max_conns.max(1)));
        let (tx, mut rx) = mpsc::channel::<Result<String, AuthError>>(1);
        let active = Arc::new(AtomicUsize::new(0));
        let state: Arc<str> = Arc::from(state);
        let done_url: Arc<str> = Arc::from(done_url);
        let deadline = tokio::time::sleep(cfg.timeout);
        tokio::pin!(deadline);
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(AuthError::Cancelled),
                Some(outcome) = rx.recv() => return outcome,
                _ = &mut deadline => return Err(AuthError::Timeout),
                accepted = async {
                    // Wait for a free slot before accepting: extra connections
                    // queue in the kernel backlog instead of being handled.
                    let permit = sem.clone().acquire_owned().await;
                    (permit, self.listener.accept().await)
                } => {
                    let (Ok(permit), Ok((stream, _))) = accepted else { continue };
                    let ctx = ConnCtx {
                        state: state.clone(),
                        done_url: done_url.clone(),
                        tx: tx.clone(),
                        active: active.clone(),
                        peak: self.peak.clone(),
                        read_timeout: cfg.read_timeout,
                    };
                    tokio::spawn(async move {
                        handle_conn(stream, ctx).await;
                        drop(permit);
                    });
                }
            }
        }
    }
}

struct ConnCtx {
    state: Arc<str>,
    done_url: Arc<str>,
    tx: mpsc::Sender<Result<String, AuthError>>,
    active: Arc<AtomicUsize>,
    peak: Arc<AtomicUsize>,
    read_timeout: Duration,
}

struct ActiveGuard(Arc<AtomicUsize>);
impl Drop for ActiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

async fn read_head(stream: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    loop {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            buf.truncate(end);
            return String::from_utf8(buf).ok();
        }
        if buf.len() > LOOPBACK_MAX_HEAD {
            return None;
        }
    }
}

const PAGE_STYLE: &str = "font:15px system-ui,sans-serif;max-width:28rem;margin:4rem auto;padding:0 1rem;line-height:1.5";

fn page(title: &str, text: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{title}</title><body style=\"{PAGE_STYLE}\"><h1 style=\"font-size:1.25rem\">{title}</h1><p>{text}</p>"
    )
}

async fn respond(stream: &mut TcpStream, status: &str, location: Option<&str>, body: &str) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(l) = location {
        head.push_str(&format!("Location: {l}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(body.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// What one request line asks of the loopback.
#[derive(Debug, PartialEq, Eq)]
enum CbRequest {
    NotFound,
    MethodNotAllowed,
    WrongState,
    Code(String),
    Error(String),
    Malformed,
}

fn route(request_line: &str, expected_state: &str) -> CbRequest {
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return CbRequest::Malformed;
    };
    let Ok(u) = url::Url::parse(&format!("http://127.0.0.1{target}")) else {
        return CbRequest::Malformed;
    };
    if u.path() != "/cb" {
        return CbRequest::NotFound;
    }
    if method != "GET" {
        return CbRequest::MethodNotAllowed;
    }
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (k, v) in u.query_pairs() {
        match &*k {
            "state" => state = Some(v.into_owned()),
            "code" => code = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if state.as_deref() != Some(expected_state) {
        return CbRequest::WrongState;
    }
    match (code, error) {
        (_, Some(e)) => CbRequest::Error(e),
        (Some(c), None) if !c.is_empty() => CbRequest::Code(c),
        _ => CbRequest::Malformed,
    }
}

async fn handle_conn(mut stream: TcpStream, ctx: ConnCtx) {
    let now = ctx.active.fetch_add(1, Ordering::SeqCst) + 1;
    ctx.peak.fetch_max(now, Ordering::SeqCst);
    let _guard = ActiveGuard(ctx.active.clone());
    let Ok(Some(head)) = tokio::time::timeout(ctx.read_timeout, read_head(&mut stream)).await
    else {
        return;
    };
    let line = head.lines().next().unwrap_or_default();
    let write = async {
        match route(line, &ctx.state) {
            CbRequest::Code(code) => {
                respond(
                    &mut stream,
                    "302 Found",
                    Some(&ctx.done_url),
                    &page(
                        "You're signed in",
                        "You can close this tab and return to ReqHarbor.",
                    ),
                )
                .await;
                let _ = ctx.tx.try_send(Ok(code));
            }
            CbRequest::Error(e) => {
                respond(
                    &mut stream,
                    "200 OK",
                    None,
                    &page(
                        "Sign-in was cancelled",
                        "You can close this tab and return to ReqHarbor.",
                    ),
                )
                .await;
                let _ = ctx.tx.try_send(Err(AuthError::Denied(e)));
            }
            CbRequest::WrongState | CbRequest::Malformed => {
                respond(
                    &mut stream,
                    "400 Bad Request",
                    None,
                    &page(
                        "This sign-in link doesn't match",
                        "Return to ReqHarbor and start again.",
                    ),
                )
                .await
            }
            CbRequest::MethodNotAllowed => {
                respond(&mut stream, "405 Method Not Allowed", None, "").await
            }
            CbRequest::NotFound => respond(&mut stream, "404 Not Found", None, "").await,
        }
    };
    let _ = tokio::time::timeout(ctx.read_timeout, write).await;
}

// ───────────────────────────── wire types ─────────────────────────────

/// `user` in token and `/v1/me` responses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct User {
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, alias = "avatar_url")]
    pub avatar_url: Option<String>,
    #[serde(default, alias = "is_admin")]
    pub is_admin: bool,
}

/// `POST /v1/auth/token` response (RFC 6749 §5.1 + `session_id`, `user`).
#[derive(Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub token_type: String,
    #[serde(default = "default_expires_in")]
    pub expires_in: i64,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub user: Option<User>,
}

fn default_expires_in() -> i64 {
    3600
}

impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("session_id", &self.session_id)
            .field("expires_in", &self.expires_in)
            .finish_non_exhaustive()
    }
}

/// `GET /v1/me`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub user: User,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub session_id: String,
}

/// One row of `GET /v1/me/sessions` (AC-AUTH-10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub created_at: Option<i64>,
    /// Unix ms of the day the session was last used.
    #[serde(default, deserialize_with = "lenient_day")]
    pub last_used_day: Option<i64>,
    #[serde(default)]
    pub current: bool,
}

/// One row of `GET /v1/me/spaces` (06 §4; the shape is final in M1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MySpace {
    pub space_id: String,
    /// `personal` | `team` | …
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default)]
    pub role: Option<String>,
    #[serde(default)]
    pub bytes_used: Option<u64>,
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    /// Team spaces (M2): how many members can see it.
    #[serde(default)]
    pub member_count: Option<u64>,
}

/// A space this account left or lost (M2): `reason` is `removed`, `left`
/// or `deleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Departed {
    pub space_id: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub team_id: Option<String>,
    #[serde(default)]
    pub team_name: Option<String>,
    #[serde(default, deserialize_with = "lenient_ms")]
    pub at: Option<i64>,
}

/// `GET /v1/me/spaces`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MySpaces {
    pub spaces: Vec<MySpace>,
    #[serde(default)]
    pub departed: Vec<Departed>,
}

const DAY_MS: i64 = 86_400_000;

/// Seconds, milliseconds or an ISO date, as ms.
fn to_ms(v: &serde_json::Value) -> Option<i64> {
    match v {
        serde_json::Value::Number(n) => {
            let n = n.as_i64()?;
            // Below year 5138 in seconds; above in ms.
            Some(if n < 100_000_000_000 { n * 1000 } else { n })
        }
        serde_json::Value::String(s) => parse_iso_day(s),
        _ => None,
    }
}

fn lenient_ms<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    Ok(to_ms(&serde_json::Value::deserialize(d)?))
}

/// `last_used_day` is an INTEGER on the server: days since the epoch, or a
/// timestamp; an ISO date is accepted too.
fn lenient_day<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<i64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(match &v {
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(days) if (0..1_000_000).contains(&days) => Some(days * DAY_MS),
            _ => to_ms(&v),
        },
        _ => to_ms(&v),
    })
}

/// `YYYY-MM-DD` (optionally followed by a time) as Unix ms at 00:00 UTC.
fn parse_iso_day(s: &str) -> Option<i64> {
    let date = s.get(..10)?;
    let mut it = date.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146_097 + doe - 719_468) * DAY_MS)
}

#[derive(Deserialize)]
struct ErrorBody {
    error: ErrorDetail,
}
#[derive(Deserialize)]
struct ErrorDetail {
    code: String,
    #[serde(default)]
    message: String,
}

/// The JSON client for the account endpoints of one server origin.
#[derive(Clone)]
pub struct AuthClient {
    http: reqwest::Client,
    base: String,
}

impl AuthClient {
    pub fn new(origin: &str) -> Result<Self, AuthError> {
        crate::http::install_crypto_provider();
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            VERSION_HEADER,
            reqwest::header::HeaderValue::from_static(CLIENT_VERSION),
        );
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .user_agent(crate::http::USER_AGENT)
            .default_headers(headers)
            .build()
            .map_err(|e| AuthError::Protocol(e.to_string()))?;
        Ok(AuthClient {
            http,
            base: origin.trim_end_matches('/').to_string(),
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<Vec<u8>, AuthError> {
        let resp = rb
            .send()
            .await
            .map_err(|e| AuthError::Offline(without_url(&e)))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AuthError::Offline(without_url(&e)))?;
        if (200..300).contains(&status) {
            return Ok(bytes.to_vec());
        }
        let parsed = serde_json::from_slice::<ErrorBody>(&bytes).ok();
        let code = parsed
            .as_ref()
            .map(|b| b.error.code.clone())
            .unwrap_or_default();
        let message = parsed
            .as_ref()
            .map(|b| b.error.message.clone())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(match (status, code.as_str()) {
            (_, "invalid_grant") => AuthError::InvalidGrant,
            (401, _) => AuthError::Unauthorized(if code.is_empty() {
                "unauthorized".into()
            } else {
                code
            }),
            (426, _) | (_, "client_too_old") => AuthError::ClientTooOld,
            (429, _) => AuthError::RateLimited,
            (507, _) => AuthError::ServerFull,
            (500..=599, _) => AuthError::Server(message),
            (415, _) => AuthError::Protocol(message),
            _ => AuthError::Rejected(message),
        })
    }

    async fn json<T: serde::de::DeserializeOwned>(
        &self,
        rb: reqwest::RequestBuilder,
    ) -> Result<T, AuthError> {
        let bytes = self.send(rb).await?;
        serde_json::from_slice(&bytes).map_err(|e| AuthError::Protocol(e.to_string()))
    }

    async fn token(&self, form: &[(&str, &str)]) -> Result<TokenResponse, AuthError> {
        let body = {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            for (k, v) in form {
                ser.append_pair(k, v);
            }
            ser.finish()
        };
        let rb = self
            .http
            .post(self.url("/v1/auth/token"))
            .header(
                reqwest::header::CONTENT_TYPE,
                "application/x-www-form-urlencoded",
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body);
        match self.json::<TokenResponse>(rb).await {
            // The token endpoint answers a dead refresh token with 401
            // `session_revoked` (reuse) or 400 `invalid_grant`.
            Err(AuthError::Unauthorized(_)) => Err(AuthError::InvalidGrant),
            Ok(t) if t.access_token.is_empty() || t.refresh_token.is_empty() => {
                Err(AuthError::Protocol("token response without tokens".into()))
            }
            other => other,
        }
    }

    /// `grant_type=authorization_code` (06 §5.2 step 5).
    pub async fn exchange_code(
        &self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<TokenResponse, AuthError> {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("client_id", CLIENT_ID),
            ("code", code),
            ("code_verifier", verifier),
            ("redirect_uri", redirect_uri),
        ])
        .await
    }

    /// `grant_type=refresh_token` (06 §5.8).
    pub async fn refresh(&self, refresh_token: &str) -> Result<TokenResponse, AuthError> {
        self.token(&[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("refresh_token", refresh_token),
        ])
        .await
    }

    /// `POST /v1/auth/logout`: revokes the calling session.
    pub async fn logout(&self, access: &str) -> Result<(), AuthError> {
        self.send(
            self.http
                .post(self.url("/v1/auth/logout"))
                .bearer_auth(access),
        )
        .await
        .map(|_| ())
    }

    pub async fn me(&self, access: &str) -> Result<Me, AuthError> {
        self.json(self.http.get(self.url("/v1/me")).bearer_auth(access))
            .await
    }

    pub async fn sessions(&self, access: &str) -> Result<Vec<SessionInfo>, AuthError> {
        #[derive(Deserialize)]
        struct List {
            sessions: Vec<SessionInfo>,
        }
        let list: List = self
            .json(
                self.http
                    .get(self.url("/v1/me/sessions"))
                    .bearer_auth(access),
            )
            .await?;
        Ok(list.sessions)
    }

    /// `GET /v1/me/spaces` (04 §6.6 step 1).
    pub async fn spaces(&self, access: &str) -> Result<MySpaces, AuthError> {
        self.json(self.http.get(self.url("/v1/me/spaces")).bearer_auth(access))
            .await
    }

    /// A JSON call to a team or invite endpoint (M2). `access` is `None` for
    /// the unauthenticated invite preview and ticket. An empty body (204)
    /// reads as JSON `null`. A 4xx with an error code becomes
    /// [`AuthError::Refused`] so the caller can word it; 401 stays
    /// [`AuthError::Unauthorized`] for the refresh-and-retry.
    pub async fn api<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        access: Option<&str>,
        body: Option<&serde_json::Value>,
    ) -> Result<T, AuthError> {
        let mut rb = self
            .http
            .request(method, self.url(path))
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(a) = access {
            rb = rb.bearer_auth(a);
        }
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let resp = rb
            .send()
            .await
            .map_err(|e| AuthError::Offline(without_url(&e)))?;
        let status = resp.status().as_u16();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| AuthError::Offline(without_url(&e)))?;
        if (200..300).contains(&status) {
            let raw: &[u8] = if bytes.iter().all(u8::is_ascii_whitespace) {
                b"null"
            } else {
                &bytes
            };
            return serde_json::from_slice(raw).map_err(|e| AuthError::Protocol(e.to_string()));
        }
        let parsed = serde_json::from_slice::<ErrorBody>(&bytes).ok();
        let code = parsed
            .as_ref()
            .map(|b| b.error.code.clone())
            .unwrap_or_default();
        let message = parsed
            .as_ref()
            .map(|b| b.error.message.clone())
            .filter(|m| !m.is_empty())
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(match (status, code.as_str()) {
            (401, _) => AuthError::Unauthorized(if code.is_empty() {
                "unauthorized".into()
            } else {
                code
            }),
            (426, _) | (_, "client_too_old") => AuthError::ClientTooOld,
            (429, _) => AuthError::RateLimited,
            (507, _) => AuthError::ServerFull,
            (500..=599, _) => AuthError::Server(message),
            (_, "") => AuthError::Rejected(message),
            _ => AuthError::Refused { code, message },
        })
    }

    pub async fn revoke_session(&self, access: &str, id: &str) -> Result<(), AuthError> {
        let path = format!(
            "/v1/me/sessions/{}",
            percent_encoding::utf8_percent_encode(id, percent_encoding::NON_ALPHANUMERIC)
        );
        self.send(self.http.delete(self.url(&path)).bearer_auth(access))
            .await
            .map(|_| ())
    }
}

/// A reqwest error without the URL (it never holds a token, but the
/// message stays short).
fn without_url(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    if let Some(u) = e.url() {
        s = s.replace(&format!(" for url ({u})"), "");
    }
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "connection failed".into()
    } else {
        s
    }
}

// ───────────────────────────── tokens and sessions ─────────────────────────────

/// What the secret store keeps per session, as one JSON value so each write
/// is atomic (04 §3.3).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub access_token: String,
    /// Unix ms.
    pub access_expires_at: i64,
    pub refresh_token: String,
    pub session_id: String,
}

impl std::fmt::Debug for Tokens {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tokens")
            .field("access_expires_at", &self.access_expires_at)
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

impl Tokens {
    pub fn from_response(r: &TokenResponse, now: i64, fallback_session: &str) -> Self {
        Tokens {
            access_token: r.access_token.clone(),
            access_expires_at: now + r.expires_in.clamp(0, 365 * 24 * 3600) * 1000,
            refresh_token: r.refresh_token.clone(),
            session_id: if r.session_id.is_empty() {
                fallback_session.to_string()
            } else {
                r.session_id.clone()
            },
        }
    }
}

/// Keychain entry for a session: `session:<serverOrigin>:<userId>` (06 §9),
/// so sessions on different servers never collide.
pub fn session_key(server_origin: &str, user_id: &str) -> String {
    format!("session:{server_origin}:{user_id}")
}

/// Run a blocking secret-store call off the async runtime.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, SecretError> + Send + 'static,
) -> Result<T, SecretError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| SecretError::Io(e.to_string()))?
}

pub async fn write_tokens(
    secrets: Arc<dyn SecretStore>,
    key: String,
    tokens: &Tokens,
) -> Result<(), SecretError> {
    let json = serde_json::to_string(tokens).map_err(|e| SecretError::Io(e.to_string()))?;
    blocking(move || secrets.set(&key, &json)).await
}

pub async fn read_tokens(
    secrets: Arc<dyn SecretStore>,
    key: String,
) -> Result<Option<Tokens>, SecretError> {
    let raw = blocking(move || secrets.get(&key)).await?;
    Ok(raw.and_then(|s| serde_json::from_str(&s).ok()))
}

pub async fn delete_tokens(secrets: Arc<dyn SecretStore>, key: String) -> Result<(), SecretError> {
    blocking(move || secrets.delete(&key)).await
}

/// The signed-in session for one account (04 §3.2).
pub struct Session {
    client: AuthClient,
    key: String,
    secrets: Arc<dyn SecretStore>,
    tokens: Mutex<Tokens>,
    sign_in_required: AtomicBool,
    refreshes: AtomicUsize,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("key", &self.key)
            .finish_non_exhaustive()
    }
}

impl Session {
    pub fn new(
        client: AuthClient,
        server_origin: &str,
        user_id: &str,
        secrets: Arc<dyn SecretStore>,
        tokens: Tokens,
    ) -> Self {
        Session {
            client,
            key: session_key(server_origin, user_id),
            secrets,
            tokens: Mutex::new(tokens),
            sign_in_required: AtomicBool::new(false),
            refreshes: AtomicUsize::new(0),
        }
    }

    /// The session for `account`, from the secret store. `Ok(None)` when the
    /// store has no (readable) entry for it.
    pub async fn load(
        account: &Account,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Option<Session>, AuthError> {
        let key = session_key(&account.server_url, &account.user_id);
        let Some(tokens) = read_tokens(secrets.clone(), key).await? else {
            return Ok(None);
        };
        let client = AuthClient::new(&account.server_url)?;
        Ok(Some(Session::new(
            client,
            &account.server_url,
            &account.user_id,
            secrets,
            tokens,
        )))
    }

    pub fn client(&self) -> &AuthClient {
        &self.client
    }

    pub fn secret_key(&self) -> &str {
        &self.key
    }

    pub fn sign_in_required(&self) -> bool {
        self.sign_in_required.load(Ordering::SeqCst)
    }

    /// Number of refresh calls made (for tests and diagnostics).
    pub fn refresh_count(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }

    pub async fn session_id(&self) -> String {
        self.tokens.lock().await.session_id.clone()
    }

    /// A valid access token, refreshing first when it expires within
    /// [`REFRESH_MARGIN_MS`]. Concurrent callers share one refresh.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        if self.sign_in_required() {
            return Err(AuthError::SignInRequired);
        }
        let mut t = self.tokens.lock().await;
        if now_ms() >= t.access_expires_at - REFRESH_MARGIN_MS {
            self.refresh_locked(&mut t).await?;
        }
        Ok(t.access_token.clone())
    }

    /// After a 401 with `used`: refresh, unless another caller already did
    /// (single-flight). Returns the token to retry with.
    pub async fn refresh_if_current(&self, used: &str) -> Result<String, AuthError> {
        if self.sign_in_required() {
            return Err(AuthError::SignInRequired);
        }
        let mut t = self.tokens.lock().await;
        if t.access_token == used {
            self.refresh_locked(&mut t).await?;
        }
        Ok(t.access_token.clone())
    }

    async fn refresh_locked(&self, t: &mut Tokens) -> Result<(), AuthError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        match self.client.refresh(&t.refresh_token).await {
            Ok(resp) => {
                let fresh = Tokens::from_response(&resp, now_ms(), &t.session_id);
                // Persist before use: if this write fails, memory keeps the
                // old pair, and the server's 60 s grace window returns the
                // same new pair on the next try.
                write_tokens(self.secrets.clone(), self.key.clone(), &fresh).await?;
                *t = fresh;
                Ok(())
            }
            Err(AuthError::InvalidGrant | AuthError::Unauthorized(_)) => {
                self.sign_in_required.store(true, Ordering::SeqCst);
                Err(AuthError::SignInRequired)
            }
            Err(e) => Err(e),
        }
    }

    /// Run an authenticated call. A 401 refreshes once and retries once; a
    /// second 401 means the session is gone ([`AuthError::SignInRequired`]).
    pub async fn call<T, F, Fut>(&self, f: F) -> Result<T, AuthError>
    where
        F: Fn(String) -> Fut,
        Fut: std::future::Future<Output = Result<T, AuthError>>,
    {
        let token = self.access_token().await?;
        match f(token.clone()).await {
            Err(AuthError::Unauthorized(_)) => {
                let retry = self.refresh_if_current(&token).await?;
                match f(retry).await {
                    Err(AuthError::Unauthorized(_)) => {
                        self.sign_in_required.store(true, Ordering::SeqCst);
                        Err(AuthError::SignInRequired)
                    }
                    other => other,
                }
            }
            other => other,
        }
    }

    pub async fn me(&self) -> Result<Me, AuthError> {
        self.call(|t| async move { self.client.me(&t).await }).await
    }

    pub async fn sessions(&self) -> Result<Vec<SessionInfo>, AuthError> {
        self.call(|t| async move { self.client.sessions(&t).await })
            .await
    }

    pub async fn revoke_session(&self, id: &str) -> Result<(), AuthError> {
        self.call(|t| async move { self.client.revoke_session(&t, id).await })
            .await
    }

    pub async fn spaces(&self) -> Result<MySpaces, AuthError> {
        self.call(|t| async move { self.client.spaces(&t).await })
            .await
    }

    /// A second 401 after a refresh on another client (space sync): the
    /// session is gone (AC-AUTH-9).
    pub fn mark_sign_in_required(&self) {
        self.sign_in_required.store(true, Ordering::SeqCst);
    }

    /// The server origin this session talks to.
    pub fn origin(&self) -> &str {
        self.client.base_url()
    }

    /// Revoke this session on the server (best effort: a dead or
    /// unreachable session is still signed out locally by the caller).
    pub async fn logout(&self) -> Result<(), AuthError> {
        let token = self.tokens.lock().await.access_token.clone();
        match self.client.logout(&token).await {
            Err(AuthError::Unauthorized(_)) => {
                // Expired access token: refresh once so the server can revoke.
                let retry = self.refresh_if_current(&token).await?;
                self.client.logout(&retry).await
            }
            other => other,
        }
    }

    /// Drop the keychain entry.
    pub async fn forget(&self) -> Result<(), AuthError> {
        delete_tokens(self.secrets.clone(), self.key.clone()).await?;
        Ok(())
    }
}

// ───────────────────────────── account.json ─────────────────────────────

/// The signed-in account (06 §9 "Per-server identity"). Never holds a token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// Normalised origin the app talks to.
    pub server_url: String,
    #[serde(default)]
    pub server_instance_id: String,
    #[serde(default)]
    pub server_name: String,
    pub user_id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    /// "password" | "github" | "google" | "oidc" | …
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub session_id: String,
}

/// `account.json` (04 §6.1). C1 writes `account`, the cached discovery of
/// its server and the sign-in-required flag; later milestones add their own
/// keys, which are passed through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountFile {
    #[serde(default = "schema_one")]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<Account>,
    /// The account server's last discovery answer (the 24 h offline cache).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<Discovery>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub sign_in_required: bool,
    /// Space bindings (04 §6.1; C2). Written by the store only.
    #[serde(flatten)]
    pub bindings: crate::spaces::Bindings,
    #[serde(flatten)]
    pub extra: Extra,
}

fn schema_one() -> u32 {
    1
}
fn is_false(b: &bool) -> bool {
    !*b
}

impl Default for AccountFile {
    fn default() -> Self {
        AccountFile {
            schema_version: 1,
            account: None,
            server: None,
            sign_in_required: false,
            bindings: crate::spaces::Bindings::default(),
            extra: Extra::new(),
        }
    }
}

/// Serialises every read-modify-write of `account.json` in this process:
/// the sign-in runtime writes the account part and the store writes the
/// bindings, from different threads.
static ACCOUNT_FILE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl AccountFile {
    /// `Ok(None)` when the file is missing. A damaged file is renamed to
    /// `account.corrupt-<ts>.json` and reads as missing.
    pub fn load(dir: &Path) -> Result<Option<AccountFile>, StoreError> {
        let path = dir.join(ACCOUNT_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        match serde_json::from_slice::<AccountFile>(&bytes) {
            Ok(f) => Ok(Some(f)),
            Err(_) => {
                let _ = std::fs::rename(
                    &path,
                    dir.join(format!("account.corrupt-{}.json", now_ms())),
                );
                Ok(None)
            }
        }
    }

    pub fn save(&self, dir: &Path) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| StoreError::Io(e.to_string()))?;
        atomic_write(&dir.join(ACCOUNT_FILE), &bytes)
    }

    /// Load (or start) the file, change it, and write it (or remove it when
    /// nothing is left), all under one process-wide lock. Returns the file
    /// as written.
    pub fn update(dir: &Path, f: impl FnOnce(&mut AccountFile)) -> Result<AccountFile, StoreError> {
        let _guard = ACCOUNT_FILE_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let mut file = AccountFile::load(dir)?.unwrap_or_default();
        f(&mut file);
        file.save_or_remove(dir)?;
        Ok(file)
    }

    /// Write the file, or remove it when nothing is left in it (pure-legacy
    /// mode means `account.json` is absent, 04 §6.1).
    pub fn save_or_remove(&self, dir: &Path) -> Result<(), StoreError> {
        if self.account.is_none() && self.extra.is_empty() && self.bindings.is_empty() {
            return match std::fs::remove_file(dir.join(ACCOUNT_FILE)) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        }
        self.save(dir)
    }
}

// ───────────────────────────── the sign-in flow ─────────────────────────────

/// A started sign-in: the listener is bound and the authorize URL is ready
/// to open in the system browser.
pub struct PendingSignIn {
    pub server_url: String,
    pub authorize_url: String,
    pub redirect_uri: String,
    loopback: Loopback,
    pkce: Pkce,
    state: String,
}

/// The code from the matching callback, ready for the exchange.
pub struct CodeReceived {
    pub server_url: String,
    pub redirect_uri: String,
    code: String,
    verifier: String,
}

impl PendingSignIn {
    /// Bind `127.0.0.1:0` and build the authorize URL for `server_url` (a
    /// normalised origin). A bind failure is
    /// [`AuthError::LoopbackUnavailable`].
    pub async fn start(
        server_url: &str,
        method: Option<&str>,
        device_name: &str,
    ) -> Result<Self, AuthError> {
        Self::start_on(LOOPBACK_ADDR, server_url, method, device_name).await
    }

    /// [`PendingSignIn::start`] on a given listener address (for tests).
    pub async fn start_on(
        addr: impl tokio::net::ToSocketAddrs,
        server_url: &str,
        method: Option<&str>,
        device_name: &str,
    ) -> Result<Self, AuthError> {
        let loopback = Loopback::bind_on(addr).await?;
        let pkce = Pkce::new();
        let state = random_b64url(16);
        let redirect_uri = loopback.redirect_uri();
        let authorize_url = authorize_url(
            server_url,
            &redirect_uri,
            &pkce.challenge,
            &state,
            device_name,
            method,
        );
        Ok(PendingSignIn {
            server_url: server_url.to_string(),
            authorize_url,
            redirect_uri,
            loopback,
            pkce,
            state,
        })
    }

    pub fn port(&self) -> u16 {
        self.loopback.port()
    }

    /// Add a signup ticket from `POST /v1/invites/ticket` (06 §10 M2), so a
    /// not-yet-registered invitee can sign up while signup is invite-only.
    /// The invite token itself never appears in a URL (PRIV-4).
    pub fn with_ticket(mut self, ticket: Option<&str>) -> Self {
        if let Some(t) = ticket.map(str::trim).filter(|t| !t.is_empty()) {
            let mut q = url::form_urlencoded::Serializer::new(String::new());
            q.append_pair("ticket", t);
            self.authorize_url = format!("{}&{}", self.authorize_url, q.finish());
        }
        self
    }

    pub fn peak_concurrency(&self) -> Arc<AtomicUsize> {
        self.loopback.peak_concurrency()
    }

    pub fn state(&self) -> &str {
        &self.state
    }

    /// Wait for the browser to come back to the loopback.
    pub async fn wait(
        self,
        cfg: LoopbackConfig,
        cancel: CancellationToken,
    ) -> Result<CodeReceived, AuthError> {
        let done = format!("{}/auth/done", self.server_url);
        let code = self.loopback.wait(&self.state, &done, cfg, cancel).await?;
        Ok(CodeReceived {
            server_url: self.server_url,
            redirect_uri: self.redirect_uri,
            code,
            verifier: self.pkce.verifier,
        })
    }
}

impl CodeReceived {
    pub async fn exchange(&self, client: &AuthClient) -> Result<TokenResponse, AuthError> {
        client
            .exchange_code(&self.code, &self.verifier, &self.redirect_uri)
            .await
    }
}

/// What the server tells us about itself at sign-in time.
#[derive(Debug, Clone, Default)]
pub struct ServerIdentity {
    pub instance_id: String,
    pub name: String,
}

/// Finish a sign-in after a successful exchange (06 §5.2 step 6):
/// 1. the tokens go to the secret store **first**;
/// 2. `/v1/me` is read with them (best effort, for the method and profile);
/// 3. `account.json` is written;
/// 4. a previous account's keychain entry is deleted.
///
/// If step 1 fails nothing else happens, so no account points at missing
/// tokens.
pub async fn complete_sign_in(
    dir: &Path,
    secrets: Arc<dyn SecretStore>,
    client: &AuthClient,
    server: &ServerIdentity,
    discovery: Option<Discovery>,
    requested_method: Option<&str>,
    resp: TokenResponse,
) -> Result<(Account, Session), AuthError> {
    let user = resp
        .user
        .clone()
        .ok_or_else(|| AuthError::Protocol("token response without user".into()))?;
    if user.id.is_empty() {
        return Err(AuthError::Protocol("token response without user id".into()));
    }
    let origin = client.base_url().to_string();
    let tokens = Tokens::from_response(&resp, now_ms(), "");
    write_tokens(secrets.clone(), session_key(&origin, &user.id), &tokens).await?;
    let session = Session::new(client.clone(), &origin, &user.id, secrets.clone(), tokens);
    let me = session.me().await.ok();
    let user = me.as_ref().map(|m| m.user.clone()).unwrap_or(user);
    let method = me
        .as_ref()
        .map(|m| m.method.clone())
        .filter(|m| !m.is_empty())
        .or_else(|| requested_method.map(str::to_string))
        .unwrap_or_default();
    let account = Account {
        server_url: origin.clone(),
        server_instance_id: server.instance_id.clone(),
        server_name: server.name.clone(),
        user_id: user.id.clone(),
        email: user.email,
        name: user.name,
        method,
        session_id: session.session_id().await,
    };
    let mut previous = None;
    let written = AccountFile::update(dir, |file| {
        previous = file.account.take();
        file.account = Some(account.clone());
        file.server = discovery;
        file.sign_in_required = false;
    });
    if let Err(e) = written {
        // Leave no orphaned tokens behind.
        let _ = delete_tokens(secrets.clone(), session.secret_key().to_string()).await;
        return Err(e.into());
    }
    if let Some(prev) = previous {
        let old = session_key(&prev.server_url, &prev.user_id);
        if old != session.secret_key() {
            let _ = delete_tokens(secrets, old).await;
        }
    }
    Ok((account, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_matches_rfc7636_example() {
        // RFC 7636 appendix B.
        assert_eq!(
            challenge_for("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
        let p = Pkce::new();
        assert_eq!(p.verifier.len(), 43);
        assert_eq!(p.challenge.len(), 43);
        assert_ne!(Pkce::new().verifier, p.verifier);
        assert_eq!(random_b64url(16).len(), 22);
        assert!(!format!("{p:?}").contains(&p.verifier));
    }

    #[test]
    fn device_names_fit_the_server_rules() {
        let n = device_name_from("Mihajlo's MacBook Pro");
        assert!(n.starts_with("Mihajlos MacBook Pro ("), "{n}");
        assert!(n.chars().count() <= 64);
        let long = device_name_from(&"x".repeat(200));
        assert_eq!(long.chars().count(), 64);
        assert!(device_name_from("<>!@").starts_with("ReqHarbor ("));
        assert!(device_name_from("  ubuntu-desk\n").starts_with("ubuntu-desk ("));
        let real = device_name();
        assert!(real.chars().count() <= 64 && !real.is_empty());
        assert!(server_accepts(&real), "{real}");
    }

    /// The server's `device_name` rule (server/internal/auth/pages_handlers.go
    /// `deviceRe`, at most 64 characters).
    fn server_accepts(name: &str) -> bool {
        let re = regex::Regex::new(r"^[\p{L}\p{N} ()._-]*$").unwrap();
        re.is_match(name) && name.chars().count() <= 64
    }

    #[test]
    fn device_names_drop_combining_marks() {
        // Combining marks are Alphabetic (so `is_alphanumeric` keeps them)
        // but not L/N, and the server refuses them.
        assert!('\u{093E}'.is_alphanumeric() && !is_letter_or_number('\u{093E}'));
        assert!('\u{0E31}'.is_alphanumeric() && !is_letter_or_number('\u{0E31}'));
        assert!('Ⓐ'.is_alphanumeric() && !is_letter_or_number('Ⓐ'));
        for c in ['a', 'Ж', 'र', 'ก', '中', '7', '٣', 'Ⅻ'] {
            assert!(is_letter_or_number(c), "{c:?}");
        }
        for computer in [
            "राम का Mac",
            "คอมพิวเตอร์ของสมชาย",
            "আমার ল্যাপটপ",
            "தமிழ் கணினி",
            "Café\u{0301} PC",
            "Ⓐcme ⓑox",
        ] {
            let n = device_name_from(computer);
            assert!(server_accepts(&n), "{computer:?} -> {n:?}");
            assert!(!n.starts_with("ReqHarbor ("), "{computer:?} -> {n:?}");
        }
        assert!(device_name_from("राम का Mac").starts_with("रम क Mac ("));
        // Only marks left: the generic name.
        assert!(device_name_from("\u{093E}\u{0E31}").starts_with("ReqHarbor ("));
    }

    #[test]
    fn authorize_url_carries_every_parameter() {
        let u = authorize_url(
            "https://rh.acme.dev",
            "http://127.0.0.1:50123/cb",
            "CHALLENGE",
            "STATE",
            "MacBook Pro (macOS)",
            Some("github"),
        );
        let parsed = url::Url::parse(&u).unwrap();
        assert_eq!(parsed.path(), "/auth/authorize");
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(q["client_id"], "reqharbor-desktop");
        assert_eq!(q["redirect_uri"], "http://127.0.0.1:50123/cb");
        assert_eq!(q["code_challenge"], "CHALLENGE");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"], "STATE");
        assert_eq!(q["device_name"], "MacBook Pro (macOS)");
        assert_eq!(q["method"], "github");
        let none = authorize_url("https://x", "r", "c", "s", "d", None);
        assert!(!none.contains("&method="));
    }

    #[test]
    fn loopback_routing() {
        assert_eq!(
            route("GET /cb?code=abc&state=S HTTP/1.1", "S"),
            CbRequest::Code("abc".into())
        );
        assert_eq!(
            route("GET /cb?code=abc&state=X HTTP/1.1", "S"),
            CbRequest::WrongState
        );
        assert_eq!(
            route("GET /cb?code=abc HTTP/1.1", "S"),
            CbRequest::WrongState
        );
        assert_eq!(
            route("GET /cb?error=access_denied&state=S HTTP/1.1", "S"),
            CbRequest::Error("access_denied".into())
        );
        assert_eq!(route("GET /favicon.ico HTTP/1.1", "S"), CbRequest::NotFound);
        assert_eq!(
            route("POST /cb?code=a&state=S HTTP/1.1", "S"),
            CbRequest::MethodNotAllowed
        );
        assert_eq!(route("GET /cb?state=S HTTP/1.1", "S"), CbRequest::Malformed);
        assert_eq!(route("garbage", "S"), CbRequest::Malformed);
    }

    #[test]
    fn session_rows_parse_leniently() {
        let rows: Vec<SessionInfo> = serde_json::from_value(serde_json::json!([
            {"id": "a", "deviceName": "Mac (macOS)", "createdAt": 1764000000000i64, "lastUsedDay": 20419, "current": true},
            {"id": "b", "createdAt": 1764000000, "lastUsedDay": "2025-11-27"},
            {"id": "c", "lastUsedDay": null}
        ]))
        .unwrap();
        assert_eq!(rows[0].created_at, Some(1_764_000_000_000));
        assert_eq!(rows[0].last_used_day, Some(20419 * DAY_MS));
        assert_eq!(rows[1].created_at, Some(1_764_000_000_000));
        assert_eq!(rows[1].last_used_day, Some(20419 * DAY_MS));
        assert_eq!(rows[2].last_used_day, None);
        assert!(!rows[2].current);
    }

    #[test]
    fn keychain_key_is_per_origin_and_user() {
        assert_eq!(
            session_key("https://rh.acme.dev", "u1"),
            "session:https://rh.acme.dev:u1"
        );
        assert_ne!(
            session_key("http://192.168.1.5:8080", "u1"),
            session_key("https://rh.acme.dev", "u1")
        );
    }

    #[test]
    fn account_file_round_trip_keeps_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(AccountFile::load(dir.path()).unwrap(), None);
        let mut f = AccountFile::default();
        f.extra
            .insert("teamsCache".into(), serde_json::json!([{"teamId": "t1"}]));
        f.bindings.migrated_at = Some(1);
        f.bindings.project_spaces.insert("p1".into(), "s1".into());
        f.account = Some(Account {
            server_url: "https://rh.acme.dev".into(),
            server_instance_id: "I".into(),
            server_name: "Acme".into(),
            user_id: "u1".into(),
            email: "p@acme.dev".into(),
            name: "Priya".into(),
            method: "github".into(),
            session_id: "s".into(),
        });
        f.save(dir.path()).unwrap();
        let back = AccountFile::load(dir.path()).unwrap().unwrap();
        assert_eq!(back, f);
        let raw = std::fs::read_to_string(dir.path().join(ACCOUNT_FILE)).unwrap();
        assert!(raw.contains("\"schemaVersion\": 1"));
        assert!(raw.contains("\"teamsCache\""));
        assert!(raw.contains("\"projectSpaces\""));
        assert!(raw.contains("\"migratedAt\": 1"));
        // Unknown keys keep the file alive when the account goes away.
        let mut gone = back.clone();
        gone.account = None;
        gone.save_or_remove(dir.path()).unwrap();
        assert!(dir.path().join(ACCOUNT_FILE).exists());
        gone.extra.clear();
        gone.save_or_remove(dir.path()).unwrap();
        assert!(dir.path().join(ACCOUNT_FILE).exists(), "bindings keep it");
        gone.bindings = Default::default();
        gone.save_or_remove(dir.path()).unwrap();
        assert!(!dir.path().join(ACCOUNT_FILE).exists());
        // A damaged file is quarantined, not fatal.
        std::fs::write(dir.path().join(ACCOUNT_FILE), "{oops").unwrap();
        assert_eq!(AccountFile::load(dir.path()).unwrap(), None);
    }

    #[test]
    fn errors_never_echo_secrets() {
        let t = Tokens {
            access_token: "rha_SECRET".into(),
            access_expires_at: 1,
            refresh_token: "rhr_SECRET".into(),
            session_id: "s".into(),
        };
        assert!(!format!("{t:?}").contains("SECRET"));
        for e in [
            AuthError::InvalidGrant,
            AuthError::SignInRequired,
            AuthError::Keychain("denied".into()),
        ] {
            let m = e.to_string();
            assert!(!m.contains("rha_") && !m.contains("rhr_"), "{m}");
        }
    }
}

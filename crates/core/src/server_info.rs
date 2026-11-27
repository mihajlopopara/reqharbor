//! Server entry and discovery (06-go-server §5.1, §9).
//!
//! There is no default server: the user types an address. This module
//! normalises it to an origin, asks `GET {origin}/v1/server` and maps the
//! answer to a [`DiscoveryStatus`] plus the copy the UI shows. The full
//! [`ServerInfo`] document is passed to the UI (sign-in methods, signup mode,
//! `publicUrl` for the canonical-address prompt).

use crate::model::{CLIENT_VERSION, Extra, now_ms, requires_newer_client};
use crate::sync::{BASE_SYNC_TYPES, VERSION_HEADER};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::time::Duration;

/// `GET /v1/server` timeout (06 §9).
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a cached discovery answer may stand in for an unreachable
/// server (06 §9 "Rediscovery").
pub const DISCOVERY_CACHE_MS: i64 = 24 * 60 * 60 * 1000;
/// The `software` value of a ReqHarbor server.
pub const SOFTWARE: &str = "reqharbor-server";

/// Why a typed address was refused before any request was made.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("Enter the server's address")]
    Empty,
    #[error("Enter the server's address without a path")]
    HasPath,
    #[error("Server URL must start with https:// or http://")]
    Scheme,
    #[error("Enter a valid server address, like https://reqharbor.example.com")]
    Invalid,
}

/// Normalise a typed server address to its origin: trim, add `https://` when
/// no scheme is given, lowercase scheme and host, drop the default port and a
/// trailing `/`. A path, query or fragment is refused (06 §9).
pub fn normalize_server_url(input: &str) -> Result<String, UrlError> {
    let t = input.trim();
    if t.is_empty() {
        return Err(UrlError::Empty);
    }
    let with_scheme = if t.contains("://") {
        t.to_string()
    } else {
        format!("https://{t}")
    };
    let u = url::Url::parse(&with_scheme).map_err(|_| UrlError::Invalid)?;
    match u.scheme() {
        "http" | "https" => {}
        _ => return Err(UrlError::Scheme),
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(UrlError::Invalid);
    }
    let host = match u.host_str() {
        Some(h) if !h.is_empty() => h.to_ascii_lowercase(),
        _ => return Err(UrlError::Invalid),
    };
    // `Url` keeps a bare "?" or "#" as an empty query/fragment.
    if (u.path() != "/" && !u.path().is_empty()) || u.query().is_some() || u.fragment().is_some() {
        return Err(UrlError::HasPath);
    }
    // `Url::port` is None for the scheme's default port.
    Ok(match u.port() {
        Some(p) => format!("{}://{host}:{p}", u.scheme()),
        None => format!("{}://{host}", u.scheme()),
    })
}

/// The host part of a normalised origin (with its port, if any), as the UI
/// names a server: `rh.acme.dev`, `192.168.1.5:8080`.
pub fn display_host(origin: &str) -> String {
    origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin)
        .to_string()
}

/// `localhost`, `127.0.0.0/8` or `::1` (06 §9 plain-http rule).
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_start_matches('[').trim_end_matches(']');
    if h.eq_ignore_ascii_case("localhost") {
        return true;
    }
    match h.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => false,
    }
}

/// Plain http to a non-loopback host: the app warns before using it.
pub fn is_insecure(origin: &str) -> bool {
    let Ok(u) = url::Url::parse(origin) else {
        return false;
    };
    u.scheme() == "http" && !u.host_str().is_some_and(is_loopback_host)
}

/// One sign-in method the server offers (`auth.methods[]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthMethod {
    pub id: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAuth {
    #[serde(default)]
    pub methods: Vec<AuthMethod>,
    /// "open" | "invite" | "closed"
    #[serde(default = "default_signup")]
    pub signup: String,
    /// "email" | "admin"
    #[serde(default = "default_password_reset")]
    pub password_reset: String,
    #[serde(default)]
    pub setup_required: bool,
}

impl Default for ServerAuth {
    fn default() -> Self {
        ServerAuth {
            methods: Vec::new(),
            signup: default_signup(),
            password_reset: default_password_reset(),
            setup_required: false,
        }
    }
}

fn default_signup() -> String {
    "invite".into()
}
fn default_password_reset() -> String {
    "admin".into()
}
fn default_legacy() -> String {
    "off".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerFeatures {
    #[serde(default)]
    pub accounts: bool,
    #[serde(default)]
    pub teams: bool,
    /// "open" | "existing" | "off"
    #[serde(default = "default_legacy")]
    pub legacy_workspaces: String,
}

impl Default for ServerFeatures {
    fn default() -> Self {
        ServerFeatures {
            accounts: false,
            teams: false,
            legacy_workspaces: default_legacy(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSync {
    #[serde(default = "base_types")]
    pub types: Vec<String>,
    #[serde(default = "http_kinds")]
    pub request_kinds: Vec<String>,
}

impl Default for ServerSync {
    fn default() -> Self {
        ServerSync {
            types: base_types(),
            request_kinds: http_kinds(),
        }
    }
}

fn base_types() -> Vec<String> {
    BASE_SYNC_TYPES.iter().map(|s| s.to_string()).collect()
}
fn http_kinds() -> Vec<String> {
    vec!["http".into()]
}

/// The `GET /v1/server` document (06 §5.1). Unknown fields are kept, so a
/// newer server's answer survives the 24 h cache unchanged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub software: String,
    #[serde(default)]
    pub version: String,
    pub api_version: u64,
    #[serde(default)]
    pub instance_id: String,
    #[serde(default)]
    pub sync_epoch: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub public_url: String,
    #[serde(default)]
    pub min_client_version: Option<String>,
    #[serde(default)]
    pub auth: ServerAuth,
    #[serde(default)]
    pub features: ServerFeatures,
    #[serde(default)]
    pub sync: ServerSync,
    #[serde(flatten)]
    pub extra: Extra,
}

/// The outcome of discovery, in the order of the 06 §9 table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryStatus {
    /// A ReqHarbor server with accounts. Sign-in may still be impossible
    /// (no methods); see [`Discovery::can_sign_in`].
    Ok,
    Unreachable,
    TokenSyncOnly,
    NotReqharbor,
    ClientTooOld,
}

/// What `server_discover` returns to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Discovery {
    /// The normalised origin that was asked.
    pub server_url: String,
    pub status: DiscoveryStatus,
    /// The error or notice line for this server, if any.
    pub message: Option<String>,
    /// The document, when the server is a ReqHarbor server.
    pub info: Option<ServerInfo>,
    /// `publicUrl` normalised, when it differs from `server_url` (the
    /// "Use that address?" prompt).
    pub canonical_url: Option<String>,
    /// Plain http to a non-loopback host.
    pub insecure: bool,
    /// Whether **Sign in** may be enabled for this server.
    pub can_sign_in: bool,
    /// Unix ms when the answer was fetched.
    pub fetched_at: i64,
    /// This is the cached answer: the server could not be reached now.
    #[serde(default)]
    pub cached: bool,
}

impl Discovery {
    fn new(server_url: &str, status: DiscoveryStatus, message: Option<String>) -> Self {
        Discovery {
            server_url: server_url.to_string(),
            status,
            message,
            info: None,
            canonical_url: None,
            insecure: is_insecure(server_url),
            can_sign_in: false,
            fetched_at: now_ms(),
            cached: false,
        }
    }

    /// Use `self` (a cached answer) when `fresh` could not reach the server
    /// and the cache is younger than 24 h for the same origin.
    pub fn or_cached(fresh: Discovery, cached: Option<&Discovery>, now: i64) -> Discovery {
        match cached {
            Some(c)
                if fresh.status == DiscoveryStatus::Unreachable
                    && c.server_url == fresh.server_url
                    && c.info.is_some()
                    && now - c.fetched_at < DISCOVERY_CACHE_MS =>
            {
                Discovery {
                    cached: true,
                    ..c.clone()
                }
            }
            _ => fresh,
        }
    }
}

pub const MSG_TOKEN_ONLY: &str = "This server only supports token sync. Use Sync › Advanced.";
pub const MSG_NO_SYNC: &str = "This server has no sync enabled yet. Ask its admin.";
pub const MSG_NOT_REQHARBOR: &str = "This doesn't look like a ReqHarbor server.";
pub const MSG_SETUP: &str =
    "This server hasn't been set up yet. Its admin will find the setup code in the server log.";
pub const MSG_NO_METHODS: &str = "This server doesn't have sign-in set up. Ask its admin, or sync with a token under Sync › Advanced.";

pub fn unreachable_message(origin: &str) -> String {
    format!(
        "Can't reach {}. Check the address and your connection.",
        display_host(origin)
    )
}

pub fn too_old_message(min: &str) -> String {
    format!("This server needs ReqHarbor {min} or newer.")
}

/// Map an HTTP answer (`None` = network error) to a [`Discovery`]. Pure, so
/// the 06 §9 table is tested row by row.
pub fn classify(origin: &str, answer: Option<(u16, &[u8])>) -> Discovery {
    use DiscoveryStatus as S;
    let Some((status, body)) = answer else {
        return Discovery::new(origin, S::Unreachable, Some(unreachable_message(origin)));
    };
    let json: Option<serde_json::Value> = serde_json::from_slice(body).ok();
    if status == 404 {
        let code = json
            .as_ref()
            .and_then(|v| v.pointer("/error/code"))
            .and_then(|c| c.as_str());
        return if code == Some("not_found") {
            Discovery::new(origin, S::TokenSyncOnly, Some(MSG_TOKEN_ONLY.into()))
        } else {
            Discovery::new(origin, S::NotReqharbor, Some(MSG_NOT_REQHARBOR.into()))
        };
    }
    if (500..=599).contains(&status) {
        return Discovery::new(origin, S::Unreachable, Some(unreachable_message(origin)));
    }
    let info = if status == 200 {
        json.and_then(|v| serde_json::from_value::<ServerInfo>(v).ok())
    } else {
        None
    };
    let Some(info) = info.filter(|i| i.software == SOFTWARE && i.api_version >= 1) else {
        return Discovery::new(origin, S::NotReqharbor, Some(MSG_NOT_REQHARBOR.into()));
    };
    let canonical_url = normalize_server_url(&info.public_url)
        .ok()
        .filter(|c| c != origin);
    let mut d = Discovery::new(origin, S::Ok, None);
    d.canonical_url = canonical_url;
    if let Some(min) = info.min_client_version.as_deref().filter(|m| !m.is_empty())
        && requires_newer_client(min)
    {
        d.status = S::ClientTooOld;
        d.message = Some(too_old_message(min));
    } else if !info.features.accounts {
        d.status = S::TokenSyncOnly;
        d.message = Some(if info.features.legacy_workspaces == "off" {
            MSG_NO_SYNC.into()
        } else {
            MSG_TOKEN_ONLY.into()
        });
    } else if info.auth.methods.is_empty() {
        // apiVersion above 1 is compatible (06 §9): nothing to check.
        d.message = Some(MSG_NO_METHODS.into());
    } else {
        d.can_sign_in = true;
        if info.auth.setup_required {
            d.message = Some(MSG_SETUP.into());
        }
    }
    d.info = Some(info);
    d
}

fn client() -> Result<reqwest::Client, String> {
    crate::http::install_crypto_provider();
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        VERSION_HEADER,
        reqwest::header::HeaderValue::from_static(CLIENT_VERSION),
    );
    reqwest::Client::builder()
        .timeout(DISCOVERY_TIMEOUT)
        .user_agent(crate::http::USER_AGENT)
        .default_headers(headers)
        .build()
        .map_err(|e| e.to_string())
}

/// `GET {origin}/v1/server` for an already normalised origin.
pub async fn discover_origin(origin: &str) -> Discovery {
    let Ok(http) = client() else {
        return classify(origin, None);
    };
    let url = format!("{origin}/v1/server");
    let resp = match http
        .get(&url)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
    {
        Ok(r) => r,
        Err(_) => return classify(origin, None),
    };
    let status = resp.status().as_u16();
    match resp.bytes().await {
        Ok(b) => classify(origin, Some((status, &b))),
        Err(_) => classify(origin, None),
    }
}

/// Normalise `input` and discover it.
pub async fn discover(input: &str) -> Result<Discovery, UrlError> {
    let origin = normalize_server_url(input)?;
    Ok(discover_origin(&origin).await)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalisation_table() {
        let ok = [
            ("rh.acme.dev", "https://rh.acme.dev"),
            ("  https://RH.Acme.DEV/  ", "https://rh.acme.dev"),
            ("HTTPS://rh.acme.dev:443", "https://rh.acme.dev"),
            ("http://rh.acme.dev:80/", "http://rh.acme.dev"),
            ("http://192.168.1.5:8080", "http://192.168.1.5:8080"),
            ("localhost:8080", "https://localhost:8080"),
            ("http://[::1]:8080", "http://[::1]:8080"),
            ("https://rh.acme.dev:8443", "https://rh.acme.dev:8443"),
        ];
        for (input, want) in ok {
            assert_eq!(normalize_server_url(input).as_deref(), Ok(want), "{input}");
        }
        let bad = [
            ("", UrlError::Empty),
            ("   ", UrlError::Empty),
            ("https://rh.acme.dev/api", UrlError::HasPath),
            ("https://rh.acme.dev/?x=1", UrlError::HasPath),
            ("https://rh.acme.dev/#top", UrlError::HasPath),
            ("https://rh.acme.dev?", UrlError::HasPath),
            ("ftp://rh.acme.dev", UrlError::Scheme),
            ("https://user:pw@rh.acme.dev", UrlError::Invalid),
            ("https://", UrlError::Invalid),
        ];
        for (input, want) in bad {
            assert_eq!(normalize_server_url(input), Err(want), "{input}");
        }
        assert_eq!(
            UrlError::HasPath.to_string(),
            "Enter the server's address without a path"
        );
    }

    #[test]
    fn insecure_only_for_plain_http_off_loopback() {
        assert!(is_insecure("http://192.168.1.5:8080"));
        assert!(is_insecure("http://rh.acme.dev"));
        assert!(!is_insecure("https://rh.acme.dev"));
        assert!(!is_insecure("http://localhost:8080"));
        assert!(!is_insecure("http://127.0.0.1:8080"));
        assert!(!is_insecure("http://127.8.9.10"));
        assert!(!is_insecure("http://[::1]:8080"));
        assert_eq!(display_host("http://192.168.1.5:8080"), "192.168.1.5:8080");
    }

    fn doc() -> serde_json::Value {
        json!({
            "software": "reqharbor-server", "version": "0.2.0", "apiVersion": 1,
            "instanceId": "01JD", "syncEpoch": "01JF", "name": "Acme ReqHarbor",
            "publicUrl": "https://rh.acme.dev", "minClientVersion": null,
            "auth": {"methods": [{"id": "password", "label": "Email and password"},
                                 {"id": "github", "label": "GitHub"}],
                     "signup": "invite", "passwordReset": "admin", "setupRequired": false},
            "features": {"accounts": true, "teams": false, "legacyWorkspaces": "off"},
            "sync": {"types": ["project", "collection", "folder", "request", "environment"],
                     "requestKinds": ["http"]},
            "limits": {"entityBytes": 524288}
        })
    }

    fn run(origin: &str, status: u16, body: &serde_json::Value) -> Discovery {
        classify(origin, Some((status, body.to_string().as_bytes())))
    }

    const O: &str = "https://rh.acme.dev";

    #[test]
    fn discovery_table() {
        let d = run(O, 200, &doc());
        assert_eq!(d.status, DiscoveryStatus::Ok);
        assert!(d.can_sign_in);
        assert_eq!(d.message, None);
        assert_eq!(d.canonical_url, None);
        assert_eq!(d.info.as_ref().unwrap().auth.methods.len(), 2);
        // Unknown fields survive.
        assert!(d.info.as_ref().unwrap().extra.contains_key("limits"));

        let d = classify(O, None);
        assert_eq!(d.status, DiscoveryStatus::Unreachable);
        assert_eq!(
            d.message.as_deref(),
            Some("Can't reach rh.acme.dev. Check the address and your connection.")
        );

        // The Worker.
        let d = run(
            O,
            404,
            &json!({"error": {"code": "not_found", "message": "Not found"}}),
        );
        assert_eq!(d.status, DiscoveryStatus::TokenSyncOnly);
        assert_eq!(d.message.as_deref(), Some(MSG_TOKEN_ONLY));

        let d = run(O, 404, &json!("nope"));
        assert_eq!(d.status, DiscoveryStatus::NotReqharbor);
        let d = classify(O, Some((200, b"<html>hello</html>")));
        assert_eq!(d.status, DiscoveryStatus::NotReqharbor);
        let mut other = doc();
        other["software"] = json!("something-else");
        assert_eq!(run(O, 200, &other).status, DiscoveryStatus::NotReqharbor);
        let mut v0 = doc();
        v0["apiVersion"] = json!(0);
        assert_eq!(run(O, 200, &v0).status, DiscoveryStatus::NotReqharbor);
        let mut missing = doc();
        missing.as_object_mut().unwrap().remove("apiVersion");
        assert_eq!(run(O, 200, &missing).status, DiscoveryStatus::NotReqharbor);

        let mut old = doc();
        old["minClientVersion"] = json!("99.0.0");
        let d = run(O, 200, &old);
        assert_eq!(d.status, DiscoveryStatus::ClientTooOld);
        assert_eq!(
            d.message.as_deref(),
            Some("This server needs ReqHarbor 99.0.0 or newer.")
        );
        assert!(!d.can_sign_in);
        let mut fine = doc();
        fine["minClientVersion"] = json!("0.0.1");
        assert_eq!(run(O, 200, &fine).status, DiscoveryStatus::Ok);

        // server-v0.1.0: accounts off.
        let mut legacy = doc();
        legacy["features"]["accounts"] = json!(false);
        legacy["auth"]["methods"] = json!([]);
        legacy["features"]["legacyWorkspaces"] = json!("open");
        let d = run(O, 200, &legacy);
        assert_eq!(d.status, DiscoveryStatus::TokenSyncOnly);
        assert_eq!(d.message.as_deref(), Some(MSG_TOKEN_ONLY));
        legacy["features"]["legacyWorkspaces"] = json!("off");
        let d = run(O, 200, &legacy);
        assert_eq!(d.status, DiscoveryStatus::TokenSyncOnly);
        assert_eq!(d.message.as_deref(), Some(MSG_NO_SYNC));

        // apiVersion 2 with minClientVersion null is compatible.
        let mut future = doc();
        future["apiVersion"] = json!(2);
        future["somethingNew"] = json!({"x": 1});
        let d = run(O, 200, &future);
        assert_eq!(d.status, DiscoveryStatus::Ok);
        assert!(d.can_sign_in);

        let mut setup = doc();
        setup["auth"]["setupRequired"] = json!(true);
        let d = run(O, 200, &setup);
        assert_eq!(d.status, DiscoveryStatus::Ok);
        assert!(d.can_sign_in);
        assert_eq!(d.message.as_deref(), Some(MSG_SETUP));

        let mut none = doc();
        none["auth"]["methods"] = json!([]);
        let d = run(O, 200, &none);
        assert_eq!(d.status, DiscoveryStatus::Ok);
        assert!(!d.can_sign_in);
        assert_eq!(d.message.as_deref(), Some(MSG_NO_METHODS));

        let d = run(
            O,
            503,
            &json!({"error": {"code": "starting", "message": "x"}}),
        );
        assert_eq!(d.status, DiscoveryStatus::Unreachable);
    }

    #[test]
    fn canonical_address_prompt() {
        let typed = "http://192.168.1.5:8080";
        let d = run(typed, 200, &doc());
        assert_eq!(d.canonical_url.as_deref(), Some("https://rh.acme.dev"));
        assert!(d.insecure);
        // Equal after normalisation: no prompt.
        let mut same = doc();
        same["publicUrl"] = json!("HTTPS://RH.ACME.DEV:443/");
        assert_eq!(run(O, 200, &same).canonical_url, None);
        // A broken publicUrl never prompts.
        let mut broken = doc();
        broken["publicUrl"] = json!("");
        assert_eq!(run(O, 200, &broken).canonical_url, None);
    }

    #[test]
    fn cache_stands_in_for_24_hours_only() {
        let fresh_ok = run(O, 200, &doc());
        let now = fresh_ok.fetched_at;
        let down = classify(O, None);
        let used = Discovery::or_cached(down.clone(), Some(&fresh_ok), now + 1000);
        assert!(used.cached);
        assert_eq!(used.status, DiscoveryStatus::Ok);
        let stale =
            Discovery::or_cached(down.clone(), Some(&fresh_ok), now + DISCOVERY_CACHE_MS + 1);
        assert_eq!(stale.status, DiscoveryStatus::Unreachable);
        let other = run("https://other.dev", 200, &doc());
        let wrong = Discovery::or_cached(down, Some(&other), now);
        assert_eq!(wrong.status, DiscoveryStatus::Unreachable);
        // A reachable answer always wins over the cache.
        let fresh_again = run(O, 200, &doc());
        assert!(!Discovery::or_cached(fresh_again, Some(&fresh_ok), now).cached);
    }
}

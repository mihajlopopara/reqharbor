//! Send orchestration: prepare -> materialize body -> execute -> history entry
//! (AC-REQ-8/9, AC-HIST-1, AC-ENV-3/7, AC-RES-*).

use crate::cookies::{ResponseCookie, parse_response_cookies};
use crate::http::{HttpEngine, HttpResponse, RedirectHop};
use crate::model::{
    Auth, BodyEncoding, ErrorKind, HistoryEntry, HistoryResponse, Request, SendError, new_id,
    now_ms,
};
use crate::prepare::{PrepareError, PreparedRequest, prepare};
use crate::redact::Redactor;
use crate::vars::VarScope;
use base64::Engine as _;
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Max response body bytes stored in a history entry.
pub const HISTORY_BODY_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyKind {
    Json,
    Xml,
    Html,
    Text,
    Image,
    Binary,
    Empty,
}

/// Response metadata sent to the UI. The body itself travels separately as
/// raw bytes (PRODUCT §7 guardrail).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseMeta {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    pub headers: Vec<(String, String)>,
    pub cookies: Vec<ResponseCookie>,
    pub time_ms: u64,
    /// Decoded body size (AC-RES-1/4).
    pub size_bytes: u64,
    /// Bytes on the wire (tooltip, AC-RES-4). `null` for history entries.
    pub wire_size_bytes: Option<u64>,
    pub content_encoding: Option<String>,
    pub content_type: Option<String>,
    pub body_kind: BodyKind,
    /// Value of an `Accept-Query` response header, if any (AC-QRY-7).
    pub accept_query: Option<String>,
    pub final_url: Option<String>,
    pub redirects: Vec<RedirectHop>,
    pub decode_error: Option<String>,
    /// True when the stored body is cut (history entries over 1 MB).
    pub body_truncated: bool,
}

pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Classify a body by Content-Type, falling back to sniffing (AC-RES-2).
pub fn body_kind(content_type: Option<&str>, body: &[u8]) -> BodyKind {
    if body.is_empty() {
        return BodyKind::Empty;
    }
    if let Some(ct) = content_type {
        let ct = ct.to_ascii_lowercase();
        let essence = ct.split(';').next().unwrap_or("").trim().to_string();
        if essence.contains("json") {
            return BodyKind::Json;
        }
        if essence.starts_with("image/") {
            return BodyKind::Image;
        }
        if essence.contains("html") {
            return BodyKind::Html;
        }
        if essence.contains("xml") {
            return BodyKind::Xml;
        }
        if essence.starts_with("text/")
            || essence.contains("javascript")
            || essence.contains("x-www-form-urlencoded")
        {
            return BodyKind::Text;
        }
        if !essence.is_empty() && essence != "application/octet-stream" {
            return if std::str::from_utf8(body).is_ok() {
                BodyKind::Text
            } else {
                BodyKind::Binary
            };
        }
    }
    sniff(body)
}

fn sniff(body: &[u8]) -> BodyKind {
    const SIGS: [&[u8]; 5] = [b"\x89PNG", b"\xFF\xD8\xFF", b"GIF8", b"RIFF", b"<svg"];
    if SIGS.iter().any(|s| body.starts_with(s)) {
        return BodyKind::Image;
    }
    let Ok(text) = std::str::from_utf8(body) else {
        return BodyKind::Binary;
    };
    let t = text.trim_start();
    if (t.starts_with('{') || t.starts_with('['))
        && serde_json::from_str::<serde::de::IgnoredAny>(text).is_ok()
    {
        return BodyKind::Json;
    }
    let lower = t.get(..t.len().min(64)).unwrap_or("").to_ascii_lowercase();
    if lower.starts_with("<!doctype html") || lower.starts_with("<html") {
        return BodyKind::Html;
    }
    if t.starts_with('<') {
        return BodyKind::Xml;
    }
    BodyKind::Text
}

impl ResponseMeta {
    pub fn from_response(resp: &HttpResponse) -> Self {
        let content_type = resp.header("content-type").map(str::to_string);
        ResponseMeta {
            status: resp.status,
            status_text: resp.status_text.clone(),
            http_version: resp.http_version.clone(),
            headers: resp.headers.clone(),
            cookies: parse_response_cookies(&resp.headers),
            time_ms: resp.time_ms,
            size_bytes: resp.body.len() as u64,
            wire_size_bytes: Some(resp.wire_size),
            content_encoding: resp.content_encoding.clone(),
            body_kind: body_kind(content_type.as_deref(), &resp.body),
            content_type,
            accept_query: resp.header("accept-query").map(str::to_string),
            final_url: Some(resp.final_url.clone()),
            redirects: resp.redirects.clone(),
            decode_error: resp.decode_error.clone(),
            body_truncated: false,
        }
    }

    /// Rebuild metadata + body bytes from a stored history response.
    pub fn from_history(h: &HistoryResponse) -> (Self, Vec<u8>) {
        let body = history_body_bytes(h);
        let content_type = header_value(&h.headers, "content-type").map(str::to_string);
        let meta = ResponseMeta {
            status: h.status,
            status_text: h.status_text.clone(),
            http_version: String::new(),
            headers: h.headers.clone(),
            cookies: parse_response_cookies(&h.headers),
            time_ms: h.time_ms,
            size_bytes: h.size_bytes,
            wire_size_bytes: None,
            content_encoding: header_value(&h.headers, "content-encoding").map(str::to_string),
            body_kind: body_kind(content_type.as_deref(), &body),
            content_type,
            accept_query: header_value(&h.headers, "accept-query").map(str::to_string),
            final_url: None,
            redirects: Vec::new(),
            decode_error: None,
            body_truncated: h.truncated,
        };
        (meta, body)
    }
}

/// Decode a stored history body to bytes.
pub fn history_body_bytes(h: &HistoryResponse) -> Vec<u8> {
    match h.body_encoding {
        BodyEncoding::Utf8 => h.body.as_bytes().to_vec(),
        BodyEncoding::Base64 => base64::engine::general_purpose::STANDARD
            .decode(h.body.as_bytes())
            .unwrap_or_default(),
    }
}

/// Encode at most [`HISTORY_BODY_LIMIT`] bytes of a body for history.
pub fn encode_history_body(body: &[u8]) -> (String, BodyEncoding, bool) {
    let truncated = body.len() > HISTORY_BODY_LIMIT;
    let slice = &body[..body.len().min(HISTORY_BODY_LIMIT)];
    match std::str::from_utf8(slice) {
        Ok(text) => (text.to_string(), BodyEncoding::Utf8, truncated),
        // The cut went through a multi-byte character: still text.
        Err(e) if truncated && e.error_len().is_none() => (
            String::from_utf8_lossy(&slice[..e.valid_up_to()]).into_owned(),
            BodyEncoding::Utf8,
            truncated,
        ),
        Err(_) => (
            base64::engine::general_purpose::STANDARD.encode(slice),
            BodyEncoding::Base64,
            truncated,
        ),
    }
}

/// Result of a send. Network failures are values, not errors (AC-REQ-9).
#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub prepared: PreparedRequest,
    pub response: Option<HttpResponse>,
    pub error: Option<SendError>,
    /// Entry to append to history (AC-HIST-1). Secret values are not in it.
    pub history: HistoryEntry,
}

/// Prepare and send `request`. Returns `Err` only for validation failures
/// that block sending (invalid method, invalid GraphQL variables, empty URL).
pub async fn send_request(
    engine: &HttpEngine,
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    cancel: &CancellationToken,
) -> Result<SendOutcome, PrepareError> {
    let prepared = prepare(request, scope, collection_auth)?;
    let result = match prepared.body.as_ref().map(|b| b.materialize()).transpose() {
        Ok(body) => engine.execute(&prepared, body, cancel).await,
        Err(e) => Err(SendError {
            kind: ErrorKind::Other,
            message: e.to_string(),
        }),
    };
    let (response, error) = match result {
        Ok(r) => (Some(r), None),
        Err(e) => (None, Some(e)),
    };
    let redactor = Redactor::from_scope(scope);
    let history = history_entry(
        request,
        &prepared,
        response.as_ref(),
        error.as_ref(),
        &redactor,
    );
    Ok(SendOutcome {
        prepared,
        response,
        error,
        history,
    })
}

/// Build the history entry for a send. Every string in it passes through
/// `redactor` so secret values never reach disk (AC-ENV-7).
pub fn history_entry(
    request: &Request,
    prepared: &PreparedRequest,
    response: Option<&HttpResponse>,
    error: Option<&SendError>,
    redactor: &Redactor,
) -> HistoryEntry {
    let source_request_id = if request.collection_id.is_empty() {
        None
    } else {
        Some(request.id.clone())
    };
    let entry = HistoryEntry {
        id: new_id(),
        timestamp: now_ms(),
        source_request_id,
        request: request.clone(),
        resolved_url: prepared.display_url.clone(),
        method: prepared.method.clone(),
        response: response.map(|r| {
            let (body, body_encoding, truncated) = encode_history_body(&r.body);
            HistoryResponse {
                status: r.status,
                status_text: r.status_text.clone(),
                time_ms: r.time_ms,
                size_bytes: r.body.len() as u64,
                headers: r.headers.clone(),
                body,
                truncated,
                body_encoding,
            }
        }),
        error: error.map(|e| SendError {
            kind: e.kind,
            // Error texts embed the resolved URL: show the masked one.
            message: e.message.replace(&prepared.url, &prepared.display_url),
        }),
        ws: None,
    };
    redactor.redact_struct(&entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_res_2_body_kind_detection() {
        assert_eq!(
            body_kind(Some("application/json; charset=utf-8"), b"{}"),
            BodyKind::Json
        );
        assert_eq!(
            body_kind(Some("application/problem+json"), b"{}"),
            BodyKind::Json
        );
        assert_eq!(body_kind(Some("application/xml"), b"<a/>"), BodyKind::Xml);
        assert_eq!(body_kind(Some("text/html"), b"<p>"), BodyKind::Html);
        assert_eq!(body_kind(Some("image/png"), b"\x89PNG"), BodyKind::Image);
        assert_eq!(body_kind(Some("text/plain"), b"hi"), BodyKind::Text);
        assert_eq!(body_kind(None, b"[1,2]"), BodyKind::Json);
        assert_eq!(
            body_kind(None, b"<?xml version=\"1.0\"?><a/>"),
            BodyKind::Xml
        );
        assert_eq!(
            body_kind(Some("application/octet-stream"), b"\x00\xff\xfe"),
            BodyKind::Binary
        );
        assert_eq!(body_kind(None, b"\x89PNG\r\n"), BodyKind::Image);
        assert_eq!(body_kind(None, b""), BodyKind::Empty);
    }

    #[test]
    fn ac_hist_4_history_restores_response_without_resend() {
        let h = HistoryResponse {
            status: 201,
            status_text: "Created".into(),
            time_ms: 42,
            size_bytes: 9,
            headers: vec![
                ("content-type".into(), "application/json".into()),
                ("set-cookie".into(), "a=1".into()),
                ("accept-query".into(), "application/json".into()),
            ],
            body: "{\"ok\":1}".into(),
            truncated: false,
            body_encoding: BodyEncoding::Utf8,
        };
        let (meta, body) = ResponseMeta::from_history(&h);
        assert_eq!(meta.status, 201);
        assert_eq!(meta.time_ms, 42);
        assert_eq!(meta.body_kind, BodyKind::Json);
        assert_eq!(meta.cookies.len(), 1);
        assert_eq!(meta.accept_query.as_deref(), Some("application/json"));
        assert_eq!(body, b"{\"ok\":1}");
    }

    #[test]
    fn history_body_truncated_at_1mb() {
        let big = vec![b'a'; HISTORY_BODY_LIMIT + 10];
        let (s, enc, trunc) = encode_history_body(&big);
        assert_eq!(s.len(), HISTORY_BODY_LIMIT);
        assert_eq!(enc, BodyEncoding::Utf8);
        assert!(trunc);
        let bin = vec![0xffu8, 0x00, 0x10];
        let (s, enc, trunc) = encode_history_body(&bin);
        assert_eq!(enc, BodyEncoding::Base64);
        assert!(!trunc);
        let h = HistoryResponse {
            status: 200,
            status_text: "OK".into(),
            time_ms: 1,
            size_bytes: 3,
            headers: vec![],
            body: s,
            truncated: false,
            body_encoding: enc,
        };
        assert_eq!(history_body_bytes(&h), bin);
    }
}

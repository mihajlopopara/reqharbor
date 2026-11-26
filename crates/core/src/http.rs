//! HTTP execution on top of reqwest (rustls).
//!
//! * One long-lived client per TLS setting (verify / don't verify), so
//!   connections are reused (PRODUCT §7 guardrails).
//! * Redirects are followed manually (AC-QRY-4): 307/308 re-send method and
//!   body; 303 switches to GET without body (HEAD stays HEAD); 301/302 rewrite
//!   only POST to GET (Fetch semantics), so QUERY and every other method are
//!   re-sent with their body. Max 10 redirects. `Authorization`, `Cookie`,
//!   `Proxy-Authorization` and the Auth tab's API-key header are dropped on
//!   cross-origin hops. Cookies set by redirect responses are carried to the
//!   next same-host hop. Raw UTF-8 `Location` values are percent-encoded.
//! * Decompression (gzip, deflate, brotli) is done here rather than by
//!   reqwest so the received headers and the wire size stay intact (AC-RES-4).
//!   Decoded output is capped at [`MAX_BODY_BYTES`] (decompression bombs).
//! * The timeout covers the whole exchange including redirects and body;
//!   `timeoutMs: 0` disables it.
//! * Known limitation: received headers are reported grouped by name (the
//!   order of first occurrence) because hyper's `HeaderMap` does not keep the
//!   interleaving of repeated names (QA-R1-019).
//! * Cancellation is immediate via a [`CancellationToken`] (AC-REQ-8).

use crate::model::{ErrorKind, SendError};
use crate::prepare::PreparedRequest;
use serde::Serialize;
use std::io::Read;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Maximum redirects followed (AC-QRY-4).
pub const MAX_REDIRECTS: usize = 10;
/// Hard cap on a response body kept in memory.
pub const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

/// Headers removed when a redirect leaves the origin (plus the API-key header
/// from the Auth tab, see `PreparedRequest::sensitive_headers`).
pub const CROSS_ORIGIN_STRIPPED: [&str; 3] = ["authorization", "cookie", "proxy-authorization"];

pub const USER_AGENT: &str = concat!("ReqHarbor/", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RedirectHop {
    pub status: u16,
    pub method: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub status_text: String,
    pub http_version: String,
    /// Received headers, names lowercase as delivered by the HTTP stack.
    pub headers: Vec<(String, String)>,
    /// Decoded body.
    pub body: Vec<u8>,
    /// Bytes received on the wire for the final response body.
    pub wire_size: u64,
    pub content_encoding: Option<String>,
    /// Set when decompression failed; `body` then holds the raw bytes.
    pub decode_error: Option<String>,
    pub time_ms: u64,
    pub final_url: String,
    pub final_method: String,
    pub redirects: Vec<RedirectHop>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Shared HTTP engine.
#[derive(Clone)]
pub struct HttpEngine {
    secure: reqwest::Client,
    insecure: reqwest::Client,
}

/// Install the ring rustls provider once (idempotent).
pub fn install_crypto_provider() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // Err means a provider is already installed: fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

fn build_client(verify: bool) -> Result<reqwest::Client, reqwest::Error> {
    install_crypto_provider();
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .http1_title_case_headers()
        .tls_danger_accept_invalid_certs(!verify)
        .connect_timeout(Duration::from_secs(30))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()
}

impl HttpEngine {
    pub fn new() -> Result<Self, SendError> {
        let secure =
            build_client(true).map_err(|e| other(format!("HTTP client init failed: {e}")))?;
        let insecure =
            build_client(false).map_err(|e| other(format!("HTTP client init failed: {e}")))?;
        Ok(HttpEngine { secure, insecure })
    }

    /// Execute a prepared request whose body has already been materialized.
    pub async fn execute(
        &self,
        prepared: &PreparedRequest,
        body: Option<Vec<u8>>,
        cancel: &CancellationToken,
    ) -> Result<HttpResponse, SendError> {
        // `timeoutMs: 0` means "no limit" (like `curl -m 0`).
        let timeout_ms = prepared.settings.timeout_ms;
        let start = Instant::now();
        let fut = self.execute_inner(prepared, body, start);
        let deadline = async {
            if timeout_ms == 0 {
                std::future::pending::<()>().await
            } else {
                tokio::time::sleep(Duration::from_millis(timeout_ms)).await
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(SendError {
                kind: ErrorKind::Cancelled,
                message: format!("Request cancelled after {} ms", start.elapsed().as_millis()),
            }),
            _ = deadline => Err(SendError {
                kind: ErrorKind::Timeout,
                message: format!("Timed out after {} ms", prepared.settings.timeout_ms),
            }),
            res = fut => res,
        }
    }

    async fn execute_inner(
        &self,
        prepared: &PreparedRequest,
        body: Option<Vec<u8>>,
        start: Instant,
    ) -> Result<HttpResponse, SendError> {
        let client = if prepared.settings.verify_tls {
            &self.secure
        } else {
            &self.insecure
        };
        let mut method = prepared.method.clone();
        let mut url = reqwest::Url::parse(&prepared.url)
            .map_err(|e| other(format!("Invalid URL \"{}\": {e}", prepared.url)))?;
        let mut headers = prepared.headers.clone();
        let mut body: Option<Arc<Vec<u8>>> = body.map(Arc::new);
        let mut redirects: Vec<RedirectHop> = Vec::new();

        loop {
            let req_method = reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|_| other(format!("Invalid method {method}")))?;
            let mut rb = client.request(req_method, url.clone());
            let mut header_map = reqwest::header::HeaderMap::new();
            for (k, v) in &headers {
                let name = reqwest::header::HeaderName::from_bytes(k.as_bytes())
                    .map_err(|_| other(format!("Invalid header name \"{k}\"")))?;
                let value = reqwest::header::HeaderValue::from_bytes(v.as_bytes())
                    .map_err(|_| other(format!("Invalid value for header \"{k}\"")))?;
                header_map.append(name, value);
            }
            let has = |n: &str| headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(n));
            if !has("accept-encoding") {
                header_map.insert(
                    reqwest::header::ACCEPT_ENCODING,
                    reqwest::header::HeaderValue::from_static("gzip, deflate, br"),
                );
            }
            if !has("user-agent") {
                header_map.insert(
                    reqwest::header::USER_AGENT,
                    reqwest::header::HeaderValue::from_static(USER_AGENT),
                );
            }
            if !has("accept") {
                header_map.insert(
                    reqwest::header::ACCEPT,
                    reqwest::header::HeaderValue::from_static("*/*"),
                );
            }
            rb = rb.headers(header_map);
            if let Some(b) = &body {
                rb = rb.body(b.as_ref().clone());
            }
            let resp = rb.send().await.map_err(|e| classify(&e))?;
            let status = resp.status().as_u16();

            let location = resp
                .headers()
                .get(reqwest::header::LOCATION)
                // Raw UTF-8 bytes are tolerated; `Url::join` percent-encodes them.
                .map(|v| String::from_utf8_lossy(v.as_bytes()).trim().to_string())
                .filter(|v| !v.is_empty());
            let is_redirect = matches!(status, 301 | 302 | 303 | 307 | 308);
            if is_redirect
                && prepared.settings.follow_redirects
                && let Some(location) = location
            {
                if redirects.len() >= MAX_REDIRECTS {
                    return Err(other(format!("Too many redirects (max {MAX_REDIRECTS})")));
                }
                let next = url
                    .join(&location)
                    .map_err(|e| other(format!("Invalid redirect location \"{location}\": {e}")))?;
                let (next_method, keep_body) = redirect_method(status, &method);
                // Cookies set on the redirect response travel to the next hop.
                let set_cookies: Vec<String> = resp
                    .headers()
                    .get_all(reqwest::header::SET_COOKIE)
                    .iter()
                    .filter_map(|v| v.to_str().ok())
                    .filter_map(|v| v.split(';').next().map(|p| p.trim().to_string()))
                    .filter(|p| p.contains('='))
                    .collect();
                drop(resp);
                let same_origin = url.origin() == next.origin();
                if !same_origin {
                    // Credentials never follow a redirect to another origin.
                    headers.retain(|(k, _)| {
                        !CROSS_ORIGIN_STRIPPED
                            .iter()
                            .any(|h| k.eq_ignore_ascii_case(h))
                            && !prepared
                                .sensitive_headers
                                .iter()
                                .any(|h| k.eq_ignore_ascii_case(h))
                    });
                }
                if !set_cookies.is_empty() && url.host_str() == next.host_str() {
                    merge_cookies(&mut headers, &set_cookies);
                }
                if !keep_body {
                    body = None;
                    headers.retain(|(k, _)| {
                        !k.eq_ignore_ascii_case("content-type")
                            && !k.eq_ignore_ascii_case("content-length")
                            && !k.eq_ignore_ascii_case("content-encoding")
                    });
                }
                redirects.push(RedirectHop {
                    status,
                    method: method.clone(),
                    url: url.to_string(),
                });
                method = next_method;
                url = next;
                continue;
            }

            // Final response.
            let http_version = format!("{:?}", resp.version());
            let status_text = resp.status().canonical_reason().unwrap_or("").to_string();
            let resp_headers: Vec<(String, String)> = resp
                .headers()
                .iter()
                .map(|(k, v)| {
                    (
                        k.as_str().to_string(),
                        String::from_utf8_lossy(v.as_bytes()).into_owned(),
                    )
                })
                .collect();
            let content_encoding = resp
                .headers()
                .get(reqwest::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.trim().to_ascii_lowercase())
                .filter(|s| !s.is_empty() && s != "identity");
            let mut resp = resp;
            let mut raw: Vec<u8> = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| classify(&e))? {
                if raw.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(other(format!(
                        "Response body exceeds {} MB",
                        MAX_BODY_BYTES / 1024 / 1024
                    )));
                }
                raw.extend_from_slice(&chunk);
            }
            let wire_size = raw.len() as u64;
            let (body, decode_error) = match &content_encoding {
                // HEAD / 204 / 304 legitimately carry an encoding with no body.
                Some(_) if raw.is_empty() => (raw, None),
                Some(enc) => match decode_body_limited(enc, &raw, MAX_BODY_BYTES) {
                    Ok(decoded) => (decoded, None),
                    Err(DecodeError::TooLarge) => {
                        return Err(other(format!(
                            "Response body exceeds {} MB after decompression",
                            MAX_BODY_BYTES / 1024 / 1024
                        )));
                    }
                    Err(DecodeError::Failed(e)) => (raw, Some(e)),
                },
                None => (raw, None),
            };
            return Ok(HttpResponse {
                status,
                status_text,
                http_version,
                headers: resp_headers,
                body,
                wire_size,
                content_encoding,
                decode_error,
                time_ms: start.elapsed().as_millis() as u64,
                final_url: url.to_string(),
                final_method: method,
                redirects,
            });
        }
    }
}

/// Method and body handling for a redirect (AC-QRY-4, Fetch semantics).
/// Returns `(next_method, keep_body)`.
pub fn redirect_method(status: u16, method: &str) -> (String, bool) {
    match status {
        303 if method != "GET" && method != "HEAD" => ("GET".to_string(), false),
        303 => (method.to_string(), false),
        301 | 302 if method == "POST" => ("GET".to_string(), false),
        _ => (method.to_string(), true),
    }
}

fn merge_cookies(headers: &mut Vec<(String, String)>, set_cookies: &[String]) {
    if let Some((_, existing)) = headers
        .iter_mut()
        .find(|(k, _)| k.eq_ignore_ascii_case("cookie"))
    {
        for c in set_cookies {
            let name = c.split('=').next().unwrap_or("");
            let mut pairs: Vec<String> = existing
                .split(';')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty() && p.split('=').next() != Some(name))
                .collect();
            pairs.push(c.clone());
            *existing = pairs.join("; ");
        }
    } else {
        headers.push(("Cookie".to_string(), set_cookies.join("; ")));
    }
}

/// Why decoding a body failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// The decoded output would exceed the size limit (decompression bomb).
    TooLarge,
    Failed(String),
}

/// Read at most `limit` bytes; more is [`DecodeError::TooLarge`].
fn read_limited(reader: impl Read, limit: usize, what: &str) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|e| DecodeError::Failed(format!("{what} decode failed: {e}")))?;
    if out.len() > limit {
        return Err(DecodeError::TooLarge);
    }
    Ok(out)
}

/// Decode a `Content-Encoding` chain (applied in reverse order), never
/// producing more than `limit` bytes at any stage.
pub fn decode_body_limited(
    encoding: &str,
    raw: &[u8],
    limit: usize,
) -> Result<Vec<u8>, DecodeError> {
    let mut data = raw.to_vec();
    for enc in encoding
        .split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .rev()
    {
        data = match enc.as_str() {
            "gzip" | "x-gzip" => {
                read_limited(flate2::read::MultiGzDecoder::new(&data[..]), limit, "gzip")?
            }
            // Spec says zlib-wrapped; some servers send raw deflate.
            "deflate" => {
                match read_limited(flate2::read::ZlibDecoder::new(&data[..]), limit, "deflate") {
                    Ok(out) => out,
                    Err(DecodeError::TooLarge) => return Err(DecodeError::TooLarge),
                    Err(DecodeError::Failed(_)) => read_limited(
                        flate2::read::DeflateDecoder::new(&data[..]),
                        limit,
                        "deflate",
                    )?,
                }
            }
            "br" => read_limited(
                brotli_decompressor::Decompressor::new(&data[..], 4096),
                limit,
                "brotli",
            )?,
            "identity" | "" => data,
            other => {
                return Err(DecodeError::Failed(format!(
                    "unsupported content-encoding {other}"
                )));
            }
        };
    }
    Ok(data)
}

/// Decode a `Content-Encoding` chain with the default [`MAX_BODY_BYTES`] cap.
pub fn decode_body(encoding: &str, raw: &[u8]) -> Result<Vec<u8>, String> {
    decode_body_limited(encoding, raw, MAX_BODY_BYTES).map_err(|e| match e {
        DecodeError::TooLarge => {
            format!("decoded body exceeds {} MB", MAX_BODY_BYTES / 1024 / 1024)
        }
        DecodeError::Failed(m) => m,
    })
}

fn other(message: String) -> SendError {
    SendError {
        kind: ErrorKind::Other,
        message,
    }
}

/// Map a reqwest error to a typed [`SendError`] (AC-REQ-9).
pub fn classify(err: &reqwest::Error) -> SendError {
    let mut chain = Vec::new();
    let mut src: Option<&dyn std::error::Error> = Some(err);
    while let Some(e) = src {
        let s = e.to_string();
        if chain.last() != Some(&s) {
            chain.push(s);
        }
        src = e.source();
    }
    let message = chain.join(": ");
    let lower = message.to_ascii_lowercase();
    let kind = if err.is_timeout() {
        ErrorKind::Timeout
    } else if lower.contains("dns error")
        || lower.contains("failed to lookup address")
        || lower.contains("nodename nor servname")
        || lower.contains("name or service not known")
        || lower.contains("no such host")
        || lower.contains("name resolution")
    {
        ErrorKind::Dns
    } else if lower.contains("certificate")
        || lower.contains("tls")
        || lower.contains("handshake")
        || lower.contains("ssl")
    {
        ErrorKind::Tls
    } else if err.is_connect() {
        ErrorKind::Connect
    } else {
        ErrorKind::Other
    };
    SendError { kind, message }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn ac_qry_4_redirect_method_rules() {
        assert_eq!(redirect_method(307, "QUERY"), ("QUERY".into(), true));
        assert_eq!(redirect_method(308, "QUERY"), ("QUERY".into(), true));
        assert_eq!(redirect_method(303, "QUERY"), ("GET".into(), false));
        assert_eq!(redirect_method(301, "QUERY"), ("QUERY".into(), true));
        assert_eq!(redirect_method(302, "QUERY"), ("QUERY".into(), true));
        assert_eq!(redirect_method(302, "POST"), ("GET".into(), false));
        assert_eq!(redirect_method(303, "HEAD"), ("HEAD".into(), false));
        assert_eq!(redirect_method(307, "POST"), ("POST".into(), true));
        assert_eq!(redirect_method(301, "PUT"), ("PUT".into(), true));
    }

    #[test]
    fn ac_res_4_decoders() {
        let text = b"hello hello hello hello".to_vec();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(&text).unwrap();
        let gz = gz.finish().unwrap();
        assert_eq!(decode_body("gzip", &gz).unwrap(), text);

        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&text).unwrap();
        assert_eq!(decode_body("deflate", &z.finish().unwrap()).unwrap(), text);

        let mut raw =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        raw.write_all(&text).unwrap();
        assert_eq!(
            decode_body("deflate", &raw.finish().unwrap()).unwrap(),
            text
        );

        // brotli: precomputed encoding of "hello" (quality 11, window 22)
        let br: [u8; 9] = [0x0b, 0x02, 0x80, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x03];
        assert_eq!(decode_body("br", &br).unwrap(), b"hello");

        assert!(decode_body("gzip", b"not gzip").is_err());
        assert!(decode_body("zstd", b"x").is_err());
        // decompression bombs are capped
        let mut zeros = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
        zeros.write_all(&vec![0u8; 64 * 1024]).unwrap();
        let bomb = zeros.finish().unwrap();
        assert_eq!(
            decode_body_limited("gzip", &bomb, 1024),
            Err(DecodeError::TooLarge)
        );
        assert_eq!(
            decode_body_limited("gzip", &bomb, 64 * 1024).unwrap().len(),
            64 * 1024
        );
    }

    #[test]
    fn merge_cookie_header() {
        let mut h = vec![("Cookie".to_string(), "a=1; b=2".to_string())];
        merge_cookies(&mut h, &["b=3".to_string(), "c=4".to_string()]);
        assert_eq!(h[0].1, "a=1; b=3; c=4");
        let mut h = vec![];
        merge_cookies(&mut h, &["s=1".to_string()]);
        assert_eq!(h, vec![("Cookie".to_string(), "s=1".to_string())]);
    }
}

//! cURL command -> [`Request`] (PRODUCT §5.4, AC-CURL-1 .. AC-CURL-17).

use super::shell::{ShellError, split_words};
use crate::model::{
    Auth, Body, BodyType, FormField, FormFieldKind, KeyValue, Request, RequestSettings,
    STANDARD_METHODS, is_valid_method, new_id,
};
use crate::prepare::{form_urldecode, form_urlencode};
use crate::urlx;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum CurlError {
    /// AC-CURL-17.
    #[error("Not a cURL command")]
    NotCurl,
    #[error("No URL found in the cURL command")]
    MissingUrl,
    #[error("{0}")]
    Syntax(String),
}

impl From<ShellError> for CurlError {
    fn from(e: ShellError) -> Self {
        CurlError::Syntax(e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CurlImport {
    pub request: Request,
    /// Unknown or unsupported flags that were ignored (dismissible warning).
    pub unknown_flags: Vec<String>,
    /// Other non-fatal notes (e.g. "file contents not included").
    pub warnings: Vec<String>,
}

/// Whether text looks like a curl command (URL-bar paste detection).
pub fn looks_like_curl(text: &str) -> bool {
    let t = strip_leading_noise(text);
    let t = t.strip_prefix("$ ").unwrap_or(t);
    t.starts_with("curl ")
        || t.starts_with("curl\t")
        || t.starts_with("curl\\")
        || t.starts_with("curl.exe ")
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arity {
    None,
    One,
}

/// Map a short option char to its long name.
fn short_to_long(c: char) -> Option<&'static str> {
    Some(match c {
        'X' => "request",
        'H' => "header",
        'd' => "data",
        'u' => "user",
        'F' => "form",
        'A' => "user-agent",
        'b' => "cookie",
        'e' => "referer",
        'm' => "max-time",
        'G' => "get",
        'k' => "insecure",
        'L' => "location",
        'I' => "head",
        's' => "silent",
        'S' => "show-error",
        'v' => "verbose",
        'i' => "include",
        'o' => "output",
        'O' => "remote-name",
        'w' => "write-out",
        'c' => "cookie-jar",
        'T' => "upload-file",
        'x' => "proxy",
        'U' => "proxy-user",
        'E' => "cert",
        'r' => "range",
        'Y' => "speed-limit",
        'y' => "speed-time",
        'z' => "time-cond",
        'C' => "continue-at",
        'D' => "dump-header",
        't' => "telnet-option",
        'Q' => "quote",
        'P' => "ftp-port",
        'K' => "config",
        'f' => "fail",
        'g' => "globoff",
        '#' => "progress-bar",
        'n' => "netrc",
        'N' => "no-buffer",
        '0' => "http1.0",
        '4' => "ipv4",
        '6' => "ipv6",
        'j' => "junk-session-cookies",
        'J' => "remote-header-name",
        'Z' => "parallel",
        'R' => "remote-time",
        'p' => "proxytunnel",
        'q' => "disable",
        'V' => "version",
        'h' => "help",
        'M' => "manual",
        'a' => "append",
        'B' => "use-ascii",
        'l' => "list-only",
        '1' => "tlsv1",
        '2' => "sslv2",
        '3' => "sslv3",
        _ => return None,
    })
}

/// Long options that take an argument.
const WITH_ARG: &[&str] = &[
    "request",
    "header",
    "data",
    "data-ascii",
    "data-raw",
    "data-binary",
    "data-urlencode",
    "json",
    "user",
    "form",
    "form-string",
    "user-agent",
    "cookie",
    "referer",
    "max-time",
    "url",
    "output",
    "write-out",
    "cookie-jar",
    "upload-file",
    "proxy",
    "proxy-user",
    "cert",
    "key",
    "cacert",
    "capath",
    "range",
    "connect-timeout",
    "retry",
    "retry-delay",
    "retry-max-time",
    "max-redirs",
    "oauth2-bearer",
    "resolve",
    "connect-to",
    "interface",
    "dns-servers",
    "limit-rate",
    "speed-limit",
    "speed-time",
    "time-cond",
    "continue-at",
    "dump-header",
    "telnet-option",
    "quote",
    "ftp-port",
    "config",
    "keepalive-time",
    "expect100-timeout",
    "local-port",
    "unix-socket",
    "abstract-unix-socket",
    "proto",
    "proto-redir",
    "ciphers",
    "tls-max",
    "pinnedpubkey",
    "cert-type",
    "key-type",
    "pass",
    "aws-sigv4",
    "variable",
    "trace",
    "trace-ascii",
    "stderr",
    "proxy-header",
    "socks4",
    "socks4a",
    "socks5",
    "socks5-hostname",
    "noproxy",
    "preproxy",
    "request-target",
    "hostpubmd5",
    "hostpubsha256",
    "login-options",
    "sasl-authzid",
    "service-name",
    "mail-from",
    "mail-rcpt",
    "mail-auth",
    "output-dir",
    "create-file-mode",
    "etag-save",
    "etag-compare",
    "ech",
    "ipfs-gateway",
    "happy-eyeballs-timeout-ms",
    "netrc-file",
    "random-file",
    "egd-file",
    "crlfile",
    "curves",
    "tls13-ciphers",
    "proxy-cacert",
    "proxy-cert",
    "proxy-key",
    "proxy-pass",
    "proxy-ciphers",
    "doh-url",
    "max-filesize",
    "rate",
    "parallel-max",
    "retry-all-errors-delay",
    "trace-config",
    "url-query",
    "json-file",
];

/// Flags ignored silently (output/transport details with no request effect).
const SILENT: &[&str] = &[
    "silent",
    "show-error",
    "verbose",
    "include",
    "output",
    "remote-name",
    "remote-name-all",
    "progress-bar",
    "write-out",
    "no-progress-meter",
    "fail",
    "fail-with-body",
    "fail-early",
    "compressed",
    "globoff",
    "http1.0",
    "http1.1",
    "http2",
    "http2-prior-knowledge",
    "http3",
    "http3-only",
    "max-redirs",
    "connect-timeout",
    "retry",
    "retry-delay",
    "retry-max-time",
    "retry-connrefused",
    "retry-all-errors",
    "no-buffer",
    "path-as-is",
    "ipv4",
    "ipv6",
    "tcp-nodelay",
    "tcp-fastopen",
    "no-keepalive",
    "keepalive-time",
    "trace",
    "trace-ascii",
    "trace-time",
    "stderr",
    "dump-header",
    "remote-header-name",
    "styled-output",
    "no-styled-output",
    "tlsv1",
    "tlsv1.0",
    "tlsv1.1",
    "tlsv1.2",
    "tlsv1.3",
    "tls-max",
    "create-dirs",
    "output-dir",
    "remote-time",
    "no-sessionid",
    "no-alpn",
    "no-npn",
    "expect100-timeout",
    "show-headers",
    "help",
    "manual",
    "version",
    "location-trusted",
    "no-clobber",
    "parallel",
    "parallel-max",
    "rate",
    "limit-rate",
    "speed-limit",
    "speed-time",
    "disable",
    "suppress-connect-headers",
    "raw",
    "tr-encoding",
    "ssl-no-revoke",
    "ssl-revoke-best-effort",
    "ssl-allow-beast",
    "false-start",
    "happy-eyeballs-timeout-ms",
    "skip-existing",
    "clobber",
    "xattr",
    "no-progress-bar",
];

fn arity(long: &str) -> Arity {
    if WITH_ARG.contains(&long) {
        Arity::One
    } else {
        Arity::None
    }
}

enum DataPart {
    Text(String),
    File(String),
}

#[derive(Default)]
struct State {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<KeyValue>,
    data: Vec<DataPart>,
    json: Vec<String>,
    url_query: Vec<String>,
    form: Vec<FormField>,
    auth: Option<Auth>,
    get: bool,
    head: bool,
    settings: RequestSettings,
    unknown: Vec<String>,
    warnings: Vec<String>,
}

/// Leading whitespace, BOM and zero-width characters (copied from docs/chat).
fn strip_leading_noise(text: &str) -> &str {
    text.trim_start_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '\u{feff}' | '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}'
            )
    })
}

/// Parse a cURL command into a new unsaved request (`collectionId == ""`).
/// Never panics.
pub fn parse_curl(input: &str) -> Result<CurlImport, CurlError> {
    let trimmed = strip_leading_noise(input);
    let trimmed = trimmed.strip_prefix("$ ").unwrap_or(trimmed);
    let words = split_words(trimmed)?;
    let Some(first) = words.first() else {
        return Err(CurlError::NotCurl);
    };
    let cmd = first.text.rsplit(['/', '\\']).next().unwrap_or("");
    if cmd != "curl" && cmd != "curl.exe" {
        return Err(CurlError::NotCurl);
    }

    // Windows cmd "Copy as cURL (cmd)" escapes with `^` (out of scope).
    let cmd_style = words.iter().any(|w| w.text == "^" && !w.quoted)
        || words
            .get(1)
            .is_some_and(|w| w.text.len() > 1 && w.text.starts_with('^'));
    if cmd_style {
        return Err(CurlError::Syntax(
            "Windows cmd syntax (^ escapes) is not supported; use \"Copy as cURL (bash)\"".into(),
        ));
    }
    let mut st = State::default();
    let args: Vec<String> = words.into_iter().skip(1).map(|w| w.text).collect();
    let mut i = 0;
    let mut end_of_options = false;
    while i < args.len() {
        let arg = &args[i];
        i += 1;
        if end_of_options || !arg.starts_with('-') || arg == "-" {
            positional(&mut st, arg);
            continue;
        }
        if arg == "--" {
            end_of_options = true;
            continue;
        }
        if let Some(long) = arg.strip_prefix("--") {
            // `--flag=value` form (AC-CURL-14).
            let (name, inline) = match long.find('=') {
                Some(eq) if arity(&long[..eq]) == Arity::One => {
                    (&long[..eq], Some(long[eq + 1..].to_string()))
                }
                _ => (long, None),
            };
            let value = if arity(name) == Arity::One {
                match inline {
                    Some(v) => Some(v),
                    None => {
                        let v = args.get(i).cloned();
                        if v.is_some() {
                            i += 1;
                        }
                        v
                    }
                }
            } else {
                None
            };
            apply(&mut st, name, value, arg);
        } else {
            // Short flags, possibly combined (`-sSLk`, `-XPOST`).
            let cluster: Vec<char> = arg[1..].chars().collect();
            let mut j = 0;
            while j < cluster.len() {
                let c = cluster[j];
                j += 1;
                let Some(long) = short_to_long(c) else {
                    st.unknown.push(format!("-{c}"));
                    continue;
                };
                if arity(long) == Arity::One {
                    let rest: String = cluster[j..].iter().collect();
                    let value = if !rest.is_empty() {
                        Some(rest)
                    } else {
                        let v = args.get(i).cloned();
                        if v.is_some() {
                            i += 1;
                        }
                        v
                    };
                    apply(&mut st, long, value, &format!("-{c}"));
                    break;
                }
                apply(&mut st, long, None, &format!("-{c}"));
            }
        }
    }
    finish(st)
}

fn positional(st: &mut State, arg: &str) {
    if st.url.is_none() {
        st.url = Some(arg.to_string());
    } else {
        st.warnings
            .push(format!("Ignored extra argument \"{arg}\""));
    }
}

fn add_header(st: &mut State, raw: &str) {
    if raw.starts_with('@') {
        st.warnings.push(format!("Header file {raw} not supported"));
        return;
    }
    let colon = raw.find(':');
    let semi = raw.find(';');
    match (colon, semi) {
        // `-H 'X-Empty;'` -> header with empty value (AC-CURL-4).
        (None, Some(s)) if raw[s + 1..].trim().is_empty() => {
            let name = raw[..s].trim();
            if !name.is_empty() {
                st.headers.push(KeyValue::new(name, ""));
            }
        }
        (Some(c), _) => {
            let name = raw[..c].trim();
            let value = raw[c + 1..].trim();
            if name.is_empty() {
                st.warnings
                    .push(format!("Ignored invalid header \"{raw}\""));
            } else if value.is_empty() {
                // curl semantics: `-H 'Name:'` removes a default header.
            } else {
                st.headers.push(KeyValue::new(name, value));
            }
        }
        _ => st
            .warnings
            .push(format!("Ignored invalid header \"{raw}\"")),
    }
}

fn apply(st: &mut State, name: &str, value: Option<String>, shown: &str) {
    let needs_value = arity(name) == Arity::One;
    let v = match (needs_value, value) {
        (true, Some(v)) => v,
        (true, None) => {
            st.warnings.push(format!("{shown} is missing its value"));
            return;
        }
        (false, _) => String::new(),
    };
    match name {
        "request" => st.method = Some(v.trim().to_string()),
        "url" => {
            if st.url.is_none() {
                st.url = Some(v);
            }
        }
        "header" => add_header(st, &v),
        "data" | "data-ascii" | "data-binary" => {
            if let Some(path) = v.strip_prefix('@') {
                st.data.push(DataPart::File(path.to_string()));
            } else {
                st.data.push(DataPart::Text(v));
            }
        }
        "data-raw" => st.data.push(DataPart::Text(v)),
        // curl 7.87+: like `-G --data-urlencode`; a leading `+` means "as is".
        "url-query" => {
            let part = match v.strip_prefix('+') {
                Some(raw) => raw.to_string(),
                None => data_urlencode(st, &v),
            };
            st.url_query.push(part);
        }
        "data-urlencode" => {
            let encoded = data_urlencode(st, &v);
            st.data.push(DataPart::Text(encoded));
        }
        "json" => st.json.push(v),
        "form" => parse_form(st, &v, false),
        "form-string" => parse_form(st, &v, true),
        "user" => {
            st.auth = Some(match v.find(':') {
                Some(c) => Auth::basic(&v[..c], &v[c + 1..]),
                None => Auth::basic(&v, ""),
            })
        }
        "oauth2-bearer" => st.auth = Some(Auth::bearer(v)),
        "user-agent" => st.headers.push(KeyValue::new("User-Agent", v)),
        "referer" => st.headers.push(KeyValue::new("Referer", v)),
        "cookie" => {
            if v.contains('=') {
                st.headers.push(KeyValue::new("Cookie", v));
            } else {
                st.warnings
                    .push(format!("Cookie file \"{v}\" not imported"));
            }
        }
        "max-time" => match v.trim().parse::<f64>() {
            Ok(secs) if secs.is_finite() && secs >= 0.0 => {
                st.settings.timeout_ms = (secs * 1000.0).round() as u64
            }
            _ => st.warnings.push(format!("Invalid --max-time \"{v}\"")),
        },
        "get" => st.get = true,
        "head" => st.head = true,
        "insecure" => st.settings.verify_tls = false,
        "location" | "location-trusted" => st.settings.follow_redirects = true,
        n if SILENT.contains(&n) => {}
        _ => st.unknown.push(shown.to_string()),
    }
}

/// `--data-urlencode` forms: `content`, `=content`, `name=content`,
/// `@file`, `name@file`.
fn data_urlencode(st: &mut State, v: &str) -> String {
    if let Some(rest) = v.strip_prefix('=') {
        return form_urlencode(rest);
    }
    let eq = v.find('=');
    let at = v.find('@');
    match (eq, at) {
        (Some(e), at) if at.is_none_or(|a| e < a) => {
            format!("{}={}", &v[..e], form_urlencode(&v[e + 1..]))
        }
        (_, Some(a)) => {
            st.warnings.push(format!(
                "--data-urlencode file \"{}\" not read",
                &v[a + 1..]
            ));
            if a == 0 {
                String::new()
            } else {
                format!("{}=", &v[..a])
            }
        }
        _ => form_urlencode(v),
    }
}

/// Split a `-F` value at `;` outside double quotes.
fn split_form_value(v: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            '\\' if in_q => {
                cur.push(c);
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            ';' if !in_q => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    parts
}

fn unquote_form(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('"') && t.ends_with('"') {
        let inner = &t[1..t.len() - 1];
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else {
                out.push(c);
            }
        }
        out
    } else {
        s.to_string()
    }
}

/// `-F name=value`, `-F name=@path;type=x`, `-F name=<path` (AC-CURL-10).
fn parse_form(st: &mut State, v: &str, literal: bool) {
    let Some(eq) = v.find('=') else {
        st.warnings
            .push(format!("Ignored invalid form field \"{v}\""));
        return;
    };
    let name = v[..eq].to_string();
    let rest = &v[eq + 1..];
    if literal {
        st.form.push(FormField::text(name, rest));
        return;
    }
    let parts = split_form_value(rest);
    let head = parts.first().cloned().unwrap_or_default();
    let mut content_type = None;
    for attr in parts.iter().skip(1) {
        let attr = attr.trim();
        if let Some(t) = attr.strip_prefix("type=") {
            content_type = Some(t.to_string());
        }
    }
    let field = if let Some(path) = head.strip_prefix('@').or_else(|| head.strip_prefix('<')) {
        FormField {
            key: name,
            value: unquote_form(path),
            kind: FormFieldKind::File,
            enabled: true,
            content_type,
        }
    } else {
        FormField {
            key: name,
            value: unquote_form(&head),
            kind: FormFieldKind::Text,
            enabled: true,
            content_type,
        }
    };
    st.form.push(field);
}

fn header_value<'a>(headers: &'a [KeyValue], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn is_form_shape(s: &str) -> bool {
    !s.is_empty()
        && s.split('&').all(|pair| match pair.find('=') {
            // An empty key (`=value`) is a valid, if unusual, row.
            Some(i) => !pair[..i].contains(char::is_whitespace),
            None => false,
        })
}

/// Append `extra` (already encoded) to the URL's query string.
fn append_query(url: &str, extra: &str) -> String {
    if extra.is_empty() {
        return url.to_string();
    }
    let (base, query, fragment) = urlx::split_url(url);
    let mut q = query.unwrap_or("").to_string();
    if !q.is_empty() {
        q.push('&');
    }
    q.push_str(extra);
    let mut u = format!("{base}?{q}");
    if let Some(f) = fragment {
        u.push('#');
        u.push_str(f);
    }
    u
}

/// `-b a=1 -b b=2 -H 'Cookie: c=3'` -> one `Cookie: a=1; b=2; c=3` header
/// (RFC 6265 §5.4 allows a single Cookie header), at the first one's place.
fn merge_cookie_headers(headers: &mut Vec<KeyValue>) {
    let positions: Vec<usize> = headers
        .iter()
        .enumerate()
        .filter(|(_, h)| h.key.eq_ignore_ascii_case("cookie"))
        .map(|(i, _)| i)
        .collect();
    if positions.len() < 2 {
        return;
    }
    let merged = positions
        .iter()
        .map(|&i| headers[i].value.trim().trim_end_matches(';').to_string())
        .filter(|v| !v.is_empty())
        .collect::<Vec<_>>()
        .join("; ");
    headers[positions[0]].value = merged;
    for &i in positions.iter().skip(1).rev() {
        headers.remove(i);
    }
}

/// Detect a GraphQL body: a JSON object with exactly a string `query` and an
/// object `variables` — the shape the exporter writes. `{"query": ...}` alone
/// stays JSON (QA-R1-014), keeping cURL round-trips lossless.
fn graphql_body(text: &str) -> Option<Body> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let obj = value.as_object()?;
    let query = obj.get("query")?.as_str()?;
    if obj.len() != 2 {
        return None;
    }
    let variables = match obj.get("variables")? {
        v @ serde_json::Value::Object(o) if !o.is_empty() => {
            serde_json::to_string_pretty(v).ok()?
        }
        serde_json::Value::Object(_) => String::new(),
        _ => return None,
    };
    Some(Body::graphql(query, variables))
}

/// Parse a browser-copied multipart body into form fields.
fn parse_multipart(
    body: &str,
    boundary: &str,
    warnings: &mut Vec<String>,
) -> Option<Vec<FormField>> {
    let delim = format!("--{boundary}");
    if !body.contains(&delim) {
        return None;
    }
    let mut fields = Vec::new();
    for chunk in body.split(&delim).skip(1) {
        if chunk.starts_with("--") {
            break;
        }
        let chunk = chunk
            .strip_prefix("\r\n")
            .or_else(|| chunk.strip_prefix('\n'))
            .unwrap_or(chunk);
        let (head, content) = match chunk.find("\r\n\r\n") {
            Some(i) => (&chunk[..i], &chunk[i + 4..]),
            None => match chunk.find("\n\n") {
                Some(i) => (&chunk[..i], &chunk[i + 2..]),
                None => continue,
            },
        };
        let content = content
            .strip_suffix("\r\n")
            .or_else(|| content.strip_suffix('\n'))
            .unwrap_or(content);
        let mut name = None;
        let mut filename = None;
        let mut ctype = None;
        for line in head.lines() {
            let lower = line.to_ascii_lowercase();
            if lower.starts_with("content-disposition:") {
                name = disposition_param(line, "name");
                filename = disposition_param(line, "filename");
            } else if lower.starts_with("content-type:") {
                ctype = Some(line["content-type:".len()..].trim().to_string());
            }
        }
        let Some(name) = name else { continue };
        match filename {
            Some(f) => {
                warnings.push(format!(
                    "File \"{f}\" for field \"{name}\" is not included in the cURL command; choose it again"
                ));
                fields.push(FormField {
                    key: name,
                    value: f,
                    kind: FormFieldKind::File,
                    enabled: true,
                    content_type: ctype,
                });
            }
            None => fields.push(FormField {
                key: name,
                value: content.to_string(),
                kind: FormFieldKind::Text,
                enabled: true,
                content_type: ctype,
            }),
        }
    }
    Some(fields)
}

fn disposition_param(line: &str, key: &str) -> Option<String> {
    for part in line.split(';').skip(1) {
        let part = part.trim();
        if let Some(rest) = part.strip_prefix(key).and_then(|r| r.strip_prefix('=')) {
            return Some(rest.trim_matches('"').to_string());
        }
    }
    None
}

fn finish(mut st: State) -> Result<CurlImport, CurlError> {
    let Some(url) = st.url.take() else {
        return Err(CurlError::MissingUrl);
    };
    let mut url = url;
    if !st.url_query.is_empty() {
        url = append_query(&url, &st.url_query.join("&"));
    }
    let mut body = Body::none();
    let has_data = !st.data.is_empty();

    if !st.form.is_empty() {
        if has_data || !st.json.is_empty() {
            st.warnings
                .push("Both -F and -d given; the -d data was ignored".into());
        }
        body = Body::form(std::mem::take(&mut st.form));
    } else if !st.json.is_empty() {
        // AC-CURL-8
        body = graphql_body(&st.json.concat()).unwrap_or_else(|| Body::json(st.json.concat()));
        if header_value(&st.headers, "content-type").is_none() {
            st.headers
                .push(KeyValue::new("Content-Type", "application/json"));
        }
        if header_value(&st.headers, "accept").is_none() {
            st.headers.push(KeyValue::new("Accept", "application/json"));
        }
    } else if has_data {
        let only_file = match st.data.as_slice() {
            [DataPart::File(p)] => Some(p.clone()),
            _ => None,
        };
        let joined = st
            .data
            .iter()
            .map(|d| match d {
                DataPart::Text(t) => t.clone(),
                DataPart::File(p) => format!("@{p}"),
            })
            .collect::<Vec<_>>()
            .join("&");
        if st.get {
            // AC-CURL-12: data goes to the query string.
            url = append_query(&url, &joined);
        } else if let Some(path) = only_file {
            if path == "-" {
                st.warnings
                    .push("Body from stdin (@-) cannot be imported".into());
            } else {
                body = Body::binary(path);
            }
        } else {
            if st.data.iter().any(|d| matches!(d, DataPart::File(_))) {
                st.warnings
                    .push("@file data mixed with other data was kept literally".into());
            }
            body = detect_body(&mut st, joined);
        }
    }

    merge_cookie_headers(&mut st.headers);
    let has_body = body.body_type != BodyType::None;
    let method = match st.method.take().filter(|m| !m.is_empty()) {
        // `-X post` -> POST for the standard methods (AC-REQ-1).
        Some(m) if STANDARD_METHODS.contains(&m.to_ascii_uppercase().as_str()) => {
            m.to_ascii_uppercase()
        }
        Some(m) => {
            if !is_valid_method(&m) {
                st.warnings.push(format!(
                    "Method \"{m}\" is not valid (use A–Z, 0–9, _ or -, starting with a letter); fix it before sending"
                ));
            }
            m
        }
        None if st.head => "HEAD".into(),
        None if st.get => "GET".into(),
        None if has_body => "POST".into(),
        None => "GET".into(),
    };
    let params = urlx::url_to_params(&url, &[]);
    let name = request_name(&method, &url);
    let request = Request {
        id: new_id(),
        name,
        method,
        url,
        params,
        headers: st.headers,
        auth: st.auth.unwrap_or_default(),
        body,
        settings: st.settings,
        ..Request::default()
    };
    Ok(CurlImport {
        request,
        unknown_flags: st.unknown,
        warnings: st.warnings,
    })
}

/// AC-CURL-7 body type detection.
fn detect_body(st: &mut State, data: String) -> Body {
    let ct = header_value(&st.headers, "content-type").map(|s| s.to_ascii_lowercase());
    match ct.as_deref() {
        Some(ct) if ct.starts_with("multipart/form-data") => {
            // Boundary in its original case (ASCII lowercasing keeps offsets).
            let original = header_value(&st.headers, "content-type")
                .unwrap_or("")
                .to_string();
            let boundary = ct.find("boundary=").and_then(|idx| {
                let rest = original.get(idx + "boundary=".len()..)?;
                let b = rest
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_matches('"');
                (!b.is_empty()).then(|| b.to_string())
            });
            if let Some(fields) =
                boundary.and_then(|b| parse_multipart(&data, &b, &mut st.warnings))
            {
                // The boundary is regenerated on send.
                st.headers
                    .retain(|h| !h.key.eq_ignore_ascii_case("content-type"));
                return Body::form(fields);
            }
            Body::text_of(BodyType::Text, data)
        }
        Some(ct) if ct.contains("json") => graphql_body(&data).unwrap_or_else(|| Body::json(data)),
        Some(ct) if ct.contains("xml") => Body::text_of(BodyType::Xml, data),
        Some(ct) if ct.contains("application/x-www-form-urlencoded") => {
            if is_form_shape(&data) {
                Body::form_encoded(form_rows(&data))
            } else {
                Body::text_of(BodyType::Text, data)
            }
        }
        Some(_) => Body::text_of(BodyType::Text, data),
        None => {
            if serde_json::from_str::<serde::de::IgnoredAny>(&data).is_ok() {
                graphql_body(&data).unwrap_or_else(|| Body::json(data))
            } else if is_form_shape(&data) {
                Body::form_encoded(form_rows(&data))
            } else {
                Body::text_of(BodyType::Text, data)
            }
        }
    }
}

fn form_rows(data: &str) -> Vec<KeyValue> {
    data.split('&')
        .map(|pair| match pair.find('=') {
            Some(i) => KeyValue::new(form_urldecode(&pair[..i]), form_urldecode(&pair[i + 1..])),
            None => KeyValue::new(form_urldecode(pair), ""),
        })
        .collect()
}

fn request_name(method: &str, url: &str) -> String {
    let (base, _, _) = urlx::split_url(url);
    let without_scheme = base.split_once("://").map(|(_, r)| r).unwrap_or(base);
    let path = without_scheme
        .find('/')
        .map(|i| &without_scheme[i..])
        .unwrap_or("/");
    format!("{method} {path}")
}

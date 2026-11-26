//! Turn an edited [`Request`] into a wire-ready [`PreparedRequest`]:
//! variable substitution, params -> URL, auth, default Content-Type and body
//! encoding (AC-REQ-3/4/5/7/10, AC-ENV-1/3, AC-QRY-2/3, AC-COL-5).
//!
//! This is pure (no I/O): file bodies are referenced by path and read by
//! [`PreparedBody::materialize`].

use crate::model::{
    ApiKeyLocation, Auth, AuthType, BodyType, FormFieldKind, Request, RequestSettings,
    is_valid_method,
};
use crate::urlx;
use crate::vars::{ResolveOptions, Unresolved, VarScope};
use base64::Engine as _;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::Serialize;

/// Characters percent-encoded in `application/x-www-form-urlencoded` bodies
/// (everything except unreserved), matching `curl --data-urlencode`.
const FORM: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// Encode a form component exactly like `curl --data-urlencode`.
pub fn form_urlencode(s: &str) -> String {
    utf8_percent_encode(s, FORM).to_string()
}

/// Decode a form component (`+` is a space).
pub fn form_urldecode(s: &str) -> String {
    let replaced = s.replace('+', " ");
    percent_decode_str(&replaced)
        .decode_utf8_lossy()
        .into_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "camelCase")]
pub enum PrepareError {
    #[error("Invalid method \"{0}\": use A–Z, 0–9, _ or -, starting with a letter")]
    InvalidMethod(String),
    #[error("Invalid GraphQL variables: {0}")]
    InvalidGraphqlVariables(String),
    #[error("URL is empty")]
    EmptyUrl,
}

/// The default Content-Type implied by a body type (AC-REQ-4). Multipart's
/// boundary is appended by [`prepare`].
pub fn default_content_type(body_type: BodyType) -> Option<&'static str> {
    match body_type {
        BodyType::None => None,
        BodyType::Json | BodyType::Graphql => Some("application/json"),
        BodyType::Xml => Some("application/xml"),
        BodyType::Text => Some("text/plain"),
        BodyType::FormEncoded => Some("application/x-www-form-urlencoded"),
        BodyType::Form => Some("multipart/form-data"),
        BodyType::Binary => Some("application/octet-stream"),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPart {
    pub name: String,
    pub kind: FormFieldKind,
    /// Text value, or file path for file parts.
    pub value: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedBody {
    Bytes(Vec<u8>),
    File(String),
    Multipart {
        boundary: String,
        parts: Vec<MultipartPart>,
    },
}

impl PreparedBody {
    /// Read referenced files and produce the bytes to send.
    pub fn materialize(&self) -> std::io::Result<Vec<u8>> {
        match self {
            PreparedBody::Bytes(b) => Ok(b.clone()),
            PreparedBody::File(path) => std::fs::read(path).map_err(|e| {
                std::io::Error::new(e.kind(), format!("Cannot read body file {path}: {e}"))
            }),
            PreparedBody::Multipart { boundary, parts } => {
                let mut out = Vec::new();
                for part in parts {
                    out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
                    let name = escape_disposition(&part.name);
                    match part.kind {
                        FormFieldKind::Text => {
                            out.extend_from_slice(
                                format!("Content-Disposition: form-data; name=\"{name}\"\r\n")
                                    .as_bytes(),
                            );
                            if let Some(ct) = &part.content_type {
                                out.extend_from_slice(format!("Content-Type: {ct}\r\n").as_bytes());
                            }
                            out.extend_from_slice(b"\r\n");
                            out.extend_from_slice(part.value.as_bytes());
                        }
                        FormFieldKind::File => {
                            let data = std::fs::read(&part.value).map_err(|e| {
                                std::io::Error::new(
                                    e.kind(),
                                    format!("Cannot read form file {}: {e}", part.value),
                                )
                            })?;
                            let filename = std::path::Path::new(&part.value)
                                .file_name()
                                .map(|f| f.to_string_lossy().into_owned())
                                .unwrap_or_else(|| part.value.clone());
                            out.extend_from_slice(
                                format!(
                                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\n",
                                    escape_disposition(&filename)
                                )
                                .as_bytes(),
                            );
                            let ct = part
                                .content_type
                                .as_deref()
                                .unwrap_or("application/octet-stream");
                            out.extend_from_slice(format!("Content-Type: {ct}\r\n\r\n").as_bytes());
                            out.extend_from_slice(&data);
                        }
                    }
                    out.extend_from_slice(b"\r\n");
                }
                out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
                Ok(out)
            }
        }
    }
}

fn escape_disposition(s: &str) -> String {
    s.replace('"', "%22")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

/// A fully resolved request, ready for the HTTP engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRequest {
    pub method: String,
    /// URL with variables resolved, params applied and scheme defaulted.
    pub url: String,
    /// Same URL but with secret variables kept as `{{name}}` (for history).
    pub display_url: String,
    /// Header (name, value) pairs in user order and casing, duplicates kept.
    pub headers: Vec<(String, String)>,
    pub body: Option<PreparedBody>,
    pub settings: RequestSettings,
    /// Variables referenced but not defined (AC-ENV-3).
    pub unresolved: Vec<String>,
    /// Extra credential headers (the Auth tab's API-key header) that are
    /// stripped on cross-origin redirects like Authorization/Cookie.
    pub sensitive_headers: Vec<String>,
}

impl PreparedRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// The auth that applies to `request`: its own, or the collection's when set
/// to Inherit (AC-COL-5). A collection auth of Inherit counts as None.
pub fn effective_auth<'a>(
    request_auth: &'a Auth,
    collection_auth: Option<&'a Auth>,
) -> Option<&'a Auth> {
    match request_auth.auth_type {
        AuthType::Inherit => {
            collection_auth.filter(|a| !matches!(a.auth_type, AuthType::Inherit | AuthType::None))
        }
        AuthType::None => None,
        _ => Some(request_auth),
    }
}

fn new_boundary() -> String {
    format!("----ReqHarborFormBoundary{}", crate::model::new_id())
}

/// Prepare a request. `collection_auth` is the parent collection's auth (for
/// Inherit). Use [`prepare_with_boundary`] for deterministic multipart output.
pub fn prepare(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
) -> Result<PreparedRequest, PrepareError> {
    prepare_with_boundary(request, scope, collection_auth, &new_boundary())
}

pub fn prepare_with_boundary(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    boundary: &str,
) -> Result<PreparedRequest, PrepareError> {
    let method = request.method.trim().to_string();
    if !is_valid_method(&method) {
        return Err(PrepareError::InvalidMethod(request.method.clone()));
    }
    if request.url.trim().is_empty() {
        return Err(PrepareError::EmptyUrl);
    }

    let mut unresolved = Unresolved::default();
    let full = ResolveOptions::default();
    let keep = ResolveOptions { keep_secrets: true };
    let mut scratch = Unresolved::default();
    let mut r = |s: &str| scope.resolve(s, full, &mut unresolved);

    // --- headers (AC-REQ-3) ---
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .filter(|h| h.enabled && !h.key.trim().is_empty())
        .map(|h| (r(h.key.trim()), r(&h.value)))
        .collect();
    let has_header = |headers: &[(String, String)], name: &str| {
        headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    };

    // --- auth (AC-REQ-7) ---
    let mut extra_query: Vec<(String, String)> = Vec::new();
    let mut sensitive_headers: Vec<String> = Vec::new();
    let mut extra_query_display: Vec<(String, String)> = Vec::new();
    if let Some(auth) = effective_auth(&request.auth, collection_auth) {
        match auth.auth_type {
            AuthType::Basic => {
                if !has_header(&headers, "Authorization") {
                    let user = r(auth.username.as_deref().unwrap_or(""));
                    let pass = r(auth.password.as_deref().unwrap_or(""));
                    let encoded =
                        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}"));
                    headers.push(("Authorization".into(), format!("Basic {encoded}")));
                }
            }
            AuthType::Bearer => {
                if !has_header(&headers, "Authorization") {
                    let token = r(auth.token.as_deref().unwrap_or(""));
                    headers.push(("Authorization".into(), format!("Bearer {token}")));
                }
            }
            AuthType::Apikey => {
                if let Some(k) = &auth.api_key
                    && !k.key.trim().is_empty()
                {
                    let key = r(k.key.trim());
                    let value = r(&k.value);
                    match k.location {
                        ApiKeyLocation::Header => {
                            sensitive_headers.push(key.clone());
                            if !has_header(&headers, &key) {
                                headers.push((key, value));
                            }
                        }
                        ApiKeyLocation::Query => {
                            let dk = scope.resolve(k.key.trim(), keep, &mut scratch);
                            let dv = scope.resolve(&k.value, keep, &mut scratch);
                            extra_query.push((key, value));
                            extra_query_display.push((dk, dv));
                        }
                    }
                }
            }
            AuthType::None | AuthType::Inherit => {}
        }
    }

    // --- URL (AC-REQ-2, AC-REQ-10) ---
    let build_url =
        |opts: ResolveOptions, extra: &[(String, String)], unresolved: &mut Unresolved| {
            let mut res = |s: &str| scope.resolve(s, opts, unresolved);
            let raw = request.url.trim();
            let mut url = if request.params.is_empty() {
                res(raw)
            } else {
                let (base, _, fragment) = urlx::split_url(raw);
                let params: Vec<_> = request
                    .params
                    .iter()
                    .filter(|p| p.enabled)
                    .map(|p| crate::model::KeyValue::new(res(&p.key), res(&p.value)))
                    .collect();
                let mut u = res(base);
                let q = urlx::build_query(&params);
                if !q.is_empty() {
                    u.push('?');
                    u.push_str(&q);
                }
                if let Some(f) = fragment {
                    u.push('#');
                    u.push_str(f);
                }
                u
            };
            if !extra.is_empty() {
                let (base, query, fragment) = urlx::split_url(&url);
                let mut q = query.unwrap_or("").to_string();
                for (k, v) in extra {
                    if !q.is_empty() {
                        q.push('&');
                    }
                    q.push_str(&urlx::encode_query_key(k));
                    if !v.is_empty() {
                        q.push('=');
                        q.push_str(&urlx::encode_query_value(v));
                    }
                }
                let mut u = format!("{base}?{q}");
                if let Some(f) = fragment {
                    u.push('#');
                    u.push_str(f);
                }
                url = u;
            }
            urlx::apply_default_scheme(&url)
        };
    let url = build_url(full, &extra_query, &mut unresolved);
    let display_url = build_url(keep, &extra_query_display, &mut scratch);

    // --- body (AC-REQ-4, AC-REQ-5, AC-QRY-2/3) ---
    let mut r = |s: &str| scope.resolve(s, full, &mut unresolved);
    let body_def = &request.body;
    let mut content_type = default_content_type(body_def.body_type).map(str::to_string);
    let body = match body_def.body_type {
        BodyType::None => None,
        BodyType::Json | BodyType::Xml | BodyType::Text => {
            Some(PreparedBody::Bytes(r(body_def.text_str()).into_bytes()))
        }
        BodyType::Graphql => {
            let gql = body_def.graphql.clone().unwrap_or_default();
            let query = r(&gql.query);
            let vars_text = r(&gql.variables);
            let variables: serde_json::Value = if vars_text.trim().is_empty() {
                serde_json::Value::Object(Default::default())
            } else {
                serde_json::from_str(&vars_text).map_err(|e| {
                    PrepareError::InvalidGraphqlVariables(format!(
                        "line {}, column {}: {e}",
                        e.line(),
                        e.column()
                    ))
                })?
            };
            let mut obj = serde_json::Map::new();
            obj.insert("query".into(), serde_json::Value::String(query));
            obj.insert("variables".into(), variables);
            let bytes = serde_json::to_vec(&serde_json::Value::Object(obj)).unwrap_or_default();
            Some(PreparedBody::Bytes(bytes))
        }
        BodyType::FormEncoded => {
            let encoded = body_def
                .form_encoded
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|f| f.enabled && !(f.key.is_empty() && f.value.is_empty()))
                .map(|f| {
                    format!(
                        "{}={}",
                        form_urlencode(&r(&f.key)),
                        form_urlencode(&r(&f.value))
                    )
                })
                .collect::<Vec<_>>()
                .join("&");
            Some(PreparedBody::Bytes(encoded.into_bytes()))
        }
        BodyType::Form => {
            let parts = body_def
                .form
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|f| f.enabled && !f.key.is_empty())
                .map(|f| MultipartPart {
                    name: r(&f.key),
                    kind: f.kind,
                    value: r(&f.value),
                    content_type: f.content_type.clone(),
                })
                .collect();
            content_type = Some(format!("multipart/form-data; boundary={boundary}"));
            Some(PreparedBody::Multipart {
                boundary: boundary.to_string(),
                parts,
            })
        }
        BodyType::Binary => Some(PreparedBody::File(r(body_def
            .binary_path
            .as_deref()
            .unwrap_or("")))),
    };
    if let Some(ct) = content_type
        && body.is_some()
        && !has_header(&headers, "Content-Type")
    {
        headers.push(("Content-Type".into(), ct));
    }

    Ok(PreparedRequest {
        method,
        url,
        display_url,
        headers,
        body,
        settings: request.settings,
        unresolved: unresolved.into_names(),
        sensitive_headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Body, Environment, FormField, GraphqlBody, KeyValue, Variable};

    fn req(method: &str, url: &str) -> Request {
        Request {
            method: method.into(),
            url: url.into(),
            ..Request::default()
        }
    }

    fn scope(vars: Vec<Variable>) -> VarScope {
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: None,
            name: "Global".into(),
            variables: vars,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        };
        VarScope::new(None, Some(&env))
    }

    fn p(r: &Request) -> PreparedRequest {
        prepare_with_boundary(r, &VarScope::empty(), None, "B").unwrap()
    }

    fn body_bytes(p: &PreparedRequest) -> Vec<u8> {
        p.body.as_ref().unwrap().materialize().unwrap()
    }

    #[test]
    fn ac_req_1_custom_method_verbatim_and_invalid_rejected() {
        assert_eq!(p(&req("PURGE", "https://x.dev")).method, "PURGE");
        assert_eq!(p(&req("QUERY", "https://x.dev")).method, "QUERY");
        let err = prepare(&req("purge", "https://x.dev"), &VarScope::empty(), None).unwrap_err();
        assert!(matches!(err, PrepareError::InvalidMethod(_)));
    }

    #[test]
    fn ac_req_3_disabled_rows_not_sent_duplicates_kept_casing_kept() {
        let mut r = req("POST", "https://x.dev/a");
        r.params = vec![
            KeyValue::new("a", "1"),
            KeyValue::new("b", "2").disabled(),
            KeyValue::new("a", "3"),
        ];
        r.headers = vec![
            KeyValue::new("X-Dup", "1"),
            KeyValue::new("X-Off", "no").disabled(),
            KeyValue::new("X-Dup", "2"),
            KeyValue::new("x-lower-Case", "v"),
        ];
        r.body = Body::form(vec![
            FormField::text("on", "1"),
            FormField {
                enabled: false,
                ..FormField::text("off", "2")
            },
        ]);
        let pr = p(&r);
        assert_eq!(pr.url, "https://x.dev/a?a=1&a=3");
        assert_eq!(
            &pr.headers[..3],
            &[
                ("X-Dup".to_string(), "1".to_string()),
                ("X-Dup".to_string(), "2".to_string()),
                ("x-lower-Case".to_string(), "v".to_string())
            ]
        );
        let body = String::from_utf8(body_bytes(&pr)).unwrap();
        assert!(body.contains("name=\"on\""));
        assert!(!body.contains("name=\"off\""));
    }

    #[test]
    fn ac_req_4_default_content_types() {
        let cases = [
            (Body::json("{}"), "application/json"),
            (Body::text_of(BodyType::Xml, "<a/>"), "application/xml"),
            (Body::text_of(BodyType::Text, "hi"), "text/plain"),
            (
                Body::form_encoded(vec![KeyValue::new("a", "b")]),
                "application/x-www-form-urlencoded",
            ),
            (Body::graphql("{a}", ""), "application/json"),
            (Body::binary("/tmp/x"), "application/octet-stream"),
            (
                Body::form(vec![FormField::text("a", "b")]),
                "multipart/form-data; boundary=B",
            ),
        ];
        for (body, ct) in cases {
            let mut r = req("POST", "https://x.dev");
            r.body = body;
            assert_eq!(p(&r).header("content-type"), Some(ct));
            // explicit header wins
            r.headers = vec![KeyValue::new("content-type", "application/vnd.custom")];
            let pr = p(&r);
            assert_eq!(pr.header("Content-Type"), Some("application/vnd.custom"));
            assert_eq!(pr.headers.len(), 1);
        }
        assert_eq!(p(&req("GET", "https://x.dev")).header("Content-Type"), None);
    }

    #[test]
    fn ac_req_4_form_multipart_and_form_encoded_bodies() {
        let mut r = req("POST", "https://x.dev");
        r.body = Body::form_encoded(vec![KeyValue::new("a b", "c&d=é"), KeyValue::new("x", "")]);
        assert_eq!(body_bytes(&p(&r)), b"a%20b=c%26d%3D%C3%A9&x=");

        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("up.txt");
        std::fs::write(&file, "FILEDATA").unwrap();
        let mut f = FormField::file("doc", file.to_string_lossy());
        f.content_type = Some("text/plain".into());
        r.body = Body::form(vec![FormField::text("name", "v"), f]);
        let body = String::from_utf8(body_bytes(&p(&r))).unwrap();
        assert_eq!(
            body,
            "--B\r\nContent-Disposition: form-data; name=\"name\"\r\n\r\nv\r\n\
             --B\r\nContent-Disposition: form-data; name=\"doc\"; filename=\"up.txt\"\r\nContent-Type: text/plain\r\n\r\nFILEDATA\r\n--B--\r\n"
        );

        r.body = Body::binary(file.to_string_lossy());
        assert_eq!(body_bytes(&p(&r)), b"FILEDATA");
    }

    #[test]
    fn ac_req_5_graphql_body_and_invalid_variables() {
        let mut r = req("POST", "https://x.dev/graphql");
        r.body = Body::graphql("query { a }", r#"{"id": 1}"#);
        assert_eq!(
            String::from_utf8(body_bytes(&p(&r))).unwrap(),
            r#"{"query":"query { a }","variables":{"id":1}}"#
        );
        r.body = Body {
            body_type: BodyType::Graphql,
            graphql: Some(GraphqlBody {
                query: "q".into(),
                variables: "{oops".into(),
            }),
            ..Body::default()
        };
        assert!(matches!(
            prepare(&r, &VarScope::empty(), None),
            Err(PrepareError::InvalidGraphqlVariables(_))
        ));
    }

    #[test]
    fn ac_req_7_auth_variants_and_explicit_header_wins() {
        let mut r = req("GET", "https://x.dev/p?z=1");
        r.params = vec![KeyValue::new("z", "1")];
        r.auth = Auth::basic("user", "pass");
        assert_eq!(p(&r).header("Authorization"), Some("Basic dXNlcjpwYXNz"));
        r.auth = Auth::bearer("tok");
        assert_eq!(p(&r).header("Authorization"), Some("Bearer tok"));
        r.auth = Auth::api_key("X-Key", "k1", ApiKeyLocation::Header);
        assert_eq!(p(&r).header("x-key"), Some("k1"));
        r.auth = Auth::api_key("api key", "a b", ApiKeyLocation::Query);
        assert_eq!(p(&r).url, "https://x.dev/p?z=1&api%20key=a%20b");
        r.auth = Auth::bearer("tok");
        r.headers = vec![KeyValue::new("authorization", "Custom abc")];
        let pr = p(&r);
        assert_eq!(pr.header("Authorization"), Some("Custom abc"));
        assert_eq!(pr.headers.len(), 1);
    }

    #[test]
    fn ac_col_5_inherit_uses_collection_auth() {
        let mut r = req("GET", "https://x.dev");
        r.auth = Auth::inherit();
        let coll = Auth::bearer("{{tok}}");
        let s = scope(vec![Variable::new("tok", "T1")]);
        let pr = prepare(&r, &s, Some(&coll)).unwrap();
        assert_eq!(pr.header("Authorization"), Some("Bearer T1"));
        let pr = prepare(&r, &s, None).unwrap();
        assert_eq!(pr.header("Authorization"), None);
    }

    #[test]
    fn ac_req_10_scheme_default_applied() {
        assert_eq!(
            p(&req("GET", "localhost:8080/a")).url,
            "http://localhost:8080/a"
        );
        assert_eq!(p(&req("GET", "example.com")).url, "https://example.com");
    }

    #[test]
    fn ac_env_1_resolves_everywhere_and_secrets_masked_in_display_url() {
        let s = scope(vec![
            Variable::new("host", "api.dev"),
            Variable::new("k", "key1"),
            Variable::new("v", "val 1"),
            Variable::new("tok", "SECRET").secret(),
            Variable::new("user", "bob"),
            Variable::new("path", "/tmp/f.bin"),
        ]);
        let mut r = req("POST", "https://{{host}}/x?{{k}}={{v}}&t={{tok}}");
        r.params = crate::urlx::url_to_params(&r.url, &[]);
        r.headers = vec![KeyValue::new("X-{{k}}", "{{v}}")];
        r.auth = Auth::basic("{{user}}", "{{tok}}");
        r.body = Body::text_of(BodyType::Text, "hello {{user}}");
        let pr = prepare(&r, &s, None).unwrap();
        assert_eq!(pr.url, "https://api.dev/x?key1=val%201&t=SECRET");
        assert_eq!(pr.display_url, "https://api.dev/x?key1=val%201&t={{tok}}");
        assert_eq!(pr.header("X-key1"), Some("val 1"));
        assert_eq!(
            pr.header("Authorization"),
            Some(
                format!(
                    "Basic {}",
                    base64::engine::general_purpose::STANDARD.encode("bob:SECRET")
                )
                .as_str()
            )
        );
        assert_eq!(body_bytes(&pr), b"hello bob");
        r.body = Body::binary("{{path}}");
        assert_eq!(
            prepare(&r, &s, None).unwrap().body,
            Some(PreparedBody::File("/tmp/f.bin".into()))
        );
        r.body = Body::form(vec![
            FormField::file("f", "{{path}}"),
            FormField::text("u", "{{user}}"),
        ]);
        match prepare(&r, &s, None).unwrap().body {
            Some(PreparedBody::Multipart { parts, .. }) => {
                assert_eq!(parts[0].value, "/tmp/f.bin");
                assert_eq!(parts[1].value, "bob");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn ac_env_3_unresolved_sent_literally_and_reported() {
        let r = req("GET", "https://{{missing}}/a");
        let pr = p(&r);
        assert_eq!(pr.url, "https://{{missing}}/a");
        assert_eq!(pr.unresolved, vec!["missing".to_string()]);
    }

    #[test]
    fn ac_qry_2_and_3_query_body_handling() {
        let mut r = req("QUERY", "https://x.dev/search");
        r.body = Body::json(r#"{"q":1}"#);
        let pr = p(&r);
        assert_eq!(pr.method, "QUERY");
        assert_eq!(pr.header("Content-Type"), Some("application/json"));
        assert_eq!(body_bytes(&pr), br#"{"q":1}"#);
        r.body = Body::none();
        let pr = p(&r);
        assert!(pr.body.is_none());
        assert_eq!(pr.header("Content-Type"), None);
    }

    #[test]
    fn form_url_coding_round_trips() {
        for s in ["a b", "x+y", "ü&=%", "~._-"] {
            assert_eq!(form_urldecode(&form_urlencode(s)), s);
        }
        assert_eq!(form_urldecode("a+b%20c"), "a b c");
    }
}

//! GraphQL schema support (PRODUCT §5.14, AC-GQL-2..5): the introspection
//! request, reading its answer, schema files, and the local schema cache.
//!
//! GraphQL stays an HTTP body type (`body.type = "graphql"`): introspection
//! is an ordinary send of the request (URL, params, headers, auth, settings,
//! variables) with its body swapped for the standard introspection query, so
//! it goes through [`prepare`] and [`HttpEngine::execute`] like any send. It
//! is not written to history.
//!
//! The cache lives in `<data dir>/graphql-schemas/`, one file per resolved
//! URL (secret variables masked, AC-ENV-7). It is never synced or shared.

use crate::dynamic::DynamicCtx;
use crate::http::HttpEngine;
use crate::model::{Auth, Body, Request, RequestKind, SendError, now_ms};
use crate::prepare::{PrepareError, prepare, prepare_in};
use crate::redact::Redactor;
use crate::storage::atomic_write;
use crate::vars::VarScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

/// Operation name of [`INTROSPECTION_QUERY`].
pub const INTROSPECTION_OPERATION: &str = "IntrospectionQuery";

/// The standard introspection query (graphql-js `getIntrospectionQuery()`
/// with descriptions and no optional extensions), which every GraphQL
/// server understands.
pub const INTROSPECTION_QUERY: &str = "query IntrospectionQuery {
  __schema {
    queryType { name }
    mutationType { name }
    subscriptionType { name }
    types { ...FullType }
    directives { name description locations args { ...InputValue } }
  }
}
fragment FullType on __Type {
  kind
  name
  description
  fields(includeDeprecated: true) {
    name
    description
    args { ...InputValue }
    type { ...TypeRef }
    isDeprecated
    deprecationReason
  }
  inputFields { ...InputValue }
  interfaces { ...TypeRef }
  enumValues(includeDeprecated: true) { name description isDeprecated deprecationReason }
  possibleTypes { ...TypeRef }
}
fragment InputValue on __InputValue {
  name
  description
  type { ...TypeRef }
  defaultValue
}
fragment TypeRef on __Type {
  kind
  name
  ofType {
    kind
    name
    ofType {
      kind
      name
      ofType {
        kind
        name
        ofType {
          kind
          name
          ofType {
            kind
            name
            ofType {
              kind
              name
              ofType { kind name }
            }
          }
        }
      }
    }
  }
}
";

/// Largest schema accepted (fetched or loaded): 10 MB.
pub const SCHEMA_MAX_BYTES: usize = 10 * 1024 * 1024;
/// Largest introspection answer parsed at all (pretty-printed JSON can be a
/// few times the stored schema).
pub const ANSWER_MAX_BYTES: usize = 4 * SCHEMA_MAX_BYTES;
/// Leftover temp files of interrupted cache writes older than this go.
const STALE_TEMP: std::time::Duration = std::time::Duration::from_secs(3600);
/// The cache keeps at most this many bytes of schemas (oldest dropped first).
pub const CACHE_MAX_BYTES: u64 = 50 * 1024 * 1024;
/// … and at most this many schemas.
pub const CACHE_MAX_ENTRIES: usize = 200;
/// Cache directory inside the app data dir.
pub const CACHE_DIR: &str = "graphql-schemas";
const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SchemaSource {
    /// Fetched from the server with the introspection query.
    Introspection,
    /// Loaded from a local file.
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SchemaFormat {
    /// `schema` is JSON text `{"__schema": …}` (an introspection result).
    Introspection,
    /// `schema` is GraphQL SDL text.
    Sdl,
}

/// A schema for one endpoint, as cached and as sent to the UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphqlSchema {
    /// The resolved URL it belongs to (secret variables shown as `{{name}}`):
    /// the cache key.
    pub url: String,
    /// When it was fetched or loaded (epoch ms).
    pub fetched_at: i64,
    pub source: SchemaSource,
    /// File name, for a schema loaded from a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_name: Option<String>,
    pub format: SchemaFormat,
    /// Introspection JSON text or SDL text (see `format`).
    pub schema: String,
    /// Size of `schema` in bytes.
    #[serde(default)]
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum GraphqlError {
    /// Nothing was sent: the request can't be prepared (empty URL, …).
    #[error(transparent)]
    Prepare(#[from] PrepareError),
    /// A network failure (AC-REQ-9 kinds).
    #[error("{}", .0.message)]
    Network(SendError),
    /// The server refuses introspection; carries the server's message.
    #[error("Introspection is disabled on this server")]
    IntrospectionDisabled(String),
    /// Any other answer without a schema.
    #[error("{message}")]
    IntrospectionFailed {
        message: String,
        status: Option<u16>,
    },
    /// A file that isn't a GraphQL schema.
    #[error("This file isn't a GraphQL schema: {0}")]
    Invalid(String),
    /// Over [`SCHEMA_MAX_BYTES`]; carries the size.
    #[error("The schema is larger than 10 MB ({} MB)", .0 / (1024 * 1024))]
    TooLarge(u64),
    #[error("{0}")]
    Io(String),
}

impl From<std::io::Error> for GraphqlError {
    fn from(e: std::io::Error) -> Self {
        GraphqlError::Io(e.to_string())
    }
}

/// The request that fetches the schema: `request` as edited (URL, params,
/// headers, auth, settings) sent as `POST` with the introspection query as
/// its GraphQL body. An explicit Content-Type header is dropped so the body
/// goes out as `application/json`.
pub fn introspection_request(request: &Request) -> Result<Request, PrepareError> {
    if request.kind == RequestKind::Ws {
        return Err(PrepareError::NotHttp);
    }
    let mut r = request.clone();
    r.method = "POST".into();
    r.headers
        .retain(|h| !(h.enabled && h.key.trim().eq_ignore_ascii_case("content-type")));
    let mut body = Body::graphql(INTROSPECTION_QUERY, "");
    if let Some(g) = body.graphql.as_mut() {
        g.operation_name = Some(INTROSPECTION_OPERATION.into());
    }
    r.body = body;
    Ok(r)
}

/// The cache key of a resolved URL: the URL without its fragment.
pub fn schema_key(display_url: &str) -> String {
    display_url
        .split_once('#')
        .map_or(display_url, |(u, _)| u)
        .to_string()
}

/// The cache key `request` fetches its schema from, without sending
/// anything: the resolved URL (fragment dropped) with `{{$dynamic}}`
/// variables kept literal (so `?n={{$uuid}}` stays one entry) and every
/// secret value masked as `{{name}}`, also where a plain variable pulls one
/// in (the same redaction as history, AC-ENV-7).
pub fn schema_url(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
) -> Result<String, PrepareError> {
    let prepared = prepare_in(
        &introspection_request(request)?,
        scope,
        collection_auth,
        "boundary",
        &DynamicCtx::keep(),
    )?;
    Ok(Redactor::from_scope(scope).redact(&schema_key(&prepared.display_url)))
}

/// True when a GraphQL error message says introspection is turned off
/// (Apollo, graphql-js `NoSchemaIntrospectionCustomRule`, Hasura, a schema
/// without `__schema` such as graphql-java's no-introspection visibility).
pub fn is_introspection_refusal(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("introspection") || m.contains("__schema") || m.contains("__type")
}

fn error_messages(v: &Value) -> Vec<String> {
    v.get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|e| e.get("message").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// An introspection result as stored: `{"__schema": …}` from either
/// `{"data": {"__schema": …}}` or a bare `{"__schema": …}`.
fn schema_object(v: &Value) -> Option<String> {
    let schema = v
        .get("data")
        .and_then(|d| d.get("__schema"))
        .or_else(|| v.get("__schema"))
        .filter(|s| s.is_object() && s.get("types").is_some_and(Value::is_array))?;
    let mut out = serde_json::Map::new();
    out.insert("__schema".into(), schema.clone());
    serde_json::to_string(&Value::Object(out)).ok()
}

/// Read the server's answer to the introspection query (AC-GQL-3). Returns
/// the introspection JSON text `{"__schema": …}`.
pub fn read_introspection(
    status: u16,
    status_text: &str,
    body: &[u8],
) -> Result<String, GraphqlError> {
    // Far over the cap: don't even parse it.
    if body.len() > ANSWER_MAX_BYTES {
        return Err(GraphqlError::TooLarge(body.len() as u64));
    }
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    if let Some(v) = &parsed
        && let Some(schema) = schema_object(v)
    {
        if schema.len() > SCHEMA_MAX_BYTES {
            return Err(GraphqlError::TooLarge(schema.len() as u64));
        }
        return Ok(schema);
    }
    let messages = parsed.as_ref().map(error_messages).unwrap_or_default();
    if let Some(m) = messages.iter().find(|m| is_introspection_refusal(m)) {
        return Err(GraphqlError::IntrospectionDisabled(m.clone()));
    }
    let failed = |message: String| GraphqlError::IntrospectionFailed {
        message,
        status: Some(status),
    };
    if let Some(first) = messages.first() {
        return Err(failed(first.clone()));
    }
    if !(200..300).contains(&status) {
        let reason = if status_text.is_empty() {
            String::new()
        } else {
            format!(" {status_text}")
        };
        return Err(failed(format!("The server answered {status}{reason}")));
    }
    Err(failed(if parsed.is_none() {
        "The response isn't JSON: is this a GraphQL endpoint?".into()
    } else {
        "The response has no schema (data.__schema): is this a GraphQL endpoint?".into()
    }))
}

/// Fetch the schema behind `request` (AC-GQL-2). Variables resolve like a
/// send; nothing is written to history.
pub async fn fetch_schema(
    engine: &HttpEngine,
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    cancel: &CancellationToken,
) -> Result<GraphqlSchema, GraphqlError> {
    let url = schema_url(request, scope, collection_auth)?;
    let prepared = prepare(&introspection_request(request)?, scope, collection_auth)?;
    let body = prepared
        .body
        .as_ref()
        .map(|b| b.materialize())
        .transpose()?;
    let response = engine.execute(&prepared, body, cancel).await.map_err(|e| {
        GraphqlError::Network(SendError {
            kind: e.kind,
            // Error texts embed the resolved URL: show the masked one.
            message: e.message.replace(&prepared.url, &prepared.display_url),
        })
    })?;
    let schema = read_introspection(response.status, &response.status_text, &response.body)?;
    Ok(GraphqlSchema {
        url,
        fetched_at: now_ms(),
        source: SchemaSource::Introspection,
        file_name: None,
        format: SchemaFormat::Introspection,
        size_bytes: schema.len() as u64,
        schema,
    })
}

const SDL_KEYWORDS: [&str; 9] = [
    "type",
    "schema",
    "scalar",
    "enum",
    "input",
    "interface",
    "union",
    "extend",
    "directive",
];

/// A schema file (AC-GQL-5): introspection JSON (`{"data":{"__schema":…}}`
/// or `{"__schema":…}`) or SDL. `text` (browser mode, tests) is used instead
/// of reading `path` when given.
pub fn load_schema_file(
    path: Option<&Path>,
    text: Option<String>,
    file_name: Option<String>,
    url: &str,
) -> Result<GraphqlSchema, GraphqlError> {
    let text = match (text, path) {
        (Some(t), _) => t,
        (None, Some(p)) => {
            // Only regular files: opening a FIFO or a device could block or
            // never end.
            let meta = std::fs::metadata(p)?;
            if !meta.is_file() {
                return Err(GraphqlError::Invalid("it isn't a regular file".into()));
            }
            if meta.len() > SCHEMA_MAX_BYTES as u64 {
                return Err(GraphqlError::TooLarge(meta.len()));
            }
            use std::io::Read as _;
            let mut bytes = Vec::new();
            std::fs::File::open(p)?
                .take(SCHEMA_MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() > SCHEMA_MAX_BYTES {
                return Err(GraphqlError::TooLarge(bytes.len() as u64));
            }
            String::from_utf8(bytes)
                .map_err(|_| GraphqlError::Invalid("it isn't UTF-8 text".into()))?
        }
        (None, None) => return Err(GraphqlError::Invalid("no file given".into())),
    };
    if text.len() > SCHEMA_MAX_BYTES {
        return Err(GraphqlError::TooLarge(text.len() as u64));
    }
    let file_name = file_name.or_else(|| {
        path.and_then(|p| p.file_name())
            .map(|f| f.to_string_lossy().into_owned())
    });
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    let is_json = trimmed.starts_with('{')
        || file_name
            .as_deref()
            .is_some_and(|n| n.to_ascii_lowercase().ends_with(".json"));
    let (format, schema) = if is_json {
        let v: Value = serde_json::from_str(trimmed)
            .map_err(|e| GraphqlError::Invalid(format!("invalid JSON ({e})")))?;
        let schema = schema_object(&v).ok_or_else(|| {
            GraphqlError::Invalid("JSON without __schema: expected an introspection result".into())
        })?;
        (SchemaFormat::Introspection, schema)
    } else {
        if trimmed.is_empty() {
            return Err(GraphqlError::Invalid("the file is empty".into()));
        }
        let has_definition = trimmed
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|w| SDL_KEYWORDS.contains(&w));
        if !has_definition {
            return Err(GraphqlError::Invalid(
                "no type definitions found (expected SDL or introspection JSON)".into(),
            ));
        }
        (SchemaFormat::Sdl, trimmed.to_string())
    };
    Ok(GraphqlSchema {
        url: url.to_string(),
        fetched_at: now_ms(),
        source: SchemaSource::File,
        file_name,
        format,
        size_bytes: schema.len() as u64,
        schema,
    })
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CacheFile {
    schema_version: u32,
    #[serde(flatten)]
    schema: GraphqlSchema,
}

/// Schemas on disk, one file per URL (AC-GQL-4).
#[derive(Debug, Clone)]
pub struct SchemaCache {
    dir: PathBuf,
}

impl SchemaCache {
    /// The cache inside the app data dir `data_dir`.
    pub fn new(data_dir: &Path) -> Self {
        SchemaCache {
            dir: data_dir.join(CACHE_DIR),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_for(&self, url: &str) -> PathBuf {
        let digest = ring::digest::digest(&ring::digest::SHA256, url.as_bytes());
        let hex: String = digest
            .as_ref()
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(format!("{hex}.json"))
    }

    /// The cached schema of `url`, if any. An unreadable file counts as
    /// missing (and is removed).
    pub fn get(&self, url: &str) -> Option<GraphqlSchema> {
        let path = self.file_for(url);
        let bytes = std::fs::read(&path).ok()?;
        match serde_json::from_slice::<CacheFile>(&bytes) {
            Ok(f) if f.schema.url == url => {
                let mut s = f.schema;
                s.size_bytes = s.schema.len() as u64;
                Some(s)
            }
            // Another URL with the same hash prefix: leave it alone.
            Ok(_) => None,
            Err(_) => {
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Store `schema` under its URL, then trim the cache to its caps (oldest
    /// first, never the schema just written).
    pub fn put(&self, schema: &GraphqlSchema) -> Result<(), GraphqlError> {
        if schema.schema.len() > SCHEMA_MAX_BYTES {
            return Err(GraphqlError::TooLarge(schema.schema.len() as u64));
        }
        let path = self.file_for(&schema.url);
        let file = CacheFile {
            schema_version: CACHE_SCHEMA_VERSION,
            schema: GraphqlSchema {
                size_bytes: schema.schema.len() as u64,
                ..schema.clone()
            },
        };
        let bytes = serde_json::to_vec(&file).map_err(|e| GraphqlError::Io(e.to_string()))?;
        atomic_write(&path, &bytes).map_err(|e| GraphqlError::Io(e.to_string()))?;
        self.evict(&path, CACHE_MAX_BYTES, CACHE_MAX_ENTRIES);
        Ok(())
    }

    /// Forget the schema of `url`. True when there was one.
    pub fn remove(&self, url: &str) -> Result<bool, GraphqlError> {
        match std::fs::remove_file(self.file_for(url)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    /// Drop the oldest files until at most `max_bytes` / `max_entries` stay.
    /// `keep` is never removed.
    pub fn evict(&self, keep: &Path, max_bytes: u64, max_entries: usize) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return;
        };
        let entries: Vec<_> = entries.filter_map(Result::ok).collect();
        // Temp files of writes that never finished (`.<name>.tmp-<id>`).
        let now = std::time::SystemTime::now();
        for e in &entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name.contains(".tmp-") {
                let old = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|t| now.duration_since(t).unwrap_or_default() > STALE_TEMP);
                if old {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
        let mut files: Vec<(PathBuf, u64, std::time::SystemTime)> = entries
            .iter()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| {
                let meta = e.metadata().ok()?;
                let modified = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                Some((e.path(), meta.len(), modified))
            })
            .collect();
        // Newest first; the kept file counts first whatever its time says.
        files.sort_by_key(|a| std::cmp::Reverse((a.0 == keep, a.2)));
        let (mut total, mut count) = (0u64, 0usize);
        for (path, len, _) in files {
            total += len;
            count += 1;
            if path != keep && (total > max_bytes || count > max_entries) {
                let _ = std::fs::remove_file(&path);
                total -= len;
                count -= 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::KeyValue;

    fn schema_json() -> &'static str {
        r#"{"data":{"__schema":{"queryType":{"name":"Query"},"types":[{"kind":"OBJECT","name":"Query"}]}}}"#
    }

    #[test]
    fn ac_gql_2_introspection_request_keeps_url_headers_auth_and_settings() {
        let mut r = Request {
            method: "GET".into(),
            url: "https://api.dev/graphql".into(),
            headers: vec![
                KeyValue::new("X-Team", "core"),
                KeyValue::new("Content-Type", "application/graphql"),
            ],
            auth: Auth::bearer("tok"),
            ..Request::default()
        };
        r.settings.timeout_ms = 1234;
        r.body = Body::graphql("{ me }", r#"{"a":1}"#);
        let i = introspection_request(&r).unwrap();
        assert_eq!(i.method, "POST");
        assert_eq!(i.url, r.url);
        assert_eq!(i.auth, r.auth);
        assert_eq!(i.settings.timeout_ms, 1234);
        assert_eq!(i.headers, vec![KeyValue::new("X-Team", "core")]);
        let g = i.body.graphql.as_ref().unwrap();
        assert_eq!(g.query, INTROSPECTION_QUERY);
        assert_eq!(g.variables, "");
        assert_eq!(g.operation_name.as_deref(), Some(INTROSPECTION_OPERATION));

        let p = prepare(&i, &VarScope::empty(), None).unwrap();
        assert_eq!(p.header("Content-Type"), Some("application/json"));
        assert_eq!(p.header("Authorization"), Some("Bearer tok"));
        let crate::prepare::PreparedBody::Bytes(bytes) = p.body.unwrap() else {
            panic!("bytes body")
        };
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["operationName"], "IntrospectionQuery");
        assert!(v["query"].as_str().unwrap().contains("__schema"));
        assert_eq!(v["variables"], serde_json::json!({}));

        r.kind = RequestKind::Ws;
        assert_eq!(introspection_request(&r), Err(PrepareError::NotHttp));
    }

    #[test]
    fn ac_gql_4_schema_url_keeps_dynamic_literal_and_redacts_indirect_secrets() {
        use crate::model::{ApiKeyLocation, Environment, Variable};
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Staging".into(),
            variables: vec![
                Variable::new("token", "tok-s3cret-value").secret(),
                // A plain variable that pulls the secret in.
                Variable::new("auth", "Key {{token}}"),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let scope = VarScope::new(Some(&env), None);
        let r = Request {
            url: "https://api.dev/graphql?n={{$uuid}}&a={{auth}}".into(),
            auth: Auth::api_key("api_key", "{{token}}", ApiKeyLocation::Query),
            ..Request::default()
        };
        let a = schema_url(&r, &scope, None).unwrap();
        assert_eq!(
            a,
            schema_url(&r, &scope, None).unwrap(),
            "stable across calls"
        );
        assert!(a.contains("n={{$uuid}}"), "{a}");
        assert!(!a.contains("s3cret"), "{a}");
        assert!(a.contains("{{token}}"), "{a}");
    }

    #[test]
    fn ac_gql_4_schema_url_masks_secrets_and_drops_fragment() {
        use crate::model::{Environment, Variable};
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Staging".into(),
            variables: vec![
                Variable::new("host", "api.dev"),
                Variable::new("key", "s3cret-key").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let scope = VarScope::new(Some(&env), None);
        let r = Request {
            url: "https://{{host}}/graphql?k={{key}}#frag".into(),
            ..Request::default()
        };
        assert_eq!(
            schema_url(&r, &scope, None).unwrap(),
            "https://api.dev/graphql?k={{key}}"
        );
        let empty = Request::default();
        assert_eq!(
            schema_url(&empty, &scope, None),
            Err(PrepareError::EmptyUrl)
        );
    }

    #[test]
    fn ac_gql_3_reading_the_answer() {
        let ok = read_introspection(200, "OK", schema_json().as_bytes()).unwrap();
        let v: Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v["__schema"]["queryType"]["name"], "Query");
        assert!(v.get("data").is_none());

        // Apollo (400) and graphql-js rule (200) refusals.
        for (status, body) in [
            (
                400,
                r#"{"errors":[{"message":"GraphQL introspection is not allowed by Apollo Server, but the query contained __schema or __type."}]}"#,
            ),
            (
                200,
                r#"{"errors":[{"message":"Cannot query field \"__schema\" on type \"Query\"."}]}"#,
            ),
        ] {
            match read_introspection(status, "", body.as_bytes()) {
                Err(GraphqlError::IntrospectionDisabled(m)) => assert!(m.contains("__schema")),
                other => panic!("{status}: {other:?}"),
            }
        }
        match read_introspection(200, "OK", br#"{"errors":[{"message":"Not authorised"}]}"#) {
            Err(GraphqlError::IntrospectionFailed { message, status }) => {
                assert_eq!(message, "Not authorised");
                assert_eq!(status, Some(200));
            }
            other => panic!("{other:?}"),
        }
        match read_introspection(404, "Not Found", b"<html>nope</html>") {
            Err(GraphqlError::IntrospectionFailed { message, .. }) => {
                assert_eq!(message, "The server answered 404 Not Found")
            }
            other => panic!("{other:?}"),
        }
        match read_introspection(200, "OK", b"pong") {
            Err(GraphqlError::IntrospectionFailed { message, .. }) => {
                assert!(message.starts_with("The response isn't JSON"))
            }
            other => panic!("{other:?}"),
        }
        match read_introspection(200, "OK", br#"{"ok":true}"#) {
            Err(GraphqlError::IntrospectionFailed { message, .. }) => {
                assert!(message.contains("data.__schema"))
            }
            other => panic!("{other:?}"),
        }
        // `types` must be a list.
        assert!(read_introspection(200, "OK", br#"{"data":{"__schema":{}}}"#).is_err());
        // Far too big: refused before parsing.
        let huge = vec![b' '; ANSWER_MAX_BYTES + 1];
        assert!(matches!(
            read_introspection(200, "OK", &huge),
            Err(GraphqlError::TooLarge(_))
        ));
    }

    #[test]
    fn ac_gql_5_schema_files() {
        let s =
            load_schema_file(None, Some(schema_json().into()), Some("s.json".into()), "u").unwrap();
        assert_eq!(s.format, SchemaFormat::Introspection);
        assert_eq!(s.source, SchemaSource::File);
        assert_eq!(s.file_name.as_deref(), Some("s.json"));
        assert!(s.schema.starts_with(r#"{"__schema""#));
        let bare =
            load_schema_file(None, Some(r#"{"__schema":{"types":[]}}"#.into()), None, "u").unwrap();
        assert_eq!(bare.format, SchemaFormat::Introspection);

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("schema.graphql");
        std::fs::write(&p, "\u{feff}type Query { me: String }\n").unwrap();
        let sdl = load_schema_file(Some(&p), None, None, "https://x/graphql").unwrap();
        assert_eq!(sdl.format, SchemaFormat::Sdl);
        assert_eq!(sdl.file_name.as_deref(), Some("schema.graphql"));
        assert_eq!(sdl.schema, "type Query { me: String }");
        assert_eq!(sdl.url, "https://x/graphql");
        assert_eq!(sdl.size_bytes, sdl.schema.len() as u64);

        for bad in ["", "hello world", r#"{"a":1}"#, "{oops"] {
            assert!(
                matches!(
                    load_schema_file(None, Some(bad.into()), None, "u"),
                    Err(GraphqlError::Invalid(_))
                ),
                "{bad:?}"
            );
        }
        // Directories (and other non-regular files) are refused without reading.
        assert!(matches!(
            load_schema_file(Some(dir.path()), None, None, "u"),
            Err(GraphqlError::Invalid(m)) if m.contains("regular file")
        ));
        let big_file = dir.path().join("big.graphql");
        std::fs::write(&big_file, vec![b'a'; SCHEMA_MAX_BYTES + 1]).unwrap();
        assert!(matches!(
            load_schema_file(Some(&big_file), None, None, "u"),
            Err(GraphqlError::TooLarge(_))
        ));
        let big = "type Q { a: Int }\n".repeat(SCHEMA_MAX_BYTES / 10);
        assert!(matches!(
            load_schema_file(None, Some(big), None, "u"),
            Err(GraphqlError::TooLarge(_))
        ));
    }

    fn sample(url: &str, size: usize) -> GraphqlSchema {
        GraphqlSchema {
            url: url.into(),
            fetched_at: 1,
            source: SchemaSource::Introspection,
            file_name: None,
            format: SchemaFormat::Sdl,
            schema: "x".repeat(size),
            size_bytes: 0,
        }
    }

    #[test]
    fn ac_gql_4_cache_put_get_remove() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SchemaCache::new(dir.path());
        assert!(cache.get("https://a/graphql").is_none());
        let s = sample("https://a/graphql", 10);
        cache.put(&s).unwrap();
        let got = cache.get("https://a/graphql").unwrap();
        assert_eq!(got.schema, s.schema);
        assert_eq!(got.size_bytes, 10);
        assert!(cache.get("https://b/graphql").is_none());
        // On disk under graphql-schemas/, keyed by a hash (no URL in the name).
        let names: Vec<String> = std::fs::read_dir(dir.path().join(CACHE_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].ends_with(".json") && !names[0].contains("graphql"));
        // Replacing keeps one file.
        cache.put(&sample("https://a/graphql", 20)).unwrap();
        assert_eq!(cache.get("https://a/graphql").unwrap().size_bytes, 20);
        assert!(cache.remove("https://a/graphql").unwrap());
        assert!(!cache.remove("https://a/graphql").unwrap());
        assert!(cache.get("https://a/graphql").is_none());

        // A corrupt file is dropped.
        cache.put(&s).unwrap();
        std::fs::write(cache.file_for("https://a/graphql"), b"{not json").unwrap();
        assert!(cache.get("https://a/graphql").is_none());
        assert!(!cache.file_for("https://a/graphql").exists());

        assert!(matches!(
            cache.put(&sample("https://big", SCHEMA_MAX_BYTES + 1)),
            Err(GraphqlError::TooLarge(_))
        ));
    }

    #[test]
    fn ac_gql_4_cache_evicts_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = SchemaCache::new(dir.path());
        let t0 = std::time::SystemTime::now() - std::time::Duration::from_secs(1000);
        for (i, u) in ["https://1", "https://2", "https://3"].iter().enumerate() {
            cache.put(&sample(u, 100)).unwrap();
            let f = std::fs::File::options()
                .write(true)
                .open(cache.file_for(u))
                .unwrap();
            f.set_modified(t0 + std::time::Duration::from_secs(i as u64 * 10))
                .unwrap();
        }
        // An entry cap of 2 drops the oldest; the kept file survives even
        // though it is the oldest.
        let keep = cache.file_for("https://1");
        cache.evict(&keep, u64::MAX, 2);
        assert!(cache.get("https://1").is_some());
        assert!(cache.get("https://2").is_none());
        assert!(cache.get("https://3").is_some());
        // A byte cap below two files keeps only the newest (plus `keep`).
        cache.put(&sample("https://4", 100)).unwrap();
        let keep = cache.file_for("https://4");
        let one = std::fs::metadata(&keep).unwrap().len();
        cache.evict(&keep, one + one / 2, 100);
        assert!(cache.get("https://4").is_some());
        assert!(cache.get("https://1").is_none());
        assert!(cache.get("https://3").is_none());
        // Stale temp files of interrupted writes go; fresh ones stay.
        let stale = cache.dir().join(".abc.json.tmp-01OLD");
        let fresh = cache.dir().join(".abc.json.tmp-01NEW");
        std::fs::write(&stale, b"x").unwrap();
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&stale)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
            .unwrap();
        cache.evict(&keep, u64::MAX, 100);
        assert!(!stale.exists());
        assert!(fresh.exists());
    }
}

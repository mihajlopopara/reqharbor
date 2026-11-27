//! OpenAPI / Swagger import (PRODUCT.md §5.6, AC-OAS-1..9, AC-QRY-6).
//!
//! [`import_openapi`] turns a Swagger 2.0 or OpenAPI 3.0 / 3.1 / 3.2 document
//! (JSON or YAML text) into one [`Collection`], one [`Folder`] per tag, one
//! [`Request`] per operation and an [`Environment`]. It is a pure function:
//! nothing is fetched or persisted, and on error no partial data is returned.
//! [`detect_spec`] runs only the format/version detection (import dialog chip).
//!
//! Behaviour:
//! - **Format / version (AC-OAS-1).** JSON when the trimmed text starts with
//!   `{` and parses as JSON, otherwise YAML. `swagger: "2.0"` is Swagger 2.0,
//!   `openapi: 3.0.x | 3.1.x | 3.2.x` is OpenAPI 3; anything else (including
//!   non-mapping documents) is [`OpenApiError::Unsupported`]. A `paths` or
//!   `info` that is present but not a mapping is [`OpenApiError::Invalid`]; a
//!   document without `paths` imports with zero operations.
//! - **Structure (AC-OAS-2).** Collection named `info.title` (fallback
//!   "Imported API"), description from `info.description`. One folder per
//!   distinct *first* tag that an operation uses, ordered by the top-level
//!   `tags` declaration, then by first use. Untagged operations sit at the
//!   collection root. Root `sortOrder`s run folders first, then root requests;
//!   inside a folder requests are numbered from 0. Operations follow document
//!   order (paths, then methods as they appear in each path item).
//! - **Requests (AC-OAS-3, AC-QRY-6).** Name = `summary` ‖ `operationId` ‖
//!   `METHOD /path`. URL = `{{baseUrl}}` + path with `{p}` → `{{p}}`, plus the
//!   enabled query params. Methods: get put post delete options head patch
//!   trace, plus (OpenAPI 3) `query` → `QUERY` and the 3.2
//!   `additionalOperations` map (key upper-cased, must match
//!   `^[A-Z][A-Z0-9_-]*$`, otherwise skipped with a warning). Requests use
//!   `Auth::inherit()`, or `Auth::none()` when the operation declares
//!   `security: []`.
//! - **Environment (AC-OAS-4).** Named after the API. `baseUrl` = first
//!   `servers[].url` with server variables replaced by their `default` (or
//!   first `enum`) — relative server URLs are kept as-is with a warning; with
//!   no servers `baseUrl` is `""` with a warning (the UI asks the user to fill
//!   it in) — or, for 2.0, `scheme://host + basePath` with the first of
//!   `schemes` (default `https`); without `host` only `basePath` is used, with
//!   a warning. A trailing `/` is always trimmed because paths start with `/`.
//!   Then one empty variable per distinct path template name (first-seen
//!   order), then the auth variables: `token` (secret) for bearer,
//!   `username` / `password` (secret) for basic, `apiKey` (secret) for API key.
//! - **Parameters (AC-OAS-5).** Path-level and operation-level parameters are
//!   merged (operation wins on `name` + `in`), `$ref`s resolved. Query → params
//!   rows, header → header rows (OpenAPI 3 ignores `Accept`, `Content-Type`,
//!   `Authorization` header params as the spec requires); required → enabled,
//!   optional → disabled. Value = example ‖ first `examples` value ‖
//!   schema example ‖ default ‖ schema default ‖ first enum ‖ `""`, non-strings
//!   as compact JSON. The parameter description becomes the row description.
//! - **Bodies (AC-OAS-6).** Media type: `application/json`, else any `*+json`,
//!   else the first. JSON: media `example` ‖ first `examples` value ‖ schema
//!   skeleton, pretty-printed with 2 spaces. XML: string example or empty.
//!   `text/*`: example as text. `multipart/*` → Form fields (file for
//!   `format: binary|base64` / 2.0 `type: file`), urlencoded → Form-encode
//!   rows; all fields enabled with their example/default/enum value. Binary
//!   media (`application/octet-stream`, `image/*`, `audio/*`, `video/*`,
//!   `application/pdf`, `application/zip`) → Binary body with empty path.
//!   Unknown media types → Text. A `Content-Type` header row is added only when
//!   the media type differs from the body type's default (json →
//!   `application/json`, xml → `application/xml`, text → `text/plain`, binary →
//!   `application/octet-stream`; forms never). Swagger 2.0: `in: body` → JSON
//!   (XML when every `consumes` entry is XML); `in: formData` → Form when
//!   `consumes` has `multipart/*` or a param has `type: file`, else
//!   Form-encode.
//! - **Skeletons.** Objects list all properties (example / examples[0] /
//!   const / default / enum[0] when present, else `""`, `0`, `false`); arrays
//!   hold one item; `allOf` is merged, `oneOf` / `anyOf` take the first usable
//!   branch. At most 5 levels of objects/arrays are expanded (deeper ones
//!   become `{}` / `[]`), a `$ref` cycle becomes `{}`, and a per-body node
//!   budget bounds pathological fan-out. Resolved `$ref` skeletons are cached
//!   per (ref, depth) when they did not depend on the expansion path.
//! - **Auth (AC-OAS-7).** Collection auth comes from the top-level `security`
//!   requirements (first scheme that maps); with no top-level `security` and
//!   exactly one scheme defined, that scheme. HTTP bearer → Bearer
//!   `{{token}}`, HTTP basic / 2.0 basic → Basic `{{username}}`/`{{password}}`,
//!   apiKey in header/query → API Key `{{apiKey}}`; apiKey in cookie, OAuth2,
//!   OpenID Connect, mutual TLS and other HTTP schemes → No Auth + warning.
//! - **References (AC-OAS-8).** Only internal JSON-pointer refs (`#/...`) are
//!   followed. External refs (anything not starting with `#`) are never
//!   fetched: the parameter / body / property is skipped and a warning names
//!   the ref and the operation. Unresolvable internal refs do the same.
//! - **Performance (AC-OAS-9).** The document is parsed once and borrowed
//!   throughout; nothing clones the whole tree.
//! - Never panics on any input.

use std::collections::{HashMap, HashSet};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::model::{
    ApiKeyLocation, Auth, Body, BodyType, Collection, Environment, Folder, FormField, KeyValue,
    Request, RequestSettings, Variable, is_valid_method, new_id, now_ms,
};
use crate::urlx::params_to_url;

type Obj = Map<String, Value>;

/// Objects/arrays nested deeper than this become `{}` / `[]` in skeletons.
const MAX_DEPTH: usize = 5;
/// Maximum `$ref` hops followed for one reference chain.
const MAX_REF_HOPS: usize = 32;
/// Maximum `$ref` nesting while generating one skeleton (stack-overflow guard).
const MAX_REF_STACK: usize = 64;
/// Maximum schema nodes visited while generating one body skeleton.
const SKELETON_BUDGET: usize = 20_000;
/// Fixed-field operation keys of a path item (lowercase, as the spec requires).
const HTTP_METHODS: [&str; 8] = [
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecFormat {
    Json,
    Yaml,
}

impl SpecFormat {
    fn label(self) -> &'static str {
        match self {
            SpecFormat::Json => "JSON",
            SpecFormat::Yaml => "YAML",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecKind {
    Swagger2,
    OpenApi3,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecInfo {
    pub format: SpecFormat,
    pub kind: SpecKind,
    /// Raw version string, e.g. "2.0", "3.0.3", "3.1.0", "3.2.0".
    pub version: String,
    /// UI chip text, e.g. "Swagger 2.0 · JSON", "OpenAPI 3.1 · YAML".
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    pub spec: SpecInfo,
    pub collection_name: String,
    pub operations_created: usize,
    /// Folders in creation order.
    pub folders: Vec<FolderSummary>,
    /// Untagged operations (collection root).
    pub root_request_count: usize,
    pub base_url: String,
    /// Human-readable, deduplicated, in the order they occurred.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FolderSummary {
    pub name: String,
    pub request_count: usize,
}

#[derive(Debug, Clone)]
pub struct OpenApiImport {
    pub collection: Collection,
    pub folders: Vec<Folder>,
    pub requests: Vec<Request>,
    pub environment: Environment,
    pub report: ImportReport,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OpenApiError {
    #[error("Not valid JSON or YAML: {0}")]
    Parse(String),
    /// Not Swagger 2.0 / OpenAPI 3.0-3.2.
    #[error("Unsupported format: {0}")]
    Unsupported(String),
    #[error("Invalid spec: {0}")]
    Invalid(String),
}

/// Detect format + version only (for the import dialog chip). Same errors as
/// [`import_openapi`] for unparsable or unsupported documents.
pub fn detect_spec(text: &str) -> Result<SpecInfo, OpenApiError> {
    let (doc, format) = parse_document(text)?;
    spec_info(&doc, format)
}

/// Full import. All new entities get fresh ULIDs ([`new_id`]) and
/// `createdAt` / `updatedAt` = [`now_ms`].
pub fn import_openapi(
    text: &str,
    workspace_id: &str,
    project_id: &str,
) -> Result<OpenApiImport, OpenApiError> {
    let (doc, format) = parse_document(text)?;
    let spec = spec_info(&doc, format)?;
    let Some(root) = doc.as_object() else {
        return Err(OpenApiError::Invalid(
            "the document root must be a mapping".into(),
        ));
    };
    match root.get("paths") {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        Some(_) => return Err(OpenApiError::Invalid("`paths` must be a mapping".into())),
    }
    match root.get("info") {
        None | Some(Value::Null) | Some(Value::Object(_)) => {}
        Some(_) => return Err(OpenApiError::Invalid("`info` must be a mapping".into())),
    }
    let importer = Importer {
        doc: &doc,
        root,
        kind: spec.kind,
        warnings: Vec::new(),
        seen_warnings: HashSet::new(),
        warn_attempts: 0,
        cycle_cuts: 0,
        budget: SKELETON_BUDGET,
        skel_cache: HashMap::new(),
    };
    Ok(importer.run(spec, workspace_id, project_id))
}

// ---------------------------------------------------------------------------
// Parsing and detection
// ---------------------------------------------------------------------------

/// Maximum `[`/`{` nesting accepted before any parser runs. Real specs stay
/// far below this; deeper input is hostile and parsers handle it in
/// super-linear time (QA-R1-008).
pub const MAX_NESTING: usize = 256;

/// Linear pre-scan of flow-collection nesting (brackets outside double
/// quoted strings).
fn check_nesting(text: &str) -> Result<(), OpenApiError> {
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for b in text.bytes() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'[' | b'{' => {
                depth += 1;
                if depth > MAX_NESTING {
                    return Err(OpenApiError::Invalid(format!(
                        "the document is nested more than {MAX_NESTING} levels deep"
                    )));
                }
            }
            b']' | b'}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    Ok(())
}

/// Parse YAML applying merge keys (`<<: *anchor`, QA-R1-022).
fn parse_yaml(text: &str) -> Result<Value, String> {
    let mut y: serde_yaml_ng::Value = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
    y.apply_merge().map_err(|e| e.to_string())?;
    serde_json::to_value(y).map_err(|e| e.to_string())
}

fn parse_document(text: &str) -> Result<(Value, SpecFormat), OpenApiError> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.is_empty() {
        return Err(OpenApiError::Parse("the document is empty".into()));
    }
    check_nesting(trimmed)?;
    if trimmed.starts_with('{') {
        return match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => Ok((v, SpecFormat::Json)),
            // `{openapi: 3.1.0}` is a YAML flow mapping, not JSON.
            Err(json_err) => match parse_yaml(trimmed) {
                Ok(v) => Ok((v, SpecFormat::Yaml)),
                Err(_) => Err(OpenApiError::Parse(json_err.to_string())),
            },
        };
    }
    parse_yaml(trimmed)
        .map(|v| (v, SpecFormat::Yaml))
        .map_err(OpenApiError::Parse)
}

fn version_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        other => other.to_string(),
    }
}

/// Minor version for `3.0`, `3.1.x`, `3.2.0-rc1`, ... (3.0 to 3.2 only).
fn oas3_minor(version: &str) -> Option<u32> {
    let mut parts = version.splitn(3, '.');
    if parts.next()? != "3" {
        return None;
    }
    let minor = parts.next()?;
    if minor.is_empty() || !minor.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let minor: u32 = minor.parse().ok()?;
    (minor <= 2).then_some(minor)
}

fn spec_info(doc: &Value, format: SpecFormat) -> Result<SpecInfo, OpenApiError> {
    const SUPPORTED: &str = "supported: Swagger 2.0 and OpenAPI 3.0, 3.1, 3.2";
    let Some(root) = doc.as_object() else {
        return Err(OpenApiError::Unsupported(format!(
            "expected a mapping with an `openapi` or `swagger` field ({SUPPORTED})"
        )));
    };
    if let Some(v) = root.get("openapi") {
        let version = version_string(v);
        return match oas3_minor(&version) {
            Some(minor) => Ok(SpecInfo {
                format,
                kind: SpecKind::OpenApi3,
                label: format!("OpenAPI 3.{minor} · {}", format.label()),
                version,
            }),
            None => Err(OpenApiError::Unsupported(format!(
                "OpenAPI version \"{version}\" ({SUPPORTED})"
            ))),
        };
    }
    if let Some(v) = root.get("swagger") {
        let version = version_string(v);
        return if version == "2.0" {
            Ok(SpecInfo {
                format,
                kind: SpecKind::Swagger2,
                label: format!("Swagger 2.0 · {}", format.label()),
                version,
            })
        } else {
            Err(OpenApiError::Unsupported(format!(
                "Swagger version \"{version}\" ({SUPPORTED})"
            )))
        };
    }
    Err(OpenApiError::Unsupported(format!(
        "no `openapi` or `swagger` version field ({SUPPORTED})"
    )))
}

// ---------------------------------------------------------------------------
// Small value helpers
// ---------------------------------------------------------------------------

fn non_null(v: Option<&Value>) -> Option<&Value> {
    v.filter(|v| !v.is_null())
}

fn first_of(v: Option<&Value>) -> Option<&Value> {
    non_null(v.and_then(Value::as_array).and_then(|a| a.first()))
}

fn non_empty_str(v: Option<&Value>) -> Option<&str> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Strings verbatim, `null` as empty, everything else as compact JSON.
fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn ref_of(v: &Value) -> Option<&str> {
    v.as_object()?.get("$ref")?.as_str()
}

/// A literal value a schema offers: example, examples[0], const, default, enum[0].
fn literal_example(o: &Obj) -> Option<&Value> {
    non_null(o.get("example"))
        .or_else(|| first_of(o.get("examples")))
        .or_else(|| non_null(o.get("const")))
        .or_else(|| non_null(o.get("default")))
        .or_else(|| first_of(o.get("enum")))
}

/// The schema type, preferring the first non-"null" entry of a 3.1 type
/// array, and inferring object/array from `properties` / `items`.
fn schema_type(o: &Obj) -> Option<&str> {
    let declared = match o.get("type") {
        Some(Value::String(s)) => Some(s.as_str()),
        Some(Value::Array(a)) => {
            let mut names = a.iter().filter_map(Value::as_str);
            let first = names.clone().next();
            names.find(|t| *t != "null").or(first)
        }
        _ => None,
    };
    declared.or_else(|| {
        if o.contains_key("properties") || o.contains_key("additionalProperties") {
            Some("object")
        } else if o.contains_key("items") {
            Some("array")
        } else {
            None
        }
    })
}

/// Lowercased media type without parameters.
fn essence(media_type: &str) -> String {
    media_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase()
}

fn is_json_media(ess: &str) -> bool {
    ess == "application/json" || ess.ends_with("+json") || ess.ends_with("/json")
}

fn is_xml_media(ess: &str) -> bool {
    ess.ends_with("/xml") || ess.ends_with("+xml")
}

/// `/pets/{petId}` → `/pets/{{petId}}`, collecting the template names.
fn template_path(path: &str, vars: &mut Vec<String>, seen: &mut HashSet<String>) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('}') {
            Some(end) if end > 0 && !after[..end].contains('{') => {
                let name = &after[..end];
                out.push_str("{{");
                out.push_str(name);
                out.push_str("}}");
                if seen.insert(name.to_string()) {
                    vars.push(name.to_string());
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// JSON body text for a media/parameter example.
fn render_json_example(ex: &Value) -> String {
    match ex {
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(v @ (Value::Object(_) | Value::Array(_))) => pretty(&v),
            _ => s.clone(),
        },
        other => pretty(other),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MediaKind {
    Json,
    Xml,
    Text,
    Multipart,
    UrlEncoded,
    Binary,
}

fn classify_media(ess: &str) -> MediaKind {
    if ess.starts_with("multipart/") {
        MediaKind::Multipart
    } else if ess == "application/x-www-form-urlencoded" {
        MediaKind::UrlEncoded
    } else if is_json_media(ess) || ess == "*/*" {
        MediaKind::Json
    } else if is_xml_media(ess) {
        MediaKind::Xml
    } else if ess.starts_with("text/") {
        MediaKind::Text
    } else if ess == "application/octet-stream"
        || ess == "application/pdf"
        || ess == "application/zip"
        || ess.starts_with("image/")
        || ess.starts_with("audio/")
        || ess.starts_with("video/")
    {
        MediaKind::Binary
    } else {
        MediaKind::Text
    }
}

/// Explicit `Content-Type` row value when the media type is not the body
/// type's default.
fn content_type_header(body_type: BodyType, media_type: &str, ess: &str) -> Option<String> {
    let default = match body_type {
        BodyType::Json => "application/json",
        BodyType::Xml => "application/xml",
        BodyType::Text => "text/plain",
        BodyType::Binary => "application/octet-stream",
        _ => return None,
    };
    if ess == default || ess == "*/*" || ess.is_empty() {
        None
    } else {
        Some(media_type.trim().to_string())
    }
}

fn auth_vars(auth: &Auth) -> Vec<Variable> {
    match auth.auth_type {
        crate::model::AuthType::Bearer => vec![Variable::new("token", "").secret()],
        crate::model::AuthType::Basic => vec![
            Variable::new("username", ""),
            Variable::new("password", "").secret(),
        ],
        crate::model::AuthType::Apikey => vec![Variable::new("apiKey", "").secret()],
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Importer
// ---------------------------------------------------------------------------

struct Operation<'a> {
    path: &'a str,
    method: String,
    item: &'a Obj,
    op: &'a Obj,
}

struct Importer<'a> {
    doc: &'a Value,
    root: &'a Obj,
    kind: SpecKind,
    warnings: Vec<String>,
    seen_warnings: HashSet<String>,
    /// Counts every warning raised (including duplicates) so skeleton caching
    /// can tell whether a subtree produced warnings.
    warn_attempts: usize,
    /// Counts skeleton cuts that depend on the expansion path (cycles, budget).
    cycle_cuts: usize,
    budget: usize,
    skel_cache: HashMap<(&'a str, usize), Value>,
}

type BodyResult = Option<(Body, Option<String>)>;

impl<'a> Importer<'a> {
    fn warn(&mut self, msg: String) {
        self.warn_attempts += 1;
        if self.seen_warnings.insert(msg.clone()) {
            self.warnings.push(msg);
        }
    }

    /// Resolve one `$ref` string to its target, or a warning message.
    fn lookup(&self, r: &str, ctx: &str) -> Result<&'a Value, String> {
        let Some(fragment) = r.strip_prefix('#') else {
            return Err(format!("Skipped external $ref {r} in {ctx}"));
        };
        let decoded = percent_encoding::percent_decode_str(fragment)
            .decode_utf8()
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| fragment.to_string());
        if !decoded.is_empty() && !decoded.starts_with('/') {
            return Err(format!("Skipped unresolvable $ref {r} in {ctx}"));
        }
        self.doc
            .pointer(&decoded)
            .ok_or_else(|| format!("Skipped unresolvable $ref {r} in {ctx}"))
    }

    /// Follow a `$ref` chain. `None` (after a warning) when the chain is
    /// external, unresolvable or loops.
    fn deref(&mut self, v: &'a Value, ctx: &str) -> Option<&'a Value> {
        let mut cur = v;
        for _ in 0..MAX_REF_HOPS {
            let Some(r) = ref_of(cur) else {
                return Some(cur);
            };
            match self.lookup(r, ctx) {
                Ok(target) => cur = target,
                Err(msg) => {
                    self.warn(msg);
                    return None;
                }
            }
        }
        let r = ref_of(v).unwrap_or("");
        self.warn(format!("Skipped circular $ref {r} in {ctx}"));
        None
    }

    fn run(mut self, spec: SpecInfo, workspace_id: &str, project_id: &str) -> OpenApiImport {
        let now = now_ms();
        let info = self.root.get("info");
        let title = non_empty_str(info.and_then(|i| i.get("title")))
            .unwrap_or("Imported API")
            .to_string();
        let description = info
            .and_then(|i| i.get("description"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        let base_url = self.base_url();
        let collection_auth = self.collection_auth();
        let ops = self.collect_operations();

        let collection = Collection {
            id: new_id(),
            project_id: project_id.to_string(),
            name: title.clone(),
            description,
            auth: collection_auth.clone(),
            sort_order: 0,
            created_at: now,
            updated_at: now,
            extra: Default::default(),
        };

        // Folders: first tags in use, declared order first, then first-seen.
        let first_tags: Vec<Option<&'a str>> = ops
            .iter()
            .map(|o| {
                o.op.get("tags")
                    .and_then(Value::as_array)
                    .and_then(|t| t.first())
                    .and_then(Value::as_str)
                    .filter(|t| !t.trim().is_empty())
            })
            .collect();
        let used: HashSet<&str> = first_tags.iter().flatten().copied().collect();
        let mut order: Vec<&str> = Vec::new();
        let mut placed: HashSet<&str> = HashSet::new();
        if let Some(declared) = self.root.get("tags").and_then(Value::as_array) {
            for name in declared
                .iter()
                .filter_map(|t| t.get("name").and_then(Value::as_str))
            {
                if used.contains(name) && placed.insert(name) {
                    order.push(name);
                }
            }
        }
        for name in first_tags.iter().flatten() {
            if placed.insert(name) {
                order.push(name);
            }
        }
        let folders: Vec<Folder> = order
            .iter()
            .enumerate()
            .map(|(i, name)| Folder {
                id: new_id(),
                collection_id: collection.id.clone(),
                parent_folder_id: None,
                name: (*name).to_string(),
                sort_order: i as i64,
                created_at: now,
                updated_at: now,
                extra: Default::default(),
            })
            .collect();
        let folder_index: HashMap<&str, usize> =
            order.iter().enumerate().map(|(i, n)| (*n, i)).collect();

        let mut folder_counts = vec![0usize; folders.len()];
        let mut root_count = 0usize;
        let mut path_vars: Vec<String> = Vec::new();
        let mut path_var_seen: HashSet<String> = HashSet::new();
        let mut requests = Vec::with_capacity(ops.len());
        for (op, tag) in ops.iter().zip(first_tags.iter()) {
            let mut req = self.build_request(op, &mut path_vars, &mut path_var_seen);
            req.collection_id = collection.id.clone();
            req.created_at = now;
            req.updated_at = now;
            match tag.and_then(|t| folder_index.get(t)) {
                Some(&i) => {
                    req.folder_id = folders.get(i).map(|f| f.id.clone());
                    if let Some(count) = folder_counts.get_mut(i) {
                        req.sort_order = *count as i64;
                        *count += 1;
                    }
                }
                None => {
                    req.sort_order = (folders.len() + root_count) as i64;
                    root_count += 1;
                }
            }
            requests.push(req);
        }

        let mut variables = vec![Variable::new("baseUrl", base_url.clone())];
        let mut keys: HashSet<String> = HashSet::from(["baseUrl".to_string()]);
        for name in path_vars {
            if keys.insert(name.clone()) {
                variables.push(Variable::new(name, ""));
            }
        }
        for var in auth_vars(&collection_auth) {
            if keys.insert(var.key.clone()) {
                variables.push(var);
            }
        }
        let environment = Environment {
            id: new_id(),
            workspace_id: workspace_id.to_string(),
            project_id: Some(project_id.to_string()),
            name: title.clone(),
            variables,
            sort_order: 0,
            created_at: now,
            updated_at: now,
            extra: Default::default(),
        };

        let report = ImportReport {
            spec,
            collection_name: title,
            operations_created: requests.len(),
            folders: folders
                .iter()
                .zip(folder_counts.iter())
                .map(|(f, &n)| FolderSummary {
                    name: f.name.clone(),
                    request_count: n,
                })
                .collect(),
            root_request_count: root_count,
            base_url,
            warnings: self.warnings,
        };
        OpenApiImport {
            collection,
            folders,
            requests,
            environment,
            report,
        }
    }

    // -- environment / auth -------------------------------------------------

    fn base_url(&mut self) -> String {
        match self.kind {
            SpecKind::OpenApi3 => {
                let server = self
                    .root
                    .get("servers")
                    .and_then(Value::as_array)
                    .and_then(|a| {
                        a.iter().find_map(|s| {
                            let s = s.as_object()?;
                            Some((s.get("url")?.as_str()?, s))
                        })
                    });
                let Some((raw, server)) = server else {
                    self.warn("No servers declared; set baseUrl in the environment".into());
                    return String::new();
                };
                let mut url = raw.trim().to_string();
                if let Some(vars) = server.get("variables").and_then(Value::as_object) {
                    for (name, var) in vars {
                        let value =
                            non_null(var.get("default")).or_else(|| first_of(var.get("enum")));
                        if let Some(value) = value {
                            url = url.replace(&format!("{{{name}}}"), &stringify(value));
                        }
                    }
                }
                let url = url.trim_end_matches('/').to_string();
                if !url.contains("://") {
                    self.warn(format!(
                        "Server URL \"{raw}\" is relative; set baseUrl to an absolute URL in the environment"
                    ));
                }
                url
            }
            SpecKind::Swagger2 => {
                let base_path = self
                    .root
                    .get("basePath")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .unwrap_or("");
                let mut base_path = base_path.trim_end_matches('/').to_string();
                if !base_path.is_empty() && !base_path.starts_with('/') {
                    base_path.insert(0, '/');
                }
                match non_empty_str(self.root.get("host")) {
                    Some(host) => {
                        let scheme = self
                            .root
                            .get("schemes")
                            .and_then(Value::as_array)
                            .and_then(|s| s.first())
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .unwrap_or("https");
                        format!("{scheme}://{}{base_path}", host.trim_end_matches('/'))
                    }
                    None => {
                        self.warn(
                            "Swagger 2.0 spec has no host; baseUrl is only the basePath, set the host in the environment"
                                .into(),
                        );
                        base_path
                    }
                }
            }
        }
    }

    fn collection_auth(&mut self) -> Auth {
        let schemes: Option<&'a Obj> = match self.kind {
            SpecKind::OpenApi3 => self
                .root
                .get("components")
                .and_then(|c| c.get("securitySchemes"))
                .and_then(Value::as_object),
            SpecKind::Swagger2 => self
                .root
                .get("securityDefinitions")
                .and_then(Value::as_object),
        };
        let mut failures: Vec<String> = Vec::new();
        match self.root.get("security") {
            Some(Value::Array(requirements)) => {
                for req in requirements.iter().filter_map(Value::as_object) {
                    for name in req.keys() {
                        match schemes.and_then(|s| s.get(name)) {
                            Some(def) => match self.map_scheme(name, def) {
                                Ok(auth) => return auth,
                                Err(w) => failures.push(w),
                            },
                            None => failures.push(format!(
                                "Security requirement references undefined scheme '{name}'; collection auth set to No Auth"
                            )),
                        }
                    }
                }
            }
            _ => {
                if let Some(schemes) = schemes {
                    if schemes.len() == 1 {
                        if let Some((name, def)) = schemes.iter().next() {
                            match self.map_scheme(name, def) {
                                Ok(auth) => return auth,
                                Err(w) => failures.push(w),
                            }
                        }
                    } else if schemes.len() > 1 {
                        failures.push(
                            "Several security schemes are defined but there is no top-level security requirement; collection auth set to No Auth"
                                .into(),
                        );
                    }
                }
            }
        }
        for f in failures {
            self.warn(f);
        }
        Auth::none()
    }

    fn map_scheme(&mut self, name: &str, def: &'a Value) -> Result<Auth, String> {
        let ctx = format!("security scheme '{name}'");
        let Some(def) = self.deref(def, &ctx).and_then(Value::as_object) else {
            return Err(format!(
                "Security scheme '{name}' could not be resolved; collection auth set to No Auth"
            ));
        };
        let ty = def.get("type").and_then(Value::as_str).unwrap_or("");
        match ty {
            "http" => {
                let scheme = def
                    .get("scheme")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                match scheme.as_str() {
                    "bearer" => Ok(Auth::bearer("{{token}}")),
                    "basic" => Ok(Auth::basic("{{username}}", "{{password}}")),
                    other => Err(format!(
                        "HTTP '{other}' security scheme '{name}' is not supported; collection auth set to No Auth"
                    )),
                }
            }
            "basic" => Ok(Auth::basic("{{username}}", "{{password}}")),
            "apiKey" => {
                let Some(key) = non_empty_str(def.get("name")) else {
                    return Err(format!(
                        "API key security scheme '{name}' has no parameter name; collection auth set to No Auth"
                    ));
                };
                let location = match def.get("in").and_then(Value::as_str).unwrap_or("") {
                    "header" => ApiKeyLocation::Header,
                    "query" => ApiKeyLocation::Query,
                    "cookie" => {
                        return Err(format!(
                            "API key security scheme '{name}' is sent in a cookie, which is not supported; collection auth set to No Auth"
                        ));
                    }
                    other => {
                        return Err(format!(
                            "API key security scheme '{name}' has unsupported location '{other}'; collection auth set to No Auth"
                        ));
                    }
                };
                Ok(Auth::api_key(key, "{{apiKey}}", location))
            }
            "oauth2" => Err(format!(
                "OAuth2 security scheme '{name}' is not supported; collection auth set to No Auth (configure auth manually)"
            )),
            "openIdConnect" => Err(format!(
                "OpenID Connect security scheme '{name}' is not supported; collection auth set to No Auth (configure auth manually)"
            )),
            other => Err(format!(
                "Security scheme '{name}' of type '{other}' is not supported; collection auth set to No Auth"
            )),
        }
    }

    // -- operations -----------------------------------------------------------

    fn collect_operations(&mut self) -> Vec<Operation<'a>> {
        let mut ops = Vec::new();
        let Some(paths) = self.root.get("paths").and_then(Value::as_object) else {
            return ops;
        };
        let oas3 = self.kind == SpecKind::OpenApi3;
        for (path, item) in paths {
            if path.starts_with("x-") {
                continue;
            }
            let Some(item) = self.deref(item, &format!("path {path}")) else {
                continue;
            };
            let Some(item) = item.as_object() else {
                self.warn(format!(
                    "Skipped path {path}: the path item is not a mapping"
                ));
                continue;
            };
            for (key, val) in item {
                if HTTP_METHODS.contains(&key.as_str()) || (oas3 && key == "query") {
                    self.push_op(&mut ops, path, key.to_ascii_uppercase(), item, val);
                } else if oas3 && key == "additionalOperations" {
                    let Some(extra) = val.as_object() else {
                        continue;
                    };
                    for (name, op) in extra {
                        let method = name.trim().to_ascii_uppercase();
                        if !is_valid_method(&method) {
                            self.warn(format!(
                                "Skipped operation with invalid method name \"{name}\" at {path}"
                            ));
                            continue;
                        }
                        self.push_op(&mut ops, path, method, item, op);
                    }
                }
            }
        }
        ops
    }

    fn push_op(
        &mut self,
        ops: &mut Vec<Operation<'a>>,
        path: &'a str,
        method: String,
        item: &'a Obj,
        op: &'a Value,
    ) {
        match op.as_object() {
            Some(op) => ops.push(Operation {
                path,
                method,
                item,
                op,
            }),
            None => self.warn(format!(
                "Skipped {method} {path}: the operation is not a mapping"
            )),
        }
    }

    fn build_request(
        &mut self,
        op: &Operation<'a>,
        path_vars: &mut Vec<String>,
        path_var_seen: &mut HashSet<String>,
    ) -> Request {
        let ctx = format!("{} {}", op.method, op.path);
        let name = non_empty_str(op.op.get("summary"))
            .or_else(|| non_empty_str(op.op.get("operationId")))
            .map(str::to_string)
            .unwrap_or_else(|| ctx.clone());
        let path = template_path(op.path, path_vars, path_var_seen);

        let all_params = self.collect_params(op.item, op.op, &ctx);
        let mut params = Vec::new();
        let mut headers = Vec::new();
        for p in &all_params {
            let pname = p.get("name").and_then(Value::as_str).unwrap_or("");
            match p.get("in").and_then(Value::as_str).unwrap_or("") {
                "query" => params.push(self.param_row(p, pname, &ctx)),
                "header" => {
                    if self.kind == SpecKind::OpenApi3
                        && ["accept", "content-type", "authorization"]
                            .contains(&pname.to_ascii_lowercase().as_str())
                    {
                        continue;
                    }
                    headers.push(self.param_row(p, pname, &ctx));
                }
                _ => {}
            }
        }

        let body = match self.kind {
            SpecKind::OpenApi3 => self.body_oas3(op.op, &ctx),
            SpecKind::Swagger2 => self.body_swagger2(op.op, &all_params, &ctx),
        };
        let (body, content_type) = body.unwrap_or((Body::none(), None));
        if let Some(ct) = content_type
            && !headers
                .iter()
                .any(|h| h.key.eq_ignore_ascii_case("content-type"))
        {
            headers.push(KeyValue::new("Content-Type", ct));
        }

        let url = params_to_url(&format!("{{{{baseUrl}}}}{path}"), &params);
        let auth = match op.op.get("security") {
            Some(Value::Array(a)) if a.is_empty() => Auth::none(),
            _ => Auth::inherit(),
        };
        Request {
            id: new_id(),
            collection_id: String::new(),
            folder_id: None,
            name,
            method: op.method.clone(),
            url,
            params,
            headers,
            auth,
            body,
            settings: RequestSettings::default(),
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            kind: Default::default(),
            min_client_version: None,
            extra: Default::default(),
        }
    }

    // -- parameters -------------------------------------------------------------

    /// Path-level then operation-level parameters, resolved, later ones
    /// replacing earlier ones with the same `name` + `in`.
    fn collect_params(&mut self, item: &'a Obj, op: &'a Obj, ctx: &str) -> Vec<&'a Obj> {
        let mut out: Vec<&'a Obj> = Vec::new();
        for list in [item.get("parameters"), op.get("parameters")] {
            let Some(list) = list.and_then(Value::as_array) else {
                continue;
            };
            for p in list {
                let Some(p) = self.deref(p, ctx).and_then(Value::as_object) else {
                    continue;
                };
                let (Some(name), Some(loc)) = (
                    p.get("name").and_then(Value::as_str),
                    p.get("in").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let same = |e: &&'a Obj| {
                    e.get("name").and_then(Value::as_str) == Some(name)
                        && e.get("in").and_then(Value::as_str) == Some(loc)
                };
                match out.iter_mut().find(|e| same(e)) {
                    Some(slot) => *slot = p,
                    None => out.push(p),
                }
            }
        }
        out
    }

    fn param_row(&mut self, p: &'a Obj, name: &str, ctx: &str) -> KeyValue {
        let required = p.get("required").and_then(Value::as_bool).unwrap_or(false);
        KeyValue {
            key: name.to_string(),
            value: self.param_value(p, ctx),
            enabled: required,
            description: non_empty_str(p.get("description")).map(str::to_string),
            extra: Default::default(),
        }
    }

    fn param_value(&mut self, p: &'a Obj, ctx: &str) -> String {
        let content_media = p
            .get("content")
            .and_then(Value::as_object)
            .and_then(|c| c.values().next())
            .and_then(Value::as_object);
        let schema = p
            .get("schema")
            .or_else(|| content_media.and_then(|m| m.get("schema")));
        let schema = match schema {
            Some(s) => self.deref(s, ctx).and_then(Value::as_object),
            None => None,
        };
        let named_example = match p
            .get("examples")
            .and_then(Value::as_object)
            .and_then(|m| m.values().next())
        {
            Some(ex) => non_null(self.deref(ex, ctx).and_then(|e| e.get("value"))),
            None => None,
        };
        let candidates = [
            non_null(p.get("example")),
            non_null(p.get("x-example")),
            named_example,
            content_media.and_then(|m| non_null(m.get("example"))),
            schema.and_then(|s| non_null(s.get("example"))),
            schema.and_then(|s| first_of(s.get("examples"))),
            non_null(p.get("default")),
            schema.and_then(|s| non_null(s.get("default"))),
            first_of(p.get("enum")),
            schema.and_then(|s| first_of(s.get("enum"))),
            schema.and_then(|s| non_null(s.get("const"))),
            p.get("items").and_then(|i| non_null(i.get("default"))),
            p.get("items").and_then(|i| first_of(i.get("enum"))),
            schema
                .and_then(|s| s.get("items"))
                .and_then(|i| first_of(i.get("enum"))),
        ];
        candidates
            .into_iter()
            .flatten()
            .next()
            .map(stringify)
            .unwrap_or_default()
    }

    // -- bodies -----------------------------------------------------------------

    fn body_oas3(&mut self, op: &'a Obj, ctx: &str) -> BodyResult {
        let rb = op.get("requestBody")?;
        let rb = self.deref(rb, ctx)?.as_object()?;
        let content = rb.get("content")?.as_object()?;
        let (media_type, media) = content
            .iter()
            .find(|(k, _)| essence(k) == "application/json")
            .or_else(|| content.iter().find(|(k, _)| is_json_media(&essence(k))))
            .or_else(|| content.iter().next())?;
        let media = media.as_object();
        let example = match media {
            Some(m) => self.media_example(m, ctx),
            None => None,
        };
        let schema = media.and_then(|m| m.get("schema"));
        self.build_body(media_type, example, schema, ctx)
    }

    fn media_example(&mut self, media: &'a Obj, ctx: &str) -> Option<&'a Value> {
        if let Some(ex) = non_null(media.get("example")) {
            return Some(ex);
        }
        let first = media.get("examples")?.as_object()?.values().next()?;
        non_null(self.deref(first, ctx)?.get("value"))
    }

    fn body_swagger2(&mut self, op: &'a Obj, params: &[&'a Obj], ctx: &str) -> BodyResult {
        let consumes: Vec<&str> = op
            .get("consumes")
            .or_else(|| self.root.get("consumes"))
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let location = |p: &&'a Obj| p.get("in").and_then(Value::as_str).unwrap_or("");
        if let Some(bp) = params.iter().find(|p| location(p) == "body") {
            let xml_only =
                !consumes.is_empty() && consumes.iter().all(|c| is_xml_media(&essence(c)));
            let media_type = if xml_only {
                consumes.first().copied().unwrap_or("application/xml")
            } else {
                "application/json"
            };
            return self.build_body(media_type, None, bp.get("schema"), ctx);
        }
        let form: Vec<&'a Obj> = params
            .iter()
            .filter(|p| location(p) == "formData")
            .copied()
            .collect();
        if form.is_empty() {
            return None;
        }
        let is_file = |p: &&'a Obj| p.get("type").and_then(Value::as_str) == Some("file");
        let multipart = consumes
            .iter()
            .any(|c| essence(c).starts_with("multipart/"))
            || form.iter().any(is_file);
        if multipart {
            let mut fields = Vec::with_capacity(form.len());
            for p in &form {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("");
                fields.push(if is_file(p) {
                    FormField::file(name, "")
                } else {
                    FormField::text(name, self.param_value(p, ctx))
                });
            }
            Some((Body::form(fields), None))
        } else {
            let mut rows = Vec::with_capacity(form.len());
            for p in &form {
                let name = p.get("name").and_then(Value::as_str).unwrap_or("");
                rows.push(KeyValue::new(name, self.param_value(p, ctx)));
            }
            Some((Body::form_encoded(rows), None))
        }
    }

    fn build_body(
        &mut self,
        media_type: &str,
        example: Option<&'a Value>,
        schema: Option<&'a Value>,
        ctx: &str,
    ) -> BodyResult {
        let ess = essence(media_type);
        match classify_media(&ess) {
            MediaKind::Multipart => {
                let fields = self
                    .form_properties(schema, ctx)?
                    .into_iter()
                    .map(|(key, value, file)| {
                        if file {
                            FormField::file(key, "")
                        } else {
                            FormField::text(key, value)
                        }
                    })
                    .collect();
                Some((Body::form(fields), None))
            }
            MediaKind::UrlEncoded => {
                let rows = self
                    .form_properties(schema, ctx)?
                    .into_iter()
                    .map(|(key, value, _)| KeyValue::new(key, value))
                    .collect();
                Some((Body::form_encoded(rows), None))
            }
            MediaKind::Json => {
                let text = match (example, schema) {
                    (Some(ex), _) => render_json_example(ex),
                    (None, Some(schema)) => {
                        self.budget = SKELETON_BUDGET;
                        let value = self.skeleton(schema, 0, &mut Vec::new(), ctx)?;
                        pretty(&value)
                    }
                    (None, None) => String::new(),
                };
                let header = content_type_header(BodyType::Json, media_type, &ess);
                Some((Body::json(text), header))
            }
            MediaKind::Xml => {
                let text = example.and_then(Value::as_str).unwrap_or("");
                let header = content_type_header(BodyType::Xml, media_type, &ess);
                Some((Body::text_of(BodyType::Xml, text), header))
            }
            MediaKind::Text => {
                let literal = match (example, schema) {
                    (Some(ex), _) => Some(ex),
                    (None, Some(schema)) => self
                        .deref(schema, ctx)
                        .and_then(Value::as_object)
                        .and_then(literal_example),
                    (None, None) => None,
                };
                let text = literal.map(stringify).unwrap_or_default();
                let header = content_type_header(BodyType::Text, media_type, &ess);
                Some((Body::text_of(BodyType::Text, text), header))
            }
            MediaKind::Binary => {
                let header = content_type_header(BodyType::Binary, media_type, &ess);
                Some((Body::binary(""), header))
            }
        }
    }

    /// `(name, value, is_file)` for each property of a form schema. `None`
    /// when the schema itself had to be skipped (external / broken ref).
    fn form_properties(
        &mut self,
        schema: Option<&'a Value>,
        ctx: &str,
    ) -> Option<Vec<(String, String, bool)>> {
        let Some(schema) = schema else {
            return Some(Vec::new());
        };
        let root = self.deref(schema, ctx)?;
        let mut props: Vec<(&'a str, &'a Value)> = Vec::new();
        self.gather_properties(root, ctx, 0, &mut props);
        let mut seen: HashSet<&str> = HashSet::new();
        let mut out = Vec::with_capacity(props.len());
        for (name, prop) in props {
            if !seen.insert(name) {
                continue;
            }
            let Some(prop) = self.deref(prop, ctx) else {
                continue;
            };
            let prop = prop.as_object();
            let file = prop.is_some_and(|p| self.is_file_schema(p, ctx, true));
            let value = if file {
                String::new()
            } else {
                prop.and_then(literal_example)
                    .map(stringify)
                    .unwrap_or_default()
            };
            out.push((name.to_string(), value, file));
        }
        Some(out)
    }

    fn gather_properties(
        &mut self,
        schema: &'a Value,
        ctx: &str,
        depth: usize,
        out: &mut Vec<(&'a str, &'a Value)>,
    ) {
        if depth > 8 {
            return;
        }
        let Some(o) = schema.as_object() else {
            return;
        };
        if let Some(all) = o.get("allOf").and_then(Value::as_array) {
            for sub in all {
                if let Some(sub) = self.deref(sub, ctx) {
                    self.gather_properties(sub, ctx, depth + 1, out);
                }
            }
        }
        for key in ["oneOf", "anyOf"] {
            if let Some(first) = o.get(key).and_then(Value::as_array).and_then(|a| a.first())
                && let Some(sub) = self.deref(first, ctx)
            {
                self.gather_properties(sub, ctx, depth + 1, out);
            }
        }
        if let Some(props) = o.get("properties").and_then(Value::as_object) {
            out.extend(props.iter().map(|(k, v)| (k.as_str(), v)));
        }
    }

    fn is_file_schema(&mut self, o: &'a Obj, ctx: &str, allow_array: bool) -> bool {
        match schema_type(o) {
            Some("file") => true,
            Some("string") => {
                matches!(
                    o.get("format").and_then(Value::as_str),
                    Some("binary" | "base64")
                ) || o.get("contentEncoding").and_then(Value::as_str) == Some("base64")
            }
            Some("array") if allow_array => {
                let items = o.get("items");
                match items
                    .and_then(|i| self.deref(i, ctx))
                    .and_then(Value::as_object)
                {
                    Some(items) => self.is_file_schema(items, ctx, false),
                    None => false,
                }
            }
            _ => false,
        }
    }

    // -- schema skeletons -------------------------------------------------------

    /// Example JSON value for a schema. `None` when the schema must be skipped
    /// (external or unresolvable `$ref`; a warning has been recorded).
    fn skeleton(
        &mut self,
        schema: &'a Value,
        depth: usize,
        stack: &mut Vec<&'a str>,
        ctx: &str,
    ) -> Option<Value> {
        if self.budget == 0 {
            self.cycle_cuts += 1;
            return Some(Value::Null);
        }
        self.budget -= 1;
        let Some(o) = schema.as_object() else {
            return Some(Value::Null);
        };
        if let Some(literal) = literal_example(o) {
            return Some(literal.clone());
        }

        if let Some(r) = o.get("$ref").and_then(Value::as_str) {
            if stack.contains(&r) || stack.len() >= MAX_REF_STACK {
                self.cycle_cuts += 1;
                return Some(Value::Object(Map::new()));
            }
            if let Some(cached) = self.skel_cache.get(&(r, depth)) {
                return Some(cached.clone());
            }
            let target = match self.lookup(r, ctx) {
                Ok(t) => t,
                Err(msg) => {
                    self.warn(msg);
                    return None;
                }
            };
            let (warns_before, cuts_before) = (self.warn_attempts, self.cycle_cuts);
            stack.push(r);
            let out = self.skeleton(target, depth, stack, ctx);
            stack.pop();
            if let Some(v) = &out
                && warns_before == self.warn_attempts
                && cuts_before == self.cycle_cuts
            {
                self.skel_cache.insert((r, depth), v.clone());
            }
            return out;
        }

        if let Some(all) = o.get("allOf").and_then(Value::as_array) {
            let mut merged = Map::new();
            let mut scalar = None;
            for sub in all {
                match self.skeleton(sub, depth, stack, ctx) {
                    Some(Value::Object(m)) => merged.extend(m),
                    Some(v) if scalar.is_none() && !v.is_null() => scalar = Some(v),
                    _ => {}
                }
            }
            if o.contains_key("properties")
                && let Value::Object(own) = self.object_skeleton(o, depth, stack, ctx)
            {
                merged.extend(own);
            }
            return Some(match scalar {
                Some(v) if merged.is_empty() => v,
                _ => Value::Object(merged),
            });
        }

        for key in ["oneOf", "anyOf"] {
            let Some(branches) = o.get(key).and_then(Value::as_array) else {
                continue;
            };
            let non_null_branches = branches
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) != Some("null"));
            for branch in non_null_branches {
                if let Some(v) = self.skeleton(branch, depth, stack, ctx) {
                    if let Value::Object(mut m) = v {
                        if o.contains_key("properties")
                            && let Value::Object(own) = self.object_skeleton(o, depth, stack, ctx)
                        {
                            m.extend(own);
                        }
                        return Some(Value::Object(m));
                    }
                    return Some(v);
                }
            }
        }

        Some(match schema_type(o) {
            Some("object") => self.object_skeleton(o, depth, stack, ctx),
            Some("array") => {
                if depth >= MAX_DEPTH {
                    Value::Array(Vec::new())
                } else {
                    let items = match o.get("items") {
                        Some(Value::Array(tuple)) => tuple.first(),
                        Some(other) => Some(other),
                        None => o
                            .get("prefixItems")
                            .and_then(Value::as_array)
                            .and_then(|a| a.first()),
                    };
                    match items.and_then(|i| self.skeleton(i, depth + 1, stack, ctx)) {
                        Some(v) => Value::Array(vec![v]),
                        None => Value::Array(Vec::new()),
                    }
                }
            }
            Some("string" | "file") => Value::String(String::new()),
            Some("integer" | "number") => Value::from(0),
            Some("boolean") => Value::Bool(false),
            Some("null") => Value::Null,
            _ => Value::Object(Map::new()),
        })
    }

    fn object_skeleton(
        &mut self,
        o: &'a Obj,
        depth: usize,
        stack: &mut Vec<&'a str>,
        ctx: &str,
    ) -> Value {
        let mut out = Map::new();
        if depth >= MAX_DEPTH {
            return Value::Object(out);
        }
        if let Some(props) = o.get("properties").and_then(Value::as_object) {
            for (name, prop) in props {
                if let Some(v) = self.skeleton(prop, depth + 1, stack, ctx) {
                    out.insert(name.clone(), v);
                }
            }
        }
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{AuthType, FormFieldKind};
    use proptest::prelude::*;
    use serde_json::json;

    const PETSTORE: &str = include_str!("../tests/fixtures/openapi/swagger2_petstore.json");
    const BOOKSHOP: &str = include_str!("../tests/fixtures/openapi/openapi30_bookshop.yaml");
    const ORGCHART: &str = include_str!("../tests/fixtures/openapi/openapi31_orgchart.json");
    const SEARCH: &str = include_str!("../tests/fixtures/openapi/openapi32_search.yaml");

    fn import(text: &str) -> OpenApiImport {
        match import_openapi(text, "ws1", "proj1") {
            Ok(i) => i,
            Err(e) => panic!("import failed: {e}"),
        }
    }

    fn by_name<'i>(imp: &'i OpenApiImport, name: &str) -> &'i Request {
        imp.requests
            .iter()
            .find(|r| r.name == name)
            .unwrap_or_else(|| panic!("no request named {name:?}"))
    }

    fn header<'r>(r: &'r Request, key: &str) -> Option<&'r KeyValue> {
        r.headers.iter().find(|h| h.key.eq_ignore_ascii_case(key))
    }

    fn body_json(r: &Request) -> Value {
        assert_eq!(r.body.body_type, BodyType::Json, "{}", r.name);
        serde_json::from_str(r.body.text_str()).expect("body is JSON")
    }

    fn var_keys(imp: &OpenApiImport) -> Vec<&str> {
        imp.environment
            .variables
            .iter()
            .map(|v| v.key.as_str())
            .collect()
    }

    fn var<'i>(imp: &'i OpenApiImport, key: &str) -> &'i Variable {
        imp.environment
            .variables
            .iter()
            .find(|v| v.key == key)
            .unwrap_or_else(|| panic!("no variable {key}"))
    }

    fn has_warning(imp: &OpenApiImport, needle: &str) -> bool {
        imp.report.warnings.iter().any(|w| w.contains(needle))
    }

    fn oas3(paths: Value, extra: Value) -> String {
        let mut doc = json!({
            "openapi": "3.0.3",
            "info": {"title": "T", "version": "1"},
            "servers": [{"url": "https://api.test"}],
            "paths": paths,
        });
        if let (Some(d), Some(e)) = (doc.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                d.insert(k.clone(), v.clone());
            }
        }
        doc.to_string()
    }

    // -- AC-OAS-1 ---------------------------------------------------------------

    #[test]
    fn ac_oas_1_detects_format_and_version() {
        let cases = [
            (
                PETSTORE,
                SpecFormat::Json,
                SpecKind::Swagger2,
                "2.0",
                "Swagger 2.0 · JSON",
            ),
            (
                BOOKSHOP,
                SpecFormat::Yaml,
                SpecKind::OpenApi3,
                "3.0.3",
                "OpenAPI 3.0 · YAML",
            ),
            (
                ORGCHART,
                SpecFormat::Json,
                SpecKind::OpenApi3,
                "3.1.0",
                "OpenAPI 3.1 · JSON",
            ),
            (
                SEARCH,
                SpecFormat::Yaml,
                SpecKind::OpenApi3,
                "3.2.0",
                "OpenAPI 3.2 · YAML",
            ),
            (
                "swagger: 2.0\n",
                SpecFormat::Yaml,
                SpecKind::Swagger2,
                "2.0",
                "Swagger 2.0 · YAML",
            ),
            (
                "openapi: 3.1\n",
                SpecFormat::Yaml,
                SpecKind::OpenApi3,
                "3.1",
                "OpenAPI 3.1 · YAML",
            ),
            (
                "{openapi: 3.0.1, info: {title: Flow}}",
                SpecFormat::Yaml,
                SpecKind::OpenApi3,
                "3.0.1",
                "OpenAPI 3.0 · YAML",
            ),
            (
                "\u{feff}  {\"openapi\": \"3.2.0\"}  ",
                SpecFormat::Json,
                SpecKind::OpenApi3,
                "3.2.0",
                "OpenAPI 3.2 · JSON",
            ),
        ];
        for (text, format, kind, version, label) in cases {
            let info = detect_spec(text).expect("detect");
            assert_eq!(
                info,
                SpecInfo {
                    format,
                    kind,
                    version: version.into(),
                    label: label.into()
                }
            );
        }
        let v = serde_json::to_value(detect_spec(BOOKSHOP).expect("detect")).expect("ser");
        assert_eq!(v["format"], "yaml");
        assert_eq!(v["kind"], "openapi3");
        assert_eq!(
            serde_json::to_value(detect_spec(PETSTORE).expect("detect")).expect("ser")["kind"],
            "swagger2"
        );
    }

    #[test]
    fn ac_oas_1_rejects_unsupported_and_unparsable() {
        for text in [
            "openapi: 4.0.0\npaths: {}",
            "openapi: 3.10.0",
            "openapi: \"3\"",
            "openapi: 2.0",
            "swagger: \"1.2\"",
            "{\"swaggerVersion\": \"1.2\", \"apis\": []}",
            "title: just some yaml\nitems: [1, 2]",
            "42",
            "hello",
            "[]",
            "- openapi: 3.0.0",
            "null",
        ] {
            assert!(
                matches!(detect_spec(text), Err(OpenApiError::Unsupported(_))),
                "{text:?} => {:?}",
                detect_spec(text)
            );
            assert!(matches!(
                import_openapi(text, "w", "p"),
                Err(OpenApiError::Unsupported(_))
            ));
        }
        for text in [
            "",
            "   \n",
            "{ \"openapi\": ",
            "key: [unclosed",
            "a: b\n- c",
            "--- 1\n--- 2",
        ] {
            assert!(
                matches!(import_openapi(text, "w", "p"), Err(OpenApiError::Parse(_))),
                "{text:?} => {:?}",
                import_openapi(text, "w", "p").map(|_| ())
            );
        }
        let err = import_openapi("openapi: 4.0.0", "w", "p").expect_err("unsupported");
        assert!(
            err.to_string()
                .starts_with("Unsupported format: OpenAPI version \"4.0.0\"")
        );
    }

    #[test]
    fn ac_oas_1_minimal_and_invalid_documents() {
        let imp = import("openapi: 3.0.0\n");
        assert_eq!(imp.requests.len(), 0);
        assert_eq!(imp.collection.name, "Imported API");
        assert_eq!(imp.report.operations_created, 0);
        assert!(imp.folders.is_empty());
        assert_eq!(var_keys(&imp), vec!["baseUrl"]);
        assert!(has_warning(&imp, "No servers declared"));

        let imp = import("swagger: '2.0'\ninfo: {title: Tiny}\npaths: ~\n");
        assert_eq!(imp.requests.len(), 0);
        assert_eq!(imp.collection.name, "Tiny");

        for text in [
            "openapi: 3.0.0\npaths: []",
            "openapi: 3.0.0\npaths: nope",
            "openapi: 3.1.0\ninfo: [a]\npaths: {}",
        ] {
            assert!(
                matches!(
                    import_openapi(text, "w", "p"),
                    Err(OpenApiError::Invalid(_))
                ),
                "{text:?}"
            );
        }

        // Garbage inside an otherwise valid document is skipped, not fatal.
        let imp = import(
            r#"{"openapi":"3.0.0","paths":{"/a":42,"/b":{"get":"nope","post":{}},"x-ext":{"get":{}}}}"#,
        );
        assert_eq!(imp.requests.len(), 1);
        assert_eq!(imp.requests[0].name, "POST /b");
        assert!(has_warning(&imp, "Skipped path /a"));
        assert!(has_warning(&imp, "Skipped GET /b"));
    }

    // -- AC-OAS-2 ---------------------------------------------------------------

    #[test]
    fn ac_oas_2_collection_folders_and_root_requests() {
        let imp = import(PETSTORE);
        assert_eq!(imp.collection.name, "Swagger Petstore");
        assert_eq!(imp.collection.project_id, "proj1");
        assert_eq!(imp.collection.description, "A sample pet store server.");
        let names: Vec<_> = imp.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["pet", "store", "user"]);
        let counts: Vec<_> = imp
            .report
            .folders
            .iter()
            .map(|f| (f.name.as_str(), f.request_count))
            .collect();
        assert_eq!(counts, vec![("pet", 7), ("store", 3), ("user", 2)]);
        assert_eq!(imp.report.root_request_count, 1);
        assert_eq!(imp.report.operations_created, 13);
        assert_eq!(imp.requests.len(), 13);

        for (i, f) in imp.folders.iter().enumerate() {
            assert_eq!(f.sort_order, i as i64);
            assert_eq!(f.collection_id, imp.collection.id);
            assert_eq!(f.parent_folder_id, None);
            let orders: Vec<i64> = imp
                .requests
                .iter()
                .filter(|r| r.folder_id.as_deref() == Some(&f.id))
                .map(|r| r.sort_order)
                .collect();
            assert_eq!(orders, (0..orders.len() as i64).collect::<Vec<_>>());
        }
        let health = by_name(&imp, "Health check");
        assert_eq!(health.folder_id, None);
        assert_eq!(health.sort_order, 3, "root requests follow the folders");

        // Request order follows the document; all ids unique ULIDs.
        let first: Vec<_> = imp
            .requests
            .iter()
            .take(3)
            .map(|r| r.name.as_str())
            .collect();
        assert_eq!(
            first,
            vec![
                "Add a new pet to the store",
                "Update an existing pet",
                "Finds Pets by status"
            ]
        );
        let mut ids: HashSet<&str> = HashSet::new();
        ids.insert(&imp.collection.id);
        ids.insert(&imp.environment.id);
        ids.extend(imp.folders.iter().map(|f| f.id.as_str()));
        ids.extend(imp.requests.iter().map(|r| r.id.as_str()));
        assert_eq!(ids.len(), 2 + imp.folders.len() + imp.requests.len());
        assert!(ids.iter().all(|id| ulid::Ulid::from_string(id).is_ok()));
        let now = imp.collection.created_at;
        assert!(now > 0);
        for r in &imp.requests {
            assert_eq!(r.collection_id, imp.collection.id);
            assert_eq!((r.created_at, r.updated_at), (now, now));
            assert_eq!(r.settings, RequestSettings::default());
        }
    }

    #[test]
    fn ac_oas_2_folder_order_declared_tags_first_then_first_seen() {
        // Bookshop declares [books, authors, unused] but uses authors first.
        let imp = import(BOOKSHOP);
        let names: Vec<_> = imp.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["books", "authors"]);
        // Only the first tag counts ("featured" gets no folder).
        let add = by_name(&imp, "Add a book");
        assert_eq!(add.folder_id.as_deref(), Some(imp.folders[0].id.as_str()));

        // Without declared tags: first-seen order.
        let imp = import(&oas3(
            json!({
                "/a": {"get": {"tags": ["zeta"]}, "post": {"tags": ["alpha", "zeta"]}},
                "/b": {"get": {"tags": ["zeta"]}, "put": {}},
                "/c": {"get": {"tags": [""]}}
            }),
            json!({"tags": [{"name": "alpha"}]}),
        ));
        let names: Vec<_> = imp.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "zeta"]);
        assert_eq!(imp.report.root_request_count, 2);
        let root_orders: Vec<i64> = imp
            .requests
            .iter()
            .filter(|r| r.folder_id.is_none())
            .map(|r| r.sort_order)
            .collect();
        assert_eq!(root_orders, vec![2, 3]);
    }

    // -- AC-OAS-3 ---------------------------------------------------------------

    #[test]
    fn ac_oas_3_request_names() {
        let imp = import(PETSTORE);
        // summary wins over operationId
        assert!(imp.requests.iter().any(|r| r.name == "Find pet by ID"));
        // operationId when no summary
        assert_eq!(by_name(&imp, "placeOrder").method, "POST");
        // METHOD /path when neither
        assert_eq!(by_name(&imp, "GET /store/order/{orderId}").method, "GET");

        let imp = import(&oas3(
            json!({"/x": {"get": {"summary": "  ", "operationId": "getX"}, "delete": {"summary": 5}}}),
            json!({}),
        ));
        assert_eq!(imp.requests[0].name, "getX");
        assert_eq!(imp.requests[1].name, "DELETE /x");
    }

    #[test]
    fn ac_oas_3_urls_use_base_url_and_path_variables() {
        let imp = import(PETSTORE);
        assert_eq!(
            by_name(&imp, "Find pet by ID").url,
            "{{baseUrl}}/pet/{{petId}}"
        );
        assert_eq!(
            by_name(&imp, "uploads an image").url,
            "{{baseUrl}}/pet/{{petId}}/uploadImage"
        );
        assert_eq!(
            by_name(&imp, "Finds Pets by status").url,
            "{{baseUrl}}/pet/findByStatus?status=available"
        );
        let imp = import(&oas3(
            json!({"/a/{x}/b/{y}.json": {"get": {}}, "/odd/{/{}": {"get": {}}}),
            json!({}),
        ));
        assert_eq!(imp.requests[0].url, "{{baseUrl}}/a/{{x}}/b/{{y}}.json");
        assert_eq!(imp.requests[1].url, "{{baseUrl}}/odd/{/{}");
    }

    // -- AC-OAS-4 ---------------------------------------------------------------

    #[test]
    fn ac_oas_4_environment_swagger2() {
        let imp = import(PETSTORE);
        let env = &imp.environment;
        assert_eq!(env.name, "Swagger Petstore");
        assert_eq!(env.workspace_id, "ws1");
        assert_eq!(env.project_id.as_deref(), Some("proj1"));
        assert_eq!(var(&imp, "baseUrl").value, "https://petstore.swagger.io/v2");
        assert_eq!(imp.report.base_url, "https://petstore.swagger.io/v2");
        assert_eq!(
            var_keys(&imp),
            vec!["baseUrl", "petId", "orderId", "apiKey"]
        );
        assert!(env.variables.iter().all(|v| v.enabled));
        assert!(var(&imp, "apiKey").secret);
        assert!(!var(&imp, "petId").secret);
        assert_eq!(var(&imp, "petId").value, "");

        // Scheme default, missing host.
        let imp = import(r#"{"swagger":"2.0","host":"h.test:8080","basePath":"api/","paths":{}}"#);
        assert_eq!(imp.report.base_url, "https://h.test:8080/api");
        let imp = import(r#"{"swagger":"2.0","schemes":["http"],"basePath":"/v1","paths":{}}"#);
        assert_eq!(imp.report.base_url, "/v1");
        assert!(has_warning(&imp, "no host"));
    }

    #[test]
    fn ac_oas_4_environment_openapi3_servers() {
        let imp = import(BOOKSHOP);
        assert_eq!(imp.environment.name, "Bookshop API");
        assert_eq!(
            var(&imp, "baseUrl").value,
            "https://api.bookshop.example.com/v1"
        );
        assert_eq!(
            var_keys(&imp),
            vec!["baseUrl", "authorId", "bookId", "token"]
        );
        assert!(var(&imp, "token").secret);
        assert!(imp.report.warnings.is_empty(), "{:?}", imp.report.warnings);

        // Relative server URL kept as-is (minus trailing slash) with a warning.
        let imp = import(ORGCHART);
        assert_eq!(imp.report.base_url, "/api/v2");
        assert!(has_warning(&imp, "Server URL \"/api/v2/\" is relative"));

        // No servers: empty baseUrl + warning. A path param named baseUrl is not duplicated.
        let imp = import(r#"{"openapi":"3.0.0","paths":{"/{baseUrl}/{id}/{id}":{"get":{}}}}"#);
        assert_eq!(imp.report.base_url, "");
        assert_eq!(var_keys(&imp), vec!["baseUrl", "id"]);
        assert!(has_warning(&imp, "No servers declared"));
    }

    // -- AC-OAS-5 ---------------------------------------------------------------

    #[test]
    fn ac_oas_5_query_and_header_params() {
        let imp = import(BOOKSHOP);
        let list = by_name(&imp, "List books");
        let rows: Vec<_> = list
            .params
            .iter()
            .map(|p| (p.key.as_str(), p.value.as_str(), p.enabled))
            .collect();
        assert_eq!(
            rows,
            vec![("limit", "20", false), ("genre", "fantasy", false)]
        );
        assert_eq!(
            list.params[1].description.as_deref(),
            Some("Filter by genre")
        );
        assert_eq!(
            list.url, "{{baseUrl}}/books",
            "disabled params are not in the URL"
        );
        // Accept / Content-Type / Authorization header params are ignored in 3.x.
        let headers: Vec<_> = list
            .headers
            .iter()
            .map(|h| (h.key.as_str(), h.value.as_str(), h.enabled))
            .collect();
        assert_eq!(headers, vec![("X-Request-Id", "7f9c-req", true)]);

        let imp = import(PETSTORE);
        let find = by_name(&imp, "Finds Pets by status");
        let rows: Vec<_> = find
            .params
            .iter()
            .map(|p| (p.key.as_str(), p.value.as_str(), p.enabled))
            .collect();
        assert_eq!(
            rows,
            vec![("status", "available", true), ("limit", "20", false)]
        );
        let login = by_name(&imp, "Logs user into the system");
        assert_eq!(login.url, "{{baseUrl}}/user/login?username&password");
        let del = by_name(&imp, "Deletes a pet");
        assert_eq!(del.headers, vec![KeyValue::new("api_key", "").disabled()]);
    }

    #[test]
    fn ac_oas_5_value_precedence_and_stringification() {
        let p = |extra: Value| {
            let mut param = json!({"name": "q", "in": "query", "required": true});
            if let (Some(o), Some(e)) = (param.as_object_mut(), extra.as_object()) {
                o.extend(e.clone());
            }
            let imp = import(&oas3(
                json!({"/s": {"get": {"parameters": [param]}}}),
                json!({}),
            ));
            imp.requests[0].params[0].value.clone()
        };
        let schema = json!({"example": "se", "default": "sd", "enum": ["e1"]});
        assert_eq!(
            p(json!({"example": "pe", "default": "pd", "schema": schema})),
            "pe"
        );
        assert_eq!(p(json!({"default": "pd", "schema": schema})), "se");
        assert_eq!(
            p(json!({"default": "pd", "schema": {"default": "sd"}})),
            "pd"
        );
        assert_eq!(
            p(json!({"schema": {"default": "sd", "enum": ["e1"]}})),
            "sd"
        );
        assert_eq!(p(json!({"schema": {"enum": ["e1", "e2"]}})), "e1");
        assert_eq!(p(json!({"schema": {"type": "string"}})), "");
        assert_eq!(p(json!({"example": ["a", "b"]})), r#"["a","b"]"#);
        assert_eq!(p(json!({"example": true})), "true");
        assert_eq!(p(json!({"example": 1.5})), "1.5");
        assert_eq!(p(json!({"example": {"k": 1}})), r#"{"k":1}"#);
        assert_eq!(p(json!({"example": null, "schema": {"default": 3}})), "3");
        assert_eq!(
            p(json!({"examples": {"first": {"value": "ex1"}, "second": {"value": "ex2"}}})),
            "ex1"
        );
        assert_eq!(
            p(
                json!({"content": {"application/json": {"schema": {"type": "object", "example": {"a": 1}}}}})
            ),
            r#"{"a":1}"#
        );
    }

    #[test]
    fn ac_oas_5_param_refs_and_path_level_merge() {
        let imp = import(BOOKSHOP);
        // $ref to #/components/parameters
        assert_eq!(by_name(&imp, "List books").params[0].key, "limit");
        // Path-level header inherited...
        let get = by_name(&imp, "Get a book");
        assert_eq!(
            get.headers,
            vec![KeyValue::new("X-Tenant", "public").disabled()]
        );
        // ...and overridden by the operation (name + in).
        let put = by_name(&imp, "Replace a book");
        assert_eq!(
            header(put, "X-Tenant"),
            Some(&KeyValue::new("X-Tenant", "acme"))
        );
        assert_eq!(
            put.headers.iter().filter(|h| h.key == "X-Tenant").count(),
            1
        );

        // Swagger 2.0 #/parameters refs.
        let imp = import(PETSTORE);
        let find = by_name(&imp, "Finds Pets by status");
        assert_eq!(
            find.params[1].description.as_deref(),
            Some("Maximum number of results")
        );

        // Same name, different location: both kept.
        let imp = import(&oas3(
            json!({"/m": {
                "parameters": [{"name": "v", "in": "query", "example": "path-level"}],
                "get": {"parameters": [{"name": "v", "in": "header", "example": "h"}]}
            }}),
            json!({}),
        ));
        assert_eq!(imp.requests[0].params[0].value, "path-level");
        assert_eq!(imp.requests[0].headers[0].value, "h");
    }

    // -- AC-OAS-6 ---------------------------------------------------------------

    #[test]
    fn ac_oas_6_json_body_example_and_skeleton() {
        let imp = import(BOOKSHOP);
        let add = by_name(&imp, "Add a book");
        assert_eq!(
            body_json(add),
            json!({"title": "The Dispossessed", "authorId": "au_1", "tags": ["scifi"]})
        );
        assert!(
            add.body.text_str().contains("\n  \"title\""),
            "2-space pretty print"
        );
        assert!(header(add, "Content-Type").is_none());

        let put = by_name(&imp, "Replace a book");
        assert_eq!(
            body_json(put),
            json!({
                "id": "", "title": "", "authorId": "", "published": "", "price": 9.99,
                "inStock": false, "tags": [""], "format": "hardcover"
            })
        );
        // Media type is not the JSON default → explicit Content-Type row.
        assert_eq!(
            header(put, "Content-Type").map(|h| h.value.as_str()),
            Some("application/vnd.bookshop.book+json")
        );
        assert_eq!(by_name(&imp, "Get a book").body, Body::none());

        let search = import(SEARCH);
        assert_eq!(
            body_json(by_name(&search, "queryProductBySku")),
            json!({"fields": ["name", "price"]}),
            "first `examples` value"
        );
    }

    #[test]
    fn ac_oas_6_skeleton_composition_cycles_and_depth() {
        let imp = import(ORGCHART);
        let create = by_name(&imp, "createEmployee");
        assert_eq!(
            body_json(create),
            json!({
                "id": "", "name": "Ada", "manager": {}, "reports": [{}],
                "department": {"code": "ENG", "parent": {}, "head": {}}
            })
        );
        let contact = by_name(&imp, "Set contact");
        assert_eq!(body_json(contact), json!({"email": ""}), "oneOf → first");
        assert_eq!(
            header(contact, "Content-Type").map(|h| h.value.as_str()),
            Some("application/merge-patch+json")
        );
        let team = body_json(by_name(&imp, "Create team"));
        assert_eq!(team["name"], "");
        assert_eq!(team["size"], 0);
        assert_eq!(team["members"][0]["department"]["code"], "ENG");
        assert!(
            team.get("address").is_none(),
            "external ref property skipped"
        );

        // Depth limit: 5 levels of nested objects are expanded.
        let mut schema = json!({"type": "string"});
        for i in (0..10).rev() {
            schema = json!({"type": "object", "properties": {format!("l{i}"): schema}});
        }
        let imp = import(&oas3(
            json!({"/deep": {"post": {"requestBody": {"content": {"application/json": {"schema": schema}}}}}}),
            json!({}),
        ));
        assert_eq!(
            body_json(&imp.requests[0]),
            json!({"l0": {"l1": {"l2": {"l3": {"l4": {}}}}}})
        );

        // allOf merge + scalar types + nullable + enum + default + additionalProperties.
        let imp = import(&oas3(
            json!({"/c": {"post": {"requestBody": {"content": {"application/json": {"schema": {
                "allOf": [
                    {"$ref": "#/components/schemas/Base"},
                    {"properties": {"extra": {"type": "boolean", "default": true}}}
                ],
                "properties": {"own": {"type": ["null", "integer"]}}
            }}}}}}}),
            json!({"components": {"schemas": {
                "Base": {"type": "object", "properties": {
                    "n": {"type": "number"},
                    "kind": {"type": "string", "enum": ["a", "b"]},
                    "map": {"type": "object", "additionalProperties": {"type": "string"}},
                    "any": {}
                }},
                "Self": {"$ref": "#/components/schemas/Self"}
            }}}),
        ));
        assert_eq!(
            body_json(&imp.requests[0]),
            json!({"n": 0, "kind": "a", "map": {}, "any": {}, "extra": true, "own": 0})
        );
    }

    #[test]
    fn ac_oas_6_form_bodies() {
        let imp = import(BOOKSHOP);
        let cover = by_name(&imp, "Upload a cover");
        assert_eq!(
            cover.body,
            Body::form(vec![
                FormField::text("caption", "Front cover"),
                FormField::file("image", ""),
            ])
        );
        assert!(header(cover, "Content-Type").is_none());
        let author = by_name(&imp, "Create an author");
        assert_eq!(
            author.body,
            Body::form_encoded(vec![
                KeyValue::new("name", "Ursula K. Le Guin"),
                KeyValue::new("born", "1929"),
            ])
        );
        assert!(header(author, "Content-Type").is_none());

        // allOf-merged multipart schema, arrays of binaries are files too.
        let imp = import(&oas3(
            json!({"/u": {"post": {"requestBody": {"content": {"multipart/form-data": {"schema": {
                "allOf": [{"$ref": "#/components/schemas/Meta"}],
                "properties": {
                    "files": {"type": "array", "items": {"type": "string", "format": "binary"}},
                    "b64": {"type": "string", "format": "base64"}
                }
            }}}}}}}),
            json!({"components": {"schemas": {"Meta": {"properties": {"title": {"type": "string", "example": "t"}}}}}}),
        ));
        let form = imp.requests[0].body.form.clone().unwrap_or_default();
        let kinds: Vec<_> = form
            .iter()
            .map(|f| (f.key.as_str(), f.kind.clone(), f.value.as_str()))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("title", FormFieldKind::Text, "t"),
                ("files", FormFieldKind::File, ""),
                ("b64", FormFieldKind::File, ""),
            ]
        );
    }

    #[test]
    fn ac_oas_6_xml_text_binary_and_content_type_rows() {
        let imp = import(BOOKSHOP);
        let csv = by_name(&imp, "Bulk import CSV");
        assert_eq!(
            csv.body,
            Body::text_of(BodyType::Text, "title,author\nDune,Herbert\n")
        );
        assert_eq!(
            header(csv, "Content-Type").map(|h| h.value.as_str()),
            Some("text/csv")
        );

        let body = |media: &str, media_obj: Value| {
            let mut content = Map::new();
            content.insert(media.to_string(), media_obj);
            let imp = import(&oas3(
                json!({"/b": {"post": {"requestBody": {"content": content}}}}),
                json!({}),
            ));
            let r = imp.requests[0].clone();
            let ct = header(&r, "Content-Type").map(|h| h.value.clone());
            (r.body, ct)
        };
        assert_eq!(
            body("application/xml", json!({"example": "<a/>"})),
            (Body::text_of(BodyType::Xml, "<a/>"), None)
        );
        assert_eq!(
            body("text/xml", json!({"example": {"not": "a string"}})),
            (Body::text_of(BodyType::Xml, ""), Some("text/xml".into()))
        );
        assert_eq!(
            body(
                "text/plain; charset=utf-8",
                json!({"schema": {"type": "string", "example": "hi"}})
            ),
            (Body::text_of(BodyType::Text, "hi"), None)
        );
        assert_eq!(
            body("application/octet-stream", json!({})),
            (Body::binary(""), None)
        );
        assert_eq!(
            body("image/png", json!({})),
            (Body::binary(""), Some("image/png".into()))
        );
        assert_eq!(
            body("application/json", json!({"example": "{\"a\":1}"})),
            (Body::json("{\n  \"a\": 1\n}"), None)
        );
        assert_eq!(body("application/json", json!({})), (Body::json(""), None));

        // JSON is preferred over the first media type; +json over others.
        let imp = import(&oas3(
            json!({"/p": {"post": {"requestBody": {"content": {
                "text/plain": {"example": "x"},
                "application/problem+json": {"example": {"p": 1}},
            }}}}}),
            json!({}),
        ));
        assert_eq!(body_json(&imp.requests[0]), json!({"p": 1}));
    }

    #[test]
    fn ac_oas_6_swagger2_body_and_form_data() {
        let imp = import(PETSTORE);
        let add = by_name(&imp, "Add a new pet to the store");
        assert_eq!(
            body_json(add),
            json!({
                "id": 0, "category": {"id": 0, "name": ""}, "name": "doggie",
                "photoUrls": [""], "tags": [{"id": 0, "name": ""}], "status": "available"
            })
        );
        assert!(header(add, "Content-Type").is_none());
        // consumes is XML-only → XML body.
        let update = by_name(&imp, "Update an existing pet");
        assert_eq!(update.body, Body::text_of(BodyType::Xml, ""));
        assert!(header(update, "Content-Type").is_none());
        // formData without files → form-encoded.
        assert_eq!(
            by_name(&imp, "Updates a pet in the store with form data").body,
            Body::form_encoded(vec![
                KeyValue::new("name", ""),
                KeyValue::new("status", "sold")
            ])
        );
        // formData with a file → multipart.
        assert_eq!(
            by_name(&imp, "uploads an image").body,
            Body::form(vec![
                FormField::text("additionalMetadata", ""),
                FormField::file("file", ""),
            ])
        );
        let order = body_json(by_name(&imp, "placeOrder"));
        assert_eq!(order["complete"], false);
        assert_eq!(order["status"], "placed");
        assert_eq!(by_name(&imp, "Find pet by ID").body, Body::none());
    }

    // -- AC-OAS-7 ---------------------------------------------------------------

    fn auth_for(schemes: Value, security: Option<Value>) -> OpenApiImport {
        let mut extra = json!({"components": {"securitySchemes": schemes}});
        if let (Some(sec), Some(o)) = (security, extra.as_object_mut()) {
            o.insert("security".into(), sec);
        }
        import(&oas3(json!({"/a": {"get": {}}}), extra))
    }

    #[test]
    fn ac_oas_7_security_scheme_mapping() {
        let imp = auth_for(
            json!({"b": {"type": "http", "scheme": "Bearer"}}),
            Some(json!([{"b": []}])),
        );
        assert_eq!(imp.collection.auth, Auth::bearer("{{token}}"));
        assert!(var(&imp, "token").secret);

        let imp = auth_for(json!({"basic": {"type": "http", "scheme": "basic"}}), None);
        assert_eq!(
            imp.collection.auth,
            Auth::basic("{{username}}", "{{password}}")
        );
        assert_eq!(var_keys(&imp), vec!["baseUrl", "username", "password"]);
        assert!(!var(&imp, "username").secret && var(&imp, "password").secret);

        let imp = auth_for(
            json!({"k": {"type": "apiKey", "name": "api_key", "in": "query"}}),
            Some(json!([{"k": []}])),
        );
        assert_eq!(
            imp.collection.auth,
            Auth::api_key("api_key", "{{apiKey}}", ApiKeyLocation::Query)
        );
        assert!(var(&imp, "apiKey").secret);

        for (scheme, needle) in [
            (
                json!({"type": "apiKey", "name": "sid", "in": "cookie"}),
                "cookie",
            ),
            (json!({"type": "oauth2", "flows": {}}), "OAuth2"),
            (
                json!({"type": "openIdConnect", "openIdConnectUrl": "https://x"}),
                "OpenID Connect",
            ),
            (json!({"type": "http", "scheme": "digest"}), "'digest'"),
            (json!({"type": "mutualTLS"}), "mutualTLS"),
        ] {
            let imp = auth_for(json!({"s": scheme}), Some(json!([{"s": []}])));
            assert_eq!(imp.collection.auth, Auth::none());
            assert!(
                has_warning(&imp, needle),
                "{needle}: {:?}",
                imp.report.warnings
            );
            assert_eq!(var_keys(&imp), vec!["baseUrl"]);
        }

        // First requirement whose scheme maps wins (oauth2 first is skipped silently).
        let imp = auth_for(
            json!({"o": {"type": "oauth2"}, "b": {"type": "http", "scheme": "bearer"}}),
            Some(json!([{}, {"o": []}, {"b": []}])),
        );
        assert_eq!(imp.collection.auth, Auth::bearer("{{token}}"));
        assert!(imp.report.warnings.is_empty());

        // Several schemes, no top-level security → none + warning.
        let imp = auth_for(
            json!({"a": {"type": "http", "scheme": "basic"}, "b": {"type": "http", "scheme": "bearer"}}),
            None,
        );
        assert_eq!(imp.collection.auth, Auth::none());
        assert!(has_warning(&imp, "no top-level security"));

        // Swagger 2.0: first mappable requirement (oauth2 skipped → api_key header).
        let imp = import(PETSTORE);
        assert_eq!(
            imp.collection.auth,
            Auth::api_key("api_key", "{{apiKey}}", ApiKeyLocation::Header)
        );
        let imp = import(
            r#"{"swagger":"2.0","host":"h","securityDefinitions":{"b":{"type":"basic"}},"paths":{}}"#,
        );
        assert_eq!(
            imp.collection.auth,
            Auth::basic("{{username}}", "{{password}}")
        );
        let search = import(SEARCH);
        assert_eq!(search.collection.auth.auth_type, AuthType::Basic);
    }

    #[test]
    fn ac_oas_7_requests_inherit_unless_security_is_empty() {
        let imp = import(BOOKSHOP);
        assert_eq!(imp.collection.auth, Auth::bearer("{{token}}"));
        for r in &imp.requests {
            let expected = if r.name == "Service status" {
                Auth::none()
            } else {
                Auth::inherit()
            };
            assert_eq!(r.auth, expected, "{}", r.name);
        }
        let imp = import(PETSTORE);
        assert_eq!(
            by_name(&imp, "Logs user into the system").auth,
            Auth::none()
        );
        assert_eq!(by_name(&imp, "Find pet by ID").auth, Auth::inherit());
    }

    // -- AC-OAS-8 ---------------------------------------------------------------

    #[test]
    fn ac_oas_8_external_and_broken_refs_are_skipped_with_warnings() {
        let imp = import(ORGCHART);
        let w = &imp.report.warnings;
        assert!(
            w.contains(
                &"Skipped external $ref ./shared-params.yaml#/TraceId in GET /employees"
                    .to_string()
            ),
            "{w:?}"
        );
        assert!(
            w.contains(&"Skipped external $ref ./common.json#/Address in POST /teams".to_string()),
            "{w:?}"
        );
        assert!(w.contains(&"Skipped external $ref https://schemas.example.com/team-patch.json in PATCH /teams/{teamId}".to_string()), "{w:?}");
        assert!(w.contains(&"Skipped unresolvable $ref #/components/schemas/DoesNotExist in POST /teams/{teamId}/missing".to_string()), "{w:?}");
        // The external param is skipped, the other one kept.
        let list = by_name(&imp, "List employees");
        assert_eq!(list.params, vec![KeyValue::new("dept", "ENG")]);
        assert_eq!(list.url, "{{baseUrl}}/employees?dept=ENG");
        // Bodies behind external/broken refs are dropped.
        assert_eq!(by_name(&imp, "Patch team").body, Body::none());
        assert_eq!(by_name(&imp, "Broken ref").body, Body::none());
        assert_eq!(imp.report.operations_created, 6);
        // Warnings are deduplicated.
        let unique: HashSet<_> = w.iter().collect();
        assert_eq!(unique.len(), w.len());

        // External path item and percent-encoded / escaped internal pointers.
        let imp = import(&oas3(
            json!({
                "/ext": {"$ref": "other.yaml#/paths/~1x"},
                "/int": {"$ref": "#/x-items/a~1b%20c"},
                "/loop": {"$ref": "#/paths/~1loop"}
            }),
            json!({"x-items": {"a/b c": {"get": {"summary": "via pointer"}}}}),
        ));
        assert_eq!(imp.requests.len(), 1);
        assert_eq!(imp.requests[0].name, "via pointer");
        assert!(has_warning(
            &imp,
            "Skipped external $ref other.yaml#/paths/~1x in path /ext"
        ));
        assert!(has_warning(&imp, "Skipped circular $ref #/paths/~1loop"));
    }

    // -- AC-OAS-9 ---------------------------------------------------------------

    fn thousand_op_spec() -> String {
        let mut paths = Map::new();
        for i in 0..500 {
            let tag = format!("group{}", i % 25);
            paths.insert(
                format!("/resources{i}/{{id}}/items"),
                json!({
                    "parameters": [{"$ref": "#/components/parameters/Id"}],
                    "get": {
                        "tags": [tag],
                        "summary": format!("List items {i}"),
                        "parameters": [
                            {"$ref": "#/components/parameters/Limit"},
                            {"name": "q", "in": "query", "schema": {"type": "string", "example": "abc"}},
                            {"name": "X-Trace", "in": "header", "required": true, "schema": {"type": "string"}}
                        ]
                    },
                    "post": {
                        "tags": [tag],
                        "operationId": format!("createItem{i}"),
                        "requestBody": {"content": {"application/json": {
                            "schema": {"$ref": format!("#/components/schemas/Item{}", i % 50)}
                        }}}
                    }
                }),
            );
        }
        let mut schemas = Map::new();
        for k in 0..50 {
            schemas.insert(
                format!("Item{k}"),
                json!({
                    "type": "object",
                    "properties": {
                        "id": {"type": "string", "format": "uuid"},
                        "name": {"type": "string"},
                        "count": {"type": "integer"},
                        "owner": {"$ref": "#/components/schemas/User"},
                        "tags": {"type": "array", "items": {"$ref": "#/components/schemas/Tag"}},
                        "parent": {"$ref": format!("#/components/schemas/Item{k}")},
                        "meta": {"allOf": [
                            {"$ref": "#/components/schemas/Meta"},
                            {"properties": {"rev": {"type": "integer"}}}
                        ]}
                    }
                }),
            );
        }
        schemas.insert(
            "User".into(),
            json!({"type": "object", "properties": {
                "id": {"type": "string"}, "name": {"type": "string"},
                "address": {"$ref": "#/components/schemas/Address"},
                "manager": {"$ref": "#/components/schemas/User"}
            }}),
        );
        schemas.insert("Address".into(), json!({"type": "object", "properties": {
            "street": {"type": "string"}, "city": {"type": "string"}, "zip": {"type": "string", "example": "10115"}
        }}));
        schemas.insert(
            "Tag".into(),
            json!({"type": "object", "properties": {"name": {"type": "string"}}}),
        );
        schemas.insert("Meta".into(), json!({"type": "object", "properties": {
            "created": {"type": "string", "format": "date-time"}, "labels": {"type": "array", "items": {"type": "string"}}
        }}));
        json!({
            "openapi": "3.1.0",
            "info": {"title": "Big API", "version": "1"},
            "servers": [{"url": "https://big.example.com"}],
            "security": [{"bearer": []}],
            "paths": paths,
            "components": {
                "schemas": schemas,
                "parameters": {
                    "Id": {"name": "id", "in": "path", "required": true, "schema": {"type": "string"}},
                    "Limit": {"name": "limit", "in": "query", "schema": {"type": "integer", "default": 50}}
                },
                "securitySchemes": {"bearer": {"type": "http", "scheme": "bearer"}}
            }
        })
        .to_string()
    }

    #[test]
    fn ac_oas_9_thousand_operations_import_quickly() {
        let spec = thousand_op_spec();
        let start = std::time::Instant::now();
        let imp = import(&spec);
        let elapsed = start.elapsed();
        eprintln!("AC-OAS-9: imported 1,000 operations in {elapsed:?}");
        assert_eq!(imp.report.operations_created, 1000);
        assert_eq!(imp.folders.len(), 25);
        assert!(imp.report.warnings.is_empty(), "{:?}", imp.report.warnings);
        let post = by_name(&imp, "createItem7");
        let body = body_json(post);
        assert_eq!(body["owner"]["address"]["zip"], "10115");
        assert_eq!(body["parent"], json!({}));
        assert_eq!(body["meta"]["rev"], 0);
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "took {elapsed:?}"
        );
    }

    // -- AC-QRY-6 ---------------------------------------------------------------

    #[test]
    fn ac_qry_6_query_method_and_additional_operations() {
        let imp = import(SEARCH);
        let methods: Vec<_> = imp
            .requests
            .iter()
            .map(|r| (r.method.as_str(), r.name.as_str()))
            .collect();
        assert_eq!(
            methods,
            vec![
                ("GET", "List products"),
                ("QUERY", "Search products"),
                ("PURGE", "Purge product cache"),
                ("LINK", "linkProducts"),
                ("QUERY", "queryProductBySku"),
            ]
        );
        assert!(has_warning(&imp, "invalid method name \"bad method\""));
        let search = by_name(&imp, "Search products");
        assert_eq!(
            body_json(search),
            json!({"text": "running shoes", "filters": {"maxPrice": 0, "brands": [""]}, "limit": 25})
        );
        assert_eq!(search.url, "{{baseUrl}}/products");
        let folders: Vec<_> = imp
            .report
            .folders
            .iter()
            .map(|f| (f.name.as_str(), f.request_count))
            .collect();
        assert_eq!(folders, vec![("products", 3), ("admin", 2)]);
        assert_eq!(
            var_keys(&imp),
            vec!["baseUrl", "sku", "username", "password"]
        );
        assert_eq!(imp.report.base_url, "https://search.example.com/v1");

        // `query` is not an operation key in Swagger 2.0.
        let imp = import(r#"{"swagger":"2.0","host":"h","paths":{"/a":{"query":{},"get":{}}}}"#);
        let methods: Vec<_> = imp.requests.iter().map(|r| r.method.as_str()).collect();
        assert_eq!(methods, vec!["GET"]);
    }

    // -- report shape -------------------------------------------------------------

    #[test]
    fn report_serializes_camel_case() {
        let imp = import(BOOKSHOP);
        let v = serde_json::to_value(&imp.report).expect("serialize");
        assert_eq!(v["collectionName"], "Bookshop API");
        assert_eq!(v["operationsCreated"], 10);
        assert_eq!(v["rootRequestCount"], 1);
        assert_eq!(v["baseUrl"], "https://api.bookshop.example.com/v1");
        assert_eq!(v["folders"][0], json!({"name": "books", "requestCount": 7}));
        assert_eq!(v["spec"]["label"], "OpenAPI 3.0 · YAML");
        assert!(v["warnings"].as_array().is_some());
    }

    // -- robustness ---------------------------------------------------------------

    const VOCAB: &[&str] = &[
        "$ref",
        "#/components/schemas/A",
        "#/components/schemas/B",
        "#/definitions/A",
        "#/components/parameters/P",
        "#/parameters/P",
        "#",
        "#/",
        "#/paths",
        "#bad",
        "./ext.yaml#/X",
        "type",
        "object",
        "array",
        "string",
        "integer",
        "file",
        "null",
        "properties",
        "items",
        "allOf",
        "oneOf",
        "anyOf",
        "example",
        "examples",
        "default",
        "enum",
        "const",
        "name",
        "in",
        "query",
        "header",
        "path",
        "body",
        "formData",
        "cookie",
        "required",
        "schema",
        "content",
        "application/json",
        "multipart/form-data",
        "application/x-www-form-urlencoded",
        "text/plain",
        "application/xml",
        "image/png",
        "requestBody",
        "parameters",
        "tags",
        "summary",
        "operationId",
        "security",
        "get",
        "post",
        "additionalOperations",
        "value",
        "format",
        "binary",
        "consumes",
        "http",
        "bearer",
        "basic",
        "apiKey",
        "oauth2",
        "scheme",
        "{id}",
        "/a/{id}",
        "x-y",
    ];

    fn arb_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i32>().prop_map(Value::from),
            prop::sample::select(VOCAB).prop_map(|s| Value::String(s.to_string())),
            ".{0,6}".prop_map(Value::String),
        ];
        leaf.prop_recursive(5, 64, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
                prop::collection::vec((prop::sample::select(VOCAB), inner), 0..6).prop_map(|kv| {
                    Value::Object(kv.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
                }),
            ]
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        #[test]
        fn never_panics_on_arbitrary_text(text in any::<String>()) {
            let _ = detect_spec(&text);
            let _ = import_openapi(&text, "w", "p");
            let _ = import_openapi(&format!("openapi: 3.1.0\n{text}"), "w", "p");
        }

        #[test]
        fn never_panics_on_structured_garbage(
            op in arb_value(), op2 in arb_value(), params in arb_value(),
            a in arb_value(), b in arb_value(), p in arb_value(),
            sec in arb_value(), schemes in arb_value(), servers in arb_value(),
        ) {
            let oas = json!({
                "openapi": "3.2.0",
                "servers": servers,
                "paths": {"/a/{id}": {"get": op, "query": op2, "parameters": params}, "/b": a},
                "components": {
                    "schemas": {"A": a, "B": b},
                    "parameters": {"P": p},
                    "securitySchemes": schemes
                },
                "security": sec
            });
            let swagger = json!({
                "swagger": "2.0",
                "host": servers,
                "paths": {"/a/{id}": {"post": op, "parameters": params}},
                "definitions": {"A": a, "B": b},
                "parameters": {"P": p},
                "securityDefinitions": schemes,
                "security": sec
            });
            for doc in [oas, swagger] {
                let text = doc.to_string();
                prop_assert!(import_openapi(&text, "w", "p").is_ok());
                if let Ok(yaml) = serde_yaml_ng::to_string(&doc) {
                    prop_assert!(import_openapi(&yaml, "w", "p").is_ok());
                }
            }
        }
    }

    #[test]
    fn pathological_documents_terminate() {
        // Self-referencing allOf / ref chains and a wide fan-out schema.
        let mut props = Map::new();
        for i in 0..30 {
            props.insert(
                format!("p{i}"),
                json!({"$ref": "#/components/schemas/Wide"}),
            );
        }
        let imp = import(&oas3(
            json!({"/x": {"post": {"requestBody": {"content": {"application/json": {
                "schema": {"$ref": "#/components/schemas/Wide"}
            }}}}}, "/y": {"post": {"requestBody": {"content": {"application/json": {
                "schema": {"allOf": [{"$ref": "#/components/schemas/Loop"}]}
            }}}}}}),
            json!({"components": {"schemas": {
                "Wide": {"type": "object", "properties": {"leaf": {"type": "string"}, "next": {"type": "object", "properties": props}}},
                "Loop": {"allOf": [{"$ref": "#/components/schemas/Loop2"}]},
                "Loop2": {"allOf": [{"$ref": "#/components/schemas/Loop"}]}
            }}}),
        ));
        assert_eq!(imp.requests.len(), 2);
        assert!(imp.requests[0].body.text_str().len() < 5_000_000);
        assert_eq!(body_json(&imp.requests[1]), json!({}));
    }
}

//! Data model (PRODUCT.md §3). All structs serialize with camelCase keys so the
//! JSON on disk, over Tauri IPC and on the sync wire is identical.

use serde::{Deserialize, Serialize};

/// File-level schema version written into every persisted JSON file.
pub const SCHEMA_VERSION: u32 = 1;

/// Generate a new ULID string id.
pub fn new_id() -> String {
    ulid::Ulid::generate().to_string()
}

/// Current Unix time in epoch milliseconds.
pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Methods offered in the dropdown (AC-REQ-1). Any other method must match
/// [`is_valid_method`].
pub const STANDARD_METHODS: [&str; 8] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "QUERY", "HEAD", "OPTIONS",
];

/// `^[A-Z][A-Z0-9_-]*$` (AC-REQ-1).
pub fn is_valid_method(method: &str) -> bool {
    let mut chars = method.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Workspace {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync: Option<SyncConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncConfig {
    pub worker_url: String,
    pub remote_workspace_id: String,
    pub token: String,
    /// Highest server `rev` seen by the last pull.
    #[serde(default)]
    pub cursor: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub active_environment_id: Option<String>,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    pub id: String,
    pub project_id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Folder {
    pub id: String,
    pub collection_id: String,
    #[serde(default)]
    pub parent_folder_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub id: String,
    /// Owning collection. Empty string for an unsaved "scratch" request.
    #[serde(default)]
    pub collection_id: String,
    #[serde(default)]
    pub folder_id: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub params: Vec<KeyValue>,
    #[serde(default)]
    pub headers: Vec<KeyValue>,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub body: Body,
    #[serde(default)]
    pub settings: RequestSettings,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

fn default_method() -> String {
    "GET".to_string()
}

impl Default for Request {
    fn default() -> Self {
        Request {
            id: new_id(),
            collection_id: String::new(),
            folder_id: None,
            name: String::new(),
            method: default_method(),
            url: String::new(),
            params: Vec::new(),
            headers: Vec::new(),
            auth: Auth::default(),
            body: Body::default(),
            settings: RequestSettings::default(),
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyValue {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl KeyValue {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        KeyValue {
            key: key.into(),
            value: value.into(),
            enabled: true,
            description: None,
        }
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthType {
    #[default]
    None,
    Inherit,
    Basic,
    Bearer,
    Apikey,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Auth {
    #[serde(rename = "type")]
    pub auth_type: AuthType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<ApiKeyAuth>,
}

impl Auth {
    pub fn none() -> Self {
        Auth::default()
    }
    pub fn inherit() -> Self {
        Auth {
            auth_type: AuthType::Inherit,
            ..Auth::default()
        }
    }
    pub fn basic(username: impl Into<String>, password: impl Into<String>) -> Self {
        Auth {
            auth_type: AuthType::Basic,
            username: Some(username.into()),
            password: Some(password.into()),
            ..Auth::default()
        }
    }
    pub fn bearer(token: impl Into<String>) -> Self {
        Auth {
            auth_type: AuthType::Bearer,
            token: Some(token.into()),
            ..Auth::default()
        }
    }
    pub fn api_key(
        key: impl Into<String>,
        value: impl Into<String>,
        location: ApiKeyLocation,
    ) -> Self {
        Auth {
            auth_type: AuthType::Apikey,
            api_key: Some(ApiKeyAuth {
                key: key.into(),
                value: value.into(),
                location,
            }),
            ..Auth::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiKeyAuth {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(rename = "in", default)]
    pub location: ApiKeyLocation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKeyLocation {
    #[default]
    Header,
    Query,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BodyType {
    #[default]
    None,
    Json,
    Xml,
    Text,
    Form,
    FormEncoded,
    Graphql,
    Binary,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Body {
    #[serde(rename = "type")]
    pub body_type: BodyType,
    /// Content for json / xml / text bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Multipart fields (body type `form`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Vec<FormField>>,
    /// URL-encoded fields (body type `formEncoded`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form_encoded: Option<Vec<KeyValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graphql: Option<GraphqlBody>,
    /// File streamed as the body (body type `binary`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
}

impl Body {
    pub fn none() -> Self {
        Body::default()
    }
    pub fn text_of(body_type: BodyType, text: impl Into<String>) -> Self {
        Body {
            body_type,
            text: Some(text.into()),
            ..Body::default()
        }
    }
    pub fn json(text: impl Into<String>) -> Self {
        Self::text_of(BodyType::Json, text)
    }
    pub fn form(fields: Vec<FormField>) -> Self {
        Body {
            body_type: BodyType::Form,
            form: Some(fields),
            ..Body::default()
        }
    }
    pub fn form_encoded(fields: Vec<KeyValue>) -> Self {
        Body {
            body_type: BodyType::FormEncoded,
            form_encoded: Some(fields),
            ..Body::default()
        }
    }
    pub fn graphql(query: impl Into<String>, variables: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Graphql,
            graphql: Some(GraphqlBody {
                query: query.into(),
                variables: variables.into(),
            }),
            ..Body::default()
        }
    }
    pub fn binary(path: impl Into<String>) -> Self {
        Body {
            body_type: BodyType::Binary,
            binary_path: Some(path.into()),
            ..Body::default()
        }
    }
    /// Text content (empty string when absent).
    pub fn text_str(&self) -> &str {
        self.text.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphqlBody {
    #[serde(default)]
    pub query: String,
    /// Variables as JSON text, exactly as edited. Empty means "no variables".
    #[serde(default)]
    pub variables: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FormFieldKind {
    #[default]
    Text,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FormField {
    pub key: String,
    /// Text value, or the file path for `kind = "file"`.
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub kind: FormFieldKind,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Optional part Content-Type (from cURL `;type=` suffixes, AC-CURL-10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

impl FormField {
    pub fn text(key: impl Into<String>, value: impl Into<String>) -> Self {
        FormField {
            key: key.into(),
            value: value.into(),
            kind: FormFieldKind::Text,
            enabled: true,
            content_type: None,
        }
    }
    pub fn file(key: impl Into<String>, path: impl Into<String>) -> Self {
        FormField {
            key: key.into(),
            value: path.into(),
            kind: FormFieldKind::File,
            enabled: true,
            content_type: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestSettings {
    #[serde(default = "default_true")]
    pub follow_redirects: bool,
    #[serde(default = "default_timeout")]
    pub timeout_ms: u64,
    #[serde(default = "default_true")]
    pub verify_tls: bool,
}

fn default_timeout() -> u64 {
    30_000
}

impl Default for RequestSettings {
    fn default() -> Self {
        RequestSettings {
            follow_redirects: true,
            timeout_ms: default_timeout(),
            verify_tls: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Environment {
    pub id: String,
    pub workspace_id: String,
    /// `None` = the Global environment.
    #[serde(default)]
    pub project_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub variables: Vec<Variable>,
    #[serde(default)]
    pub sort_order: i64,
    #[serde(default)]
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Variable {
    pub key: String,
    #[serde(default)]
    pub value: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub secret: bool,
}

impl Variable {
    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Variable {
            key: key.into(),
            value: value.into(),
            enabled: true,
            secret: false,
        }
    }
    pub fn secret(mut self) -> Self {
        self.secret = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ErrorKind {
    Dns,
    Connect,
    Tls,
    Timeout,
    Cancelled,
    Other,
}

/// A typed network error (AC-REQ-9). Never a crash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendError {
    pub kind: ErrorKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BodyEncoding {
    #[default]
    Utf8,
    Base64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryResponse {
    pub status: u16,
    pub status_text: String,
    pub time_ms: u64,
    /// Decoded body size in bytes (full size, even when `body` is truncated).
    pub size_bytes: u64,
    pub headers: Vec<(String, String)>,
    /// At most 1 MB of the decoded body, encoded per `bodyEncoding`.
    pub body: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub body_encoding: BodyEncoding,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub id: String,
    pub timestamp: i64,
    #[serde(default)]
    pub source_request_id: Option<String>,
    /// The request as edited (variables unresolved).
    pub request: Request,
    /// The URL actually sent; secret variable values are kept as `{{name}}`.
    pub resolved_url: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<HistoryResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<SendError>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntityType {
    Project,
    Collection,
    Folder,
    Request,
    Environment,
}

impl EntityType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntityType::Project => "project",
            EntityType::Collection => "collection",
            EntityType::Folder => "folder",
            EntityType::Request => "request",
            EntityType::Environment => "environment",
        }
    }
    /// Parent-first ordering used when applying pulled changes.
    pub fn depth(&self) -> u8 {
        match self {
            EntityType::Project => 0,
            EntityType::Environment => 1,
            EntityType::Collection => 1,
            EntityType::Folder => 2,
            EntityType::Request => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tombstone {
    pub id: String,
    #[serde(rename = "type")]
    pub entity_type: EntityType,
    pub deleted_at: i64,
}

/// One project's file: `projects/<projectId>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectTree {
    pub project: Project,
    #[serde(default)]
    pub collections: Vec<Collection>,
    #[serde(default)]
    pub folders: Vec<Folder>,
    #[serde(default)]
    pub requests: Vec<Request>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

/// `settings.json`. Unknown keys written by the UI are preserved in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    #[serde(default, deserialize_with = "lenient_theme")]
    pub theme: Theme,
    #[serde(default, deserialize_with = "lenient_opt_px")]
    pub window_width: Option<u32>,
    #[serde(default, deserialize_with = "lenient_opt_px")]
    pub window_height: Option<u32>,
    /// Accepts fractional pixels from HiDPI layouts; rounded (QA-R2-001).
    #[serde(default = "default_sidebar_width", deserialize_with = "lenient_px")]
    pub sidebar_width: u32,
    #[serde(default)]
    pub sidebar_collapsed: bool,
    #[serde(default = "default_split_ratio", deserialize_with = "lenient_ratio")]
    pub split_ratio: f64,
    /// "auto" | "side" | "stacked"
    #[serde(default = "default_layout_mode")]
    pub layout_mode: String,
    #[serde(default)]
    pub last_opened_request_id: Option<String>,
    #[serde(default)]
    pub active_project_id: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn default_sidebar_width() -> u32 {
    300
}

/// Largest pixel size accepted for persisted layout values.
const MAX_PX: f64 = 100_000.0;

/// Any JSON number (or numeric string) -> rounded, clamped pixel count.
/// Non-numbers yield `None` instead of failing the whole settings object.
fn px_from_value(v: &serde_json::Value) -> Option<u32> {
    let n = match v {
        serde_json::Value::Number(n) => n.as_f64()?,
        serde_json::Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    n.is_finite().then(|| n.round().clamp(0.0, MAX_PX) as u32)
}

fn lenient_px<'de, D: serde::Deserializer<'de>>(d: D) -> Result<u32, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(px_from_value(&v).unwrap_or_else(default_sidebar_width))
}

fn lenient_opt_px<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<u32>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(px_from_value(&v))
}

fn lenient_ratio<'de, D: serde::Deserializer<'de>>(d: D) -> Result<f64, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_f64()
        .filter(|r| r.is_finite())
        .map(|r| r.clamp(0.0, 1.0))
        .unwrap_or_else(default_split_ratio))
}

fn lenient_theme<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Theme, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}
fn default_split_ratio() -> f64 {
    0.5
}
fn default_layout_mode() -> String {
    "auto".to_string()
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            theme: Theme::System,
            window_width: None,
            window_height: None,
            sidebar_width: default_sidebar_width(),
            sidebar_collapsed: false,
            split_ratio: default_split_ratio(),
            layout_mode: default_layout_mode(),
            last_opened_request_id: None,
            active_project_id: None,
            extra: serde_json::Map::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_req_1_method_validation() {
        for m in STANDARD_METHODS {
            assert!(is_valid_method(m));
        }
        assert!(is_valid_method("PURGE"));
        assert!(is_valid_method("X-CUSTOM_1"));
        assert!(!is_valid_method("get"));
        assert!(!is_valid_method("1GET"));
        assert!(!is_valid_method(""));
        assert!(!is_valid_method("GE T"));
        assert!(!is_valid_method("-GET"));
    }

    #[test]
    fn ids_are_ulids() {
        let id = new_id();
        assert_eq!(id.len(), 26);
        assert!(ulid::Ulid::from_string(&id).is_ok());
    }

    #[test]
    fn request_json_shape_is_camel_case_with_defaults() {
        let req: Request = serde_json::from_str(r#"{"id":"x","url":"http://a"}"#).unwrap();
        assert_eq!(req.method, "GET");
        assert!(req.settings.follow_redirects);
        assert_eq!(req.settings.timeout_ms, 30000);
        assert!(req.settings.verify_tls);
        let v = serde_json::to_value(&req).unwrap();
        assert!(v.get("collectionId").is_some());
        assert_eq!(v["auth"]["type"], "none");
        assert_eq!(v["body"]["type"], "none");
        let b = serde_json::to_value(Body::form_encoded(vec![])).unwrap();
        assert_eq!(b["type"], "formEncoded");
        let a = serde_json::to_value(Auth::api_key("k", "v", ApiKeyLocation::Query)).unwrap();
        assert_eq!(a["apiKey"]["in"], "query");
        assert_eq!(a["type"], "apikey");
    }

    #[test]
    fn qa_r2_001_settings_accept_fractional_and_odd_numbers() {
        let s: Settings = serde_json::from_str(
            r#"{"sidebarWidth":320.5,"windowWidth":1279.6,"windowHeight":null,"splitRatio":1.7,"theme":"neon"}"#,
        )
        .unwrap();
        assert_eq!(s.sidebar_width, 321);
        assert_eq!(s.window_width, Some(1280));
        assert_eq!(s.window_height, None);
        assert_eq!(s.split_ratio, 1.0);
        assert_eq!(s.theme, Theme::System);
        let s: Settings =
            serde_json::from_str(r#"{"sidebarWidth":-5,"windowWidth":"800"}"#).unwrap();
        assert_eq!(s.sidebar_width, 0);
        assert_eq!(s.window_width, Some(800));
        let s: Settings = serde_json::from_str(r#"{"sidebarWidth":"wide"}"#).unwrap();
        assert_eq!(s.sidebar_width, 300);
        // round-trips as integers
        let v = serde_json::to_value(Settings {
            sidebar_width: 321,
            ..Settings::default()
        })
        .unwrap();
        assert_eq!(v["sidebarWidth"], 321);
    }

    #[test]
    fn qa_r2_002_settings_round_trip_ui_share_options_key() {
        let raw = r#"{"theme":"dark","sidebarWidth":312.5,"shareOptions":{"S1":{"includeCredentials":true,"environmentId":"e1"},"S2":{"includeCredentials":false,"environmentId":null}}}"#;
        let s: Settings = serde_json::from_str(raw).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["shareOptions"]["S1"]["includeCredentials"], true);
        assert_eq!(v["shareOptions"]["S1"]["environmentId"], "e1");
        assert!(v["shareOptions"]["S2"]["environmentId"].is_null());
        assert_eq!(v["sidebarWidth"], 313);
        let again: Settings = serde_json::from_value(v).unwrap();
        assert_eq!(again, s);
    }

    #[test]
    fn settings_preserve_unknown_keys() {
        let s: Settings = serde_json::from_str(r#"{"theme":"dark","fooBar":42}"#).unwrap();
        assert_eq!(s.theme, Theme::Dark);
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(v["fooBar"], 42);
        assert_eq!(v["sidebarWidth"], 300);
    }
}

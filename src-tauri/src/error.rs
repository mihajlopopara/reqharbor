//! The single error shape every command rejects with:
//! `{ code: string, message: string, details?: unknown }`.

use reqharbor_core::curl::CurlError;
use reqharbor_core::format::FormatError;
use reqharbor_core::openapi::OpenApiError;
use reqharbor_core::prepare::PrepareError;
use reqharbor_core::storage::StoreError;
use reqharbor_core::sync::SyncError;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

impl CommandError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        CommandError {
            code: code.to_string(),
            message: message.into(),
            details: None,
        }
    }
}

pub type CmdResult<T> = Result<T, CommandError>;

impl From<StoreError> for CommandError {
    fn from(e: StoreError) -> Self {
        let code = match e {
            StoreError::NotFound(_) => "not_found",
            StoreError::Invalid(_) => "invalid",
            StoreError::Io(_) => "io",
            StoreError::ReadOnlyNewer(_) => "read_only_newer",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<PrepareError> for CommandError {
    fn from(e: PrepareError) -> Self {
        if e == PrepareError::NeedsNewerVersion {
            return CommandError::new("read_only_newer", e.to_string());
        }
        let mut err = CommandError::new("validation", e.to_string());
        err.details = serde_json::to_value(&e).ok();
        err
    }
}

impl From<CurlError> for CommandError {
    fn from(e: CurlError) -> Self {
        let code = match e {
            CurlError::NotCurl => "not_curl",
            CurlError::MissingUrl => "curl_missing_url",
            CurlError::Syntax(_) => "curl_syntax",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<OpenApiError> for CommandError {
    fn from(e: OpenApiError) -> Self {
        let code = match e {
            OpenApiError::Parse(_) => "openapi_parse",
            OpenApiError::Unsupported(_) => "openapi_unsupported",
            OpenApiError::Invalid(_) => "openapi_invalid",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<SyncError> for CommandError {
    fn from(e: SyncError) -> Self {
        let code = match e {
            SyncError::NotConnected => "sync_not_connected",
            SyncError::Unauthorized => "sync_unauthorized",
            SyncError::Offline(_) => "sync_offline",
            SyncError::Server(_) => "sync_server",
            SyncError::Gone(_) => "share_gone",
            SyncError::NotFound(_) => "sync_not_found",
            SyncError::TooLarge(_) => "sync_too_large",
            SyncError::QuotaExceeded(_) => "sync_quota_exceeded",
            SyncError::Rejected(_) => "sync_rejected",
            SyncError::Protocol(_) => "sync_protocol",
            SyncError::Store(_) => "io",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<FormatError> for CommandError {
    fn from(e: FormatError) -> Self {
        let mut err = CommandError::new("format_error", e.to_string());
        err.details = serde_json::to_value(&e).ok();
        err
    }
}

impl From<std::io::Error> for CommandError {
    fn from(e: std::io::Error) -> Self {
        CommandError::new("io", e.to_string())
    }
}

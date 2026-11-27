//! The single error shape every command rejects with:
//! `{ code: string, message: string, details?: unknown }`.

use reqharbor_core::auth::AuthError;
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
            StoreError::ReadOnlyRole(_) => "read_only_role",
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
            SyncError::SignInRequired => "sign_in_required",
            SyncError::SpaceGone(_) => "space_unavailable",
            SyncError::Forbidden(_) => "forbidden_role",
            SyncError::RateLimited(_) => "rate_limited",
            SyncError::ClientTooOld(_) => "client_too_old",
            SyncError::ServerFull(_) => "server_full",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<AuthError> for CommandError {
    fn from(e: AuthError) -> Self {
        if let AuthError::Refused { code, message } = &e {
            let text = reqharbor_core::teams::refusal_message(code, "").unwrap_or(message.clone());
            return CommandError::new(code, text);
        }
        let code = match e {
            AuthError::Offline(_) => "server_unreachable",
            AuthError::Cancelled => "sign_in_cancelled",
            AuthError::Timeout => "sign_in_timeout",
            AuthError::Denied(_) => "sign_in_denied",
            AuthError::LoopbackUnavailable(_) => "loopback_unavailable",
            AuthError::InvalidGrant => "sign_in_failed",
            AuthError::SignInRequired | AuthError::Unauthorized(_) => "sign_in_required",
            AuthError::ClientTooOld => "client_too_old",
            AuthError::RateLimited => "rate_limited",
            AuthError::ServerFull => "server_full",
            AuthError::Server(_) | AuthError::Protocol(_) => "server_error",
            AuthError::Rejected(_) => "auth_rejected",
            AuthError::Keychain(_) => "keychain_unavailable",
            AuthError::NotSignedIn => "not_signed_in",
            AuthError::Store(_) => "io",
            AuthError::Refused { .. } => "auth_rejected",
        };
        CommandError::new(code, e.to_string())
    }
}

impl From<reqharbor_core::transfer::TransferError> for CommandError {
    fn from(e: reqharbor_core::transfer::TransferError) -> Self {
        use reqharbor_core::transfer::TransferError;
        match e {
            TransferError::Store(e) => e.into(),
            TransferError::Sync(e) => e.into(),
            TransferError::Rejected { reason, message } => {
                let code = match reason.as_str() {
                    "forbidden_role" => "forbidden_role",
                    "entity_too_large" => "sync_too_large",
                    "clock_skew" => "clock_skew",
                    _ => "sync_rejected",
                };
                CommandError::new(code, message)
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_errors_have_stable_codes_and_no_secrets() {
        let cases = [
            (
                AuthError::Offline("connection failed".into()),
                "server_unreachable",
            ),
            (AuthError::Timeout, "sign_in_timeout"),
            (AuthError::Denied("access_denied".into()), "sign_in_denied"),
            (
                AuthError::LoopbackUnavailable("AddrInUse".into()),
                "loopback_unavailable",
            ),
            (AuthError::InvalidGrant, "sign_in_failed"),
            (AuthError::SignInRequired, "sign_in_required"),
            (AuthError::Keychain("denied".into()), "keychain_unavailable"),
            (AuthError::RateLimited, "rate_limited"),
            (AuthError::ServerFull, "server_full"),
        ];
        for (e, code) in cases {
            let c = CommandError::from(e);
            assert_eq!(c.code, code);
            assert!(!c.message.contains("rha_") && !c.message.contains("rhr_"));
        }
        assert_eq!(
            CommandError::from(AuthError::Timeout).message,
            "Sign-in timed out. Try again."
        );
    }

    #[test]
    fn sync_errors_of_account_spaces_have_codes() {
        for (e, code) in [
            (SyncError::SignInRequired, "sign_in_required"),
            (SyncError::SpaceGone("x".into()), "space_unavailable"),
            (SyncError::ClientTooOld("x".into()), "client_too_old"),
            (SyncError::ServerFull("x".into()), "server_full"),
            (SyncError::RateLimited("x".into()), "rate_limited"),
            (SyncError::Unauthorized, "sync_unauthorized"),
        ] {
            assert_eq!(CommandError::from(e).code, code);
        }
        assert_eq!(
            CommandError::from(StoreError::ReadOnlyRole("v".into())).code,
            "read_only_role"
        );
        assert_eq!(
            CommandError::from(SyncError::ServerFull("x".into())).message,
            "The sync server is out of space. Your changes are saved on this device."
        );
    }

    #[test]
    fn a_real_loopback_bind_failure_is_loopback_unavailable() {
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = taken.local_addr().unwrap().port();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let err = rt
            .block_on(reqharbor_core::auth::PendingSignIn::start_on(
                ("127.0.0.1", port),
                "https://rh.acme.dev",
                None,
                "x",
            ))
            .err()
            .expect("a taken port can't be bound");
        assert!(matches!(err, AuthError::LoopbackUnavailable(_)), "{err:?}");
        let c = CommandError::from(err);
        assert_eq!(c.code, "loopback_unavailable");
        assert!(c.message.contains("Sync › Advanced"), "{}", c.message);
    }
}

//! [`Request`] -> multi-line bash cURL command (PRODUCT §5.5, AC-CEXP-2..5).
//!
//! The command is generated from the *prepared* request, so what cURL sends
//! matches what ReqHarbor sends: implied `Content-Type` headers are written
//! out (cURL would otherwise default to form-urlencoded), Bearer and API Key
//! auth become headers / query params, Basic auth becomes `-u`.

use super::shell::quote;
use crate::dynamic::DynamicCtx;
use crate::model::{Auth, AuthType, BodyType, FormFieldKind, Request};
use crate::prepare::{PrepareError, PreparedBody, effective_auth, form_urlencode, prepare_in};
use crate::vars::{ResolveOptions, Unresolved, VarScope};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportOptions {
    /// "Keep {{variables}}": export raw text without resolving (AC-CEXP-5).
    /// Dynamic variables (`{{$uuid}}`) stay literal too; otherwise they are
    /// generated once and reused everywhere in the command.
    pub keep_variables: bool,
}

/// Separator between command parts: ` \` + newline (+ 2-space indent).
pub const LINE_JOIN: &str = " \\\n  ";

fn needs_form_string(value: &str) -> bool {
    value.starts_with('@')
        || value.starts_with('<')
        || value.starts_with('"')
        || value.contains(';')
}

fn form_path(path: &str) -> String {
    if path.contains([';', ',', '"', '\\']) {
        format!("\"{}\"", path.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        path.to_string()
    }
}

/// Export `request` as a cURL command. `scope` resolves variables unless
/// `keep_variables` is set; `collection_auth` backs Auth "Inherit".
pub fn export_curl(
    request: &Request,
    scope: &VarScope,
    collection_auth: Option<&Auth>,
    opts: ExportOptions,
) -> Result<String, PrepareError> {
    let empty = VarScope::empty();
    let scope = if opts.keep_variables { &empty } else { scope };
    let dynamic = if opts.keep_variables {
        DynamicCtx::keep()
    } else {
        DynamicCtx::new()
    };
    let prepared = prepare_in(request, scope, collection_auth, "boundary", &dynamic)?;

    let explicit = |name: &str| {
        request
            .headers
            .iter()
            .any(|h| h.enabled && h.key.trim().eq_ignore_ascii_case(name))
    };
    let mut headers = prepared.headers.clone();
    let mut basic: Option<String> = None;
    if let Some(auth) = effective_auth(&request.auth, collection_auth)
        && auth.auth_type == AuthType::Basic
        && !explicit("authorization")
    {
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("authorization"));
        let mut u = Unresolved::default();
        let user = scope.resolve_in(
            auth.username.as_deref().unwrap_or(""),
            ResolveOptions::default(),
            &mut u,
            &dynamic,
        );
        let pass = scope.resolve_in(
            auth.password.as_deref().unwrap_or(""),
            ResolveOptions::default(),
            &mut u,
            &dynamic,
        );
        basic = Some(format!("{user}:{pass}"));
    }
    if matches!(
        request.body.body_type,
        BodyType::Form | BodyType::FormEncoded
    ) && !explicit("content-type")
    {
        // cURL sets these itself (and generates its own multipart boundary).
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-type"));
    }

    let mut body_parts: Vec<String> = Vec::new();
    match (&prepared.body, &request.body.body_type) {
        (None, _) => {}
        (Some(PreparedBody::File(path)), _) => {
            body_parts.push(format!("--data-binary {}", quote(&format!("@{path}"))));
        }
        (Some(PreparedBody::Multipart { parts, .. }), _) => {
            for p in parts {
                match &p.kind {
                    // Never prepared: such a request is not editable.
                    FormFieldKind::Other(_) => {}
                    FormFieldKind::File => {
                        let mut v = format!("{}=@{}", p.name, form_path(&p.value));
                        if let Some(ct) = &p.content_type {
                            v.push_str(";type=");
                            v.push_str(ct);
                        }
                        body_parts.push(format!("-F {}", quote(&v)));
                    }
                    FormFieldKind::Text => {
                        if needs_form_string(&p.value) {
                            body_parts.push(format!(
                                "--form-string {}",
                                quote(&format!("{}={}", p.name, p.value))
                            ));
                        } else {
                            let mut v = format!("{}={}", p.name, p.value);
                            if let Some(ct) = &p.content_type {
                                v.push_str(";type=");
                                v.push_str(ct);
                            }
                            body_parts.push(format!("-F {}", quote(&v)));
                        }
                    }
                }
            }
        }
        (Some(PreparedBody::Bytes(_)), BodyType::FormEncoded) => {
            let mut u = Unresolved::default();
            for f in request
                .body
                .form_encoded
                .as_deref()
                .unwrap_or_default()
                .iter()
                .filter(|f| f.enabled && !(f.key.is_empty() && f.value.is_empty()))
            {
                let key = scope.resolve_in(&f.key, ResolveOptions::default(), &mut u, &dynamic);
                let value = scope.resolve_in(&f.value, ResolveOptions::default(), &mut u, &dynamic);
                if key.is_empty() {
                    // `--data-urlencode '=v'` would drop the `=`; send it verbatim.
                    body_parts.push(format!(
                        "--data-raw {}",
                        quote(&format!("={}", form_urlencode(&value)))
                    ));
                } else {
                    body_parts.push(format!(
                        "--data-urlencode {}",
                        quote(&format!("{}={}", form_urlencode(&key), value))
                    ));
                }
            }
        }
        (Some(PreparedBody::Bytes(bytes)), _) => {
            body_parts.push(format!(
                "--data-raw {}",
                quote(&String::from_utf8_lossy(bytes))
            ));
        }
    }
    let has_body = !body_parts.is_empty();

    // AC-CEXP-2: `curl`, the method flag, the URL, then headers and body
    // flags — every part on its own line.
    let mut parts = vec!["curl".to_string()];
    match (prepared.method.as_str(), has_body) {
        ("GET", false) | ("POST", true) => {}
        ("HEAD", false) => parts.push("--head".into()),
        (m, _) => parts.push(format!("-X {m}")),
    }
    parts.push(quote(&prepared.url));
    if prepared.url.contains(['{', '}', '[', ']']) {
        parts.push("--globoff".into());
    }
    for (k, v) in &headers {
        if v.is_empty() {
            parts.push(format!("-H {}", quote(&format!("{k};"))));
        } else {
            parts.push(format!("-H {}", quote(&format!("{k}: {v}"))));
        }
    }
    if let Some(userpass) = basic {
        parts.push(format!("-u {}", quote(&userpass)));
    }
    parts.extend(body_parts);
    if !prepared.settings.verify_tls {
        parts.push("-k".into());
    }
    Ok(parts.join(LINE_JOIN))
}

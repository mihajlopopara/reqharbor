//! `Set-Cookie` parsing for the Cookies tab (AC-RES-5).

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    /// Raw `Expires` attribute (or `Max-Age=<n>` rendered as "Max-Age=<n>").
    pub expires: Option<String>,
    pub max_age: Option<i64>,
    pub http_only: bool,
    pub secure: bool,
    pub same_site: Option<String>,
}

/// Parse one `Set-Cookie` header value. Returns `None` when there is no
/// `name=value` pair.
pub fn parse_set_cookie(header: &str) -> Option<ResponseCookie> {
    let mut parts = header.split(';');
    let first = parts.next()?.trim();
    let eq = first.find('=')?;
    let name = first[..eq].trim();
    if name.is_empty() {
        return None;
    }
    let mut value = first[eq + 1..].trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value = &value[1..value.len() - 1];
    }
    let mut cookie = ResponseCookie {
        name: name.to_string(),
        value: value.to_string(),
        ..ResponseCookie::default()
    };
    for attr in parts {
        let attr = attr.trim();
        let (k, v) = match attr.find('=') {
            Some(i) => (attr[..i].trim(), Some(attr[i + 1..].trim())),
            None => (attr, None),
        };
        match k.to_ascii_lowercase().as_str() {
            "domain" => cookie.domain = v.map(|d| d.trim_start_matches('.').to_string()),
            "path" => cookie.path = v.map(str::to_string),
            "expires" => cookie.expires = v.map(str::to_string),
            "max-age" => cookie.max_age = v.and_then(|n| n.parse().ok()),
            "httponly" => cookie.http_only = true,
            "secure" => cookie.secure = true,
            "samesite" => cookie.same_site = v.map(str::to_string),
            _ => {}
        }
    }
    Some(cookie)
}

/// Parse every `Set-Cookie` header in received order.
pub fn parse_response_cookies(headers: &[(String, String)]) -> Vec<ResponseCookie> {
    headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .filter_map(|(_, v)| parse_set_cookie(v))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_res_5_parses_all_attributes() {
        let c = parse_set_cookie(
            "sid=abc123; Domain=.example.com; Path=/app; Expires=Wed, 21 Oct 2026 07:28:00 GMT; HttpOnly; Secure; SameSite=Lax",
        )
        .unwrap();
        assert_eq!(c.name, "sid");
        assert_eq!(c.value, "abc123");
        assert_eq!(c.domain.as_deref(), Some("example.com"));
        assert_eq!(c.path.as_deref(), Some("/app"));
        assert_eq!(c.expires.as_deref(), Some("Wed, 21 Oct 2026 07:28:00 GMT"));
        assert!(c.http_only && c.secure);
        assert_eq!(c.same_site.as_deref(), Some("Lax"));
    }

    #[test]
    fn ac_res_5_every_set_cookie_in_order() {
        let headers = vec![
            ("Set-Cookie".to_string(), "a=1".to_string()),
            ("content-type".to_string(), "text/plain".to_string()),
            ("set-cookie".to_string(), "b=\"2\"; Max-Age=60".to_string()),
            ("set-cookie".to_string(), "garbage".to_string()),
        ];
        let cookies = parse_response_cookies(&headers);
        assert_eq!(cookies.len(), 2);
        assert_eq!(cookies[0].name, "a");
        assert_eq!(cookies[1].value, "2");
        assert_eq!(cookies[1].max_age, Some(60));
        assert!(!cookies[1].http_only);
    }
}

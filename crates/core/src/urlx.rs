//! URL helpers: query string <-> params rows (AC-REQ-2) and default scheme
//! selection (AC-REQ-10).
//!
//! Query parsing is *raw*: values are not percent-decoded, so a URL typed by
//! the user round-trips through the params table without changes. When building
//! a query from rows, only characters that would change the structure of the
//! URL (space, `&`, `#`, `=` in keys, control and non-ASCII characters) are
//! percent-encoded; existing `%XX` sequences are left alone. This encoding is
//! idempotent.

use crate::model::KeyValue;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};

const QUERY_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'&')
    .add(b'#')
    .add(b'"')
    .add(b'<')
    .add(b'>');
const QUERY_KEY: &AsciiSet = &QUERY_VALUE.add(b'=');

/// Split a URL into `(base, query, fragment)`. `query` and `fragment` exclude
/// their leading `?` / `#`.
pub fn split_url(url: &str) -> (&str, Option<&str>, Option<&str>) {
    let (rest, fragment) = match url.find('#') {
        Some(i) => (&url[..i], Some(&url[i + 1..])),
        None => (url, None),
    };
    match rest.find('?') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..]), fragment),
        None => (rest, None, fragment),
    }
}

/// Parse a raw query string into enabled rows, preserving order and duplicates.
pub fn parse_query(query: &str) -> Vec<KeyValue> {
    query
        .split('&')
        .filter(|seg| !seg.is_empty())
        .map(|seg| match seg.find('=') {
            Some(i) => KeyValue::new(&seg[..i], &seg[i + 1..]),
            None => KeyValue::new(seg, ""),
        })
        .collect()
}

/// Percent-encode a query key (structure-breaking characters only).
pub fn encode_query_key(s: &str) -> String {
    utf8_percent_encode(s, QUERY_KEY).to_string()
}

/// Percent-encode a query value (structure-breaking characters only).
pub fn encode_query_value(s: &str) -> String {
    utf8_percent_encode(s, QUERY_VALUE).to_string()
}

/// Build a query string (without `?`) from the enabled rows.
pub fn build_query(params: &[KeyValue]) -> String {
    let mut out = String::new();
    for p in params.iter().filter(|p| p.enabled) {
        if p.key.is_empty() && p.value.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push('&');
        }
        out.push_str(&encode_query_key(&p.key));
        if !p.value.is_empty() {
            out.push('=');
            out.push_str(&encode_query_value(&p.value));
        }
    }
    out
}

/// Rewrite the URL's query string from the params rows (Query tab -> URL).
/// Disabled rows are not included (AC-REQ-2).
pub fn params_to_url(url: &str, params: &[KeyValue]) -> String {
    let (base, _, fragment) = split_url(url);
    let query = build_query(params);
    let mut out = base.to_string();
    if !query.is_empty() {
        out.push('?');
        out.push_str(&query);
    }
    if let Some(f) = fragment {
        out.push('#');
        out.push_str(f);
    }
    out
}

/// Derive params rows from the URL (URL -> Query tab). Disabled rows from
/// `existing` keep their position; enabled rows are replaced positionally by
/// the URL's query items, keeping descriptions when the key is unchanged.
pub fn url_to_params(url: &str, existing: &[KeyValue]) -> Vec<KeyValue> {
    let (_, query, _) = split_url(url);
    let fresh = query.map(parse_query).unwrap_or_default();
    let mut fresh_iter = fresh.into_iter();
    let mut out = Vec::with_capacity(existing.len());
    for row in existing {
        if !row.enabled {
            out.push(row.clone());
            continue;
        }
        if let Some(mut next) = fresh_iter.next() {
            if next.key == row.key {
                next.description = row.description.clone();
            }
            out.push(next);
        }
    }
    out.extend(fresh_iter);
    out
}

/// AC-REQ-10: a URL without a scheme is sent as `http://` for loopback hosts
/// and `https://` otherwise.
pub fn apply_default_scheme(url: &str) -> String {
    let trimmed = url.trim();
    // A leading variable usually carries the scheme (`{{baseUrl}}/users`).
    if has_scheme(trimmed) || trimmed.starts_with("{{") {
        return trimmed.to_string();
    }
    let rest = trimmed.trim_start_matches("//");
    let host = host_of(rest).to_ascii_lowercase();
    let scheme = if host == "localhost" || host == "127.0.0.1" || host == "[::1]" {
        "http"
    } else {
        "https"
    };
    format!("{scheme}://{rest}")
}

fn has_scheme(url: &str) -> bool {
    match url.find("://") {
        Some(i) => {
            let scheme = &url[..i];
            !scheme.is_empty()
                && scheme
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '-' || c == '.')
        }
        None => false,
    }
}

/// Host part (with brackets for IPv6, without port/userinfo) of a scheme-less URL.
fn host_of(rest: &str) -> &str {
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..end];
    let authority = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    if authority.starts_with('[') {
        match authority.find(']') {
            Some(i) => &authority[..=i],
            None => authority,
        }
    } else {
        match authority.find(':') {
            Some(i) => &authority[..i],
            None => authority,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ac_req_2_url_to_params_preserves_order_and_duplicates() {
        let rows = url_to_params("https://x.dev/a?b=1&a=2&b=3&flag", &[]);
        let pairs: Vec<_> = rows
            .iter()
            .map(|r| (r.key.as_str(), r.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![("b", "1"), ("a", "2"), ("b", "3"), ("flag", "")]
        );
    }

    #[test]
    fn ac_req_2_disabled_rows_preserved_and_not_in_url() {
        let existing = vec![
            KeyValue::new("a", "1"),
            KeyValue::new("off", "x").disabled(),
            KeyValue::new("b", "2"),
        ];
        let url = params_to_url("https://x.dev/p?old=1#frag", &existing);
        assert_eq!(url, "https://x.dev/p?a=1&b=2#frag");
        // User edits the URL: b's value changes, a new param appears.
        let rows = url_to_params("https://x.dev/p?a=1&b=9&c=3", &existing);
        let pairs: Vec<_> = rows
            .iter()
            .map(|r| (r.key.as_str(), r.value.as_str(), r.enabled))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("a", "1", true),
                ("off", "x", false),
                ("b", "9", true),
                ("c", "3", true)
            ]
        );
        // And back again is stable.
        assert_eq!(
            params_to_url("https://x.dev/p", &rows),
            "https://x.dev/p?a=1&b=9&c=3"
        );
    }

    #[test]
    fn ac_req_2_round_trip_is_lossless_for_raw_queries() {
        for url in [
            "https://x.dev/?q=a%20b&x=%26",
            "https://x.dev/?a=1&a=2&a=3",
            "http://localhost:8080/p?{{k}}={{v}}",
        ] {
            let rows = url_to_params(url, &[]);
            assert_eq!(params_to_url(url, &rows), url);
        }
    }

    #[test]
    fn query_encoding_is_idempotent() {
        let rows = vec![KeyValue::new("a b=", "x&y #z ü")];
        let once = params_to_url("https://h/", &rows);
        assert_eq!(once, "https://h/?a%20b%3D=x%26y%20%23z%20%C3%BC");
        let again = params_to_url("https://h/", &url_to_params(&once, &[]));
        assert_eq!(once, again);
    }

    #[test]
    fn ac_req_10_default_scheme() {
        assert_eq!(
            apply_default_scheme("localhost:3000/a"),
            "http://localhost:3000/a"
        );
        assert_eq!(apply_default_scheme("127.0.0.1/x"), "http://127.0.0.1/x");
        assert_eq!(apply_default_scheme("[::1]:8080/"), "http://[::1]:8080/");
        assert_eq!(
            apply_default_scheme("api.example.com/v1"),
            "https://api.example.com/v1"
        );
        assert_eq!(apply_default_scheme("LOCALHOST"), "http://LOCALHOST");
        assert_eq!(
            apply_default_scheme("user@localhost/x"),
            "http://user@localhost/x"
        );
        assert_eq!(
            apply_default_scheme("http://example.com"),
            "http://example.com"
        );
        assert_eq!(
            apply_default_scheme("https://localhost"),
            "https://localhost"
        );
        assert_eq!(
            apply_default_scheme("{{baseUrl}}/users"),
            "{{baseUrl}}/users"
        );
        assert_eq!(
            apply_default_scheme("example.com?a=http://b"),
            "https://example.com?a=http://b"
        );
    }
}

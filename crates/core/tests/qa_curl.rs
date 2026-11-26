//! QA round 1 — adversarial cURL import / export tests (PRODUCT §5.4, §5.5).
//!
//! Written by QA; not product code. Tests whose name starts with `bug_` are
//! EXPECTED TO FAIL until the referenced QA-R1-xxx bug is fixed — they are the
//! repro. Everything else is a passing regression check.
//!
//! Run: docker compose run --rm dev cargo test -p reqharbor-core --test qa_curl

use reqharbor_core::curl::{CurlError, ExportOptions, export_curl, parse_curl};
use reqharbor_core::model::*;
use reqharbor_core::prepare::{PreparedBody, prepare_with_boundary};
use reqharbor_core::vars::VarScope;

fn imp(s: &str) -> Request {
    parse_curl(s)
        .unwrap_or_else(|e| panic!("import failed: {e:?}\n{s}"))
        .request
}

fn header<'a>(r: &'a Request, name: &str) -> Option<&'a str> {
    r.headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
}

fn headers_named<'a>(r: &'a Request, name: &str) -> Vec<&'a str> {
    r.headers
        .iter()
        .filter(|h| h.key.eq_ignore_ascii_case(name))
        .map(|h| h.value.as_str())
        .collect()
}

fn export(r: &Request) -> String {
    export_curl(r, &VarScope::empty(), None, ExportOptions::default()).unwrap()
}

/// Wire-level equivalence (same definition as the devs' AC-CEXP-6 helper).
fn assert_equivalent(a: &Request, b: &Request) {
    let s = VarScope::empty();
    let pa = prepare_with_boundary(a, &s, None, "B").unwrap();
    let pb = prepare_with_boundary(b, &s, None, "B").unwrap();
    assert_eq!(pa.method, pb.method, "method");
    assert_eq!(pa.url, pb.url, "url");
    let mut ha = pa.headers.clone();
    let mut hb = pb.headers.clone();
    ha.sort();
    hb.sort();
    assert_eq!(ha, hb, "headers");
    assert_eq!(a.body.body_type, b.body.body_type, "body type");
    match (&pa.body, &pb.body) {
        (Some(PreparedBody::Bytes(x)), Some(PreparedBody::Bytes(y))) => {
            assert_eq!(
                String::from_utf8_lossy(x),
                String::from_utf8_lossy(y),
                "body"
            )
        }
        (x, y) => assert_eq!(x, y, "body"),
    }
    assert_eq!(pa.settings.verify_tls, pb.settings.verify_tls);
}

// ---------------------------------------------------------------- browsers

/// Chrome 131 "Copy as cURL (bash)": `!` forces `$'...'` with `!`,
/// `\'` inside, raw non-ASCII, `-b` cookie flag (Chrome >= 120 emits -b).
#[test]
fn chrome_bash_ansi_c_body_with_bang_quote_and_unicode() {
    let cmd = r#"curl 'https://api.example.com/v1/items?x=1&y=%20z' \
  -H 'accept: application/json, text/plain, */*' \
  -H 'accept-language: en-US,en;q=0.9,sr;q=0.8' \
  -b '_ga=GA1.1.123.456; session=abc%3D%3D; theme=dark' \
  -H 'content-type: application/json' \
  -H 'origin: https://app.example.com' \
  -H 'priority: u=1, i' \
  -H 'sec-ch-ua: "Chromium";v="131", "Not_A Brand";v="24"' \
  -H 'sec-ch-ua-mobile: ?0' \
  -H 'user-agent: Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36' \
  --data-raw $'{"name":"O\'Brien","msg":"hi!","city":"Beograd — Ђурђевдан 😀","nl":"a\\nb"}'"#;
    let r = imp(cmd);
    assert_eq!(r.method, "POST");
    assert_eq!(r.url, "https://api.example.com/v1/items?x=1&y=%20z");
    assert_eq!(
        header(&r, "cookie"),
        Some("_ga=GA1.1.123.456; session=abc%3D%3D; theme=dark")
    );
    assert_eq!(
        header(&r, "sec-ch-ua"),
        Some(r#""Chromium";v="131", "Not_A Brand";v="24""#)
    );
    assert_eq!(r.body.body_type, BodyType::Json);
    let text = r.body.text_str();
    let v: serde_json::Value = serde_json::from_str(text).expect("body must stay valid JSON");
    assert_eq!(v["name"], "O'Brien");
    assert_eq!(v["msg"], "hi!");
    assert_eq!(v["city"], "Beograd — Ђурђевдан 😀");
    assert_eq!(v["nl"], "a\nb");
}

/// Safari "Copy as cURL": `-X 'POST'` quoted, `--data-binary` literal JSON,
/// no line indentation.
#[test]
fn safari_copy_as_curl() {
    let cmd = "curl 'https://example.com/graphql' \\\n-X 'POST' \\\n-H 'Content-Type: application/json' \\\n-H 'Accept: */*' \\\n-H 'Cookie: sid=1' \\\n--data-binary '{\"query\":\"{ me { id } }\",\"variables\":{}}'";
    let r = imp(cmd);
    assert_eq!(r.method, "POST");
    assert_eq!(header(&r, "cookie"), Some("sid=1"));
    // A JSON object with only query/variables is detected as GraphQL.
    assert_eq!(r.body.body_type, BodyType::Graphql);
    assert_eq!(r.body.graphql.as_ref().unwrap().query, "{ me { id } }");
}

/// Firefox: explicit `-X POST`, `--compressed`, `-H 'Connection: keep-alive'`,
/// `--data-raw` urlencoded form.
#[test]
fn firefox_form_urlencoded() {
    let cmd = "curl 'https://shop.example/cart' --compressed -X POST -H 'User-Agent: Mozilla/5.0 (X11; Linux x86_64; rv:133.0) Gecko/20100101 Firefox/133.0' -H 'Content-Type: application/x-www-form-urlencoded' -H 'Connection: keep-alive' --data-raw 'item=42&qty=2&note=hello+world%21&empty='";
    let r = imp(cmd);
    assert_eq!(r.body.body_type, BodyType::FormEncoded);
    let rows = r.body.form_encoded.as_ref().unwrap();
    let pairs: Vec<(&str, &str)> = rows
        .iter()
        .map(|k| (k.key.as_str(), k.value.as_str()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("item", "42"),
            ("qty", "2"),
            ("note", "hello world!"),
            ("empty", "")
        ]
    );
    assert!(
        header(&r, "accept-encoding").is_none(),
        "--compressed must add no header"
    );
}

/// Windows Chrome "Copy as cURL (cmd)" uses `^` escapes. The dialect is out of
/// scope (PRODUCT §2.2) but the import must fail gracefully: a clear error,
/// never a request with a garbage URL like `^https://...^`.
#[test]
fn bug_qa_r1_010_windows_cmd_caret_style_imports_garbage_url() {
    let cmd = "curl ^\"https://api.example.com/v1/items^\" ^\n  -H ^\"accept: application/json^\" ^\n  --data-raw ^\"^{^\\^\"a^\\^\":1^}^\"";
    match parse_curl(cmd) {
        Err(_) => {}
        Ok(i) => {
            let has_warning = !i.warnings.is_empty() || !i.unknown_flags.is_empty();
            assert!(
                !i.request.url.contains('^'),
                "cmd-style import produced URL {:?} (warnings: {:?}) — expected an error such as \
                 'Windows cmd syntax is not supported'",
                i.request.url,
                i.warnings
            );
            assert!(has_warning);
        }
    }
}

#[test]
fn powershell_invoke_webrequest_is_not_curl() {
    let cmd = "Invoke-WebRequest -Uri \"https://x.dev\" -Method POST";
    assert_eq!(parse_curl(cmd).unwrap_err(), CurlError::NotCurl);
}

// ------------------------------------------------------------- data flags

#[test]
fn data_binary_at_file_is_binary_body_and_data_raw_at_is_literal() {
    let r = imp("curl https://x.dev/up --data-binary @/tmp/my file.bin");
    // unquoted space: second word is an extra positional (warning), path is /tmp/my
    assert_eq!(r.body.body_type, BodyType::Binary);
    assert_eq!(r.body.binary_path.as_deref(), Some("/tmp/my"));

    let r = imp("curl https://x.dev/up --data-binary '@/tmp/my file.bin'");
    assert_eq!(r.body.binary_path.as_deref(), Some("/tmp/my file.bin"));
    assert_eq!(r.method, "POST");

    let r = imp("curl https://x.dev/up -d @payload.json");
    assert_eq!(r.body.body_type, BodyType::Binary);

    let r = imp("curl https://x.dev/up --data-raw @payload.json");
    assert_ne!(r.body.body_type, BodyType::Binary);
    assert_eq!(r.body.text_str(), "@payload.json");

    let i = parse_curl("curl https://x.dev/up --data-binary @-").unwrap();
    assert!(!i.warnings.is_empty(), "stdin body should warn");
}

#[test]
fn repeated_d_joins_with_ampersand() {
    let r = imp("curl https://x.dev -d a=1 -d 'b=two words' --data c=3");
    assert_eq!(r.body.body_type, BodyType::FormEncoded);
    let rows = r.body.form_encoded.as_ref().unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[1].value, "two words");

    // Non-form pieces join literally like curl does.
    let r = imp(r#"curl https://x.dev -H 'Content-Type: text/plain' -d 'hello' -d 'world'"#);
    assert_eq!(r.body.text_str(), "hello&world");
}

#[test]
fn get_flag_moves_data_and_urlencode_to_query() {
    let r = imp(
        "curl -G 'https://x.dev/search?lang=en#top' -d q=rust --data-urlencode 'tag=a b&c' -d n=1",
    );
    assert_eq!(r.method, "GET");
    assert_eq!(r.body.body_type, BodyType::None);
    assert_eq!(
        r.url,
        "https://x.dev/search?lang=en&q=rust&tag=a%20b%26c&n=1#top"
    );
    let keys: Vec<&str> = r.params.iter().map(|p| p.key.as_str()).collect();
    assert_eq!(keys, vec!["lang", "q", "tag", "n"]);

    // -X overrides -G's GET, data still goes to the query.
    let r = imp("curl -G -X DELETE https://x.dev/item -d id=7");
    assert_eq!(r.method, "DELETE");
    assert_eq!(r.url, "https://x.dev/item?id=7");

    // --get long form and combined short flags
    let r = imp("curl -sSG https://x.dev/a -d k=v");
    assert_eq!(r.url, "https://x.dev/a?k=v");
    assert_eq!(r.method, "GET");
}

/// `--url-query` (curl 7.87+) appends to the query string like `-G -d`.
/// Not listed in the AC, but it is a common modern flag; currently it is
/// dropped into `unknownFlags` and the parameter is lost.
#[test]
fn bug_qa_r1_011_url_query_flag_is_dropped() {
    let i = parse_curl("curl https://x.dev/list --url-query 'page=2' --url-query 'q=a b'").unwrap();
    assert_eq!(
        i.request.url, "https://x.dev/list?page=2&q=a%20b",
        "unknown flags: {:?}",
        i.unknown_flags
    );
}

// ------------------------------------------------------------------ cookies

/// curl merges several `-b` values into one Cookie header; sending two Cookie
/// headers violates RFC 6265 §5.4 ("MUST NOT attach more than one Cookie
/// header field") and many servers only read the first.
#[test]
fn bug_qa_r1_012_multiple_cookie_sources_produce_duplicate_cookie_headers() {
    let r = imp("curl https://x.dev -b 'a=1' -b 'b=2' -H 'Cookie: c=3'");
    let cookies = headers_named(&r, "cookie");
    assert_eq!(
        cookies.len(),
        1,
        "expected one merged Cookie header, got {cookies:?}"
    );
}

#[test]
fn cookie_file_and_cookie_header_variants() {
    let i = parse_curl("curl https://x.dev -b cookies.txt -c jar.txt").unwrap();
    assert!(header(&i.request, "cookie").is_none());
    assert!(i.warnings.iter().any(|w| w.contains("cookies.txt")));
    let r = imp("curl https://x.dev --cookie='a=1; b=2'");
    assert_eq!(header(&r, "cookie"), Some("a=1; b=2"));
    let r = imp("curl https://x.dev -H 'cookie:a=1'");
    assert_eq!(header(&r, "cookie"), Some("a=1"));
}

// -------------------------------------------------------- errors, not panics

#[test]
fn missing_url_and_unbalanced_quotes_are_clear_errors() {
    assert_eq!(parse_curl("curl").unwrap_err(), CurlError::MissingUrl);
    assert_eq!(
        parse_curl("curl -X POST -d 'a=1'").unwrap_err(),
        CurlError::MissingUrl
    );
    assert_eq!(parse_curl("curl -H").unwrap_err(), CurlError::MissingUrl);
    for bad in [
        "curl 'https://x.dev",
        "curl \"https://x.dev",
        "curl https://x.dev -d $'abc",
        "curl https://x.dev -H 'A: b' -d '{\"a\": \"it's\"}'",
    ] {
        match parse_curl(bad) {
            Err(CurlError::Syntax(m)) => assert!(m.to_lowercase().contains("quote"), "{m}"),
            other => panic!("{bad}: expected syntax error, got {other:?}"),
        }
    }
    assert_eq!(parse_curl("   ").unwrap_err(), CurlError::NotCurl);
    assert_eq!(
        parse_curl("wget https://x").unwrap_err(),
        CurlError::NotCurl
    );
    assert_eq!(
        parse_curl("curly https://x").unwrap_err(),
        CurlError::NotCurl
    );
}

#[test]
fn nasty_inputs_never_panic() {
    let long_header = format!("curl https://x.dev -H 'X: {}'", "é".repeat(50_000));
    let inputs: Vec<String> = vec![
        "curl -".into(),
        "curl --".into(),
        "curl -- -not-a-flag".into(),
        "curl -X".into(),
        "curl https://x -m".into(),
        "curl https://x -m -1".into(),
        "curl https://x -m NaN".into(),
        "curl https://x -m 1e400".into(),
        "curl https://x -é".into(),
        "curl https://x -H ':'".into(),
        "curl https://x -H ';'".into(),
        "curl https://x -F".into(),
        "curl https://x -F '=@'".into(),
        "curl https://x -F 'a=@\"unterminated'".into(),
        "curl https://x --data-urlencode '@'".into(),
        "curl https://x --data-urlencode '='".into(),
        "curl https://x -u".into(),
        "curl $'\\x'".into(),
        "curl $'\\u'".into(),
        "curl $'\\U110000'".into(),
        "curl $'\\c'".into(),
        "curl $'\\777'".into(),
        "curl \\".into(),
        "curl \u{0}".into(),
        "curl https://x -H 'Content-Type: multipart/form-data; boundary=' -d 'x'".into(),
        "curl https://x -H 'Content-Type: multipart/form-data; boundary=\"\"' -d '--\"\"'".into(),
        "curl https://x -H 'Content-Type: MULTIPART/FORM-DATA; BOUNDARY=İİİ' -d 'x'".into(),
        "\u{feff}curl https://x".into(),
        long_header,
    ];
    for s in inputs {
        let _ = parse_curl(&s);
    }
}

/// A BOM or a zero-width char at the start (common when copying from docs or
/// Slack) should not turn a valid command into "Not a cURL command".
#[test]
fn bug_qa_r1_016_leading_bom_is_not_curl() {
    let r = parse_curl("\u{feff}curl https://x.dev/a");
    assert!(r.is_ok(), "{r:?}");
}

#[test]
fn line_continuation_variants() {
    let single = imp("curl https://x.dev -H 'A: b' -d 'k=v'");
    // backslash followed by trailing spaces before the newline (copy from docs)
    let trailing = imp("curl https://x.dev \\\n  -H 'A: b' \\\r\n  -d 'k=v'");
    assert_eq!(single.headers, trailing.headers);
    assert_eq!(single.body, trailing.body);
}

#[test]
fn header_edge_cases() {
    let r = imp(
        "curl https://x.dev -H 'X-Empty;' -H 'Host:example.com' -H 'X-Time: 12:30:00' -H 'X-Sp:   padded   '",
    );
    assert_eq!(header(&r, "x-empty"), Some(""));
    assert_eq!(header(&r, "host"), Some("example.com"));
    assert_eq!(header(&r, "x-time"), Some("12:30:00"));
    assert_eq!(header(&r, "x-sp"), Some("padded"));
}

/// Lower-case `-X post` imports a method the editor rejects on send
/// (AC-REQ-1 regex `^[A-Z][A-Z0-9_-]*$`): the user gets "Invalid method" only
/// after clicking Send. Import should upper-case standard methods or warn.
#[test]
fn bug_qa_r1_013_lowercase_method_imports_unsendable_request() {
    let i = parse_curl("curl -X post https://x.dev -d a=1").unwrap();
    let sendable = is_valid_method(&i.request.method);
    assert!(
        sendable || !i.warnings.is_empty(),
        "method {:?} imported silently but fails validation on send",
        i.request.method
    );
}

#[test]
fn user_flag_variants() {
    let r = imp("curl -u 'user:pa:ss' https://x.dev");
    assert_eq!(r.auth.username.as_deref(), Some("user"));
    assert_eq!(r.auth.password.as_deref(), Some("pa:ss"));
    let r = imp("curl --user=':tok' https://x.dev");
    assert_eq!(r.auth.username.as_deref(), Some(""));
    assert_eq!(r.auth.password.as_deref(), Some("tok"));
}

#[test]
fn form_with_quoted_path_semicolon_and_type() {
    let r = imp(
        r#"curl https://x.dev -F 'doc=@"/tmp/a;b.txt";type=text/plain' -F 'name=Ђ' -F 'raw=<notes.txt'"#,
    );
    assert_eq!(r.body.body_type, BodyType::Form);
    let f = r.body.form.as_ref().unwrap();
    assert_eq!(f[0].kind, FormFieldKind::File);
    assert_eq!(f[0].value, "/tmp/a;b.txt");
    assert_eq!(f[0].content_type.as_deref(), Some("text/plain"));
    assert_eq!(f[1].value, "Ђ");
}

#[test]
fn ac_curl_3_url_flag_and_unicode_url() {
    let r = imp("curl --url 'https://例え.jp/パス?q=日本語' -XQUERY --json '{}'");
    assert_eq!(r.url, "https://例え.jp/パス?q=日本語");
    assert_eq!(r.method, "QUERY");
}

/// `-m 0` / `--max-time 0`: curl treats 0 as "no limit"? ReqHarbor stores
/// timeoutMs = 0, which the engine turns into a 1 ms timeout (every send
/// times out). See also qa_http::bug_qa_r1_018.
#[test]
fn max_time_zero_imports_timeout_zero() {
    let r = imp("curl -m 0 https://x.dev");
    assert_eq!(r.settings.timeout_ms, 0);
}

// ----------------------------------------------------- export -> import

#[test]
fn round_trip_weird_headers_and_bodies() {
    let mut reqs: Vec<Request> = Vec::new();
    let base = |m: &str| Request {
        method: m.into(),
        url: "https://api.example.com/v1/it's?q=a%20b&x=[1]".into(),
        ..Request::default()
    };
    // header values with quotes, dollar signs, backslashes, unicode, empty
    let mut r = base("PATCH");
    r.headers = vec![
        KeyValue::new("X-Quote", r#"it's "quoted" $HOME `id` \n"#),
        KeyValue::new("X-Uni", "Ђурђевдан 😀"),
        KeyValue::new("X-Empty", ""),
        KeyValue::new("X-Dup", "1"),
        KeyValue::new("X-Dup", "2"),
        KeyValue::new("X-Off", "no").disabled(),
    ];
    r.body = Body::text_of(
        BodyType::Text,
        "line1\nline2\r\n\ttab 'q' \"dq\" $(rm -rf /) !bang",
    );
    reqs.push(r);

    let mut r = base("QUERY");
    r.body = Body::json("{\n  \"sql\": \"select * from t where a = 'x'\"\n}");
    reqs.push(r);

    let mut r = base("PUT");
    r.body = Body::text_of(BodyType::Xml, "<a b='1'>&amp;</a>");
    reqs.push(r);

    let mut r = base("POST");
    r.body = Body::form_encoded(vec![
        KeyValue::new("a b", "c&d=e"),
        KeyValue::new("emoji", "😀"),
        KeyValue::new("at", "@notafile"),
        KeyValue::new("eq", "="),
    ]);
    reqs.push(r);

    let mut r = base("POST");
    r.body = Body::form(vec![
        FormField::text("t", "@literal"),
        FormField::text("semi", "a;type=x"),
        FormField::file("f", "/tmp/dir with space/it's;\"x\".bin"),
    ]);
    reqs.push(r);

    let mut r = base("POST");
    r.body = Body::binary("/tmp/it's a file.bin");
    reqs.push(r);

    let mut r = base("POST");
    r.body = Body::graphql(
        "query Q($id: ID!) { node(id: $id) { id } }",
        "{\"id\": \"1\"}",
    );
    reqs.push(r);

    for r in reqs {
        let cmd = export(&r);
        let back = parse_curl(&cmd).unwrap_or_else(|e| panic!("{e:?}\n{cmd}"));
        assert!(
            back.unknown_flags.is_empty(),
            "{:?}\n{cmd}",
            back.unknown_flags
        );
        assert_equivalent(&r, &back.request);
    }
}

/// AC-CEXP-6: a JSON-typed body that happens to look like a GraphQL payload
/// (`{"query": ...}`) comes back as body type GraphQL.
#[test]
fn bug_qa_r1_014_json_body_with_query_key_round_trips_as_graphql() {
    let mut r = Request {
        method: "POST".into(),
        url: "https://search.example/_search".into(),
        ..Request::default()
    };
    r.body = Body::json(r#"{"query": "status:open"}"#);
    let back = imp(&export(&r));
    assert_eq!(
        back.body.body_type,
        BodyType::Json,
        "export:\n{}",
        export(&r)
    );
}

/// AC-CEXP-6: a Form-encode row with an empty key exports as
/// `--data-urlencode '=v'`, which re-imports as a Text body.
#[test]
fn bug_qa_r1_015_form_encoded_row_with_empty_key_round_trips_as_text() {
    let mut r = Request {
        method: "POST".into(),
        url: "https://x.dev/a".into(),
        ..Request::default()
    };
    r.body = Body::form_encoded(vec![KeyValue::new("", "value")]);
    let back = imp(&export(&r));
    assert_eq!(
        back.body.body_type,
        BodyType::FormEncoded,
        "export:\n{}",
        export(&r)
    );
}

#[test]
fn round_trip_head_query_and_custom_methods_without_body() {
    for m in ["HEAD", "QUERY", "OPTIONS", "PURGE", "X-CUSTOM_1", "DELETE"] {
        let r = Request {
            method: m.into(),
            url: "https://x.dev/a".into(),
            ..Request::default()
        };
        let back = imp(&export(&r));
        assert_equivalent(&r, &back);
    }
}

/// Performance budget PRODUCT §7: cURL parse <= 1 ms for a 10 KB command —
/// check a pathological 10 KB command made of many tiny flags too.
#[test]
fn parse_10kb_of_tiny_flags_within_budget() {
    let mut cmd = String::from("curl https://x.dev");
    while cmd.len() < 10 * 1024 {
        cmd.push_str(" -H 'a: b' -d x");
    }
    let _ = parse_curl(&cmd).unwrap();
    let start = std::time::Instant::now();
    for _ in 0..20 {
        let _ = parse_curl(&cmd).unwrap();
    }
    let per = start.elapsed() / 20;
    // debug build: allow 10x the release budget
    assert!(per.as_millis() <= 10, "{per:?}");
}

//! cURL import and export (PRODUCT §5.4, §5.5).

mod export;
mod import;
pub mod shell;

pub use export::{ExportOptions, LINE_JOIN, export_curl};
pub use import::{CurlError, CurlImport, looks_like_curl, parse_curl};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;
    use crate::prepare::{PreparedBody, prepare_with_boundary};
    use crate::vars::VarScope;

    fn imp(s: &str) -> Request {
        parse_curl(s).unwrap().request
    }

    fn header<'a>(r: &'a Request, name: &str) -> Option<&'a str> {
        r.headers
            .iter()
            .find(|h| h.key.eq_ignore_ascii_case(name))
            .map(|h| h.value.as_str())
    }

    fn fixture(name: &str) -> String {
        let path = format!("{}/tests/fixtures/curl/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).unwrap()
    }

    // ---------- import ----------

    #[test]
    fn ac_curl_1_multiline_equals_single_line() {
        let single = "curl -X PUT 'https://x.dev/a' -H 'A: 1' --data-raw '{\"k\":1}'";
        let multi = "curl -X PUT 'https://x.dev/a' \\\n  -H 'A: 1' \\\n  --data-raw '{\"k\":1}'";
        let crlf = multi.replace('\n', "\r\n");
        let mut a = imp(single);
        let mut b = imp(multi);
        let mut c = imp(&crlf);
        a.id.clear();
        b.id.clear();
        c.id.clear();
        assert_eq!(a, b);
        assert_eq!(a, c);
    }

    #[test]
    fn ac_curl_2_quoting_styles() {
        let r = imp(
            r#"curl https://x.dev -H "X-A: \"q\" \\ \$HOME" -H $'X-B: l1\tt \'s\' \\' -H 'X-C: it'\''s'"#,
        );
        assert_eq!(header(&r, "X-A"), Some(r#""q" \ $HOME"#));
        assert_eq!(header(&r, "X-B"), Some("l1\tt 's' \\"));
        assert_eq!(header(&r, "X-C"), Some("it's"));
        let r = imp("curl https://x.dev --data-raw $'a\\nb'");
        assert_eq!(r.body.text_str(), "a\nb");
    }

    #[test]
    fn ac_curl_3_url_and_method() {
        let r = imp("curl -X QUERY https://x.dev/search");
        assert_eq!(r.method, "QUERY");
        assert_eq!(r.url, "https://x.dev/search");
        let r = imp("curl --url 'https://x.dev/a?b=1' --request PURGE");
        assert_eq!(r.method, "PURGE");
        assert_eq!(r.url, "https://x.dev/a?b=1");
        assert_eq!(r.params, vec![KeyValue::new("b", "1")]);
        assert_eq!(imp("curl https://x.dev").method, "GET");
        assert_eq!(
            parse_curl("curl -H 'A: b'").unwrap_err(),
            CurlError::MissingUrl
        );
    }

    #[test]
    fn ac_curl_4_headers() {
        let r = imp("curl https://x.dev -H 'X-A:nospace' -H 'X-Empty;' --header 'X-B:   padded  '");
        assert_eq!(header(&r, "X-A"), Some("nospace"));
        assert_eq!(header(&r, "X-Empty"), Some(""));
        assert_eq!(header(&r, "X-B"), Some("padded"));
        assert_eq!(r.headers[0].key, "X-A");
    }

    #[test]
    fn ac_curl_5_data_flags() {
        for flag in [
            "-d",
            "--data",
            "--data-ascii",
            "--data-raw",
            "--data-binary",
        ] {
            let r = imp(&format!("curl https://x.dev {flag} 'hello world'"));
            assert_eq!(r.method, "POST", "{flag}");
            assert_eq!(r.body.text_str(), "hello world", "{flag}");
            assert_eq!(r.body.body_type, BodyType::Text);
        }
        let r = imp("curl https://x.dev -d a=1 -d b=2");
        assert_eq!(r.body.body_type, BodyType::FormEncoded);
        assert_eq!(
            r.body.form_encoded.unwrap(),
            vec![KeyValue::new("a", "1"), KeyValue::new("b", "2")]
        );
        assert_eq!(imp("curl -X PUT https://x.dev -d x").method, "PUT");
        let r = imp("curl https://x.dev -d @payload.json");
        assert_eq!(r.body, Body::binary("payload.json"));
        let r = imp("curl https://x.dev --data-binary @/tmp/a.bin");
        assert_eq!(r.body, Body::binary("/tmp/a.bin"));
        let r = imp("curl https://x.dev --data-raw @x");
        assert_eq!(r.body.text_str(), "@x");
        assert_eq!(r.body.body_type, BodyType::Text);
    }

    #[test]
    fn ac_curl_6_data_urlencode() {
        let r =
            imp("curl https://x.dev --data-urlencode 'q=a b&c' --data-urlencode 'n=é' -d raw=1");
        assert_eq!(r.body.body_type, BodyType::FormEncoded);
        assert_eq!(
            r.body.form_encoded.unwrap(),
            vec![
                KeyValue::new("q", "a b&c"),
                KeyValue::new("n", "é"),
                KeyValue::new("raw", "1")
            ]
        );
        let r = imp("curl https://x.dev -G --data-urlencode 'q=a b'");
        assert_eq!(r.url, "https://x.dev?q=a%20b");
    }

    #[test]
    fn ac_curl_7_body_type_detection() {
        let r = imp("curl https://x.dev -H 'Content-Type: application/vnd.api+json' -d 'not json'");
        assert_eq!(r.body.body_type, BodyType::Json);
        let r = imp("curl https://x.dev -d '{\"a\":1}'");
        assert_eq!(r.body.body_type, BodyType::Json);
        let r = imp("curl https://x.dev -H 'content-type: text/xml' -d '<a/>'");
        assert_eq!(r.body.body_type, BodyType::Xml);
        let r = imp(
            "curl https://x.dev -H 'Content-Type: application/x-www-form-urlencoded' -d 'a=1&b=x%20y'",
        );
        assert_eq!(r.body.body_type, BodyType::FormEncoded);
        assert_eq!(
            r.body.form_encoded.as_ref().unwrap()[1],
            KeyValue::new("b", "x y")
        );
        let r = imp("curl https://x.dev -d 'k=v&k2=v2'");
        assert_eq!(r.body.body_type, BodyType::FormEncoded);
        let r = imp("curl https://x.dev -d 'just some text'");
        assert_eq!(r.body.body_type, BodyType::Text);
        let r = imp("curl https://x.dev -H 'Content-Type: text/csv' -d 'a=1'");
        assert_eq!(r.body.body_type, BodyType::Text);
        // GraphQL-shaped JSON becomes a GraphQL body
        let r = imp(
            r#"curl https://x.dev/graphql -H 'content-type: application/json' --data-raw '{"query":"{ me { id } }","variables":{"a":1}}'"#,
        );
        assert_eq!(r.body.body_type, BodyType::Graphql);
        assert_eq!(r.body.graphql.as_ref().unwrap().query, "{ me { id } }");
    }

    #[test]
    fn ac_curl_8_json_flag() {
        let r = imp(r#"curl https://x.dev --json '{"a":1}'"#);
        assert_eq!(r.method, "POST");
        assert_eq!(r.body, Body::json(r#"{"a":1}"#));
        assert_eq!(header(&r, "Content-Type"), Some("application/json"));
        assert_eq!(header(&r, "Accept"), Some("application/json"));
        assert_eq!(
            imp(r#"curl -X QUERY https://x.dev --json '{}'"#).method,
            "QUERY"
        );
    }

    #[test]
    fn ac_curl_9_basic_user() {
        assert_eq!(
            imp("curl -u bob:s3:cr https://x.dev").auth,
            Auth::basic("bob", "s3:cr")
        );
        assert_eq!(
            imp("curl --user bob https://x.dev").auth,
            Auth::basic("bob", "")
        );
    }

    #[test]
    fn ac_curl_10_form_fields() {
        let r = imp(
            "curl https://x.dev -F name=value -F 'doc=@/tmp/a b.pdf;type=application/pdf' -F 'pic=@\"x;y.png\"'",
        );
        assert_eq!(r.method, "POST");
        assert_eq!(r.body.body_type, BodyType::Form);
        let form = r.body.form.unwrap();
        assert_eq!(form[0], FormField::text("name", "value"));
        assert_eq!(form[1].kind, FormFieldKind::File);
        assert_eq!(form[1].value, "/tmp/a b.pdf");
        assert_eq!(form[1].content_type.as_deref(), Some("application/pdf"));
        assert_eq!(form[2].value, "x;y.png");
        let r = imp("curl https://x.dev --form-string 'k=@literal;x'");
        assert_eq!(r.body.form.unwrap()[0], FormField::text("k", "@literal;x"));
    }

    #[test]
    fn ac_curl_11_compressed_is_silent() {
        let out = parse_curl("curl --compressed https://x.dev").unwrap();
        assert!(out.request.headers.is_empty());
        assert!(out.unknown_flags.is_empty());
        assert!(out.warnings.is_empty());
    }

    #[test]
    fn ac_curl_12_get_moves_data_to_query() {
        let r = imp("curl -G https://x.dev/s?x=0 -d a=1 -d b=2");
        assert_eq!(r.method, "GET");
        assert_eq!(r.url, "https://x.dev/s?x=0&a=1&b=2");
        assert_eq!(r.body.body_type, BodyType::None);
        assert_eq!(r.params.len(), 3);
        assert_eq!(
            imp("curl --get -X QUERY https://x.dev -d a=1").method,
            "QUERY"
        );
    }

    #[test]
    fn ac_curl_13_misc_flags() {
        let r = imp("curl -k -L -A 'UA/1' -b 'a=1; b=2' -e https://ref.dev -m 2.5 https://x.dev");
        assert!(!r.settings.verify_tls);
        assert!(r.settings.follow_redirects);
        assert_eq!(header(&r, "User-Agent"), Some("UA/1"));
        assert_eq!(header(&r, "Cookie"), Some("a=1; b=2"));
        assert_eq!(header(&r, "Referer"), Some("https://ref.dev"));
        assert_eq!(r.settings.timeout_ms, 2500);
        assert_eq!(imp("curl -I https://x.dev").method, "HEAD");
        assert_eq!(imp("curl --head https://x.dev").method, "HEAD");
        assert_eq!(
            imp("curl --insecure --location --max-time 3 https://x.dev")
                .settings
                .timeout_ms,
            3000
        );
    }

    #[test]
    fn ac_curl_14_combined_and_equals_forms() {
        let out = parse_curl("curl -sSLk -XPOST -HX-A:1 --data=a=b --header=X-B:2 -o /dev/null -v -i --progress-bar https://x.dev").unwrap();
        let r = out.request;
        assert!(out.unknown_flags.is_empty(), "{:?}", out.unknown_flags);
        assert!(out.warnings.is_empty(), "{:?}", out.warnings);
        assert_eq!(r.method, "POST");
        assert!(!r.settings.verify_tls);
        assert_eq!(header(&r, "X-A"), Some("1"));
        assert_eq!(header(&r, "X-B"), Some("2"));
        assert_eq!(r.body.form_encoded.unwrap(), vec![KeyValue::new("a", "b")]);
        assert_eq!(r.url, "https://x.dev");
        // unknown flags are listed
        let out = parse_curl("curl --frobnicate -Z9 --proxy http://p:1 https://x.dev").unwrap();
        assert!(out.unknown_flags.contains(&"--frobnicate".to_string()));
        assert!(out.unknown_flags.contains(&"--proxy".to_string()));
        assert_eq!(out.request.url, "https://x.dev");
    }

    #[test]
    fn ac_curl_15_bearer_header_stays_header() {
        let r = imp("curl https://x.dev -H 'Authorization: Bearer x'");
        assert_eq!(r.auth.auth_type, AuthType::None);
        assert_eq!(header(&r, "Authorization"), Some("Bearer x"));
    }

    #[test]
    fn ac_curl_16_chrome_and_firefox_fixtures() {
        for name in ["chrome_get.txt", "firefox_get.txt"] {
            let r = imp(&fixture(name));
            assert_eq!(r.method, "GET", "{name}");
            assert!(
                r.url.starts_with("https://api.example.com/v1/users?page=2"),
                "{name}"
            );
            assert_eq!(r.params[0], KeyValue::new("page", "2"));
            assert!(
                header(&r, "Cookie").unwrap().contains("session=abc123"),
                "{name}"
            );
            assert_eq!(r.body.body_type, BodyType::None);
        }
        let chrome = imp(&fixture("chrome_get.txt"));
        assert_eq!(
            header(&chrome, "sec-ch-ua"),
            Some(r#""Chromium";v="131", "Not_A Brand";v="24""#)
        );
        assert_eq!(
            header(&chrome, "authorization"),
            Some("Bearer eyJhbGciOiJIUzI1NiJ9.e30.abc")
        );

        for name in ["chrome_post_json.txt", "firefox_post_json.txt"] {
            let out = parse_curl(&fixture(name)).unwrap();
            assert!(
                out.unknown_flags.is_empty(),
                "{name}: {:?}",
                out.unknown_flags
            );
            let r = out.request;
            assert_eq!(r.method, "POST", "{name}");
            assert_eq!(r.body.body_type, BodyType::Json, "{name}");
            let v: serde_json::Value = serde_json::from_str(r.body.text_str()).unwrap();
            assert_eq!(v["qty"], 2);
        }
        let chrome = imp(&fixture("chrome_post_json.txt"));
        let v: serde_json::Value = serde_json::from_str(chrome.body.text_str()).unwrap();
        assert_eq!(v["item"], "Bob's widget");
        assert_eq!(v["note"], "line1\nline2");

        for name in ["chrome_multipart.txt", "firefox_multipart.txt"] {
            let out = parse_curl(&fixture(name)).unwrap();
            let r = out.request;
            assert_eq!(r.method, "POST", "{name}");
            assert_eq!(r.body.body_type, BodyType::Form, "{name}");
            let form = r.body.form.clone().unwrap();
            assert_eq!(form.len(), 2, "{name}");
            assert_eq!(form[0], FormField::text("title", "Quarterly report"));
            assert_eq!(form[1].kind, FormFieldKind::File);
            assert_eq!(form[1].value, "report.pdf");
            assert_eq!(form[1].content_type.as_deref(), Some("application/pdf"));
            // stale boundary header removed; a new one is generated on send
            assert_eq!(header(&r, "content-type"), None, "{name}");
            assert_eq!(out.warnings.len(), 1);
        }
    }

    #[test]
    fn ac_curl_17_not_a_curl_command() {
        for s in [
            "",
            "   ",
            "wget https://x.dev",
            "https://x.dev",
            "curly https://x",
            "GET /",
        ] {
            assert_eq!(parse_curl(s).unwrap_err(), CurlError::NotCurl, "{s:?}");
        }
        assert!(looks_like_curl("curl https://x"));
        assert!(looks_like_curl("  $ curl -X GET x"));
        assert!(!looks_like_curl("https://curl.se"));
        assert!(parse_curl("/usr/bin/curl https://x.dev").is_ok());
    }

    #[test]
    fn parse_curl_10kb_under_1ms_budget() {
        let mut cmd = String::from("curl 'https://x.dev/a' \\\n");
        while cmd.len() < 10 * 1024 {
            cmd.push_str("  -H 'X-Header-Name: some header value with words' \\\n");
        }
        cmd.push_str("  --data-raw '{\"a\":1}'");
        // warm up, then measure the average of many runs
        parse_curl(&cmd).unwrap();
        let runs = 50;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            parse_curl(&cmd).unwrap();
        }
        let per = start.elapsed() / runs;
        // Budget is 1 ms on release builds; allow headroom for debug builds.
        let budget = if cfg!(debug_assertions) { 10 } else { 1 };
        assert!(per.as_millis() < budget, "parse took {per:?}");
    }

    // ---------- export ----------

    fn export(r: &Request) -> String {
        export_curl(r, &VarScope::empty(), None, ExportOptions::default()).unwrap()
    }

    #[test]
    fn ac_cexp_2_layout() {
        let mut r = Request {
            method: "PUT".into(),
            url: "https://x.dev/a".into(),
            headers: vec![
                KeyValue::new("A", "1"),
                KeyValue::new("Off", "x").disabled(),
                KeyValue::new("B", "2"),
            ],
            body: Body::json("{}"),
            ..Request::default()
        };
        assert_eq!(
            export(&r),
            "curl \\\n  -X PUT \\\n  'https://x.dev/a' \\\n  -H 'A: 1' \\\n  -H 'B: 2' \\\n  -H 'Content-Type: application/json' \\\n  --data-raw '{}'"
        );
        r.method = "POST".into();
        assert!(export(&r).starts_with("curl \\\n  'https://x.dev/a' \\\n"));
        r.method = "GET".into();
        r.body = Body::none();
        r.headers.clear();
        assert_eq!(export(&r), "curl \\\n  'https://x.dev/a'");
        r.method = "POST".into();
        assert_eq!(export(&r), "curl \\\n  -X POST \\\n  'https://x.dev/a'");
    }

    #[test]
    fn ac_cexp_3_single_quote_escaping() {
        let r = Request {
            url: "https://x.dev/it's".into(),
            headers: vec![KeyValue::new("X-Q", "a'b")],
            ..Request::default()
        };
        assert_eq!(
            export(&r),
            "curl \\\n  'https://x.dev/it'\\''s' \\\n  -H 'X-Q: a'\\''b'"
        );
    }

    #[test]
    fn ac_cexp_4_body_flags_and_auth() {
        let base = Request {
            method: "POST".into(),
            url: "https://x.dev".into(),
            ..Request::default()
        };
        let with = |body: Body, auth: Auth| {
            let mut r = base.clone();
            r.body = body;
            r.auth = auth;
            export(&r)
        };
        assert!(
            with(Body::text_of(BodyType::Xml, "<a/>"), Auth::none()).contains("--data-raw '<a/>'")
        );
        assert!(
            with(Body::graphql("{a}", ""), Auth::none())
                .contains(r#"--data-raw '{"query":"{a}","variables":{}}'"#)
        );
        let fe = with(
            Body::form_encoded(vec![KeyValue::new("a b", "c d"), KeyValue::new("x", "1")]),
            Auth::none(),
        );
        assert!(fe.contains("--data-urlencode 'a%20b=c d'"));
        assert!(fe.contains("--data-urlencode 'x=1'"));
        assert!(!fe.contains("Content-Type"));
        let mut f = FormField::file("doc", "/tmp/a.pdf");
        f.content_type = Some("application/pdf".into());
        let form = with(Body::form(vec![FormField::text("t", "v"), f]), Auth::none());
        assert!(form.contains("-F 't=v'"));
        assert!(form.contains("-F 'doc=@/tmp/a.pdf;type=application/pdf'"));
        assert!(!form.contains("Content-Type"));
        assert!(
            with(Body::binary("/tmp/b.bin"), Auth::none()).contains("--data-binary '@/tmp/b.bin'")
        );
        assert!(with(Body::none(), Auth::basic("u", "p")).contains("-u 'u:p'"));
        assert!(with(Body::none(), Auth::bearer("t")).contains("-H 'Authorization: Bearer t'"));
        assert!(
            with(
                Body::none(),
                Auth::api_key("X-K", "v", ApiKeyLocation::Header)
            )
            .contains("-H 'X-K: v'")
        );
        assert!(
            with(Body::none(), Auth::api_key("k", "v", ApiKeyLocation::Query))
                .contains("'https://x.dev?k=v'")
        );
        let mut r = base.clone();
        r.settings.verify_tls = false;
        assert!(export(&r).ends_with(" \\\n  -k"));
    }

    #[test]
    fn ac_qry_5_query_export_and_import() {
        let r = Request {
            method: "QUERY".into(),
            url: "https://x.dev/search".into(),
            body: Body::json(r#"{"q":"x"}"#),
            ..Request::default()
        };
        let out = export(&r);
        assert!(out.starts_with("curl \\\n  -X QUERY \\\n  'https://x.dev/search'"));
        assert!(out.contains(r#"--data-raw '{"q":"x"}'"#));
        let back = imp(&out);
        assert_eq!(back.method, "QUERY");
        assert_eq!(back.body, Body::json(r#"{"q":"x"}"#));
    }

    #[test]
    fn ac_cexp_5_variables_resolved_or_kept() {
        let env = Environment {
            id: "e".into(),
            workspace_id: "w".into(),
            project_id: Some("p".into()),
            name: "Dev".into(),
            variables: vec![
                Variable::new("host", "api.dev"),
                Variable::new("tok", "SECRET").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let scope = VarScope::new(Some(&env), None);
        let r = Request {
            url: "https://{{host}}/a".into(),
            auth: Auth::bearer("{{tok}}"),
            ..Request::default()
        };
        let resolved = export_curl(&r, &scope, None, ExportOptions::default()).unwrap();
        assert!(resolved.contains("'https://api.dev/a'"));
        assert!(resolved.contains("Bearer SECRET"));
        let kept = export_curl(
            &r,
            &scope,
            None,
            ExportOptions {
                keep_variables: true,
            },
        )
        .unwrap();
        assert!(kept.contains("'https://{{host}}/a'"));
        assert!(kept.contains("Bearer {{tok}}"));
        assert!(kept.contains("--globoff"));
        assert!(!kept.contains("SECRET"));
    }

    #[test]
    fn export_dynamic_variables_kept_or_generated_once() {
        let r = Request {
            method: "POST".into(),
            url: "https://x.dev/u/{{$uuid}}".into(),
            auth: Auth::basic("{{$uuid}}", "pw"),
            body: Body {
                body_type: BodyType::FormEncoded,
                form_encoded: Some(vec![KeyValue::new("id", "{{$uuid}}")]),
                ..Body::none()
            },
            ..Request::default()
        };
        let kept = export_curl(
            &r,
            &VarScope::empty(),
            None,
            ExportOptions {
                keep_variables: true,
            },
        )
        .unwrap();
        assert!(kept.contains("'https://x.dev/u/{{$uuid}}'"), "{kept}");
        assert!(kept.contains("-u '{{$uuid}}:pw'"), "{kept}");
        assert!(kept.contains("--data-urlencode 'id={{$uuid}}'"), "{kept}");

        let made = export(&r);
        assert!(!made.contains("{{"), "{made}");
        let id = made
            .split("https://x.dev/u/")
            .nth(1)
            .and_then(|s| s.split('\'').next())
            .unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{made}");
        // the extra resolves (Basic -u, form fields) reuse the same value
        assert!(made.contains(&format!("-u '{id}:pw'")), "{made}");
        assert!(
            made.contains(&format!("--data-urlencode 'id={id}'")),
            "{made}"
        );
    }

    #[test]
    fn ac_cexp_6_round_trip_examples() {
        let reqs = vec![
            Request {
                method: "HEAD".into(),
                url: "https://x.dev".into(),
                ..Request::default()
            },
            Request {
                method: "GET".into(),
                url: "https://x.dev".into(),
                body: Body::text_of(BodyType::Text, "get with body"),
                ..Request::default()
            },
        ];
        for r in reqs {
            assert_equivalent(&r, &imp(&export(&r)));
        }
    }

    /// Wire-level equivalence used by AC-CEXP-6.
    pub(crate) fn assert_equivalent(a: &Request, b: &Request) {
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

    mod props {
        use super::*;
        use proptest::prelude::*;

        fn text() -> impl Strategy<Value = String> {
            "[\\PC\n]{0,16}"
        }

        fn method() -> impl Strategy<Value = String> {
            prop_oneof![
                Just("GET"),
                Just("POST"),
                Just("PUT"),
                Just("PATCH"),
                Just("DELETE"),
                Just("QUERY"),
                Just("HEAD"),
                Just("OPTIONS"),
                Just("PURGE"),
                Just("X-CUSTOM_1"),
            ]
            .prop_map(str::to_string)
        }

        fn params() -> impl Strategy<Value = Vec<KeyValue>> {
            prop::collection::vec(
                ("[a-zA-Z0-9_]{1,6}", "[ -~]{0,8}", any::<bool>()).prop_map(|(k, v, on)| {
                    KeyValue {
                        enabled: on,
                        ..KeyValue::new(k, v)
                    }
                }),
                0..4,
            )
        }

        fn headers() -> impl Strategy<Value = Vec<KeyValue>> {
            prop::collection::vec(
                (
                    "X-[A-Za-z0-9-]{1,10}",
                    prop_oneof!["", "[!-~]([ -~]{0,10}[!-~])?"],
                    any::<bool>(),
                )
                    .prop_map(|(k, v, on)| KeyValue {
                        enabled: on,
                        ..KeyValue::new(k, v)
                    }),
                0..4,
            )
        }

        fn auth() -> impl Strategy<Value = Auth> {
            prop_oneof![
                Just(Auth::none()),
                ("[a-zA-Z0-9_]{0,6}", "[ -~]{0,8}").prop_map(|(u, p)| Auth::basic(u, p)),
                "[!-~]{1,12}".prop_map(Auth::bearer),
                ("Api-[A-Za-z]{1,4}", "[!-~]{1,8}").prop_map(|(k, v)| Auth::api_key(
                    k,
                    v,
                    ApiKeyLocation::Header
                )),
                ("[a-z]{1,5}", "[ -~]{0,6}").prop_map(|(k, v)| Auth::api_key(
                    k,
                    v,
                    ApiKeyLocation::Query
                )),
            ]
        }

        fn body() -> impl Strategy<Value = Body> {
            let fe_row =
                ("[\\PC]{1,6}", "[\\PC]{0,8}", any::<bool>()).prop_map(|(k, v, on)| KeyValue {
                    enabled: on,
                    ..KeyValue::new(k, v)
                });
            let form_row = prop_oneof![
                ("[a-zA-Z0-9_]{1,6}", "[\\PC]{0,8}").prop_map(|(k, v)| FormField::text(k, v)),
                (
                    "[a-zA-Z0-9_]{1,6}",
                    "/[a-zA-Z0-9_. ;,-]{1,12}",
                    prop::option::of(prop_oneof![Just("text/plain"), Just("image/png")])
                )
                    .prop_map(|(k, p, ct)| FormField {
                        content_type: ct.map(str::to_string),
                        ..FormField::file(k, p)
                    }),
            ];
            prop_oneof![
                Just(Body::none()),
                text().prop_map(Body::json),
                text().prop_map(|t| Body::text_of(BodyType::Xml, t)),
                text().prop_map(|t| Body::text_of(BodyType::Text, t)),
                (fe_row.clone(), prop::collection::vec(fe_row, 0..3)).prop_map(
                    |(mut first, rest)| {
                        first.enabled = true;
                        let mut rows = vec![first];
                        rows.extend(rest);
                        Body::form_encoded(rows)
                    }
                ),
                (form_row.clone(), prop::collection::vec(form_row, 0..3)).prop_map(
                    |(first, rest)| {
                        let mut rows = vec![first];
                        rows.extend(rest);
                        Body::form(rows)
                    }
                ),
                (
                    text(),
                    prop_oneof![
                        Just(""),
                        Just("{\"a\": 1}"),
                        Just("{\"b\": [true, null, \"x'y\"]}")
                    ]
                )
                    .prop_map(|(q, v)| Body::graphql(q, v)),
                "/[a-zA-Z0-9_. '-]{1,12}".prop_map(Body::binary),
            ]
        }

        fn request() -> impl Strategy<Value = Request> {
            (
                method(),
                "https://[a-z]{1,8}\\.(com|dev)(/[a-zA-Z0-9_.~-]{1,6}){0,3}",
                params(),
                headers(),
                auth(),
                body(),
                any::<bool>(),
            )
                .prop_map(|(method, url, params, headers, auth, body, verify)| {
                    let mut r = Request {
                        method,
                        url,
                        params,
                        headers,
                        auth,
                        body,
                        ..Request::default()
                    };
                    r.settings.verify_tls = verify;
                    r
                })
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(512))]

            /// AC-CEXP-6: export -> import yields an equivalent request for
            /// every body type.
            #[test]
            fn ac_cexp_6_round_trip(r in request()) {
                let cmd = export(&r);
                let back = parse_curl(&cmd).unwrap();
                prop_assert!(back.unknown_flags.is_empty(), "{:?}", back.unknown_flags);
                assert_equivalent(&r, &back.request);
            }

            /// The shell quoting used by export always round-trips.
            #[test]
            fn quote_round_trip(s in "\\PC*") {
                let words = shell::split_words(&shell::quote(&s)).unwrap();
                prop_assert_eq!(words.len(), 1);
                prop_assert_eq!(&words[0].text, &s);
            }

            /// Import never panics on arbitrary input.
            #[test]
            fn import_never_panics(s in "(curl )?[\\PC\n\\\\'\"$ -]{0,80}") {
                let _ = parse_curl(&s);
            }
        }
    }
}

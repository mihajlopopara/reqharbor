//! GraphQL schema fetching against a local mock server (AC-GQL-2..4).

use reqharbor_core::CancellationToken;
use reqharbor_core::graphql::{
    GraphqlError, SchemaCache, SchemaFormat, SchemaSource, fetch_schema, schema_url,
};
use reqharbor_core::http::HttpEngine;
use reqharbor_core::model::*;
use reqharbor_core::vars::VarScope;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SCHEMA: &str = r#"{"data":{"__schema":{"queryType":{"name":"Query"},"mutationType":null,"subscriptionType":null,"types":[{"kind":"OBJECT","name":"Query","fields":[]}],"directives":[]}}}"#;

fn gql(url: String) -> Request {
    let mut r = Request {
        method: "GET".into(),
        url,
        headers: vec![KeyValue::new("X-Tenant", "acme")],
        auth: Auth::bearer("t0k"),
        ..Request::default()
    };
    r.body = Body::graphql("{ me { id } }", "{\"a\": 1}");
    r
}

async fn fetch(r: &Request) -> Result<reqharbor_core::graphql::GraphqlSchema, GraphqlError> {
    let engine = HttpEngine::new().unwrap();
    fetch_schema(
        &engine,
        r,
        &VarScope::empty(),
        None,
        &CancellationToken::new(),
    )
    .await
}

#[tokio::test]
async fn ac_gql_2_fetch_sends_introspection_with_the_requests_headers_and_auth() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .and(header("authorization", "Bearer t0k"))
        .and(header("x-tenant", "acme"))
        .and(header("content-type", "application/json"))
        .and(body_partial_json(
            serde_json::json!({"operationName": "IntrospectionQuery", "variables": {}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(SCHEMA))
        .expect(1)
        .mount(&server)
        .await;
    let r = gql(format!("{}/graphql#x", server.uri()));
    let s = fetch(&r).await.unwrap();
    assert_eq!(s.url, format!("{}/graphql", server.uri()));
    assert_eq!(s.source, SchemaSource::Introspection);
    assert_eq!(s.format, SchemaFormat::Introspection);
    assert!(s.schema.starts_with(r#"{"__schema":"#));
    assert_eq!(s.size_bytes, s.schema.len() as u64);
    assert_eq!(
        schema_url(&r, &VarScope::empty(), None).unwrap(),
        s.url,
        "the cache key is known without a send"
    );

    // The cache round-trips it.
    let dir = tempfile::tempdir().unwrap();
    let cache = SchemaCache::new(dir.path());
    cache.put(&s).unwrap();
    assert_eq!(cache.get(&s.url).unwrap(), s);
}

#[tokio::test]
async fn ac_gql_3_disabled_failed_and_network_errors() {
    let server = MockServer::start().await;
    Mock::given(path("/off"))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            r#"{"errors":[{"message":"GraphQL introspection is not allowed by Apollo Server"}]}"#,
        ))
        .mount(&server)
        .await;
    Mock::given(path("/missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not here"))
        .mount(&server)
        .await;
    match fetch(&gql(format!("{}/off", server.uri()))).await {
        Err(GraphqlError::IntrospectionDisabled(m)) => assert!(m.contains("not allowed")),
        other => panic!("{other:?}"),
    }
    match fetch(&gql(format!("{}/missing", server.uri()))).await {
        Err(GraphqlError::IntrospectionFailed { message, status }) => {
            assert_eq!(status, Some(404));
            assert!(message.starts_with("The server answered 404"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    // Nothing listens on port 1.
    match fetch(&gql("http://127.0.0.1:1/graphql".into())).await {
        Err(GraphqlError::Network(e)) => assert_eq!(e.kind, ErrorKind::Connect),
        other => panic!("{other:?}"),
    }
    // Validation problems block like a send.
    let mut empty = gql(String::new());
    empty.url = "  ".into();
    assert!(matches!(
        fetch(&empty).await,
        Err(GraphqlError::Prepare(
            reqharbor_core::prepare::PrepareError::EmptyUrl
        ))
    ));
}

#[tokio::test]
async fn ac_gql_2_introspection_honours_timeout_tls_and_cancel() {
    // Timeout from the request's settings.
    let server = MockServer::start().await;
    Mock::given(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(SCHEMA)
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let mut r = gql(format!("{}/slow", server.uri()));
    r.settings.timeout_ms = 200;
    match fetch(&r).await {
        Err(GraphqlError::Network(e)) => assert_eq!(e.kind, ErrorKind::Timeout),
        other => panic!("{other:?}"),
    }

    // Cancel token.
    let engine = HttpEngine::new().unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        c2.cancel();
    });
    r.settings.timeout_ms = 0;
    match fetch_schema(&engine, &r, &VarScope::empty(), None, &cancel).await {
        Err(GraphqlError::Network(e)) => assert_eq!(e.kind, ErrorKind::Cancelled),
        other => panic!("{other:?}"),
    }

    // Verify TLS: a self-signed server fails, and works with verifyTls: false.
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    reqharbor_core::http::install_crypto_provider();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let der = cert.cert.der().clone();
    let key: rustls::pki_types::PrivateKeyDer<'static> =
        rustls::pki_types::PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into();
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![der], key)
        .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(cfg));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((tcp, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                if let Ok(mut tls) = acceptor.accept(tcp).await {
                    let mut buf = vec![0u8; 65536];
                    let _ = tls.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SCHEMA}",
                        SCHEMA.len()
                    );
                    let _ = tls.write_all(resp.as_bytes()).await;
                    let _ = tls.shutdown().await;
                }
            });
        }
    });
    let mut tls = gql(format!("https://localhost:{port}/graphql"));
    match fetch(&tls).await {
        Err(GraphqlError::Network(e)) => assert_eq!(e.kind, ErrorKind::Tls),
        other => panic!("{other:?}"),
    }
    tls.settings.verify_tls = false;
    assert!(fetch(&tls).await.unwrap().schema.contains("__schema"));
}

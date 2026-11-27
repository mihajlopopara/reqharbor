//! GraphQL schema commands (AC-GQL-2..5): fetch by introspection, the local
//! schema cache keyed by resolved URL, and schema files. Thin wrappers over
//! `reqharbor_core::graphql`; documented in docs/API_CONTRACT.md §3.15.
//!
//! The UI fetches and loads with `store: false`, builds the schema with
//! graphql-js and only then commits it with `graphql_store_schema`, so a
//! schema it can't use never replaces a good cached one.

use crate::error::{CmdResult, CommandError};
use crate::state::AppState;
use reqharbor_core::graphql::{self, GraphqlSchema, SCHEMA_MAX_BYTES, SchemaCache};
use reqharbor_core::model::{Auth, Request};
use reqharbor_core::prepare::PrepareError;
use reqharbor_core::vars::VarScope;
use serde::{Deserialize, Serialize};
use tauri::State;

type S<'a> = State<'a, AppState>;

/// Variables, collection auth and the schema cache for `request`, like
/// `send_request` resolves them. A stored request from a newer ReqHarbor is
/// refused (AC-FWD-3/4).
fn context(
    state: &AppState,
    request: &Request,
    project_id: Option<String>,
) -> CmdResult<(VarScope, Option<Auth>, SchemaCache)> {
    let store = state.store();
    if store.request(&request.id).is_some_and(|r| !r.is_editable()) {
        return Err(PrepareError::NeedsNewerVersion.into());
    }
    let pid = project_id.or_else(|| {
        store
            .collection(&request.collection_id)
            .map(|c| c.project_id.clone())
    });
    let auth = store
        .collection(&request.collection_id)
        .map(|c| c.auth.clone());
    Ok((
        store.var_scope(pid.as_deref()),
        auth,
        SchemaCache::new(store.dir()),
    ))
}

/// The schema the UI already shows, so an unchanged cache entry isn't sent
/// over IPC again.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnownSchema {
    pub url: String,
    pub fetched_at: i64,
}

/// `graphql_cached_schema` result.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedSchemaLookup {
    /// The request's schema URL (cache key).
    pub url: String,
    /// The cached schema is the `known` one: `schema` is left out.
    pub unchanged: bool,
    pub schema: Option<GraphqlSchema>,
}

pub async fn fetch_schema_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    execution_id: Option<String>,
    store: bool,
) -> CmdResult<GraphqlSchema> {
    let (scope, auth, cache) = context(state, &request, project_id)?;
    let id = execution_id.unwrap_or_else(reqharbor_core::model::new_id);
    let handle = state.inflight.register(&id);
    let result = graphql::fetch_schema(
        &state.engine,
        &request,
        &scope,
        auth.as_ref(),
        &handle.token,
    )
    .await;
    state.inflight.finish(&id, &handle);
    let schema = result?;
    if store {
        cache.put(&schema)?;
    }
    Ok(schema)
}

pub fn cached_schema_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    known: Option<KnownSchema>,
) -> CmdResult<CachedSchemaLookup> {
    let (scope, auth, cache) = context(state, &request, project_id)?;
    let url = graphql::schema_url(&request, &scope, auth.as_ref())?;
    let schema = cache.get(&url);
    let unchanged = matches!((&schema, &known), (Some(s), Some(k)) if s.url == k.url && s.fetched_at == k.fetched_at);
    Ok(CachedSchemaLookup {
        url,
        unchanged,
        schema: if unchanged { None } else { schema },
    })
}

/// The schema URL a file loads for, and the cache (needs the store lock).
fn load_target(
    state: &AppState,
    request: &Request,
    project_id: Option<String>,
) -> CmdResult<(String, SchemaCache)> {
    let (scope, auth, cache) = context(state, request, project_id)?;
    Ok((graphql::schema_url(request, &scope, auth.as_ref())?, cache))
}

/// Read (and optionally cache) a schema file: blocking file I/O.
fn read_schema_file(
    url: &str,
    cache: &SchemaCache,
    path: Option<String>,
    text: Option<String>,
    file_name: Option<String>,
    store: bool,
) -> CmdResult<GraphqlSchema> {
    let schema = graphql::load_schema_file(
        path.as_deref().map(std::path::Path::new),
        text,
        file_name,
        url,
    )?;
    if store {
        cache.put(&schema)?;
    }
    Ok(schema)
}

/// `graphql_load_schema_file` without the blocking-pool hop (tests).
#[cfg(test)]
pub fn load_schema_file_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    path: Option<String>,
    text: Option<String>,
    file_name: Option<String>,
    store: bool,
) -> CmdResult<GraphqlSchema> {
    let (url, cache) = load_target(state, &request, project_id)?;
    read_schema_file(&url, &cache, path, text, file_name, store)
}

/// Cache a schema the UI built. It goes under the URL core computed when it
/// was fetched or loaded (already redacted); if the request's schema URL has
/// changed since (another environment or host variable), nothing is cached,
/// so one endpoint's schema never lands under another's key. Returns the
/// schema either way.
pub fn store_schema_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
    schema: GraphqlSchema,
) -> CmdResult<GraphqlSchema> {
    let (scope, auth, cache) = context(state, &request, project_id)?;
    let now = graphql::schema_url(&request, &scope, auth.as_ref())?;
    if schema.schema.trim().is_empty() {
        return Err(CommandError::new(
            "graphql_schema_invalid",
            "This file isn't a GraphQL schema: it is empty",
        ));
    }
    if schema.schema.len() > SCHEMA_MAX_BYTES {
        return Err(graphql::GraphqlError::TooLarge(schema.schema.len() as u64).into());
    }
    let schema = GraphqlSchema {
        size_bytes: schema.schema.len() as u64,
        ..schema
    };
    if schema.url != now {
        return Ok(schema);
    }
    cache.put(&schema)?;
    Ok(schema)
}

pub fn clear_schema_in(
    state: &AppState,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<bool> {
    let (scope, auth, cache) = context(state, &request, project_id)?;
    let url = graphql::schema_url(&request, &scope, auth.as_ref())?;
    Ok(cache.remove(&url)?)
}

/// Fetch the schema behind `request` with the introspection query; cache it
/// for the request's schema URL unless `store` is false. `executionId`
/// makes it cancellable with `cancel_request`. Not recorded in history.
#[tauri::command]
pub async fn graphql_fetch_schema(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    execution_id: Option<String>,
    store: Option<bool>,
) -> CmdResult<GraphqlSchema> {
    fetch_schema_in(
        &state,
        request,
        project_id,
        execution_id,
        store.unwrap_or(true),
    )
    .await
}

/// The cached schema for `request`'s schema URL. Never sends.
#[tauri::command]
pub fn graphql_cached_schema(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    known: Option<KnownSchema>,
) -> CmdResult<CachedSchemaLookup> {
    cached_schema_in(&state, request, project_id, known)
}

/// Load a schema file (SDL or introspection JSON) for `request`'s schema URL;
/// cache it unless `store` is false. `text` (browser mode) wins over `path`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn graphql_load_schema_file(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    path: Option<String>,
    text: Option<String>,
    file_name: Option<String>,
    store: Option<bool>,
) -> CmdResult<GraphqlSchema> {
    let (url, cache) = load_target(&state, &request, project_id)?;
    tauri::async_runtime::spawn_blocking(move || {
        read_schema_file(&url, &cache, path, text, file_name, store.unwrap_or(true))
    })
    .await
    .map_err(|e| CommandError::new("internal", e.to_string()))?
}

/// Cache a schema the UI has checked (fetched or loaded with `store: false`)
/// under `request`'s schema URL.
#[tauri::command]
pub fn graphql_store_schema(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
    schema: GraphqlSchema,
) -> CmdResult<GraphqlSchema> {
    store_schema_in(&state, request, project_id, schema)
}

/// Forget the cached schema for `request`'s schema URL. True when there
/// was one.
#[tauri::command]
pub fn graphql_clear_schema(
    state: S<'_>,
    request: Request,
    project_id: Option<String>,
) -> CmdResult<bool> {
    clear_schema_in(&state, request, project_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqharbor_core::model::Body;
    use reqharbor_core::storage::Store;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const SCHEMA: &str = r#"{"data":{"__schema":{"queryType":{"name":"Query"},"types":[{"kind":"OBJECT","name":"Query","fields":[]}],"directives":[]}}}"#;

    fn state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        (dir, AppState::new(store).unwrap())
    }

    /// A one-route HTTP server answering every request with `SCHEMA` after
    /// `delay_ms`; returns its URL.
    async fn server(delay_ms: u64) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut tcp, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let _ = tcp.read(&mut buf).await;
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SCHEMA}",
                        SCHEMA.len()
                    );
                    let _ = tcp.write_all(resp.as_bytes()).await;
                    let _ = tcp.shutdown().await;
                });
            }
        });
        format!("http://127.0.0.1:{port}/graphql")
    }

    fn gql(url: &str) -> Request {
        let mut r = Request {
            id: "scratch".into(),
            method: "POST".into(),
            url: url.into(),
            ..Request::default()
        };
        r.body = Body::graphql("{ a }", "");
        r
    }

    fn history_len(s: &AppState) -> usize {
        s.store().history.len()
    }

    #[tokio::test]
    async fn fetch_caches_unless_store_false_and_never_writes_history() {
        let (_dir, s) = state();
        let url = server(0).await;
        let r = gql(&url);
        let before = history_len(&s);
        let unstored = fetch_schema_in(&s, r.clone(), None, None, false)
            .await
            .unwrap();
        assert_eq!(unstored.url, url);
        let lookup = cached_schema_in(&s, r.clone(), None, None).unwrap();
        assert_eq!(lookup.url, url);
        assert!(lookup.schema.is_none(), "store: false caches nothing");

        let stored = store_schema_in(&s, r.clone(), None, unstored.clone()).unwrap();
        assert_eq!(stored.url, url);
        let lookup = cached_schema_in(&s, r.clone(), None, None).unwrap();
        assert_eq!(lookup.schema.as_ref().unwrap().schema, unstored.schema);
        // The known schema isn't sent again.
        let known = KnownSchema {
            url: url.clone(),
            fetched_at: stored.fetched_at,
        };
        let again = cached_schema_in(&s, r.clone(), None, Some(known)).unwrap();
        assert!(again.unchanged && again.schema.is_none());

        fetch_schema_in(&s, r.clone(), None, None, true)
            .await
            .unwrap();
        assert_eq!(history_len(&s), before, "introspection is not a send");
        assert!(clear_schema_in(&s, r.clone(), None).unwrap());
        assert!(
            cached_schema_in(&s, r, None, None)
                .unwrap()
                .schema
                .is_none()
        );
    }

    #[tokio::test]
    async fn fetch_is_cancellable_with_its_execution_id() {
        let (_dir, s) = state();
        let url = server(5_000).await;
        let s = std::sync::Arc::new(s);
        let s2 = s.clone();
        let task = tokio::spawn(async move {
            fetch_schema_in(&s2, gql(&url), None, Some("ex-1".into()), true).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(s.inflight.cancel("ex-1"));
        let err = task.await.unwrap().unwrap_err();
        assert_eq!(err.code, "graphql_network");
        assert_eq!(err.details.unwrap()["kind"], "cancelled");
    }

    #[test]
    fn load_and_store_validate_and_refuse_newer_requests() {
        let (_dir, s) = state();
        let r = gql("https://api.dev/graphql");
        let f = load_schema_file_in(
            &s,
            r.clone(),
            None,
            None,
            Some("type Query { a: Int }".into()),
            Some("a.graphql".into()),
            false,
        )
        .unwrap();
        assert!(
            cached_schema_in(&s, r.clone(), None, None)
                .unwrap()
                .schema
                .is_none()
        );
        let bad = load_schema_file_in(&s, r.clone(), None, None, Some("hello".into()), None, true)
            .unwrap_err();
        assert_eq!(bad.code, "graphql_schema_invalid");
        // The schema URL changed since the load (another environment / host): returned, not cached.
        let moved = Request {
            url: "https://prod.dev/graphql".into(),
            ..r.clone()
        };
        let kept = store_schema_in(&s, moved.clone(), None, f.clone()).unwrap();
        assert_eq!(kept.url, "https://api.dev/graphql");
        assert!(
            cached_schema_in(&s, moved, None, None)
                .unwrap()
                .schema
                .is_none()
        );
        assert!(
            cached_schema_in(&s, r.clone(), None, None)
                .unwrap()
                .schema
                .is_none()
        );
        let stored = store_schema_in(&s, r.clone(), None, f).unwrap();
        assert_eq!(stored.url, "https://api.dev/graphql");
        assert!(
            cached_schema_in(&s, r.clone(), None, None)
                .unwrap()
                .schema
                .is_some()
        );
        let empty = GraphqlSchema {
            schema: " ".into(),
            ..stored
        };
        assert_eq!(
            store_schema_in(&s, r.clone(), None, empty)
                .unwrap_err()
                .code,
            "graphql_schema_invalid"
        );
        let blank = Request {
            url: " ".into(),
            ..r
        };
        assert_eq!(
            cached_schema_in(&s, blank, None, None).unwrap_err().code,
            "validation"
        );
    }
}

//! QA round 1 — OpenAPI import with realistic public-style specs and hostile
//! documents (AC-OAS-1..9, AC-QRY-6).
//!
//! Fixtures live in /qa/fixtures/openapi (modelled on the public Swagger
//! Petstore 3.0, Petstore Swagger 2.0 and OAI 3.1 webhook examples; the
//! sandbox could not download the originals).
//!
//! Tests whose name starts with `bug_` are EXPECTED TO FAIL until the
//! referenced QA-R1-xxx bug is fixed — they are the repro.
//!
//! Run: docker compose run --rm dev cargo test -p reqharbor-core --test qa_openapi
//! Perf (AC-OAS-9 is a release budget):
//!      docker compose run --rm dev cargo test --release -p reqharbor-core --test qa_openapi perf_

use reqharbor_core::model::*;
use reqharbor_core::openapi::{OpenApiError, OpenApiImport, SpecKind, import_openapi};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

fn fixture(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../qa/fixtures/openapi")
        .join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

fn import(text: &str) -> OpenApiImport {
    import_openapi(text, "ws", "proj").unwrap_or_else(|e| panic!("import failed: {e}"))
}

fn by_op<'a>(imp: &'a OpenApiImport, name: &str) -> &'a Request {
    imp.requests
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| {
            panic!(
                "no request {name:?}; have {:?}",
                imp.requests.iter().map(|r| &r.name).collect::<Vec<_>>()
            )
        })
}

fn var<'a>(imp: &'a OpenApiImport, key: &str) -> Option<&'a str> {
    imp.environment
        .variables
        .iter()
        .find(|v| v.key == key)
        .map(|v| v.value.as_str())
}

#[test]
fn petstore3_openapi30_yaml() {
    let imp = import(&fixture("petstore3_v30.yaml"));
    assert_eq!(imp.report.spec.kind, SpecKind::OpenApi3);
    assert_eq!(imp.collection.name, "Swagger Petstore - OpenAPI 3.0");
    assert_eq!(imp.report.operations_created, 19);
    let folders: Vec<_> = imp
        .report
        .folders
        .iter()
        .map(|f| (f.name.as_str(), f.request_count))
        .collect();
    assert_eq!(folders, vec![("pet", 8), ("store", 4), ("user", 7)]);
    // relative server URL kept with a warning
    assert_eq!(imp.report.base_url, "/api/v3");
    assert!(!imp.report.warnings.is_empty());

    let r = by_op(&imp, "Find pet by ID.");
    assert_eq!(r.url, "{{baseUrl}}/pet/{{petId}}");
    assert!(var(&imp, "petId").is_some());

    let r = by_op(&imp, "Finds Pets by status.");
    assert_eq!(r.params[0].key, "status");
    assert_eq!(r.params[0].value, "available");
    assert!(
        !r.params[0].enabled,
        "optional query param must be disabled"
    );

    let r = by_op(&imp, "Add a new pet to the store.");
    assert_eq!(r.method, "POST");
    assert_eq!(r.body.body_type, BodyType::Json);
    let body: Value = serde_json::from_str(r.body.text_str()).unwrap();
    assert_eq!(body["name"], "doggie");
    assert_eq!(body["category"]["name"], "Dogs");

    let r = by_op(&imp, "Uploads an image.");
    assert_eq!(r.body.body_type, BodyType::Binary);

    let r = by_op(&imp, "Deletes a pet.");
    assert!(r.headers.iter().any(|h| h.key == "api_key" && !h.enabled));

    let r = by_op(&imp, "Creates list of users with given input array.");
    let body: Value = serde_json::from_str(r.body.text_str()).unwrap();
    assert_eq!(body[0]["username"], "theUser");
}

#[test]
fn petstore_swagger2_json() {
    let imp = import(&fixture("petstore_swagger2.json"));
    assert_eq!(imp.report.spec.kind, SpecKind::Swagger2);
    assert_eq!(imp.report.base_url, "https://petstore.swagger.io/v2");
    assert_eq!(var(&imp, "baseUrl"), Some("https://petstore.swagger.io/v2"));

    let up = by_op(&imp, "uploads an image");
    assert_eq!(up.body.body_type, BodyType::Form);
    let f = up.body.form.as_ref().unwrap();
    assert!(
        f.iter()
            .any(|x| x.key == "file" && x.kind == FormFieldKind::File)
    );
    assert!(
        f.iter()
            .any(|x| x.key == "additionalMetadata" && x.kind == FormFieldKind::Text)
    );

    let upd = by_op(&imp, "Updates a pet in the store with form data");
    assert_eq!(upd.body.body_type, BodyType::FormEncoded);

    let add = by_op(&imp, "Add a new pet to the store");
    assert_eq!(add.body.body_type, BodyType::Json);
    let body: Value = serde_json::from_str(add.body.text_str()).unwrap();
    assert_eq!(body["name"], "doggie");

    let login = by_op(&imp, "Logs user into the system");
    assert!(
        login.params.iter().all(|p| p.enabled),
        "required params enabled"
    );

    // array query param with enum default
    let st = by_op(&imp, "Finds Pets by status");
    assert_eq!(st.params[0].key, "status");
    assert!(st.params[0].enabled);
}

#[test]
fn openapi31_webhooks_server_variables_and_dotted_path_params() {
    let imp = import(&fixture("webhooks_v31.yaml"));
    assert_eq!(imp.report.base_url, "https://eu-west-1.api.example.com/v2");
    // webhooks are not callable endpoints: only the 2 paths are imported
    assert_eq!(imp.report.operations_created, 2);
    let r = by_op(&imp, "Fetch a pet file 🐶");
    assert_eq!(
        r.url.split('?').next().unwrap(),
        "{{baseUrl}}/pets/{{petId}}/files/{{name}}.{{ext}}"
    );
    assert_eq!(r.params[0].value, "2024-01-01T00:00:00Z");
    let c = by_op(&imp, "createPet");
    let body: Value = serde_json::from_str(c.body.text_str()).unwrap();
    assert_eq!(body["id"], 42);
    assert_eq!(body["name"], "Rex");
    // owner.pets[0] is a cycle back to Pet => {}
    assert!(body["owner"]["pets"].is_array());
}

/// YAML versions written without quotes are YAML floats.
#[test]
fn unquoted_numeric_versions() {
    for (doc, kind) in [
        (
            "openapi: 3.0\ninfo: {title: t}\npaths: {}\n",
            SpecKind::OpenApi3,
        ),
        (
            "openapi: 3.1\ninfo: {title: t}\npaths: {}\n",
            SpecKind::OpenApi3,
        ),
        (
            "swagger: 2.0\ninfo: {title: t}\npaths: {}\n",
            SpecKind::Swagger2,
        ),
    ] {
        let imp = import(doc);
        assert_eq!(imp.report.spec.kind, kind, "{doc}");
    }
}

#[test]
fn rejects_unsupported_without_partial_data() {
    for doc in [
        r#"{"openapi": "4.0.0", "paths": {}}"#,
        r#"{"swagger": "1.2", "apis": []}"#,
        r#"{"info": {"_postman_id": "x", "schema": "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"}, "item": []}"#,
        "asyncapi: 2.6.0\ninfo: {title: t, version: 1}\n",
        "- just\n- a list\n",
        "42",
    ] {
        match import_openapi(doc, "w", "p") {
            Err(OpenApiError::Unsupported(_))
            | Err(OpenApiError::Parse(_))
            | Err(OpenApiError::Invalid(_)) => {}
            Ok(i) => panic!("{doc}: accepted with {} ops", i.report.operations_created),
        }
    }
}

/// `$ref` cycles of every shape terminate and produce `{}` at the cut.
#[test]
fn ref_cycles_terminate() {
    let doc = json!({
        "openapi": "3.0.3", "info": {"title": "cycles"},
        "paths": {
            "/self": {"post": {"requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Self"}}}}}},
            "/mutual": {"post": {"requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/A"}}}}}},
            "/tree": {"post": {"requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Node"}}}}}},
            "/pref": {"get": {"parameters": [{"$ref": "#/components/parameters/P1"}]}},
            "/rbref": {"post": {"requestBody": {"$ref": "#/components/requestBodies/RB1"}}}
        },
        "components": {
            "schemas": {
                "Self": {"$ref": "#/components/schemas/Self"},
                "A": {"type": "object", "properties": {"b": {"$ref": "#/components/schemas/B"}}},
                "B": {"type": "object", "properties": {"a": {"$ref": "#/components/schemas/A"}}},
                "Node": {"type": "object", "properties": {"children": {"type": "array", "items": {"$ref": "#/components/schemas/Node"}}, "parent": {"$ref": "#/components/schemas/Node"}}}
            },
            "parameters": {"P1": {"$ref": "#/components/parameters/P2"}, "P2": {"$ref": "#/components/parameters/P1"}},
            "requestBodies": {"RB1": {"$ref": "#/components/requestBodies/RB1"}}
        }
    })
    .to_string();
    let t = Instant::now();
    let imp = import(&doc);
    assert!(t.elapsed() < Duration::from_secs(2));
    assert_eq!(imp.report.operations_created, 5);
    let tree: Value = serde_json::from_str(by_op(&imp, "POST /tree").body.text_str()).unwrap();
    assert!(tree["children"].is_array());
}

/// YAML anchors (`&`/`*`) are common in hand-written specs.
#[test]
fn yaml_anchors_and_aliases() {
    let doc = r#"
openapi: 3.0.3
info: {title: anchors, version: '1'}
paths:
  /a:
    get:
      parameters:
        - &page {name: page, in: query, required: true, schema: {type: integer, default: 1}}
        - &size {name: size, in: query, schema: {type: integer, default: 20}}
  /b:
    get:
      parameters: [*page, *size]
"#;
    let imp = import(doc);
    let b = by_op(&imp, "GET /b");
    assert_eq!(b.params.len(), 2);
    assert_eq!(b.params[0].value, "1");
}

/// YAML merge keys (`<<: *base`) are silently not applied: serde_yaml_ng
/// deserialising straight into serde_json::Value keeps `<<` as a literal key,
/// so merged parameter fields (`in`, `name`, `required`) are missing and the
/// parameter is dropped / the schema is empty.
#[test]
fn bug_qa_r1_022_yaml_merge_keys_not_applied() {
    let doc = r#"
openapi: 3.0.3
info: {title: merge, version: '1'}
x-common: &common
  in: query
  required: true
  schema: {type: string, default: abc}
paths:
  /m:
    get:
      parameters:
        - <<: *common
          name: tenant
"#;
    let imp = import(doc);
    let m = by_op(&imp, "GET /m");
    assert_eq!(
        m.params
            .iter()
            .map(|p| (p.key.as_str(), p.value.as_str(), p.enabled))
            .collect::<Vec<_>>(),
        vec![("tenant", "abc", true)],
        "warnings: {:?}",
        imp.report.warnings
    );
}

/// "Billion laughs": 9 levels x 10 aliases => 10^9 nodes if expanded. Must be
/// rejected quickly (or bounded), never hang or exhaust memory.
#[test]
fn yaml_billion_laughs_is_bounded() {
    let mut doc = String::from(
        "openapi: 3.0.3\ninfo: {title: lol}\nx-a: &a [\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\",\"lol\"]\n",
    );
    let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i"];
    for w in names.windows(2) {
        let prev = w[0];
        let cur = w[1];
        doc.push_str(&format!(
            "x-{cur}: &{cur} [*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev},*{prev}]\n"
        ));
    }
    doc.push_str("paths: {}\n");
    let t = Instant::now();
    let r = import_openapi(&doc, "w", "p");
    assert!(t.elapsed() < Duration::from_secs(3), "{:?}", t.elapsed());
    assert!(r.is_err(), "alias bomb should be rejected");
}

/// Deeply nested input is rejected (recursion limit) but only after a
/// QUADRATIC scan: 16k levels (32 KB) take ~1.25 s in a debug build, 200k
/// levels (400 KB) take minutes. `preview_openapi` / `import_openapi` are
/// synchronous Tauri commands (main thread), so pasting such text freezes the
/// whole UI. JSON text that fails to parse is re-parsed as YAML, doubling it.
#[test]
fn bug_qa_r1_008_deeply_nested_document_parses_quadratically() {
    let depth = 30_000;
    let json_doc = format!(
        "{{\"openapi\":\"3.0.0\",\"x\":{}{}}}",
        "[".repeat(depth),
        "]".repeat(depth)
    );
    let t = Instant::now();
    assert!(import_openapi(&json_doc, "w", "p").is_err());
    let took = t.elapsed();
    assert!(
        took < Duration::from_millis(500),
        "60 KB document took {took:?} to reject"
    );
}

#[test]
fn external_refs_are_skipped_and_reported() {
    let doc = json!({
        "openapi": "3.0.3", "info": {"title": "ext"},
        "paths": {
            "/e": {"post": {
                "parameters": [{"$ref": "https://evil.example/params.yaml#/P"}],
                "requestBody": {"content": {"application/json": {"schema": {"$ref": "common.yaml#/Thing"}}}}
            }}
        }
    })
    .to_string();
    let imp = import(&doc);
    assert_eq!(imp.report.operations_created, 1);
    let w = imp.report.warnings.join("\n");
    assert!(
        w.contains("evil.example") && w.contains("common.yaml"),
        "{w}"
    );
}

// ---------------------------------------------------------- performance

/// A realistic "large enterprise API": 1,000 operations whose request bodies
/// reference an order graph (12 schemas, 10-20 properties each, arrays,
/// allOf, enums). AC-OAS-9: <= 1.5 s (release build).
fn heavy_spec(ops: usize, fanout_props: usize) -> String {
    let mut schemas = serde_json::Map::new();
    let names = [
        "Order", "Customer", "Address", "LineItem", "Product", "Price", "Tax", "Discount",
        "Shipment", "Carrier", "Payment", "Refund",
    ];
    for (i, n) in names.iter().enumerate() {
        let mut props = serde_json::Map::new();
        for p in 0..fanout_props {
            props.insert(
                format!("field{p}"),
                json!({"type": "string", "example": format!("{n}-{p}")}),
            );
        }
        // link to the next 3 schemas (DAG, plus one back-edge cycle)
        for k in 1..=3 {
            let t = names[(i + k) % names.len()];
            props.insert(
                format!("rel{k}"),
                json!({"$ref": format!("#/components/schemas/{t}")}),
            );
        }
        props.insert("items".into(), json!({"type": "array", "items": {"$ref": format!("#/components/schemas/{}", names[(i + 4) % names.len()])}}));
        props.insert(
            "status".into(),
            json!({"type": "string", "enum": ["a", "b", "c"]}),
        );
        schemas.insert((*n).into(), json!({"allOf": [{"type": "object", "properties": props}, {"properties": {"id": {"type": "string", "format": "uuid"}}}]}));
    }
    let mut paths = serde_json::Map::new();
    for i in 0..ops / 2 {
        let n = names[i % names.len()];
        paths.insert(
            format!("/v1/tenants/{{tenantId}}/{}s/{{id{i}}}", n.to_lowercase()),
            json!({
                "get": {"tags": [n], "operationId": format!("get{n}{i}"),
                        "parameters": [{"name": "tenantId", "in": "path", "required": true, "schema": {"type": "string"}},
                                       {"name": format!("id{i}"), "in": "path", "required": true, "schema": {"type": "string"}},
                                       {"name": "expand", "in": "query", "schema": {"type": "array", "items": {"type": "string"}}}]},
                "post": {"tags": [n], "operationId": format!("update{n}{i}"),
                         "requestBody": {"content": {"application/json": {"schema": {"$ref": format!("#/components/schemas/{n}")}}}}}
            }),
        );
    }
    json!({"openapi": "3.0.3", "info": {"title": "Heavy"}, "servers": [{"url": "https://api.heavy.dev"}],
           "paths": paths, "components": {"schemas": schemas}})
    .to_string()
}

#[test]
fn perf_heavy_1000_operation_spec_within_budget() {
    let spec = heavy_spec(1000, 15);
    let t = Instant::now();
    let imp = import(&spec);
    let took = t.elapsed();
    let total_body: usize = imp.requests.iter().map(|r| r.body.text_str().len()).sum();
    let max_body = imp
        .requests
        .iter()
        .map(|r| r.body.text_str().len())
        .max()
        .unwrap();
    eprintln!(
        "heavy spec: {} KB, {} ops in {took:?}; bodies total {} KB, max {} KB",
        spec.len() / 1024,
        imp.report.operations_created,
        total_body / 1024,
        max_body / 1024
    );
    assert_eq!(imp.report.operations_created, 1000);
    let budget = if cfg!(debug_assertions) {
        15_000
    } else {
        1_500
    };
    assert!(took < Duration::from_millis(budget), "AC-OAS-9: {took:?}");
}

/// Observation (passes): request-body skeletons of wide schemas are large
/// (~260 KB for 40-property schemas), still under the Worker's 512 KB entity
/// limit. The 1,000-op heavy spec above generates ~47 MB of request bodies,
/// which matters for QA-R1-004 (push batches exceed the 5 MB request cap).
#[test]
fn skeleton_size_observation() {
    let spec = heavy_spec(20, 40);
    let imp = import(&spec);
    let max = imp
        .requests
        .iter()
        .map(|r| serde_json::to_string(r).unwrap().len())
        .max()
        .unwrap();
    eprintln!("largest generated request entity: {} KB", max / 1024);
    assert!(
        max <= 512 * 1024,
        "a single imported request is {} KB",
        max / 1024
    );
}

/// Measurement helper for QA-R1-008 (run with --ignored --nocapture).
#[test]
#[ignore]
fn measure_nesting_parse_time() {
    for depth in [1_000usize, 2_000, 4_000, 8_000, 16_000] {
        let json_doc = format!(
            "{{\"openapi\":\"3.0.0\",\"x\":{}{}}}",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let t = Instant::now();
        let r1 = import_openapi(&json_doc, "w", "p").is_err();
        let tj = t.elapsed();
        let yaml_doc = format!(
            "openapi: 3.0.0\nx: {}{}\n",
            "[".repeat(depth),
            "]".repeat(depth)
        );
        let t = Instant::now();
        let r2 = import_openapi(&yaml_doc, "w", "p").is_err();
        eprintln!(
            "depth {depth}: json-text {tj:?} (err={r1}), yaml-text {:?} (err={r2})",
            t.elapsed()
        );
    }
}

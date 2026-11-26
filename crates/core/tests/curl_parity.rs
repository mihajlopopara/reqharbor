//! cURL export parity with the Worker's share-page exporter (QA-R1-006).
//! Loads the shared fixture `worker/test/fixtures/curl_parity.json` (also
//! consumed by `worker/test/curl_parity.test.ts`) and checks that the Rust
//! exporter still produces each `expected` value.

use reqharbor_core::curl::{ExportOptions, export_curl};
use reqharbor_core::model::{Auth, Request, Variable};
use reqharbor_core::vars::{Layer, VarScope};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: String,
    request: Request,
    #[serde(default)]
    variables: Vec<Variable>,
    #[serde(default)]
    collection_auth: Option<Auth>,
    #[serde(default)]
    keep_variables: bool,
    expected: Value,
}

#[test]
fn qa_r1_006_export_matches_shared_parity_fixture() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../worker/test/fixtures/curl_parity.json"
    );
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => panic!("parity fixture missing at {path}: {e}"),
    };
    let doc: Value = serde_json::from_str(&text).unwrap();
    let cases: Vec<Case> = serde_json::from_value(doc["cases"].clone()).unwrap();
    assert!(cases.len() >= 30, "only {} cases", cases.len());
    let mut failures = Vec::new();
    for c in cases {
        let scope = VarScope::from_layers(vec![Layer {
            environment_id: "env".into(),
            environment_name: "Fixture".into(),
            variables: c.variables,
        }]);
        let opts = ExportOptions {
            keep_variables: c.keep_variables,
        };
        let got = match export_curl(&c.request, &scope, c.collection_auth.as_ref(), opts) {
            Ok(s) => serde_json::json!({ "curl": s }),
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        if got != c.expected {
            failures.push(format!(
                "{}:\n  expected {}\n  got      {}",
                c.name, c.expected, got
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

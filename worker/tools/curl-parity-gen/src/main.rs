//! For every case in the fixture file, run `reqharbor_core::curl::export_curl` and store the
//! result as `expected: {"curl": "..."}` or `expected: {"error": "..."}`.

use reqharbor_core::curl::{ExportOptions, export_curl};
use reqharbor_core::model::{Auth, Request, Variable};
use reqharbor_core::vars::{Layer, VarScope};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    request: Request,
    #[serde(default)]
    variables: Vec<Variable>,
    #[serde(default)]
    collection_auth: Option<Auth>,
    #[serde(default)]
    keep_variables: bool,
}

fn main() {
    let path = std::env::args().nth(1).expect("usage: curl-parity-gen <fixture.json>");
    let text = std::fs::read_to_string(&path).expect("read fixture");
    let mut doc: Value = serde_json::from_str(&text).expect("parse fixture");
    let cases = doc["cases"].as_array_mut().expect("cases array");
    for case in cases.iter_mut() {
        let name = case["name"].as_str().unwrap_or("?").to_string();
        let c: Case = serde_json::from_value(case.clone()).unwrap_or_else(|e| panic!("case {name}: {e}"));
        let scope = VarScope::from_layers(vec![Layer {
            environment_id: "env".into(),
            environment_name: "Fixture".into(),
            variables: c.variables,
        }]);
        let opts = ExportOptions { keep_variables: c.keep_variables };
        let expected = match export_curl(&c.request, &scope, c.collection_auth.as_ref(), opts) {
            Ok(s) => json!({ "curl": s }),
            Err(e) => json!({ "error": e.to_string() }),
        };
        case["expected"] = expected;
    }
    let out = serde_json::to_string_pretty(&doc).expect("serialize") + "\n";
    std::fs::write(&path, out).expect("write fixture");
    println!("updated {} cases in {path}", doc["cases"].as_array().map_or(0, Vec::len));
}

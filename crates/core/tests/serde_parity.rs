//! Serde parity with the Go server's canonical JSON (docs/plans/accounts/06-go-server.md §1.6).
//!
//! The Go server stores entity `data` and share snapshots only after `jsonx.Canonicalize`
//! accepts them, so one unreadable value can never fail every pull of a workspace (W9). The
//! fixture is generated on the Go side:
//!
//!   docker compose run --rm go go test ./internal/jsonx -run SerdeParity -update
//!
//! and checked here with the client's real types:
//!   - every accepted `output`, wrapped in a pull page, parses as the client's `PullPage`;
//!   - every input the server refuses for `number_range` or `lone_surrogate` is refused by
//!     `serde_json` too, which proves the server does not over-refuse.
//!
//! Run: docker compose run --rm dev cargo test -p reqharbor-core --test serde_parity

use std::path::PathBuf;

use reqharbor_core::sync::PullPage;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    input: String,
    accepted: bool,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    reason: Option<String>,
}

fn fixture() -> Fixture {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../server/internal/jsonx/testdata/serde_parity.json");
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    serde_json::from_str(&text).expect("fixture is valid JSON")
}

/// The pull page the server would send for one stored entity.
fn pull_page(data: &str) -> String {
    format!(
        r#"{{"changes":[{{"type":"request","id":"x","updatedAt":1,"deleted":false,"deviceId":"","data":{data},"rev":1}}],"cursor":1,"hasMore":false,"syncEpoch":"e"}}"#
    )
}

#[test]
fn accepted_outputs_parse_as_the_clients_pull_page() {
    let f = fixture();
    let mut checked = 0;
    for c in f.cases.iter().filter(|c| c.accepted) {
        let output = c
            .output
            .as_deref()
            .unwrap_or_else(|| panic!("{}: accepted without output", c.name));
        let page: PullPage = serde_json::from_str(&pull_page(output)).unwrap_or_else(|e| {
            panic!("{}: pull page with {output:?} does not parse: {e}", c.name)
        });
        assert_eq!(page.changes.len(), 1, "{}", c.name);
        assert_eq!(page.cursor, 1, "{}", c.name);
        // The canonical output is the same value as the input the client pushed.
        let out: Value = serde_json::from_str(output).expect("output parses");
        let input: Value = serde_json::from_str(&c.input).unwrap_or_else(|e| {
            panic!(
                "{}: serde_json refuses an input the server accepts: {e}",
                c.name
            )
        });
        assert_eq!(page.changes[0].data.as_ref(), Some(&out), "{}", c.name);
        assert_eq!(out, input, "{}: canonicalisation changed the value", c.name);
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} accepted cases");
}

#[test]
fn server_refusals_for_range_and_surrogates_are_serde_refusals() {
    let f = fixture();
    let mut checked = 0;
    for c in &f.cases {
        match c.reason.as_deref() {
            Some("number_range") | Some("lone_surrogate") => {
                assert!(!c.accepted, "{}", c.name);
                assert!(
                    serde_json::from_str::<Value>(&c.input).is_err(),
                    "{}: the server refuses {:?} but serde_json accepts it",
                    c.name,
                    c.input
                );
                checked += 1;
            }
            Some("too_deep") => assert!(!c.accepted, "{}", c.name),
            Some(other) => panic!("{}: unexpected reason {other}", c.name),
            None => assert!(c.accepted, "{}: refused without a reason", c.name),
        }
    }
    assert!(checked >= 8, "only {checked} refusal cases");
}

#[test]
fn fixture_covers_the_designs_edge_list() {
    let f = fixture();
    for name in [
        "max finite exponent",
        "just above f64 max",
        "overflow",
        "negative overflow",
        "underflow to zero",
        "u64 max plus one",
        "400-digit integer",
        "paired surrogates",
        "lone high surrogate",
        "lone low surrogate",
        "max code point raw",
        "depth 100",
        "depth 101",
    ] {
        assert!(
            f.cases.iter().any(|c| c.name == name),
            "missing case {name}"
        );
    }
}

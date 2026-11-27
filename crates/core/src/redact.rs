//! Central secret redaction for anything persisted to history (AC-ENV-7,
//! QA-R1-001). Every string value of a history entry — URL, error messages,
//! response headers and text bodies — has secret variable values replaced by
//! their `{{name}}` placeholder, in raw, percent-encoded and JSON-escaped form.

use crate::prepare::form_urlencode;
use crate::urlx::encode_query_value;
use crate::vars::VarScope;
use serde_json::Value;

/// Secrets shorter than this are not redacted (a 1–2 character "secret"
/// would mangle every string and is not meaningfully secret).
pub const MIN_SECRET_LEN: usize = 3;

#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// (needle, replacement), longest needle first.
    pairs: Vec<(String, String)>,
}

impl Redactor {
    /// Redact every enabled secret variable visible in `scope`, including
    /// shadowed ones.
    pub fn from_scope(scope: &VarScope) -> Self {
        let mut r = Redactor::default();
        for (key, value) in scope.secrets() {
            r.add(key, value);
        }
        r
    }

    pub fn add(&mut self, key: &str, value: &str) {
        if value.chars().count() < MIN_SECRET_LEN {
            return;
        }
        let placeholder = format!("{{{{{key}}}}}");
        let json = serde_json::to_string(value).unwrap_or_default();
        let json = json.trim_matches('"').to_string();
        for needle in [
            value.to_string(),
            form_urlencode(value),
            encode_query_value(value),
            value.replace(' ', "+"),
            json,
        ] {
            if needle.chars().count() >= MIN_SECRET_LEN
                && !self.pairs.iter().any(|(n, _)| *n == needle)
            {
                self.pairs.push((needle, placeholder.clone()));
            }
        }
        self.pairs.sort_by_key(|a| std::cmp::Reverse(a.0.len()));
    }

    pub fn is_empty(&self) -> bool {
        self.pairs.is_empty()
    }

    pub fn redact(&self, s: &str) -> String {
        let mut out = s.to_string();
        for (needle, replacement) in &self.pairs {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), replacement);
            }
        }
        out
    }

    /// Redact every string (not object keys) inside a JSON value.
    pub fn redact_json(&self, v: &mut Value) {
        match v {
            Value::String(s) => {
                if self.pairs.iter().any(|(n, _)| s.contains(n.as_str())) {
                    *s = self.redact(s);
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|i| self.redact_json(i)),
            Value::Object(map) => map.values_mut().for_each(|i| self.redact_json(i)),
            _ => {}
        }
    }

    /// Redact a serializable value by round-tripping it through JSON.
    ///
    /// Fails closed: when masking every string makes the value unreadable
    /// (a secret that is a substring of an enum literal such as "system"),
    /// the strings are masked one at a time and only those whose masking
    /// breaks the value keep their text (they are enum literals, not data).
    pub fn redact_struct<T: serde::Serialize + serde::de::DeserializeOwned + Clone>(
        &self,
        value: &T,
    ) -> T {
        if self.is_empty() {
            return value.clone();
        }
        let Ok(orig) = serde_json::to_value(value) else {
            return value.clone();
        };
        let mut all = orig.clone();
        self.redact_json(&mut all);
        if let Ok(v) = serde_json::from_value(all) {
            return v;
        }
        let mut leaves = Vec::new();
        self.leaves(&orig, String::new(), &mut leaves);
        let mut cur = orig;
        for (ptr, masked) in leaves {
            let Some(slot) = cur.pointer_mut(&ptr) else {
                continue;
            };
            let before = std::mem::replace(slot, Value::String(masked));
            if serde_json::from_value::<T>(cur.clone()).is_err() {
                *cur.pointer_mut(&ptr).unwrap() = before;
            }
        }
        serde_json::from_value(cur).unwrap_or_else(|_| value.clone())
    }

    /// (JSON pointer, masked text) of every string that holds a secret.
    fn leaves(&self, v: &Value, ptr: String, out: &mut Vec<(String, String)>) {
        match v {
            Value::String(s) => {
                if self.pairs.iter().any(|(n, _)| s.contains(n.as_str())) {
                    out.push((ptr, self.redact(s)));
                }
            }
            Value::Array(items) => {
                for (i, x) in items.iter().enumerate() {
                    self.leaves(x, format!("{ptr}/{i}"), out);
                }
            }
            Value::Object(map) => {
                for (k, x) in map {
                    let k = k.replace('~', "~0").replace('/', "~1");
                    self.leaves(x, format!("{ptr}/{k}"), out);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn redact_struct_fails_closed_when_a_secret_is_inside_an_enum_literal() {
        #[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        #[serde(rename_all = "lowercase")]
        enum Dir {
            System,
            Out,
        }
        #[derive(Clone, serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct F {
            dir: Dir,
            data: String,
        }
        let mut r = Redactor::default();
        r.add("w", "stem");
        let f = F {
            dir: Dir::System,
            data: "Couldn't resolve stem.example.dev".into(),
        };
        let out = r.redact_struct(&f);
        assert_eq!(out.dir, Dir::System);
        assert_eq!(out.data, "Couldn't resolve {{w}}.example.dev");
        assert_ne!(out.dir, Dir::Out);
    }

    use super::*;
    use crate::model::{Environment, Variable};

    #[test]
    fn redacts_raw_encoded_and_json_forms() {
        let env = Environment {
            id: "g".into(),
            workspace_id: "w".into(),
            project_id: None,
            name: "G".into(),
            variables: vec![
                Variable::new("tok", "a b\"c&d").secret(),
                Variable::new("plain", "visible"),
                Variable::new("short", "xy").secret(),
            ],
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        };
        let r = Redactor::from_scope(&VarScope::new(None, Some(&env)));
        assert_eq!(r.redact("x=a b\"c&d"), "x={{tok}}");
        assert_eq!(r.redact("q=a%20b%22c%26d"), "q={{tok}}");
        assert_eq!(r.redact("f=a%20b%22c%26d"), "f={{tok}}");
        assert_eq!(r.redact("visible xy"), "visible xy");
        let mut v = serde_json::json!({"a b\"c&d": ["a b\"c&d", 1, {"m": "err: a b\"c&d"}]});
        r.redact_json(&mut v);
        assert_eq!(
            v,
            serde_json::json!({"a b\"c&d": ["{{tok}}", 1, {"m": "err: {{tok}}"}]})
        );
    }
}

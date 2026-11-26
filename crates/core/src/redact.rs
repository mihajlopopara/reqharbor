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
    pub fn redact_struct<T: serde::Serialize + serde::de::DeserializeOwned + Clone>(
        &self,
        value: &T,
    ) -> T {
        if self.is_empty() {
            return value.clone();
        }
        let Ok(mut v) = serde_json::to_value(value) else {
            return value.clone();
        };
        self.redact_json(&mut v);
        serde_json::from_value(v).unwrap_or_else(|_| value.clone())
    }
}

#[cfg(test)]
mod tests {
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

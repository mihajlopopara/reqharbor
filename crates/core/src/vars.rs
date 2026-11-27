//! `{{variable}}` substitution (AC-ENV-1 .. AC-ENV-4).
//!
//! * A reference is `{{ name }}` where name matches `\$?[A-Za-z0-9_.-]+`;
//!   whitespace inside the braces is trimmed.
//! * A name starting with `$` is a dynamic variable (`{{$uuid}}`, see
//!   [`crate::dynamic`]): generated, never looked up, never unresolved when
//!   known. Environment variable names may not start with `$`.
//! * Lookup order: active project environment, then Global. Disabled
//!   variables are skipped.
//! * Values may reference other variables up to depth 3. A reference that
//!   would recurse deeper, or that is part of a cycle, is left as literal text.
//! * Unresolved references are left verbatim and reported.

use crate::dynamic::DynamicCtx;
use crate::model::{Environment, Variable};
use serde::Serialize;

/// Maximum nesting depth for variables referencing variables (AC-ENV-2).
pub const MAX_DEPTH: usize = 3;

/// One layer of variables (an environment) in precedence order.
#[derive(Debug, Clone)]
pub struct Layer {
    pub environment_id: String,
    pub environment_name: String,
    pub variables: Vec<Variable>,
}

/// Variables visible to a request: project environment first, then Global.
#[derive(Debug, Clone, Default)]
pub struct VarScope {
    layers: Vec<Layer>,
}

/// Where a variable resolved from (AC-ENV-4 hover).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariableInfo {
    pub key: String,
    pub value: String,
    pub secret: bool,
    pub environment_id: String,
    pub environment_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolveOptions {
    /// Leave `{{name}}` in place for secret variables (history, exports with
    /// secrets withheld). Such references are not reported as unresolved.
    pub keep_secrets: bool,
}

/// Accumulates unresolved names across several `resolve` calls.
#[derive(Debug, Default, Clone)]
pub struct Unresolved {
    names: Vec<String>,
}

impl Unresolved {
    fn add(&mut self, name: &str) {
        if !self.names.iter().any(|n| n == name) {
            self.names.push(name.to_string());
        }
    }
    pub fn names(&self) -> &[String] {
        &self.names
    }
    pub fn into_names(self) -> Vec<String> {
        self.names
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

impl VarScope {
    pub fn empty() -> Self {
        VarScope::default()
    }

    /// Build the scope from the active project environment and Global.
    pub fn new(project_env: Option<&Environment>, global: Option<&Environment>) -> Self {
        let mut layers = Vec::new();
        for env in [project_env, global].into_iter().flatten() {
            layers.push(Layer {
                environment_id: env.id.clone(),
                environment_name: env.name.clone(),
                variables: env.variables.clone(),
            });
        }
        VarScope { layers }
    }

    pub fn from_layers(layers: Vec<Layer>) -> Self {
        VarScope { layers }
    }

    /// Look up an enabled variable by exact name, honouring precedence.
    pub fn lookup(&self, name: &str) -> Option<(&Variable, &Layer)> {
        self.layers.iter().find_map(|layer| {
            layer
                .variables
                .iter()
                .find(|v| v.enabled && v.key == name)
                .map(|v| (v, layer))
        })
    }

    /// `(key, value)` of every enabled secret variable in any layer.
    pub fn secrets(&self) -> impl Iterator<Item = (&str, &str)> {
        self.layers
            .iter()
            .flat_map(|l| l.variables.iter())
            .filter(|v| v.enabled && v.secret && !v.value.is_empty())
            .map(|v| (v.key.as_str(), v.value.as_str()))
    }

    /// All visible variables (first definition wins), with source info.
    pub fn visible(&self) -> Vec<VariableInfo> {
        let mut out: Vec<VariableInfo> = Vec::new();
        for layer in &self.layers {
            for v in layer.variables.iter().filter(|v| v.enabled) {
                if out.iter().any(|o| o.key == v.key) {
                    continue;
                }
                out.push(VariableInfo {
                    key: v.key.clone(),
                    value: v.value.clone(),
                    secret: v.secret,
                    environment_id: layer.environment_id.clone(),
                    environment_name: layer.environment_name.clone(),
                });
            }
        }
        out
    }

    /// Whether `text` references a secret variable.
    pub fn references_secret(&self, text: &str) -> bool {
        references(text)
            .iter()
            .any(|n| self.lookup(n).is_some_and(|(v, _)| v.secret))
    }

    /// Resolve all references in `text`. Dynamic variables get fresh values;
    /// use [`VarScope::resolve_in`] to share them across several calls.
    pub fn resolve(&self, text: &str, opts: ResolveOptions, unresolved: &mut Unresolved) -> String {
        self.resolve_in(text, opts, unresolved, &DynamicCtx::new())
    }

    /// Resolve all references in `text`; `{{$name}}` takes its value from
    /// `dynamic` (one value per name for every call sharing it).
    pub fn resolve_in(
        &self,
        text: &str,
        opts: ResolveOptions,
        unresolved: &mut Unresolved,
        dynamic: &DynamicCtx,
    ) -> String {
        let mut stack = Vec::new();
        let mut cx = Cx {
            opts,
            unresolved,
            stack: &mut stack,
            dynamic,
        };
        self.resolve_inner(text, &mut cx, 0)
            .unwrap_or_else(|_| text.to_string())
    }

    /// Convenience: resolve and return unresolved names.
    pub fn resolve_simple(&self, text: &str) -> (String, Vec<String>) {
        let mut u = Unresolved::default();
        let s = self.resolve(text, ResolveOptions::default(), &mut u);
        (s, u.into_names())
    }

    // Err(()) signals a cycle: the caller leaves its reference literal.
    fn resolve_inner(&self, text: &str, cx: &mut Cx<'_>, depth: usize) -> Result<String, ()> {
        if !text.contains("{{") {
            return Ok(text.to_string());
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(found) = find_reference(rest) {
            out.push_str(&rest[..found.start]);
            let literal = &rest[found.start..found.end];
            let name = found.name;
            if name.starts_with('$') {
                match cx.dynamic.value(name) {
                    Some(_) if cx.dynamic.keeps() => out.push_str(literal),
                    Some(v) => out.push_str(&v),
                    None => {
                        cx.unresolved.add(name);
                        out.push_str(literal);
                    }
                }
                rest = &rest[found.end..];
                continue;
            }
            match self.lookup(name) {
                None => {
                    cx.unresolved.add(name);
                    out.push_str(literal);
                }
                Some((var, _)) if var.secret && cx.opts.keep_secrets => out.push_str(literal),
                Some(_) if cx.stack.iter().any(|s| s == name) => return Err(()),
                Some(_) if depth > MAX_DEPTH => out.push_str(literal),
                Some((var, _)) => {
                    cx.stack.push(name.to_string());
                    let value = var.value.clone();
                    let resolved = self.resolve_inner(&value, cx, depth + 1);
                    cx.stack.pop();
                    match resolved {
                        Ok(v) => out.push_str(&v),
                        Err(()) if depth == 0 => {
                            // Cycle detected below this top-level reference.
                            out.push_str(literal);
                        }
                        Err(()) => return Err(()),
                    }
                }
            }
            rest = &rest[found.end..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// State threaded through one top-level resolve.
struct Cx<'a> {
    opts: ResolveOptions,
    unresolved: &'a mut Unresolved,
    stack: &'a mut Vec<String>,
    dynamic: &'a DynamicCtx,
}

struct Found<'a> {
    start: usize,
    end: usize,
    name: &'a str,
}

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-'
}

/// `\$?[A-Za-z0-9_.-]+`: a leading `$` marks a dynamic variable.
pub fn is_valid_reference_name(name: &str) -> bool {
    let rest = name.strip_prefix('$').unwrap_or(name);
    !rest.is_empty() && rest.chars().all(is_name_char)
}

fn find_reference(text: &str) -> Option<Found<'_>> {
    let mut search_from = 0;
    while let Some(rel) = text[search_from..].find("{{") {
        let start = search_from + rel;
        let inner_start = start + 2;
        let close_rel = text[inner_start..].find("}}")?;
        let inner = &text[inner_start..inner_start + close_rel];
        let name = inner.trim();
        if is_valid_reference_name(name) {
            return Some(Found {
                start,
                end: inner_start + close_rel + 2,
                name,
            });
        }
        search_from = start + 1;
    }
    None
}

/// Names referenced in `text`, in order (with duplicates removed).
pub fn references(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(f) = find_reference(rest) {
        if !out.iter().any(|n| n == f.name) {
            out.push(f.name.to_string());
        }
        rest = &rest[f.end..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(id: &str, name: &str, vars: Vec<Variable>) -> Environment {
        Environment {
            id: id.into(),
            workspace_id: "w".into(),
            project_id: None,
            name: name.into(),
            variables: vars,
            sort_order: 0,
            created_at: 0,
            updated_at: 0,
            extra: Default::default(),
        }
    }

    #[test]
    fn ac_env_1_syntax_and_whitespace_trim() {
        let g = env("g", "Global", vec![Variable::new("host.name-1_x", "h")]);
        let s = VarScope::new(None, Some(&g));
        assert_eq!(s.resolve_simple("a{{ host.name-1_x }}b").0, "ahb");
        assert_eq!(s.resolve_simple("{{host.name-1_x}}").0, "h");
        // invalid names are not references
        assert_eq!(s.resolve_simple("{{not valid}}").0, "{{not valid}}");
        assert_eq!(s.resolve_simple("{{}}").0, "{{}}");
        assert_eq!(s.resolve_simple("{{{host.name-1_x}}}").0, "{h}");
        assert_eq!(s.resolve_simple("{{ unterminated").0, "{{ unterminated");
    }

    #[test]
    fn ac_env_2_precedence_project_then_global_and_disabled_skipped() {
        let mut disabled = Variable::new("b", "project-disabled");
        disabled.enabled = false;
        let p = env(
            "p",
            "Staging",
            vec![Variable::new("a", "project"), disabled],
        );
        let g = env(
            "g",
            "Global",
            vec![Variable::new("a", "global"), Variable::new("b", "global-b")],
        );
        let s = VarScope::new(Some(&p), Some(&g));
        assert_eq!(s.resolve_simple("{{a}}-{{b}}").0, "project-global-b");
        let info = s.visible();
        assert_eq!(info[0].environment_name, "Staging");
        assert_eq!(
            info.iter().find(|i| i.key == "b").unwrap().environment_name,
            "Global"
        );
    }

    #[test]
    fn ac_env_2_nested_up_to_depth_3() {
        let g = env(
            "g",
            "G",
            vec![
                Variable::new("a", "A{{b}}"),
                Variable::new("b", "B{{c}}"),
                Variable::new("c", "C{{d}}"),
                Variable::new("d", "D{{e}}"),
                Variable::new("e", "E"),
            ],
        );
        let s = VarScope::new(None, Some(&g));
        // a -> b -> c -> d : depth 3 reached, {{e}} stays literal
        assert_eq!(s.resolve_simple("{{a}}").0, "ABCD{{e}}");
        assert_eq!(s.resolve_simple("{{c}}").0, "CDE");
    }

    #[test]
    fn ac_env_2_cycles_leave_literal() {
        let g = env(
            "g",
            "G",
            vec![
                Variable::new("a", "x{{b}}"),
                Variable::new("b", "y{{a}}"),
                Variable::new("self", "{{self}}"),
            ],
        );
        let s = VarScope::new(None, Some(&g));
        assert_eq!(s.resolve_simple("<{{a}}>").0, "<{{a}}>");
        assert_eq!(s.resolve_simple("{{self}}").0, "{{self}}");
    }

    #[test]
    fn ac_env_3_unresolved_kept_literal_and_reported() {
        let s = VarScope::empty();
        let (out, missing) = s.resolve_simple("https://{{host}}/{{ path }}/{{host}}");
        assert_eq!(out, "https://{{host}}/{{ path }}/{{host}}");
        assert_eq!(missing, vec!["host".to_string(), "path".to_string()]);
    }

    #[test]
    fn secrets_can_be_kept_unresolved() {
        let g = env(
            "g",
            "G",
            vec![
                Variable::new("tok", "s3cr3t").secret(),
                Variable::new("h", "x"),
            ],
        );
        let s = VarScope::new(None, Some(&g));
        let mut u = Unresolved::default();
        let out = s.resolve(
            "{{h}}?t={{tok}}",
            ResolveOptions { keep_secrets: true },
            &mut u,
        );
        assert_eq!(out, "x?t={{tok}}");
        assert!(u.is_empty());
        assert!(s.references_secret("{{ tok }}"));
        assert!(!s.references_secret("{{h}}"));
    }

    #[test]
    fn dynamic_names_are_references_and_generated() {
        let s = VarScope::empty();
        let (out, missing) = s.resolve_simple("id={{ $uuid }}");
        assert!(missing.is_empty());
        let id = out.strip_prefix("id=").unwrap();
        assert!(uuid::Uuid::parse_str(id).is_ok(), "{out}");
        // `$` only as the first character
        assert_eq!(s.resolve_simple("{{a$b}}").0, "{{a$b}}");
        assert_eq!(s.resolve_simple("{{$}}").0, "{{$}}");
        assert_eq!(s.resolve_simple("{{$$uuid}}").0, "{{$$uuid}}");
        assert_eq!(references("{{$uuid}} {{x}}"), vec!["$uuid", "x"]);
    }

    #[test]
    fn dynamic_values_are_stable_within_one_ctx() {
        let g = env("g", "G", vec![Variable::new("id", "{{$uuid}}")]);
        let s = VarScope::new(None, Some(&g));
        let ctx = DynamicCtx::new();
        let mut u = Unresolved::default();
        let a = s.resolve_in(
            "{{$uuid}}/{{$uuid}}",
            ResolveOptions::default(),
            &mut u,
            &ctx,
        );
        let (x, y) = a.split_once('/').unwrap();
        assert_eq!(x, y);
        // nested through an env var, and across calls
        let b = s.resolve_in(
            "{{id}}",
            ResolveOptions { keep_secrets: true },
            &mut u,
            &ctx,
        );
        assert_eq!(b, x);
        assert!(u.is_empty());
        // a new ctx draws a new value
        let c = s.resolve_in(
            "{{$uuid}}",
            ResolveOptions::default(),
            &mut u,
            &DynamicCtx::new(),
        );
        assert_ne!(c, x);
    }

    #[test]
    fn dynamic_keep_mode_leaves_literal() {
        let s = VarScope::empty();
        let mut u = Unresolved::default();
        let out = s.resolve_in(
            "{{ $randomEmail }} {{$nope}}",
            ResolveOptions::default(),
            &mut u,
            &DynamicCtx::keep(),
        );
        assert_eq!(out, "{{ $randomEmail }} {{$nope}}");
        assert_eq!(u.names(), ["$nope".to_string()]);
    }

    #[test]
    fn unknown_dynamic_and_missing_vars_untouched() {
        let s = VarScope::empty();
        let (out, missing) = s.resolve_simple("{{$randomEmial}}-{{host}}-{{$randomInt}}");
        assert!(out.starts_with("{{$randomEmial}}-{{host}}-"), "{out}");
        assert!(out.rsplit('-').next().unwrap().parse::<u32>().is_ok());
        assert_eq!(
            missing,
            vec!["$randomEmial".to_string(), "host".to_string()]
        );
    }

    #[test]
    fn env_vars_named_with_dollar_are_never_looked_up() {
        let g = env("g", "G", vec![Variable::new("$uuid", "fixed")]);
        let s = VarScope::new(None, Some(&g));
        assert_ne!(s.resolve_simple("{{$uuid}}").0, "fixed");
    }
}

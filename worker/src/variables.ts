// `{{variable}}` resolution (AC-ENV-1, AC-ENV-2) — exact port of crates/core/src/vars.rs
// (QA-R1-009), shared by the cURL exporter and the share page.

/** `{{ name }}` — for highlighting only; resolution uses the core's scanner below. */
export const VAR_RE = /\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}/g;
/** Maximum nesting depth for variables referencing variables (AC-ENV-2), as in the core. */
const MAX_DEPTH = 3;

export type VariableMap = ReadonlyMap<string, string>;

// Rust `str::trim` (Unicode White_Space); JS `trim` additionally strips U+FEFF.
const RUST_WS = "\\t\\n\\v\\f\\r \\u0085\\u00a0\\u1680\\u2000-\\u200a\\u2028\\u2029\\u202f\\u205f\\u3000";
const TRIM_RE = new RegExp(`^[${RUST_WS}]+|[${RUST_WS}]+$`, "g");
export const rustTrim = (s: string): string => s.replace(TRIM_RE, "");
const NAME_RE = /^[A-Za-z0-9_.-]+$/;

interface Found {
  start: number;
  end: number;
  name: string;
}

/** `vars::find_reference`: first `{{ name }}` with a valid name; stops at the first `{{` with no `}}`. */
function findReference(text: string): Found | null {
  let from = 0;
  for (;;) {
    const start = text.indexOf("{{", from);
    if (start === -1) return null;
    const innerStart = start + 2;
    const close = text.indexOf("}}", innerStart);
    if (close === -1) return null;
    const name = rustTrim(text.slice(innerStart, close));
    if (name !== "" && NAME_RE.test(name)) return { start, end: close + 2, name };
    from = start + 1;
  }
}

const CYCLE = Symbol("cycle");

/**
 * Replaces known variables like the core: nested references resolve while depth <= 3 (deeper
 * ones stay literal); a cycle anywhere below a top-level reference leaves that top-level
 * reference literal. Unknown names stay literal.
 */
export function resolveVariables(text: string, vars: VariableMap): string {
  const inner = (s: string, stack: string[], depth: number): string | typeof CYCLE => {
    if (!s.includes("{{")) return s;
    let out = "";
    let rest = s;
    for (let f = findReference(rest); f; f = findReference(rest)) {
      out += rest.slice(0, f.start);
      const literal = rest.slice(f.start, f.end);
      const value = vars.get(f.name);
      if (value === undefined) {
        out += literal;
      } else if (stack.includes(f.name)) {
        return CYCLE;
      } else if (depth > MAX_DEPTH) {
        out += literal;
      } else {
        stack.push(f.name);
        const resolved = inner(value, stack, depth + 1);
        stack.pop();
        if (resolved !== CYCLE) out += resolved;
        else if (depth === 0) out += literal;
        else return CYCLE;
      }
      rest = rest.slice(f.end);
    }
    return out + rest;
  };
  const r = inner(text, [], 0);
  return r === CYCLE ? text : r;
}

/** Builds a lookup from environment variables: disabled and secret ones are skipped; first wins. */
export function variableMap(
  variables: readonly { key: string; value: string; enabled: boolean; secret?: boolean }[],
): Map<string, string> {
  const m = new Map<string, string>();
  for (const v of variables) {
    if (!v.enabled || v.secret || m.has(v.key)) continue;
    m.set(v.key, v.value);
  }
  return m;
}

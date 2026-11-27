/**
 * GraphQL helpers that need no graphql-js (main bundle): the operation scanner behind the
 * Operation picker (AC-GQL-9), the response `errors` badge (AC-GQL-11), and the JSON scanning
 * behind Variables completion and checks (AC-GQL-8). Schema-aware work lives in ../graphql/
 * (a lazy chunk with graphql-js).
 */

export type OperationType = "query" | "mutation" | "subscription";

export interface GqlOperation {
  type: OperationType;
  /** null for an anonymous operation (`{ … }` or `query { … }`). */
  name: string | null;
  /** Offset of the operation's first token. */
  from: number;
}

const NAME_START = /[_A-Za-z]/;
const NAME_CHAR = /[_0-9A-Za-z]/;

/**
 * The executable operations of a document, in order. A small lexer (strings, block strings and
 * comments skipped, braces counted) so it works while the user is typing and the document
 * doesn't parse yet. Fragments and type-system definitions are skipped.
 */
export function scanOperations(src: string): GqlOperation[] {
  const ops: GqlOperation[] = [];
  let depth = 0; // { }
  let parens = 0; // ( ) and [ ] inside a definition header
  /** A definition keyword was seen at the top level and its body `{` hasn't opened yet. */
  let pending: { op: GqlOperation | null; wantName: boolean } | null = null;
  const n = src.length;
  let i = 0;
  while (i < n) {
    const c = src[i];
    if (c === "#") {
      while (i < n && src[i] !== "\n" && src[i] !== "\r") i++;
      continue;
    }
    if (c === '"') {
      if (src.startsWith('"""', i)) {
        i += 3;
        while (i < n && !src.startsWith('"""', i)) i += src[i] === "\\" && src.startsWith('\\"""', i) ? 4 : 1;
        i += 3;
      } else {
        i++;
        while (i < n && src[i] !== '"' && src[i] !== "\n") i += src[i] === "\\" ? 2 : 1;
        i++;
      }
      if (pending) pending.wantName = false;
      continue;
    }
    if (NAME_START.test(c)) {
      let j = i + 1;
      while (j < n && NAME_CHAR.test(src[j])) j++;
      const word = src.slice(i, j);
      if (depth === 0 && parens === 0) {
        if (pending?.wantName) {
          if (pending.op) pending.op.name = word;
          pending.wantName = false;
        } else if (!pending) {
          if (word === "query" || word === "mutation" || word === "subscription") {
            const op: GqlOperation = { type: word, name: null, from: i };
            ops.push(op);
            pending = { op, wantName: true };
          } else {
            // fragment, type, schema, extend, … : a definition that is not an operation.
            pending = { op: null, wantName: false };
          }
        }
      }
      i = j;
      continue;
    }
    if (c === "(" || c === "[") parens++;
    else if (c === ")" || c === "]") parens = Math.max(0, parens - 1);
    else if (c === "{") {
      if (parens > 0) {
        // `{` inside a header's default value or directive argument: not the body.
      } else {
        if (depth === 0) {
          if (!pending) ops.push({ type: "query", name: null, from: i });
          pending = null;
        }
        depth++;
      }
    } else if (c === "}") {
      if (parens === 0) depth = Math.max(0, depth - 1);
    } else if (pending && c !== " " && c !== "\t" && c !== "\n" && c !== "\r" && c !== ",") {
      pending.wantName = false;
    }
    i++;
  }
  return ops;
}

/**
 * The `operationName` to keep for a document (AC-GQL-9): none unless it defines several
 * operations; then the chosen one when it still exists, else the first named one.
 */
export function pickOperationName(ops: GqlOperation[], chosen: string | undefined): string | undefined {
  const named = ops.filter((o) => o.name);
  if (named.length < 2) return undefined;
  if (chosen && named.some((o) => o.name === chosen)) return chosen;
  return named[0].name ?? undefined;
}

/**
 * The request as it is sent / exported (AC-GQL-9): `operationName` normalised to what the
 * picker shows — the first named operation when none (or a stale one) is stored, nothing with
 * fewer than two named operations. Returns `r` itself when nothing changes; never mutates it.
 */
export function withOperation<R extends { body: { type: string; graphql?: { query: string; operationName?: string } } }>(r: R): R {
  const g = r.body.type === "graphql" ? r.body.graphql : undefined;
  if (!g) return r;
  const op = pickOperationName(scanOperations(g.query), g.operationName);
  if (op === g.operationName) return r;
  const graphql = { ...g };
  if (op === undefined) delete graphql.operationName;
  else graphql.operationName = op;
  return { ...r, body: { ...r.body, graphql } };
}

/** True when the document has `#` comments (outside strings): Format would drop them (AC-GQL-7). */
export function hasComments(src: string): boolean {
  const n = src.length;
  let i = 0;
  while (i < n) {
    const c = src[i];
    if (c === "#") return true;
    if (c === '"') {
      if (src.startsWith('"""', i)) {
        i += 3;
        while (i < n && !src.startsWith('"""', i)) i += src[i] === "\\" && src.startsWith('\\"""', i) ? 4 : 1;
        i += 3;
      } else {
        i++;
        while (i < n && src[i] !== '"' && src[i] !== "\n") i += src[i] === "\\" ? 2 : 1;
        i++;
      }
      continue;
    }
    i++;
  }
  return false;
}

export interface GqlResponseError {
  message: string;
  /** "user.posts.0.title" */
  path: string | null;
  /** "3:5" of the first location */
  location: string | null;
}

/**
 * GraphQL `errors` of a response body (AC-GQL-11): a JSON object with a non-empty `errors`
 * array. Anything else (not JSON, an array, no errors) gives [].
 */
export function graphqlErrorsOf(text: string | null): GqlResponseError[] {
  if (!text) return [];
  const t = text.trimStart();
  if (!t.startsWith("{") || !t.includes('"errors"')) return [];
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    return [];
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return [];
  const errors = (v as { errors?: unknown }).errors;
  if (!Array.isArray(errors)) return [];
  return errors.map((e) => {
    const o = e && typeof e === "object" ? (e as Record<string, unknown>) : {};
    const message = typeof o.message === "string" ? o.message : JSON.stringify(e);
    const path = Array.isArray(o.path) ? o.path.map(String).join(".") : null;
    const loc = Array.isArray(o.locations) ? (o.locations[0] as { line?: unknown; column?: unknown } | undefined) : undefined;
    const location = loc && typeof loc.line === "number" ? `${loc.line}:${loc.column ?? 1}` : null;
    return { message, path, location };
  });
}

/** "1 GraphQL error" / "3 GraphQL errors" */
export function graphqlErrorsLabel(n: number): string {
  return `${n} GraphQL error${n === 1 ? "" : "s"}`;
}

// ───────────── Variables JSON (AC-GQL-8) ─────────────

/** A variable the selected operation declares. */
export interface VariableDef {
  /** Without `$`. */
  name: string;
  /** As written, e.g. "ID!", "[String!]", "UserInput". */
  type: string;
  /** Non-null without a default value. */
  required: boolean;
  /** The default value as written, if any. */
  defaultValue: string | null;
}

export interface JsonKey {
  name: string;
  /** Range of the key token, quotes included. */
  from: number;
  to: number;
  /** Range of the value (trimmed); valueFrom === valueTo when it is missing. */
  valueFrom: number;
  valueTo: number;
}

export interface Diagnostic {
  from: number;
  to: number;
  severity: "error" | "warning";
  message: string;
}

function skipString(text: string, i: number): number {
  // i is at the opening quote; returns the index after the closing quote (or text.length).
  let j = i + 1;
  while (j < text.length && text[j] !== '"') {
    if (text[j] === "\n") return j;
    j += text[j] === "\\" ? 2 : 1;
  }
  return Math.min(j + 1, text.length);
}

/**
 * Top-level keys of a JSON object text, with their ranges. Tolerant: works on half-typed text
 * (stops at the end), `null` when the text isn't an object.
 */
export function jsonTopLevelKeys(text: string): JsonKey[] | null {
  let i = 0;
  const n = text.length;
  while (i < n && /\s/.test(text[i])) i++;
  if (text[i] !== "{") return null;
  i++;
  const keys: JsonKey[] = [];
  let depth = 1;
  let expectKey = true;
  let current: JsonKey | null = null;
  const close = (end: number) => {
    if (!current) return;
    let e = end;
    while (e > current.valueFrom && /\s/.test(text[e - 1])) e--;
    current.valueTo = Math.max(current.valueFrom, e);
    current = null;
  };
  while (i < n && depth > 0) {
    const c = text[i];
    if (c === '"') {
      const end = skipString(text, i);
      if (depth === 1 && expectKey) {
        let name = text.slice(i + 1, end - 1);
        try {
          name = JSON.parse(text.slice(i, end)) as string;
        } catch {
          /* half-typed key: raw text */
        }
        let j = end;
        while (j < n && /\s/.test(text[j])) j++;
        if (text[j] === ":") {
          j++;
          while (j < n && /\s/.test(text[j])) j++;
          current = { name, from: i, to: end, valueFrom: j, valueTo: j };
          keys.push(current);
          expectKey = false;
          i = j;
          continue;
        }
        keys.push({ name, from: i, to: end, valueFrom: end, valueTo: end });
      }
      i = end;
      continue;
    }
    if (c === "{" || c === "[") depth++;
    else if (c === "}" || c === "]") {
      depth--;
      if (depth === 0) close(i);
    } else if (c === "," && depth === 1) {
      close(i);
      expectKey = true;
    }
    i++;
  }
  close(n);
  return keys;
}

export interface KeyCompletionSpot {
  /** Replace this range with the completion. */
  from: number;
  to: number;
  /** Text typed so far (without quotes), for filtering. */
  prefix: string;
  /** The text is empty: wrap the completion in `{ … }`. */
  wrap: boolean;
  /** A `:` already follows the key being edited: insert only the quoted name. */
  hasColon: boolean;
}

/**
 * Where a variable name can be completed at `pos` (AC-GQL-8): an empty document, or a key
 * position of the top-level object (after `{` or `,`, possibly inside a half-typed `"na`).
 */
export function keyCompletionAt(text: string, pos: number): KeyCompletionSpot | null {
  if (!text.trim()) return { from: 0, to: text.length, prefix: "", wrap: true, hasColon: false };
  let i = 0;
  while (i < pos && /\s/.test(text[i])) i++;
  if (text[i] !== "{" || i >= pos) return null;
  i++;
  let depth = 1;
  let expectKey = true;
  while (i < pos) {
    const c = text[i];
    if (c === '"') {
      const end = skipString(text, i);
      const closed = end - 1 > i && text[end - 1] === '"';
      if (depth === 1 && expectKey) {
        if (pos <= (closed ? end - 1 : end)) {
          let j = end;
          while (j < text.length && /[ \t]/.test(text[j])) j++;
          const hasColon = closed && text[j] === ":";
          return { from: i, to: closed ? end : pos, prefix: text.slice(i + 1, pos), wrap: false, hasColon };
        }
        expectKey = false; // a complete key before the caret
      }
      i = end;
      continue;
    }
    if (c === "{" || c === "[") depth++;
    else if (c === "}" || c === "]") {
      depth--;
      if (depth <= 0) return null;
    } else if (c === "," && depth === 1) expectKey = true;
    else if (depth === 1 && !/\s/.test(c) && !(expectKey && /[_0-9A-Za-z$]/.test(c))) expectKey = false;
    i++;
  }
  if (depth !== 1 || !expectKey) return null;
  // A bare word typed at a key position (no quote yet) is replaced too.
  let from = pos;
  while (from > 0 && /[_0-9A-Za-z$]/.test(text[from - 1])) from--;
  return { from, to: pos, prefix: text.slice(from, pos).replace(/^\$/, ""), wrap: false, hasColon: false };
}

/** Text a variable completion inserts. */
export function variableCompletionText(name: string, spot: Pick<KeyCompletionSpot, "wrap" | "hasColon">): string {
  if (spot.hasColon) return JSON.stringify(name);
  const entry = `${JSON.stringify(name)}: `;
  return spot.wrap ? `{\n  ${entry}\n}` : entry;
}

/**
 * Check variables JSON against the operation's definitions (AC-GQL-8). Invalid JSON gives no
 * diagnostics here (the editor shows its own hint, AC-REQ-5). `check` (schema-aware, from the
 * lazy chunk) returns a type error for one value, or null.
 */
export function validateVariables(
  text: string,
  defs: VariableDef[],
  operationLabel: string,
  check?: (def: VariableDef, value: unknown) => string | null,
): Diagnostic[] {
  const out: Diagnostic[] = [];
  const trimmed = text.trim();
  let value: unknown = {};
  if (trimmed) {
    try {
      value = JSON.parse(text);
    } catch {
      return [];
    }
  }
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    const start = text.length - text.trimStart().length;
    return [{ from: start, to: start + trimmed.length, severity: "error", message: "Variables must be a JSON object" }];
  }
  const keys = trimmed ? (jsonTopLevelKeys(text) ?? []) : [];
  const obj = value as Record<string, unknown>;
  const byName = new Map(defs.map((d) => [d.name, d]));
  for (const k of keys) {
    const def = byName.get(k.name);
    if (!def) {
      out.push({ from: k.from, to: k.to, severity: "warning", message: `Variable "$${k.name}" isn't defined by ${operationLabel}` });
      continue;
    }
    const v = obj[k.name];
    if (check && !containsTemplate(v)) {
      const problem = check(def, v);
      if (problem) out.push({ from: k.valueFrom, to: Math.max(k.valueTo, k.valueFrom), severity: "error", message: problem });
    }
  }
  const brace = text.length - text.trimStart().length;
  for (const d of defs) {
    if (d.required && !(d.name in obj)) {
      out.push({ from: brace, to: trimmed ? brace + 1 : brace, severity: "error", message: `Missing required variable "$${d.name}" (${d.type})` });
    }
  }
  return out;
}

/** True when a value holds a `{{variable}}` somewhere: it resolves at send time, so its type isn't checked. */
function containsTemplate(v: unknown): boolean {
  if (typeof v === "string") return v.includes("{{");
  if (Array.isArray(v)) return v.some(containsTemplate);
  if (v && typeof v === "object") return Object.values(v).some(containsTemplate);
  return false;
}

/** "operation “GetUser”" / "the query" — for messages. */
export function operationLabel(name: string | null | undefined): string {
  return name ? `operation "${name}"` : "the query";
}

// ───────────── Schema model (AC-GQL-10) ─────────────
// A plain-data view of a schema, built in the lazy chunk (../graphql/gql.ts) so the explorer
// and these helpers need no graphql-js.

export type TypeKind = "OBJECT" | "INTERFACE" | "UNION" | "ENUM" | "INPUT_OBJECT" | "SCALAR";

export interface ArgModel {
  name: string;
  /** As printed: "ID!", "[String!]". */
  type: string;
  /** The named type under lists / non-null: "ID". */
  namedType: string;
  description: string | null;
  defaultValue: string | null;
  /** Non-null without a default. */
  required: boolean;
  deprecated: string | null;
}

export interface FieldModel {
  name: string;
  type: string;
  namedType: string;
  description: string | null;
  args: ArgModel[];
  /** Deprecation reason ("" when deprecated without one), null when not deprecated. */
  deprecated: string | null;
  /** The named type is an object, interface or union: selecting it needs `{ … }`. */
  composite: boolean;
}

export interface EnumValueModel {
  name: string;
  description: string | null;
  deprecated: string | null;
}

export interface TypeModel {
  name: string;
  kind: TypeKind;
  description: string | null;
  /** Object / interface fields, or input-object fields (args empty). */
  fields: FieldModel[];
  interfaces: string[];
  possibleTypes: string[];
  enumValues: EnumValueModel[];
}

export interface SchemaModel {
  queryType: string | null;
  mutationType: string | null;
  subscriptionType: string | null;
  /** Named types without the introspection ones (`__…`), sorted by name. */
  types: TypeModel[];
  byName: Record<string, TypeModel>;
}

export const KIND_LABEL: Record<TypeKind, string> = {
  OBJECT: "type",
  INTERFACE: "interface",
  UNION: "union",
  ENUM: "enum",
  INPUT_OBJECT: "input",
  SCALAR: "scalar",
};

/** Root operation types that exist, in order. */
export function rootTypes(m: SchemaModel): { operation: OperationType; type: string }[] {
  const out: { operation: OperationType; type: string }[] = [];
  if (m.queryType) out.push({ operation: "query", type: m.queryType });
  if (m.mutationType) out.push({ operation: "mutation", type: m.mutationType });
  if (m.subscriptionType) out.push({ operation: "subscription", type: m.subscriptionType });
  return out;
}

export interface SearchHit {
  /** The type to open. */
  type: string;
  /** The field (or argument's field / enum value) to highlight, if the match is below the type. */
  field: string | null;
  /** Argument name when an argument matched. */
  arg: string | null;
  /** "User", "User.email", "Query.user(id)" */
  label: string;
  /** Kind or the field's type, shown dimmed. */
  detail: string;
}

/**
 * Search type, field, argument and enum value names (AC-GQL-10): case-insensitive substring;
 * exact, then prefix, then other matches; types before fields before arguments; at most `limit`.
 */
export function searchSchema(m: SchemaModel, term: string, limit = 50): SearchHit[] {
  const q = term.trim().toLowerCase();
  if (!q) return [];
  const scored: { hit: SearchHit; score: number }[] = [];
  const rank = (name: string, tier: number) => {
    const n = name.toLowerCase();
    if (!n.includes(q)) return -1;
    return (n === q ? 0 : n.startsWith(q) ? 1 : 2) * 3 + tier;
  };
  for (const t of m.types) {
    let s = rank(t.name, 0);
    if (s >= 0) scored.push({ hit: { type: t.name, field: null, arg: null, label: t.name, detail: KIND_LABEL[t.kind] }, score: s });
    for (const f of t.fields) {
      s = rank(f.name, 1);
      if (s >= 0) scored.push({ hit: { type: t.name, field: f.name, arg: null, label: `${t.name}.${f.name}`, detail: f.type }, score: s });
      for (const a of f.args) {
        s = rank(a.name, 2);
        if (s >= 0) scored.push({ hit: { type: t.name, field: f.name, arg: a.name, label: `${t.name}.${f.name}(${a.name})`, detail: a.type }, score: s });
      }
    }
    for (const v of t.enumValues) {
      s = rank(v.name, 1);
      if (s >= 0) scored.push({ hit: { type: t.name, field: v.name, arg: null, label: `${t.name}.${v.name}`, detail: "enum value" }, score: s });
    }
  }
  scored.sort((a, b) => a.score - b.score || a.hit.label.localeCompare(b.hit.label));
  return scored.slice(0, limit).map((x) => x.hit);
}

/** "(id: ID!, first: Int = 10)" or "" */
export function argsSignature(args: ArgModel[]): string {
  if (!args.length) return "";
  return `(${args.map((a) => `${a.name}: ${a.type}${a.defaultValue !== null ? ` = ${a.defaultValue}` : ""}`).join(", ")})`;
}

/**
 * What "Insert" puts at the query caret for a field (AC-GQL-10): its name, required arguments
 * as `name(arg: )` and `{ }` for an object result. `caret` is where the cursor goes, relative to
 * the start of `text`: after the first argument's colon, else inside the braces, else the end.
 */
export function insertTextForField(f: FieldModel): { text: string; caret: number } {
  const required = f.args.filter((a) => a.required);
  let text = f.name;
  let caret = -1;
  if (required.length) {
    text += "(";
    required.forEach((a, i) => {
      if (i) text += ", ";
      text += `${a.name}: `;
      if (caret < 0) caret = text.length;
    });
    text += ")";
  }
  if (f.composite) {
    text += " {  }";
    if (caret < 0) caret = text.length - 2;
  }
  return { text, caret: caret < 0 ? text.length : caret };
}

import type { DynamicVariableInfo, VariableInfo } from "../api/types";
import { dynamicCatalogue } from "./dynamic";

/**
 * `{{ name }}` — name in [A-Za-z0-9_.-]+, optionally led by `$` (a dynamic
 * variable, AC-ENV-8); whitespace inside braces trimmed (AC-ENV-1).
 */
export const VAR_RE = /\{\{\s*(\$?[A-Za-z0-9_.-]+)\s*\}\}/g;

export type VarSegment =
  | { kind: "text"; text: string; start: number; end: number }
  | { kind: "var"; text: string; name: string; info: VariableInfo; start: number; end: number }
  | { kind: "dynamic"; text: string; name: string; dyn: DynamicVariableInfo; start: number; end: number }
  | { kind: "unresolved"; text: string; name: string; start: number; end: number };

export type VarLookup = ReadonlyMap<string, VariableInfo>;

/**
 * Dynamic-variable catalogue by name. Starts as the built-in copy (same as
 * core) and is replaced by `get_dynamic_variables` at bootstrap.
 */
let dynamicByName = new Map<string, DynamicVariableInfo>(dynamicCatalogue().map((d) => [d.name, d]));

export function setDynamicCatalogue(list: readonly DynamicVariableInfo[]): void {
  if (list.length) dynamicByName = new Map(list.map((d) => [d.name, d]));
}

export function dynamicVariables(): DynamicVariableInfo[] {
  return [...dynamicByName.values()];
}

export function dynamicInfo(name: string): DynamicVariableInfo | undefined {
  return dynamicByName.get(name);
}

/** Hover text for a dynamic variable. */
export function dynamicTip(d: DynamicVariableInfo): string {
  return `${d.description} · e.g. ${d.example} · new value each send`;
}

/** Environment variable names may not start with `$` (reserved for dynamic variables). */
export function isReservedVarName(key: string): boolean {
  return key.trimStart().startsWith("$");
}

/** First definition per key wins (get_variables is already ordered project env -> Global). */
export function makeLookup(vars: readonly VariableInfo[]): Map<string, VariableInfo> {
  const m = new Map<string, VariableInfo>();
  for (const v of vars) if (!m.has(v.key)) m.set(v.key, v);
  return m;
}

/** Split text into plain / resolved-var / unresolved-var segments for highlighting. */
export function tokenizeVars(text: string, lookup: VarLookup): VarSegment[] {
  const out: VarSegment[] = [];
  let last = 0;
  for (const m of text.matchAll(VAR_RE)) {
    const start = m.index!;
    const end = start + m[0].length;
    if (start > last) out.push({ kind: "text", text: text.slice(last, start), start: last, end: start });
    const name = m[1];
    const dyn = name.startsWith("$") ? dynamicByName.get(name) : undefined;
    const info = name.startsWith("$") ? undefined : lookup.get(name);
    out.push(
      dyn
        ? { kind: "dynamic", text: m[0], name, dyn, start, end }
        : info
          ? { kind: "var", text: m[0], name, info, start, end }
          : { kind: "unresolved", text: m[0], name, start, end },
    );
    last = end;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last), start: last, end: text.length });
  return out;
}

/** Whether `{{name}}` resolves (an environment variable or a known dynamic one). */
export function isResolvable(name: string, lookup: VarLookup): boolean {
  return name.startsWith("$") ? dynamicByName.has(name) : lookup.has(name);
}

/** Names of unresolved variables in the given texts (deduplicated, in order). */
export function unresolvedNames(texts: readonly string[], lookup: VarLookup): string[] {
  const seen = new Set<string>();
  for (const t of texts) for (const m of t.matchAll(VAR_RE)) if (!isResolvable(m[1], lookup)) seen.add(m[1]);
  return [...seen];
}

/** Segment at a character offset (for hover tooltips). */
export function segmentAt(segs: readonly VarSegment[], offset: number): VarSegment | null {
  for (const s of segs) if (offset >= s.start && offset < s.end) return s;
  return null;
}

export const maskSecret = (v: string): string => (v ? "••••••••" : "");

export interface VarCompletion {
  /** Name as typed inside the braces, e.g. "baseUrl" or "$randomEmail". */
  label: string;
  /** Text that replaces [from, to): the name plus the closing braces. */
  insert: string;
  /** "https://api.dev · Staging", or the dynamic variable's description. */
  detail: string;
  kind: "var" | "dynamic";
}

export interface VarCompletionResult {
  /** Replace text[from, to) — the partial name and any closing braces already typed. */
  from: number;
  to: number;
  options: VarCompletion[];
}

/**
 * `{{` autocomplete: `before` is the text up to the caret, `after` the text
 * following it. Environment variables first (project env, then Global), then
 * the dynamic catalogue; prefix matches rank before substring matches.
 * Returns null when the caret is not inside an open `{{`.
 */
export function completeVar(before: string, after: string, lookup: VarLookup, limit = 50): VarCompletionResult | null {
  const m = /\{\{\s*(\$?[A-Za-z0-9_.-]*)$/.exec(before);
  if (!m) return null;
  const query = m[1];
  const from = before.length - query.length;
  const close = /^[A-Za-z0-9_.-]*\s*\}\}/.exec(after);
  const to = before.length + (close ? close[0].length : 0);
  const q = query.toLowerCase();
  const qBare = q.replace(/^\$/, "");
  const rank = (name: string): number => {
    const n = name.toLowerCase();
    if (n.startsWith(q)) return 0;
    const bare = n.replace(/^\$/, "");
    if (bare.startsWith(qBare)) return 1;
    return bare.includes(qBare) ? 2 : -1;
  };
  const vars: (VarCompletion & { r: number })[] = [];
  if (!query.startsWith("$")) {
    for (const v of lookup.values()) {
      const r = rank(v.key);
      if (r < 0 || isReservedVarName(v.key)) continue;
      vars.push({
        label: v.key,
        insert: `${v.key}}}`,
        detail: `${v.secret ? maskSecret(v.value) : v.value || "(empty)"} · ${v.environmentName}`,
        kind: "var",
        r,
      });
    }
  }
  const dyn: (VarCompletion & { r: number })[] = [];
  for (const d of dynamicByName.values()) {
    const r = rank(d.name);
    if (r < 0) continue;
    dyn.push({ label: d.name, insert: `${d.name}}}`, detail: d.description, kind: "dynamic", r });
  }
  const byRank = (a: { r: number }, b: { r: number }) => a.r - b.r;
  const options = [...vars.sort(byRank), ...dyn.sort(byRank)].slice(0, limit).map(({ label, insert, detail, kind }) => ({ label, insert, detail, kind }));
  return options.length ? { from, to, options } : null;
}

import type { VariableInfo } from "../api/types";

/** `{{ name }}` — name in [A-Za-z0-9_.-]+, whitespace inside braces trimmed (AC-ENV-1). */
export const VAR_RE = /\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}/g;

export type VarSegment =
  | { kind: "text"; text: string; start: number; end: number }
  | { kind: "var"; text: string; name: string; info: VariableInfo; start: number; end: number }
  | { kind: "unresolved"; text: string; name: string; start: number; end: number };

export type VarLookup = ReadonlyMap<string, VariableInfo>;

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
    const info = lookup.get(name);
    out.push(
      info
        ? { kind: "var", text: m[0], name, info, start, end }
        : { kind: "unresolved", text: m[0], name, start, end },
    );
    last = end;
  }
  if (last < text.length) out.push({ kind: "text", text: text.slice(last), start: last, end: text.length });
  return out;
}

/** Names of unresolved variables in the given texts (deduplicated, in order). */
export function unresolvedNames(texts: readonly string[], lookup: VarLookup): string[] {
  const seen = new Set<string>();
  for (const t of texts) for (const m of t.matchAll(VAR_RE)) if (!lookup.has(m[1])) seen.add(m[1]);
  return [...seen];
}

/** Segment at a character offset (for hover tooltips). */
export function segmentAt(segs: readonly VarSegment[], offset: number): VarSegment | null {
  for (const s of segs) if (offset >= s.start && offset < s.end) return s;
  return null;
}

export const maskSecret = (v: string): string => (v ? "••••••••" : "");

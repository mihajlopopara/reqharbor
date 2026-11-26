/** Split text into plain / matched parts for <mark> highlighting (case-insensitive). */
export function highlightParts(text: string, query: string): { t: string; m: boolean }[] {
  const q = query.trim().toLowerCase();
  if (!q) return [{ t: text, m: false }];
  const lower = text.toLowerCase();
  const out: { t: string; m: boolean }[] = [];
  let i = 0;
  while (i < text.length) {
    const j = lower.indexOf(q, i);
    if (j < 0) {
      out.push({ t: text.slice(i), m: false });
      break;
    }
    if (j > i) out.push({ t: text.slice(i, j), m: false });
    out.push({ t: text.slice(j, j + q.length), m: true });
    i = j + q.length;
  }
  return out;
}

/** AC-HIST-3: match URL, method or status code. */
export function historyMatches(h: { resolvedUrl: string; method: string; status: number | null; errorKind: string | null }, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return (
    h.resolvedUrl.toLowerCase().includes(q) ||
    h.method.toLowerCase().includes(q) ||
    (h.status !== null && String(h.status).includes(q)) ||
    (h.errorKind !== null && h.errorKind.includes(q))
  );
}

/**
 * Literal-preserving JSON pretty-printer (2 spaces). Numbers, escapes and key
 * order are kept byte-for-byte, so it is safe for response bodies (no
 * precision loss on big integers). Linear time; ~1 MB in a few ms.
 * Returns null if the text is not structurally valid JSON.
 */
export function prettyJson(text: string): string | null {
  try {
    JSON.parse(text);
  } catch {
    return null;
  }
  const out: string[] = [];
  let indent = 0;
  let i = 0;
  const n = text.length;
  const pad = (k: number) => "\n" + "  ".repeat(k);
  let buf = "";
  const flush = () => {
    if (buf.length > 65536) {
      out.push(buf);
      buf = "";
    }
  };
  while (i < n) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < n) {
        const d = text[j];
        if (d === "\\") j += 2;
        else if (d === '"') break;
        else j++;
      }
      buf += text.slice(i, j + 1);
      i = j + 1;
      flush();
      continue;
    }
    if (c === " " || c === "\n" || c === "\r" || c === "\t") {
      i++;
      continue;
    }
    if (c === "{" || c === "[") {
      // empty container stays inline
      let j = i + 1;
      while (j < n && /\s/.test(text[j])) j++;
      if (text[j] === (c === "{" ? "}" : "]")) {
        buf += c + text[j];
        i = j + 1;
        continue;
      }
      indent++;
      buf += c + pad(indent);
    } else if (c === "}" || c === "]") {
      indent--;
      buf += pad(indent) + c;
    } else if (c === ",") {
      buf += "," + pad(indent);
    } else if (c === ":") {
      buf += ": ";
    } else {
      let j = i;
      while (j < n && !',:{}[]" \n\r\t'.includes(text[j])) j++;
      buf += text.slice(i, j);
      i = j;
      flush();
      continue;
    }
    i++;
    flush();
  }
  out.push(buf);
  return out.join("");
}

/** Best-effort line/column (1-based) of a JSON.parse error. */
export function jsonErrorPosition(text: string, err: unknown): { line: number; column: number; message: string } {
  const msg = err instanceof Error ? err.message : String(err);
  const lc = /line (\d+) column (\d+)/.exec(msg);
  if (lc) return { line: Number(lc[1]), column: Number(lc[2]), message: msg };
  const pos = /position (\d+)/.exec(msg);
  let offset = pos ? Number(pos[1]) : text.length;
  offset = Math.min(offset, text.length);
  const before = text.slice(0, offset);
  const line = before.split("\n").length;
  const column = offset - before.lastIndexOf("\n");
  return { line, column, message: msg };
}

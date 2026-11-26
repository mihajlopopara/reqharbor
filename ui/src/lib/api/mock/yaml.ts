/**
 * Minimal YAML subset parser for the browser mock (OpenAPI specs): block
 * mappings and sequences, plain/quoted scalars, flow collections that are
 * valid JSON-ish, block scalars (| and >), comments, anchors/aliases and `<<`
 * merge keys. Throws Error on anything it cannot read.
 */
interface Line {
  indent: number;
  text: string;
  no: number;
}

export function parseYaml(src: string): unknown {
  const lines: Line[] = [];
  src.split(/\r?\n/).forEach((raw, i) => {
    if (/^\s*(#.*)?$/.test(raw) || raw.startsWith("---") || raw.startsWith("...")) return;
    if (/^\t/.test(raw)) throw new Error(`Tabs are not allowed for indentation (line ${i + 1})`);
    const indent = raw.length - raw.trimStart().length;
    lines.push({ indent, text: raw.slice(indent), no: i + 1 });
  });
  const rawLines = src.split(/\r?\n/);
  const anchors = new Map<string, unknown>();
  let pos = 0;

  function stripComment(s: string): string {
    let inS = false;
    let inD = false;
    for (let i = 0; i < s.length; i++) {
      const c = s[i];
      if (c === "'" && !inD) inS = !inS;
      else if (c === '"' && !inS && s[i - 1] !== "\\") inD = !inD;
      else if (c === "#" && !inS && !inD && (i === 0 || /\s/.test(s[i - 1]))) return s.slice(0, i).trimEnd();
    }
    return s.trimEnd();
  }

  function scalarValue(s: string): unknown {
    if (s === "" || s === "~" || s === "null" || s === "Null" || s === "NULL") return null;
    if (s === "true" || s === "True") return true;
    if (s === "false" || s === "False") return false;
    if (/^[-+]?(\d+\.\d*|\.\d+|\d+)([eE][-+]?\d+)?$/.test(s)) return Number(s);
    return s;
  }

  function scalar(raw: string): unknown {
    let s = stripComment(raw).trim();
    let anchor: string | null = null;
    const am = /^&([\w.-]+)\s*(.*)$/.exec(s);
    if (am) {
      anchor = am[1];
      s = am[2];
    }
    let v: unknown;
    if (s.startsWith("*")) {
      const name = s.slice(1);
      if (!anchors.has(name)) throw new Error(`Unknown alias *${name}`);
      v = anchors.get(name);
    } else if (s === "" || s === "~" || s === "null" || s === "Null" || s === "NULL") v = null;
    else if (s === "true" || s === "True") v = true;
    else if (s === "false" || s === "False") v = false;
    else if (/^[-+]?\d+$/.test(s)) v = Number(s);
    else if (/^[-+]?(\d+\.\d*|\.\d+|\d+)([eE][-+]?\d+)?$/.test(s)) v = Number(s);
    else if (s.startsWith('"')) {
      if (!s.endsWith('"') || s.length < 2) throw new Error(`Unterminated string: ${s}`);
      v = JSON.parse(s);
    } else if (s.startsWith("'")) {
      if (!s.endsWith("'") || s.length < 2) throw new Error(`Unterminated string: ${s}`);
      v = s.slice(1, -1).replace(/''/g, "'");
    } else if (s.startsWith("[") || s.startsWith("{")) v = flow(completeFlow(s));
    else v = s;
    if (anchor) anchors.set(anchor, v);
    return v;
  }

  /** Flow collections: `{ a: 1, b: [x, 'y z'] }`, nested, with quoted and plain scalars. */
  function flow(src: string): unknown {
    let i = 0;
    const ws = () => {
      while (i < src.length && /\s/.test(src[i])) i++;
    };
    const quoted = (): string => {
      const q = src[i];
      if (q === '"') {
        let j = i + 1;
        while (j < src.length && src[j] !== '"') j += src[j] === "\\" ? 2 : 1;
        if (j >= src.length) throw new Error(`Unterminated string in ${src}`);
        const out = JSON.parse(src.slice(i, j + 1)) as string;
        i = j + 1;
        return out;
      }
      let out = "";
      i++;
      for (;;) {
        if (i >= src.length) throw new Error(`Unterminated string in ${src}`);
        if (src[i] === "'") {
          if (src[i + 1] === "'") {
            out += "'";
            i += 2;
            continue;
          }
          i++;
          return out;
        }
        out += src[i++];
      }
    };
    const plain = (isKey: boolean): string => {
      const startAt = i;
      while (i < src.length) {
        const c = src[i];
        if (c === "," || c === "}" || c === "]") break;
        if (isKey && c === ":" && (i + 1 >= src.length || /[\s,}\]]/.test(src[i + 1]))) break;
        i++;
      }
      return src.slice(startAt, i).trim();
    };
    const value = (): unknown => {
      ws();
      const c = src[i];
      if (c === "{") {
        i++;
        const obj: Record<string, unknown> = {};
        for (;;) {
          ws();
          if (src[i] === "}") {
            i++;
            return obj;
          }
          const key = src[i] === '"' || src[i] === "'" ? quoted() : plain(true);
          ws();
          let v: unknown = null;
          if (src[i] === ":") {
            i++;
            ws();
            v = src[i] === "," || src[i] === "}" ? null : value();
          }
          obj[key] = v;
          ws();
          if (src[i] === ",") i++;
          else if (src[i] !== "}") throw new Error(`Bad flow mapping: ${src}`);
        }
      }
      if (c === "[") {
        i++;
        const arr: unknown[] = [];
        for (;;) {
          ws();
          if (src[i] === "]") {
            i++;
            return arr;
          }
          arr.push(value());
          ws();
          if (src[i] === ",") i++;
          else if (src[i] !== "]") throw new Error(`Bad flow sequence: ${src}`);
        }
      }
      if (c === '"' || c === "'") return quoted();
      return scalarValue(plain(false));
    };
    const out = value();
    ws();
    if (i < src.length) throw new Error(`Unexpected trailing content: ${src.slice(i)}`);
    return out;
  }

  /** Join continuation lines of a flow collection that spans several lines. */
  function completeFlow(first: string): string {
    let text = first;
    const depth = (t: string) => {
      let d = 0;
      let q: string | null = null;
      for (const ch of t) {
        if (q) {
          if (ch === q) q = null;
        } else if (ch === '"' || ch === "'") q = ch;
        else if (ch === "{" || ch === "[") d++;
        else if (ch === "}" || ch === "]") d--;
      }
      return d;
    };
    while (depth(text) > 0 && pos < lines.length) {
      text += " " + stripComment(lines[pos].text);
      pos++;
    }
    return text;
  }

  function blockScalar(indicator: string, parentIndent: number): string {
    const folded = indicator.startsWith(">");
    const keep = indicator.includes("+");
    const strip = indicator.includes("-");
    const startLine = pos < lines.length ? lines[pos].no : rawLines.length + 1;
    const body: string[] = [];
    let blockIndent = -1;
    let ln = startLine - 1;
    while (ln < rawLines.length) {
      const raw = rawLines[ln];
      if (raw.trim() === "") {
        body.push("");
        ln++;
        continue;
      }
      const ind = raw.length - raw.trimStart().length;
      if (ind <= parentIndent) break;
      if (blockIndent < 0) blockIndent = ind;
      if (ind < blockIndent) break;
      body.push(raw.slice(blockIndent));
      ln++;
    }
    while (pos < lines.length && lines[pos].no <= ln) pos++;
    while (body.length && body[body.length - 1] === "" && !keep) body.pop();
    let text = folded ? body.join("\n").replace(/([^\n])\n(?=[^\n])/g, "$1 ") : body.join("\n");
    if (!strip) text += "\n";
    return text;
  }

  function parseNode(indent: number): unknown {
    if (pos >= lines.length) return null;
    const l = lines[pos];
    if (l.indent < indent) return null;
    if (l.text.startsWith("- ") || l.text === "-") return parseSeq(l.indent);
    return parseMap(l.indent);
  }

  function valueAfter(rest: string, ownIndent: number): unknown {
    const r = stripComment(rest).trim();
    if (r === "" || /^&[\w.-]+$/.test(r)) {
      const anchor = r ? r.slice(1) : null;
      let v: unknown = null;
      if (pos < lines.length && lines[pos].indent > ownIndent) v = parseNode(lines[pos].indent);
      else if (pos < lines.length && lines[pos].indent === ownIndent && lines[pos].text.startsWith("- ")) {
        v = parseSeq(ownIndent);
      }
      if (anchor) anchors.set(anchor, v);
      return v;
    }
    if (/^[|>][-+]?\d*$/.test(r)) return blockScalar(r, ownIndent);
    return scalar(rest);
  }

  function parseMap(indent: number): Record<string, unknown> {
    const obj: Record<string, unknown> = {};
    while (pos < lines.length && lines[pos].indent === indent && !lines[pos].text.startsWith("- ")) {
      const l = lines[pos];
      const m = /^("(?:[^"\\]|\\.)*"|'(?:[^']|'')*'|[^:#][^:]*?)\s*:(?:\s+(.*))?$/.exec(l.text) ?? /^(.+?):()$/.exec(l.text);
      if (!m) throw new Error(`Expected "key: value" at line ${l.no}`);
      let key = m[1];
      if (key.startsWith('"')) key = JSON.parse(key);
      else if (key.startsWith("'")) key = key.slice(1, -1).replace(/''/g, "'");
      pos++;
      const v = valueAfter(m[2] ?? "", indent);
      if (key === "<<" && v && typeof v === "object") {
        for (const src of Array.isArray(v) ? v : [v]) {
          for (const [k, val] of Object.entries(src as Record<string, unknown>)) if (!(k in obj)) obj[k] = val;
        }
      } else obj[key] = v;
    }
    if (pos < lines.length && lines[pos].indent > indent) throw new Error(`Bad indentation at line ${lines[pos].no}`);
    return obj;
  }

  function parseSeq(indent: number): unknown[] {
    const arr: unknown[] = [];
    while (pos < lines.length && lines[pos].indent === indent && (lines[pos].text.startsWith("- ") || lines[pos].text === "-")) {
      const l = lines[pos];
      const rest = l.text === "-" ? "" : l.text.slice(2);
      const restTrim = rest.trimStart();
      if (rest.trim() === "") {
        pos++;
        arr.push(pos < lines.length && lines[pos].indent > indent ? parseNode(lines[pos].indent) : null);
      } else if (/^("[^"]*"|'[^']*'|[^\s"'[{][^:]*?)\s*:(\s|$)/.test(restTrim) && !restTrim.startsWith("{")) {
        // inline mapping start: re-enter as a map at the item's content indent
        const childIndent = indent + 2 + (rest.length - restTrim.length);
        lines[pos] = { indent: childIndent, text: restTrim, no: l.no };
        arr.push(parseMap(childIndent));
      } else {
        pos++;
        arr.push(valueAfter(rest, indent));
      }
    }
    return arr;
  }

  if (lines.length === 0) return null;
  const first = lines[0];
  if (first.indent === 0 && !first.text.includes(":") && !first.text.startsWith("-")) return scalar(first.text);
  const result = parseNode(0);
  if (pos < lines.length) throw new Error(`Unexpected content at line ${lines[pos].no}`);
  return result;
}

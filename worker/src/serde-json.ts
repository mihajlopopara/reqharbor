// Minimal JSON parse -> compact re-serialize that reproduces `serde_json` (with `preserve_order`)
// byte for byte, so the share page's GraphQL cURL body matches the Rust exporter (QA-R1-006).
// Differences from JSON.parse/JSON.stringify that matter here:
//  - object keys keep insertion order (no integer-key reordering); a duplicate key replaces the
//    value but keeps the first key's position (IndexMap::insert);
//  - integers that fit u64/i64 are printed exactly; other numbers are parsed with serde_json's
//    default (not correctly rounded) algorithm and printed like serde_json 1.0.154
//    (`1.0`, `1e+21`, `1e-7`, `-0.0`); out-of-range numbers are an error;
//  - strings escape only `"`, `\\` and control characters (`\u001f` style, lowercase hex);
//  - nesting deeper than 128 is an error (serde_json's recursion limit).

type Value =
  | { t: "null" }
  | { t: "bool"; v: boolean }
  | { t: "num"; s: string }
  | { t: "str"; v: string }
  | { t: "arr"; v: Value[] }
  | { t: "obj"; v: [string, Value][] };

export class JsonError extends Error {
  constructor(
    message: string,
    readonly line: number,
    readonly column: number,
  ) {
    super(message);
  }
}

const MAX_DEPTH = 128;
const U64_MAX = (1n << 64n) - 1n;
const I64_MIN_MAG = 1n << 63n;

function parse(text: string): Value {
  let i = 0;
  const fail = (msg: string): never => {
    const before = text.slice(0, i);
    const line = before.split("\n").length;
    const column = i - (before.lastIndexOf("\n") + 1) + 1;
    throw new JsonError(`${msg} at line ${line} column ${column}`, line, column);
  };
  const ws = () => {
    while (i < text.length && (text[i] === " " || text[i] === "\t" || text[i] === "\n" || text[i] === "\r")) i++;
  };
  const value = (depth: number): Value => {
    ws();
    const c = text[i];
    if (c === undefined) return fail("EOF while parsing a value");
    if (c === "{" || c === "[") {
      if (depth >= MAX_DEPTH) fail("recursion limit exceeded");
      i++;
      ws();
      if (c === "[") {
        const items: Value[] = [];
        if (text[i] === "]") {
          i++;
          return { t: "arr", v: items };
        }
        for (;;) {
          items.push(value(depth + 1));
          ws();
          if (text[i] === ",") i++;
          else if (text[i] === "]") {
            i++;
            return { t: "arr", v: items };
          } else fail("expected `,` or `]`");
        }
      }
      const entries: [string, Value][] = [];
      if (text[i] === "}") {
        i++;
        return { t: "obj", v: entries };
      }
      for (;;) {
        ws();
        if (text[i] !== '"') fail("key must be a string");
        const key = string();
        ws();
        if (text[i] !== ":") fail("expected `:`");
        i++;
        const v = value(depth + 1);
        const at = entries.findIndex(([k]) => k === key);
        if (at === -1) entries.push([key, v]);
        else entries[at] = [key, v];
        ws();
        if (text[i] === ",") i++;
        else if (text[i] === "}") {
          i++;
          return { t: "obj", v: entries };
        } else fail("expected `,` or `}`");
      }
    }
    if (c === '"') return { t: "str", v: string() };
    for (const [lit, v] of [["true", { t: "bool", v: true }], ["false", { t: "bool", v: false }], ["null", { t: "null" }]] as const) {
      if (text.startsWith(lit, i)) {
        i += lit.length;
        return v as Value;
      }
    }
    const m = /^-?(0|[1-9]\d*)(\.\d+)?([eE][+-]?\d+)?/.exec(text.slice(i));
    if (!m || m[0] === "-") return fail("expected value");
    i += m[0].length;
    return { t: "num", s: m[0] };
  };
  const string = (): string => {
    i++; // opening quote
    let out = "";
    for (;;) {
      const c = text[i];
      if (c === undefined) return fail("EOF while parsing a string");
      if (c === '"') {
        i++;
        return out;
      }
      if (c < " ") fail("control character (\\u0000-\\u001F) found while parsing a string");
      if (c !== "\\") {
        out += c;
        i++;
        continue;
      }
      const e = text[i + 1];
      i += 2;
      const simple: Record<string, string> = { '"': '"', "\\": "\\", "/": "/", b: "\b", f: "\f", n: "\n", r: "\r", t: "\t" };
      if (e !== undefined && e in simple) {
        out += simple[e];
        continue;
      }
      if (e !== "u") fail("invalid escape");
      const hex = (): number => {
        const h = text.slice(i, i + 4);
        if (!/^[0-9a-fA-F]{4}$/.test(h)) fail("invalid escape");
        i += 4;
        return parseInt(h, 16);
      };
      const cu = hex();
      if (cu >= 0xdc00 && cu <= 0xdfff) fail("lone leading surrogate in hex escape");
      if (cu >= 0xd800 && cu <= 0xdbff) {
        if (text[i] !== "\\" || text[i + 1] !== "u") fail("unexpected end of hex escape");
        i += 2;
        const lo = hex();
        if (lo < 0xdc00 || lo > 0xdfff) fail("lone leading surrogate in hex escape");
        out += String.fromCharCode(cu, lo);
      } else out += String.fromCharCode(cu);
    }
  };
  const v = value(0);
  ws();
  if (i < text.length) fail("trailing characters");
  return v;
}

/**
 * Float formatting of serde_json 1.0.154 (shortest round-trip digits): plain decimal with a
 * trailing `.0` while -5 < kk <= 16 (10^(kk-1) <= |f| < 10^kk), otherwise `d.ddde+X` / `de-X`.
 * Verified against the Rust exporter by test/fixtures/curl_parity.json.
 */
export function formatF64(f: number): string {
  if (!Number.isFinite(f)) throw new Error("non-finite");
  const sign = f < 0 || Object.is(f, -0) ? "-" : "";
  if (f === 0) return sign + "0.0";
  const [mant, expStr] = Math.abs(f).toExponential().split("e") as [string, string];
  const digits = mant.replace(".", "");
  const length = digits.length;
  const kk = Number(expStr) + 1;
  const k = kk - length;
  const e = kk - 1;
  const exp = "e" + (e < 0 ? "-" : "+") + Math.abs(e);
  let out: string;
  if (k >= 0 && kk <= 16) out = digits + "0".repeat(k) + ".0";
  else if (kk > 0 && kk <= 16) out = digits.slice(0, kk) + "." + digits.slice(kk);
  else if (kk > -5 && kk <= 0) out = "0." + "0".repeat(-kk) + digits;
  else if (length === 1) out = digits + exp;
  else out = digits[0] + "." + digits.slice(1) + exp;
  return sign + out;
}

const POW10 = Array.from({ length: 309 }, (_, i) => Number(`1e${i}`));
const I32_MAX = 2147483647;
const I32_MIN = -2147483648;
const overflowU64 = (sig: bigint, d: bigint) => sig >= U64_MAX / 10n && (sig > U64_MAX / 10n || d > U64_MAX % 10n);
const outOfRange = () => new JsonError("number out of range", 1, 1);

/** serde_json's default (non-`float_roundtrip`) `f64_from_parts`. */
function f64FromParts(significand: bigint, exponent: number): number {
  let f = Number(significand);
  for (;;) {
    const idx = Math.abs(exponent);
    if (idx <= 308) {
      if (exponent >= 0) {
        f *= POW10[idx]!;
        if (!Number.isFinite(f)) throw outOfRange();
      } else f /= POW10[idx]!;
      return f;
    }
    if (f === 0) return f;
    if (exponent >= 0) throw outOfRange();
    f /= 1e308;
    exponent += 308;
  }
}

/** serde_json number lexeme -> Value -> serialized text (parse_integer/decimal/exponent paths). */
function number(lexeme: string): string {
  let i = 0;
  const positive = lexeme[0] !== "-";
  if (!positive) i++;
  const isDigit = (c: string | undefined) => c !== undefined && c >= "0" && c <= "9";
  let sig = 0n;
  let exponent = 0;
  let isFloat = false;
  if (lexeme[i] === "0") i++;
  else {
    while (isDigit(lexeme[i])) {
      const d = BigInt(lexeme.charCodeAt(i) - 48);
      if (overflowU64(sig, d)) {
        isFloat = true; // parse_long_integer: remaining integer digits only scale the exponent
        while (isDigit(lexeme[i])) {
          i++;
          exponent++;
        }
        break;
      }
      sig = sig * 10n + d;
      i++;
    }
  }
  if (lexeme[i] === ".") {
    isFloat = true;
    i++;
    while (isDigit(lexeme[i])) {
      const d = BigInt(lexeme.charCodeAt(i) - 48);
      if (overflowU64(sig, d)) {
        while (isDigit(lexeme[i])) i++; // parse_decimal_overflow: ignore further digits
        break;
      }
      sig = sig * 10n + d;
      exponent--;
      i++;
    }
  }
  if (lexeme[i] === "e" || lexeme[i] === "E") {
    isFloat = true;
    i++;
    let positiveExp = true;
    if (lexeme[i] === "+" || lexeme[i] === "-") positiveExp = lexeme[i++] === "+";
    let exp = 0;
    while (isDigit(lexeme[i])) {
      const d = lexeme.charCodeAt(i) - 48;
      if (exp >= Math.floor(I32_MAX / 10) && (exp > Math.floor(I32_MAX / 10) || d > I32_MAX % 10)) {
        // parse_exponent_overflow: error instead of infinity, else +/-0.0
        if (sig !== 0n && positiveExp) throw outOfRange();
        return formatF64(positive ? 0 : -0);
      }
      exp = exp * 10 + d;
      i++;
    }
    exponent = Math.min(I32_MAX, Math.max(I32_MIN, positiveExp ? exponent + exp : exponent - exp));
  }
  if (!isFloat) {
    if (positive) return sig.toString();
    // `-0` and magnitudes beyond i64 become f64.
    if (sig > 0n && sig <= I64_MIN_MAG) return "-" + sig.toString();
    return formatF64(-Number(sig));
  }
  const f = f64FromParts(sig, exponent);
  return formatF64(positive ? f : -f);
}

function str(s: string): string {
  let out = '"';
  for (const ch of s) {
    const c = ch.codePointAt(0)!;
    if (ch === '"') out += '\\"';
    else if (ch === "\\") out += "\\\\";
    else if (c < 0x20) {
      const short: Record<number, string> = { 8: "\\b", 9: "\\t", 10: "\\n", 12: "\\f", 13: "\\r" };
      out += short[c] ?? "\\u" + c.toString(16).padStart(4, "0");
    } else out += ch;
  }
  return out + '"';
}

function write(v: Value): string {
  switch (v.t) {
    case "null":
      return "null";
    case "bool":
      return v.v ? "true" : "false";
    case "num":
      return number(v.s);
    case "str":
      return str(v.v);
    case "arr":
      return "[" + v.v.map(write).join(",") + "]";
    case "obj":
      return "{" + v.v.map(([k, x]) => str(k) + ":" + write(x)).join(",") + "}";
  }
}

/** serde_json::to_string(&serde_json::from_str::<Value>(text)?) */
export function reserialize(text: string): string {
  return write(parse(text));
}

/** serde_json string serialization (for building `{"query":…,"variables":…}`). */
export const serdeString = str;

// TypeScript port of the app's cURL exporter (PRODUCT.md §5.5, AC-CEXP-2..5, AC-QRY-5).
// Reference: crates/core/src/curl/export.rs + prepare.rs + urlx.rs (QA-R1-006). This is a
// line-by-line port of `prepare_with_boundary` + `export_curl`; keep them in sync, and keep
// test/fixtures/curl_parity.json passing on both sides.

import { type VariableMap, resolveVariables, rustTrim } from "./variables";
import { JsonError, reserialize, serdeString } from "./serde-json";

export interface CurlKeyValue {
  key: string;
  value: string;
  enabled: boolean;
}

export interface CurlFormField extends CurlKeyValue {
  kind: "text" | "file";
  /** Optional per-field content type (`;type=` in cURL). */
  contentType?: string;
}

export type AuthType = "none" | "inherit" | "basic" | "bearer" | "apikey";

export interface CurlAuth {
  type: AuthType;
  username?: string;
  password?: string;
  token?: string;
  apiKey?: { key: string; value: string; in: "header" | "query" };
}

export type BodyType = "none" | "json" | "xml" | "text" | "form" | "formEncoded" | "graphql" | "binary";

export interface CurlBody {
  type: BodyType;
  text?: string;
  form?: CurlFormField[];
  formEncoded?: CurlKeyValue[];
  /** `variables` is the editor's JSON text (the core model); a non-string value is stringified. */
  graphql?: { query: string; variables?: unknown };
  binaryPath?: string;
}

export interface CurlRequest {
  method: string;
  url: string;
  /** Query rows; when non-empty the URL's query string is rebuilt from the enabled rows (AC-REQ-2). */
  params?: CurlKeyValue[];
  headers: CurlKeyValue[];
  auth: CurlAuth;
  body: CurlBody;
  settings?: { verifyTls?: boolean };
}

export interface CurlOptions {
  /** Variables to resolve (AC-CEXP-5). Ignored when `keepVariables` is true. */
  variables?: VariableMap;
  /** "Keep {{variables}}" toggle: export raw text. */
  keepVariables?: boolean;
  /** Auth of the parent collection, used when `auth.type === "inherit"`. */
  inheritedAuth?: CurlAuth;
}

/** Same messages as `prepare::PrepareError`. */
export class CurlExportError extends Error {}

/** Separator between command parts: ` \` + newline + 2-space indent (`LINE_JOIN`). */
export const LINE_JOIN = " \\\n  ";

// ---------------------------------------------------------------------------------------------
// shell.rs / model.rs / urlx.rs / prepare.rs helpers

/** POSIX single-quote escaping (`shell::quote`): `it's` -> `'it'\''s'`. */
export function shellQuote(value: string): string {
  return "'" + value.replace(/'/g, "'\\''") + "'";
}

/** `model::is_valid_method`. */
const isValidMethod = (m: string) => /^[A-Z][A-Z0-9_-]*$/.test(m);

const asciiLower = (s: string) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());
const eqIgnoreAsciiCase = (a: string, b: string) => asciiLower(a) === asciiLower(b);

const utf8 = new TextEncoder();
/** `percent_encoding::utf8_percent_encode` with CONTROLS + `extra` (non-ASCII always encoded). */
function percentEncode(s: string, keep: (b: number) => boolean): string {
  let out = "";
  for (const b of utf8.encode(s)) {
    out += keep(b) ? String.fromCharCode(b) : "%" + b.toString(16).toUpperCase().padStart(2, "0");
  }
  return out;
}
const isControlOrHigh = (b: number) => b < 0x20 || b === 0x7f || b >= 0x80;
const QUERY_VALUE = new Set([..." &#\"<>"].map((c) => c.charCodeAt(0)));
const QUERY_KEY = new Set([...QUERY_VALUE, "=".charCodeAt(0)]);
export const encodeQueryValue = (s: string) => percentEncode(s, (b) => !isControlOrHigh(b) && !QUERY_VALUE.has(b));
export const encodeQueryKey = (s: string) => percentEncode(s, (b) => !isControlOrHigh(b) && !QUERY_KEY.has(b));
/** `prepare::form_urlencode`: everything but ASCII alphanumerics and `-._~`. */
export const formUrlencode = (s: string) => percentEncode(s, (b) => /[A-Za-z0-9\-._~]/.test(String.fromCharCode(b)) && b < 0x80);

/** `urlx::split_url` -> [base, query|null, fragment|null]. */
function splitUrl(url: string): [string, string | null, string | null] {
  const h = url.indexOf("#");
  const rest = h === -1 ? url : url.slice(0, h);
  const fragment = h === -1 ? null : url.slice(h + 1);
  const q = rest.indexOf("?");
  return q === -1 ? [rest, null, fragment] : [rest.slice(0, q), rest.slice(q + 1), fragment];
}

/** `urlx::build_query`. */
function buildQuery(params: CurlKeyValue[]): string {
  let out = "";
  for (const p of params) {
    if (!p.enabled || (p.key === "" && p.value === "")) continue;
    if (out !== "") out += "&";
    out += encodeQueryKey(p.key);
    if (p.value !== "") out += "=" + encodeQueryValue(p.value);
  }
  return out;
}

function hasScheme(url: string): boolean {
  const i = url.indexOf("://");
  return i > 0 && /^[A-Za-z][A-Za-z0-9+\-.]*$/.test(url.slice(0, i));
}

function hostOf(rest: string): string {
  const m = /[/?#]/.exec(rest);
  let authority = rest.slice(0, m ? m.index : rest.length);
  const at = authority.lastIndexOf("@");
  if (at !== -1) authority = authority.slice(at + 1);
  if (authority.startsWith("[")) {
    const end = authority.indexOf("]");
    return end === -1 ? authority : authority.slice(0, end + 1);
  }
  const colon = authority.indexOf(":");
  return colon === -1 ? authority : authority.slice(0, colon);
}

/** AC-REQ-10 (`urlx::apply_default_scheme`). */
export function withDefaultScheme(url: string): string {
  const trimmed = rustTrim(url);
  if (hasScheme(trimmed) || trimmed.startsWith("{{")) return trimmed;
  const rest = trimmed.replace(/^(\/\/)+/, "");
  const host = asciiLower(hostOf(rest));
  const scheme = host === "localhost" || host === "127.0.0.1" || host === "[::1]" ? "http" : "https";
  return `${scheme}://${rest}`;
}

const DEFAULT_CONTENT_TYPE: Record<BodyType, string | null> = {
  none: null,
  json: "application/json",
  graphql: "application/json",
  xml: "application/xml",
  text: "text/plain",
  formEncoded: "application/x-www-form-urlencoded",
  form: "multipart/form-data",
  binary: "application/octet-stream",
};

/** `prepare::effective_auth`. */
function effectiveAuth(auth: CurlAuth, collection?: CurlAuth): CurlAuth | null {
  if (auth.type === "inherit") return collection && collection.type !== "inherit" && collection.type !== "none" ? collection : null;
  return auth.type === "none" ? null : auth;
}

function base64Utf8(s: string): string {
  let bin = "";
  for (const b of utf8.encode(s)) bin += String.fromCharCode(b);
  return btoa(bin);
}

const needsFormString = (v: string) => v.startsWith("@") || v.startsWith("<") || v.startsWith('"') || v.includes(";");
const formPath = (p: string) => (/[;,"\\]/.test(p) ? `"${p.replace(/\\/g, "\\\\").replace(/"/g, '\\"')}"` : p);

interface Part {
  name: string;
  kind: "text" | "file";
  value: string;
  contentType?: string;
}
type Prepared = { kind: "bytes"; text: string } | { kind: "file"; path: string } | { kind: "multipart"; parts: Part[] } | null;

// ---------------------------------------------------------------------------------------------

/** Export `req` as a multi-line bash cURL command. Throws `CurlExportError` like `PrepareError`. */
export function toCurl(req: CurlRequest, opts: CurlOptions = {}): string {
  const scope: VariableMap = opts.keepVariables ? new Map() : (opts.variables ?? new Map());
  const r = (s: string | undefined): string => resolveVariables(s ?? "", scope);

  // --- prepare_with_boundary ---
  const method = rustTrim(req.method);
  if (!isValidMethod(method)) {
    throw new CurlExportError(`Invalid method "${req.method}": use A–Z, 0–9, _ or -, starting with a letter`);
  }
  if (rustTrim(req.url) === "") throw new CurlExportError("URL is empty");

  let headers: [string, string][] = req.headers
    .filter((h) => h.enabled && rustTrim(h.key) !== "")
    .map((h) => [r(rustTrim(h.key)), r(h.value)]);
  const hasHeader = (name: string) => headers.some(([k]) => eqIgnoreAsciiCase(k, name));

  const auth = effectiveAuth(req.auth, opts.inheritedAuth);
  const extraQuery: [string, string][] = [];
  if (auth) {
    if (auth.type === "basic") {
      if (!hasHeader("Authorization")) {
        headers.push(["Authorization", `Basic ${base64Utf8(`${r(auth.username)}:${r(auth.password)}`)}`]);
      }
    } else if (auth.type === "bearer") {
      if (!hasHeader("Authorization")) headers.push(["Authorization", `Bearer ${r(auth.token)}`]);
    } else if (auth.type === "apikey" && auth.apiKey && rustTrim(auth.apiKey.key) !== "") {
      const key = r(rustTrim(auth.apiKey.key));
      const value = r(auth.apiKey.value);
      if (auth.apiKey.in === "query") extraQuery.push([key, value]);
      else if (!hasHeader(key)) headers.push([key, value]);
    }
  }

  // URL (AC-REQ-2, AC-REQ-10)
  const raw = rustTrim(req.url);
  const params = req.params ?? [];
  let url: string;
  if (params.length === 0) {
    url = r(raw);
  } else {
    const [base, , fragment] = splitUrl(raw);
    const rows = params.filter((p) => p.enabled).map((p) => ({ key: r(p.key), value: r(p.value), enabled: true }));
    url = r(base);
    const q = buildQuery(rows);
    if (q !== "") url += "?" + q;
    if (fragment !== null) url += "#" + fragment;
  }
  if (extraQuery.length > 0) {
    const [base, query, fragment] = splitUrl(url);
    let q = query ?? "";
    for (const [k, v] of extraQuery) {
      if (q !== "") q += "&";
      q += encodeQueryKey(k);
      if (v !== "") q += "=" + encodeQueryValue(v);
    }
    url = `${base}?${q}` + (fragment !== null ? "#" + fragment : "");
  }
  url = withDefaultScheme(url);

  // Body (AC-REQ-4, AC-REQ-5)
  const b = req.body;
  let contentType = DEFAULT_CONTENT_TYPE[b.type];
  let body: Prepared = null;
  switch (b.type) {
    case "json":
    case "xml":
    case "text":
      body = { kind: "bytes", text: r(b.text) };
      break;
    case "graphql": {
      const query = r(b.graphql?.query);
      const v = b.graphql?.variables;
      const varsText = r(typeof v === "string" ? v : v === undefined || v === null ? "" : JSON.stringify(v));
      let variables = "{}";
      if (rustTrim(varsText) !== "") {
        try {
          variables = reserialize(varsText);
        } catch (e) {
          const je = e instanceof JsonError ? e : new JsonError(String(e), 1, 1);
          throw new CurlExportError(`Invalid GraphQL variables: line ${je.line}, column ${je.column}: ${je.message}`);
        }
      }
      body = { kind: "bytes", text: `{"query":${serdeString(query)},"variables":${variables}}` };
      break;
    }
    case "formEncoded":
      body = { kind: "bytes", text: "" }; // exported per field below
      break;
    case "form":
      body = {
        kind: "multipart",
        parts: (b.form ?? [])
          .filter((f) => f.enabled && f.key !== "")
          .map((f) => ({ name: r(f.key), kind: f.kind, value: r(f.value), contentType: f.contentType })),
      };
      contentType = "multipart/form-data; boundary=boundary";
      break;
    case "binary":
      body = { kind: "file", path: r(b.binaryPath) };
      break;
    default:
      break;
  }
  if (contentType && body && !hasHeader("Content-Type")) headers.push(["Content-Type", contentType]);

  // --- export_curl ---
  const explicit = (name: string) => req.headers.some((h) => h.enabled && eqIgnoreAsciiCase(rustTrim(h.key), name));
  let basic: string | null = null;
  if (auth?.type === "basic" && !explicit("authorization")) {
    headers = headers.filter(([k]) => !eqIgnoreAsciiCase(k, "authorization"));
    basic = `${r(auth.username)}:${r(auth.password)}`;
  }
  if ((b.type === "form" || b.type === "formEncoded") && !explicit("content-type")) {
    // cURL sets these itself (and generates its own multipart boundary).
    headers = headers.filter(([k]) => !eqIgnoreAsciiCase(k, "content-type"));
  }

  const bodyParts: string[] = [];
  if (body?.kind === "file") {
    bodyParts.push(`--data-binary ${shellQuote("@" + body.path)}`);
  } else if (body?.kind === "multipart") {
    for (const p of body.parts) {
      const type = p.contentType ? `;type=${p.contentType}` : "";
      if (p.kind === "file") bodyParts.push(`-F ${shellQuote(`${p.name}=@${formPath(p.value)}${type}`)}`);
      else if (needsFormString(p.value)) bodyParts.push(`--form-string ${shellQuote(`${p.name}=${p.value}`)}`);
      else bodyParts.push(`-F ${shellQuote(`${p.name}=${p.value}${type}`)}`);
    }
  } else if (body?.kind === "bytes" && b.type === "formEncoded") {
    for (const f of b.formEncoded ?? []) {
      if (!f.enabled || (f.key === "" && f.value === "")) continue;
      const key = r(f.key);
      const value = r(f.value);
      // `--data-urlencode '=v'` would drop the `=`; send it verbatim.
      if (key === "") bodyParts.push(`--data-raw ${shellQuote(`=${formUrlencode(value)}`)}`);
      else bodyParts.push(`--data-urlencode ${shellQuote(`${formUrlencode(key)}=${value}`)}`);
    }
  } else if (body?.kind === "bytes") {
    bodyParts.push(`--data-raw ${shellQuote(body.text)}`);
  }
  const hasBody = bodyParts.length > 0;

  // AC-CEXP-2: `curl`, the method flag, the URL, then headers and body flags — every part on its own line.
  const parts = ["curl"];
  if ((method === "GET" && !hasBody) || (method === "POST" && hasBody)) {
    // implied by curl
  } else if (method === "HEAD" && !hasBody) {
    parts.push("--head");
  } else {
    parts.push(`-X ${method}`);
  }
  parts.push(shellQuote(url));
  if (/[{}[\]]/.test(url)) parts.push("--globoff");
  for (const [k, v] of headers) parts.push(`-H ${shellQuote(v === "" ? `${k};` : `${k}: ${v}`)}`);
  if (basic !== null) parts.push(`-u ${shellQuote(basic)}`);
  parts.push(...bodyParts);
  if (req.settings?.verifyTls === false) parts.push("-k");
  return parts.join(LINE_JOIN);
}

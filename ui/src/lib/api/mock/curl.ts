/** Browser-mock cURL importer/exporter (mirrors the AC-CURL / AC-CEXP rules of reqharbor-core). */
import type { Auth, Body, CurlImport, FormField, KeyValue, Request } from "../types";
import { METHOD_RE } from "../types";
import { scratchRequest } from "../../util/request";
import { urlToParams, encodeQueryValue } from "../../util/urlx";
import { fail } from "./errors";

const SILENT = new Set([
  "silent", "show-error", "verbose", "include", "output", "progress-bar", "compressed", "write-out",
]);
const SHORT_WITH_VALUE = new Set("XHduAbemFowxTEcDrCUtK".split(""));
const LONG_WITH_VALUE = new Set([
  "request", "header", "data", "data-ascii", "data-raw", "data-binary", "data-urlencode", "json", "user",
  "user-agent", "cookie", "referer", "max-time", "form", "form-string", "url", "output", "write-out", "proxy",
  "connect-timeout", "url-query", "cookie-jar", "dump-header", "retry", "cacert", "cert", "key", "upload-file",
  "range", "oauth2-bearer", "resolve", "interface",
]);
const SHORT_TO_LONG: Record<string, string> = {
  X: "request", H: "header", d: "data", u: "user", A: "user-agent", b: "cookie", e: "referer", m: "max-time",
  F: "form", o: "output", w: "write-out", x: "proxy", T: "upload-file", E: "cert", c: "cookie-jar",
  D: "dump-header", r: "range", C: "continue-at", U: "proxy-user", t: "telnet-option", K: "config",
  s: "silent", S: "show-error", v: "verbose", i: "include", L: "location", k: "insecure", I: "head", G: "get",
};

const ANSI: Record<string, string> = { n: "\n", t: "\t", r: "\r", "'": "'", "\\": "\\", '"': '"', "0": "\0" };

/** Shell-like tokenizer: quotes, $'…' ANSI-C quoting, backslash-newline continuations. */
export function tokenize(input: string): string[] {
  const text = input.replace(/^[﻿​‌‍⁠]+/, "");
  if (/\^\r?\n/.test(text) && !/\\\r?\n/.test(text)) {
    fail("curl_syntax", "Windows cmd (^) syntax is not supported; copy as cURL (bash)");
  }
  const out: string[] = [];
  let cur = "";
  let has = false;
  let i = 0;
  const n = text.length;
  while (i < n) {
    const c = text[i];
    if (c === "\\" && (text[i + 1] === "\n" || (text[i + 1] === "\r" && text[i + 2] === "\n"))) {
      i += text[i + 1] === "\r" ? 3 : 2;
      continue;
    }
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      if (has) out.push(cur);
      cur = "";
      has = false;
      i++;
      continue;
    }
    has = true;
    if (c === "'") {
      const j = text.indexOf("'", i + 1);
      if (j < 0) fail("curl_syntax", "Unterminated single quote");
      cur += text.slice(i + 1, j);
      i = j + 1;
    } else if (c === "$" && text[i + 1] === "'") {
      i += 2;
      let closed = false;
      while (i < n) {
        const d = text[i];
        if (d === "'") {
          closed = true;
          i++;
          break;
        }
        if (d === "\\" && i + 1 < n) {
          const e = text[i + 1];
          cur += ANSI[e] ?? "\\" + e;
          i += 2;
          continue;
        }
        cur += d;
        i++;
      }
      if (!closed) fail("curl_syntax", "Unterminated $'…' quote");
    } else if (c === '"') {
      i++;
      let closed = false;
      while (i < n) {
        const d = text[i];
        if (d === '"') {
          closed = true;
          i++;
          break;
        }
        if (d === "\\" && i + 1 < n) {
          const e = text[i + 1];
          if (e === '"' || e === "\\" || e === "$" || e === "`") {
            cur += e;
            i += 2;
            continue;
          }
          if (e === "\n") {
            i += 2;
            continue;
          }
          if (e === "\r" && text[i + 2] === "\n") {
            i += 3;
            continue;
          }
        }
        cur += d;
        i++;
      }
      if (!closed) fail("curl_syntax", "Unterminated double quote");
    } else if (c === "\\" && i + 1 < n) {
      cur += text[i + 1];
      i += 2;
    } else {
      cur += c;
      i++;
    }
  }
  if (has) out.push(cur);
  return out;
}

export function looksLikeCurl(text: string): boolean {
  return /^[﻿​‌‍⁠]*curl\s/.test(text.trimStart());
}

interface DataPart {
  kind: "data" | "raw" | "binary" | "urlencode";
  value: string;
}

export function importCurl(text: string): CurlImport {
  if (!looksLikeCurl(text.trimEnd() + " ")) fail("not_curl", "Not a cURL command");
  const toks = tokenize(text);
  if (toks[0] !== "curl") fail("not_curl", "Not a cURL command");
  const unknownFlags: string[] = [];
  const warnings: string[] = [];
  let url: string | null = null;
  let method: string | null = null;
  let head = false;
  let get = false;
  const headers: KeyValue[] = [];
  const cookies: string[] = [];
  const data: DataPart[] = [];
  const form: FormField[] = [];
  const urlQuery: string[] = [];
  let json: string | null = null;
  let auth: Auth = { type: "none" };
  const settings = { followRedirects: true, timeoutMs: 30000, verifyTls: true };

  const flags: [string, string | null][] = [];
  for (let i = 1; i < toks.length; i++) {
    const t = toks[i];
    if (t.startsWith("--") && t.length > 2) {
      const eq = t.indexOf("=");
      const name = eq > 0 ? t.slice(2, eq) : t.slice(2);
      if (LONG_WITH_VALUE.has(name)) flags.push([name, eq > 0 ? t.slice(eq + 1) : (toks[++i] ?? "")]);
      else flags.push([name, null]);
    } else if (t.startsWith("-") && t.length > 1) {
      for (let j = 1; j < t.length; j++) {
        const ch = t[j];
        const long = SHORT_TO_LONG[ch] ?? `-${ch}`;
        if (SHORT_WITH_VALUE.has(ch)) {
          const rest = t.slice(j + 1);
          flags.push([long, rest.length ? rest : (toks[++i] ?? "")]);
          break;
        }
        flags.push([long, null]);
      }
    } else if (url === null) url = t;
    else unknownFlags.push(t);
  }

  for (const [name, val] of flags) {
    const v = val ?? "";
    switch (name) {
      case "request": {
        const up = v.toUpperCase();
        if (METHOD_RE.test(v)) method = v;
        else if (METHOD_RE.test(up)) method = up;
        else warnings.push(`Ignored invalid method "${v}"`);
        break;
      }
      case "header": {
        const semi = /^([^:;]+);$/.exec(v);
        if (semi) headers.push({ key: semi[1].trim(), value: "", enabled: true });
        else {
          const c = v.indexOf(":");
          if (c > 0) {
            const key = v.slice(0, c).trim();
            const value = v.slice(c + 1).trim();
            if (key.toLowerCase() === "cookie") cookies.push(value);
            else headers.push({ key, value, enabled: true });
          }
        }
        break;
      }
      case "data":
      case "data-ascii":
        data.push({ kind: v.startsWith("@") ? "binary" : "data", value: v });
        break;
      case "data-binary":
        data.push({ kind: v.startsWith("@") ? "binary" : "raw", value: v });
        break;
      case "data-raw":
        data.push({ kind: "raw", value: v });
        break;
      case "data-urlencode": {
        const eq = v.indexOf("=");
        const enc = (s: string) => encodeURIComponent(s);
        const value = eq < 0 ? enc(v) : eq === 0 ? enc(v.slice(1)) : `${v.slice(0, eq)}=${enc(v.slice(eq + 1))}`;
        data.push({ kind: "urlencode", value });
        break;
      }
      case "json":
        json = (json ?? "") + v;
        break;
      case "form":
      case "form-string": {
        const eq = v.indexOf("=");
        const key = eq >= 0 ? v.slice(0, eq) : v;
        let value = eq >= 0 ? v.slice(eq + 1) : "";
        let contentType: string | undefined;
        const tm = /;type=([^;]+)/.exec(value);
        if (tm) {
          contentType = tm[1];
          value = value.replace(/;type=[^;]+/, "");
        }
        const isFile = name === "form" && value.startsWith("@");
        const f: FormField = { key, value: isFile ? value.slice(1) : value, kind: isFile ? "file" : "text", enabled: true };
        if (contentType) f.contentType = contentType;
        form.push(f);
        break;
      }
      case "user": {
        const c = v.indexOf(":");
        auth =
          c >= 0
            ? { type: "basic", username: v.slice(0, c), password: v.slice(c + 1) }
            : { type: "basic", username: v, password: "" };
        break;
      }
      case "user-agent":
        headers.push({ key: "User-Agent", value: v, enabled: true });
        break;
      case "cookie":
        cookies.push(v);
        break;
      case "referer":
        headers.push({ key: "Referer", value: v, enabled: true });
        break;
      case "max-time":
        settings.timeoutMs = Math.round(parseFloat(v) * 1000) || 30000;
        break;
      case "url":
        url = v;
        break;
      case "url-query":
        urlQuery.push(v);
        break;
      case "insecure":
        settings.verifyTls = false;
        break;
      case "location":
        settings.followRedirects = true;
        break;
      case "head":
        head = true;
        break;
      case "get":
        get = true;
        break;
      case "oauth2-bearer":
        headers.push({ key: "Authorization", value: `Bearer ${v}`, enabled: true });
        break;
      default:
        if (!SILENT.has(name)) unknownFlags.push(name.startsWith("-") ? name : `--${name}`);
    }
  }
  if (url === null) return fail("curl_missing_url", "The cURL command has no URL");
  if (cookies.length) headers.push({ key: "Cookie", value: cookies.join("; "), enabled: true });

  let body: Body = { type: "none" };
  const hasHeader = (k: string) => headers.some((h) => h.key.toLowerCase() === k);
  const ct = headers.find((h) => h.key.toLowerCase() === "content-type")?.value.toLowerCase() ?? "";
  let u: string = url;
  if (json !== null) {
    body = { type: "json", text: json };
    if (!hasHeader("content-type")) headers.push({ key: "Content-Type", value: "application/json", enabled: true });
    if (!hasHeader("accept")) headers.push({ key: "Accept", value: "application/json", enabled: true });
    method ??= "POST";
  } else if (form.length) {
    body = { type: "form", form };
    method ??= "POST";
  } else if (data.length) {
    if (get) {
      u += (u.includes("?") ? "&" : "?") + data.map((d) => d.value).join("&");
      method ??= "GET";
    } else if (data.length === 1 && data[0].kind === "binary") {
      body = { type: "binary", binaryPath: data[0].value.slice(1) };
      method ??= "POST";
    } else {
      method ??= "POST";
      body = detectBody(data.map((d) => d.value).join("&"), ct);
    }
  }
  if (urlQuery.length) u += (u.includes("?") ? "&" : "?") + urlQuery.join("&");
  if (head && !method) method = "HEAD";
  method ??= "GET";
  const request: Request = scratchRequest({
    name: requestNameFromUrl(u),
    method,
    url: u,
    params: urlToParams(u, []),
    headers,
    auth,
    body,
    settings,
  });
  return { request, unknownFlags, warnings };
}

function requestNameFromUrl(u: string): string {
  const path = u.replace(/^[a-z]+:\/\/[^/]+/i, "").split("?")[0];
  if (path && path !== "/") return path;
  return u.replace(/^[a-z]+:\/\//i, "").split(/[/?]/)[0] || "Imported request";
}

function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s.replace(/\+/g, " "));
  } catch {
    return s;
  }
}

function detectBody(text: string, ct: string): Body {
  if (ct.includes("json")) return { type: "json", text };
  if (ct.includes("xml")) return { type: "xml", text };
  const kvShape = /^[^=&\s]+=[^&]*(&[^=&\s]+=[^&]*)*$/.test(text);
  if (ct.includes("application/x-www-form-urlencoded") || (!ct && kvShape)) {
    return {
      type: "formEncoded",
      formEncoded: text.split("&").map((p) => {
        const i = p.indexOf("=");
        return {
          key: safeDecode(i >= 0 ? p.slice(0, i) : p),
          value: safeDecode(i >= 0 ? p.slice(i + 1) : ""),
          enabled: true,
        };
      }),
    };
  }
  if (!ct) {
    try {
      JSON.parse(text);
      return { type: "json", text };
    } catch {
      /* not json */
    }
  }
  return { type: "text", text };
}

const sq = (s: string) => `'${s.replace(/'/g, "'\\''")}'`;

/** AC-CEXP-2..4 (variables already resolved by the caller unless keepVariables). */
export function exportCurl(r: Request, keepVariables: boolean, inheritedAuth: Auth | null): string {
  const parts: string[] = ["curl"];
  const headers = r.headers.filter((h) => h.enabled && h.key);
  let url = r.url;
  const auth = r.auth.type === "inherit" ? (inheritedAuth ?? { type: "none" }) : r.auth;
  const hasAuthz = headers.some((h) => h.key.toLowerCase() === "authorization");
  const extraHeaders: string[] = [];
  let userFlag: string | null = null;
  if (!hasAuthz) {
    if (auth.type === "basic") userFlag = `-u ${sq(`${auth.username ?? ""}:${auth.password ?? ""}`)}`;
    if (auth.type === "bearer") extraHeaders.push(`-H ${sq(`Authorization: Bearer ${auth.token ?? ""}`)}`);
  }
  if (auth.type === "apikey" && auth.apiKey) {
    if (auth.apiKey.in === "header") extraHeaders.push(`-H ${sq(`${auth.apiKey.key}: ${auth.apiKey.value}`)}`);
    else url += (url.includes("?") ? "&" : "?") + `${auth.apiKey.key}=${encodeQueryValue(auth.apiKey.value)}`;
  }
  const b = r.body;
  const bodyFlags: string[] = [];
  const hasCT = headers.some((h) => h.key.toLowerCase() === "content-type");
  const impliedCT: Record<string, string> = {
    json: "application/json",
    graphql: "application/json",
    xml: "application/xml",
    text: "text/plain",
  };
  switch (b.type) {
    case "json":
    case "xml":
    case "text":
      bodyFlags.push(`--data-raw ${sq(b.text ?? "")}`);
      break;
    case "graphql": {
      let vars: unknown = {};
      try {
        vars = b.graphql?.variables ? JSON.parse(b.graphql.variables) : {};
      } catch {
        vars = {};
      }
      bodyFlags.push(`--data-raw ${sq(JSON.stringify({ query: b.graphql?.query ?? "", variables: vars }))}`);
      break;
    }
    case "formEncoded":
      for (const f of b.formEncoded ?? []) if (f.enabled) bodyFlags.push(`--data-urlencode ${sq(`${f.key}=${f.value}`)}`);
      break;
    case "form":
      for (const f of b.form ?? []) {
        if (!f.enabled) continue;
        const v = f.kind === "file" ? `@${f.value}` : f.value;
        bodyFlags.push(`-F ${sq(`${f.key}=${v}${f.contentType ? `;type=${f.contentType}` : ""}`)}`);
      }
      break;
    case "binary":
      bodyFlags.push(`--data-binary ${sq("@" + (b.binaryPath ?? ""))}`);
      break;
    default:
      break;
  }
  const hasBody = bodyFlags.length > 0;
  if (r.method === "HEAD" && !hasBody) parts.push("--head");
  else if (!((r.method === "GET" && !hasBody) || (r.method === "POST" && hasBody))) parts.push(`-X ${r.method}`);
  if (keepVariables) parts.push("--globoff");
  parts.push(sq(url));
  for (const h of headers) parts.push(`-H ${sq(`${h.key}: ${h.value}`)}`);
  parts.push(...extraHeaders);
  if (!hasCT && impliedCT[b.type]) parts.push(`-H ${sq(`Content-Type: ${impliedCT[b.type]}`)}`);
  if (userFlag) parts.push(userFlag);
  if (!r.settings.verifyTls) parts.push("-k");
  parts.push(...bodyFlags);
  return parts.join(" \\\n  ");
}

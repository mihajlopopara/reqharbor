/** Fake HTTP exchange for the browser mock: realistic canned responses, optional real fetch for localhost. */
import type { BodyKind, ErrorKind, ResponseCookie } from "../types";

export interface PreparedRequest {
  method: string;
  url: string;
  headers: [string, string][];
  bodyText: string | null;
  timeoutMs: number;
  verifyTls: boolean;
  followRedirects: boolean;
}

export interface FakeResponse {
  status: number;
  statusText: string;
  httpVersion: string;
  headers: [string, string][];
  body: Uint8Array;
  wireSize: number | null;
  contentEncoding: string | null;
  latencyMs: number;
  finalUrl: string;
  redirects: { status: number; method: string; url: string }[];
}

export class SendFailure {
  constructor(
    public kind: ErrorKind,
    public message: string,
  ) {}
}

export const STATUS_TEXT: Record<number, string> = {
  100: "Continue", 101: "Switching Protocols", 200: "OK", 201: "Created", 202: "Accepted", 204: "No Content",
  301: "Moved Permanently", 302: "Found", 303: "See Other", 304: "Not Modified", 307: "Temporary Redirect",
  308: "Permanent Redirect", 400: "Bad Request", 401: "Unauthorized", 403: "Forbidden", 404: "Not Found",
  405: "Method Not Allowed", 409: "Conflict", 410: "Gone", 415: "Unsupported Media Type", 418: "I'm a teapot",
  422: "Unprocessable Entity", 429: "Too Many Requests", 500: "Internal Server Error", 502: "Bad Gateway",
  503: "Service Unavailable", 504: "Gateway Timeout",
};

const enc = new TextEncoder();

// 1x1 transparent PNG
const PNG = Uint8Array.from(
  atob("iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAYAAAAf8/9hAAAAKElEQVR42mNkYPhfz0AEYBxVSF+FjAwM/4lRyDiqkL4KGUcVDl+FAADL7wkRqHbiEQAAAABJRU5ErkJggg=="),
  (c) => c.charCodeAt(0),
);

function hash(s: string): number {
  let h = 2166136261;
  for (let i = 0; i < s.length; i++) h = Math.imul(h ^ s.charCodeAt(i), 16777619);
  return h >>> 0;
}

const USERS = [
  { id: "01JD8ZQK7Y3M9V2T4R6N8B0C5E", name: "Ada Lovelace", email: "ada@example.com", role: "admin", teams: ["platform", "research"], active: true },
  { id: "01JD8ZR1B4K2P7X9Q3W5E8T6Y0", name: "Grace Hopper", email: "grace@example.com", role: "member", teams: ["compilers"], active: true },
  { id: "01JD8ZR9H6M3N5V8C2X4Z7B1D3", name: "Alan Turing", email: "alan@example.com", role: "member", teams: ["research"], active: false },
];

function json(status: number, value: unknown, extra: [string, string][] = []): Omit<FakeResponse, "latencyMs" | "finalUrl" | "redirects"> {
  const text = JSON.stringify(value);
  const body = enc.encode(text);
  return {
    status,
    statusText: STATUS_TEXT[status] ?? "",
    httpVersion: "HTTP/2.0",
    headers: [
      ["content-type", "application/json; charset=utf-8"],
      ["content-length", String(body.length)],
      ["date", new Date().toUTCString()],
      ["server", "mock-harbor/1.0"],
      ["x-request-id", hash(text).toString(16)],
      ...extra,
    ],
    body,
    wireSize: Math.round(body.length * 0.62),
    contentEncoding: body.length > 200 ? "gzip" : null,
  };
}

function text(status: number, contentType: string, value: string | Uint8Array, extra: [string, string][] = []): Omit<FakeResponse, "latencyMs" | "finalUrl" | "redirects"> {
  const body = typeof value === "string" ? enc.encode(value) : value;
  return {
    status,
    statusText: STATUS_TEXT[status] ?? "",
    httpVersion: "HTTP/1.1",
    headers: [["content-type", contentType], ["content-length", String(body.length)], ["date", new Date().toUTCString()], ...extra],
    body,
    wireSize: body.length,
    contentEncoding: null,
  };
}

function parseBody(bodyText: string | null): unknown {
  if (!bodyText) return null;
  try {
    return JSON.parse(bodyText);
  } catch {
    return bodyText;
  }
}

/** Pure canned response for a request (no delay). Throws SendFailure for simulated network errors. */
export function cannedResponse(req: PreparedRequest): FakeResponse {
  let u: URL;
  try {
    u = new URL(req.url);
  } catch {
    throw new SendFailure("other", `Invalid URL: ${req.url}`);
  }
  const host = u.hostname;
  const path = u.pathname;
  const latencyMs = 40 + (hash(req.method + req.url) % 180);
  const base = { latencyMs, finalUrl: req.url, redirects: [] as FakeResponse["redirects"] };
  if (host.endsWith(".invalid") || host.includes("nonexistent")) {
    throw new SendFailure("dns", `failed to lookup address information: ${host}: nodename nor servname provided, or not known`);
  }
  if (host.startsWith("self-signed") && req.verifyTls) {
    throw new SendFailure("tls", `invalid peer certificate: UnknownIssuer (${host})`);
  }
  if (host.startsWith("refused") || u.port === "1") {
    throw new SendFailure("connect", `tcp connect error: Connection refused (os error 61) — ${u.host}`);
  }
  const reqHeaders = Object.fromEntries(req.headers.map(([k, v]) => [k, v]));
  const status = /\/status\/(\d{3})/.exec(path);
  if (status) {
    const code = Number(status[1]);
    const r = code === 204 || code === 304 ? text(code, "text/plain", "") : json(code, { status: code, message: STATUS_TEXT[code] ?? "Status" });
    return { ...r, ...base };
  }
  if (/\/redirect/.test(path) && req.followRedirects) {
    const hops = [
      { status: 302, method: req.method, url: req.url },
      { status: 307, method: req.method, url: `${u.origin}/v1/landing` },
    ];
    return { ...json(200, { redirected: true, from: req.url }), ...base, redirects: hops, finalUrl: `${u.origin}/v1/landing` };
  }
  if (/\/redirect/.test(path)) return { ...json(302, { location: `${u.origin}/v1/landing` }, [["location", `${u.origin}/v1/landing`]]), ...base };
  if (/\.xml$|\/xml/.test(path)) {
    const xml = `<?xml version="1.0" encoding="UTF-8"?><users><user id="1"><name>Ada Lovelace</name><email>ada@example.com</email></user><user id="2"><name>Grace Hopper</name><email>grace@example.com</email></user></users>`;
    return { ...text(200, "application/xml; charset=utf-8", xml), ...base };
  }
  if (/\.html$|\/html/.test(path)) {
    const html = `<!doctype html><html><head><title>Mock page</title></head><body><h1>Hello from ReqHarbor mock</h1><p>This is an <b>HTML</b> response.</p><ul><li>One</li><li>Two</li></ul></body></html>`;
    return { ...text(200, "text/html; charset=utf-8", html), ...base };
  }
  if (/\.png$|\/image/.test(path)) return { ...text(200, "image/png", PNG), ...base };
  if (/\.pdf$|\/bytes|\/binary|\/download/.test(path)) {
    const bytes = new Uint8Array(4096).map((_, i) => (i * 31) % 256);
    bytes.set(enc.encode("%PDF-1.7"), 0);
    return { ...text(200, "application/pdf", bytes, [["content-disposition", 'attachment; filename="report.pdf"']]), ...base };
  }
  if (/\/huge/.test(path)) {
    const line = "lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt\n";
    const big = line.repeat(Math.ceil((6 * 1024 * 1024) / line.length));
    return { ...text(200, "text/plain; charset=utf-8", big), ...base };
  }
  if (/\/large/.test(path)) {
    const items = Array.from({ length: 6000 }, (_, i) => ({ id: i + 1, name: `Item ${i + 1}`, price: (i * 7.31) % 100, tags: ["a", "b"], active: i % 3 === 0 }));
    return { ...json(200, { items, total: items.length }), ...base };
  }
  if (/\/text/.test(path)) return { ...text(200, "text/plain; charset=utf-8", "pong\nAll systems nominal.\n"), ...base };
  if (/\/cookies/.test(path)) {
    return {
      ...json(200, { ok: true }, [
        ["set-cookie", "session=abc123; Path=/; HttpOnly; Secure; SameSite=Lax"],
        ["set-cookie", "theme=dark; Domain=example.com; Path=/; Expires=Wed, 21 Oct 2027 07:28:00 GMT"],
        ["set-cookie", "tracking=; Max-Age=0; Path=/"],
      ]),
      ...base,
    };
  }
  if (/\/health/.test(path)) return { ...json(200, { status: "ok", uptime: 86400, version: "1.4.2" }), ...base };
  const acceptQuery: [string, string][] = /\/search|\/users/.test(path) ? [["accept-query", "application/json"]] : [];
  if (/\/users\/?$/.test(path) || /\/users\/search/.test(path)) {
    if (req.method === "GET" || req.method === "QUERY") {
      const q = parseBody(req.bodyText);
      return {
        ...json(200, { data: USERS, page: { limit: Number(u.searchParams.get("limit") ?? 20), total: USERS.length }, query: q ?? undefined }, [
          ["cache-control", "no-store"],
          ["set-cookie", "session=s3ss10n; Path=/; HttpOnly; Secure; SameSite=Strict"],
          ...acceptQuery,
        ]),
        ...base,
      };
    }
    if (req.method === "POST") {
      const b = parseBody(req.bodyText);
      return {
        ...json(201, { id: "01JDA2" + hash(req.bodyText ?? "").toString(36).toUpperCase().slice(0, 20), ...(typeof b === "object" && b ? b : { raw: b }), createdAt: new Date().toISOString() }, [
          ["location", `${u.origin}${path}/01JDA2`],
          ...acceptQuery,
        ]),
        ...base,
      };
    }
  }
  if (/\/users\/[^/]+$/.test(path)) {
    if (req.method === "DELETE") return { ...text(204, "text/plain", ""), ...base };
    if (!reqHeaders["Authorization"] && !reqHeaders["authorization"] && /\/me$/.test(path)) {
      return { ...json(401, { error: "unauthorized" }, [["www-authenticate", 'Bearer realm="api"']]), ...base };
    }
    return { ...json(200, { ...USERS[0], id: path.split("/").pop() }), ...base };
  }
  return {
    ...json(
      200,
      {
        method: req.method,
        url: req.url,
        args: Object.fromEntries(u.searchParams),
        headers: reqHeaders,
        data: req.bodyText ?? "",
        json: parseBody(req.bodyText),
        origin: "203.0.113.7",
      },
      acceptQuery,
    ),
    ...base,
  };
}

/** Real fetch for localhost targets (CORS permitting); maps failures to typed errors. */
export async function realFetch(req: PreparedRequest, signal: AbortSignal): Promise<FakeResponse> {
  const started = performance.now();
  try {
    const res = await fetch(req.url, {
      method: req.method,
      headers: req.headers.filter(([k]) => !/^(user-agent|accept-encoding|cookie|host|content-length)$/i.test(k)),
      body: req.bodyText !== null && req.method !== "GET" && req.method !== "HEAD" ? req.bodyText : undefined,
      redirect: req.followRedirects ? "follow" : "manual",
      signal,
    });
    const body = new Uint8Array(await res.arrayBuffer());
    const headers: [string, string][] = [];
    res.headers.forEach((v, k) => headers.push([k, v]));
    return {
      status: res.status,
      statusText: res.statusText || STATUS_TEXT[res.status] || "",
      httpVersion: "HTTP/1.1",
      headers,
      body,
      wireSize: null,
      contentEncoding: res.headers.get("content-encoding"),
      latencyMs: performance.now() - started,
      finalUrl: res.url || req.url,
      redirects: [],
    };
  } catch (e) {
    if ((e as Error).name === "AbortError") throw e;
    throw new SendFailure("connect", `Connection refused: ${new URL(req.url).host} (${(e as Error).message})`);
  }
}

export function contentTypeOf(headers: [string, string][]): string | null {
  return headers.find(([k]) => k.toLowerCase() === "content-type")?.[1] ?? null;
}

export function bodyKindOf(contentType: string | null, body: Uint8Array): BodyKind {
  if (body.length === 0) return "empty";
  const ct = (contentType ?? "").toLowerCase();
  if (ct.includes("json")) return "json";
  if (ct.includes("html")) return "html";
  if (ct.includes("xml")) return ct.includes("svg") ? "image" : "xml";
  if (/image\/(png|jpeg|gif|webp|svg)/.test(ct)) return "image";
  if (ct.startsWith("text/") || ct.includes("javascript")) return "text";
  if (ct) return "binary";
  // sniff
  const head = new TextDecoder().decode(body.slice(0, 512)).trimStart();
  if (head.startsWith("{") || head.startsWith("[")) return "json";
  if (head.startsWith("<?xml")) return "xml";
  if (/^<!doctype html|^<html/i.test(head)) return "html";
  return /[\x00-\x08\x0e-\x1f]/.test(head) ? "binary" : "text";
}

export function parseCookies(headers: [string, string][]): ResponseCookie[] {
  return headers
    .filter(([k]) => k.toLowerCase() === "set-cookie")
    .map(([, v]) => {
      const [first, ...attrs] = v.split(";").map((s) => s.trim());
      const eq = first.indexOf("=");
      const c: ResponseCookie = {
        name: eq >= 0 ? first.slice(0, eq) : first,
        value: eq >= 0 ? first.slice(eq + 1) : "",
        domain: null,
        path: null,
        expires: null,
        maxAge: null,
        httpOnly: false,
        secure: false,
        sameSite: null,
      };
      for (const a of attrs) {
        const i = a.indexOf("=");
        const k = (i >= 0 ? a.slice(0, i) : a).toLowerCase();
        const val = i >= 0 ? a.slice(i + 1) : "";
        if (k === "domain") c.domain = val;
        else if (k === "path") c.path = val;
        else if (k === "expires") c.expires = val;
        else if (k === "max-age") c.maxAge = Number(val);
        else if (k === "httponly") c.httpOnly = true;
        else if (k === "secure") c.secure = true;
        else if (k === "samesite") c.sameSite = val;
      }
      return c;
    });
}

/** Tiny XML pretty-printer with well-formedness check (format_body kind "xml"). */
export function formatXml(src: string): string {
  const tokens = src.replace(/>\s+</g, "><").trim().match(/<[^>]+>|[^<]+/g) ?? [];
  const stack: string[] = [];
  let out = "";
  let depth = 0;
  let line = 1;
  for (let i = 0; i < tokens.length; i++) {
    const t = tokens[i];
    const pad = "  ".repeat(depth);
    if (t.startsWith("<?") || t.startsWith("<!")) out += pad + t + "\n";
    else if (t.startsWith("</")) {
      const name = t.slice(2, -1).trim();
      const open = stack.pop();
      if (open !== name) throw Object.assign(new Error(`Mismatched closing tag </${name}>`), { line });
      depth--;
      out += "  ".repeat(depth) + t + "\n";
    } else if (t.startsWith("<")) {
      if (!t.endsWith(">")) throw Object.assign(new Error("Unterminated tag"), { line });
      const selfClosing = t.endsWith("/>");
      const name = /^<([^\s/>]+)/.exec(t)?.[1] ?? "";
      const next = tokens[i + 1];
      const after = tokens[i + 2];
      if (!selfClosing && next && !next.startsWith("<") && after === `</${name}>`) {
        out += pad + t + next.trim() + after + "\n";
        i += 2;
        continue;
      }
      out += pad + t + "\n";
      if (!selfClosing) {
        stack.push(name);
        depth++;
      }
    } else if (t.trim()) out += pad + t.trim() + "\n";
    line++;
  }
  if (stack.length) throw Object.assign(new Error(`Unclosed tag <${stack[stack.length - 1]}>`), { line });
  return out.trimEnd();
}

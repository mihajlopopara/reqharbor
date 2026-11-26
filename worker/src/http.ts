// Shared HTTP helpers: consistent JSON errors, security headers, size-limited JSON bodies.

export type AppEnv = Env & { PUBLIC_BASE_URL?: string };

/** PRODUCT.md §8 limits. */
export const LIMITS = {
  requestBodyBytes: 5 * 1024 * 1024,
  entityDataBytes: 512 * 1024,
  snapshotBytes: 1024 * 1024,
  changesPerPush: 500,
  pullLimitDefault: 500,
  pullLimitMax: 500,
  workspaceCreatesPerHour: 10,
  /** QA-R1-024: stored entity data + active share snapshots per workspace. */
  workspaceQuotaBytes: 50 * 1024 * 1024,
} as const;

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    message: string,
    readonly headers: Record<string, string> = {},
  ) {
    super(message);
  }
}

export const badRequest = (message: string, code = "validation_failed") =>
  new ApiError(400, code, message);
export const notFound = (message = "Not found") => new ApiError(404, "not_found", message);
export const gone = (message = "This share link has been revoked") => new ApiError(410, "gone", message);
export const tooLarge = (message: string, code = "payload_too_large") => new ApiError(413, code, message);

/** Headers sent on every response. */
const BASE_SECURITY_HEADERS: Record<string, string> = {
  "X-Content-Type-Options": "nosniff",
  "Referrer-Policy": "no-referrer",
  "X-Frame-Options": "DENY",
  "Cross-Origin-Opener-Policy": "same-origin",
  "Cross-Origin-Resource-Policy": "same-origin",
  "Strict-Transport-Security": "max-age=31536000; includeSubDomains",
  "Permissions-Policy": "camera=(), microphone=(), geolocation=(), interest-cohort=()",
};

/** CSP for JSON responses: nothing may load or execute. */
const API_CSP = "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

export function withSecurityHeaders(res: Response): Response {
  const headers = new Headers(res.headers);
  for (const [k, v] of Object.entries(BASE_SECURITY_HEADERS)) {
    if (!headers.has(k)) headers.set(k, v);
  }
  if (!headers.has("Content-Security-Policy")) headers.set("Content-Security-Policy", API_CSP);
  if (!headers.has("Cache-Control")) headers.set("Cache-Control", "no-store");
  headers.delete("Set-Cookie");
  return new Response(res.body, { status: res.status, statusText: res.statusText, headers });
}

export function json(body: unknown, status = 200, headers: Record<string, string> = {}): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json; charset=utf-8", ...headers },
  });
}

export function errorResponse(err: ApiError): Response {
  return json({ error: { code: err.code, message: err.message } }, err.status, err.headers);
}

const encoder = new TextEncoder();
export const utf8Length = (s: string): number => encoder.encode(s).byteLength;

/**
 * Reads the request body as JSON, enforcing Content-Type, the 5 MB limit (413) both via
 * Content-Length and while streaming (chunked bodies), and JSON syntax (400).
 */
export async function readJson(req: Request, maxBytes: number = LIMITS.requestBodyBytes): Promise<unknown> {
  const ct = (req.headers.get("Content-Type") ?? "").toLowerCase();
  if (!/^application\/json\s*(;|$)/.test(ct)) {
    throw new ApiError(415, "unsupported_media_type", "Content-Type must be application/json");
  }
  const declared = req.headers.get("Content-Length");
  if (declared !== null && Number(declared) > maxBytes) {
    throw tooLarge(`Request body exceeds ${maxBytes} bytes`);
  }
  if (!req.body) throw badRequest("Request body is required", "invalid_json");

  const reader = req.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.byteLength;
    if (total > maxBytes) {
      await reader.cancel().catch(() => {});
      throw tooLarge(`Request body exceeds ${maxBytes} bytes`);
    }
    chunks.push(value);
  }
  const buf = new Uint8Array(total);
  let off = 0;
  for (const c of chunks) {
    buf.set(c, off);
    off += c.byteLength;
  }
  let text: string;
  try {
    text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: false }).decode(buf);
  } catch {
    throw badRequest("Request body is not valid UTF-8", "invalid_json");
  }
  if (text.trim() === "") throw badRequest("Request body is required", "invalid_json");
  try {
    return JSON.parse(text);
  } catch {
    throw badRequest("Request body is not valid JSON", "invalid_json");
  }
}

export function isPlainObject(v: unknown): v is Record<string, unknown> {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

export function requireObject(v: unknown, what = "Request body"): Record<string, unknown> {
  if (!isPlainObject(v)) throw badRequest(`${what} must be a JSON object`);
  return v;
}

/** Origin used in share URLs. */
export function publicOrigin(req: Request, env: AppEnv): string {
  const configured = env.PUBLIC_BASE_URL?.trim();
  if (configured) return configured.replace(/\/+$/, "");
  return new URL(req.url).origin;
}

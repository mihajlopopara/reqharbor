// Workspaces, token auth, token rotation, creation rate limit.

import { TOKEN_RE, newToken, sha256Hex, timingSafeEqual, ulid } from "./crypto";
import { ApiError, LIMITS, type AppEnv, badRequest, json, readJson, requireObject } from "./http";
import { workspaceUsage } from "./quota";

export interface Workspace {
  id: string;
  name: string;
  rev: number;
  created_at: number;
  /** Hash of the token that authenticated this request (used for compare-and-swap rotation). */
  token_hash: string;
}

const unauthorized = (message = "Missing or invalid workspace token") =>
  new ApiError(401, "unauthorized", message, { "WWW-Authenticate": 'Bearer realm="reqharbor"' });

/** Resolves `Authorization: Bearer rh_…` to its workspace or throws 401. */
export async function authenticate(req: Request, env: AppEnv): Promise<Workspace> {
  const header = req.headers.get("Authorization") ?? "";
  const m = /^Bearer\s+(\S+)\s*$/i.exec(header);
  const token = m?.[1];
  if (!token || !TOKEN_RE.test(token)) throw unauthorized();
  const hash = await sha256Hex(token);
  const row = await env.DB.prepare(
    "SELECT id, name, rev, created_at, token_hash FROM workspaces WHERE token_hash = ?",
  )
    .bind(hash)
    .first<Workspace>();
  if (!row || !timingSafeEqual(row.token_hash, hash)) throw unauthorized();
  return { id: row.id, name: row.name, rev: row.rev, created_at: row.created_at, token_hash: row.token_hash };
}

/** Parses an IPv6 address (incl. `::` and embedded IPv4) into 8 hextets, or null. */
function parseIPv6(ip: string): number[] | null {
  let s = ip.trim().toLowerCase().replace(/^\[|\]$/g, "").replace(/%.*$/, "");
  const v4 = /(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(s);
  if (v4) {
    const o = v4.slice(1).map(Number);
    if (o.some((x) => x > 255)) return null;
    s = s.slice(0, v4.index) + `${((o[0]! << 8) | o[1]!).toString(16)}:${((o[2]! << 8) | o[3]!).toString(16)}`;
  }
  const halves = s.split("::");
  if (halves.length > 2) return null;
  const parse = (part: string) => (part === "" ? [] : part.split(":"));
  const head = parse(halves[0]!);
  const tail = halves.length === 2 ? parse(halves[1]!) : [];
  const fill = 8 - head.length - tail.length;
  if (halves.length === 2 ? fill < 1 : fill !== 0) return null;
  const groups = [...head, ...Array<string>(halves.length === 2 ? fill : 0).fill("0"), ...tail];
  if (!groups.every((g) => /^[0-9a-f]{1,4}$/.test(g))) return null;
  return groups.map((g) => parseInt(g, 16));
}

/**
 * Rate-limit bucket for a client address (QA-R1-025): IPv4 per address; IPv6 per /64, since a
 * single subscriber usually controls a whole /64. IPv4-mapped IPv6 addresses count as IPv4.
 */
export function rateLimitBucket(ip: string): string {
  if (!ip.includes(":")) return ip.trim();
  const h = parseIPv6(ip);
  if (!h) return ip.trim();
  const mapped = h.slice(0, 5).every((x) => x === 0) && h[5] === 0xffff;
  if (mapped) return `${h[6]! >> 8}.${h[6]! & 255}.${h[7]! >> 8}.${h[7]! & 255}`;
  return h.slice(0, 4).map((x) => x.toString(16)).join(":") + "::/64";
}

/** Fixed one-hour window per client bucket (the bucket is hashed before storage). */
async function enforceCreateRateLimit(req: Request, env: AppEnv): Promise<void> {
  const ip = req.headers.get("CF-Connecting-IP") ?? "unknown";
  const key = "ws_create:" + (await sha256Hex(rateLimitBucket(ip)));
  const windowMs = 60 * 60 * 1000;
  const now = Date.now();
  const windowStart = now - (now % windowMs);
  const row = await env.DB.prepare(
    `INSERT INTO rate_limits (key, window_start, count) VALUES (?1, ?2, 1)
     ON CONFLICT(key) DO UPDATE SET
       count = CASE WHEN rate_limits.window_start = excluded.window_start THEN rate_limits.count + 1 ELSE 1 END,
       window_start = excluded.window_start
     RETURNING count`,
  )
    .bind(key, windowStart)
    .first<{ count: number }>();
  if ((row?.count ?? 0) > LIMITS.workspaceCreatesPerHour) {
    const retryAfter = Math.ceil((windowStart + windowMs - now) / 1000);
    throw new ApiError(429, "rate_limited", "Too many workspaces created from this IP; try again later", {
      "Retry-After": String(retryAfter),
    });
  }
}

export async function createWorkspace(req: Request, env: AppEnv): Promise<Response> {
  const body = requireObject(await readJson(req));
  const name = body.name;
  if (typeof name !== "string" || name.trim().length === 0 || name.trim().length > 200) {
    throw badRequest("`name` must be a non-empty string of at most 200 characters");
  }
  await enforceCreateRateLimit(req, env);
  const id = ulid();
  const token = newToken();
  const createdAt = Date.now();
  await env.DB.prepare("INSERT INTO workspaces (id, name, token_hash, rev, created_at) VALUES (?, ?, ?, 0, ?)")
    .bind(id, name.trim(), await sha256Hex(token), createdAt)
    .run();
  // The token is returned exactly once; only its hash is stored.
  return json({ workspaceId: id, token }, 201);
}

export async function getWorkspace(ws: Workspace, env: AppEnv): Promise<Response> {
  const [count, bytesUsed] = await Promise.all([
    env.DB.prepare("SELECT COUNT(*) AS n FROM entities WHERE workspace_id = ? AND deleted = 0").bind(ws.id).first<{ n: number }>(),
    workspaceUsage(env, ws.id),
  ]);
  return json({
    workspaceId: ws.id,
    name: ws.name,
    createdAt: ws.created_at,
    entityCount: count?.n ?? 0,
    bytesUsed,
    quotaBytes: LIMITS.workspaceQuotaBytes,
  });
}

/**
 * Atomic compare-and-swap rotation (contract C4): the hash only changes if it still equals the
 * token that authenticated this call, so the old token stops working in the same statement and
 * two concurrent rotations cannot both succeed (the loser gets 401).
 */
export async function rotateToken(ws: Workspace, env: AppEnv): Promise<Response> {
  const token = newToken();
  const res = await env.DB.prepare("UPDATE workspaces SET token_hash = ?1 WHERE id = ?2 AND token_hash = ?3")
    .bind(await sha256Hex(token), ws.id, ws.token_hash)
    .run();
  if (res.meta.changes !== 1) throw unauthorized("Token was already rotated");
  return json({ token });
}

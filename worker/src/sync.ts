// Push/pull sync (PRODUCT.md §8, AC-SYNC-3/4/5; QA-R1-005/026/024).
//
// Every accepted change gets the next per-workspace `rev`. LWW is decided inside SQLite (one
// transactional D1 batch), so concurrent pushes cannot interleave a read-modify-write:
//   incoming wins  <=>  updatedAt > stored.updatedAt
//                      OR (updatedAt = stored.updatedAt AND deviceId >= stored.deviceId)
// (contract C3: on a tie the lexicographically greater deviceId wins; missing deviceId = "").
// A stale push still bumps the *server copy* to a fresh rev, guaranteeing the client receives the
// winning version on its next pull even if its cursor was already past the old rev.
// Entities whose data exceeds 512 KB are reported per change as `rejected` (contract C2) while
// the rest of the push applies.

import { LIMITS, type AppEnv, badRequest, isPlainObject, json, readJson, requireObject, tooLarge, utf8Length } from "./http";
import { enforceQuota, workspaceUsage } from "./quota";
import { stripSecretValuesForSync } from "./sanitize";
import type { Workspace } from "./workspaces";

/** AC-SYNC-3: history and settings are never synced. */
export const SYNC_TYPES = ["project", "collection", "folder", "request", "environment"] as const;
type SyncType = (typeof SYNC_TYPES)[number];

const ID_RE = /^[A-Za-z0-9_.:-]{1,128}$/;
const DEVICE_ID_RE = /^[A-Za-z0-9_.:-]{0,128}$/;
/** D1 allows at most 100 bound parameters per statement: 1 shared + 7 per row. */
const ROWS_PER_STATEMENT = 14;
/** Ids per lookup statement (1 shared parameter + ids). */
const IDS_PER_LOOKUP = 90;

interface ValidChange {
  type: SyncType;
  id: string;
  updatedAt: number;
  deleted: boolean;
  deviceId: string;
  data: string | null;
  size: number;
  rejected?: "entity_too_large";
}

interface EntityRow {
  type: string;
  id: string;
  rev: number;
  updated_at: number;
  deleted: number;
  device_id: string;
  data: string | null;
}

function parseIntParam(raw: string | null, name: string, def: number, min: number): number {
  if (raw === null || raw === "") return def;
  if (!/^\d+$/.test(raw)) throw badRequest(`\`${name}\` must be a non-negative integer`);
  const n = Number(raw);
  if (!Number.isSafeInteger(n) || n < min) throw badRequest(`\`${name}\` must be an integer >= ${min}`);
  return n;
}

export async function pull(req: Request, ws: Workspace, env: AppEnv): Promise<Response> {
  const url = new URL(req.url);
  const since = parseIntParam(url.searchParams.get("since"), "since", 0, 0);
  const limit = Math.min(parseIntParam(url.searchParams.get("limit"), "limit", LIMITS.pullLimitDefault, 1), LIMITS.pullLimitMax);

  const { results } = await env.DB.prepare(
    `SELECT type, id, rev, updated_at, deleted, device_id, data FROM entities
     WHERE workspace_id = ? AND rev > ? ORDER BY rev ASC LIMIT ?`,
  )
    .bind(ws.id, since, limit + 1)
    .all<EntityRow>();

  const hasMore = results.length > limit;
  const page = hasMore ? results.slice(0, limit) : results;
  const changes = page.map((r) => ({
    type: r.type,
    id: r.id,
    updatedAt: r.updated_at,
    deleted: r.deleted === 1,
    deviceId: r.device_id,
    data: r.deleted === 1 || r.data === null ? null : (JSON.parse(r.data) as unknown),
    rev: r.rev,
  }));
  // Empty page: the client is caught up; hand back the workspace's current head.
  const cursor = page.length > 0 ? page[page.length - 1]!.rev : ws.rev;
  return json({ changes, cursor, hasMore });
}

function validateChange(raw: unknown, i: number): ValidChange {
  const at = `changes[${i}]`;
  if (!isPlainObject(raw)) throw badRequest(`${at} must be an object`);
  const { type, id, updatedAt, deleted, data } = raw;
  if (typeof type !== "string" || !(SYNC_TYPES as readonly string[]).includes(type)) {
    throw badRequest(`${at}.type must be one of ${SYNC_TYPES.join(", ")} (history and settings are never synced)`, "unsupported_type");
  }
  if (typeof id !== "string" || !ID_RE.test(id)) throw badRequest(`${at}.id must be a 1-128 character id`);
  if (typeof updatedAt !== "number" || !Number.isSafeInteger(updatedAt) || updatedAt < 0) {
    throw badRequest(`${at}.updatedAt must be a non-negative integer (epoch ms)`);
  }
  if (typeof deleted !== "boolean") throw badRequest(`${at}.deleted must be a boolean`);
  const rawDevice = raw.deviceId;
  if (rawDevice !== undefined && rawDevice !== null && (typeof rawDevice !== "string" || !DEVICE_ID_RE.test(rawDevice))) {
    throw badRequest(`${at}.deviceId must be a string of at most 128 id characters`);
  }
  const deviceId = typeof rawDevice === "string" ? rawDevice : "";
  const base = { type: type as SyncType, id, updatedAt, deleted, deviceId };
  if (deleted) {
    if (data !== null && data !== undefined) throw badRequest(`${at}.data must be null for a tombstone`);
    return { ...base, data: null, size: 0 };
  }
  if (!isPlainObject(data)) throw badRequest(`${at}.data must be an object unless deleted is true`);
  const clean = type === "environment" ? stripSecretValuesForSync(data) : data;
  const serialized = JSON.stringify(clean);
  const size = utf8Length(serialized);
  if (size > LIMITS.entityDataBytes) return { ...base, data: null, size: 0, rejected: "entity_too_large" };
  return { ...base, data: serialized, size };
}

const keyOf = (c: { type: string; id: string }) => `${c.type}\u0000${c.id}`;

async function existingSizes(env: AppEnv, ws: Workspace, changes: ValidChange[]): Promise<Map<string, number>> {
  const sizes = new Map<string, number>();
  const ids = [...new Set(changes.map((c) => c.id))];
  const stmts: D1PreparedStatement[] = [];
  for (let i = 0; i < ids.length; i += IDS_PER_LOOKUP) {
    const chunk = ids.slice(i, i + IDS_PER_LOOKUP);
    stmts.push(
      env.DB.prepare(
        `SELECT type, id, size FROM entities WHERE workspace_id = ?1 AND id IN (${chunk.map((_, j) => `?${j + 2}`).join(", ")})`,
      ).bind(ws.id, ...chunk),
    );
  }
  if (stmts.length === 0) return sizes;
  for (const r of await env.DB.batch(stmts)) {
    for (const row of r.results as { type: string; id: string; size: number }[]) sizes.set(keyOf(row), row.size);
  }
  return sizes;
}

export async function push(req: Request, ws: Workspace, env: AppEnv): Promise<Response> {
  const body = requireObject(await readJson(req));
  if (!Array.isArray(body.changes)) throw badRequest("`changes` must be an array");
  if (body.changes.length > LIMITS.changesPerPush) {
    throw tooLarge(`At most ${LIMITS.changesPerPush} changes per push`, "too_many_changes");
  }
  const all = body.changes.map(validateChange);
  const seen = new Set<string>();
  for (const c of all) {
    if (seen.has(keyOf(c))) throw badRequest(`Duplicate change for ${c.type} ${c.id} in one push`, "duplicate_change");
    seen.add(keyOf(c));
  }
  const changes = all.filter((c) => !c.rejected);

  if (changes.length > 0) {
    // Quota: net growth if every change applied (stale changes are rare; overshoot is bounded).
    const [used, previous] = await Promise.all([workspaceUsage(env, ws.id), existingSizes(env, ws, changes)]);
    const delta = changes.reduce((sum, c) => sum + c.size - (previous.get(keyOf(c)) ?? 0), 0);
    enforceQuota(used, delta);
  }

  const written = new Map<string, { rev: number; updated_at: number; device_id: string }>();
  let head = ws.rev;
  if (changes.length > 0) {
    const n = changes.length;
    const stmts: D1PreparedStatement[] = [
      // Reserve revs (head-n, head]; change i receives head - (n - 1 - i), i.e. revs ascend in push order.
      env.DB.prepare("UPDATE workspaces SET rev = rev + ?2 WHERE id = ?1").bind(ws.id, n),
    ];
    const WIN = `(excluded.updated_at > entities.updated_at OR (excluded.updated_at = entities.updated_at AND excluded.device_id >= entities.device_id))`;
    for (let start = 0; start < n; start += ROWS_PER_STATEMENT) {
      const chunk = changes.slice(start, start + ROWS_PER_STATEMENT);
      const params: unknown[] = [ws.id];
      const rows = chunk.map((c, j) => {
        const k = n - 1 - (start + j);
        const p = params.length + 1;
        params.push(c.type, c.id, c.updatedAt, c.deleted ? 1 : 0, c.deviceId, c.data, c.size);
        return `(?1, ?${p}, ?${p + 1}, (SELECT rev FROM workspaces WHERE id = ?1) - ${k}, ?${p + 2}, ?${p + 3}, ?${p + 4}, ?${p + 5}, ?${p + 6})`;
      });
      stmts.push(
        env.DB.prepare(
          `INSERT INTO entities (workspace_id, type, id, rev, updated_at, deleted, device_id, data, size) VALUES ${rows.join(", ")}
           ON CONFLICT (workspace_id, type, id) DO UPDATE SET
             rev        = excluded.rev,
             updated_at = CASE WHEN ${WIN} THEN excluded.updated_at ELSE entities.updated_at END,
             deleted    = CASE WHEN ${WIN} THEN excluded.deleted    ELSE entities.deleted    END,
             device_id  = CASE WHEN ${WIN} THEN excluded.device_id  ELSE entities.device_id  END,
             data       = CASE WHEN ${WIN} THEN excluded.data       ELSE entities.data       END,
             size       = CASE WHEN ${WIN} THEN excluded.size       ELSE entities.size       END
           RETURNING type, id, rev, updated_at, device_id`,
        ).bind(...params),
      );
    }
    stmts.push(env.DB.prepare("SELECT rev FROM workspaces WHERE id = ?").bind(ws.id));

    const out = await env.DB.batch(stmts);
    for (const r of out.slice(1, -1)) {
      for (const row of r.results as { type: string; id: string; rev: number; updated_at: number; device_id: string }[]) {
        written.set(keyOf(row), row);
      }
    }
    head = (out[out.length - 1]!.results[0] as { rev: number } | undefined)?.rev ?? ws.rev;
  }

  const results = all.map((c) => {
    if (c.rejected) return { type: c.type, id: c.id, status: "rejected" as const, reason: c.rejected };
    const row = written.get(keyOf(c))!;
    // The stored row equals the incoming change iff the incoming change won LWW.
    const applied = row.updated_at === c.updatedAt && row.device_id === c.deviceId;
    return { type: c.type, id: c.id, status: applied ? ("applied" as const) : ("stale" as const), rev: row.rev };
  });
  return json({ results, cursor: head });
}

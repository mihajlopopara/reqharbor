// Share links (PRODUCT.md §8 share semantics, AC-SHARE-1..6).

import { SHARE_ID_RE, shareId as newShareId } from "./crypto";
import { type AppEnv, LIMITS, ApiError, badRequest, gone, json, notFound, publicOrigin, readJson, requireObject, utf8Length } from "./http";
import { enforceQuota, workspaceUsage } from "./quota";
import { renderErrorPage, renderSharePage, sharePageCsp } from "./share-page";
import { SHARE_KINDS, type ShareKind, normalizeSnapshot, prepareSnapshot } from "./snapshot";
import type { Workspace } from "./workspaces";

const ENTITY_ID_RE = /^[A-Za-z0-9_.:-]{1,128}$/;

interface ShareRow {
  id: string;
  workspace_id: string;
  kind: ShareKind;
  entity_id: string;
  snapshot: string | null;
  size: number;
  created_at: number;
  updated_at: number;
  revoked_at: number | null;
}

const shareUrl = (req: Request, env: AppEnv, id: string) => `${publicOrigin(req, env)}/s/${id}`;

/** Share bodies may carry a 1 MB snapshot plus JSON overhead; the global 5 MB cap still applies. */
const readShareBody = (req: Request) => readJson(req, LIMITS.requestBodyBytes);

export async function createShare(req: Request, ws: Workspace, env: AppEnv): Promise<Response> {
  const body = requireObject(await readShareBody(req));
  const kind = body.kind;
  if (typeof kind !== "string" || !SHARE_KINDS.includes(kind as ShareKind)) {
    throw badRequest('`kind` must be "collection" or "request"');
  }
  if (typeof body.entityId !== "string" || !ENTITY_ID_RE.test(body.entityId)) {
    throw badRequest("`entityId` must be a 1-128 character id");
  }
  const snapshot = prepareSnapshot(kind as ShareKind, body.snapshot);
  const size = utf8Length(snapshot);
  enforceQuota(await workspaceUsage(env, ws.id), size);
  const id = newShareId();
  const now = Date.now();
  await env.DB.prepare(
    `INSERT INTO shares (id, workspace_id, kind, entity_id, snapshot, size, created_at, updated_at, revoked_at)
     VALUES (?, ?, ?, ?, ?, ?, ?, ?, NULL)`,
  )
    .bind(id, ws.id, kind, body.entityId, snapshot, size, now, now)
    .run();
  return json({ shareId: id, url: shareUrl(req, env, id), createdAt: now, updatedAt: now }, 201);
}

export async function listShares(req: Request, ws: Workspace, env: AppEnv): Promise<Response> {
  const { results } = await env.DB.prepare(
    `SELECT id, kind, entity_id, created_at, updated_at,
            COALESCE(json_extract(snapshot, '$.collection.name'), json_extract(snapshot, '$.request.name')) AS name
     FROM shares WHERE workspace_id = ? AND revoked_at IS NULL ORDER BY created_at DESC, id`,
  )
    .bind(ws.id)
    .all<{ id: string; kind: string; entity_id: string; created_at: number; updated_at: number; name: unknown }>();
  return json({
    shares: results.map((r) => ({
      shareId: r.id,
      url: shareUrl(req, env, r.id),
      kind: r.kind,
      entityId: r.entity_id,
      name: typeof r.name === "string" ? r.name : null,
      createdAt: r.created_at,
      updatedAt: r.updated_at,
    })),
  });
}

async function ownedShare(id: string, ws: Workspace, env: AppEnv): Promise<ShareRow> {
  // Unknown ids and other workspaces' shares are indistinguishable (404).
  if (!SHARE_ID_RE.test(id)) throw notFound("Share not found");
  const row = await env.DB.prepare("SELECT * FROM shares WHERE id = ? AND workspace_id = ?").bind(id, ws.id).first<ShareRow>();
  if (!row) throw notFound("Share not found");
  return row;
}

export async function updateShare(req: Request, id: string, ws: Workspace, env: AppEnv): Promise<Response> {
  const row = await ownedShare(id, ws, env);
  if (row.revoked_at !== null) throw gone();
  const body = requireObject(await readShareBody(req));
  const snapshot = prepareSnapshot(row.kind, body.snapshot);
  const size = utf8Length(snapshot);
  enforceQuota(await workspaceUsage(env, ws.id), size - row.size);
  const now = Math.max(Date.now(), row.updated_at);
  const res = await env.DB.prepare(
    "UPDATE shares SET snapshot = ?, size = ?, updated_at = ? WHERE id = ? AND workspace_id = ? AND revoked_at IS NULL",
  )
    .bind(snapshot, size, now, id, ws.id)
    .run();
  if (res.meta.changes === 0) throw gone(); // revoked concurrently
  return json({ shareId: id, url: shareUrl(req, env, id), createdAt: row.created_at, updatedAt: now });
}

export async function revokeShare(id: string, ws: Workspace, env: AppEnv): Promise<Response> {
  const row = await ownedShare(id, ws, env);
  const revokedAt = row.revoked_at ?? Date.now();
  if (row.revoked_at === null) {
    // The snapshot is discarded on revoke; nothing remains to leak.
    await env.DB.prepare("UPDATE shares SET revoked_at = ?, snapshot = NULL, size = 0 WHERE id = ? AND revoked_at IS NULL")
      .bind(revokedAt, id)
      .run();
  }
  return json({ shareId: id, revokedAt });
}

// ----------------------------------------------------------------------------------------------
// Public, unauthenticated read-only views.

const PUBLIC_HEADERS = {
  "X-Robots-Tag": "noindex, nofollow",
  "Cache-Control": "public, max-age=60",
};

async function loadPublic(id: string, env: AppEnv): Promise<ShareRow> {
  if (!SHARE_ID_RE.test(id)) throw notFound("Share not found");
  const row = await env.DB.prepare("SELECT * FROM shares WHERE id = ?").bind(id).first<ShareRow>();
  if (!row) throw notFound("Share not found");
  if (row.revoked_at !== null || row.snapshot === null) throw gone();
  return row;
}

export async function publicShareJson(id: string, env: AppEnv): Promise<Response> {
  try {
    const row = await loadPublic(id, env);
    return json(
      {
        shareId: row.id,
        kind: row.kind,
        createdAt: row.created_at,
        updatedAt: row.updated_at,
        snapshot: JSON.parse(row.snapshot!) as unknown,
      },
      200,
      {
        ...PUBLIC_HEADERS,
        // Read-only and unguessable; the app's "Import from link" may fetch it from any origin.
        "Access-Control-Allow-Origin": "*",
        "Cross-Origin-Resource-Policy": "cross-origin",
      },
    );
  } catch (e) {
    if (e instanceof ApiError) {
      return json({ error: { code: e.code, message: e.message } }, e.status, {
        "X-Robots-Tag": "noindex, nofollow",
        "Access-Control-Allow-Origin": "*",
        "Cross-Origin-Resource-Policy": "cross-origin",
      });
    }
    throw e;
  }
}

export async function publicSharePage(id: string, env: AppEnv): Promise<Response> {
  const csp = await sharePageCsp();
  const html = (body: string, status: number, extra: Record<string, string> = {}) =>
    new Response(body, {
      status,
      headers: {
        "Content-Type": "text/html; charset=utf-8",
        "Content-Security-Policy": csp,
        "X-Robots-Tag": "noindex, nofollow",
        ...extra,
      },
    });
  try {
    const row = await loadPublic(id, env);
    const view = normalizeSnapshot(row.kind, JSON.parse(row.snapshot!) as unknown);
    return html(renderSharePage(view, { shareId: row.id, createdAt: row.created_at, updatedAt: row.updated_at }), 200, PUBLIC_HEADERS);
  } catch (e) {
    if (e instanceof ApiError && (e.status === 404 || e.status === 410)) {
      return html(renderErrorPage(e.status), e.status, { "Cache-Control": "no-store" });
    }
    throw e;
  }
}

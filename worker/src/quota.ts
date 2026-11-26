// Per-workspace storage quota (QA-R1-024): entity data + active share snapshots, in UTF-8 bytes.

import { ApiError, type AppEnv, LIMITS } from "./http";

export async function workspaceUsage(env: AppEnv, workspaceId: string): Promise<number> {
  const row = await env.DB.prepare(
    `SELECT (SELECT COALESCE(SUM(size), 0) FROM entities WHERE workspace_id = ?1)
          + (SELECT COALESCE(SUM(size), 0) FROM shares WHERE workspace_id = ?1 AND revoked_at IS NULL) AS used`,
  )
    .bind(workspaceId)
    .first<{ used: number }>();
  return row?.used ?? 0;
}

/**
 * Throws 413 `quota_exceeded` when a write would grow usage past the quota. Writes that do not
 * grow usage (deletes, same-size or smaller updates) are always allowed, so a workspace that is
 * over quota can still clean up.
 */
export function enforceQuota(used: number, delta: number): void {
  if (delta <= 0 || used + delta <= LIMITS.workspaceQuotaBytes) return;
  throw new ApiError(
    413,
    "quota_exceeded",
    `Workspace storage quota of ${LIMITS.workspaceQuotaBytes} bytes exceeded (${used} used, this write adds ${delta})`,
  );
}

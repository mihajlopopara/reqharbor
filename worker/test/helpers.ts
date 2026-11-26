import { SELF } from "cloudflare:test";

export const BASE = "https://worker.test";

/** Random client IP so the workspace-creation rate limit never couples unrelated tests. */
export const randomIp = () =>
  `10.${Math.floor(Math.random() * 256)}.${Math.floor(Math.random() * 256)}.${Math.floor(Math.random() * 256)}`;

export interface CallOptions {
  method?: string;
  token?: string;
  body?: unknown;
  rawBody?: BodyInit;
  headers?: Record<string, string>;
}

export async function call(path: string, opts: CallOptions = {}): Promise<Response> {
  const headers: Record<string, string> = { ...(opts.headers ?? {}) };
  if (opts.token) headers.Authorization = `Bearer ${opts.token}`;
  let body: BodyInit | undefined = opts.rawBody;
  if (opts.body !== undefined) {
    body = JSON.stringify(opts.body);
    headers["Content-Type"] ??= "application/json";
  }
  return SELF.fetch(BASE + path, { method: opts.method ?? (body ? "POST" : "GET"), headers, body });
}

export async function callJson<T = any>(path: string, opts: CallOptions = {}): Promise<{ res: Response; data: T }> {
  const res = await call(path, opts);
  return { res, data: (await res.json()) as T };
}

export async function newWorkspace(name = "Test workspace"): Promise<{ workspaceId: string; token: string }> {
  const { res, data } = await callJson("/v1/workspaces", {
    body: { name },
    headers: { "CF-Connecting-IP": randomIp() },
  });
  if (res.status !== 201) throw new Error(`create workspace failed: ${res.status} ${JSON.stringify(data)}`);
  return data;
}

export const change = (
  type: string,
  id: string,
  updatedAt: number,
  data: Record<string, unknown> | null = { id, name: `${type} ${id}` },
  deleted = false,
) => ({ type, id, updatedAt, deleted, data: deleted ? null : data });

export function sampleRequest(overrides: Record<string, unknown> = {}) {
  return {
    id: "01REQ0000000000000000000001",
    collectionId: "01COL0000000000000000000001",
    folderId: null,
    name: "List users",
    method: "GET",
    url: "{{baseUrl}}/users?page=1",
    params: [{ key: "page", value: "1", enabled: true }],
    headers: [
      { key: "Accept", value: "application/json", enabled: true },
      { key: "X-Debug", value: "1", enabled: false },
    ],
    auth: { type: "none" },
    body: { type: "none" },
    settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
    sortOrder: 0,
    ...overrides,
  };
}

export function sampleCollectionSnapshot() {
  return {
    schemaVersion: 1,
    collection: {
      id: "01COL0000000000000000000001",
      name: "Users API",
      description: "Endpoints for users",
      auth: { type: "bearer", token: "{{token}}" },
      folders: [
        { id: "F1", collectionId: "01COL0000000000000000000001", parentFolderId: null, name: "Admin", sortOrder: 1 },
        { id: "F2", collectionId: "01COL0000000000000000000001", parentFolderId: "F1", name: "Danger zone", sortOrder: 0 },
      ],
      requests: [
        sampleRequest(),
        sampleRequest({
          id: "R2",
          folderId: "F1",
          name: "Search users",
          method: "QUERY",
          url: "{{baseUrl}}/users/search",
          auth: { type: "inherit" },
          body: { type: "json", text: '{"q":"ada"}' },
          sortOrder: 0,
        }),
        sampleRequest({
          id: "R3",
          folderId: "F2",
          name: "Delete user",
          method: "DELETE",
          url: "https://api.example.com/users/1",
          sortOrder: 0,
        }),
      ],
    },
    environment: {
      name: "Staging",
      variables: [
        { key: "baseUrl", value: "https://staging.example.com", enabled: true, secret: false },
        { key: "token", value: "super-secret-token", enabled: true, secret: true },
      ],
    },
  };
}

/** Seed data for the browser mock: a realistic workspace with history. */
import type { Collection, Environment, Folder, HistoryEntry, Project, Request, Settings } from "../types";
import { ulid } from "../../util/ulid";
import { scratchRequest } from "../../util/request";
import { urlToParams } from "../../util/urlx";

export interface SeedData {
  workspace: { id: string; name: string };
  projects: Project[];
  collections: Collection[];
  folders: Folder[];
  requests: Request[];
  environments: Environment[];
  settings: Settings;
  /** Requests to replay through the canned exchange to build history (newest last). */
  historySends: { request: Request; ageMs: number; projectId: string }[];
  history: HistoryEntry[];
}

export function defaultSettings(): Settings {
  return {
    theme: "system",
    windowWidth: null,
    windowHeight: null,
    sidebarWidth: 300,
    sidebarCollapsed: false,
    splitRatio: 0.5,
    layoutMode: "auto",
    lastOpenedRequestId: null,
    activeProjectId: null,
  };
}

export function seed(now: number, empty = false): SeedData {
  const ws = { id: ulid(now - 9e8), name: "My Workspace" };
  let t = now - 8e8;
  const stamp = () => (t += 1000);
  const project = (name: string, sortOrder: number): Project => {
    const c = stamp();
    return { id: ulid(c), workspaceId: ws.id, name, sortOrder, activeEnvironmentId: null, createdAt: c, updatedAt: c };
  };
  const def = project("Default", 0);
  const globalEnv: Environment = {
    id: ulid(stamp()),
    workspaceId: ws.id,
    projectId: null,
    name: "Global",
    variables: empty ? [] : [{ key: "apiVersion", value: "v1", enabled: true, secret: false }],
    sortOrder: 0,
    createdAt: t,
    updatedAt: t,
  };
  if (empty) {
    return {
      workspace: ws,
      projects: [def],
      collections: [],
      folders: [],
      requests: [],
      environments: [globalEnv],
      settings: defaultSettings(),
      historySends: [],
      history: [],
    };
  }
  const side = project("Side project", 1);
  const env = (projectId: string, name: string, vars: Environment["variables"], sortOrder: number): Environment => {
    const c = stamp();
    return { id: ulid(c), workspaceId: ws.id, projectId, name, variables: vars, sortOrder, createdAt: c, updatedAt: c };
  };
  const staging = env(def.id, "Staging", [
    { key: "baseUrl", value: "https://api.staging.example.com", enabled: true, secret: false },
    { key: "token", value: "stg_9f8e7d6c5b4a", enabled: true, secret: true },
    { key: "userId", value: "01JD8ZQK7Y3M9V2T4R6N8B0C5E", enabled: true, secret: false },
    { key: "debug", value: "true", enabled: false, secret: false },
  ], 0);
  const local = env(def.id, "Local", [
    { key: "baseUrl", value: "http://localhost:8080", enabled: true, secret: false },
    { key: "token", value: "dev-token", enabled: true, secret: true },
  ], 1);
  const sideEnv = env(side.id, "Production", [{ key: "host", value: "https://httpbin.example.org", enabled: true, secret: false }], 0);
  def.activeEnvironmentId = staging.id;

  const coll = (projectId: string, name: string, sortOrder: number, auth: Collection["auth"], description = ""): Collection => {
    const c = stamp();
    return { id: ulid(c), projectId, name, description, auth, sortOrder, createdAt: c, updatedAt: c };
  };
  const users = coll(def.id, "Users API", 0, { type: "bearer", token: "{{token}}" }, "User management endpoints.");
  const billing = coll(def.id, "Billing", 1, { type: "basic", username: "billing", password: "{{token}}" });
  const sideColl = coll(side.id, "Playground", 0, { type: "none" });
  const folder = (collectionId: string, name: string, sortOrder: number, parentFolderId: string | null = null): Folder => {
    const c = stamp();
    return { id: ulid(c), collectionId, parentFolderId, name, sortOrder, createdAt: c, updatedAt: c };
  };
  const usersF = folder(users.id, "Users", 0);
  const adminF = folder(users.id, "Admin", 1);
  const auditF = folder(users.id, "Audit", 0, adminF.id);
  const invoicesF = folder(billing.id, "Invoices", 0);

  const req = (collectionId: string, folderId: string | null, sortOrder: number, p: Partial<Request>): Request => {
    const c = stamp();
    const r = scratchRequest({ ...p, collectionId, folderId, sortOrder, createdAt: c, updatedAt: c });
    r.id = ulid(c);
    r.params = urlToParams(r.url, r.params);
    return r;
  };
  const requests: Request[] = [
    req(users.id, usersF.id, 0, { name: "List users", method: "GET", url: "{{baseUrl}}/v1/users?limit=20&page=1", auth: { type: "inherit" } }),
    req(users.id, usersF.id, 1, {
      name: "Create user",
      method: "POST",
      url: "{{baseUrl}}/v1/users",
      auth: { type: "inherit" },
      headers: [
        { key: "Accept", value: "application/json", enabled: true },
        { key: "X-Request-Source", value: "reqharbor", enabled: true },
        { key: "X-Debug", value: "{{debug}}", enabled: false },
      ],
      body: { type: "json", text: '{\n  "name": "Ada Lovelace",\n  "email": "ada@example.com",\n  "role": "admin",\n  "teams": ["platform", "research"],\n  "notify": true\n}' },
    }),
    req(users.id, usersF.id, 2, {
      name: "Search users",
      method: "QUERY",
      url: "{{baseUrl}}/v1/users/search",
      auth: { type: "inherit" },
      body: { type: "json", text: '{\n  "where": { "role": "admin" },\n  "limit": 10\n}' },
    }),
    req(users.id, usersF.id, 3, { name: "Delete user", method: "DELETE", url: "{{baseUrl}}/v1/users/{{userId}}", auth: { type: "inherit" } }),
    req(users.id, auditF.id, 0, { name: "Audit log", method: "GET", url: "{{baseUrl}}/v1/admin/audit?since={{since}}", auth: { type: "inherit" } }),
    req(users.id, null, 2, { name: "Health", method: "GET", url: "{{baseUrl}}/health" }),
    req(billing.id, invoicesF.id, 0, { name: "List invoices", method: "GET", url: "{{baseUrl}}/v1/invoices", auth: { type: "inherit" } }),
    req(billing.id, invoicesF.id, 1, {
      name: "Update invoice",
      method: "PATCH",
      url: "{{baseUrl}}/v1/invoices/inv_123",
      auth: { type: "inherit" },
      body: { type: "json", text: '{ "status": "paid" }' },
    }),
    req(billing.id, null, 1, {
      name: "Upload receipt",
      method: "POST",
      url: "{{baseUrl}}/v1/receipts",
      body: { type: "form", form: [{ key: "note", value: "Q4", kind: "text", enabled: true }, { key: "file", value: "/Users/me/receipt.pdf", kind: "file", enabled: true }] },
    }),
    req(sideColl.id, null, 0, { name: "Echo", method: "POST", url: "{{host}}/anything", body: { type: "text", text: "hello" } }),
    req(sideColl.id, null, 1, { name: "Purge cache", method: "PURGE", url: "{{host}}/cache/all" }),
  ];

  const scratch = (p: Partial<Request>) => scratchRequest(p);
  const byName = (n: string) => requests.find((r) => r.name === n)!;
  const historySends = [
    { request: scratch({ method: "GET", url: "https://bad-host.invalid/ping" }), ageMs: 30 * 3600e3, projectId: def.id },
    { request: scratch({ method: "HEAD", url: "https://cdn.example.com/app.js" }), ageMs: 28 * 3600e3, projectId: def.id },
    { request: scratch({ method: "OPTIONS", url: "{{baseUrl}}/v1/users" }), ageMs: 27 * 3600e3, projectId: def.id },
    { request: scratch({ method: "GET", url: "https://api.staging.example.com/status/503" }), ageMs: 26 * 3600e3, projectId: def.id },
    { request: { ...byName("Update invoice") }, ageMs: 25 * 3600e3, projectId: def.id },
    { request: scratch({ method: "DELETE", url: "https://api.staging.example.com/v1/sessions/status/401" }), ageMs: 5 * 3600e3, projectId: def.id },
    { request: { ...byName("Delete user") }, ageMs: 3 * 3600e3, projectId: def.id },
    { request: { ...byName("Search users") }, ageMs: 3600e3, projectId: def.id },
    { request: { ...byName("List users") }, ageMs: 14 * 60e3, projectId: def.id },
    { request: { ...byName("Create user") }, ageMs: 2 * 60e3, projectId: def.id },
  ];

  return {
    workspace: ws,
    projects: [def, side],
    collections: [users, billing, sideColl],
    folders: [usersF, adminF, auditF, invoicesF],
    requests,
    environments: [globalEnv, staging, local, sideEnv],
    settings: { ...defaultSettings(), activeProjectId: def.id },
    historySends,
    history: [],
  };
}

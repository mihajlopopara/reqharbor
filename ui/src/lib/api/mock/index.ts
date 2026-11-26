/**
 * In-memory implementation of the full Commands map for running the UI in a
 * plain browser (vite dev, Playwright). Mirrors API_CONTRACT.md: same result
 * shapes, same CommandError codes. State lives in memory and resets on reload.
 *
 * Query-string switches (browser only): `?seed=empty` (first launch),
 * `?platform=macos|windows|linux`, `?corrupt=1` (one quarantined file).
 */
import type { Backend, Unlisten } from "../index";
import type {
  Auth,
  Bootstrap,
  ChildCounts,
  Collection,
  CommandName,
  Commands,
  Environment,
  Events,
  Folder,
  HistoryEntry,
  HistorySummary,
  KeyValue,
  OpenedHistory,
  Project,
  ProjectTree,
  Request,
  ResponseMeta,
  SendError,
  SendResult,
  Settings,
  ShareInfo,
  ShareKind,
  ShareSnapshot,
  SkippedEntity,
  SyncReport,
  SyncStatus,
  Variable,
  VariableInfo,
} from "../types";
import { METHOD_RE } from "../types";
import { ulid } from "../../util/ulid";
import { applyDefaultScheme, paramsToUrl, urlToParams, encodeQueryValue } from "../../util/urlx";
import { prettyJson, jsonErrorPosition } from "../../util/json";
import { MockError, fail } from "./errors";
import { seed, defaultSettings } from "./seed";
import { importCurl, looksLikeCurl, exportCurl } from "./curl";
import { convertSpec, reportOf } from "./openapi";
import {
  bodyKindOf,
  cannedResponse,
  contentTypeOf,
  formatXml,
  parseCookies,
  realFetch,
  SendFailure,
  type FakeResponse,
  type PreparedRequest,
} from "./http";

type Handlers = { [K in CommandName]: (args: Commands[K]["args"]) => Promise<Commands[K]["result"]> | Commands[K]["result"] };

const clone = <T>(v: T): T => structuredClone(v);
const HISTORY_MAX = 500;
const HISTORY_BODY_MAX = 1024 * 1024;
const SYNC_ENTITY_MAX = 512 * 1024;
const enc = new TextEncoder();

interface ShareRecord {
  info: ShareInfo;
  snapshot: ShareSnapshot;
  revoked: boolean;
}

export interface MockOptions {
  empty?: boolean;
  platform?: string;
  corrupt?: boolean;
  /** Scale simulated latency (0 = instant, for unit tests). */
  latency?: number;
  now?: () => number;
}

export class MockServer {
  workspace: { id: string; name: string };
  projects: Project[];
  collections: Collection[];
  folders: Folder[];
  requests: Request[];
  environments: Environment[];
  settings: Settings;
  history: HistoryEntry[] = [];
  historyBodies = new Map<string, Uint8Array>();
  responses = new Map<string, Uint8Array>();
  responseOrder: string[] = [];
  inflight = new Map<string, AbortController>();
  listeners = new Map<string, Set<(p: unknown) => void>>();
  shares = new Map<string, ShareRecord>();
  sync: SyncStatus = {
    state: "disconnected",
    connected: false,
    workerUrl: null,
    remoteWorkspaceId: null,
    lastSyncAt: null,
    lastPullAt: null,
    lastError: null,
    nextRetryAt: null,
    retryAttempt: 0,
    pendingChanges: 0,
    skippedEntities: [],
    warning: null,
  };
  syncToken: string | null = null;
  private clock = 0;
  private opts: MockOptions;

  constructor(opts: MockOptions = {}) {
    this.opts = opts;
    const now = this.now();
    const s = seed(now, opts.empty);
    this.workspace = s.workspace;
    this.projects = s.projects;
    this.collections = s.collections;
    this.folders = s.folders;
    this.requests = s.requests;
    this.environments = s.environments;
    this.settings = s.settings;
    for (const h of s.historySends) {
      const ts = now - h.ageMs;
      this.recordCanned(h.request, h.projectId, ts);
    }
  }

  private now(): number {
    const n = this.opts.now ? this.opts.now() : Date.now();
    this.clock = Math.max(this.clock + 1, n);
    return this.clock;
  }

  private delay(ms: number, signal?: AbortSignal): Promise<void> {
    const scaled = ms * (this.opts.latency ?? 1);
    if (scaled <= 0) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const t = setTimeout(resolve, scaled);
      signal?.addEventListener("abort", () => {
        clearTimeout(t);
        reject(new DOMException("Aborted", "AbortError"));
      });
    });
  }

  emit<E extends keyof Events>(event: E, payload: Events[E]): void {
    for (const l of this.listeners.get(event) ?? []) l(clone(payload));
  }

  // ───────────── lookups ─────────────
  private project(id: string): Project {
    return this.projects.find((p) => p.id === id) ?? fail("not_found", `Project ${id} not found`);
  }
  private collection(id: string): Collection {
    return this.collections.find((c) => c.id === id) ?? fail("not_found", `Collection ${id} not found`);
  }
  private folder(id: string): Folder {
    return this.folders.find((f) => f.id === id) ?? fail("not_found", `Folder ${id} not found`);
  }
  private request(id: string): Request {
    return this.requests.find((r) => r.id === id) ?? fail("not_found", `Request ${id} not found`);
  }
  private env(id: string): Environment {
    return this.environments.find((e) => e.id === id) ?? fail("not_found", `Environment ${id} not found`);
  }
  private tree(projectId: string): ProjectTree {
    const project = this.project(projectId);
    const collections = this.collections.filter((c) => c.projectId === projectId);
    const ids = new Set(collections.map((c) => c.id));
    return clone({
      project,
      collections,
      folders: this.folders.filter((f) => ids.has(f.collectionId)),
      requests: this.requests.filter((r) => ids.has(r.collectionId)),
    });
  }
  private sortedProjects(): Project[] {
    return clone([...this.projects].sort((a, b) => a.sortOrder - b.sortOrder));
  }
  private sortedEnvs(): Environment[] {
    const g = this.environments.filter((e) => e.projectId === null);
    const rest = this.environments
      .filter((e) => e.projectId !== null)
      .sort((a, b) => a.sortOrder - b.sortOrder || a.createdAt - b.createdAt);
    return clone([...g, ...rest]);
  }
  private name(n: string): string {
    const t = n.trim();
    if (!t) fail("invalid", "Name must not be empty");
    return t;
  }
  private touch<T extends { updatedAt: number }>(e: T): T {
    e.updatedAt = this.now();
    this.markChanged();
    return e;
  }
  private markChanged(): void {
    if (this.sync.connected) {
      this.sync.pendingChanges++;
      this.refreshSkipped();
      this.emit("sync://status", this.sync);
    }
  }
  private refreshSkipped(): void {
    const skipped: SkippedEntity[] = this.requests
      .filter((r) => enc.encode(JSON.stringify(r)).length > SYNC_ENTITY_MAX)
      .map((r) => ({ type: "request", id: r.id, name: r.name, reason: "too_large" }));
    this.sync.skippedEntities = skipped;
    this.sync.warning = skipped.length
      ? `${skipped.length} item${skipped.length === 1 ? " is" : "s are"} too large to sync (over 512 KB): ${skipped.map((s) => s.name).join(", ")}`
      : null;
  }
  private projectOfCollection(collectionId: string): string {
    return this.collection(collectionId).projectId;
  }
  private siblingsOf(collectionId: string, folderId: string | null) {
    return {
      folders: this.folders.filter((f) => f.collectionId === collectionId && f.parentFolderId === folderId).sort((a, b) => a.sortOrder - b.sortOrder),
      requests: this.requests.filter((r) => r.collectionId === collectionId && r.folderId === folderId).sort((a, b) => a.sortOrder - b.sortOrder),
    };
  }
  private nextSort(collectionId: string, folderId: string | null): number {
    const s = this.siblingsOf(collectionId, folderId);
    return Math.max(-1, ...s.folders.map((f) => f.sortOrder), ...s.requests.map((r) => r.sortOrder)) + 1;
  }
  private descendantFolders(folderId: string): Folder[] {
    const out: Folder[] = [];
    const walk = (id: string) => {
      for (const f of this.folders.filter((x) => x.parentFolderId === id)) {
        out.push(f);
        walk(f.id);
      }
    };
    walk(folderId);
    return out;
  }

  // ───────────── variables ─────────────
  private lookup(projectId: string | null | undefined): Map<string, { v: Variable; env: Environment }> {
    const m = new Map<string, { v: Variable; env: Environment }>();
    const p = projectId ? this.projects.find((x) => x.id === projectId) : undefined;
    const active = p?.activeEnvironmentId ? this.environments.find((e) => e.id === p.activeEnvironmentId) : undefined;
    const global = this.environments.find((e) => e.projectId === null);
    for (const env of [active, global]) {
      if (!env) continue;
      for (const v of env.variables) if (v.enabled && v.key && !m.has(v.key)) m.set(v.key, { v, env });
    }
    return m;
  }
  private resolver(projectId: string | null | undefined, maskSecrets: boolean) {
    const map = this.lookup(projectId);
    const unresolved = new Set<string>();
    const resolve = (text: string, depth = 0, stack: string[] = []): string =>
      text.replace(/\{\{\s*([A-Za-z0-9_.-]+)\s*\}\}/g, (whole, name: string) => {
        const hit = map.get(name);
        if (!hit) {
          unresolved.add(name);
          return whole;
        }
        if (maskSecrets && hit.v.secret) return `{{${name}}}`;
        if (stack.includes(name) || depth >= 3) return whole;
        return resolve(hit.v.value, depth + 1, [...stack, name]);
      });
    return { resolve, unresolved };
  }

  private inheritedAuth(r: Request): Auth | null {
    if (!r.collectionId) return null;
    return this.collections.find((c) => c.id === r.collectionId)?.auth ?? null;
  }

  private resolveRequest(r: Request, projectId: string | null | undefined, maskSecrets: boolean) {
    const { resolve, unresolved } = this.resolver(projectId, maskSecrets);
    const kv = (rows: KeyValue[]) => rows.filter((x) => x.enabled).map((x) => ({ ...x, key: resolve(x.key), value: resolve(x.value) }));
    let url = resolve(r.url.trim());
    const params = kv(r.params);
    if (r.params.length) url = paramsToUrl(url, params);
    url = applyDefaultScheme(url);
    const auth: Auth = clone(r.auth.type === "inherit" ? (this.inheritedAuth(r) ?? { type: "none" }) : r.auth);
    if (auth.username !== undefined) auth.username = resolve(auth.username);
    if (auth.password !== undefined) auth.password = resolve(auth.password);
    if (auth.token !== undefined) auth.token = resolve(auth.token);
    if (auth.apiKey) auth.apiKey = { ...auth.apiKey, key: resolve(auth.apiKey.key), value: resolve(auth.apiKey.value) };
    const headers = kv(r.headers);
    const body = clone(r.body);
    if (body.text !== undefined) body.text = resolve(body.text);
    if (body.formEncoded) body.formEncoded = kv(body.formEncoded);
    if (body.form) body.form = body.form.filter((f) => f.enabled).map((f) => ({ ...f, key: resolve(f.key), value: resolve(f.value) }));
    if (body.graphql) body.graphql = { query: resolve(body.graphql.query), variables: resolve(body.graphql.variables) };
    if (body.binaryPath) body.binaryPath = resolve(body.binaryPath);
    return { url, headers, auth, body, unresolved };
  }

  private prepare(r: Request, projectId: string | null | undefined): { prepared: PreparedRequest; displayUrl: string; unresolved: string[] } {
    if (!METHOD_RE.test(r.method)) fail("validation", `Invalid method "${r.method}"`, { kind: "invalidMethod", message: "Use A–Z, 0–9, _ or -, starting with a letter" });
    if (!r.url.trim()) fail("validation", "Enter a URL", { kind: "emptyUrl" });
    if (r.body.type === "graphql" && r.body.graphql?.variables.trim()) {
      try {
        JSON.parse(r.body.graphql.variables);
      } catch (e) {
        fail("validation", "Fix GraphQL variables", { kind: "invalidGraphqlVariables", message: (e as Error).message });
      }
    }
    const res = this.resolveRequest(r, projectId, false);
    const display = this.resolveRequest(r, projectId, true);
    const headers: [string, string][] = res.headers.map((h) => [h.key, h.value]);
    const has = (k: string) => headers.some(([n]) => n.toLowerCase() === k);
    let url = res.url;
    if (!has("authorization")) {
      if (res.auth.type === "basic") headers.push(["Authorization", `Basic ${btoa(`${res.auth.username ?? ""}:${res.auth.password ?? ""}`)}`]);
      if (res.auth.type === "bearer") headers.push(["Authorization", `Bearer ${res.auth.token ?? ""}`]);
    }
    if (res.auth.type === "apikey" && res.auth.apiKey?.key) {
      if (res.auth.apiKey.in === "header") headers.push([res.auth.apiKey.key, res.auth.apiKey.value]);
      else url += (url.includes("?") ? "&" : "?") + `${res.auth.apiKey.key}=${encodeQueryValue(res.auth.apiKey.value)}`;
    }
    let bodyText: string | null = null;
    let ct: string | null = null;
    const b = res.body;
    switch (b.type) {
      case "json":
        bodyText = b.text ?? "";
        ct = "application/json";
        break;
      case "xml":
        bodyText = b.text ?? "";
        ct = "application/xml";
        break;
      case "text":
        bodyText = b.text ?? "";
        ct = "text/plain";
        break;
      case "graphql":
        bodyText = JSON.stringify({ query: b.graphql?.query ?? "", variables: b.graphql?.variables.trim() ? JSON.parse(b.graphql.variables) : {} });
        ct = "application/json";
        break;
      case "formEncoded":
        bodyText = (b.formEncoded ?? []).map((f) => `${encodeURIComponent(f.key)}=${encodeURIComponent(f.value)}`).join("&");
        ct = "application/x-www-form-urlencoded";
        break;
      case "form": {
        const boundary = `----ReqHarbor${ulid()}`;
        bodyText = (b.form ?? [])
          .map((f) => `--${boundary}\r\nContent-Disposition: form-data; name="${f.key}"${f.kind === "file" ? `; filename="${f.value.split(/[\\/]/).pop()}"` : ""}\r\n\r\n${f.kind === "file" ? "<file contents>" : f.value}\r\n`)
          .join("") + `--${boundary}--\r\n`;
        ct = `multipart/form-data; boundary=${boundary}`;
        break;
      }
      case "binary":
        bodyText = `<binary contents of ${b.binaryPath ?? ""}>`;
        ct = "application/octet-stream";
        break;
      default:
        break;
    }
    if (ct && !has("content-type")) headers.push(["Content-Type", ct]);
    if (!has("accept")) headers.push(["Accept", "*/*"]);
    if (!has("user-agent")) headers.push(["User-Agent", "ReqHarbor/0.1.0"]);
    if (!has("accept-encoding")) headers.push(["Accept-Encoding", "gzip, deflate, br"]);
    let displayUrl = display.url;
    if (display.auth.type === "apikey" && display.auth.apiKey?.in === "query" && display.auth.apiKey.key) {
      displayUrl += (displayUrl.includes("?") ? "&" : "?") + `${display.auth.apiKey.key}=${encodeQueryValue(display.auth.apiKey.value)}`;
    }
    return {
      prepared: {
        method: r.method,
        url,
        headers,
        bodyText,
        timeoutMs: r.settings.timeoutMs,
        verifyTls: r.settings.verifyTls,
        followRedirects: r.settings.followRedirects,
      },
      displayUrl,
      unresolved: [...res.unresolved],
    };
  }

  private meta(responseId: string, f: FakeResponse, fromHistory = false, truncated = false): ResponseMeta {
    const ct = contentTypeOf(f.headers);
    const headers = f.headers.map(([k, v]) => [k.toLowerCase(), v] as [string, string]);
    return {
      responseId,
      status: f.status,
      statusText: f.statusText,
      httpVersion: fromHistory ? "" : f.httpVersion,
      headers,
      cookies: parseCookies(headers),
      timeMs: Math.round(f.latencyMs),
      sizeBytes: f.body.length,
      wireSizeBytes: fromHistory ? null : (f.wireSize ?? f.body.length),
      contentEncoding: fromHistory ? null : f.contentEncoding,
      contentType: ct,
      bodyKind: bodyKindOf(ct, f.body),
      acceptQuery: headers.find(([k]) => k === "accept-query")?.[1] ?? null,
      finalUrl: fromHistory ? null : f.finalUrl,
      redirects: fromHistory ? [] : f.redirects,
      decodeError: null,
      bodyTruncated: truncated,
    };
  }

  private putResponse(id: string, body: Uint8Array): void {
    this.responses.set(id, body);
    if (!id.startsWith("history:")) {
      this.responseOrder.push(id);
      while (this.responseOrder.length > 8) this.responses.delete(this.responseOrder.shift()!);
    }
  }

  private addHistory(entry: HistoryEntry, body: Uint8Array | null): void {
    this.history.unshift(entry);
    if (body) this.historyBodies.set(entry.id, body.slice(0, HISTORY_BODY_MAX));
    while (this.history.length > HISTORY_MAX) {
      const old = this.history.pop()!;
      this.historyBodies.delete(old.id);
    }
  }

  private historyEntry(r: Request, projectId: string | null, ts: number, displayUrl: string, res: FakeResponse | null, error: SendError | null): HistoryEntry {
    const request = clone(r);
    // AC-ENV-7: secrets never reach history
    const entry: HistoryEntry = {
      id: ulid(ts),
      timestamp: ts,
      sourceRequestId: r.collectionId ? r.id : null,
      request,
      resolvedUrl: displayUrl,
      method: r.method,
    };
    void projectId;
    if (res) {
      const isText = !/^(image|application\/(pdf|octet))/.test(contentTypeOf(res.headers) ?? "");
      entry.response = {
        status: res.status,
        statusText: res.statusText,
        timeMs: Math.round(res.latencyMs),
        sizeBytes: res.body.length,
        headers: res.headers.map(([k, v]) => [k.toLowerCase(), v]),
        body: "",
        truncated: res.body.length > HISTORY_BODY_MAX,
        bodyEncoding: isText ? "utf8" : "base64",
      };
    }
    if (error) entry.error = error;
    return entry;
  }

  private recordCanned(r: Request, projectId: string, ts: number): void {
    try {
      const { prepared, displayUrl } = this.prepare(r, projectId);
      try {
        const res = cannedResponse(prepared);
        this.addHistory(this.historyEntry(r, projectId, ts, displayUrl, res, null), res.body);
      } catch (e) {
        if (e instanceof SendFailure) this.addHistory(this.historyEntry(r, projectId, ts, displayUrl, null, { kind: e.kind, message: e.message }), null);
      }
    } catch {
      /* invalid seed request */
    }
  }

  private summary(h: HistoryEntry): HistorySummary {
    return {
      id: h.id,
      timestamp: h.timestamp,
      method: h.method,
      resolvedUrl: h.resolvedUrl,
      status: h.response?.status ?? null,
      timeMs: h.response?.timeMs ?? null,
      errorKind: h.error?.kind ?? null,
      sourceRequestId: h.sourceRequestId,
    };
  }

  private requireSync(): void {
    if (!this.sync.connected) fail("sync_not_connected", "Sync is not connected. Set up sync in Settings → Sync.");
  }

  private snapshotFor(kind: ShareKind, entityId: string, includeCredentials: boolean, environmentId: string | null | undefined) {
    let credentialFields = 0;
    const scrub = (r: Request): Request => {
      const c = clone(r);
      for (const h of c.headers) {
        const k = h.key.toLowerCase();
        if (k === "authorization" || k === "proxy-authorization" || k === "cookie") {
          credentialFields++;
          if (!includeCredentials) h.value = `{{${k === "authorization" ? "authorization" : k === "cookie" ? "cookie" : "proxyAuthorization"}}}`;
        }
      }
      c.auth = scrubAuth(c.auth);
      return c;
    };
    const scrubAuth = (a: Auth): Auth => {
      const c = clone(a);
      if (c.type === "basic") {
        credentialFields += 2;
        if (!includeCredentials) {
          c.username = "{{username}}";
          c.password = "{{password}}";
        }
      } else if (c.type === "bearer") {
        credentialFields++;
        if (!includeCredentials) c.token = "{{token}}";
      } else if (c.type === "apikey" && c.apiKey) {
        credentialFields++;
        if (!includeCredentials) c.apiKey.value = "{{apiKey}}";
      }
      return c;
    };
    const snapshot: ShareSnapshot = { schemaVersion: 1, environment: null };
    let name = "";
    let requestCount = 0;
    let folderCount = 0;
    if (kind === "collection") {
      const col = this.collection(entityId);
      const folders = this.folders.filter((f) => f.collectionId === col.id);
      const requests = this.requests.filter((r) => r.collectionId === col.id).map(scrub);
      snapshot.collection = { ...clone(col), auth: scrubAuth(col.auth), folders: clone(folders), requests };
      name = col.name;
      requestCount = requests.length;
      folderCount = folders.length;
    } else {
      const r = this.request(entityId);
      const inlined = r.auth.type === "inherit" ? { ...r, auth: this.inheritedAuth(r) ?? { type: "none" as const } } : r;
      snapshot.request = scrub(inlined);
      name = r.name;
      requestCount = 1;
    }
    let secretVariablesRemoved = 0;
    if (environmentId) {
      const env = this.env(environmentId);
      secretVariablesRemoved = env.variables.filter((v) => v.secret).length;
      snapshot.environment = { name: env.name, variables: clone(env.variables.filter((v) => !v.secret)) };
    }
    return {
      snapshot,
      preview: {
        name,
        requestCount,
        folderCount,
        secretVariablesRemoved,
        credentialsReplaced: includeCredentials ? 0 : credentialFields,
        credentialFields,
      },
    };
  }

  private shareIdFromUrl(url: string): string {
    const m = /\/s\/([A-Za-z0-9]+)(?:\.json)?\/?$/.exec(url.trim());
    if (!m) fail("invalid", "Not a ReqHarbor share link");
    return m![1];
  }

  private demoShare(): ShareRecord {
    const r = (name: string, method: string, url: string, extra: Partial<Request> = {}): Request => ({
      id: ulid(),
      collectionId: "demo",
      folderId: null,
      name,
      method,
      url,
      params: [],
      headers: [],
      auth: { type: "inherit" },
      body: { type: "none" },
      settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
      sortOrder: 0,
      createdAt: 0,
      updatedAt: 0,
      ...extra,
    });
    const reqs = [
      r("List pets", "GET", "{{baseUrl}}/pets?limit=10"),
      r("Find pets", "QUERY", "{{baseUrl}}/pets/search", { body: { type: "json", text: '{ "species": "cat" }' }, sortOrder: 1 }),
      r("Upload photo", "POST", "{{baseUrl}}/pets/1/photo", { body: { type: "binary", binaryPath: "/etc/passwd" }, sortOrder: 2 }),
    ];
    return {
      info: { shareId: "demo", url: "https://share.reqharbor.dev/s/demo", kind: "collection", entityId: "demo", name: "Pet Store", createdAt: 0, updatedAt: 0 },
      snapshot: {
        schemaVersion: 1,
        collection: { id: "demo", projectId: "", name: "Pet Store", description: "Shared demo", auth: { type: "bearer", token: "{{token}}" }, sortOrder: 0, createdAt: 0, updatedAt: 0, folders: [], requests: reqs },
        environment: { name: "Pet Store", variables: [{ key: "baseUrl", value: "https://petstore.example.com/v1", enabled: true, secret: false }] },
      },
      revoked: false,
    };
  }

  private findShare(url: string): ShareRecord {
    const id = this.shareIdFromUrl(url);
    const rec = id === "demo" ? this.demoShare() : this.shares.get(id);
    if (!rec) return fail("sync_not_found", `not_found: share ${id} does not exist`);
    if (rec.revoked) fail("share_gone", "This share link has been revoked");
    return rec;
  }

  private shareFileWarnings(snap: ShareSnapshot): string[] {
    const reqs = snap.collection ? snap.collection.requests : snap.request ? [snap.request] : [];
    const out: string[] = [];
    for (const r of reqs) {
      if (r.body.type === "binary" && r.body.binaryPath) out.push(`“${r.name}”: binary file path “${r.body.binaryPath}” was cleared — choose the file again`);
      for (const f of r.body.form ?? []) if (f.kind === "file" && f.value) out.push(`“${r.name}”: form file “${f.key}” (${f.value}) was cleared and disabled`);
    }
    return out;
  }

  private copyCollectionTree(src: Collection & { folders?: Folder[]; requests?: Request[] }, projectId: string, folders: Folder[], requests: Request[], name: string, sortOrder: number): Collection {
    const ts = this.now();
    const col: Collection = { ...clone(src), id: ulid(ts), projectId, name, sortOrder, createdAt: ts, updatedAt: ts };
    delete (col as { folders?: unknown }).folders;
    delete (col as { requests?: unknown }).requests;
    this.collections.push(col);
    const idMap = new Map<string, string>();
    for (const f of folders) idMap.set(f.id, ulid(this.now()));
    for (const f of folders) {
      this.folders.push({ ...clone(f), id: idMap.get(f.id)!, collectionId: col.id, parentFolderId: f.parentFolderId ? (idMap.get(f.parentFolderId) ?? null) : null, createdAt: ts, updatedAt: ts });
    }
    for (const r of requests) {
      this.requests.push({ ...clone(r), id: ulid(this.now()), collectionId: col.id, folderId: r.folderId ? (idMap.get(r.folderId) ?? null) : null, createdAt: ts, updatedAt: ts });
    }
    return col;
  }

  // ───────────── command handlers ─────────────
  handlers: Handlers = {
    bootstrap: (): Bootstrap => {
      const ua = typeof navigator !== "undefined" ? navigator.userAgent : "";
      const platform = this.opts.platform ?? (/Mac/i.test(ua) ? "macos" : /Win/i.test(ua) ? "windows" : "linux");
      return clone({
        workspace: this.workspace,
        projects: this.sortedProjects(),
        environments: this.sortedEnvs(),
        settings: this.settings,
        corruptFiles: this.opts.corrupt
          ? [{ originalPath: "/mock/reqharbor/projects/01JCORRUPT.json", renamedTo: "/mock/reqharbor/projects/01JCORRUPT.corrupt-1764000000000.json", error: "expected value at line 1 column 1" }]
          : [],
        sync: this.sync,
        platform,
        dataDir: "/mock/reqharbor",
      });
    },
    get_settings: () => clone(this.settings),
    save_settings: ({ settings }) => {
      for (const k of ["sidebarWidth", "windowWidth", "windowHeight"] as const) {
        const v = settings[k];
        if (v !== null && v !== undefined && (!Number.isInteger(v) || (v as number) < 0)) {
          fail("invalid", `invalid args 'settings' for command 'save_settings': invalid type: floating point '${v}', expected u32`);
        }
      }
      this.settings = { ...defaultSettings(), ...this.settings, ...clone(settings) };
      return clone(this.settings);
    },

    list_projects: () => this.sortedProjects(),
    get_project_tree: ({ projectId }) => this.tree(projectId),
    create_project: ({ name }) => {
      const ts = this.now();
      const p: Project = { id: ulid(ts), workspaceId: this.workspace.id, name: this.name(name), sortOrder: this.projects.length, activeEnvironmentId: null, createdAt: ts, updatedAt: ts };
      this.projects.push(p);
      this.markChanged();
      return this.tree(p.id);
    },
    rename_project: ({ projectId, name }) => {
      const p = this.project(projectId);
      p.name = this.name(name);
      return clone(this.touch(p));
    },
    duplicate_project: ({ projectId }) => {
      const src = this.project(projectId);
      const ts = this.now();
      const p: Project = { ...clone(src), id: ulid(ts), name: `${src.name} copy`, sortOrder: this.projects.length, createdAt: ts, updatedAt: ts };
      this.projects.push(p);
      const envMap = new Map<string, string>();
      for (const e of this.environments.filter((x) => x.projectId === src.id)) {
        const ne = { ...clone(e), id: ulid(this.now()), projectId: p.id, createdAt: ts, updatedAt: ts };
        envMap.set(e.id, ne.id);
        this.environments.push(ne);
      }
      p.activeEnvironmentId = src.activeEnvironmentId ? (envMap.get(src.activeEnvironmentId) ?? null) : null;
      for (const c of this.collections.filter((x) => x.projectId === src.id)) {
        this.copyCollectionTree(c, p.id, this.folders.filter((f) => f.collectionId === c.id), this.requests.filter((r) => r.collectionId === c.id), c.name, c.sortOrder);
      }
      this.markChanged();
      return this.tree(p.id);
    },
    delete_project: ({ projectId }) => {
      this.project(projectId);
      if (this.projects.length <= 1) fail("invalid", "Cannot delete the last project");
      const cols = new Set(this.collections.filter((c) => c.projectId === projectId).map((c) => c.id));
      this.requests = this.requests.filter((r) => !cols.has(r.collectionId));
      this.folders = this.folders.filter((f) => !cols.has(f.collectionId));
      this.collections = this.collections.filter((c) => !cols.has(c.id));
      this.environments = this.environments.filter((e) => e.projectId !== projectId);
      this.projects = this.projects.filter((p) => p.id !== projectId);
      this.projects.sort((a, b) => a.sortOrder - b.sortOrder).forEach((p, i) => (p.sortOrder = i));
      this.markChanged();
      return this.sortedProjects();
    },
    move_project: ({ projectId, index }) => {
      const p = this.project(projectId);
      const list = [...this.projects].sort((a, b) => a.sortOrder - b.sortOrder).filter((x) => x.id !== projectId);
      list.splice(Math.max(0, Math.min(index, list.length)), 0, p);
      list.forEach((x, i) => (x.sortOrder = i));
      this.touch(p);
      return this.sortedProjects();
    },
    set_active_environment: ({ projectId, environmentId }) => {
      const p = this.project(projectId);
      if (environmentId) {
        const e = this.env(environmentId);
        if (e.projectId !== projectId) fail("invalid", "The environment does not belong to this project");
      }
      p.activeEnvironmentId = environmentId ?? null;
      return clone(this.touch(p));
    },
    child_counts: ({ kind, id }): ChildCounts => {
      const countIn = (cols: string[], folderFilter?: Set<string>) => ({
        folders: this.folders.filter((f) => cols.includes(f.collectionId) && (!folderFilter || folderFilter.has(f.id))).length,
        requests: this.requests.filter((r) => cols.includes(r.collectionId) && (!folderFilter || (r.folderId !== null && folderFilter.has(r.folderId)))).length,
      });
      if (kind === "project") {
        this.project(id);
        const cols = this.collections.filter((c) => c.projectId === id).map((c) => c.id);
        return { collections: cols.length, ...countIn(cols) };
      }
      if (kind === "collection") {
        this.collection(id);
        return { collections: 0, ...countIn([id]) };
      }
      if (kind === "folder") {
        const f = this.folder(id);
        const desc = this.descendantFolders(id);
        const all = new Set([id, ...desc.map((d) => d.id)]);
        return { collections: 0, folders: desc.length, requests: this.requests.filter((r) => r.collectionId === f.collectionId && r.folderId !== null && all.has(r.folderId)).length };
      }
      this.request(id);
      return { collections: 0, folders: 0, requests: 0 };
    },

    create_collection: ({ projectId, name }) => {
      this.project(projectId);
      const ts = this.now();
      const c: Collection = {
        id: ulid(ts),
        projectId,
        name: this.name(name),
        description: "",
        auth: { type: "none" },
        sortOrder: Math.max(-1, ...this.collections.filter((x) => x.projectId === projectId).map((x) => x.sortOrder)) + 1,
        createdAt: ts,
        updatedAt: ts,
      };
      this.collections.push(c);
      this.markChanged();
      return clone(c);
    },
    update_collection: ({ collection }) => {
      const c = this.collection(collection.id);
      c.name = this.name(collection.name);
      c.description = collection.description ?? "";
      c.auth = clone(collection.auth);
      return clone(this.touch(c));
    },
    duplicate_collection: ({ collectionId }) => {
      const c = this.collection(collectionId);
      const sort = Math.max(...this.collections.filter((x) => x.projectId === c.projectId).map((x) => x.sortOrder)) + 1;
      this.copyCollectionTree(c, c.projectId, this.folders.filter((f) => f.collectionId === c.id), this.requests.filter((r) => r.collectionId === c.id), `${c.name} copy`, sort);
      this.markChanged();
      return this.tree(c.projectId);
    },
    delete_collection: ({ collectionId }) => {
      const c = this.collection(collectionId);
      this.requests = this.requests.filter((r) => r.collectionId !== collectionId);
      this.folders = this.folders.filter((f) => f.collectionId !== collectionId);
      this.collections = this.collections.filter((x) => x.id !== collectionId);
      this.markChanged();
      return this.tree(c.projectId);
    },

    create_folder: ({ collectionId, parentFolderId, name }) => {
      this.collection(collectionId);
      if (parentFolderId) {
        const p = this.folder(parentFolderId);
        if (p.collectionId !== collectionId) fail("invalid", "Parent folder belongs to another collection");
      }
      const ts = this.now();
      const f: Folder = { id: ulid(ts), collectionId, parentFolderId: parentFolderId ?? null, name: this.name(name), sortOrder: this.nextSort(collectionId, parentFolderId ?? null), createdAt: ts, updatedAt: ts };
      this.folders.push(f);
      this.markChanged();
      return clone(f);
    },
    rename_folder: ({ folderId, name }) => {
      const f = this.folder(folderId);
      f.name = this.name(name);
      return clone(this.touch(f));
    },
    duplicate_folder: ({ folderId }) => {
      const src = this.folder(folderId);
      const ts = this.now();
      const idMap = new Map<string, string>();
      const nf: Folder = { ...clone(src), id: ulid(ts), name: `${src.name} copy`, sortOrder: this.nextSort(src.collectionId, src.parentFolderId), createdAt: ts, updatedAt: ts };
      idMap.set(src.id, nf.id);
      this.folders.push(nf);
      const desc = this.descendantFolders(src.id);
      for (const d of desc) idMap.set(d.id, ulid(this.now()));
      for (const d of desc) this.folders.push({ ...clone(d), id: idMap.get(d.id)!, parentFolderId: idMap.get(d.parentFolderId!) ?? null, createdAt: ts, updatedAt: ts });
      for (const r of this.requests.filter((x) => x.folderId && idMap.has(x.folderId) && x.collectionId === src.collectionId)) {
        this.requests.push({ ...clone(r), id: ulid(this.now()), folderId: idMap.get(r.folderId!)!, createdAt: ts, updatedAt: ts });
      }
      this.markChanged();
      return this.tree(this.projectOfCollection(src.collectionId));
    },
    delete_folder: ({ folderId }) => {
      const f = this.folder(folderId);
      const ids = new Set([folderId, ...this.descendantFolders(folderId).map((d) => d.id)]);
      this.folders = this.folders.filter((x) => !ids.has(x.id));
      this.requests = this.requests.filter((r) => !(r.folderId && ids.has(r.folderId)));
      this.markChanged();
      return this.tree(this.projectOfCollection(f.collectionId));
    },

    create_request: ({ collectionId, folderId, request }) => {
      this.collection(collectionId);
      if (folderId) {
        const f = this.folder(folderId);
        if (f.collectionId !== collectionId) fail("invalid", "Folder belongs to another collection");
      }
      if (request && !METHOD_RE.test(request.method)) fail("invalid", `Invalid method "${request.method}"`);
      const ts = this.now();
      const base: Request = request
        ? clone(request)
        : { id: "", collectionId, folderId: null, name: "Untitled request", method: "GET", url: "", params: [], headers: [], auth: { type: "none" }, body: { type: "none" }, settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true }, sortOrder: 0, createdAt: 0, updatedAt: 0 };
      const r: Request = { ...base, id: ulid(ts), collectionId, folderId: folderId ?? null, name: base.name.trim() || "Untitled request", sortOrder: this.nextSort(collectionId, folderId ?? null), createdAt: ts, updatedAt: ts };
      this.requests.push(r);
      this.markChanged();
      return clone(r);
    },
    save_request: ({ request }) => {
      const r = this.request(request.id);
      if (!METHOD_RE.test(request.method)) fail("invalid", `Invalid method "${request.method}"`);
      const { collectionId, folderId, sortOrder, createdAt } = r;
      Object.assign(r, clone(request), { collectionId, folderId, sortOrder, createdAt });
      if (!r.name.trim()) r.name = "Untitled request";
      return clone(this.touch(r));
    },
    rename_request: ({ requestId, name }) => {
      const r = this.request(requestId);
      r.name = this.name(name);
      return clone(this.touch(r));
    },
    duplicate_request: ({ requestId }) => {
      const src = this.request(requestId);
      const ts = this.now();
      const r: Request = { ...clone(src), id: ulid(ts), name: `${src.name} copy`, sortOrder: this.nextSort(src.collectionId, src.folderId), createdAt: ts, updatedAt: ts };
      this.requests.push(r);
      this.markChanged();
      return clone(r);
    },
    delete_request: ({ requestId }) => {
      const r = this.request(requestId);
      this.requests = this.requests.filter((x) => x.id !== requestId);
      this.markChanged();
      return this.tree(this.projectOfCollection(r.collectionId));
    },
    move_node: ({ kind, id, targetCollectionId, targetFolderId, index }) => {
      if (kind === "collection") {
        const c = this.collection(id);
        const list = this.collections.filter((x) => x.projectId === c.projectId && x.id !== id).sort((a, b) => a.sortOrder - b.sortOrder);
        list.splice(Math.max(0, Math.min(index, list.length)), 0, c);
        list.forEach((x, i) => (x.sortOrder = i));
        this.touch(c);
        return this.tree(c.projectId);
      }
      if (!targetCollectionId) return fail("invalid", "targetCollectionId is required");
      const target = this.collection(targetCollectionId);
      const tf = targetFolderId ? this.folder(targetFolderId) : null;
      if (tf && tf.collectionId !== target.id) fail("invalid", "Target folder belongs to another collection");
      const node = kind === "folder" ? this.folder(id) : this.request(id);
      const srcCollection = this.collection(node.collectionId);
      if (srcCollection.projectId !== target.projectId) fail("invalid", "Cannot move across projects");
      if (kind === "folder") {
        if (targetFolderId === id || (targetFolderId && this.descendantFolders(id).some((d) => d.id === targetFolderId))) {
          fail("invalid", "Cannot move a folder into itself");
        }
      }
      const parent = targetFolderId ?? null;
      const sib = this.siblingsOf(target.id, parent);
      const sameKind = (kind === "folder" ? sib.folders : sib.requests).filter((x) => x.id !== id) as (Folder | Request)[];
      sameKind.splice(Math.max(0, Math.min(index, sameKind.length)), 0, node);
      if (kind === "folder") {
        const f = node as Folder;
        const moved = [f, ...this.descendantFolders(f.id)];
        const ids = new Set(moved.map((m) => m.id));
        if (f.collectionId !== target.id) {
          for (const m of moved) m.collectionId = target.id;
          for (const r of this.requests) if (r.folderId && ids.has(r.folderId)) r.collectionId = target.id;
        }
        f.parentFolderId = parent;
      } else {
        const r = node as Request;
        r.collectionId = target.id;
        r.folderId = parent;
      }
      const folders = kind === "folder" ? (sameKind as Folder[]) : sib.folders;
      const reqs = kind === "request" ? (sameKind as Request[]) : sib.requests;
      [...folders, ...reqs].forEach((x, i) => (x.sortOrder = i));
      this.touch(node);
      return this.tree(target.projectId);
    },

    list_environments: () => this.sortedEnvs(),
    create_environment: ({ projectId, name }) => {
      this.project(projectId);
      const ts = this.now();
      const e: Environment = {
        id: ulid(ts),
        workspaceId: this.workspace.id,
        projectId,
        name: this.name(name),
        variables: [],
        sortOrder: Math.max(-1, ...this.environments.filter((x) => x.projectId === projectId).map((x) => x.sortOrder)) + 1,
        createdAt: ts,
        updatedAt: ts,
      };
      this.environments.push(e);
      this.markChanged();
      return clone(e);
    },
    update_environment: ({ environment }) => {
      const e = this.env(environment.id);
      e.name = this.name(environment.name);
      e.variables = clone(environment.variables);
      return clone(this.touch(e));
    },
    duplicate_environment: ({ environmentId }) => {
      const src = this.env(environmentId);
      if (src.projectId === null) fail("invalid", "The Global environment cannot be duplicated");
      const ts = this.now();
      const e: Environment = { ...clone(src), id: ulid(ts), name: `${src.name} copy`, sortOrder: src.sortOrder + 0.5, createdAt: ts, updatedAt: ts };
      this.environments.push(e);
      this.environments.filter((x) => x.projectId === src.projectId).sort((a, b) => a.sortOrder - b.sortOrder).forEach((x, i) => (x.sortOrder = i));
      this.markChanged();
      return clone(e);
    },
    delete_environment: ({ environmentId }) => {
      const e = this.env(environmentId);
      if (e.projectId === null) fail("invalid", "The Global environment cannot be deleted");
      this.environments = this.environments.filter((x) => x.id !== environmentId);
      for (const p of this.projects) if (p.activeEnvironmentId === environmentId) this.touch(p).activeEnvironmentId = null;
      this.markChanged();
      return this.sortedEnvs();
    },
    get_variables: ({ projectId }): VariableInfo[] =>
      [...this.lookup(projectId ?? this.settings.activeProjectId).values()].map(({ v, env }) => ({
        key: v.key,
        value: v.value,
        secret: v.secret,
        environmentId: env.id,
        environmentName: env.name,
      })),

    list_history: () => this.history.map((h) => this.summary(h)),
    open_history_entry: ({ historyId }): OpenedHistory => {
      const h = this.history.find((x) => x.id === historyId) ?? fail("not_found", `History entry ${historyId} not found`);
      const entry = clone(h);
      if (!h.response) return { entry, response: null };
      const body = this.historyBodies.get(h.id) ?? new Uint8Array();
      const responseId = `history:${h.id}`;
      this.putResponse(responseId, body);
      const fake: FakeResponse = {
        status: h.response.status,
        statusText: h.response.statusText,
        httpVersion: "",
        headers: h.response.headers,
        body,
        wireSize: null,
        contentEncoding: null,
        latencyMs: h.response.timeMs,
        finalUrl: h.resolvedUrl,
        redirects: [],
      };
      const meta = this.meta(responseId, fake, true, h.response.truncated);
      meta.sizeBytes = h.response.sizeBytes;
      return { entry, response: meta };
    },
    delete_history_entry: ({ historyId }) => {
      const before = this.history.length;
      this.history = this.history.filter((h) => h.id !== historyId);
      if (this.history.length === before) fail("not_found", `History entry ${historyId} not found`);
      this.historyBodies.delete(historyId);
      return null;
    },
    clear_history: () => {
      this.history = [];
      this.historyBodies.clear();
      return null;
    },

    send_request: async ({ executionId, request, projectId }): Promise<SendResult> => {
      const pid = projectId ?? (request.collectionId ? this.collections.find((c) => c.id === request.collectionId)?.projectId : null) ?? this.settings.activeProjectId;
      const { prepared, displayUrl, unresolved } = this.prepare(request, pid);
      const ctrl = new AbortController();
      this.inflight.set(executionId, ctrl);
      const started = performance.now();
      const ts = this.now();
      let res: FakeResponse | null = null;
      let error: SendError | null = null;
      try {
        const u = new URL(prepared.url);
        const isLocal = /^(localhost|127\.0\.0\.1|\[::1\])$/.test(u.hostname) && u.port !== "1";
        const timeout = prepared.timeoutMs > 0 ? prepared.timeoutMs : Infinity;
        const delayMatch = /\/delay\/(\d+(?:\.\d+)?)/.exec(u.pathname);
        const wait = delayMatch ? Number(delayMatch[1]) * 1000 : 0;
        if (wait > timeout || /\/timeout$/.test(u.pathname)) {
          await this.delay(Math.min(timeout === Infinity ? 60000 : timeout, 60000), ctrl.signal);
          throw new SendFailure("timeout", `Request timed out after ${prepared.timeoutMs} ms`);
        }
        if (wait) await this.delay(wait, ctrl.signal);
        if (isLocal) res = await realFetch(prepared, ctrl.signal);
        else {
          res = cannedResponse(prepared);
          await this.delay(res.latencyMs, ctrl.signal);
        }
        if (wait) res.latencyMs = performance.now() - started;
      } catch (e) {
        if (e instanceof SendFailure) error = { kind: e.kind, message: e.message };
        else if ((e as Error).name === "AbortError") error = { kind: "cancelled", message: `Request cancelled after ${Math.round(performance.now() - started)} ms` };
        else error = { kind: "other", message: String((e as Error).message ?? e) };
        res = null;
      } finally {
        this.inflight.delete(executionId);
      }
      const entry = this.historyEntry(request, pid, ts, displayUrl, res, error);
      this.addHistory(entry, res?.body ?? null);
      let response: ResponseMeta | null = null;
      if (res) {
        this.putResponse(executionId, res.body);
        response = this.meta(executionId, res);
      }
      return { executionId, historyId: entry.id, method: request.method, resolvedUrl: displayUrl, unresolvedVariables: unresolved, response, error };
    },
    cancel_request: ({ executionId }) => {
      const c = this.inflight.get(executionId);
      if (!c) return false;
      c.abort();
      return true;
    },
    get_response_body: ({ responseId, limit }) => {
      const body = this.responses.get(responseId) ?? fail("not_found", `Response ${responseId} is no longer cached`);
      const end = limit != null ? Math.min(limit, body.length) : body.length;
      return body.slice(0, end).buffer as ArrayBuffer;
    },
    save_response_body: ({ responseId, path }) => {
      if (!this.responses.has(responseId)) fail("not_found", `Response ${responseId} is no longer cached`);
      if (!path) fail("invalid", "No path given");
      return null;
    },

    format_body: ({ kind, text }) => {
      if (kind === "json") {
        try {
          JSON.parse(text);
        } catch (e) {
          const p = jsonErrorPosition(text, e);
          return fail("format_error", `Invalid JSON at line ${p.line}, col ${p.column}`, { message: p.message, line: p.line, column: p.column });
        }
        return prettyJson(text) ?? text;
      }
      if (kind === "xml") {
        try {
          return formatXml(text);
        } catch (e) {
          const line = (e as { line?: number }).line ?? 1;
          return fail("format_error", `Invalid XML at line ${line}, col 1`, { message: (e as Error).message, line, column: 1 });
        }
      }
      return fail("invalid", `Unknown format kind ${String(kind)}`);
    },
    url_to_params: ({ url, params }) => urlToParams(url, params),
    params_to_url: ({ url, params }) => paramsToUrl(url, params),

    import_curl: ({ text }) => importCurl(text),
    looks_like_curl: ({ text }) => looksLikeCurl(text),
    export_curl: ({ request, projectId, keepVariables }) => {
      if (keepVariables) return exportCurl(request, true, this.inheritedAuth(request));
      const pid = projectId ?? (request.collectionId ? this.collections.find((c) => c.id === request.collectionId)?.projectId : null) ?? this.settings.activeProjectId;
      const res = this.resolveRequest({ ...request, params: [] }, pid, false);
      const { resolve } = this.resolver(pid, false);
      const r: Request = {
        ...request,
        url: res.url,
        headers: request.headers.map((h) => ({ ...h, key: resolve(h.key), value: resolve(h.value) })),
        auth: res.auth,
        body: { ...res.body, form: request.body.form?.map((f) => ({ ...f, key: resolve(f.key), value: resolve(f.value) })), formEncoded: request.body.formEncoded?.map((f) => ({ ...f, key: resolve(f.key), value: resolve(f.value) })) },
      };
      return exportCurl(r, false, null);
    },

    preview_openapi: ({ text, path }) => {
      const src = this.specText(text, path);
      const c = convertSpec(src);
      return { spec: c.spec, report: reportOf(c) };
    },
    import_openapi: ({ projectId, text, path }) => {
      this.project(projectId);
      const c = convertSpec(this.specText(text, path));
      const ts = this.now();
      const col: Collection = {
        id: ulid(ts),
        projectId,
        name: c.title,
        description: "Imported from OpenAPI",
        auth: c.auth,
        sortOrder: Math.max(-1, ...this.collections.filter((x) => x.projectId === projectId).map((x) => x.sortOrder)) + 1,
        createdAt: ts,
        updatedAt: ts,
      };
      this.collections.push(col);
      const folderIds = new Map<string, string>();
      let fi = 0;
      const counters = new Map<string, number>();
      for (const r of c.requests) {
        let folderId: string | null = null;
        if (r.folder) {
          if (!folderIds.has(r.folder)) {
            const f: Folder = { id: ulid(this.now()), collectionId: col.id, parentFolderId: null, name: r.folder, sortOrder: fi++, createdAt: ts, updatedAt: ts };
            this.folders.push(f);
            folderIds.set(r.folder, f.id);
          }
          folderId = folderIds.get(r.folder)!;
        }
        const k = folderId ?? "";
        const so = counters.get(k) ?? (folderId ? 0 : 1000);
        counters.set(k, so + 1);
        const url = r.url + (r.params.some((p) => p.enabled) ? "?" + r.params.filter((p) => p.enabled).map((p) => (p.value ? `${p.key}=${p.value}` : p.key)).join("&") : "");
        this.requests.push({
          id: ulid(this.now()),
          collectionId: col.id,
          folderId,
          name: r.name,
          method: r.method,
          url,
          params: r.params,
          headers: r.headers,
          auth: r.auth,
          body: r.body,
          settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
          sortOrder: so,
          createdAt: ts,
          updatedAt: ts,
        });
      }
      const env: Environment = {
        id: ulid(this.now()),
        workspaceId: this.workspace.id,
        projectId,
        name: c.title,
        variables: c.envVariables,
        sortOrder: Math.max(-1, ...this.environments.filter((x) => x.projectId === projectId).map((x) => x.sortOrder)) + 1,
        createdAt: ts,
        updatedAt: ts,
      };
      this.environments.push(env);
      this.markChanged();
      return { report: reportOf(c), collectionId: col.id, environment: clone(env), tree: this.tree(projectId) };
    },

    get_sync_status: () => {
      this.refreshSkipped();
      return clone(this.sync);
    },
    sync_create_workspace: async ({ workerUrl }) => {
      const url = this.workerUrl(workerUrl);
      await this.delay(150);
      const token = `rh_${ulid()}${ulid()}`.slice(0, 46);
      this.connect(url, token);
      return { workspaceId: this.sync.remoteWorkspaceId!, token };
    },
    sync_connect: async ({ workerUrl, token }) => {
      const url = this.workerUrl(workerUrl);
      await this.delay(150);
      if (!/^rh_[A-Za-z0-9_-]{10,}$/.test(token.trim())) {
        this.sync = { ...this.sync, state: "unauthorized", connected: false, lastError: "401 unauthorized: invalid token" };
        this.emit("sync://status", this.sync);
        return fail("sync_unauthorized", "Token rejected — sync disconnected. Enter a new token.");
      }
      this.connect(url, token.trim());
      return clone(this.sync);
    },
    sync_disconnect: () => {
      this.sync = { ...this.sync, state: "disconnected", connected: false, workerUrl: null, remoteWorkspaceId: null, lastError: null, pendingChanges: 0, nextRetryAt: null, retryAttempt: 0, skippedEntities: [], warning: null };
      this.syncToken = null;
      this.emit("sync://status", this.sync);
      return clone(this.sync);
    },
    sync_rotate_token: async () => {
      this.requireSync();
      await this.delay(100);
      const token = `rh_${ulid()}${ulid()}`.slice(0, 46);
      this.syncToken = token;
      if (this.sync.workerUrl?.includes("rotate-fail")) {
        return fail("io", "The new token could not be saved to disk. Copy it now and reconnect.", { token } as never);
      }
      return { token };
    },
    sync_now: async (): Promise<SyncReport> => {
      this.requireSync();
      if (this.sync.workerUrl?.includes("full")) {
        this.sync = { ...this.sync, state: "error", lastError: "quota_exceeded: workspace storage is full (50 MB)" };
        this.emit("sync://status", this.sync);
        return fail("sync_quota_exceeded", "quota_exceeded: The remote workspace is full (50 MB). Delete items or large bodies to keep syncing.");
      }
      if (this.sync.workerUrl?.includes("offline")) {
        const next = this.now() + 30000;
        this.sync = { ...this.sync, state: "offline", lastError: "network unreachable", nextRetryAt: next, retryAttempt: this.sync.retryAttempt + 1 };
        this.emit("sync://status", this.sync);
        return fail("sync_offline", "The sync server is unreachable. Retrying automatically.");
      }
      this.sync = { ...this.sync, state: "syncing" };
      this.emit("sync://status", this.sync);
      await this.delay(200);
      const pushed = this.sync.pendingChanges;
      this.refreshSkipped();
      const ts = this.now();
      this.sync = { ...this.sync, state: "idle", lastSyncAt: ts, lastPullAt: ts, pendingChanges: 0, lastError: null, nextRetryAt: null, retryAttempt: 0 };
      this.emit("sync://status", this.sync);
      return { pushed, pulled: 0, deleted: 0, conflicts: [], skipped: [], cursor: ts, tooLarge: clone(this.sync.skippedEntities) };
    },
    sync_workspace_info: async () => {
      this.requireSync();
      await this.delay(80);
      const bytes = enc.encode(JSON.stringify([this.projects, this.collections, this.folders, this.requests, this.environments])).length;
      return {
        workspaceId: this.sync.remoteWorkspaceId!,
        name: this.workspace.name,
        createdAt: this.sync.lastSyncAt ?? this.now(),
        entityCount: this.projects.length + this.collections.length + this.folders.length + this.requests.length + this.environments.length,
        bytesUsed: bytes,
        quotaBytes: 50 * 1024 * 1024,
      };
    },

    share_preview: ({ kind, entityId, includeCredentials, environmentId }) => this.snapshotFor(kind, entityId, includeCredentials, environmentId).preview,
    share_create: async ({ kind, entityId, includeCredentials, environmentId }) => {
      this.requireSync();
      const { snapshot, preview } = this.snapshotFor(kind, entityId, includeCredentials, environmentId);
      await this.delay(120);
      const shareId = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"[b % 62]).join("") + "x";
      const ts = this.now();
      const info: ShareInfo = { shareId, url: `${this.sync.workerUrl}/s/${shareId}`, kind, entityId, name: preview.name, createdAt: ts, updatedAt: ts };
      this.shares.set(shareId, { info, snapshot, revoked: false });
      return clone(info);
    },
    share_update: async ({ shareId, kind, entityId, includeCredentials, environmentId }) => {
      this.requireSync();
      const rec = this.shares.get(shareId) ?? fail("sync_not_found", `not_found: share ${shareId} does not exist`);
      if (rec.revoked) fail("share_gone", "This share link has been revoked");
      const { snapshot, preview } = this.snapshotFor(kind, entityId, includeCredentials, environmentId);
      rec.snapshot = snapshot;
      rec.info = { ...rec.info, kind, entityId, name: preview.name, updatedAt: this.now() };
      return clone(rec.info);
    },
    share_revoke: ({ shareId }) => {
      this.requireSync();
      const rec = this.shares.get(shareId) ?? fail("sync_not_found", `not_found: share ${shareId} does not exist`);
      rec.revoked = true;
      return null;
    },
    share_list: () => {
      this.requireSync();
      return clone([...this.shares.values()].filter((s) => !s.revoked).map((s) => s.info));
    },
    import_share: ({ url, projectId }) => {
      this.project(projectId);
      const rec = this.findShare(url);
      const snap = clone(rec.snapshot);
      const warnings = this.shareFileWarnings(snap);
      const clean = (r: Request): Request => {
        if (r.body.type === "binary") r.body.binaryPath = "";
        for (const f of r.body.form ?? []) if (f.kind === "file") {
          f.value = "";
          f.enabled = false;
        }
        return r;
      };
      const sort = Math.max(-1, ...this.collections.filter((x) => x.projectId === projectId).map((x) => x.sortOrder)) + 1;
      if (snap.collection) {
        this.copyCollectionTree(snap.collection, projectId, snap.collection.folders, snap.collection.requests.map(clean), snap.collection.name, sort);
      } else if (snap.request) {
        const ts = this.now();
        const col: Collection = { id: ulid(ts), projectId, name: snap.request.name, description: "Imported from share link", auth: { type: "none" }, sortOrder: sort, createdAt: ts, updatedAt: ts };
        this.collections.push(col);
        this.requests.push({ ...clean(snap.request), id: ulid(this.now()), collectionId: col.id, folderId: null, sortOrder: 0, createdAt: ts, updatedAt: ts });
      }
      if (snap.environment) {
        const ts = this.now();
        this.environments.push({ id: ulid(ts), workspaceId: this.workspace.id, projectId, name: snap.environment.name, variables: snap.environment.variables, sortOrder: 99, createdAt: ts, updatedAt: ts });
      }
      this.markChanged();
      return { tree: this.tree(projectId), warnings };
    },
    preview_share: ({ url }) => {
      const rec = this.findShare(url);
      const snap = clone(rec.snapshot);
      return { kind: snap.collection ? "collection" : "request", snapshot: snap, warnings: this.shareFileWarnings(snap) };
    },
  };

  private specText(text: string | null | undefined, path: string | null | undefined): string {
    if (text && text.trim()) return text;
    if (path) return fail("io", `Cannot read ${path}: files can only be read by the desktop app (paste the spec instead)`);
    return fail("openapi_parse", "Paste a spec or choose a file");
  }

  private workerUrl(u: string): string {
    const t = u.trim().replace(/\/+$/, "");
    if (!/^https?:\/\/[^/\s]+/.test(t)) fail("invalid", "Enter a valid Worker URL (https://…)");
    return t;
  }

  private connect(workerUrl: string, token: string): void {
    const ts = this.now();
    this.syncToken = token;
    this.sync = {
      ...this.sync,
      state: "idle",
      connected: true,
      workerUrl,
      remoteWorkspaceId: this.sync.remoteWorkspaceId ?? ulid(ts),
      lastSyncAt: ts,
      lastPullAt: ts,
      lastError: null,
      nextRetryAt: null,
      retryAttempt: 0,
      pendingChanges: 0,
    };
    this.refreshSkipped();
    this.emit("sync://status", this.sync);
  }

  async invoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const h = (this.handlers as Record<string, (a: unknown) => unknown>)[cmd];
    if (!h) throw new MockError("internal", `Unknown command ${cmd}`);
    try {
      // Simulate the IPC boundary: callers never share references with the store.
      const result = await h(clone(args ?? {}));
      return result instanceof ArrayBuffer ? result : result === undefined ? null : clone(result);
    } catch (e) {
      if (e instanceof MockError) throw { code: e.code, message: e.message, ...(e.details !== undefined ? { details: e.details } : {}) };
      throw { code: "internal", message: e instanceof Error ? e.message : String(e) };
    }
  }

  backend(): Backend {
    return {
      invoke: (cmd, args) => this.invoke(cmd, args),
      listen: async (event, handler): Promise<Unlisten> => {
        const set = this.listeners.get(event) ?? new Set();
        this.listeners.set(event, set);
        const fn = handler as (p: unknown) => void;
        set.add(fn);
        return () => set.delete(fn);
      },
    };
  }
}

let singleton: MockServer | null = null;

export function getMockServer(): MockServer {
  if (!singleton) {
    const qs = typeof location !== "undefined" ? new URLSearchParams(location.search) : new URLSearchParams();
    singleton = new MockServer({
      empty: qs.get("seed") === "empty",
      platform: qs.get("platform") ?? undefined,
      corrupt: qs.get("corrupt") === "1",
    });
    if (typeof window !== "undefined") (window as unknown as { __mock?: MockServer }).__mock = singleton;
  }
  return singleton;
}

export function getMockBackend(): Backend {
  return getMockServer().backend();
}

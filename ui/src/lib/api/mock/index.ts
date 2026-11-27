/**
 * In-memory implementation of the full Commands map for running the UI in a
 * plain browser (vite dev, Playwright). Mirrors API_CONTRACT.md: same result
 * shapes, same CommandError codes. State lives in memory and resets on reload.
 *
 * Query-string switches (browser only): `?seed=empty` (first launch),
 * `?platform=macos|windows|linux`, `?corrupt=1` (one quarantined file),
 * `?newer=1` (adds requests written by a newer ReqHarbor: a gRPC one and one
 * with an unknown body type, AC-FWD-3), `?mockAccounts=off` (hides sign-in,
 * like a release build). Sign-in switches are documented in ./account.ts.
 */
import { dynamicCatalogue, dynamicMemo } from "../../util/dynamic";
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
  GraphqlSchema,
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
  TeamRole,
  TransferPreview,
  CredentialRow,
  Variable,
  VariableInfo,
} from "../types";
import { METHOD_RE } from "../types";
import { ulid } from "../../util/ulid";
import { isContainerReadOnly, isKnownAuth, isReadOnlyNewer } from "../../util/request";
import { skippedWarning } from "../../util/format";
import { applyDefaultScheme, paramsToUrl, urlToParams, encodeQueryValue } from "../../util/urlx";
import { prettyJson, jsonErrorPosition } from "../../util/json";
import { MockError, fail } from "./errors";
import { MockAccounts, type MockAccountOptions } from "./account";
import { MockSpaces, LEGACY, type MockSpaceMode, type MockSpacesOptions } from "./spaces";
import { MockTeams } from "./teams";
import { isInsecureUrl } from "../../util/account";
import { seed, defaultSettings } from "./seed";
import { importCurl, looksLikeCurl, exportCurl } from "./curl";
import { convertSpec, reportOf } from "./openapi";
import { MockChannel, MockWs, isMockChannel, toOutMessage } from "./ws";
import { normalizeNamespace, stampMinClientVersion, wsUrl } from "../../util/ws";
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
/** Mirrors Bootstrap.appVersion of the real app (workspace version). */
export const MOCK_APP_VERSION = "0.1.0";

const isPlainObject = (v: unknown): v is Record<string, unknown> => !!v && typeof v === "object" && !Array.isArray(v);

/**
 * The keys this build knows per object, and where nested objects / keyed lists are. Mirrors the
 * Rust structs: only keys *not* listed here are "unknown" (serde `extra`).
 */
export interface KeyShape {
  known: readonly string[];
  /** Nested objects, merged only when both sides have them. */
  objects?: Record<string, KeyShape>;
  /** Lists of rows matched by `key` (same index first). */
  lists?: Record<string, KeyShape>;
  /** Known optional keys that are restored too when the update lacks them (core does this for `minClientVersion`). */
  restore?: readonly string[];
}

const KV_SHAPE: KeyShape = { known: ["key", "value", "enabled", "description"] };
const FORM_FIELD_SHAPE: KeyShape = { known: ["key", "value", "kind", "enabled", "contentType"] };
export const AUTH_SHAPE: KeyShape = {
  known: ["type", "username", "password", "token", "apiKey"],
  objects: { apiKey: { known: ["key", "value", "in"] } },
};
export const WS_SHAPE: KeyShape = {
  known: ["messages", "protocols", "socketIo", "autoReconnect"],
  objects: { socketIo: { known: ["path", "namespace", "version", "auth"] } },
  lists: { messages: { known: ["name", "kind", "body", "event", "ack"] } },
};
export const REQUEST_SHAPE: KeyShape = {
  known: ["id", "kind", "minClientVersion", "collectionId", "folderId", "name", "method", "url", "params", "headers", "auth", "body", "settings", "ws", "sortOrder", "createdAt", "updatedAt"],
  // Like core: a UI that dropped `ws` (or the stamp) doesn't wipe it.
  restore: ["minClientVersion", "ws"],
  objects: {
    auth: AUTH_SHAPE,
    ws: WS_SHAPE,
    settings: { known: ["followRedirects", "timeoutMs", "verifyTls"] },
    body: {
      known: ["type", "text", "form", "formEncoded", "graphql", "binaryPath"],
      objects: { graphql: { known: ["query", "variables", "operationName"] } },
      lists: { form: FORM_FIELD_SHAPE, formEncoded: KV_SHAPE },
    },
  },
  lists: { params: KV_SHAPE, headers: KV_SHAPE },
};

export const COLLECTION_SHAPE: KeyShape = {
  known: ["id", "projectId", "name", "description", "auth", "sortOrder", "createdAt", "updatedAt"],
  objects: { auth: AUTH_SHAPE },
};
export const FOLDER_SHAPE: KeyShape = { known: ["id", "collectionId", "parentFolderId", "name", "sortOrder", "createdAt", "updatedAt"] };
export const VARIABLE_SHAPE: KeyShape = { known: ["key", "value", "enabled", "secret"] };

/**
 * Mirrors core `share::strip_request_extra` and friends: removes the keys this build doesn't know at
 * every level of `obj` (in place) and returns how many objects had some. A share without
 * credentials leaves them out, since they may hold secrets.
 */
export function dropUnknownKeys(obj: unknown, shape: KeyShape): number {
  if (!isPlainObject(obj)) return 0;
  let n = 0;
  const unknown = Object.keys(obj).filter((k) => !shape.known.includes(k));
  if (unknown.length) {
    n++;
    for (const k of unknown) delete obj[k];
  }
  for (const [k, sub] of Object.entries(shape.objects ?? {})) n += dropUnknownKeys(obj[k], sub);
  for (const [k, sub] of Object.entries(shape.lists ?? {})) {
    const list = obj[k];
    if (Array.isArray(list)) for (const row of list) n += dropUnknownKeys(row, sub);
  }
  return n;
}

function keepUnknownRows(update: unknown[], stored: unknown[], shape: KeyShape): unknown[] {
  const used = new Set<number>();
  return update.map((item, i) => {
    if (!isPlainObject(item)) return item;
    const k = item.key;
    let j = isPlainObject(stored[i]) && stored[i].key === k && !used.has(i) ? i : -1;
    if (j < 0) j = stored.findIndex((s, n) => !used.has(n) && isPlainObject(s) && s.key === k);
    if (j < 0) return item;
    used.add(j);
    return keepUnknownKeys(item, stored[j], shape);
  });
}

/**
 * Mirrors core `merge_request_extra` (tech §9.1): keys of `stored` that this build doesn't know
 * (not in `shape.known`) and that the update lacks are kept, at every level; list rows are matched
 * by `key` (same index first). Known keys are never brought back, so removing an optional field
 * (e.g. a header's description) sticks.
 */
export function keepUnknownKeys<T>(update: T, stored: unknown, shape: KeyShape = REQUEST_SHAPE): T {
  if (!isPlainObject(update) || !isPlainObject(stored)) return update;
  const out: Record<string, unknown> = { ...update };
  for (const [k, v] of Object.entries(stored)) {
    if (!(k in out) && (!shape.known.includes(k) || shape.restore?.includes(k))) out[k] = clone(v);
  }
  for (const [k, sub] of Object.entries(shape.objects ?? {})) {
    if (isPlainObject(out[k]) && isPlainObject(stored[k])) out[k] = keepUnknownKeys(out[k], stored[k], sub);
  }
  for (const [k, sub] of Object.entries(shape.lists ?? {})) {
    const u = out[k];
    const st = stored[k];
    if (Array.isArray(u) && Array.isArray(st)) out[k] = keepUnknownRows(u, st, sub);
  }
  return out as T;
}

/** Structural equality of JSON values (key order ignored). */
function jsonEqual(a: unknown, b: unknown): boolean {
  if (a === b) return true;
  if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((x, i) => jsonEqual(x, b[i]));
  if (isPlainObject(a) && isPlainObject(b)) {
    const ka = Object.keys(a).filter((k) => a[k] !== undefined);
    const kb = Object.keys(b).filter((k) => b[k] !== undefined);
    return ka.length === kb.length && ka.every((k) => jsonEqual(a[k], b[k]));
  }
  return false;
}

/** Message of core `storage::read_only_newer` for container actions (AC-FWD-3/4). */
function readOnlyNewerMessage(r: { name: string }, verb: "move" | "copy" | "delete", fallback = "a request"): string {
  const name = r.name.trim() ? `“${r.name}”` : fallback;
  const advice = verb === "delete" ? `Delete ${name} on its own first, or update ReqHarbor.` : `Update ReqHarbor to ${verb} it.`;
  return `This would change ${name}, which was made with a newer version of ReqHarbor and is read-only here. ${advice}`;
}

/**
 * Mirrors core `renumber_around`: sort orders for `order` that keep the values of `fixed` items;
 * the others are numbered into the gaps. Null when a gap is too small.
 */
export function renumberAround<T extends { sortOrder: number }>(order: T[], fixed: Set<T>): Map<T, number> | null {
  const out = new Map<T, number>();
  if (!fixed.size) {
    order.forEach((x, i) => out.set(x, i));
    return out;
  }
  let pending: T[] = [];
  let prev: number | null = null;
  for (const x of order) {
    if (!fixed.has(x)) {
      pending.push(x);
      continue;
    }
    const v = x.sortOrder;
    const k = pending.length;
    let start: number;
    if (prev === null) start = v - k;
    else {
      if (k > 0 && v - prev - 1 < k) return null;
      start = prev + 1;
    }
    pending.forEach((p, i) => out.set(p, start + i));
    pending = [];
    out.set(x, v);
    prev = v;
  }
  const start = prev === null ? 0 : prev + 1;
  pending.forEach((p, i) => out.set(p, start + i));
  return out;
}

/** Requests as written by a newer ReqHarbor (`?newer=1`). */
export function newerRequests(collectionId: string, ts: number): Request[] {
  const base = (): Omit<Request, "id" | "name" | "method" | "url" | "body" | "sortOrder"> => ({
    collectionId,
    folderId: null,
    params: [],
    headers: [],
    auth: { type: "none" },
    settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
    createdAt: ts,
    updatedAt: ts,
  });
  return [
    {
      ...base(),
      id: ulid(ts),
      kind: "grpc",
      name: "Say hello",
      method: "POST",
      url: "grpc://greeter.example.com:443",
      body: { type: "none" },
      sortOrder: 90,
      grpc: { service: "helloworld.Greeter", method: "SayHello", message: '{ "name": "Ada" }' },
    } as Request,
    {
      ...base(),
      id: ulid(ts + 1),
      name: "Upload protobuf",
      method: "POST",
      url: "https://api.example.com/v1/upload",
      body: { type: "protobuf", schemaRef: "upload.proto#Upload" } as Request["body"],
      sortOrder: 91,
    },
  ];
}

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
  /** Seed requests from a newer ReqHarbor (AC-FWD-3). */
  newer?: boolean;
  /** The `accounts` flag (default on). */
  accountsEnabled?: boolean;
  /** Sign-in switches (see ./account.ts). */
  accounts?: MockAccountOptions;
  /** Personal sync switches (see ./spaces.ts). */
  spaces?: MockSpacesOptions;
  /** Token sync connected at launch (`?mockLegacy=1`). */
  legacy?: boolean;
  /** The sync after a legacy merge fails (`?mockMergeSyncFails=1`). */
  mergeSyncFails?: boolean;
  /** Teams switches (see ./teams.ts). */
  teams?: { enabled?: boolean; role?: TeamRole; team?: boolean };
  /** `?mockMove=offline|quota|slow`: how a project move ends ("slow" shows progress for ~2 s). */
  move?: "offline" | "quota" | "slow";
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
  /** GraphQL schema cache by resolved URL (core `graphql::SchemaCache`, AC-GQL-4). */
  graphqlSchemas = new Map<string, GraphqlSchema>();
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
    unsupportedEntities: [],
    warning: null,
    accountMode: false,
    spaces: [],
    legacy: null,
    firstSyncPending: false,
    pendingMoves: [],
  };
  syncToken: string | null = null;
  /** WebSocket sessions (AC-WS-*): the echo / Socket.IO server and what each history entry needs. */
  ws: MockWs;
  private wsMeta = new Map<string, { request: Request; projectId: string | null; historyId: string; ts: number; displayUrl: string; method: "WS" | "SIO"; sio: boolean }>();
  accounts: MockAccounts;
  spaces: MockSpaces;
  teams: MockTeams;
  /** Requests saved on this device (no "Edited by" for them). */
  private editedHere = new Set<string>();
  /** Requests created after this were made on this device. */
  private bootAt = 0;
  private clock = 0;
  private opts: MockOptions;

  constructor(opts: MockOptions = {}) {
    this.opts = opts;
    this.ws = new MockWs({
      latency: () => this.opts.latency ?? 1,
      finished: (id, summary, error) => {
        const m = this.wsMeta.get(id);
        if (!m) return;
        this.wsMeta.delete(id);
        const entry: HistoryEntry = {
          id: m.historyId,
          timestamp: m.ts,
          sourceRequestId: m.request.collectionId ? m.request.id : null,
          request: m.request,
          resolvedUrl: m.displayUrl,
          method: m.method,
          ws: summary,
        };
        if (error) entry.error = error;
        this.addHistory(entry, null);
      },
    });
    this.accounts = new MockAccounts(opts.accounts ?? {}, {
      emit: (e, p) => this.emit(e, p),
      delay: (ms) => this.delay(ms),
      now: () => this.now(),
      settings: () => this.settings,
      onSignedIn: (a) => {
        const legacy = this.sync.connected && this.sync.workerUrl ? { workerUrl: this.sync.workerUrl, remoteWorkspaceId: this.sync.remoteWorkspaceId ?? "LEGACYWS" } : null;
        const fresh = this.spaces.signIn(a, legacy);
        this.emitStatus();
        if (fresh) this.emit("sync://notice", { kind: "success", message: this.spaces.freshNotice() });
      },
      onSignedOut: () => {
        this.spaces.signOut();
        this.emitStatus();
      },
    });
    this.spaces = new MockSpaces(
      {
        projects: () => this.projects,
        counts: (id) => ({ collections: this.collections.filter((c) => c.projectId === id).length, requests: this.requestsOfProject(id).length }),
        forget: (id) => this.forgetProject(id),
        rename: (id, name) => {
          const p = this.projects.find((x) => x.id === id);
          if (p) p.name = name;
        },
        now: () => this.now(),
      },
      opts.spaces,
    );
    this.teams = new MockTeams(
      {
        me: () => {
          const a = this.accounts.status.account;
          return a ? { userId: a.userId, name: a.name, email: a.email } : null;
        },
        serverUrl: () => this.accounts.status.account?.serverUrl ?? null,
        now: () => this.now(),
        spaces: this.spaces,
        addProject: (name, spaceId) => {
          const ts = this.now();
          const p: Project = { id: ulid(ts), workspaceId: this.workspace.id, name, sortOrder: this.projects.length, activeEnvironmentId: null, createdAt: ts, updatedAt: ts };
          this.projects.push(p);
          this.spaces.bind(p.id, spaceId);
          const col: Collection = { id: ulid(this.now()), projectId: p.id, name: "API", description: "", auth: { type: "none" }, sortOrder: 0, createdAt: ts, updatedAt: ts };
          this.collections.push(col);
          this.requests.push({ id: ulid(this.now()), collectionId: col.id, folderId: null, name: "List accounts", method: "GET", url: "https://api.globex.dev/v1/accounts", params: [], headers: [], auth: { type: "inherit" }, body: { type: "none" }, settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true }, sortOrder: 0, createdAt: ts, updatedAt: ts });
        },
        unsynced: () => Math.min(this.sync.pendingChanges, 3),
      },
      { enabled: opts.teams?.enabled ?? true, role: opts.teams?.role, team: opts.teams?.team },
    );
    this.accounts.status.enabled = opts.accountsEnabled ?? true;
    const now = this.now();
    const s = seed(now, opts.empty);
    this.workspace = s.workspace;
    this.projects = s.projects;
    this.collections = s.collections;
    this.folders = s.folders;
    this.requests = s.requests;
    this.environments = s.environments;
    this.settings = s.settings;
    this.bootAt = this.now();
    const first = this.collections.find((c) => c.projectId === this.settings.activeProjectId);
    if (opts.newer && first) this.requests.push(...newerRequests(first.id, now));
    for (const h of s.historySends) {
      const ts = now - h.ageMs;
      this.recordCanned(h.request, h.projectId, ts);
    }
    if (opts.legacy) this.connect("https://sync.example.dev", `rh_${ulid()}`);
    // `?mockAccount=signedIn` (and friends): this device was set up earlier, so it is already in
    // account mode with every project in Personal (or migrated token sync with `mockLegacy`).
    const account = this.accounts.status.account;
    if (account) {
      const legacy = this.sync.connected && this.sync.workerUrl ? { workerUrl: this.sync.workerUrl, remoteWorkspaceId: this.sync.remoteWorkspaceId ?? "LEGACYWS" } : null;
      this.spaces.signIn(account, legacy, true);
      if (opts.spaces?.foreign) this.spaces.makeForeign();
    }
  }

  /** The status the UI sees: the token-sync part plus the per-space part (account mode). */
  syncStatus(): SyncStatus {
    return { ...this.spaces.apply(this.sync), pendingMoves: clone(this.pendingMoves) };
  }

  /** Moves that didn't finish (`?mockMove=offline|quota`): resume or cancel them. */
  pendingMoves: import("../types").PendingMove[] = [];

  emitStatus(): void {
    this.emit("sync://status", this.syncStatus());
  }

  /** Drop a project and everything in it (no tombstones). */
  private forgetProject(projectId: string): void {
    const cols = new Set(this.collections.filter((c) => c.projectId === projectId).map((c) => c.id));
    this.requests = this.requests.filter((r) => !cols.has(r.collectionId));
    this.folders = this.folders.filter((f) => !cols.has(f.collectionId));
    this.collections = this.collections.filter((c) => !cols.has(c.id));
    this.environments = this.environments.filter((e) => e.projectId !== projectId);
    this.projects = this.projects.filter((p) => p.id !== projectId);
    this.spaces.onDelete(projectId);
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
  /** Store guard (AC-FWD-3/4): mirrors core `StoreError::ReadOnlyNewer`. */
  private ensureEditable(r: Request | undefined): void {
    if (r && isReadOnlyNewer(r, MOCK_APP_VERSION)) {
      fail("read_only_newer", `${r.name.trim() ? `“${r.name}”` : "This request"} was made with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it.`);
    }
  }
  /** Container guard (AC-FWD-3): mirrors core moves/copies/cascading deletes refusing to touch read-only requests. */
  private refuseReadOnlyIn(requests: Request[], verb: "move" | "copy" | "delete"): void {
    const r = requests.find((x) => isReadOnlyNewer(x, MOCK_APP_VERSION));
    if (r) fail("read_only_newer", readOnlyNewerMessage(r, verb));
  }
  /** Mirrors core `ensure_entity_editable` (AC-FWD-4): a project, collection, folder or environment with a newer `minClientVersion`. */
  private ensureContainer(e: { name: string; minClientVersion?: unknown }, fallback: string): void {
    if (isContainerReadOnly(e, MOCK_APP_VERSION)) {
      fail("read_only_newer", `${e.name.trim() ? `“${e.name}”` : fallback} was made with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it.`);
    }
  }
  /** Mirrors core `first_read_only`: cascades and copies never touch read-only containers. */
  private refuseReadOnlyContainers(items: { name: string; minClientVersion?: unknown }[], verb: "move" | "copy" | "delete", fallback: string): void {
    const x = items.find((i) => isContainerReadOnly(i, MOCK_APP_VERSION));
    if (x) fail("read_only_newer", readOnlyNewerMessage(x, verb, fallback));
  }
  /** Sort orders for `list` (new sequence) keeping read-only items in place; fails like core when there's no room. */
  private renumberContainers<T extends { sortOrder: number; name: string; minClientVersion?: unknown }>(list: T[], fallback: string): void {
    const fixed = new Set(list.filter((x) => isContainerReadOnly(x, MOCK_APP_VERSION)));
    const numbers = renumberAround(list, fixed);
    if (!numbers) return fail("read_only_newer", readOnlyNewerMessage([...fixed][0], "move", fallback));
    for (const [x, n] of numbers) x.sortOrder = n;
  }
  private requestsUnderFolder(folderId: string): Request[] {
    const ids = new Set([folderId, ...this.descendantFolders(folderId).map((d) => d.id)]);
    return this.requests.filter((r) => r.folderId !== null && ids.has(r.folderId));
  }
  private foldersOfProject(projectId: string): Folder[] {
    const cols = new Set(this.collections.filter((c) => c.projectId === projectId).map((c) => c.id));
    return this.folders.filter((f) => cols.has(f.collectionId));
  }
  private requestsOfProject(projectId: string): Request[] {
    const cols = new Set(this.collections.filter((c) => c.projectId === projectId).map((c) => c.id));
    return this.requests.filter((r) => cols.has(r.collectionId));
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
    if (this.sync.connected || this.spaces.syncable()) {
      this.sync.pendingChanges++;
      this.refreshSkipped();
      this.emitStatus();
    }
  }
  private refreshSkipped(): void {
    const skipped: SkippedEntity[] = this.requests
      .filter((r) => enc.encode(JSON.stringify(r)).length > SYNC_ENTITY_MAX)
      .map((r) => ({ type: "request", id: r.id, name: r.name, reason: "too_large" }));
    this.sync.skippedEntities = skipped;
    this.sync.warning = skippedWarning(skipped);
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
  /** `dynamic` is the per-send memo: share one across every resolve of a send (AC-ENV-8). */
  private resolver(projectId: string | null | undefined, maskSecrets: boolean, dynamic = dynamicMemo()) {
    const map = this.lookup(projectId);
    const unresolved = new Set<string>();
    const resolve = (text: string, depth = 0, stack: string[] = []): string =>
      text.replace(/\{\{\s*(\$?[A-Za-z0-9_.-]+)\s*\}\}/g, (whole, name: string) => {
        if (name.startsWith("$")) {
          const v = dynamic(name);
          if (v === undefined) unresolved.add(name);
          return v ?? whole;
        }
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

  private resolveRequest(r: Request, projectId: string | null | undefined, maskSecrets: boolean, dynamic = dynamicMemo()) {
    const { resolve, unresolved } = this.resolver(projectId, maskSecrets, dynamic);
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
    if (body.graphql) body.graphql = { ...body.graphql, query: resolve(body.graphql.query), variables: resolve(body.graphql.variables) };
    if (body.binaryPath) body.binaryPath = resolve(body.binaryPath);
    return { url, headers, auth, body, unresolved };
  }

  private prepare(r: Request, projectId: string | null | undefined, dynamicOverride?: (name: string) => string | undefined): { prepared: PreparedRequest; displayUrl: string; unresolved: string[] } {
    if (!METHOD_RE.test(r.method)) fail("validation", `Invalid method "${r.method}"`, { kind: "invalidMethod", message: "Use A–Z, 0–9, _ or -, starting with a letter" });
    if (!r.url.trim()) fail("validation", "Enter a URL", { kind: "emptyUrl" });
    if (r.auth.type === "inherit" && !isKnownAuth(this.inheritedAuth(r))) {
      fail("validation", "The collection's auth needs a newer version of ReqHarbor", { kind: "inheritedAuthNeedsNewerVersion" });
    }
    if (r.body.type === "graphql" && r.body.graphql?.variables.trim()) {
      try {
        JSON.parse(r.body.graphql.variables);
      } catch (e) {
        fail("validation", "Fix GraphQL variables", { kind: "invalidGraphqlVariables", message: (e as Error).message });
      }
    }
    // One memo: the URL sent and the URL kept in history agree on {{$…}} values.
    const dynamic = dynamicOverride ?? dynamicMemo();
    const res = this.resolveRequest(r, projectId, false, dynamic);
    const display = this.resolveRequest(r, projectId, true, dynamic);
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
        bodyText = JSON.stringify({
          query: b.graphql?.query ?? "",
          ...(b.graphql?.operationName?.trim() ? { operationName: b.graphql.operationName.trim() } : {}),
          variables: b.graphql?.variables.trim() ? JSON.parse(b.graphql.variables) : {},
        });
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

  /** Canned answer, or the fake GraphQL endpoint for `…/graphql` URLs (AC-GQL-*). */
  private async fakeExchange(prepared: PreparedRequest): Promise<FakeResponse> {
    let path = "";
    try {
      path = new URL(prepared.url).pathname;
    } catch {
      /* cannedResponse reports it */
    }
    if (prepared.method === "POST" && /\/graphql\/?$/.test(path)) {
      cannedResponse({ ...prepared, url: prepared.url.replace(/\/graphql\/?(?=$|[?#])/, "/health") }); // network failures (dns, refused, tls) first
      const g = await import("./graphql");
      return g.graphqlResponse(prepared);
    }
    return cannedResponse(prepared);
  }

  private graphqlContext(request: Request, projectId: string | null | undefined) {
    if (isReadOnlyNewer(request, MOCK_APP_VERSION) || this.requests.some((r) => r.id === request.id && isReadOnlyNewer(r, MOCK_APP_VERSION))) {
      fail("read_only_newer", "This request needs a newer version of ReqHarbor");
    }
    if (request.kind === "ws") fail("validation", "This is a WebSocket request: connect to it instead", { kind: "notHttp" });
    return projectId ?? (request.collectionId ? this.collections.find((c) => c.id === request.collectionId)?.projectId : null) ?? this.settings.activeProjectId;
  }

  private async graphqlSchemaUrl(request: Request, projectId: string | null | undefined): Promise<{ url: string; pid: string | null | undefined; g: typeof import("./graphql") }> {
    const g = await import("./graphql");
    const pid = this.graphqlContext(request, projectId);
    // Like core `schema_url`: `{{$dynamic}}` stays literal, secrets masked.
    const { displayUrl } = this.prepare(g.introspectionRequest(request), pid, (name) => `{{${name}}}`);
    return { url: g.schemaKey(displayUrl), pid, g };
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
      ws: h.ws ?? null,
    };
  }

  /** Where a share of this entity goes: its project's space (account mode) or token sync. */
  private shareBase(kind: "collection" | "request", entityId: string): { url: string; spaceId: string | null } {
    if (!this.spaces.accountMode) return { url: this.sync.workerUrl ?? "", spaceId: null };
    const pid = kind === "collection" ? this.collection(entityId).projectId : this.collection(this.request(entityId).collectionId).projectId;
    const loc = this.spaces.location(pid, this.sync.connected);
    if (!loc.spaceId) return fail("sync_not_connected", "Sharing needs this project to sync. Change its sync location to Personal first.");
    if (loc.spaceId === "legacy") return { url: this.sync.workerUrl ?? "", spaceId: "legacy" };
    // Only an active binding of the signed-in account gets this session's tokens (06 §9).
    if (loc.state !== "active") return fail("sync_not_connected", "This project's sync is paused, so it can't be shared. Resolve it in Settings › Sync first.");
    return { url: this.accounts.status.account?.serverUrl ?? "", spaceId: loc.spaceId };
  }

  private requireSync(): void {
    if (!this.sync.connected && !this.spaces.syncable()) fail("sync_not_connected", "Sync is not connected. Set up sync in Settings → Sync.");
  }

  private snapshotFor(kind: ShareKind, entityId: string, includeCredentials: boolean, environmentId: string | null | undefined) {
    let credentialFields = 0;
    const replace = !includeCredentials;
    // Mirrors core `share_needs_credentials`: without credentials, an item from a newer ReqHarbor
    // can't be shared, since this build can't tell which of its fields are secret.
    const refuse = (name: string) =>
      fail(
        "read_only_newer",
        `${name.trim() ? `“${name}”` : "An item"} was made with a newer version of ReqHarbor, so this version can't tell which of its fields are credentials. Turn on Include credentials to share it as it is, or update ReqHarbor.`,
      );
    /** Unknown keys: dropped (and counted) without credentials, counted with them. */
    const unknownKeys = (obj: unknown, shape: KeyShape): number => (replace ? dropUnknownKeys(obj, shape) : dropUnknownKeys(clone(obj), shape));
    const scrub = (r: Request): Request => {
      const c = clone(r);
      for (const h of c.headers) {
        const k = h.key.trim().toLowerCase();
        if ((k === "authorization" || k === "proxy-authorization" || k === "cookie") && h.value) {
          credentialFields++;
          if (replace) h.value = `{{${k === "authorization" ? "authorization" : k === "cookie" ? "cookie" : "proxyAuthorization"}}}`;
        }
      }
      // Mirrors core sanitize_request: the Socket.IO CONNECT payload carries credentials.
      const sio = c.ws?.socketIo;
      if (sio?.auth?.trim()) {
        credentialFields++;
        if (replace) sio.auth = "{{socketIoAuth}}";
      }
      // The auth block is handled by scrubAuth (unknown schemes as a whole).
      const { auth, ...rest } = c;
      credentialFields += unknownKeys(rest, REQUEST_SHAPE);
      return { ...rest, auth: scrubAuth(auth) } as Request;
    };
    const scrubAuth = (a: Auth): Auth => {
      const c = clone(a);
      if (!isKnownAuth({ type: c.type })) {
        // Mirrors core sanitize_auth for AuthType::Other: every field may be a secret.
        const { type, ...fields } = c as Auth & Record<string, unknown>;
        if (Object.values(fields).some((v) => v !== undefined && v !== null && v !== "")) {
          credentialFields++;
          if (replace) return { type } as Auth;
        }
        return c;
      }
      const has = (v: string | undefined) => !!v;
      if (c.type === "basic") {
        if (has(c.username)) credentialFields++;
        if (has(c.password)) credentialFields++;
        if (replace) {
          if (has(c.username)) c.username = "{{username}}";
          if (has(c.password)) c.password = "{{password}}";
        }
      } else if (c.type === "bearer") {
        if (has(c.token)) {
          credentialFields++;
          if (replace) c.token = "{{token}}";
        }
      } else if (c.type === "apikey" && c.apiKey && has(c.apiKey.value)) {
        credentialFields++;
        if (replace) c.apiKey.value = "{{apiKey}}";
      }
      credentialFields += unknownKeys(c, AUTH_SHAPE);
      return c;
    };
    const snapshot: ShareSnapshot = { schemaVersion: 1, environment: null };
    let name = "";
    let requestCount = 0;
    let folderCount = 0;
    if (kind === "collection") {
      const col = this.collection(entityId);
      const folders = this.folders.filter((f) => f.collectionId === col.id);
      const raw = this.requests.filter((r) => r.collectionId === col.id);
      if (replace) {
        if (isContainerReadOnly(col, MOCK_APP_VERSION)) refuse(col.name);
        const f = folders.find((x) => isContainerReadOnly(x, MOCK_APP_VERSION));
        if (f) refuse(f.name);
        const r = raw.find((x) => isReadOnlyNewer(x, MOCK_APP_VERSION));
        if (r) refuse(r.name);
      }
      const c = { ...clone(col), auth: scrubAuth(col.auth) };
      credentialFields += unknownKeys(c, { ...COLLECTION_SHAPE, objects: {} });
      const fs = clone(folders);
      for (const f of fs) credentialFields += unknownKeys(f, FOLDER_SHAPE);
      const requests = raw.map(scrub);
      snapshot.collection = { ...c, folders: fs, requests };
      name = col.name;
      requestCount = requests.length;
      folderCount = folders.length;
    } else {
      const r = this.request(entityId);
      if (replace && isReadOnlyNewer(r, MOCK_APP_VERSION)) refuse(r.name);
      const inlined = r.auth.type === "inherit" ? { ...r, auth: this.inheritedAuth(r) ?? { type: "none" as const } } : r;
      snapshot.request = scrub(inlined);
      name = r.name;
      requestCount = 1;
    }
    const credentialsReplaced = replace ? credentialFields : 0;
    let secretVariablesRemoved = 0;
    if (environmentId) {
      const env = this.env(environmentId);
      secretVariablesRemoved = env.variables.filter((v) => v.secret).length;
      const vars = clone(env.variables.filter((v) => !v.secret));
      let varExtras = 0;
      for (const v of vars) varExtras += unknownKeys(v, VARIABLE_SHAPE);
      credentialFields += varExtras;
      snapshot.environment = { name: env.name, variables: vars };
      return {
        snapshot,
        preview: { name, requestCount, folderCount, secretVariablesRemoved, credentialsReplaced: credentialsReplaced + (replace ? varExtras : 0), credentialFields },
      };
    }
    return {
      snapshot,
      preview: { name, requestCount, folderCount, secretVariablesRemoved, credentialsReplaced, credentialFields },
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

  /** Only the first failure of a move fails (the user's Retry then succeeds). */
  private moveRecovered = new Set<string>();

  /** Mirrors teams_rt `parse_link`: an invite token goes to plain http only after "Continue anyway". */
  private inviteInsecureGate(url: string, confirmed: boolean): void {
    const { origin } = MockTeams.parse(url);
    const known = (this.settings as Settings & { knownServers?: { url: string; insecureAck?: boolean }[] }).knownServers;
    const acked = !!known?.some((k) => k.url === origin && k.insecureAck);
    if (isInsecureUrl(origin) && !confirmed && !acked) fail("insecure_unconfirmed", "This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.");
  }

  private emptyReport(): SyncReport {
    return { pushed: 0, pulled: 0, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 0 };
  }

  /** A whole project copied with new ids, bound to `spaceId` (null: this device only). */
  private copyProject(projectId: string, name: string, spaceId: string | null, sortOrder: number): string {
    const src = this.project(projectId);
    const ts = this.now();
    const p: Project = { ...clone(src), id: ulid(ts), name, sortOrder, createdAt: ts, updatedAt: ts };
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
    this.spaces.bind(p.id, spaceId);
    return p.id;
  }

  /** Copy a collection, folder or request into another project (new ids); returns the copy's id. */
  private copyNode(kind: "collection" | "folder" | "request", id: string, targetProjectId: string, targetCollectionId: string | null, targetFolderId: string | null): string {
    const sort = Math.max(-1, ...this.collections.filter((x) => x.projectId === targetProjectId).map((x) => x.sortOrder)) + 1;
    if (kind === "collection") {
      const c = this.collection(id);
      return this.copyCollectionTree(c, targetProjectId, this.folders.filter((f) => f.collectionId === c.id), this.requests.filter((r) => r.collectionId === c.id), c.name, sort).id;
    }
    const sourceCollection = kind === "folder" ? this.folder(id).collectionId : this.request(id).collectionId;
    let cid = targetCollectionId;
    if (cid) {
      if (this.collection(cid).projectId !== targetProjectId) fail("not_found", `Collection ${cid} not found`);
    } else {
      const src = this.collection(sourceCollection);
      const ts = this.now();
      const col: Collection = { ...clone(src), id: ulid(ts), projectId: targetProjectId, sortOrder: sort, createdAt: ts, updatedAt: ts };
      this.collections.push(col);
      cid = col.id;
    }
    const ts = this.now();
    if (kind === "request") {
      const r = this.request(id);
      const nr: Request = { ...clone(r), id: ulid(ts), collectionId: cid, folderId: targetFolderId, sortOrder: this.requests.filter((x) => x.collectionId === cid).length, createdAt: ts, updatedAt: ts };
      this.requests.push(nr);
      return nr.id;
    }
    const top = this.folder(id);
    const subtree = [top, ...this.descendantFolders(id)];
    const idMap = new Map(subtree.map((f) => [f.id, ulid(this.now())] as const));
    for (const f of subtree) {
      this.folders.push({ ...clone(f), id: idMap.get(f.id)!, collectionId: cid, parentFolderId: f.id === id ? targetFolderId : (idMap.get(f.parentFolderId ?? "") ?? null), createdAt: ts, updatedAt: ts });
    }
    for (const r of this.requestsUnderFolder(id)) {
      this.requests.push({ ...clone(r), id: ulid(this.now()), collectionId: cid, folderId: idMap.get(r.folderId ?? "") ?? null, createdAt: ts, updatedAt: ts });
    }
    return idMap.get(id)!;
  }

  /** Projects of a space copied to Personal as "X (my changes)" (role-changed banner, edge case 3). */
  private saveDirtyAsPersonal(spaceId: string): string[] {
    const personal = this.spaces.personal();
    const ids = [...this.spaces.projectSpaces].filter(([, s]) => s === spaceId).map(([p]) => p);
    return ids.map((pid) => this.copyProject(pid, `${this.project(pid).name} (my changes)`, personal?.spaceId ?? null, this.projects.length));
  }

  /** Mirrors core `plain_credentials`. */
  private credentialRows(r: Request): CredentialRow[] {
    const plain = (v: string | null | undefined) => {
      const rest = (v ?? "").replace(/\{\{[^}]*\}\}/g, "").trim().replace(/^(Bearer|Basic|Token)\b/i, "").trim();
      return rest.length > 0;
    };
    const row = (field: string, tab: CredentialRow["tab"]): CredentialRow => ({ requestId: r.id, requestName: r.name, field, tab });
    const out: CredentialRow[] = [];
    for (const h of r.headers) {
      const k = h.key.trim().toLowerCase();
      if (["authorization", "proxy-authorization", "cookie"].includes(k) && plain(h.value)) out.push(row(k === "cookie" ? "Cookie header" : k === "proxy-authorization" ? "Proxy-Authorization header" : "Authorization header", "headers"));
    }
    const a = r.auth;
    if (a.type === "basic" && plain(a.password)) out.push(row("Basic auth password", "auth"));
    if (a.type === "bearer" && plain(a.token)) out.push(row("Bearer token", "auth"));
    if (a.type === "apikey" && plain(a.apiKey?.value)) out.push(row("API key", "auth"));
    if (plain(r.ws?.socketIo?.auth)) out.push(row("Socket.IO auth", "ws"));
    return out;
  }

  /** Mirrors core `collection_credentials`. */
  private collectionCredentialRows(c: Collection): CredentialRow[] {
    const plain = (v: string | null | undefined) => (v ?? "").replace(/\{\{[^}]*\}\}/g, "").trim().replace(/^(Bearer|Basic|Token)\b/i, "").trim().length > 0;
    const row = (field: string): CredentialRow => ({ requestId: c.id, requestName: c.name, field: `Collection auth: ${field}`, tab: "collection" });
    const a = c.auth;
    if (a.type === "basic" && plain(a.password)) return [row("Basic auth password")];
    if (a.type === "bearer" && plain(a.token)) return [row("Bearer token")];
    if (a.type === "apikey" && plain(a.apiKey?.value)) return [row("API key")];
    return [];
  }

  private filePaths(r: Request): number {
    return (r.body.binaryPath?.trim() ? 1 : 0) + (r.body.form ?? []).filter((f) => f.kind === "file" && f.value.trim()).length;
  }

  /** Mirrors core `move_check` + `move_preview`. */
  private movePreview(projectId: string, spaceId: string | null): TransferPreview {
    const p = this.project(projectId);
    if (!this.spaces.accountMode) fail("invalid", "Sign in to move projects between spaces.");
    const from = this.spaces.projectSpaces.get(projectId) ?? null;
    if (from === spaceId) fail("invalid", `“${p.name}” is already there.`);
    if (from === LEGACY) fail("invalid", "This project syncs with a token. Merge it into your personal space first (Settings › Sync).");
    const src = from ? this.spaces.binding(from) : undefined;
    if (src?.kind === "team" && !["admin", "owner"].includes(src.role ?? "")) fail("read_only_role", `Only admins can move a project out of ${src.name}. Use Copy to project… instead.`);
    this.spaces.spaceGuard(spaceId);
    const target = spaceId ? this.spaces.binding(spaceId) : undefined;
    const envs = this.environments.filter((e) => e.projectId === projectId);
    const reqs = this.requestsOfProject(projectId);
    return {
      name: p.name,
      targetName: target ? target.name : "This device only",
      targetKind: target ? target.kind : "local",
      memberCount: target?.memberCount ?? null,
      items: 1 + this.collections.filter((c) => c.projectId === projectId).length + this.foldersOfProject(projectId).length + reqs.length + envs.length,
      secretVariables: envs.flatMap((e) => e.variables).filter((v) => v.secret).length,
      filePaths: reqs.reduce((n, r) => n + this.filePaths(r), 0),
      credentials: [...this.collections.filter((c) => c.projectId === projectId).flatMap((c) => this.collectionCredentialRows(c)), ...reqs.flatMap((r) => this.credentialRows(r))],
      unresolvedVariables: [],
      sharesStay: !!spaceId,
    };
  }

  /** Mirrors core `copy_preview`. */
  private copyPreview(kind: "collection" | "folder" | "request", id: string, targetProjectId: string): TransferPreview {
    const target = this.project(targetProjectId);
    let name: string;
    let reqs: Request[];
    let items: number;
    if (kind === "collection") {
      const c = this.collection(id);
      name = c.name;
      reqs = this.requests.filter((r) => r.collectionId === id);
      items = 1 + this.folders.filter((f) => f.collectionId === id).length + reqs.length;
    } else if (kind === "folder") {
      name = this.folder(id).name;
      reqs = this.requestsUnderFolder(id);
      items = 1 + this.descendantFolders(id).length + reqs.length;
    } else {
      const r = this.request(id);
      name = r.name;
      reqs = [r];
      items = 1;
    }
    const defined = new Set(this.environments.filter((e) => e.projectId === targetProjectId).flatMap((e) => e.variables.map((v) => v.key.trim())));
    const used = new Set<string>();
    for (const r of reqs) {
      const text = [r.url, ...r.headers.flatMap((h) => [h.key, h.value]), ...r.params.flatMap((h) => [h.key, h.value]), r.body.text ?? "", r.auth.token ?? "", r.auth.password ?? "", r.auth.username ?? ""].join("\n");
      for (const m of text.matchAll(/\{\{\s*([^{}$\s][^{}]*?)\s*\}\}/g)) if (!defined.has(m[1])) used.add(m[1]);
    }
    const space = this.spaces.projectSpaces.get(targetProjectId);
    const b = space ? this.spaces.binding(space) : undefined;
    return {
      name,
      targetName: target.name,
      targetKind: this.spaces.kindOf(targetProjectId),
      memberCount: b?.memberCount ?? null,
      items,
      secretVariables: 0,
      filePaths: reqs.reduce((n, r) => n + this.filePaths(r), 0),
      credentials: reqs.flatMap((r) => this.credentialRows(r)),
      unresolvedVariables: [...used].sort(),
      sharesStay: false,
    };
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
        sync: this.syncStatus(),
        platform,
        dataDir: "/mock/reqharbor",
        appVersion: MOCK_APP_VERSION,
        accounts: this.accounts.status.enabled,
      });
    },
    get_settings: () => clone(this.settings),
    save_settings: ({ settings }) => {
      // Real backend accepts fractional u32 fields and rounds them (QA-R3-003).
      for (const k of ["sidebarWidth", "windowWidth", "windowHeight"] as const) {
        const v = settings[k];
        if (typeof v === "number") (settings as Record<string, unknown>)[k] = Math.max(0, Math.round(v));
      }
      this.settings = { ...defaultSettings(), ...this.settings, ...clone(settings) };
      return clone(this.settings);
    },

    list_projects: () => this.sortedProjects(),
    get_project_tree: ({ projectId }) => this.tree(projectId),
    create_project: ({ name, location }) => {
      const ts = this.now();
      const p: Project = { id: ulid(ts), workspaceId: this.workspace.id, name: this.name(name), sortOrder: this.projects.length, activeEnvironmentId: null, createdAt: ts, updatedAt: ts };
      this.spaces.onCreate(p.id, location);
      this.projects.push(p);
      this.markChanged();
      return this.tree(p.id);
    },
    rename_project: ({ projectId, name }) => {
      const p = this.project(projectId);
      this.ensureContainer(p, "This project");
      this.spaces.guard(projectId);
      p.name = this.name(name);
      return clone(this.touch(p));
    },
    duplicate_project: ({ projectId }) => {
      const src = this.project(projectId);
      this.refuseReadOnlyContainers([src], "copy", "a project");
      this.refuseReadOnlyContainers(this.collections.filter((c) => c.projectId === projectId), "copy", "a collection");
      this.refuseReadOnlyContainers(this.foldersOfProject(projectId), "copy", "a folder");
      this.refuseReadOnlyIn(this.requestsOfProject(projectId), "copy");
      this.refuseReadOnlyContainers(this.environments.filter((e) => e.projectId === projectId), "copy", "an environment");
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
      this.spaces.onDuplicate(src.id, p.id);
      for (const c of this.collections.filter((x) => x.projectId === src.id)) {
        this.copyCollectionTree(c, p.id, this.folders.filter((f) => f.collectionId === c.id), this.requests.filter((r) => r.collectionId === c.id), c.name, c.sortOrder);
      }
      this.markChanged();
      return this.tree(p.id);
    },
    delete_project: ({ projectId }) => {
      this.project(projectId);
      if (this.projects.length <= 1) fail("invalid", "Cannot delete the last project");
      this.spaces.deleteGuard(projectId);
      this.refuseReadOnlyContainers(this.collections.filter((c) => c.projectId === projectId), "delete", "a collection");
      this.refuseReadOnlyContainers(this.foldersOfProject(projectId), "delete", "a folder");
      this.refuseReadOnlyIn(this.requestsOfProject(projectId), "delete");
      this.refuseReadOnlyContainers(this.environments.filter((e) => e.projectId === projectId), "delete", "an environment");
      const cols = new Set(this.collections.filter((c) => c.projectId === projectId).map((c) => c.id));
      this.requests = this.requests.filter((r) => !cols.has(r.collectionId));
      this.folders = this.folders.filter((f) => !cols.has(f.collectionId));
      this.collections = this.collections.filter((c) => !cols.has(c.id));
      this.environments = this.environments.filter((e) => e.projectId !== projectId);
      this.projects = this.projects.filter((p) => p.id !== projectId);
      this.projects.sort((a, b) => a.sortOrder - b.sortOrder).forEach((p, i) => (p.sortOrder = i));
      this.spaces.onDelete(projectId);
      this.markChanged();
      return this.sortedProjects();
    },
    move_project: ({ projectId, index }) => {
      const p = this.project(projectId);
      this.ensureContainer(p, "This project");
      const list = [...this.projects].sort((a, b) => a.sortOrder - b.sortOrder).filter((x) => x.id !== projectId);
      list.splice(Math.max(0, Math.min(index, list.length)), 0, p);
      this.renumberContainers(list, "a project");
      this.touch(p);
      return this.sortedProjects();
    },
    set_active_environment: ({ projectId, environmentId }) => {
      const p = this.project(projectId);
      if ((p.activeEnvironmentId ?? null) === (environmentId ?? null)) return clone(p);
      this.ensureContainer(p, "This project");
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
      this.ensureContainer(c, "This collection");
      const auth = keepUnknownKeys(clone(collection.auth), c.auth, AUTH_SHAPE);
      // Mirrors core: auth from a newer ReqHarbor is read-only, and never written by this build.
      if (!jsonEqual(auth, c.auth) && !(isKnownAuth(auth) && isKnownAuth(c.auth))) {
        fail("read_only_newer", `The auth of “${c.name}” was set up with a newer version of ReqHarbor and is read-only here. Update ReqHarbor to edit it.`);
      }
      c.name = this.name(collection.name);
      c.description = collection.description ?? "";
      c.auth = auth;
      return clone(this.touch(c));
    },
    duplicate_collection: ({ collectionId }) => {
      const c = this.collection(collectionId);
      this.refuseReadOnlyContainers([c], "copy", "a collection");
      this.refuseReadOnlyContainers(this.folders.filter((f) => f.collectionId === collectionId), "copy", "a folder");
      this.refuseReadOnlyIn(this.requests.filter((r) => r.collectionId === collectionId), "copy");
      const sort = Math.max(...this.collections.filter((x) => x.projectId === c.projectId).map((x) => x.sortOrder)) + 1;
      this.copyCollectionTree(c, c.projectId, this.folders.filter((f) => f.collectionId === c.id), this.requests.filter((r) => r.collectionId === c.id), `${c.name} copy`, sort);
      this.markChanged();
      return this.tree(c.projectId);
    },
    delete_collection: ({ collectionId }) => {
      const c = this.collection(collectionId);
      this.refuseReadOnlyContainers(this.folders.filter((f) => f.collectionId === collectionId), "delete", "a folder");
      this.refuseReadOnlyIn(this.requests.filter((r) => r.collectionId === collectionId), "delete");
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
      this.ensureContainer(f, "This folder");
      f.name = this.name(name);
      return clone(this.touch(f));
    },
    duplicate_folder: ({ folderId }) => {
      const src = this.folder(folderId);
      this.refuseReadOnlyContainers([src, ...this.descendantFolders(folderId)], "copy", "a folder");
      this.refuseReadOnlyIn(this.requestsUnderFolder(folderId), "copy");
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
      this.refuseReadOnlyContainers(this.descendantFolders(folderId), "delete", "a folder");
      this.refuseReadOnlyIn(this.requestsUnderFolder(folderId), "delete");
      const ids = new Set([folderId, ...this.descendantFolders(folderId).map((d) => d.id)]);
      this.folders = this.folders.filter((x) => !ids.has(x.id));
      this.requests = this.requests.filter((r) => !(r.folderId && ids.has(r.folderId)));
      this.markChanged();
      return this.tree(this.projectOfCollection(f.collectionId));
    },

    create_request: ({ collectionId, folderId, request }) => {
      if (request) this.editedHere.add(request.id);
      this.collection(collectionId);
      if (folderId) {
        const f = this.folder(folderId);
        if (f.collectionId !== collectionId) fail("invalid", "Folder belongs to another collection");
      }
      this.ensureEditable(request ?? undefined);
      if (request && !METHOD_RE.test(request.method)) fail("invalid", `Invalid method "${request.method}"`);
      const ts = this.now();
      const base: Request = request
        ? clone(request)
        : { id: "", collectionId, folderId: null, name: "Untitled request", method: "GET", url: "", params: [], headers: [], auth: { type: "none" }, body: { type: "none" }, settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true }, sortOrder: 0, createdAt: 0, updatedAt: 0 };
      const r: Request = { ...base, id: ulid(ts), collectionId, folderId: folderId ?? null, name: base.name.trim() || "Untitled request", sortOrder: this.nextSort(collectionId, folderId ?? null), createdAt: ts, updatedAt: ts };
      stampMinClientVersion(r);
      this.requests.push(r);
      this.markChanged();
      return clone(r);
    },
    save_request: ({ request }) => {
      this.editedHere.add(request.id);
      const r = this.request(request.id);
      this.ensureEditable(r);
      this.ensureEditable(request);
      if (!METHOD_RE.test(request.method)) fail("invalid", `Invalid method "${request.method}"`);
      const { collectionId, folderId, sortOrder, createdAt } = r;
      Object.assign(r, keepUnknownKeys(clone(request), r), { collectionId, folderId, sortOrder, createdAt });
      stampMinClientVersion(r);
      if (!r.name.trim()) r.name = "Untitled request";
      return clone(this.touch(r));
    },
    rename_request: ({ requestId, name }) => {
      const r = this.request(requestId);
      this.ensureEditable(r);
      r.name = this.name(name);
      return clone(this.touch(r));
    },
    duplicate_request: ({ requestId }) => {
      const src = this.request(requestId);
      this.ensureEditable(src);
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
        this.ensureContainer(c, "This collection");
        const list = this.collections.filter((x) => x.projectId === c.projectId && x.id !== id).sort((a, b) => a.sortOrder - b.sortOrder);
        list.splice(Math.max(0, Math.min(index, list.length)), 0, c);
        this.renumberContainers(list, "a collection");
        this.touch(c);
        return this.tree(c.projectId);
      }
      if (!targetCollectionId) return fail("invalid", "targetCollectionId is required");
      const target = this.collection(targetCollectionId);
      const tf = targetFolderId ? this.folder(targetFolderId) : null;
      if (tf && tf.collectionId !== target.id) fail("invalid", "Target folder belongs to another collection");
      const node = kind === "folder" ? this.folder(id) : this.request(id);
      if (kind === "request") this.ensureEditable(node as Request);
      else this.ensureContainer(node, "This folder");
      const srcCollection = this.collection(node.collectionId);
      if (srcCollection.projectId !== target.projectId) fail("invalid", "Cannot move across projects");
      if (kind === "folder") {
        if (targetFolderId === id || (targetFolderId && this.descendantFolders(id).some((d) => d.id === targetFolderId))) {
          fail("invalid", "Cannot move a folder into itself");
        }
      }
      // AC-FWD-3: never re-home a read-only request with its folder.
      if (kind === "folder" && node.collectionId !== target.id) {
        this.refuseReadOnlyContainers(this.descendantFolders(id), "move", "a folder");
        this.refuseReadOnlyIn(this.requestsUnderFolder(id), "move");
      }
      const parent = targetFolderId ?? null;
      const sib = this.siblingsOf(target.id, parent);
      const sameKind = (kind === "folder" ? sib.folders : sib.requests).filter((x) => x.id !== id) as (Folder | Request)[];
      sameKind.splice(Math.max(0, Math.min(index, sameKind.length)), 0, node);
      // Read-only siblings keep their sort order (core numbers requests around them).
      const reqOrder = kind === "request" ? (sameKind as Request[]) : sib.requests;
      const fixed = new Set(reqOrder.filter((x) => isReadOnlyNewer(x, MOCK_APP_VERSION)));
      const numbers = fixed.size ? renumberAround(reqOrder, fixed) : null;
      if (fixed.size && !numbers) fail("read_only_newer", readOnlyNewerMessage([...fixed][0], "move"));
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
      // Read-only sibling folders (AC-FWD-4) keep their sort order too.
      const folderFixed = new Set(folders.filter((x) => isContainerReadOnly(x, MOCK_APP_VERSION)));
      if (numbers || folderFixed.size) {
        const fnums = renumberAround(folders, folderFixed);
        if (!fnums) return fail("read_only_newer", readOnlyNewerMessage([...folderFixed][0], "move", "a folder"));
        for (const [x, n] of fnums) x.sortOrder = n;
        if (numbers) for (const [x, n] of numbers) x.sortOrder = n;
        else reqs.forEach((x, i) => (x.sortOrder = folders.length + i));
      } else [...folders, ...reqs].forEach((x, i) => (x.sortOrder = i));
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
      this.ensureContainer(e, "This environment");
      e.name = this.name(environment.name);
      const reserved = environment.variables.find((v) => v.key.trimStart().startsWith("$"));
      if (reserved) fail("invalid", `Variable names can't start with $ ("${reserved.key.trim()}"): {{$…}} is reserved for dynamic variables`);
      e.variables = clone(environment.variables);
      return clone(this.touch(e));
    },
    duplicate_environment: ({ environmentId }) => {
      const src = this.env(environmentId);
      if (src.projectId === null) fail("invalid", "The Global environment cannot be duplicated");
      this.refuseReadOnlyContainers([src], "copy", "an environment");
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
      // A read-only project (AC-FWD-4) keeps its dangling selection, like core.
      for (const p of this.projects) if (p.activeEnvironmentId === environmentId && !isContainerReadOnly(p, MOCK_APP_VERSION)) this.touch(p).activeEnvironmentId = null;
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

    get_dynamic_variables: () => dynamicCatalogue(),

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
      if (isReadOnlyNewer(request, MOCK_APP_VERSION) || this.requests.some((r) => r.id === request.id && isReadOnlyNewer(r, MOCK_APP_VERSION))) {
        fail("read_only_newer", "This request needs a newer version of ReqHarbor");
      }
      if (request.kind === "ws") fail("validation", "This is a WebSocket request: connect to it instead", { kind: "notHttp" });
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
          res = await this.fakeExchange(prepared);
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
    ws_connect: ({ connectionId, request, projectId, onEvent }) => {
      if (isReadOnlyNewer(request, MOCK_APP_VERSION) || this.requests.some((r) => r.id === request.id && isReadOnlyNewer(r, MOCK_APP_VERSION))) {
        fail("read_only_newer", "This request needs a newer version of ReqHarbor");
      }
      if (request.kind !== "ws") fail("validation", "This is not a WebSocket request", { kind: "notWebSocket" });
      if (!request.url.trim()) fail("validation", "URL is empty", { kind: "emptyUrl" });
      const pid = projectId ?? (request.collectionId ? this.collections.find((c) => c.id === request.collectionId)?.projectId : null) ?? this.settings.activeProjectId;
      const dynamic = dynamicMemo();
      const res = this.resolveRequest(request, pid, false, dynamic);
      const disp = this.resolveRequest(request, pid, true, dynamic);
      const sioCfg = request.ws?.socketIo ?? null;
      let url = "";
      try {
        url = wsUrl(res.url, sioCfg);
      } catch (e) {
        const message = (e as Error).message;
        fail("validation", `Invalid WebSocket URL: ${message}`, { kind: "invalidWsUrl", message });
      }
      let displayUrl = disp.url;
      try {
        displayUrl = wsUrl(disp.url, sioCfg);
      } catch {
        /* shown as typed */
      }
      let sio: { namespace: string; auth: unknown } | null = null;
      if (sioCfg) {
        let auth: unknown = null;
        const text = sioCfg.auth?.trim();
        if (text) {
          const { resolve, unresolved: missing } = this.resolver(pid, false, dynamic);
          const resolved = resolve(text);
          // Mirrors core prepare_ws: name an undefined variable instead of a confusing JSON error.
          const name = [...missing][0];
          if (name !== undefined) {
            fail("validation", `Define {{${name}}} (the shared Socket.IO auth payload) in your environment`, { kind: "undefinedSocketIoAuthVariable", message: name });
          }
          try {
            auth = JSON.parse(resolved);
          } catch (e) {
            const message = (e as Error).message;
            fail("validation", `Invalid Socket.IO auth payload: ${message}`, { kind: "invalidSocketIoAuth", message });
          }
          if (!auth || typeof auth !== "object" || Array.isArray(auth)) {
            fail("validation", "Invalid Socket.IO auth payload: it must be a JSON object", { kind: "invalidSocketIoAuth", message: "it must be a JSON object" });
          }
        }
        sio = { namespace: normalizeNamespace(sioCfg.namespace), auth };
      }
      const ts = this.now();
      const method = sio ? "SIO" : "WS";
      const historyId = ulid(ts);
      this.wsMeta.set(connectionId, { request: clone(request), projectId: pid ?? null, historyId, ts, displayUrl, method, sio: !!sio });
      this.ws.connect(connectionId, url, displayUrl, sio, onEvent);
      return { connectionId, historyId, method, resolvedUrl: displayUrl, unresolvedVariables: [...res.unresolved] };
    },
    ws_send: ({ connectionId, message }) => {
      const meta = this.wsMeta.get(connectionId);
      const conn = this.ws.conns.get(connectionId);
      if (!meta || !conn) return fail("ws_not_connected", "Not connected");
      if (conn.state !== "open") return fail("ws_not_connected", "Not connected yet");
      const { resolve } = this.resolver(meta.projectId, false);
      const out = toOutMessage(message.kind, resolve(message.body), resolve(message.event ?? ""), !!message.ack, meta.sio);
      if (typeof out === "string") return fail("ws_invalid_message", out);
      try {
        return this.ws.send(connectionId, out);
      } catch (e) {
        return fail("ws_not_connected", (e as Error).message);
      }
    },
    ws_ping: ({ connectionId }) => this.ws.ping(connectionId),
    ws_close: ({ connectionId, code, reason }) => {
      if (code !== undefined && code !== null && code !== 1000 && (code < 3000 || code > 4999)) {
        fail("invalid", `Close code ${code} isn't allowed: use 1000 or 3000–4999`);
      }
      if (new TextEncoder().encode(reason ?? "").length > 123) fail("invalid", "The close reason can be at most 123 bytes");
      return this.ws.close(connectionId, code ?? null, reason ?? "");
    },
    graphql_fetch_schema: async ({ request, projectId, executionId, store }) => {
      const { url, pid, g } = await this.graphqlSchemaUrl(request, projectId);
      const { prepared } = this.prepare(g.introspectionRequest(request), pid);
      const ctrl = new AbortController();
      const id = executionId ?? ulid();
      this.inflight.set(id, ctrl);
      let res: FakeResponse;
      try {
        res = await this.fakeExchange(prepared);
        await this.delay(res.latencyMs, ctrl.signal);
      } catch (e) {
        if (e instanceof SendFailure) throw new MockError("graphql_network", e.message, { kind: e.kind });
        if ((e as Error).name === "AbortError") throw new MockError("graphql_network", "Request cancelled", { kind: "cancelled" });
        throw e;
      } finally {
        this.inflight.delete(id);
      }
      const read = g.readIntrospection(res.status, res.statusText, new TextDecoder().decode(res.body));
      if (!read.ok) throw new MockError(read.code, read.message, read.details);
      const schema: GraphqlSchema = { url, fetchedAt: this.now(), source: "introspection", format: "introspection", schema: read.schema, sizeBytes: read.schema.length };
      if (store ?? true) this.graphqlSchemas.set(schema.url, schema);
      return clone(schema);
    },
    graphql_cached_schema: async ({ request, projectId, known }) => {
      const { url } = await this.graphqlSchemaUrl(request, projectId);
      const s = this.graphqlSchemas.get(url);
      const unchanged = !!s && !!known && known.url === s.url && known.fetchedAt === s.fetchedAt;
      return { url, unchanged, schema: s && !unchanged ? clone(s) : null };
    },
    graphql_store_schema: async ({ request, projectId, schema }) => {
      const { url } = await this.graphqlSchemaUrl(request, projectId);
      if (!schema.schema.trim()) throw new MockError("graphql_schema_invalid", "This file isn't a GraphQL schema: it is empty");
      // Core: cached under the URL of the fetch / load, and only while it is still the request's.
      const s: GraphqlSchema = { ...clone(schema), sizeBytes: schema.schema.length };
      if (s.url === url) this.graphqlSchemas.set(url, s);
      return clone(s);
    },
    graphql_load_schema_file: async ({ request, projectId, path, text, fileName, store }) => {
      const { url, g } = await this.graphqlSchemaUrl(request, projectId);
      const content = text ?? (path ? g.mockSchemaFile(path) : null);
      if (content === null) throw new MockError("graphql_schema_invalid", "This file isn't a GraphQL schema: no file given");
      let schema: GraphqlSchema;
      try {
        schema = g.schemaFromText(content, fileName ?? path?.split(/[\\/]/).pop() ?? null, url, this.now());
      } catch (e) {
        const err = e as { code: "graphql_schema_invalid"; message: string };
        throw new MockError(err.code, err.message);
      }
      if (store ?? true) this.graphqlSchemas.set(url, schema);
      return clone(schema);
    },
    graphql_clear_schema: async ({ request, projectId }) => {
      const { url } = await this.graphqlSchemaUrl(request, projectId);
      return this.graphqlSchemas.delete(url);
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
      if (isReadOnlyNewer(request, MOCK_APP_VERSION)) fail("read_only_newer", "This request needs a newer version of ReqHarbor");
      if (request.kind === "ws") fail("validation", "This is a WebSocket request: connect to it instead", { kind: "notHttp" });
      if (keepVariables) return exportCurl(request, true, this.inheritedAuth(request));
      const pid = projectId ?? (request.collectionId ? this.collections.find((c) => c.id === request.collectionId)?.projectId : null) ?? this.settings.activeProjectId;
      const dynamic = dynamicMemo();
      const res = this.resolveRequest({ ...request, params: [] }, pid, false, dynamic);
      const { resolve } = this.resolver(pid, false, dynamic);
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
      return clone(this.syncStatus());
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
        this.emitStatus();
        return fail("sync_unauthorized", "Token rejected — sync disconnected. Enter a new token.");
      }
      this.connect(url, token.trim());
      return clone(this.syncStatus());
    },
    sync_disconnect: () => {
      if (this.spaces.accountMode) this.spaces.disconnectLegacy();
      this.sync = { ...this.sync, state: "disconnected", connected: false, workerUrl: null, remoteWorkspaceId: null, lastError: null, pendingChanges: 0, nextRetryAt: null, retryAttempt: 0, skippedEntities: [], unsupportedEntities: [], warning: null };
      this.syncToken = null;
      this.emitStatus();
      return clone(this.syncStatus());
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
      if (this.spaces.accountMode && this.accounts.status.signInRequired && !this.sync.connected) {
        this.sync = { ...this.sync, state: "signInRequired", lastError: "Your session ended. Sign in again to keep syncing." };
        this.emitStatus();
        return fail("sign_in_required", "Your session ended. Sign in again to keep syncing.");
      }
      if (this.sync.workerUrl?.includes("full")) {
        this.sync = { ...this.sync, state: "error", lastError: "quota_exceeded: workspace storage is full (50 MB)" };
        this.emitStatus();
        return fail("sync_quota_exceeded", "quota_exceeded: The remote workspace is full (50 MB). Delete items or large bodies to keep syncing.");
      }
      if (this.sync.workerUrl?.includes("offline")) {
        const next = this.now() + 30000;
        this.sync = { ...this.sync, state: "offline", lastError: "network unreachable", nextRetryAt: next, retryAttempt: this.sync.retryAttempt + 1 };
        this.emitStatus();
        return fail("sync_offline", "The sync server is unreachable. Retrying automatically.");
      }
      this.sync = { ...this.sync, state: "syncing" };
      this.emitStatus();
      await this.delay(200);
      const pushed = this.sync.pendingChanges;
      this.refreshSkipped();
      const ts = this.now();
      this.sync = { ...this.sync, state: "idle", lastSyncAt: ts, lastPullAt: ts, pendingChanges: 0, lastError: null, nextRetryAt: null, retryAttempt: 0 };
      this.spaces.synced(ts);
      this.emitStatus();
      return { pushed, pulled: 0, deleted: 0, conflicts: [], skipped: [], cursor: ts, tooLarge: clone(this.sync.skippedEntities), unsupported: [], restored: false };
    },

    project_locations: () => this.spaces.locations(this.sync.connected),
    project_set_location: ({ projectId, spaceId }) => {
      this.project(projectId);
      this.spaces.setLocation(projectId, spaceId);
      this.markChanged();
      this.emitStatus();
      return this.spaces.locations(this.sync.connected);
    },
    first_sync_candidates: () => this.spaces.candidates(),
    first_sync_apply: async ({ projectIds }) => {
      this.spaces.applyFirstSync(projectIds);
      await this.delay(100);
      this.spaces.synced(this.now());
      this.emitStatus();
      return clone(this.syncStatus());
    },
    space_notice_later: ({ spaceIds }) => {
      this.spaces.later(spaceIds, this.now());
      return clone(this.syncStatus());
    },
    space_notice_resolve: ({ spaceId, keep, saveChanges }) => {
      if (saveChanges) this.saveDirtyAsPersonal(spaceId);
      this.spaces.resolve(spaceId, keep);
      this.emitStatus();
      this.emit("sync://changed", { pushed: 0, pulled: 0, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 0 });
      return clone(this.syncStatus());
    },
    legacy_merge: async () => {
      const n = this.spaces.merge();
      await this.delay(200);
      const ts = this.now();
      this.emitStatus();
      // `?mockMergeSyncFails=1`: the merge stands, the follow-up sync fails.
      if (this.opts.mergeSyncFails) return { merged: n, report: null, syncError: { code: "sync_offline", message: "Offline: the server can't be reached" } };
      this.spaces.synced(ts);
      this.emitStatus();
      return { merged: n, report: { pushed: n, pulled: 0, deleted: 0, conflicts: [], skipped: [], cursor: ts, tooLarge: [], unsupported: [], restored: false }, syncError: null };
    },
    legacy_disconnect: () => {
      this.spaces.disconnectLegacy();
      this.sync = { ...this.sync, connected: false, workerUrl: null, remoteWorkspaceId: null, state: "disconnected" };
      this.syncToken = null;
      this.emitStatus();
      return clone(this.syncStatus());
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

    server_discover: ({ serverUrl }) => this.accounts.discover(serverUrl),
    auth_status: () => clone(this.accounts.status),
    auth_sign_in_start: (args) => this.accounts.signInStart(args),
    auth_sign_in_reopen: () => this.accounts.reopen(),
    auth_sign_in_cancel: () => this.accounts.cancel(),
    auth_sign_out: () => this.accounts.signOut(),
    auth_me: () => this.accounts.me(),
    auth_sessions: () => this.accounts.listSessions(),
    auth_session_revoke: ({ sessionId }) => this.accounts.revoke(sessionId),

    teams_list: () => this.teams.overview(),
    team_create: async ({ name }) => {
      await this.delay(150);
      const t = this.teams.create(name);
      this.emitStatus();
      return t;
    },
    team_get: async ({ teamId }) => {
      await this.delay(80);
      return this.teams.get(teamId);
    },
    team_rename: ({ teamId, name }) => {
      this.teams.rename(teamId, name);
      this.emitStatus();
      return null;
    },
    team_delete: async ({ teamId }) => {
      const t = this.teams.list().find((x) => x.teamId === teamId);
      this.teams.delete(teamId);
      if (t?.spaceId) this.spaces.detach(t.spaceId, true);
      await this.delay(100);
      this.emitStatus();
      this.emit("sync://changed", this.emptyReport());
      return clone(this.syncStatus());
    },
    team_set_role: ({ teamId, userId, role }) => {
      this.teams.setRole(teamId, userId, role);
      this.emitStatus();
      return null;
    },
    team_remove_member: ({ teamId, userId, revokeInvites }) => this.teams.removeMember(teamId, userId, revokeInvites),
    team_leave: async ({ teamId, keepCopy }) => {
      const t = this.teams.leave(teamId);
      this.spaces.detach(t.spaceId, keepCopy);
      await this.delay(100);
      this.emitStatus();
      this.emit("sync://changed", this.emptyReport());
      return clone(this.syncStatus());
    },
    team_transfer: ({ teamId, userId }) => {
      this.teams.transfer(teamId, userId);
      this.emitStatus();
      return null;
    },
    invite_create: async ({ teamId, role, maxUses, expiresInDays }) => {
      await this.delay(100);
      return this.teams.createInvite(teamId, role, maxUses, expiresInDays);
    },
    invite_list: ({ teamId }) => this.teams.listInvites(teamId),
    invite_revoke: ({ teamId, inviteId }) => this.teams.revokeInvite(teamId, inviteId),
    invite_preview: async ({ url, insecureConfirmed }) => {
      this.inviteInsecureGate(url, !!insecureConfirmed);
      await this.delay(150);
      return this.teams.preview(url);
    },
    invite_accept: async ({ url }) => {
      await this.delay(200);
      const r = this.teams.accept(url);
      this.spaces.synced(this.now());
      this.emitStatus();
      this.emit("sync://changed", this.emptyReport());
      return r;
    },
    invite_ticket: async ({ url, insecureConfirmed }) => {
      this.inviteInsecureGate(url, !!insecureConfirmed);
      await this.delay(80);
      return "tk_mock_ticket";
    },
    move_preview: ({ projectId, spaceId }) => this.movePreview(projectId, spaceId),
    move_project_to_space: async ({ projectId, spaceId }) => {
      this.movePreview(projectId, spaceId);
      // A plan of this project to another place is undone first.
      this.pendingMoves = this.pendingMoves.filter((m) => m.projectId !== projectId || m.to === spaceId);
      const total = 1 + this.collections.filter((c) => c.projectId === projectId).length + this.foldersOfProject(projectId).length + this.requestsOfProject(projectId).length;
      const steps = this.opts.move === "slow" ? 8 : 2;
      for (let i = 1; i <= steps; i++) {
        await this.delay(this.opts.move === "slow" ? 250 : 60);
        this.emit("sync://move-progress", { projectId, done: Math.round((total * i) / steps), total });
        if (i === 1 && (this.opts.move === "offline" || this.opts.move === "quota") && !this.moveRecovered.has(projectId)) {
          if (!this.pendingMoves.some((m) => m.projectId === projectId)) {
            this.pendingMoves.push({ moveId: ulid(this.now()), projectId, projectName: this.project(projectId).name, to: spaceId, targetName: this.spaces.binding(spaceId ?? "")?.name ?? "This device only", done: 1, total, copySaved: false });
          }
          this.moveRecovered.add(projectId);
          this.emitStatus();
          if (this.opts.move === "offline") fail("sync_offline", "You're offline. Nothing was moved.");
          const name = this.spaces.binding(spaceId ?? "")?.name ?? "The team";
          fail("sync_quota_exceeded", `${name}'s storage is full (250 MB). Ask an admin to clean up.`);
        }
      }
      const src = this.project(projectId);
      const copy = this.copyProject(projectId, src.name, spaceId, src.sortOrder);
      this.pendingMoves = this.pendingMoves.filter((m) => m.projectId !== projectId);
      if (this.settings.activeProjectId === projectId) this.settings.activeProjectId = copy;
      this.forgetProject(projectId);
      this.spaces.synced(this.now());
      this.emitStatus();
      return { moveId: ulid(this.now()), projectId: copy, items: total };
    },
    move_cancel: ({ projectId }) => {
      this.pendingMoves = this.pendingMoves.filter((m) => m.projectId !== projectId);
      this.emitStatus();
      return null;
    },
    copy_preview: ({ kind, id, targetProjectId }) => this.copyPreview(kind, id, targetProjectId),
    copy_to_project: ({ kind, id, targetProjectId, targetCollectionId, targetFolderId }) => {
      this.project(targetProjectId);
      this.spaces.guard(targetProjectId);
      const copyId = this.copyNode(kind, id, targetProjectId, targetCollectionId ?? null, targetFolderId ?? null);
      this.markChanged();
      return { tree: this.tree(targetProjectId), copyId };
    },
    project_duplicate_to: ({ projectId, spaceId, name }) => {
      this.spaces.spaceGuard(spaceId);
      const src = this.project(projectId);
      const id = this.copyProject(projectId, name?.trim() || `${src.name} copy`, spaceId, this.projects.length);
      this.markChanged();
      return this.tree(id);
    },
    request_edited_by: ({ requestId }) => {
      const r = this.request(requestId);
      const pid = this.collection(r.collectionId).projectId;
      if (this.spaces.kindOf(pid) !== "team" || this.editedHere.has(requestId) || r.createdAt > this.bootAt) return null;
      return { userId: "01JUSERSAM000000000000000", name: "Sam Lee", updatedAt: r.updatedAt };
    },
    role_change_resolve: async ({ spaceId, action }) => {
      if (!this.spaces.binding(spaceId)) fail("not_found", `Space ${spaceId} not found`);
      const copies = action === "savePersonal" ? this.saveDirtyAsPersonal(spaceId) : [];
      this.spaces.unfreeze(spaceId);
      await this.delay(100);
      this.emitStatus();
      this.emit("sync://changed", this.emptyReport());
      return { status: clone(this.syncStatus()), copies };
    },

    share_preview: ({ kind, entityId, includeCredentials, environmentId }) => this.snapshotFor(kind, entityId, includeCredentials, environmentId).preview,
    share_create: async ({ kind, entityId, includeCredentials, environmentId }) => {
      this.requireSync();
      const base = this.shareBase(kind, entityId);
      const { snapshot, preview } = this.snapshotFor(kind, entityId, includeCredentials, environmentId);
      await this.delay(120);
      const shareId = Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"[b % 62]).join("") + "x";
      const ts = this.now();
      const me = this.accounts.status.account;
      const team = base.spaceId && base.spaceId !== LEGACY ? { createdBy: me ? { userId: me.userId, name: me.name } : null, options: { includeCredentials, environmentId: environmentId ?? null } } : {};
      const info: ShareInfo = { shareId, url: `${base.url}/s/${shareId}`, kind, entityId, name: preview.name, createdAt: ts, updatedAt: ts, ...(base.spaceId ? { spaceId: base.spaceId } : {}), ...team };
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
    if (this.spaces?.accountMode) this.spaces.legacy = { workerUrl, remoteWorkspaceId: this.sync.remoteWorkspaceId ?? ulid(ts), mode: "sync" };
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
    this.emitStatus();
  }

  async invoke(cmd: string, args: Record<string, unknown> = {}): Promise<unknown> {
    const h = (this.handlers as Record<string, (a: unknown) => unknown>)[cmd];
    if (!h) throw new MockError("internal", `Unknown command ${cmd}`);
    try {
      // Simulate the IPC boundary: callers never share references with the store. Channels
      // (ws_connect's onEvent) cross it by reference, like Tauri's ipc::Channel.
      const plain: Record<string, unknown> = {};
      const channels: Record<string, unknown> = {};
      for (const [k, v] of Object.entries(args ?? {})) (isMockChannel(v) ? channels : plain)[k] = v;
      const result = await h({ ...clone(plain), ...channels });
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
      channel: (onmessage) => new MockChannel(onmessage as (m: unknown) => void) as never,
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
      newer: qs.get("newer") === "1",
      accountsEnabled: qs.get("mockAccounts") !== "off",
      legacy: qs.get("mockLegacy") === "1",
      mergeSyncFails: qs.get("mockMergeSyncFails") === "1",
      move: (qs.get("mockMove") ?? undefined) as MockOptions["move"],
      teams: {
        enabled: qs.get("mockTeams") !== "off",
        role: (qs.get("mockRole") ?? undefined) as TeamRole | undefined,
        team: qs.get("mockTeam") === "1",
      },
      spaces: {
        mode: (qs.get("mockSpace") ?? undefined) as MockSpaceMode | undefined,
        team: qs.get("mockTeam") === "1",
        role: (qs.get("mockRole") ?? undefined) as TeamRole | undefined,
        foreign: qs.get("mockForeign") === "1",
      },
      accounts: {
        server: (qs.get("mockServer") ?? undefined) as MockAccountOptions["server"],
        methods: qs.get("mockMethods")?.split(",").map((m) => m.trim()).filter(Boolean),
        auth: (qs.get("mockAuth") ?? undefined) as MockAccountOptions["auth"],
        authDelay: qs.get("mockAuthDelay") !== null ? Number(qs.get("mockAuthDelay")) : undefined,
        account: (qs.get("mockAccount") ?? undefined) as MockAccountOptions["account"],
        teams: qs.get("mockTeams") !== "off",
      },
    });
    if (typeof window !== "undefined") (window as unknown as { __mock?: MockServer }).__mock = singleton;
  }
  return singleton;
}

export function getMockBackend(): Backend {
  return getMockServer().backend();
}

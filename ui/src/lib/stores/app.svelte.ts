/**
 * Application state (Svelte 5 runes): workspace data, the open editor, send
 * state and per-request responses. All backend access goes through `call`.
 */
import {
  call,
  listen,
  toCommandError,
  type AuthStatus,
  type BodyKind,
  type KnownServer,
  type Collection,
  type CommandError,
  type Environment,
  type HistorySummary,
  type Project,
  type ProjectLocation,
  type ProjectTree,
  type Request,
  type ResponseMeta,
  type SavedMessage,
  type WsSessionSummary,
  type GrpcCallKind,
  type GrpcCallSummary,
  type GrpcStatus,
  type SendError,
  type Settings,
  type SyncStatus,
  type VariableInfo,
  type MoveResult,
  type NodeKind,
} from "../api";
import { teamsAvailable, projectPermissions } from "../util/teams";
import { copyText } from "../api/platform";
import { indexTree, ancestorsOf, requestsInSubtree, type TreeIndex } from "../util/tree";
import { makeLookup, setDynamicCatalogue } from "../util/vars";
import { isContainerReadOnly, isReadOnlyNewer, looksLikeCurl, NEEDS_NEWER_VERSION, requestFingerprint, scratchRequest } from "../util/request";
import { ulid } from "../util/ulid";
import { urlToParams, paramsToUrl } from "../util/urlx";
import { prettyJson } from "../util/json";
import { RESTORED_NOTICE } from "../util/format";
import { toasts } from "./toasts.svelte";
import { ui } from "./ui.svelte";
import { ws } from "./ws.svelte";
import { grpc } from "./grpc.svelte";
import { messageError, switchKind, type KindTarget } from "../util/ws";
import { isStreaming, sendsMessages, tryTarget } from "../util/grpc";
import { hasComments, withOperation } from "../util/graphql";
import { shortcutText } from "../util/shortcuts";

export const BODY_LIMIT = 5 * 1024 * 1024;

export interface ResponseView {
  /** Request this response belongs to. */
  requestId: string;
  response: ResponseMeta | null;
  error: SendError | null;
  unresolved: string[];
  sentAt: number;
  /** Wall-clock duration of the send as seen by the UI (null for history entries). */
  elapsedMs: number | null;
  envName: string | null;
  method: string;
  resolvedUrl: string;
  historyId: string;
  /** Set when restored from history (timestamp). */
  fromHistory: number | null;
  /** Raw body bytes (possibly limited to 5 MB). */
  body: Uint8Array | null;
  bodyLimited: boolean;
  bodyError: string | null;
  /** A WebSocket session restored from history (AC-WS-7). */
  wsSummary?: WsSessionSummary | null;
  /** The request had a GraphQL body: the strip counts `errors` in a JSON answer (AC-GQL-11). */
  graphql?: boolean;
  /** A gRPC call (AC-GRPC-7, AC-GRPC-10): the unary answer, or a stream summary from history. */
  grpc?: GrpcView | null;
}

/** What the gRPC response pane shows. */
export interface GrpcView {
  service: string;
  method: string;
  callKind: GrpcCallKind;
  status: GrpcStatus | null;
  /** Pretty JSON of the response message (unary). */
  response: string | null;
  responseTruncated: boolean;
  headers: [string, string][];
  trailers: [string, string][];
  timeMs: number | null;
  sizeBytes: number | null;
  /** History entry of a stream (or unary) call. */
  summary: GrpcCallSummary | null;
}

/** Which GraphQL editor a Format error belongs to (AC-GQL-7). */
export type FormatTarget = "body" | "query" | "variables";

export const DEFAULT_AUTH: AuthStatus = {
  enabled: false,
  account: null,
  signInRequired: false,
  keychain: "system",
  keychainNote: null,
  keychainUnreadable: false,
  server: null,
  offline: false,
  signIn: { state: "idle", serverUrl: null, method: null, startedAt: null, error: null, message: null, authorizeUrl: null },
};

/** Most recent first, at most this many suggestions. */
export const KNOWN_SERVERS_MAX = 8;

/** Upsert one `knownServers` entry (pure, for tests). */
export function mergeKnownServer(list: KnownServer[] | undefined, url: string, patch: Partial<KnownServer>, now: number): KnownServer[] {
  const all = Array.isArray(list) ? list.filter((k) => k && typeof k.url === "string") : [];
  const prev = all.find((k) => k.url === url);
  const next: KnownServer = { ...(prev ?? { url, lastUsedAt: now }), ...patch, url };
  return [next, ...all.filter((k) => k.url !== url)].sort((a, b) => b.lastUsedAt - a.lastUsedAt).slice(0, KNOWN_SERVERS_MAX);
}

export type MiddleView = { kind: "request" } | { kind: "collection"; id: string } | { kind: "environment"; id: string };

const defaultSync: SyncStatus = {
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

/** A key of what decides the project switcher's groups (bindings and space states). */
function spacesKey(s: SyncStatus): string {
  return `${s.accountMode}|${s.spaces.map((x) => `${x.spaceId}:${x.state}:${x.projectCount}`).join(",")}|${s.legacy?.mode ?? ""}`;
}

class AppStore {
  ready = $state(false);
  fatal = $state<string | null>(null);
  platform = $state("macos");
  /** This build's version (Bootstrap.appVersion), for `minClientVersion` checks. */
  appVersion = $state("0.1.0");
  workspaceName = $state("My Workspace");
  settings = $state<Settings>({
    theme: "system",
    windowWidth: null,
    windowHeight: null,
    sidebarWidth: 300,
    sidebarCollapsed: false,
    splitRatio: 0.5,
    layoutMode: "auto",
    lastOpenedRequestId: null,
    activeProjectId: null,
  });
  projects = $state.raw<Project[]>([]);
  activeProjectId = $state<string | null>(null);
  tree = $state.raw<ProjectTree | null>(null);
  environments = $state.raw<Environment[]>([]);
  variables = $state.raw<VariableInfo[]>([]);
  history = $state.raw<HistorySummary[]>([]);
  sync = $state.raw<SyncStatus>(defaultSync);
  /** Sign-in state (`auth://status`). */
  auth = $state.raw<AuthStatus>(DEFAULT_AUTH);
  /** Where each project syncs (account mode; the grouped switcher, UX §5.1). */
  locations = $state.raw<ProjectLocation[]>([]);
  now = $state(Date.now());

  // editor
  request = $state<Request>(scratchRequest());
  savedFingerprint = $state<string | null>(null);
  drafts = $state.raw<Record<string, Request>>({});
  middle = $state<MiddleView>({ kind: "request" });
  /** Responses per request id (last one shown when switching back). */
  responses = $state.raw<Record<string, ResponseView>>({});
  inflight = $state.raw<Record<string, { executionId: string; startedAt: number }>>({});
  /** Inline validation shown in the request bar / body. */
  sendBlocked = $state<string | null>(null);
  formatError = $state<{ message: string; line: number; column: number; target?: FormatTarget } | null>(null);
  /** AC-GQL-7: Format removed `#` comments from this request's query. */
  formatNotice = $state<{ requestId: string; message: string } | null>(null);
  /** Undo snapshot for "Imported from cURL". */
  private undoRequest: Request | null = null;
  exportKeepVariables = $state(false);

  // derived
  project = $derived(this.projects.find((p) => p.id === this.activeProjectId) ?? null);
  index = $derived<TreeIndex | null>(this.tree ? indexTree(this.tree) : null);
  lookup = $derived(makeLookup(this.variables));
  globalEnv = $derived(this.environments.find((e) => e.projectId === null) ?? null);
  projectEnvs = $derived(this.environments.filter((e) => e.projectId === this.activeProjectId));
  activeEnv = $derived(this.project?.activeEnvironmentId ? (this.environments.find((e) => e.id === this.project!.activeEnvironmentId) ?? null) : null);
  isScratch = $derived(this.request.collectionId === "");
  fingerprint = $derived(requestFingerprint(this.request));
  dirty = $derived(!this.isScratch && this.savedFingerprint !== null && this.fingerprint !== this.savedFingerprint);
  /**
   * The open request came from a newer ReqHarbor: view only, never saved or sent (AC-FWD-3/4).
   * The saved copy decides too, so an editor copy that lost `kind` / `body.type` can't unlock it.
   */
  readOnly = $derived(isReadOnlyNewer(this.request, this.appVersion) || (!this.isScratch && this.isReadOnly(this.request.id)));
  currentResponse = $derived(this.responses[this.request.id] ?? null);
  currentInflight = $derived(this.inflight[this.request.id] ?? null);
  /** Not-synced items by id, with the reason (`SkippedEntity.reason`). */
  skippedReasons = $derived(new Map(this.sync.skippedEntities.map((s) => [s.id, s.reason] as const)));
  mod = $derived(this.platform === "macos" ? "⌘" : "Ctrl+");
  locationById = $derived(new Map(this.locations.map((l) => [l.projectId, l] as const)));
  /** Signed in with a readable session (the account part of the UI is on). */
  signedIn = $derived(this.auth.enabled && !!this.auth.account && !this.auth.keychainUnreadable);
  /** The account's server advertises `features.teams` (06 §10). */
  teamsOn = $derived(teamsAvailable(this.auth));
  /** The active project is in a team where I'm a viewer (AC-ROLE-1, UX §6.2). */
  viewerHere = $derived(!!projectPermissions(this.activeProjectId ? this.locationById.get(this.activeProjectId) : undefined).edit);
  /** The active project's space status (role, frozen changes for the banner). */
  activeSpace = $derived.by(() => {
    const l = this.activeProjectId ? this.locationById.get(this.activeProjectId) : undefined;
    return l?.spaceId ? (this.sync.spaces.find((s) => s.spaceId === l.spaceId) ?? null) : null;
  });

  private settingsTimer: ReturnType<typeof setTimeout> | null = null;
  private tick: ReturnType<typeof setInterval> | null = null;

  // ───────────── bootstrap ─────────────
  async init(): Promise<void> {
    ws.onEnded = () => void this.loadHistory();
    try {
      // QA-R2-007: subscribe before the startup load so an early pull isn't missed.
      await listen("sync://status", (s) => this.onSyncStatus(s));
      await listen("auth://status", (s) => this.onAuth(s));
      await listen("sync://changed", (r) => {
        void this.reloadAll();
        for (const c of r.conflicts) toasts.warning(`Sync conflict: kept your edit as “${c.name}”`);
        if (r.restored) toasts.info(RESTORED_NOTICE);
      });
      await listen("sync://notice", (n) => {
        void this.reloadAll();
        if (n.kind === "success") toasts.success(n.message);
        else toasts.info(n.message);
      });
      const b = await call("bootstrap", {});
      this.platform = b.platform;
      if (b.appVersion) this.appVersion = b.appVersion;
      this.workspaceName = b.workspace.name;
      this.settings = b.settings;
      this.projects = b.projects;
      this.environments = b.environments;
      this.sync = { ...defaultSync, ...b.sync };
      this.auth = { ...this.auth, enabled: !!b.accounts };
      if (b.accounts) {
        try {
          this.auth = await call("auth_status", {});
        } catch {
          /* signed out */
        }
      }
      const extra = b.settings as Settings & { sidebarTab?: string; exportKeepVariables?: boolean };
      if (extra.sidebarTab === "activity" || extra.sidebarTab === "collections" || extra.sidebarTab === "env") ui.sidebarTab = extra.sidebarTab;
      this.exportKeepVariables = !!extra.exportKeepVariables;
      const pid = b.projects.find((p) => p.id === b.settings.activeProjectId)?.id ?? b.projects[0]?.id ?? null;
      this.activeProjectId = pid;
      await Promise.all([this.loadTree(), this.loadVariables(), this.loadHistory(), this.loadDynamicVariables(), this.loadLocations()]);
      const last = b.settings.lastOpenedRequestId;
      if (last && this.index?.requestById.has(last)) this.openRequest(last, { reveal: true });
      for (const c of b.corruptFiles) {
        const name = c.originalPath.split(/[\\/]/).pop() ?? c.originalPath;
        const saved = c.renamedTo ? ` — saved as ${c.renamedTo.split(/[\\/]/).pop()}` : "";
        toasts.error(`${name} couldn't be read${saved}`, c.renamedTo ? { actionLabel: "Copy path", action: () => void copyText(c.renamedTo) } : {});
      }
      // Anything that changed between bootstrap and now is picked up here.
      this.onSyncStatus(await call("get_sync_status", {}));
      this.tick = setInterval(() => (this.now = Date.now()), 60_000);
      this.ready = true;
    } catch (e) {
      this.fatal = toCommandError(e).message;
    }
  }

  destroy() {
    if (this.tick) clearInterval(this.tick);
  }

  /** A new sync status; the switcher's groups reload when bindings or space states change. */
  onSyncStatus(s: SyncStatus) {
    const next = { ...defaultSync, ...s };
    const changed = spacesKey(next) !== spacesKey(this.sync);
    this.sync = next;
    if (changed) void this.loadLocations();
  }

  async loadLocations() {
    try {
      this.locations = await call("project_locations", {});
    } catch {
      this.locations = [];
    }
  }

  /**
   * WebSocket sessions of requests that no longer exist (deleted here, by a sync pull, or with their
   * project or space) are closed and forgotten. The open request keeps its session: deleted
   * underneath us it becomes a scratch request that is still on screen.
   */
  pruneWsSessions(t: ProjectTree | null = null) {
    const projects = new Set(this.projects.map((p) => p.id));
    const requests = t ? new Set(t.requests.map((r) => r.id)) : null;
    void ws.prune((s) => {
      if (s.requestId === this.request.id) return true;
      if (s.projectId && !projects.has(s.projectId)) return false;
      return !(t && requests && s.saved && s.projectId === t.project.id && !requests.has(s.requestId));
    });
  }

  async reloadAll() {
    void this.loadLocations();
    this.projects = await call("list_projects", {});
    this.pruneWsSessions();
    if (!this.projects.some((p) => p.id === this.activeProjectId)) this.activeProjectId = this.projects[0]?.id ?? null;
    this.environments = await call("list_environments", {});
    await Promise.all([this.loadTree(), this.loadVariables()]);
  }

  async loadTree() {
    if (!this.activeProjectId) return;
    this.setTree(await call("get_project_tree", { projectId: this.activeProjectId }));
  }

  setTree(t: ProjectTree) {
    if (t.project.id !== this.activeProjectId) return;
    this.tree = t;
    this.pruneWsSessions(t);
    this.projects = this.projects.map((p) => (p.id === t.project.id ? t.project : p));
    /** Unsaved edits of requests that a newer ReqHarbor changed underneath us (AC-FWD-3). */
    const lost: { draft: Request; saved: Request }[] = [];
    // Keep the open saved request's baseline in sync (rename/move change it).
    if (!this.isScratch) {
      const saved = t.requests.find((r) => r.id === this.request.id);
      if (!saved) {
        // deleted underneath us → becomes a scratch
        this.request = { ...this.request, collectionId: "", folderId: null };
        this.savedFingerprint = null;
      } else if (isReadOnlyNewer(saved, this.appVersion)) {
        // A sync pull turned it into a request from a newer ReqHarbor: switch the editor to the
        // view-only saved copy. Unsaved edits can't be applied to it; they are offered as a copy.
        if (this.dirty && !isReadOnlyNewer(this.request, this.appVersion)) {
          lost.push({ draft: $state.snapshot(this.request) as Request, saved });
        }
        this.request = structuredClone(saved);
        this.savedFingerprint = requestFingerprint(saved);
        this.sendBlocked = null;
      } else {
        const wasDirty = this.dirty;
        if (!wasDirty) {
          this.request = structuredClone(saved);
          this.savedFingerprint = requestFingerprint(saved);
        } else {
          this.request.name = saved.name;
          this.request.collectionId = saved.collectionId;
          this.request.folderId = saved.folderId;
          this.savedFingerprint = requestFingerprint({ ...saved });
        }
      }
    }
    const byId = new Map(t.requests.map((r) => [r.id, r]));
    const d = Object.fromEntries(
      Object.entries(this.drafts).filter(([id, draft]) => {
        const saved = byId.get(id);
        if (!saved) return false;
        if (!isReadOnlyNewer(saved, this.appVersion)) return true;
        if (!isReadOnlyNewer(draft, this.appVersion)) lost.push({ draft, saved });
        return false;
      }),
    );
    if (Object.keys(d).length !== Object.keys(this.drafts).length) this.drafts = d;
    this.offerDraftCopies(lost);
  }

  /** Drafts dropped because their request became read-only: one persistent toast offering copies. */
  private offerDraftCopies(lost: { draft: Request; saved: Request }[]) {
    if (!lost.length) return;
    const msg =
      lost.length === 1
        ? `“${lost[0].saved.name}” was changed with a newer version of ReqHarbor and is now read-only here. Your unsaved edits weren't applied.`
        : `${lost.length} requests were changed with a newer version of ReqHarbor and are now read-only here. Your unsaved edits weren't applied.`;
    toasts.warning(msg, {
      actionLabel: lost.length === 1 ? "Save my edits as a copy" : "Save my edits as copies",
      action: () => void this.saveDraftCopies(lost),
      timeout: 0,
    });
  }

  private async saveDraftCopies(lost: { draft: Request; saved: Request }[]) {
    let last: Request | null = null;
    try {
      for (const { draft, saved } of lost) {
        const request: Request = { ...draft, name: `${draft.name || saved.name} (my edits)` };
        last = await call("create_request", { collectionId: saved.collectionId, folderId: saved.folderId, request });
      }
    } catch (e) {
      this.reportError(e);
    }
    await this.loadTree();
    if (last) {
      this.openRequest(last.id, { reveal: true });
      toasts.success(lost.length === 1 ? `Saved your edits as “${last.name}”` : `Saved your edits as ${lost.length} copies`);
    }
  }

  async loadVariables() {
    this.variables = await call("get_variables", { projectId: this.activeProjectId });
  }

  /** `{{$name}}` catalogue (AC-ENV-8); the built-in copy stays if this fails. */
  async loadDynamicVariables() {
    try {
      setDynamicCatalogue(await call("get_dynamic_variables", {}));
    } catch {
      /* keep the built-in catalogue */
    }
  }

  async loadHistory() {
    this.history = await call("list_history", {});
  }

  async loadEnvironments() {
    this.environments = await call("list_environments", {});
    await this.loadVariables();
  }

  // ───────────── settings ─────────────
  private settingsSaveFailing = false;

  /**
   * Persist settings (debounced). QA-R2-001: integer fields are u32 in Rust, so pane sizes are
   * rounded before saving; a failed save shows one error toast and later saves keep trying.
   */
  updateSettings(patch: Partial<Settings>, immediate = false) {
    this.settings = { ...this.settings, ...patch };
    if (this.settingsTimer) clearTimeout(this.settingsTimer);
    const run = async () => {
      const s = $state.snapshot(this.settings) as Settings;
      s.sidebarWidth = Math.round(s.sidebarWidth);
      if (s.windowWidth !== null) s.windowWidth = Math.round(s.windowWidth);
      if (s.windowHeight !== null) s.windowHeight = Math.round(s.windowHeight);
      try {
        await call("save_settings", { settings: s });
        this.settingsSaveFailing = false;
      } catch (e) {
        if (!this.settingsSaveFailing) toasts.error(`Couldn't save settings: ${toCommandError(e).message}`);
        this.settingsSaveFailing = true;
      }
    };
    if (immediate) void run();
    else this.settingsTimer = setTimeout(() => void run(), 300);
  }

  /** Apply an auth status; a fresh sign-in toasts and is remembered as a known server. */
  onAuth(next: AuthStatus) {
    const prev = this.auth;
    this.auth = next;
    if (next.signIn.state === "success" && prev.signIn.state !== "success" && next.account) {
      toasts.success(`Signed in as ${next.account.name || next.account.email}`);
      this.rememberServer(next.account.serverUrl, { name: next.account.serverName, lastUsedAt: Date.now() });
    }
    // Signing in or out changes where projects sync (and their pause state).
    if (!!prev.account !== !!next.account || prev.account?.userId !== next.account?.userId) {
      void this.reloadAll();
      void call("get_sync_status", {}).then((s) => this.onSyncStatus(s), () => {});
    }
  }

  /** AC-AUTH-11 "Sign out on this device", after the UX §4.3 confirm. */
  async signOut(): Promise<boolean> {
    const pending = this.sync.connected ? this.sync.pendingChanges : 0;
    const extra = pending > 0 ? ` ${pending} ${pending === 1 ? "change hasn't" : "changes haven't"} synced yet. They'll sync when you sign back in.` : "";
    const ok = await ui.confirm({
      title: "Sign out on this device?",
      body: `Your projects stay on this device. Syncing pauses until you sign in again.${extra}`,
      confirmLabel: "Sign out",
      danger: false,
    });
    if (!ok) return false;
    try {
      this.auth = await call("auth_sign_out", {});
      toasts.info("Signed out on this device");
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  get knownServers(): KnownServer[] {
    const list = (this.settings as Settings & { knownServers?: KnownServer[] }).knownServers;
    return Array.isArray(list) ? list : [];
  }

  /** Remember a server for suggestions and its per-origin choices (06 §9). */
  rememberServer(url: string, patch: Partial<KnownServer> = {}) {
    this.updateSettings({ knownServers: mergeKnownServer(this.knownServers, url, patch, Date.now()) }, true);
  }

  /** QA-R2-002: options a share was last uploaded with (kept client-side in settings.json). */
  shareOptions(shareId: string): { includeCredentials: boolean; environmentId: string | null } | null {
    const all = (this.settings as Settings & { shareOptions?: Record<string, { includeCredentials: boolean; environmentId: string | null }> }).shareOptions;
    return all?.[shareId] ?? null;
  }

  rememberShareOptions(shareId: string, opts: { includeCredentials: boolean; environmentId: string | null } | null) {
    const all = { ...((this.settings as Settings & { shareOptions?: Record<string, unknown> }).shareOptions ?? {}) };
    if (opts) all[shareId] = opts;
    else delete all[shareId];
    this.updateSettings({ shareOptions: all }, true);
  }

  // ───────────── projects ─────────────
  async switchProject(id: string) {
    if (id === this.activeProjectId) return;
    this.stashDraft();
    this.activeProjectId = id;
    this.tree = null;
    this.middle = { kind: "request" };
    ui.expanded = new Set();
    this.updateSettings({ activeProjectId: id });
    await Promise.all([this.loadTree(), this.loadVariables()]);
    if (!this.isScratch && !this.index?.requestById.has(this.request.id)) this.newRequest();
  }

  async setActiveEnvironment(envId: string | null) {
    if (!this.activeProjectId) return;
    try {
      const p = await call("set_active_environment", { projectId: this.activeProjectId, environmentId: envId });
      this.projects = this.projects.map((x) => (x.id === p.id ? p : x));
      await this.loadVariables();
    } catch (e) {
      this.reportError(e);
    }
  }

  /** Saved request `id` (or the given request) is from a newer ReqHarbor. */
  isReadOnly(r: string | Request): boolean {
    const req = typeof r === "string" ? this.index?.requestById.get(r) : r;
    return !!req && isReadOnlyNewer(req, this.appVersion);
  }

  /** A collection or folder of the open project carries a newer `minClientVersion` (AC-FWD-4). */
  isContainerReadOnly(kind: "collection" | "folder", id: string): boolean {
    const e = kind === "collection" ? this.index?.collectionById.get(id) : this.index?.folderById.get(id);
    return isContainerReadOnly(e, this.appVersion);
  }

  /** Guard for request mutations: explains and returns true when `id` is read-only. */
  private refuseReadOnly(id: string): boolean {
    if (!this.isReadOnly(id)) return false;
    toasts.info(`${NEEDS_NEWER_VERSION}. Update ReqHarbor to edit it.`);
    return true;
  }

  // ───────────── editor ─────────────
  private stashDraft() {
    if (this.isScratch) return;
    const id = this.request.id;
    if (this.dirty) this.drafts = { ...this.drafts, [id]: $state.snapshot(this.request) as Request };
    else if (this.drafts[id]) {
      const { [id]: _drop, ...rest } = this.drafts;
      this.drafts = rest;
    }
  }

  isDirty(requestId: string): boolean {
    if (requestId === this.request.id) return this.dirty;
    return !!this.drafts[requestId];
  }

  openRequest(id: string, opts: { reveal?: boolean } = {}) {
    const saved = this.index?.requestById.get(id);
    if (!saved) return;
    if (id !== this.request.id) this.stashDraft();
    this.request = structuredClone(this.drafts[id] ?? saved);
    this.savedFingerprint = requestFingerprint(saved);
    this.middle = { kind: "request" };
    this.sendBlocked = null;
    this.formatError = null;
    ui.selectedNode = { kind: "request", id, collectionId: saved.collectionId };
    if (opts.reveal && this.index) ui.expandAll(ancestorsOf(this.index, id));
    this.updateSettings({ lastOpenedRequestId: id });
  }

  newRequest(partial: Partial<Request> = {}) {
    this.stashDraft();
    this.request = scratchRequest(partial);
    this.savedFingerprint = null;
    this.middle = { kind: "request" };
    this.sendBlocked = null;
    this.formatError = null;
    ui.selectedNode = null;
    ui.focusUrlTick++;
  }

  openCollection(id: string) {
    this.middle = { kind: "collection", id };
    ui.selectedNode = { kind: "collection", id, collectionId: id };
  }

  openEnvironment(id: string) {
    this.middle = { kind: "environment", id };
    ui.selectedEnvId = id;
  }

  /** URL edited → derive Query rows (AC-REQ-2). */
  setUrl(url: string) {
    this.request.url = url;
    this.request.params = urlToParams(url, this.request.params);
    this.sendBlocked = null;
  }

  /** Query rows edited → rewrite URL (AC-REQ-2). */
  syncUrlFromParams() {
    this.request.url = paramsToUrl(this.request.url, this.request.params);
  }

  async save(): Promise<void> {
    if (this.middle.kind !== "request") return;
    if (this.readOnly) {
      toasts.info(`${NEEDS_NEWER_VERSION}. Update ReqHarbor to edit it.`);
      return;
    }
    if (this.viewerHere && !this.isScratch) {
      // UX §6.2: edits in a viewer's team project are scratch; ⌘S explains and offers a copy.
      const id = this.request.id;
      toasts.info("Viewers can't change team projects. Duplicate to personal to experiment.", {
        actionLabel: "Duplicate to personal",
        action: () => (ui.copyToProject = { kind: "request", id, name: this.request.name, mode: "duplicate" }),
      });
      return;
    }
    if (this.isScratch) {
      ui.picker = { kind: "saveRequest" };
      return;
    }
    try {
      const saved = await call("save_request", { request: $state.snapshot(this.request) as Request });
      this.savedFingerprint = requestFingerprint(saved);
      this.request.updatedAt = saved.updatedAt;
      if (this.drafts[saved.id]) {
        const { [saved.id]: _d, ...rest } = this.drafts;
        this.drafts = rest;
      }
      if (this.tree) this.tree = { ...this.tree, requests: this.tree.requests.map((r) => (r.id === saved.id ? saved : r)) };
      toasts.success("Saved");
    } catch (e) {
      this.reportError(e);
    }
  }

  /** Save the scratch request into a collection/folder (AC-REQ-11). */
  async saveScratchTo(collectionId: string, folderId: string | null, name: string) {
    try {
      const req = { ...($state.snapshot(this.request) as Request), name };
      const created = await call("create_request", { collectionId, folderId, request: req });
      await this.loadTree();
      const prevId = this.request.id;
      if (this.responses[prevId]) this.responses = { ...this.responses, [created.id]: { ...this.responses[prevId], requestId: created.id } };
      ws.rekey(prevId, created.id);
      grpc.rekey(prevId, created.id);
      this.openRequest(created.id, { reveal: true });
      toasts.success(`Saved to ${this.index?.collectionById.get(collectionId)?.name ?? "collection"}`);
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── send ─────────────
  async sendOrCancel() {
    if (this.middle.kind !== "request") return;
    // ⌘Enter on a WebSocket request: send the composed message when open, else connect.
    if (this.request.kind === "ws") return ws.session(this.request.id)?.state === "open" ? this.wsSendDraft() : this.wsConnectOrDisconnect();
    if (this.request.kind === "grpc") {
      const s = ws.session(this.request.id);
      // ⌘Enter on an open client / bidi stream sends the composed message (AC-GRPC-8).
      if (s?.transport === "grpc" && s.state === "open" && sendsMessages(s.callKind) && !s.halfClosed) return this.grpcSendDraft();
      return this.grpcRunOrCancel();
    }
    if (this.currentInflight) return this.cancel();
    return this.send();
  }

  async send() {
    if (this.request.kind === "ws") return this.wsConnect();
    if (this.request.kind === "grpc") return this.grpcRun();
    // AC-GQL-9: send the operation the picker shows (stored requests may lack or carry a stale one).
    const req = withOperation($state.snapshot(this.request) as Request);
    if (this.readOnly) {
      this.sendBlocked = NEEDS_NEWER_VERSION;
      return;
    }
    if (!req.url.trim()) {
      this.sendBlocked = "Enter a URL";
      return;
    }
    // A cURL command that reached the URL bar without the paste hook (typed,
    // dropped, or pasted from a context menu): import it instead of sending it.
    if (looksLikeCurl(req.url) && (await this.importCurlInPlace(req.url))) return;
    if (req.body.type === "graphql" && req.body.graphql?.variables.trim()) {
      try {
        JSON.parse(req.body.graphql.variables);
      } catch {
        this.sendBlocked = "Fix GraphQL variables";
        return;
      }
    }
    const executionId = ulid();
    const startedAt = Date.now();
    this.inflight = { ...this.inflight, [req.id]: { executionId, startedAt } };
    this.sendBlocked = null;
    const envName = this.activeEnv?.name ?? null;
    try {
      const res = await call("send_request", { executionId, request: req, projectId: this.activeProjectId });
      const view: ResponseView = {
        requestId: req.id,
        response: res.response,
        error: res.error,
        unresolved: res.unresolvedVariables,
        sentAt: startedAt,
        elapsedMs: Date.now() - startedAt,
        envName,
        method: res.method,
        resolvedUrl: res.resolvedUrl,
        historyId: res.historyId,
        fromHistory: null,
        body: null,
        bodyLimited: false,
        bodyError: null,
        graphql: req.body.type === "graphql",
      };
      this.setResponse(view);
      void this.loadHistory();
      if (res.response) void this.fetchBody(view);
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "read_only_newer") this.sendBlocked = NEEDS_NEWER_VERSION;
      else if (err.code === "validation") {
        const d = err.details as { kind?: string; message?: string } | undefined;
        this.sendBlocked =
          d?.kind === "invalidGraphqlVariables" ? "Fix GraphQL variables" : d?.kind === "emptyUrl" ? "Enter a URL" : err.message;
      } else this.reportError(err);
    } finally {
      const { [req.id]: _x, ...rest } = this.inflight;
      this.inflight = rest;
    }
  }

  // ───────────── WebSocket (AC-WS-*) ─────────────

  /** Method dropdown: switch the open request between HTTP (with `method`), WebSocket, Socket.IO and gRPC. */
  setKind(target: KindTarget, method = "GET") {
    if (this.readOnly) return;
    const s = ws.session(this.request.id);
    const transport = target === "grpc" ? "grpc" : target === "http" ? null : "ws";
    if (s && ws.isLive(this.request.id) && s.transport !== transport) void ws.disconnect(this.request.id);
    switchKind(this.request, target, method);
    this.sendBlocked = null;
  }

  // ───────────── gRPC (AC-GRPC-*) ─────────────

  /** The call kind of the open gRPC request's method (services, else its live stream), null when unknown. */
  grpcCallKind(r: Request = this.request): GrpcCallKind | null {
    const k = grpc.callKind(r);
    if (k) return k;
    const s = ws.session(r.id);
    const g = r.grpc;
    if (s?.transport === "grpc" && g && s.callKind && s.title.endsWith(`/${g.method}`)) return s.callKind;
    // What a rejected Invoke taught us when the services aren't cached (AC-GRPC-7).
    const hint = g ? this.grpcKindHints[r.id] : undefined;
    return hint && g && hint.key === `${g.service}/${g.method}` ? hint.kind : null;
  }

  /** requestId → the call kind the core reported for its method ("grpcStreamingMethod"). */
  grpcKindHints = $state<Record<string, { key: string; kind: GrpcCallKind }>>({});

  /** The gRPC call of the open request is live: an Invoke in flight or a stream. */
  grpcLive(r: Request = this.request): boolean {
    return !!this.inflight[r.id] || (ws.session(r.id)?.transport === "grpc" && ws.isLive(r.id));
  }

  /** Request bar button: Invoke / Connect, or Cancel while live. */
  async grpcRunOrCancel() {
    const id = this.request.id;
    if (this.inflight[id]) return this.cancel();
    if (ws.session(id)?.transport === "grpc" && ws.isLive(id)) return ws.disconnect(id);
    return this.grpcRun();
  }

  /** Checks the UI can make before anything is sent; the inline reason, or null. */
  private grpcBlocked(req: Request): string | null {
    if (this.readOnly) return NEEDS_NEWER_VERSION;
    if (!req.url.trim()) return "Enter a URL";
    if (!tryTarget(req.url) && !/\{\{/.test(req.url)) return "Use grpc://, grpcs://, http:// or https://";
    if (!req.grpc?.service || !req.grpc.method) return "Pick a method";
    return null;
  }

  /** A grpc_* rejection as the inline reason under the bar. */
  private grpcReject(req: Request, e: unknown) {
    const err = toCommandError(e);
    if (err.code === "read_only_newer") this.sendBlocked = NEEDS_NEWER_VERSION;
    else if (err.code === "validation") {
      const d = err.details as { kind?: string } | undefined;
      this.sendBlocked = d?.kind === "emptyUrl" ? "Enter a URL" : d?.kind === "noGrpcMethod" ? "Pick a method" : err.message;
    } else if (err.code.startsWith("grpc_")) {
      this.sendBlocked = err.message;
      grpc.reportError(req.id, err);
    } else this.reportError(err);
  }

  /** Invoke (unary; also when the kind isn't known yet) or start a stream. */
  async grpcRun() {
    const req = $state.snapshot(this.request) as Request;
    const blocked = this.grpcBlocked(req);
    if (blocked) {
      this.sendBlocked = blocked;
      return;
    }
    this.sendBlocked = null;
    const kind = this.grpcCallKind(req);
    if (isStreaming(kind)) return this.grpcStart(req, kind);
    return this.grpcInvoke(req);
  }

  /** Start a stream, then pick up the services the call just fetched or compiled. */
  private async grpcStart(req: Request, kind: GrpcCallKind | null) {
    try {
      await ws.startGrpc(req, this.activeProjectId, kind);
    } catch (e) {
      this.grpcReject(req, e);
    }
    void grpc.refreshCached(req, this.activeProjectId);
  }

  async grpcInvoke(req: Request) {
    let streaming: GrpcCallKind | null = null;
    const executionId = ulid();
    const startedAt = Date.now();
    this.inflight = { ...this.inflight, [req.id]: { executionId, startedAt } };
    const envName = this.activeEnv?.name ?? null;
    try {
      const res = await call("grpc_invoke", { executionId, request: req, projectId: this.activeProjectId });
      this.setResponse({
        requestId: req.id,
        response: null,
        error: res.error,
        unresolved: res.unresolvedVariables,
        sentAt: startedAt,
        elapsedMs: Date.now() - startedAt,
        envName,
        method: "GRPC",
        resolvedUrl: res.resolvedUrl,
        historyId: res.historyId,
        fromHistory: null,
        body: null,
        bodyLimited: false,
        bodyError: null,
        grpc: {
          service: req.grpc?.service ?? "",
          method: req.grpc?.method ?? "",
          callKind: res.callKind,
          status: res.status,
          response: res.response,
          responseTruncated: false,
          headers: res.headers,
          trailers: res.trailers,
          timeMs: res.timeMs,
          sizeBytes: res.sizeBytes,
          summary: null,
        },
      });
      void this.loadHistory();
    } catch (e) {
      const err = toCommandError(e);
      const d = err.details as { kind?: string; callKind?: GrpcCallKind } | undefined;
      if (err.code === "validation" && d?.kind === "grpcStreamingMethod" && d.callKind) {
        // The services weren't known, so the button said Invoke: the method streams (AC-GRPC-7/8).
        streaming = d.callKind;
        this.grpcKindHints = { ...this.grpcKindHints, [req.id]: { key: `${req.grpc?.service}/${req.grpc?.method}`, kind: d.callKind } };
        if (d.callKind !== "serverStreaming") this.sendBlocked = "This method streams: use Connect";
      } else this.grpcReject(req, e);
    } finally {
      const { [req.id]: _x, ...rest } = this.inflight;
      this.inflight = rest;
    }
    // The call fetched (or compiled) the services: show them, and the right button.
    void grpc.refreshCached(req, this.activeProjectId);
    // A server-streaming method shows every message: start it as a stream right away.
    if (streaming === "serverStreaming") await this.grpcStart(req, streaming);
  }

  /** Send the Message tab's JSON on the open client / bidi stream. */
  async grpcSendDraft(): Promise<boolean> {
    const r = this.request;
    const text = r.grpc?.message ?? "";
    if (!/\{\{/.test(text) && text.trim()) {
      try {
        JSON.parse(text);
      } catch (e) {
        toasts.error(`Invalid JSON: ${(e as Error).message}`);
        return false;
      }
    }
    return this.grpcSend(text);
  }

  async grpcSend(message: string): Promise<boolean> {
    try {
      await ws.grpcSend(this.request.id, message);
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  async grpcEnd() {
    await ws.grpcEnd(this.request.id);
  }

  /** Save the Message tab's JSON to the request's saved messages (marks it dirty). */
  grpcSaveDraft(name: string) {
    const g = this.request.grpc;
    if (this.readOnly || this.request.kind !== "grpc" || !g) return;
    g.messages.push({ name: name.trim(), body: g.message });
  }

  /** Request bar button: Connect when closed, Disconnect (or stop connecting) when live. */
  async wsConnectOrDisconnect() {
    if (ws.isLive(this.request.id)) return ws.disconnect(this.request.id);
    return this.wsConnect();
  }

  async wsConnect() {
    const req = $state.snapshot(this.request) as Request;
    if (this.readOnly) {
      this.sendBlocked = NEEDS_NEWER_VERSION;
      return;
    }
    if (!req.url.trim()) {
      this.sendBlocked = "Enter a URL";
      return;
    }
    this.sendBlocked = null;
    try {
      await ws.connect(req, this.activeProjectId);
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "read_only_newer") this.sendBlocked = NEEDS_NEWER_VERSION;
      else if (err.code === "validation") {
        const d = err.details as { kind?: string } | undefined;
        this.sendBlocked = d?.kind === "emptyUrl" ? "Enter a URL" : err.message;
      } else this.reportError(err);
    }
  }

  /** Send the composer's message on the open connection. */
  async wsSendDraft() {
    const r = this.request;
    const msg = $state.snapshot(ws.draftOf(r)) as SavedMessage;
    return this.wsSend(msg);
  }

  async wsSend(msg: SavedMessage): Promise<boolean> {
    const r = this.request;
    const sio = !!r.ws?.socketIo;
    const problem = messageError(msg, sio);
    if (problem) {
      toasts.error(problem);
      return false;
    }
    try {
      await ws.send(r.id, msg);
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  /** Save the composer's message to the request's saved messages (marks the request dirty). */
  wsSaveDraft(name: string) {
    const r = this.request;
    if (this.readOnly || r.kind !== "ws") return;
    const msg = structuredClone($state.snapshot(ws.draftOf(r)) as SavedMessage);
    msg.name = name.trim();
    r.ws ??= { messages: [], protocols: [], autoReconnect: false };
    r.ws.messages.push(msg);
  }

  async cancel() {
    const f = this.currentInflight;
    if (f) await call("cancel_request", { executionId: f.executionId }).catch(() => false);
  }

  private setResponse(view: ResponseView) {
    this.responses = { ...this.responses, [view.requestId]: view };
  }

  async fetchBody(view: ResponseView) {
    const meta = view.response;
    if (!meta) return;
    const limited = meta.sizeBytes > BODY_LIMIT;
    try {
      const buf = await call("get_response_body", { responseId: meta.responseId, limit: limited ? BODY_LIMIT : null });
      const cur = this.responses[view.requestId];
      if (cur && cur.response?.responseId === meta.responseId) {
        this.setResponse({ ...cur, body: new Uint8Array(buf), bodyLimited: limited });
      }
    } catch (e) {
      const cur = this.responses[view.requestId];
      if (cur) this.setResponse({ ...cur, bodyError: toCommandError(e).message });
    }
  }

  // ───────────── history ─────────────
  async openHistory(id: string) {
    try {
      const { entry, response } = await call("open_history_entry", { historyId: id });
      ui.selectedHistoryId = id;
      this.stashDraft();
      const req = scratchRequest({ ...entry.request, collectionId: "", folderId: null });
      req.id = `history-${entry.id}`;
      this.request = req;
      this.savedFingerprint = null;
      this.middle = { kind: "request" };
      this.sendBlocked = null;
      this.formatError = null;
      ui.selectedNode = null;
      const view: ResponseView = {
        requestId: req.id,
        response,
        error: entry.error ?? null,
        unresolved: [],
        sentAt: entry.timestamp,
        elapsedMs: null,
        envName: null,
        method: entry.method,
        resolvedUrl: entry.resolvedUrl,
        historyId: entry.id,
        fromHistory: entry.timestamp,
        body: null,
        bodyLimited: false,
        bodyError: null,
        wsSummary: entry.ws ?? null,
        graphql: entry.request.body?.type === "graphql",
        grpc: entry.grpc
          ? {
              service: entry.grpc.service,
              method: entry.grpc.method,
              callKind: entry.grpc.callKind,
              status: entry.grpc.status ?? null,
              response: entry.grpc.response ?? null,
              responseTruncated: !!entry.grpc.responseTruncated,
              headers: entry.grpc.headers ?? [],
              trailers: entry.grpc.trailers ?? [],
              timeMs: entry.grpc.durationMs,
              sizeBytes: null,
              summary: entry.grpc,
            }
          : null,
      };
      this.setResponse(view);
      if (response) void this.fetchBody(view);
    } catch (e) {
      this.reportError(e);
    }
  }

  /** Entries hidden while their delete can still be undone. */
  hiddenHistory = $state.raw<ReadonlySet<string>>(new Set());

  /** Delete with Undo: hidden immediately, committed when the toast expires. */
  deleteHistory(id: string) {
    this.hiddenHistory = new Set([...this.hiddenHistory, id]);
    const unhide = () => {
      const s = new Set(this.hiddenHistory);
      s.delete(id);
      this.hiddenHistory = s;
    };
    const timer = setTimeout(async () => {
      try {
        await call("delete_history_entry", { historyId: id });
        this.history = this.history.filter((h) => h.id !== id);
      } catch (e) {
        this.reportError(e);
      }
      unhide();
    }, 8000);
    toasts.info("History entry deleted", {
      actionLabel: "Undo",
      action: () => {
        clearTimeout(timer);
        unhide();
      },
    });
  }

  async clearHistory() {
    const ok = await ui.confirm({ title: "Clear history?", body: `This deletes all ${this.history.length} history entries. This can't be undone.`, confirmLabel: "Clear history" });
    if (!ok) return;
    await call("clear_history", {});
    this.history = [];
    toasts.success("History cleared");
  }

  async saveHistoryTo(historyId: string, collectionId: string, folderId: string | null, name: string) {
    try {
      const { entry } = await call("open_history_entry", { historyId });
      const created = await call("create_request", { collectionId, folderId, request: { ...entry.request, name } });
      await this.loadTree();
      this.openRequest(created.id, { reveal: true });
      toasts.success(`Saved “${created.name}”`);
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── body tools ─────────────
  /**
   * Format (AC-REQ-6) the body; for GraphQL (AC-GQL-7) the query (prettified), the variables
   * (JSON), or both (`part` omitted, ⌘⇧F). Returns where a parse error is, text untouched.
   */
  async formatBody(part?: "query" | "variables"): Promise<{ line: number; column: number; target: FormatTarget } | null> {
    const b = this.request.body;
    this.formatError = null;
    this.formatNotice = null;
    let target: FormatTarget = "body";
    try {
      if (b.type === "json" || b.type === "xml") {
        b.text = await call("format_body", { kind: b.type as "json" | "xml", text: b.text ?? "" });
      } else if (b.type === "graphql" && b.graphql) {
        const g = b.graphql;
        if (part !== "variables" && g.query.trim()) {
          target = "query";
          const { prettifyQuery, GraphqlSyntaxError } = await import("../graphql/gql");
          try {
            const pretty = prettifyQuery(g.query);
            if (pretty !== g.query) {
              if (hasComments(g.query)) this.formatNotice = { requestId: this.request.id, message: `Comments were removed. ${shortcutText("Mod+Z", this.platform)} to undo.` };
              g.query = pretty;
            }
          } catch (e) {
            if (!(e instanceof GraphqlSyntaxError)) throw e;
            this.formatError = { message: `Invalid GraphQL at line ${e.line}, col ${e.column}: ${e.message}`, line: e.line, column: e.column, target };
            return { line: e.line, column: e.column, target };
          }
        }
        if (part !== "query") {
          target = "variables";
          g.variables = await call("format_body", { kind: "json", text: g.variables || "{}" });
        }
      }
      return null;
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "format_error") {
        const d = err.details as { message: string; line: number; column: number };
        const kind = b.type === "xml" ? "XML" : "JSON";
        this.formatError = { message: `Invalid ${kind} at line ${d.line}, col ${d.column}`, line: d.line, column: d.column, target };
        return { line: d.line, column: d.column, target };
      }
      this.reportError(err);
      return null;
    }
  }

  // ───────────── cURL ─────────────
  async importCurlInPlace(text: string): Promise<boolean> {
    try {
      const res = await call("import_curl", { text });
      this.undoRequest = $state.snapshot(this.request) as Request;
      const keep = { id: this.request.id, collectionId: this.request.collectionId, folderId: this.request.folderId, name: this.request.collectionId ? this.request.name : res.request.name, sortOrder: this.request.sortOrder, createdAt: this.request.createdAt, updatedAt: this.request.updatedAt };
      this.request = { ...res.request, ...keep };
      toasts.info("Imported from cURL", { actionLabel: "Undo", action: () => this.undoCurl() });
      if (res.unknownFlags.length) toasts.warning(`Ignored flags: ${res.unknownFlags.join(", ")}`);
      for (const w of res.warnings) toasts.warning(w);
      return true;
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "not_curl") return false;
      toasts.error(err.message);
      return true;
    }
  }

  undoCurl() {
    if (this.undoRequest) {
      this.request = this.undoRequest;
      this.undoRequest = null;
    }
  }

  async exportCurl(keepVariables: boolean): Promise<string> {
    return call("export_curl", { request: withOperation($state.snapshot(this.request) as Request), projectId: this.activeProjectId, keepVariables });
  }

  /** AC-GRPC-11: a grpcurl command for the open gRPC request. */
  async exportGrpcurl(keepVariables: boolean): Promise<string> {
    return call("export_grpcurl", { request: $state.snapshot(this.request) as Request, projectId: this.activeProjectId, keepVariables });
  }

  async copyCurl() {
    if (this.middle.kind !== "request") return;
    if (this.request.kind === "grpc") {
      try {
        await copyText(await this.exportGrpcurl(this.exportKeepVariables));
        toasts.success("grpcurl command copied to clipboard");
      } catch (e) {
        this.reportError(e);
      }
      return;
    }
    if (this.request.kind === "ws") {
      toasts.info("WebSocket requests have no cURL form");
      return;
    }
    try {
      await copyText(await this.exportCurl(this.exportKeepVariables));
      toasts.success("cURL copied to clipboard");
    } catch (e) {
      this.reportError(e);
    }
  }

  async copyRequestCurl(r: Request) {
    try {
      await copyText(await call("export_curl", { request: withOperation(r), projectId: this.activeProjectId, keepVariables: this.exportKeepVariables }));
      toasts.success("cURL copied to clipboard");
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── tree mutations ─────────────
  async createCollection(name = "New collection") {
    if (!this.activeProjectId) return;
    try {
      const c = await call("create_collection", { projectId: this.activeProjectId, name });
      await this.loadTree();
      ui.sidebarTab = "collections";
      ui.selectedNode = { kind: "collection", id: c.id, collectionId: c.id };
      ui.renaming = { kind: "collection", id: c.id };
      return c;
    } catch (e) {
      this.reportError(e);
    }
  }

  async createFolder(collectionId: string, parentFolderId: string | null, name = "New folder") {
    try {
      const f = await call("create_folder", { collectionId, parentFolderId, name });
      await this.loadTree();
      ui.expandAll([collectionId, ...(parentFolderId ? [parentFolderId] : [])]);
      ui.selectedNode = { kind: "folder", id: f.id, collectionId };
      ui.renaming = { kind: "folder", id: f.id };
    } catch (e) {
      this.reportError(e);
    }
  }

  async createRequestIn(collectionId: string, folderId: string | null) {
    try {
      const r = await call("create_request", { collectionId, folderId });
      await this.loadTree();
      this.openRequest(r.id, { reveal: true });
      ui.renaming = { kind: "request", id: r.id };
    } catch (e) {
      this.reportError(e);
    }
  }

  async rename(kind: "collection" | "folder" | "request" | "environment" | "project", id: string, name: string) {
    const n = name.trim();
    if (!n) return;
    try {
      if (kind === "collection") {
        const c = this.index?.collectionById.get(id);
        if (c) await call("update_collection", { collection: { ...c, name: n } });
        await this.loadTree();
      } else if (kind === "folder") {
        await call("rename_folder", { folderId: id, name: n });
        await this.loadTree();
      } else if (kind === "request") {
        if (this.refuseReadOnly(id)) return;
        await call("rename_request", { requestId: id, name: n });
        if (this.request.id === id) this.request.name = n;
        if (this.drafts[id]) this.drafts = { ...this.drafts, [id]: { ...this.drafts[id], name: n } };
        await this.loadTree();
      } else if (kind === "environment") {
        const env = this.environments.find((e) => e.id === id);
        if (env) await call("update_environment", { environment: { ...env, name: n } });
        await this.loadEnvironments();
      } else {
        await call("rename_project", { projectId: id, name: n });
        this.projects = await call("list_projects", {});
      }
    } catch (e) {
      this.reportError(e);
    }
  }

  async duplicate(kind: "collection" | "folder" | "request", id: string) {
    if (kind === "request" && this.refuseReadOnly(id)) return;
    try {
      if (kind === "collection") this.setTree(await call("duplicate_collection", { collectionId: id }));
      else if (kind === "folder") this.setTree(await call("duplicate_folder", { folderId: id }));
      else {
        const r = await call("duplicate_request", { requestId: id });
        await this.loadTree();
        this.openRequest(r.id, { reveal: true });
      }
      toasts.success("Duplicated");
    } catch (e) {
      this.reportError(e);
    }
  }

  async deleteNode(kind: "collection" | "folder" | "request", id: string) {
    const ix = this.index;
    if (!ix) return;
    const name =
      kind === "collection" ? ix.collectionById.get(id)?.name : kind === "folder" ? ix.folderById.get(id)?.name : ix.requestById.get(id)?.name;
    let body = "This can't be undone.";
    if (kind !== "request") {
      const c = await call("child_counts", { kind, id });
      const parts: string[] = [];
      if (c.folders) parts.push(`${c.folders} folder${c.folders === 1 ? "" : "s"}`);
      if (c.requests) parts.push(`${c.requests} request${c.requests === 1 ? "" : "s"}`);
      if (parts.length) body = `This also deletes ${parts.join(" and ")}.`;
    }
    const ok = await ui.confirm({ title: `Delete “${name ?? ""}”?`, body });
    if (!ok) return;
    // Their WebSocket sessions go too (AC-WS-3): nothing keeps running or reconnecting without a request.
    const doomed = requestsInSubtree(ix, kind, id);
    try {
      const t =
        kind === "collection"
          ? await call("delete_collection", { collectionId: id })
          : kind === "folder"
            ? await call("delete_folder", { folderId: id })
            : await call("delete_request", { requestId: id });
      if (this.middle.kind === "collection" && this.middle.id === id) this.middle = { kind: "request" };
      void ws.discard(doomed);
      this.setTree(t);
      if (ui.selectedNode?.id === id) ui.selectedNode = null;
      toasts.success(`Deleted “${name ?? ""}”`);
    } catch (e) {
      this.reportError(e);
    }
  }

  async moveNode(kind: "collection" | "folder" | "request", id: string, targetCollectionId: string | null, targetFolderId: string | null, index: number) {
    if (kind === "request" && this.refuseReadOnly(id)) return;
    try {
      const t = await call("move_node", { kind, id, targetCollectionId, targetFolderId, index });
      this.setTree(t);
      if (targetCollectionId) ui.expandAll([targetCollectionId, ...(targetFolderId ? [targetFolderId] : [])]);
    } catch (e) {
      this.reportError(e);
    }
  }

  async updateCollection(c: Collection) {
    try {
      const saved = await call("update_collection", { collection: c });
      if (this.tree) this.tree = { ...this.tree, collections: this.tree.collections.map((x) => (x.id === saved.id ? saved : x)) };
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── projects ─────────────
  /** `location`: undefined = default (Personal when signed in), "local", or a space id (UX §5.2). */
  async createProject(name: string, location?: string | null): Promise<boolean> {
    try {
      const t = await call("create_project", { name, location: location ?? null });
      this.projects = await call("list_projects", {});
      await this.loadLocations();
      await this.switchProject(t.project.id);
      ui.renaming = null;
      toasts.success(`Created project “${t.project.name}”`);
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  /**
   * Move to team / move out (04 §6.5): online; on failure nothing moved and this rejects with the
   * CommandError so the dialog can show it. Switches to the moved copy (it has new ids).
   */
  async moveProjectTo(projectId: string, spaceId: string | null): Promise<MoveResult> {
    const res = await call("move_project_to_space", { projectId, spaceId });
    this.projects = await call("list_projects", {});
    this.environments = await call("list_environments", {});
    await this.loadLocations();
    this.onSyncStatus(await call("get_sync_status", {}));
    if (this.activeProjectId === projectId) {
      this.activeProjectId = null;
      await this.switchProject(res.projectId);
    }
    return res;
  }

  /** Undo an unfinished move (what already reached the target is removed there). */
  async cancelMove(projectId: string): Promise<boolean> {
    try {
      await call("move_cancel", { projectId });
      this.onSyncStatus(await call("get_sync_status", {}));
      await this.reloadAll();
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  /** Copy to project… / Duplicate to personal… (UX §6.1): new ids, the source is untouched. */
  async copyNodeTo(kind: NodeKind, id: string, targetProjectId: string, targetCollectionId: string | null): Promise<string | null> {
    try {
      const r = await call("copy_to_project", { kind, id, targetProjectId, targetCollectionId });
      if (r.tree.project.id === this.activeProjectId) this.setTree(r.tree);
      return r.copyId;
    } catch (e) {
      this.reportError(e);
      return null;
    }
  }

  /** A whole project copied to a space (Duplicate to personal…). */
  async duplicateProjectTo(projectId: string, spaceId: string | null, name: string): Promise<string | null> {
    try {
      const t = await call("project_duplicate_to", { projectId, spaceId, name });
      this.projects = await call("list_projects", {});
      this.environments = await call("list_environments", {});
      await this.loadLocations();
      return t.project.id;
    } catch (e) {
      this.reportError(e);
      return null;
    }
  }

  /** The role-changed banner (UX §6.8). */
  async resolveRoleChange(spaceId: string, action: "savePersonal" | "discard"): Promise<boolean> {
    try {
      const r = await call("role_change_resolve", { spaceId, action });
      this.onSyncStatus(r.status);
      await this.reloadAll();
      if (action === "savePersonal") toasts.success(r.copies.length === 1 ? "Saved your changes as a personal copy" : `Saved your changes as ${r.copies.length} personal copies`);
      else toasts.info("Discarded your unsynced changes");
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  /** Sync location… (AC-PROJ-3): a space id, or null for this device only. */
  async setProjectLocation(projectId: string, spaceId: string | null): Promise<boolean> {
    try {
      this.locations = await call("project_set_location", { projectId, spaceId });
      this.onSyncStatus(await call("get_sync_status", {}));
      return true;
    } catch (e) {
      this.reportError(e);
      return false;
    }
  }

  async duplicateProject(id: string) {
    try {
      const t = await call("duplicate_project", { projectId: id });
      this.projects = await call("list_projects", {});
      this.environments = await call("list_environments", {});
      await this.loadLocations();
      await this.switchProject(t.project.id);
      toasts.success(`Duplicated as “${t.project.name}”`);
    } catch (e) {
      this.reportError(e);
    }
  }

  async deleteProject(id: string) {
    const p = this.projects.find((x) => x.id === id);
    if (!p) return;
    const c = await call("child_counts", { kind: "project", id });
    const parts: string[] = [];
    if (c.collections) parts.push(`${c.collections} collection${c.collections === 1 ? "" : "s"}`);
    if (c.folders) parts.push(`${c.folders} folder${c.folders === 1 ? "" : "s"}`);
    if (c.requests) parts.push(`${c.requests} request${c.requests === 1 ? "" : "s"}`);
    const ok = await ui.confirm({
      title: `Delete “${p.name}”?`,
      body: parts.length ? `This also deletes ${parts.join(", ")} and the project's environments.` : "This also deletes the project's environments.",
    });
    if (!ok) return;
    try {
      this.projects = await call("delete_project", { projectId: id });
      this.environments = await call("list_environments", {});
      void this.loadLocations();
      if (this.activeProjectId === id) {
        this.activeProjectId = null;
        await this.switchProject(this.projects[0].id);
      }
      this.pruneWsSessions();
      toasts.success(`Deleted “${p.name}”`);
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── environments ─────────────
  async createEnvironment(name = "New environment") {
    if (!this.activeProjectId) return;
    try {
      const env = await call("create_environment", { projectId: this.activeProjectId, name });
      await this.loadEnvironments();
      this.openEnvironment(env.id);
      ui.renaming = { kind: "environment", id: env.id };
    } catch (e) {
      this.reportError(e);
    }
  }

  async updateEnvironment(env: Environment) {
    try {
      const saved = await call("update_environment", { environment: env });
      this.environments = this.environments.map((e) => (e.id === saved.id ? saved : e));
      await this.loadVariables();
    } catch (e) {
      this.reportError(e);
    }
  }

  async duplicateEnvironment(id: string) {
    try {
      const env = await call("duplicate_environment", { environmentId: id });
      await this.loadEnvironments();
      this.openEnvironment(env.id);
    } catch (e) {
      this.reportError(e);
    }
  }

  async deleteEnvironment(id: string) {
    const env = this.environments.find((e) => e.id === id);
    if (!env) return;
    const ok = await ui.confirm({ title: `Delete “${env.name}”?`, body: `Its ${env.variables.length} variable${env.variables.length === 1 ? "" : "s"} will be removed.` });
    if (!ok) return;
    try {
      this.environments = await call("delete_environment", { environmentId: id });
      this.projects = await call("list_projects", {});
      await this.loadVariables();
      if (this.middle.kind === "environment" && this.middle.id === id) this.middle = { kind: "request" };
      toasts.success(`Deleted “${env.name}”`);
    } catch (e) {
      this.reportError(e);
    }
  }

  // ───────────── misc ─────────────
  reportError(e: unknown) {
    const err: CommandError = toCommandError(e);
    // AC-FWD-3: a refusal to touch a request from a newer ReqHarbor is an explanation, not a failure;
    // so is a viewer's refused edit (AC-ROLE-1).
    if (err.code === "read_only_newer" || err.code === "read_only_role") toasts.info(err.message);
    else toasts.error(err.message);
  }
}

export const app = new AppStore();

/** Decode response bytes into text for the viewer. */
export function decodeBody(bytes: Uint8Array): string {
  return new TextDecoder().decode(bytes);
}

export function prettyBody(kind: BodyKind, text: string): string | null {
  if (kind === "json") return prettyJson(text);
  return null;
}

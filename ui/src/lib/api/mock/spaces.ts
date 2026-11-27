/**
 * Mock of personal sync and bindings (04 §6, §7; 06-go-server §9), mirroring
 * crates/core/src/{spaces,space_sync}.rs closely enough for the UI:
 * the sign-in migration, the first-sync checklist, Sync location…, the
 * legacy merge, foreign-account pauses and their Keep/Remove notice.
 *
 * Query-string switches (read by `getMockServer`):
 * - `?mockSpace=quota|server_full|skew|client_too_old|unavailable` — an issue on the personal space;
 *   `removed` — with a team: "Acme" was removed by an admin (the removal notice opens);
 *   `roleChanged` — with a team: the role dropped to viewer with 2 unsynced changes (the banner).
 * - `?mockTeam=1` — a bound team "Acme" holding the "Side project" project; `?mockRole=` sets my role
 *   there (default editor; implies `mockTeam`).
 * - `?mockLegacy=1` — token sync is connected at launch (so a sign-in migrates it).
 * - `?mockForeign=1` — with `mockAccount=signedIn`: the bindings belong to another account
 *   (sam@acme.dev), so the different-account notice opens.
 */
import type {
  Account,
  FirstSyncCandidate,
  LegacyStatus,
  Project,
  ProjectLocation,
  SpaceIssue,
  SpaceKind,
  SpaceRole,
  SpaceStateName,
  SpaceStatus,
  SyncStatus,
} from "../types";
import { fail } from "./errors";

export type MockSpaceMode = "quota" | "server_full" | "skew" | "client_too_old" | "unavailable" | "removed" | "roleChanged";

export const PERSONAL_ID = "01JSPACEPERSONAL000000000";
export const TEAM_ID = "01JSPACEACME0000000000000";
/** The team behind TEAM_ID. */
export const ACME_TEAM_ID = "01JTEAMACME00000000000000";
export const LEGACY = "legacy";

export interface MockSpacesOptions {
  mode?: MockSpaceMode;
  team?: boolean;
  /** My role in Acme (default editor). */
  role?: SpaceRole;
  foreign?: boolean;
}

interface Binding {
  spaceId: string;
  kind: Exclude<SpaceKind, "legacy">;
  name: string;
  role: SpaceRole | null;
  state: "active" | "paused" | "unavailable" | "removed";
  reason: string | null;
  serverUrl: string;
  accountEmail: string;
  issue: SpaceIssue | null;
  lastSyncAt: number | null;
  bytesUsed: number | null;
  quotaBytes: number | null;
  /** × on the notice ("decide later"): Keep applies after 7 days (UX §6.9). */
  decideLaterAt?: number | null;
  teamId?: string | null;
  memberCount?: number | null;
  /** Changes waiting for the role-changed banner (UX §6.8). */
  frozen?: number;
}

/** How long a space notice closed with × waits before Keep is applied. */
export const DECIDE_LATER_MS = 7 * 86_400_000;

export interface SpacesHost {
  projects(): Project[];
  counts(projectId: string): { collections: number; requests: number };
  /** Remove a project and everything in it from this device (no tombstones). */
  forget(projectId: string): void;
  rename(projectId: string, name: string): void;
  now(): number;
}

const ISSUES: Record<MockSpaceMode, Omit<SpaceIssue, "at">> = {
  quota: { code: "quota", message: "Personal storage full (100 MB)." },
  server_full: { code: "serverFull", message: "The sync server is out of space. Your changes are saved on this device." },
  skew: { code: "clockSkew", message: "Your computer's clock is ahead by about 7 minutes. Changes can't sync until it's fixed." },
  client_too_old: { code: "clientTooOld", message: "Update ReqHarbor to keep syncing." },
  unavailable: { code: "unavailable", message: "Temporarily unavailable · Retrying" },
  removed: { code: "unavailable", message: "" },
  roleChanged: { code: "roleChanged", message: "" },
};

const PERSONAL_MODES: MockSpaceMode[] = ["quota", "server_full", "skew", "client_too_old", "unavailable"];

export class MockSpaces {
  accountMode = false;
  bindings: Binding[] = [];
  projectSpaces = new Map<string, string>();
  globalSpace: string | null = null;
  /** The Global was kept on this device ("Keep all", or Keep for another account): never auto-bound again. */
  globalLocal = false;
  legacy: { workerUrl: string; remoteWorkspaceId: string; mode: "sync" | "sharesOnly" } | null = null;
  firstSyncPending = false;
  account: Account | null = null;
  private host: SpacesHost;
  private opts: MockSpacesOptions;

  constructor(host: SpacesHost, opts: MockSpacesOptions = {}) {
    this.host = host;
    this.opts = opts;
  }

  private belongs(b: Binding): boolean {
    return !!this.account && b.serverUrl === this.account.serverUrl && b.accountEmail === this.account.email;
  }

  personal(): Binding | undefined {
    return this.bindings.find((b) => b.kind === "personal" && this.belongs(b));
  }

  private isFresh(): boolean {
    return this.host.projects().every((p) => p.name === "Default" && this.host.counts(p.id).collections === 0);
  }

  /**
   * After a sign-in (the core's `sign_in_migrate` then the first `/v1/me/spaces`). With
   * `restore`, the device was set up earlier: every project is already in Personal. Returns
   * whether this sign-in found a fresh device (the "Signed in · syncing N projects…" toast).
   */
  signIn(account: Account, legacySync: { workerUrl: string; remoteWorkspaceId: string } | null, restore = false): boolean {
    this.account = account;
    let fresh = false;
    if (!this.accountMode) {
      this.accountMode = true;
      if (legacySync) {
        this.legacy = { ...legacySync, mode: "sync" };
        for (const p of this.host.projects()) this.projectSpaces.set(p.id, LEGACY);
        this.globalSpace = LEGACY;
      } else if (restore) {
        // bound below, once Personal exists
      } else {
        fresh = this.isFresh();
        this.firstSyncPending = !fresh;
      }
    }
    // Pause bindings of another server or account (06 §9); resume this account's (A → B → A).
    for (const b of this.bindings) {
      if (this.belongs(b)) {
        if (b.state === "paused" && b.reason === "foreignAccount") {
          b.state = "active";
          b.reason = null;
          b.decideLaterAt = null;
        }
      } else if (b.state !== "paused") {
        b.state = "paused";
        b.reason = "foreignAccount";
        b.decideLaterAt = null;
      }
    }
    if (!this.personal()) {
      this.bindings.push({
        spaceId: this.bindings.some((b) => b.spaceId === PERSONAL_ID) ? `${PERSONAL_ID.slice(0, -1)}2` : PERSONAL_ID,
        kind: "personal",
        name: "Personal",
        role: "owner",
        state: "active",
        reason: null,
        serverUrl: account.serverUrl,
        accountEmail: account.email,
        issue: this.opts.mode && PERSONAL_MODES.includes(this.opts.mode) ? { ...ISSUES[this.opts.mode], at: this.host.now() } : null,
        lastSyncAt: this.host.now(),
        bytesUsed: 12 * 1024 * 1024,
        quotaBytes: 100 * 1024 * 1024,
      });
    }
    const personal = this.personal()!.spaceId;
    if (restore && !legacySync) {
      for (const p of this.host.projects()) if (!this.projectSpaces.has(p.id)) this.projectSpaces.set(p.id, personal);
    }
    if ((this.opts.team || this.opts.role) && !this.bindings.some((b) => b.spaceId === TEAM_ID)) {
      const role = this.opts.mode === "roleChanged" ? "viewer" : (this.opts.role ?? "editor");
      this.addTeam(TEAM_ID, ACME_TEAM_ID, "Acme", role, 4);
      const side = this.host.projects().find((p) => p.name === "Side project");
      if (side) this.projectSpaces.set(side.id, TEAM_ID);
      if (this.opts.mode === "removed") this.removeTeam(TEAM_ID, "removed");
      if (this.opts.mode === "roleChanged") {
        const b = this.bindings.find((x) => x.spaceId === TEAM_ID)!;
        b.frozen = 2;
        b.issue = { code: "roleChanged", message: "Your role in Acme changed to Viewer. You have 2 unsynced changes.", at: this.host.now() };
      }
    }
    // Nothing uploads before the checklist is answered (AC-ONB-5), nor a Global kept on this device.
    if (this.globalSpace === null && !this.firstSyncPending && !this.globalLocal) this.globalSpace = personal;
    // A fresh device's untouched Default syncs to Personal (UX §10).
    if (!this.firstSyncPending) {
      for (const p of this.host.projects()) {
        if (p.name === "Default" && this.host.counts(p.id).collections === 0 && !this.projectSpaces.has(p.id)) this.projectSpaces.set(p.id, personal);
      }
    }
    return fresh;
  }

  /** The fresh-device toast (UX §10), as the core's `fresh_device_notice`. */
  freshNotice(): string {
    const mine = this.bindings.filter((b) => this.belongs(b) && b.state === "active");
    const projects = [...this.projectSpaces.values()].filter((s) => mine.some((b) => b.spaceId === s)).length;
    const teams = mine.filter((b) => b.kind === "team").length;
    const n = (k: number, w: string) => `${k} ${w}${k === 1 ? "" : "s"}`;
    return `Signed in · syncing ${n(projects, "project")} from your account${teams ? ` and ${n(teams, "team")}` : ""}.`;
  }

  /** `?mockForeign=1`: what this device synced belongs to another account. */
  makeForeign(): void {
    for (const b of this.bindings) {
      b.accountEmail = "sam@acme.dev";
      b.state = "paused";
      b.reason = "foreignAccount";
    }
    if (this.account) this.signIn(this.account, null);
  }

  signOut(): void {
    this.account = null;
  }

  /** Bind a team space of the signed-in account (what `/v1/me/spaces` does after create or join). */
  addTeam(spaceId: string, teamId: string, name: string, role: SpaceRole, memberCount: number): void {
    const existing = this.bindings.find((b) => b.spaceId === spaceId);
    if (existing) {
      Object.assign(existing, { name, role, memberCount, state: "active", reason: null, teamId });
      return;
    }
    this.bindings.push({
      spaceId,
      kind: "team",
      name,
      role,
      state: "active",
      reason: null,
      serverUrl: this.account?.serverUrl ?? "",
      accountEmail: this.account?.email ?? "",
      issue: null,
      lastSyncAt: this.host.now(),
      bytesUsed: 8 * 1024 * 1024,
      quotaBytes: 250 * 1024 * 1024,
      teamId,
      memberCount,
      frozen: 0,
    });
  }

  /** A role change seen by a sync (04 §6.8): dropping below editor freezes unsynced changes. */
  setRole(spaceId: string, role: SpaceRole, unsynced = 0): void {
    const b = this.bindings.find((x) => x.spaceId === spaceId);
    if (!b) return;
    const wasWritable = this.writable(spaceId);
    b.role = role;
    if (wasWritable && !this.writable(spaceId)) {
      b.frozen = (b.frozen ?? 0) + unsynced;
      const n = b.frozen;
      const label = role.charAt(0).toUpperCase() + role.slice(1);
      b.issue = { code: "roleChanged", message: `Your role in ${b.name} changed to ${label}.${n ? ` You have ${n} unsynced change${n === 1 ? "" : "s"}.` : ""}`, at: this.host.now() };
    }
  }

  /** An authoritative `/v1/me/spaces` without the team: removed, left or deleted (UX §6.9). */
  removeTeam(spaceId: string, reason: "removed" | "left" | "deleted"): void {
    const b = this.bindings.find((x) => x.spaceId === spaceId);
    if (!b) return;
    b.state = "removed";
    b.reason = reason;
  }

  /** Stop syncing a space here (leave with or without a copy, delete): no notice. */
  detach(spaceId: string, keep: boolean): string[] {
    return this.bindings.some((b) => b.spaceId === spaceId) ? this.resolve(spaceId, keep) : [];
  }

  /** The role-changed banner: Save as personal copy (the caller copies) or Discard. */
  unfreeze(spaceId: string): void {
    const b = this.bindings.find((x) => x.spaceId === spaceId);
    if (!b) return;
    b.frozen = 0;
    if (b.issue?.code === "roleChanged" || b.issue?.code === "heldDelete") b.issue = null;
  }

  roleOf(projectId: string): SpaceRole | null {
    const s = this.projectSpaces.get(projectId);
    return this.bindings.find((x) => x.spaceId === s)?.role ?? null;
  }

  kindOf(projectId: string): "personal" | "team" | "legacy" | "local" {
    const s = this.projectSpaces.get(projectId);
    if (!this.accountMode || !s) return "local";
    if (s === LEGACY) return "legacy";
    return this.bindings.find((x) => x.spaceId === s)?.kind ?? "local";
  }

  /** Deleting a team project needs admin (AC-ROLE-2). */
  deleteGuard(projectId: string): void {
    this.guard(projectId);
    const s = this.projectSpaces.get(projectId);
    const b = this.bindings.find((x) => x.spaceId === s);
    if (b?.kind === "team" && !["admin", "owner"].includes(b.role ?? "")) fail("read_only_role", "Only admins can delete team projects.");
  }

  /** A space a project may be created in or copied to (null: this device only). */
  spaceGuard(spaceId: string | null): void {
    if (!spaceId || spaceId === LEGACY || !this.accountMode) return;
    const b = this.bindings.find((x) => x.spaceId === spaceId) ?? fail("not_found", `Space ${spaceId} not found`);
    if (!this.writable(spaceId)) fail("read_only_role", `Viewers can't add projects to ${b.name}.`);
  }

  /** Bind a project (a moved or duplicated copy) to a space; null = this device only. */
  bind(projectId: string, spaceId: string | null): void {
    if (spaceId) this.projectSpaces.set(projectId, spaceId);
    else this.projectSpaces.delete(projectId);
  }

  binding(spaceId: string): Binding | undefined {
    return this.bindings.find((b) => b.spaceId === spaceId);
  }

  /** New projects: Personal when signed in, legacy with only token sync, else local (04 §6.1). */
  defaultLocation(): string | null {
    if (!this.accountMode) return null;
    const p = this.personal();
    if (p && p.state === "active") return p.spaceId;
    if (!this.account && this.legacy?.mode === "sync") return LEGACY;
    return null;
  }

  onCreate(projectId: string, location: string | null | undefined): void {
    if (!this.accountMode) return;
    const space = location === "local" ? null : location ? location : this.defaultLocation();
    this.spaceGuard(space);
    if (space) this.projectSpaces.set(projectId, space);
  }

  onDuplicate(srcId: string, copyId: string): void {
    if (!this.accountMode) return;
    const same = this.projectSpaces.get(srcId);
    const space = same && this.writable(same) ? same : this.defaultLocation();
    if (space) this.projectSpaces.set(copyId, space);
  }

  onDelete(projectId: string): void {
    this.projectSpaces.delete(projectId);
  }

  writable(spaceId: string): boolean {
    const b = this.bindings.find((x) => x.spaceId === spaceId);
    return !b?.role || ["editor", "admin", "owner"].includes(b.role);
  }

  /** The viewer guard (04 §6.8). */
  guard(projectId: string): void {
    const s = this.projectSpaces.get(projectId);
    if (s && !this.writable(s)) fail("read_only_role", "Viewers can't change team projects. Duplicate to personal to experiment.");
  }

  location(projectId: string, legacyConnected: boolean): ProjectLocation {
    const local: ProjectLocation = { projectId, spaceId: null, kind: "local", label: "This device only", role: null, state: null };
    if (!this.accountMode) return legacyConnected ? { ...local, spaceId: LEGACY, kind: "legacy", label: "Legacy workspace", state: "active" } : local;
    const space = this.projectSpaces.get(projectId);
    if (!space) return local;
    if (space === LEGACY) return { ...local, spaceId: LEGACY, kind: "legacy", label: "Legacy workspace", state: this.legacy?.mode === "sync" ? "active" : "paused" };
    const b = this.bindings.find((x) => x.spaceId === space);
    if (!b) return local;
    const state: SpaceStateName = !this.account ? "signedOut" : !this.belongs(b) ? "paused" : b.state;
    return { projectId, spaceId: b.spaceId, kind: b.kind, label: b.name, role: b.role, state };
  }

  locations(legacyConnected: boolean): ProjectLocation[] {
    return [...this.host.projects()].sort((a, b) => a.sortOrder - b.sortOrder).map((p) => this.location(p.id, legacyConnected));
  }

  setLocation(projectId: string, spaceId: string | null): void {
    if (!this.accountMode) fail("invalid", "Sign in to choose where a project syncs.");
    const current = this.projectSpaces.get(projectId) ?? null;
    if (current === spaceId) return;
    // B6: only the merge moves a token-synced project to Personal (same ids, token sync stops).
    if (current === LEGACY && spaceId) fail("invalid", "This project syncs with a token. Use Merge into my personal space… in Settings › Sync to move it.");
    const from = this.bindings.find((b) => b.spaceId === current);
    if (from?.kind === "team") fail("invalid", "A project leaves a team with Move… (new ids), or use Copy to project…");
    if (spaceId) {
      const to = this.bindings.find((b) => b.spaceId === spaceId) ?? fail("not_found", `Space ${spaceId} not found`);
      if (to.kind !== "personal") fail("invalid", "Use Move to team… to move a project into a team (new ids).");
      this.projectSpaces.set(projectId, spaceId);
    } else this.projectSpaces.delete(projectId);
  }

  candidates(): FirstSyncCandidate[] {
    if (!this.accountMode || !this.firstSyncPending) return [];
    return [...this.host.projects()]
      .sort((a, b) => a.sortOrder - b.sortOrder)
      .filter((p) => !this.projectSpaces.has(p.id))
      .map((p) => ({ projectId: p.id, name: p.name, ...this.host.counts(p.id) }));
  }

  applyFirstSync(ids: string[]): number {
    const personal = this.personal() ?? fail("invalid", "Your personal space isn't available yet. Try again.");
    let n = 0;
    for (const id of ids) {
      if (this.host.projects().some((p) => p.id === id) && !this.projectSpaces.has(id)) {
        this.projectSpaces.set(id, personal.spaceId);
        n++;
      }
    }
    // The Global goes with checked projects; "Keep all" (or ×) keeps it here too.
    if (this.globalSpace === null) {
      if (n > 0) this.globalSpace = personal.spaceId;
      else this.globalLocal = true;
    }
    this.firstSyncPending = false;
    return n;
  }

  /** SpaceNoticeDialog: keep as local-only copies, or remove from this device. */
  resolve(spaceId: string, keep: boolean): string[] {
    const b = this.bindings.find((x) => x.spaceId === spaceId) ?? fail("not_found", `Space ${spaceId} not found`);
    const projects = [...this.projectSpaces].filter(([, s]) => s === spaceId).map(([p]) => p);
    for (const pid of projects) {
      this.projectSpaces.delete(pid);
      if (keep) {
        if (b.kind === "team") {
          const p = this.host.projects().find((x) => x.id === pid);
          if (p) this.host.rename(pid, `${p.name} (from ${b.name})`);
        }
      } else if (this.host.projects().length > 1) this.host.forget(pid);
    }
    // Never hand one account's Global to another (06 §9): it stays on this device.
    if (this.globalSpace === spaceId) {
      this.globalSpace = null;
      this.globalLocal = true;
    }
    this.bindings = this.bindings.filter((x) => x.spaceId !== spaceId);
    return projects;
  }

  /** × on the notice: remember the first time; Keep applies after 7 days. */
  later(spaceIds: string[], now: number): void {
    for (const b of this.bindings) {
      if (spaceIds.includes(b.spaceId) && b.decideLaterAt == null && this.needsNotice(b)) b.decideLaterAt = now;
    }
  }

  private needsNotice(b: Binding): boolean {
    return b.state === "removed" || (b.state === "paused" && b.reason === "foreignAccount");
  }

  /** Keep every notice left undecided for 7 days (only while signed in). */
  applyOverdue(now: number): string[] {
    if (!this.account) return [];
    const due = this.bindings.filter((b) => this.needsNotice(b) && b.decideLaterAt != null && now - b.decideLaterAt >= DECIDE_LATER_MS).map((b) => b.spaceId);
    for (const id of due) this.resolve(id, true);
    return due;
  }

  merge(): number {
    const personal = this.personal() ?? fail("invalid", "Your personal space isn't available yet. Try again.");
    if (!this.legacy) return fail("invalid", "No token-synced workspace to merge.");
    this.legacy.mode = "sharesOnly";
    let n = 0;
    for (const [pid, s] of this.projectSpaces) {
      if (s === LEGACY) {
        this.projectSpaces.set(pid, personal.spaceId);
        n++;
      }
    }
    this.globalSpace = personal.spaceId;
    return n;
  }

  disconnectLegacy(): void {
    this.legacy = null;
    for (const [pid, s] of [...this.projectSpaces]) if (s === LEGACY) this.projectSpaces.delete(pid);
    if (this.globalSpace === LEGACY) {
      this.globalSpace = null;
      this.globalLocal = true;
    }
  }

  /** A sync run: every active space of this account is synced now. */
  synced(now: number): void {
    this.applyOverdue(now);
    for (const b of this.bindings) {
      if (this.account && this.belongs(b) && b.state === "active") {
        b.lastSyncAt = now;
        if (b.issue?.code === "clockFixed") b.issue = null;
      }
    }
  }

  syncable(): boolean {
    return (!!this.account && !!this.personal()) || this.legacy?.mode === "sync";
  }

  spaceStatuses(pending: number): SpaceStatus[] {
    const count = (id: string) => [...this.projectSpaces.values()].filter((s) => s === id).length;
    const out: SpaceStatus[] = [...this.bindings]
      .sort((a, b) => Number(a.kind !== "personal") - Number(b.kind !== "personal") || a.name.localeCompare(b.name))
      .map((b) => {
        const state: SpaceStateName = !this.account ? "signedOut" : !this.belongs(b) ? "paused" : b.state;
        return {
          spaceId: b.spaceId,
          kind: b.kind,
          name: b.name,
          role: b.role,
          state,
          reason: state === "paused" ? (b.reason ?? "foreignAccount") : null,
          issue: b.issue,
          lastSyncAt: b.lastSyncAt,
          pendingChanges: state === "active" && b.kind === "personal" ? pending : 0,
          projectCount: count(b.spaceId),
          bytesUsed: b.bytesUsed,
          quotaBytes: b.quotaBytes,
          unsupported: 0,
          accountEmail: b.accountEmail,
          teamId: b.teamId ?? null,
          memberCount: b.memberCount ?? null,
          frozen: b.frozen ?? 0,
          held: [],
        };
      });
    if (this.legacy) {
      out.push({
        spaceId: LEGACY,
        kind: "legacy",
        name: "Legacy",
        role: null,
        state: this.legacy.mode === "sharesOnly" ? "sharesOnly" : "active",
        reason: null,
        issue: null,
        lastSyncAt: null,
        pendingChanges: 0,
        projectCount: count(LEGACY),
        bytesUsed: null,
        quotaBytes: null,
        unsupported: 0,
        accountEmail: "",
        teamId: null,
        memberCount: null,
        frozen: 0,
        held: [],
      });
    }
    return out;
  }

  legacyStatus(): LegacyStatus | null {
    if (!this.legacy) return null;
    return { ...this.legacy, projectCount: [...this.projectSpaces.values()].filter((s) => s === LEGACY).length, tokenMissing: false };
  }

  /** Fill the account-mode part of a sync status. */
  apply(sync: SyncStatus): SyncStatus {
    if (!this.accountMode) return { ...sync, accountMode: false, spaces: [], legacy: null, firstSyncPending: false };
    const connected = this.syncable() || sync.connected;
    const spaces = this.spaceStatuses(sync.pendingChanges);
    let state = sync.state;
    if (!connected && (state === "idle" || state === "disconnected")) state = "paused";
    else if (connected && (state === "paused" || state === "disconnected")) state = "idle";
    const last = spaces.map((s) => s.lastSyncAt ?? 0).reduce((a, b) => Math.max(a, b), 0) || null;
    return {
      ...sync,
      state,
      connected,
      workerUrl: this.legacy?.workerUrl ?? sync.workerUrl,
      remoteWorkspaceId: this.legacy?.remoteWorkspaceId ?? sync.remoteWorkspaceId,
      lastSyncAt: last ?? sync.lastSyncAt,
      accountMode: true,
      spaces,
      legacy: this.legacyStatus(),
      firstSyncPending: this.firstSyncPending && this.candidates().length > 0,
    };
  }
}

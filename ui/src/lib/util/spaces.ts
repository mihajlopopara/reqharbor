/**
 * Personal sync in the UI (UX §5.1, §6.6, §6.7, §11): the grouped project switcher, binding
 * icons and tooltips, and the per-space status lines. Pure, so it is unit-tested.
 */
import type { Project, ProjectLocation, SpaceRole, SpaceStatus, SyncStatus } from "../api/types";
import { relativeTime } from "./format";

export interface ProjectGroup {
  /** Stable key, also the `project-group-<key>` test id. */
  key: string;
  kind: "personal" | "team" | "local" | "legacy";
  spaceId: string | null;
  /** Upper-cased by the menu ("PERSONAL", "ACME", "ON THIS DEVICE ONLY", "LEGACY WORKSPACE"). */
  heading: string;
  /** Right-aligned heading text: the team role ("Editor"). */
  trailing?: string;
  projects: Project[];
  /** Teams always render, with a disabled "No projects yet" row. */
  emptyRow: boolean;
}

export function roleLabel(role: SpaceRole | null): string {
  switch (role) {
    case "owner":
      return "Owner";
    case "admin":
      return "Admin";
    case "editor":
      return "Editor";
    case "viewer":
      return "Viewer";
    default:
      return "";
  }
}

/** " · Paused: signed out" / " · Paused" for a space that isn't syncing (UX §4.3). */
function pausedSuffix(sp: SpaceStatus | undefined): string {
  if (!sp) return "";
  if (sp.state === "signedOut") return " · Paused: signed out";
  if (sp.state === "paused") return sp.reason === "foreignAccount" && sp.accountEmail ? ` · ${sp.accountEmail} · Paused` : " · Paused";
  if (sp.state === "removed") return " · Removed";
  return "";
}

/**
 * The switcher groups (UX §5.1): PERSONAL, each team, ON THIS DEVICE ONLY, LEGACY WORKSPACE.
 * Groups render only with projects, except teams (always, with "No projects yet") and "On this
 * device only" while signed in. `projects` keeps its order inside each group.
 */
export function groupProjects(projects: Project[], locations: ProjectLocation[], spaces: SpaceStatus[], signedIn: boolean): ProjectGroup[] {
  const loc = new Map(locations.map((l) => [l.projectId, l]));
  const bySpace = new Map<string, Project[]>();
  const local: Project[] = [];
  const legacy: Project[] = [];
  for (const p of projects) {
    const l = loc.get(p.id);
    if (!l || l.kind === "local" || !l.spaceId) local.push(p);
    else if (l.kind === "legacy") legacy.push(p);
    else bySpace.set(l.spaceId, [...(bySpace.get(l.spaceId) ?? []), p]);
  }
  const groups: ProjectGroup[] = [];
  const known = new Set<string>();
  for (const sp of spaces) {
    if (sp.kind === "legacy") continue;
    known.add(sp.spaceId);
    const list = bySpace.get(sp.spaceId) ?? [];
    if (sp.kind === "personal" && list.length === 0) continue;
    groups.push({
      key: sp.spaceId,
      kind: sp.kind,
      spaceId: sp.spaceId,
      heading: (sp.kind === "personal" ? "Personal" : sp.name) + pausedSuffix(sp),
      trailing: sp.kind === "team" ? roleLabel(sp.role) || undefined : undefined,
      projects: list,
      emptyRow: sp.kind === "team" && list.length === 0,
    });
  }
  // A binding the status doesn't list (shouldn't happen): keep its projects visible.
  for (const [spaceId, list] of bySpace) {
    if (!known.has(spaceId)) local.push(...list);
  }
  if (local.length || signedIn) groups.push({ key: "local", kind: "local", spaceId: null, heading: "On this device only", projects: local, emptyRow: false });
  if (legacy.length) groups.push({ key: "legacy", kind: "legacy", spaceId: "legacy", heading: "Legacy workspace", projects: legacy, emptyRow: false });
  return groups;
}

/** The binding icon of a project (UX §5.1): `layers` when nothing is bound (zero change signed out). */
export function bindingIcon(l: ProjectLocation | undefined, accountMode: boolean): "layers" | "cloud" | "cloud-off" | "users" | "device" | "key" {
  if (!accountMode || !l) return "layers";
  switch (l.kind) {
    case "local":
      return "device";
    case "legacy":
      return "key";
    case "team":
      return l.state === "active" ? "users" : "cloud-off";
    default:
      return l.state === "active" ? "cloud" : "cloud-off";
  }
}

/** "Payments API · Acme · Editor · Synced" / "Scratch · This device only" (UX §5.1). */
export function locationTooltip(name: string, l: ProjectLocation | undefined, sp: SpaceStatus | undefined): string {
  if (!l || l.kind === "local") return `${name} · This device only`;
  const parts = [name, l.kind === "legacy" ? "Legacy workspace" : l.label];
  const role = roleLabel(l.role);
  if (l.kind === "team" && role) parts.push(role);
  parts.push(sp ? spaceState(sp) : l.state === "active" ? "Synced" : "Paused");
  return parts.join(" · ");
}

/** The state part of a space line (UX §6.6, §6.7). */
export function spaceState(sp: SpaceStatus): string {
  if (sp.issue) return sp.issue.message;
  switch (sp.state) {
    case "signedOut":
      return "Paused: signed out";
    case "paused":
      return sp.reason === "foreignAccount" ? "Paused: another account" : "Paused";
    case "unavailable":
      return "Temporarily unavailable · Retrying";
    case "removed":
      return "Removed · Decide";
    case "sharesOnly":
      return "Shares only · syncing stopped";
    default:
      if (sp.unsupported > 0) return `${sp.unsupported} item${sp.unsupported === 1 ? "" : "s"} waiting for a server update`;
      return sp.pendingChanges > 0 ? `${sp.pendingChanges} pending` : "Synced";
  }
}

/** "Personal · Synced" (one tooltip line per space). */
export function spaceLine(sp: SpaceStatus): string {
  return `${sp.name} · ${spaceState(sp)}`;
}

/**
 * The footer chip's tooltip and aria-label (UX §6.6): the overall line, then one line per space
 * in account mode.
 */
export function chipTooltip(sync: SyncStatus, now: number, retryIn: number | null): string {
  let head: string;
  switch (sync.state) {
    case "offline":
      head = retryIn !== null ? `Retrying in ${retryIn} s — click to sync now` : "Offline — click to sync now";
      break;
    case "signInRequired":
      head = "Your session ended. Sign in again to keep syncing.";
      break;
    case "unauthorized":
    case "error":
      head = sync.lastError ?? "Sync is off";
      break;
    case "paused":
      head = "Sync is paused";
      break;
    default:
      head = sync.lastSyncAt ? `Synced ${relativeTime(sync.lastSyncAt, now)} — click to sync now` : "Click to sync now";
  }
  if (!sync.accountMode || !sync.spaces.length) return head;
  return [head, ...sync.spaces.map(spaceLine)].join("\n");
}

/** Spaces with a problem worth the separate warning icon left of the chip (UX §6.6). */
export function spacesWithIssues(sync: SyncStatus): SpaceStatus[] {
  return sync.spaces.filter((s) => s.issue && s.issue.code !== "clockFixed");
}

/** Spaces paused for another account: the different-account notice (UX §4.3). */
export function foreignSpaces(sync: SyncStatus): SpaceStatus[] {
  return sync.spaces.filter((s) => s.state === "paused" && s.reason === "foreignAccount");
}

/** Spaces removed by an authoritative `/v1/me/spaces`: the removal notice (UX §6.9). */
export function removedSpaces(sync: SyncStatus): SpaceStatus[] {
  return sync.spaces.filter((s) => s.state === "removed");
}

/** "12.0 MB of 25 MB" usage text, or null when unknown. */
export function usageText(sp: SpaceStatus, format: (n: number) => string): string | null {
  if (sp.bytesUsed == null) return null;
  return sp.quotaBytes ? `${format(sp.bytesUsed)} of ${format(sp.quotaBytes)}` : format(sp.bytesUsed);
}

/**
 * Whether a project's location can share right now (UX §12.3, 06 §9): it syncs somewhere and that
 * binding is active. Paused (another account), signed-out, removed and unavailable can't.
 */
export function locationCanShare(l: ProjectLocation | undefined): boolean {
  return !!l?.spaceId && l.state === "active";
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

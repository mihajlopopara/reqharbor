/**
 * Teams in the UI (UX §6–§9, 04 §5.4): who may do what, and the copy for team rows, invites,
 * attribution and shares. Pure, so it is unit-tested; the server enforces the same rules.
 */
import type { AuthStatus, EditedBy, InviteInfo, ProjectLocation, SpaceRole, TeamMember, UserRef } from "../api/types";
import { dateTime } from "./format";

export const ROLES = ["owner", "admin", "editor", "viewer"] as const;
const RANK: Record<string, number> = { owner: 4, admin: 3, editor: 2, viewer: 1 };
export const rank = (r: string | null | undefined): number => RANK[r ?? ""] ?? 0;

export function roleName(r: string | null | undefined): string {
  return r ? r.charAt(0).toUpperCase() + r.slice(1) : "";
}

/** Teams are offered only when the account's server advertises `features.teams` (06 §10). */
export function teamsAvailable(auth: AuthStatus): boolean {
  return auth.enabled && !!auth.account && !auth.keychainUnreadable && !!auth.server?.info?.features?.teams;
}

export interface MemberContext {
  /** My role in the team. */
  actor: string;
  /** My user id. */
  me: string;
  /** The team's billing owner. */
  ownerUserId: string | null;
  /** How many owners the team has. */
  owners: number;
}

/** Why a member's role can't be changed by me, or null when it can (04 §5.4, UX §7.2). */
export function roleLockReason(m: TeamMember, ctx: MemberContext): string | null {
  if (m.userId === ctx.ownerUserId) return "Transfer ownership first";
  if (m.role === "owner" && ctx.owners <= 1) return "A team needs at least one owner";
  if (ctx.actor === "owner") return null;
  if (ctx.actor === "admin" && rank(m.role) < 3) return null;
  if (m.userId === ctx.me && rank(m.role) > 1) return null; // anyone may lower themselves
  return ctx.actor === "admin" ? "Admins can change only editors and viewers" : "Only admins can change roles";
}

/** The role options of the select, with options above my power disabled (UX §7.2). */
export function roleOptions(m: TeamMember, ctx: MemberContext): { value: string; disabled: boolean; reason: string | null }[] {
  return ROLES.map((r) => {
    let reason: string | null = null;
    if (ctx.actor === "owner") reason = null;
    else if (m.userId === ctx.me && ctx.actor !== "admin") reason = rank(r) < rank(m.role) || r === m.role ? null : "You can only lower your own role";
    else if (r === "owner") reason = "Only owners can grant owner";
    else if (ctx.actor === "admin" && rank(r) > 3) reason = "Only owners can grant owner";
    return { value: r, disabled: !!reason, reason };
  });
}

/** Why I can't remove this member, or null (UX §7.2–§7.3). */
export function removeLockReason(m: TeamMember, ctx: MemberContext): string | null {
  if (m.userId === ctx.me) return "Use Leave team… instead";
  if (m.userId === ctx.ownerUserId) return "Transfer ownership first";
  if (m.role === "owner" && ctx.owners <= 1) return "A team needs at least one owner";
  if (ctx.actor === "owner") return null;
  if (ctx.actor === "admin" && rank(m.role) < 3) return null;
  return "Only admins can remove members";
}

/** Why I can't leave, or null (the billing owner and the last owner can't). */
export function leaveLockReason(ctx: MemberContext): string | null {
  if (ctx.me === ctx.ownerUserId) return "You're the team's owner. Transfer ownership or delete the team first.";
  if (ctx.actor === "owner" && ctx.owners <= 1) return "You're the last owner. Transfer ownership or delete the team first.";
  return null;
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** "expires in 6 days" / "expires Dec 20" / "expired". */
export function expiresText(at: number | null, now: number): string {
  if (at === null) return "never expires";
  const days = Math.round((at - now) / 86_400_000);
  if (at <= now) return "expired";
  if (days <= 0) return "expired";
  if (days <= 7) return `expires in ${plural(days, "day")}`;
  return `expires ${new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" })}`;
}

/** "Editor · 1 of 1 left · expires in 6 days · by Priya" (UX §7.2). */
export function inviteSummary(i: InviteInfo, now: number): string {
  const uses = i.maxUses === null ? "unlimited" : `${Math.max(0, i.maxUses - i.uses)} of ${i.maxUses} left`;
  const by = i.createdBy ? ` · by ${(i.createdBy.name ?? "Former member").split(" ")[0]}` : "";
  return `${roleName(i.role)} · ${uses} · ${expiresText(i.expiresAt, now)}${by}`;
}

/** "Anyone with the link can join as Editor until Dec 3." (UX §7.4, live). */
export function inviteSentence(role: string, uses: number | null, expiresInDays: number, now: number): string {
  const until = new Date(now + expiresInDays * 86_400_000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
  const who = uses === 1 ? "One person" : uses === null ? "Anyone with the link" : `Up to ${uses} people`;
  return `${who} can join as ${roleName(role)} until ${until}.`;
}

/** "You", "Sam Lee" or "Former member" (UX §12.1). */
export function createdByText(c: UserRef | null | undefined, me: string | null): string {
  if (!c) return "—";
  if (me && c.userId === me) return "You";
  return c.name ?? "Former member";
}

/** "3h", "5m", "2d", "now". */
export function shortAge(ts: number, now: number): string {
  const s = Math.max(0, Math.round((now - ts) / 1000));
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86_400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86_400)}d`;
}

/** "Edited by Sam · 3h" and its tooltip "Last edited by Sam Lee · Nov 26, 14:02" (UX §6.5). */
export function editedByText(e: EditedBy, now: number): { label: string; title: string } {
  const first = e.name === "Former member" ? e.name : e.name.split(" ")[0];
  return { label: `Edited by ${first} · ${shortAge(e.updatedAt, now)}`, title: `Last edited by ${e.name} · ${dateTime(e.updatedAt)}` };
}

/** A conflict copy made by sync (AC-SYNC-4): "Create order (conflict)". */
export function isConflictCopy(name: string): boolean {
  return /\(conflict\)\s*$/.test(name);
}

/** Why a share link can't be updated or revoked by me (UX §12.2), or null. */
export function shareLockReason(role: SpaceRole | null | undefined, mine: boolean, action: "update" | "revoke"): string | null {
  if (!role) return null;
  if (rank(role) < 2) return "Viewers can't change share links";
  if (!mine && rank(role) < 3) return action === "revoke" ? "Only admins can revoke others' links" : "Only admins can update others' links";
  return null;
}

/** What I may do with a project in the switcher menu (UX §5.1, AC-ROLE-1/2). */
export function projectPermissions(l: ProjectLocation | null | undefined): { edit: string | null; delete: string | null; moveOut: string | null } {
  if (!l || l.kind !== "team") return { edit: null, delete: null, moveOut: null };
  const r = rank(l.role);
  const viewer = r < 2 ? "Viewers can't change team projects" : null;
  return {
    edit: viewer,
    delete: viewer ?? (r < 3 ? "Only admins can delete team projects" : null),
    moveOut: viewer ?? (r < 3 ? `Only admins can move a project out of ${l.label}. Use Copy to project… instead.` : null),
  };
}

/** Teams I can move or copy a project into (editor or above). */
export function writableTeam(role: SpaceRole | null | undefined): boolean {
  return rank(role) >= 2;
}

/** The space notice's unsynced line, "4 changes hadn't synced yet." */
export function unsyncedLine(n: number): string | null {
  return n > 0 ? `${plural(n, "change")} hadn't synced yet.` : null;
}

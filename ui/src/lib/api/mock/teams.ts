/**
 * Mock of teams and invites (04 §5.3–§5.4, 06-go-server §10 M2), mirroring the server's role rules
 * closely enough for the UI: the actor × target × role matrix (`forbidden_role`), the billing
 * owner (`transfer_required`), the last owner (`last_owner`), owned-team and invite limits, single-use
 * admin links, and invite preview / accept.
 *
 * Seed: the team "Acme" (space TEAM_ID) with the signed-in user (role from `?mockRole`, default
 * editor; only with `?mockTeam=1` or `?mockRole`), Priya Shah as billing owner when I'm not the
 * owner, Sam Lee (editor) and Olu Okafor (viewer). "Globex" exists for joining.
 *
 * Invite links: `https://<server>/i#<token>`. Tokens made by `invite_create` resolve to their team;
 * otherwise a token containing `invalid` or `expired` is refused (410), `full` joins fail with
 * `limit_members`, `acme` points at Acme, anything else at Globex; `viewer` / `admin` in the token
 * set the role (default editor).
 *
 * Switches: `?mockTeams=off` (the server doesn't advertise features.teams).
 */
import type { CreatedInvite, InviteCheck, InviteInfo, JoinResult, TeamDetail, TeamMember, TeamRole, TeamSummary, TeamsOverview } from "../types";
import { fail } from "./errors";
import { ACME_TEAM_ID, TEAM_ID, type MockSpaces } from "./spaces";
import { ulid } from "../../util/ulid";

export const GLOBEX_TEAM_ID = "01JTEAMGLOBEX000000000000";
export const GLOBEX_SPACE_ID = "01JSPACEGLOBEX00000000000";
const MAX_OWNED = 3;
const MAX_INVITES = 20;

interface MockTeam {
  teamId: string;
  spaceId: string;
  name: string;
  ownerUserId: string;
  members: TeamMember[];
  invites: (InviteInfo & { token: string; revoked: boolean })[];
  bytesUsed: number;
  quotaBytes: number;
  createdAt: number;
}

export interface TeamsHost {
  me(): { userId: string; name: string; email: string } | null;
  serverUrl(): string | null;
  now(): number;
  spaces: MockSpaces;
  /** A team project that arrives with the first sync after joining. */
  addProject(name: string, spaceId: string): void;
  /** How many local projects of a space have unsynced edits (for the downgrade banner). */
  unsynced(spaceId: string): number;
}

const R: Record<string, number> = { owner: 4, admin: 3, editor: 2, viewer: 1 };
const rank = (r: string) => R[r] ?? 0;
const label = (r: string) => r.charAt(0).toUpperCase() + r.slice(1);

export class MockTeams {
  enabled: boolean;
  teams: MockTeam[] = [];
  private host: TeamsHost;
  private seededFor: string | null = null;
  private opts: { role?: TeamRole; team?: boolean };

  constructor(host: TeamsHost, opts: { enabled?: boolean; role?: TeamRole; team?: boolean } = {}) {
    this.host = host;
    this.enabled = opts.enabled ?? true;
    this.opts = opts;
  }

  private member(userId: string, name: string, email: string, role: TeamRole, daysAgo: number): TeamMember {
    return { userId, name, email, role, joinedAt: this.host.now() - daysAgo * 86_400_000 };
  }

  /** The server side, seeded once per signed-in user. */
  private seed(): void {
    const me = this.host.me();
    if (!me || this.seededFor === me.userId) return;
    this.seededFor = me.userId;
    const now = this.host.now();
    const priya = "01JUSERPRIYA0000000000000";
    const sam = this.member("01JUSERSAM000000000000000", "Sam Lee", "sam@acme.dev", "editor", 24);
    const olu = this.member("01JUSEROLU000000000000000", "Olu Okafor", "olu@acme.dev", "viewer", 7);
    this.teams = [];
    if (this.opts.team || this.opts.role) {
      const role = this.opts.role ?? "editor";
      const members: TeamMember[] = [this.member(me.userId, me.name, me.email, role, 56), sam, olu];
      let owner = me.userId;
      if (role !== "owner") {
        const billing = me.userId === priya ? this.member("01JUSERDANA0000000000000", "Dana Kim", "dana@acme.dev", "owner", 90) : this.member(priya, "Priya Shah", "priya@acme.dev", "owner", 90);
        members.unshift(billing);
        owner = billing.userId;
      }
      this.teams.push({
        teamId: ACME_TEAM_ID,
        spaceId: TEAM_ID,
        name: "Acme",
        ownerUserId: owner,
        members,
        invites: [
          { inviteId: "01JINVITEVIEWER0000000000", role: "viewer", maxUses: null, uses: 2, expiresAt: now + 20 * 86_400_000, createdAt: now - 3 * 86_400_000, createdBy: { userId: sam.userId, name: sam.name }, token: "acmeviewer", revoked: false },
        ],
        bytesUsed: 8 * 1024 * 1024,
        quotaBytes: 250 * 1024 * 1024,
        createdAt: now - 90 * 86_400_000,
      });
    }
    this.teams.push({
      teamId: GLOBEX_TEAM_ID,
      spaceId: GLOBEX_SPACE_ID,
      name: "Globex",
      ownerUserId: "01JUSERHANK000000000000",
      members: [this.member("01JUSERHANK000000000000", "Hank Scorpio", "hank@globex.dev", "owner", 300)],
      invites: [],
      bytesUsed: 3 * 1024 * 1024,
      quotaBytes: 250 * 1024 * 1024,
      createdAt: now - 300 * 86_400_000,
    });
  }

  private requireMe(): { userId: string; name: string; email: string } {
    if (!this.enabled) fail("teams_disabled", "Teams aren't turned on for this server. Ask its admin.");
    const me = this.host.me() ?? fail("not_signed_in", "You're not signed in.");
    this.seed();
    return me;
  }

  private mine(): MockTeam[] {
    const me = this.requireMe();
    return this.teams.filter((t) => t.members.some((m) => m.userId === me.userId));
  }

  private team(teamId: string): MockTeam {
    const me = this.requireMe();
    const t = this.teams.find((x) => x.teamId === teamId && x.members.some((m) => m.userId === me.userId));
    return t ?? fail("not_found", "Team not found");
  }

  private myRole(t: MockTeam): TeamRole {
    const me = this.requireMe();
    return (t.members.find((m) => m.userId === me.userId)?.role ?? "viewer") as TeamRole;
  }

  private summary(t: MockTeam): TeamSummary {
    return {
      teamId: t.teamId,
      name: t.name,
      role: this.myRole(t),
      spaceId: t.spaceId,
      memberCount: t.members.length,
      bytesUsed: t.bytesUsed,
      quotaBytes: t.quotaBytes,
      ownerUserId: t.ownerUserId,
      createdAt: t.createdAt,
    };
  }

  /** Keep this device's bindings in step (what the next `/v1/me/spaces` does). */
  private bindMine(): void {
    const me = this.host.me();
    if (!me) return;
    for (const t of this.teams) {
      const m = t.members.find((x) => x.userId === me.userId);
      const b = this.host.spaces.binding(t.spaceId);
      if (m && (!b || b.state !== "active" || b.role !== m.role || b.memberCount !== t.members.length)) {
        if (b && b.state === "active") {
          this.host.spaces.setRole(t.spaceId, m.role as TeamRole, this.host.unsynced(t.spaceId));
          b.memberCount = t.members.length;
          b.name = t.name;
        } else this.host.spaces.addTeam(t.spaceId, t.teamId, t.name, m.role as TeamRole, t.members.length);
      }
    }
  }

  list(): TeamSummary[] {
    return this.mine().map((t) => this.summary(t));
  }

  /** `GET /v1/teams` with `ownedCount` and the server's limits. */
  overview(): TeamsOverview {
    const me = this.requireMe();
    return {
      teams: this.list(),
      ownedCount: this.teams.filter((t) => t.ownerUserId === me.userId).length,
      limits: { ownedTeams: MAX_OWNED, teamMembers: 50, teamInvites: MAX_INVITES },
      owner: { bytesUsed: this.teams.filter((t) => t.ownerUserId === me.userId).reduce((n, t) => n + t.bytesUsed, 12 * 1024 * 1024), quotaBytes: null },
    };
  }

  create(name: string): TeamSummary {
    const me = this.requireMe();
    const n = name.trim();
    if (!n || n.length > 80) fail("invalid", "A team name is 1 to 80 characters.");
    const owned = this.teams.filter((t) => t.ownerUserId === me.userId).length;
    if (owned >= MAX_OWNED) fail("limit_teams", "You can own up to 3 teams on this server.");
    const now = this.host.now();
    const t: MockTeam = {
      teamId: ulid(now),
      spaceId: ulid(now + 1),
      name: n,
      ownerUserId: me.userId,
      members: [this.member(me.userId, me.name, me.email, "owner", 0)],
      invites: [],
      bytesUsed: 0,
      quotaBytes: 250 * 1024 * 1024,
      createdAt: now,
    };
    this.teams.push(t);
    this.bindMine();
    return this.summary(t);
  }

  get(teamId: string): TeamDetail {
    const me = this.requireMe();
    const t = this.team(teamId);
    const admin = rank(this.myRole(t)) >= 3;
    return {
      team: this.summary(t),
      members: structuredClone(t.members),
      invites: admin ? t.invites.filter((i) => !i.revoked && (i.expiresAt ?? Infinity) > this.host.now()).map(({ token: _t, revoked: _r, ...i }) => structuredClone(i)) : null,
      me: me.userId,
    };
  }

  rename(teamId: string, name: string): null {
    const t = this.team(teamId);
    if (rank(this.myRole(t)) < 3) fail("forbidden_role", "Only admins can rename the team.");
    const n = name.trim();
    if (!n || n.length > 80) fail("invalid", "A team name is 1 to 80 characters.");
    t.name = n;
    const b = this.host.spaces.binding(t.spaceId);
    if (b) b.name = n;
    return null;
  }

  delete(teamId: string): void {
    const me = this.requireMe();
    const t = this.team(teamId);
    if (t.ownerUserId !== me.userId) fail("forbidden_role", "Only the team's owner can delete it.");
    this.teams = this.teams.filter((x) => x !== t);
  }

  /** The 04 §5.4 predicate shared by role change and removal. */
  private check(t: MockTeam, targetId: string, newRole: TeamRole | null): TeamMember {
    const me = this.requireMe();
    const target = t.members.find((m) => m.userId === targetId) ?? fail("not_a_member", "That person isn't in this team.");
    if (t.ownerUserId === targetId) fail("transfer_required", "Transfer ownership first");
    const actor = this.myRole(t);
    const self = targetId === me.userId;
    const allowed =
      actor === "owner" ||
      (actor === "admin" && rank(target.role) < 3 && (newRole === null || rank(newRole) <= 3)) ||
      (self && newRole !== null && rank(newRole) < rank(target.role));
    if (!allowed) fail("forbidden_role", "Your role doesn't allow that.");
    const owners = t.members.filter((m) => m.role === "owner").length;
    if (target.role === "owner" && newRole !== "owner" && owners <= 1) fail("last_owner", "A team needs at least one owner");
    return target;
  }

  setRole(teamId: string, userId: string, role: TeamRole): null {
    if (!R[role]) fail("invalid", `Unknown role ${role}`);
    const t = this.team(teamId);
    const target = this.check(t, userId, role);
    target.role = role;
    // QA fix: demotion below admin revokes the links they created.
    if (rank(role) < 3) for (const i of t.invites) if (i.createdBy?.userId === userId) i.revoked = true;
    this.bindMine();
    return null;
  }

  removeMember(teamId: string, userId: string, revokeInvites: boolean): null {
    const t = this.team(teamId);
    this.check(t, userId, null);
    t.members = t.members.filter((m) => m.userId !== userId);
    for (const i of t.invites) if (revokeInvites || i.createdBy?.userId === userId) i.revoked = true;
    return null;
  }

  leave(teamId: string): MockTeam {
    const me = this.requireMe();
    const t = this.team(teamId);
    if (t.ownerUserId === me.userId) fail("transfer_required", "You're the team's owner. Transfer ownership or delete the team first.");
    const owners = t.members.filter((m) => m.role === "owner").length;
    if (this.myRole(t) === "owner" && owners <= 1) fail("last_owner", "You're the last owner. Transfer ownership or delete the team first.");
    t.members = t.members.filter((m) => m.userId !== me.userId);
    return t;
  }

  transfer(teamId: string, userId: string): null {
    const me = this.requireMe();
    const t = this.team(teamId);
    if (t.ownerUserId !== me.userId) fail("forbidden_role", "Only the team's owner can transfer it.");
    const target = t.members.find((m) => m.userId === userId && m.userId !== me.userId) ?? fail("not_a_member", "That person isn't in this team.");
    // Olu's storage is full in the mock: "Olu doesn't have room for Acme."
    if (target.name.startsWith("Olu")) fail("quota_exceeded", `${target.name.split(" ")[0]} doesn't have room for ${t.name}. They need 8.1 MB more.`);
    target.role = "owner";
    t.ownerUserId = userId;
    const mine = t.members.find((m) => m.userId === me.userId)!;
    mine.role = "admin";
    this.bindMine();
    return null;
  }

  createInvite(teamId: string, role: TeamRole, maxUses: number | null, expiresInDays: number): CreatedInvite {
    const me = this.requireMe();
    const t = this.team(teamId);
    const actor = this.myRole(t);
    if (rank(actor) < 3) fail("forbidden_role", "Only admins can create invite links.");
    if (role === "admin" && actor !== "owner") fail("forbidden_role", "Only owners can create admin links");
    if (role === "admin" && maxUses !== 1) fail("invalid", "Admin links are single-use.");
    if (![1, 7, 30].includes(expiresInDays)) fail("invalid", "Invite links expire after 1, 7 or 30 days.");
    const now = this.host.now();
    const active = t.invites.filter((i) => !i.revoked && (i.expiresAt ?? Infinity) > now);
    if (active.length >= MAX_INVITES) fail("limit_invites", `${t.name} already has 20 active invite links. Revoke some first.`);
    const token = `k${ulid(now).toLowerCase()}${Math.random().toString(36).slice(2, 10)}`;
    const invite = { inviteId: ulid(now), role, maxUses, uses: 0, expiresAt: now + expiresInDays * 86_400_000, createdAt: now, createdBy: { userId: me.userId, name: me.name }, token, revoked: false };
    t.invites.push(invite);
    const { token: _t, revoked: _r, ...info } = invite;
    return { invite: structuredClone(info), url: `${this.host.serverUrl() ?? ""}/i#${token}` };
  }

  listInvites(teamId: string): InviteInfo[] {
    return this.get(teamId).invites ?? fail("forbidden_role", "Only admins can see invite links.");
  }

  revokeInvite(teamId: string, inviteId: string): null {
    const t = this.team(teamId);
    if (rank(this.myRole(t)) < 3) fail("forbidden_role", "Only admins can revoke invite links.");
    const i = t.invites.find((x) => x.inviteId === inviteId) ?? fail("not_found", "Invite not found");
    i.revoked = true;
    return null;
  }

  /** Mirrors core `parse_invite_link`. */
  static parse(url: string): { origin: string; token: string } {
    const raw = url.trim();
    try {
      const u = new URL(raw.includes("://") ? raw : `https://${raw}`);
      const token = u.hash.replace(/^#/, "").trim();
      if (!/^https?:$/.test(u.protocol) || u.pathname.replace(/\/$/, "") !== "/i" || !token || !/^[A-Za-z0-9._~-]+$/.test(token)) throw new Error();
      return { origin: u.origin, token };
    } catch {
      return fail("invite_not_a_link", "That isn't a ReqHarbor invite link.");
    }
  }

  private resolve(token: string): { team: MockTeam; role: TeamRole; invite: (MockTeam["invites"][number]) | null } {
    this.seed();
    for (const t of this.teams) {
      const i = t.invites.find((x) => x.token === token);
      if (i) {
        const valid = !i.revoked && (i.expiresAt ?? Infinity) > this.host.now() && (i.maxUses === null || i.uses < i.maxUses);
        if (!valid) fail("invite_invalid", "This invite link is no longer valid.");
        return { team: t, role: i.role as TeamRole, invite: i };
      }
    }
    if (/invalid|expired/i.test(token)) fail("invite_invalid", "This invite link is no longer valid.");
    const role: TeamRole = /viewer/i.test(token) ? "viewer" : /admin/i.test(token) ? "admin" : "editor";
    const acme = this.teams.find((t) => t.teamId === ACME_TEAM_ID);
    const team = /acme/i.test(token) && acme ? acme : this.teams.find((t) => t.teamId === GLOBEX_TEAM_ID)!;
    return { team, role, invite: null };
  }

  preview(url: string): InviteCheck {
    if (!this.enabled) fail("teams_disabled", "Teams aren't turned on for this server. Ask its admin.");
    const link = MockTeams.parse(url);
    const { team, role, invite } = this.resolve(link.token);
    const inviter = invite?.createdBy?.name ?? team.members.find((m) => m.userId === team.ownerUserId)?.name ?? null;
    const account = this.host.serverUrl();
    return {
      serverUrl: link.origin,
      otherServer: !!account && account.replace(/\/$/, "") !== link.origin,
      preview: { teamId: team.teamId, teamName: team.name, inviterName: inviter, role, expiresAt: invite?.expiresAt ?? this.host.now() + 6 * 86_400_000 },
    };
  }

  accept(url: string): JoinResult {
    const me = this.requireMe();
    const link = MockTeams.parse(url);
    const account = this.host.serverUrl();
    if (account && account.replace(/\/$/, "") !== link.origin) fail("invite_other_server", "This invite is for another server. Sign in there to join.");
    const { team, role, invite } = this.resolve(link.token);
    const existing = team.members.find((m) => m.userId === me.userId);
    if (existing) {
      return { teamId: team.teamId, teamName: team.name, role: existing.role, spaceId: team.spaceId, alreadyMember: true, projects: [...this.host.spaces.projectSpaces.values()].filter((s) => s === team.spaceId).length };
    }
    if (/full/i.test(link.token)) fail("limit_members", `${team.name} has reached 50 members.`);
    team.members.push(this.member(me.userId, me.name, me.email, role, 0));
    if (invite) invite.uses++;
    this.host.spaces.addTeam(team.spaceId, team.teamId, team.name, role, team.members.length);
    if (team.teamId === GLOBEX_TEAM_ID && ![...this.host.spaces.projectSpaces.values()].includes(team.spaceId)) {
      this.host.addProject("Globex API", team.spaceId);
      this.host.addProject("Globex Billing", team.spaceId);
    }
    const projects = [...this.host.spaces.projectSpaces.values()].filter((s) => s === team.spaceId).length;
    return { teamId: team.teamId, teamName: team.name, role, spaceId: team.spaceId, alreadyMember: false, projects };
  }

  /** Test hook (`__mock.teams.removeMe("Acme")`): an admin removed me; the next sync notices. */
  removeMe(name: string, reason: "removed" | "deleted" = "removed"): void {
    this.seed();
    const me = this.host.me();
    const t = this.teams.find((x) => x.name === name);
    if (!me || !t) return;
    t.members = t.members.filter((m) => m.userId !== me.userId);
    this.host.spaces.removeTeam(t.spaceId, reason);
  }

  /** Test hook: an admin changed my role; the next sync notices (and freezes unsynced edits). */
  setMyRole(name: string, role: TeamRole): void {
    this.seed();
    const me = this.host.me();
    const t = this.teams.find((x) => x.name === name);
    const m = t?.members.find((x) => x.userId === me?.userId);
    if (!t || !m) return;
    m.role = role;
    this.host.spaces.setRole(t.spaceId, role, this.host.unsynced(t.spaceId));
  }

  /** "Sam is now Admin". */
  static roleLabel(r: string): string {
    return label(r);
  }
}

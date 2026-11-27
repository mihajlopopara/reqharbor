import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { flushSync } from "svelte";
import { setBackend } from "../../src/lib/api";
import { MockServer } from "../../src/lib/api/mock";
import { MockTeams } from "../../src/lib/api/mock/teams";
import { TEAM_ID } from "../../src/lib/api/mock/spaces";
import { makeCall, type CommandError, type InviteInfo, type TeamMember } from "../../src/lib/api/types";
import { app } from "../../src/lib/stores/app.svelte";
import { ui } from "../../src/lib/stores/ui.svelte";
import { toasts } from "../../src/lib/stores/toasts.svelte";
import {
  createdByText,
  editedByText,
  inviteSentence,
  inviteSummary,
  isConflictCopy,
  leaveLockReason,
  projectPermissions,
  removeLockReason,
  roleLockReason,
  roleOptions,
  shareLockReason,
  teamsAvailable,
  type MemberContext,
} from "../../src/lib/util/teams";
import { DEFAULT_AUTH } from "../../src/lib/stores/app.svelte";

const member = (userId: string, role: string): TeamMember => ({ userId, name: userId, email: `${userId}@acme.dev`, role, joinedAt: 1 });

describe("team role rules (04 §5.4, UX §7.2)", () => {
  const owner: MemberContext = { actor: "owner", me: "P", ownerUserId: "P", owners: 1 };
  const admin: MemberContext = { actor: "admin", me: "A", ownerUserId: "P", owners: 1 };
  const editor: MemberContext = { actor: "editor", me: "E", ownerUserId: "P", owners: 1 };

  it("the billing owner and the last owner are locked", () => {
    expect(roleLockReason(member("P", "owner"), owner)).toBe("Transfer ownership first");
    expect(roleLockReason(member("O2", "owner"), { ...owner, ownerUserId: "X", owners: 1 })).toBe("A team needs at least one owner");
    expect(removeLockReason(member("P", "owner"), admin)).toBe("Transfer ownership first");
  });

  it("admins act only on editors and viewers and grant at most admin", () => {
    expect(roleLockReason(member("S", "editor"), admin)).toBeNull();
    expect(roleLockReason(member("A2", "admin"), admin)).toBe("Admins can change only editors and viewers");
    const opts = roleOptions(member("S", "editor"), admin);
    expect(opts.find((o) => o.value === "owner")?.disabled).toBe(true);
    expect(opts.find((o) => o.value === "admin")?.disabled).toBe(false);
    expect(removeLockReason(member("S", "editor"), admin)).toBeNull();
    expect(removeLockReason(member("A2", "admin"), admin)).toBe("Only admins can remove members");
  });

  it("editors and viewers can only lower themselves", () => {
    expect(roleLockReason(member("S", "viewer"), editor)).toBe("Only admins can change roles");
    expect(roleLockReason(member("E", "editor"), editor)).toBeNull();
    const own = roleOptions(member("E", "editor"), editor);
    expect(own.find((o) => o.value === "viewer")?.disabled).toBe(false);
    expect(own.find((o) => o.value === "admin")?.disabled).toBe(true);
    expect(removeLockReason(member("E", "editor"), editor)).toBe("Use Leave team… instead");
  });

  it("the billing owner and the last owner can't leave", () => {
    expect(leaveLockReason(owner)).toMatch(/Transfer ownership or delete the team/);
    expect(leaveLockReason({ ...owner, ownerUserId: "X", owners: 1 })).toMatch(/last owner/);
    expect(leaveLockReason(editor)).toBeNull();
  });

  it("project permissions in the switcher (AC-ROLE-1/2)", () => {
    const base = { projectId: "p", spaceId: "s", kind: "team" as const, label: "Acme", state: "active" as const };
    expect(projectPermissions({ ...base, role: "viewer" }).edit).toBe("Viewers can't change team projects");
    expect(projectPermissions({ ...base, role: "editor" }).delete).toBe("Only admins can delete team projects");
    expect(projectPermissions({ ...base, role: "editor" }).moveOut).toMatch(/Only admins can move a project out of Acme/);
    expect(projectPermissions({ ...base, role: "admin" })).toEqual({ edit: null, delete: null, moveOut: null });
    expect(projectPermissions({ ...base, kind: "personal", role: "owner" }).delete).toBeNull();
  });
});

describe("team copy (UX §6.4, §6.5, §7.4, §12)", () => {
  const now = Date.UTC(2026, 10, 27, 12);
  it("invite rows and the live sentence", () => {
    const i: InviteInfo = { inviteId: "i", role: "editor", maxUses: 1, uses: 0, expiresAt: now + 6 * 86_400_000, createdAt: now, createdBy: { userId: "P", name: "Priya Shah" } };
    expect(inviteSummary(i, now)).toBe("Editor · 1 of 1 left · expires in 6 days · by Priya");
    expect(inviteSummary({ ...i, maxUses: null, createdBy: { userId: "X", name: null } }, now)).toMatch(/^Editor · unlimited · .* · by Former$/);
    expect(inviteSentence("viewer", null, 7, now)).toMatch(/^Anyone with the link can join as Viewer until /);
    expect(inviteSentence("editor", 1, 1, now)).toMatch(/^One person can join as Editor until /);
  });

  it("attribution, conflict copies and share permissions", () => {
    expect(createdByText({ userId: "me", name: "Priya" }, "me")).toBe("You");
    expect(createdByText({ userId: "u", name: null }, "me")).toBe("Former member");
    expect(editedByText({ userId: "u", name: "Sam Lee", updatedAt: now - 3 * 3_600_000 }, now).label).toBe("Edited by Sam · 3h");
    expect(editedByText({ userId: "u", name: "Former member", updatedAt: now }, now).label).toBe("Edited by Former member · now");
    expect(isConflictCopy("Create order (conflict)")).toBe(true);
    expect(isConflictCopy("Create order")).toBe(false);
    expect(shareLockReason("viewer", true, "update")).toBe("Viewers can't change share links");
    expect(shareLockReason("editor", false, "revoke")).toBe("Only admins can revoke others' links");
    expect(shareLockReason("editor", true, "revoke")).toBeNull();
    expect(shareLockReason(null, false, "revoke")).toBeNull();
  });

  it("teams need features.teams on the account's server", () => {
    expect(teamsAvailable(DEFAULT_AUTH)).toBe(false);
    const on = { ...DEFAULT_AUTH, enabled: true, account: { userId: "u" } as never, server: { info: { features: { teams: true } } } as never };
    expect(teamsAvailable(on)).toBe(true);
    expect(teamsAvailable({ ...on, server: { info: { features: { teams: false } } } as never })).toBe(false);
  });

  it("invite links parse like the core", () => {
    expect(MockTeams.parse("https://rh.acme.dev/i#k3J9-Qz_x")).toEqual({ origin: "https://rh.acme.dev", token: "k3J9-Qz_x" });
    expect(MockTeams.parse("rh.acme.dev/i/#abc").token).toBe("abc");
    for (const bad of ["", "https://rh.acme.dev/i", "https://rh.acme.dev/s/x#t"]) expect(() => MockTeams.parse(bad)).toThrow();
  });
});

describe("mock teams commands", () => {
  async function signedIn(opts: ConstructorParameters<typeof MockServer>[0] = {}) {
    const server = new MockServer({ latency: 0, ...opts, accounts: { authDelay: 0, account: "signedIn", ...(opts.accounts ?? {}) } });
    const call = makeCall((cmd, args) => server.invoke(cmd, args));
    return { server, call };
  }
  const rejects = async (p: Promise<unknown>): Promise<CommandError> => {
    try {
      await p;
    } catch (e) {
      return e as CommandError;
    }
    throw new Error("expected rejection");
  };

  it("an owner creates teams up to the limit, and the team space binds", async () => {
    const { server, call } = await signedIn();
    const t = await call("team_create", { name: "Globex Labs" });
    expect(t.role).toBe("owner");
    expect(server.syncStatus().spaces.some((s) => s.spaceId === t.spaceId && s.kind === "team")).toBe(true);
    await call("team_create", { name: "Two" });
    await call("team_create", { name: "Three" });
    expect((await rejects(call("team_create", { name: "Four" }))).code).toBe("limit_teams");
    expect((await rejects(call("team_create", { name: " " }))).code).toBe("invalid");
  });

  it("role changes follow the matrix; the billing owner needs a transfer", async () => {
    const { call } = await signedIn({ teams: { role: "admin", team: true }, spaces: { team: true, role: "admin" } });
    const [acme] = (await call("teams_list", {})).teams.filter((t) => t.name === "Acme");
    const d = await call("team_get", { teamId: acme.teamId });
    const sam = d.members.find((m) => m.name === "Sam Lee")!;
    const owner = d.members.find((m) => m.userId === acme.ownerUserId)!;
    await call("team_set_role", { teamId: acme.teamId, userId: sam.userId, role: "admin" });
    expect((await rejects(call("team_set_role", { teamId: acme.teamId, userId: sam.userId, role: "viewer" }))).code).toBe("forbidden_role");
    expect((await rejects(call("team_remove_member", { teamId: acme.teamId, userId: owner.userId, revokeInvites: false }))).code).toBe("transfer_required");
    expect((await rejects(call("invite_create", { teamId: acme.teamId, role: "admin", maxUses: 1, expiresInDays: 7 }))).message).toBe("Only owners can create admin links");
    expect(d.invites?.length).toBe(1);
  });

  it("an owner's links: admin links are single-use; revoke and transfer", async () => {
    const { call } = await signedIn({ teams: { role: "owner", team: true }, spaces: { team: true, role: "owner" } });
    const acme = (await call("teams_list", {})).teams.find((t) => t.name === "Acme")!;
    expect((await rejects(call("invite_create", { teamId: acme.teamId, role: "admin", maxUses: 5, expiresInDays: 7 }))).code).toBe("invalid");
    const link = await call("invite_create", { teamId: acme.teamId, role: "admin", maxUses: 1, expiresInDays: 1 });
    expect(link.url).toMatch(/^https:\/\/reqharbor\.acme\.dev\/i#k/);
    const p = await call("invite_preview", { url: link.url });
    expect(p.preview.teamName).toBe("Acme");
    expect(p.otherServer).toBe(false);
    await call("invite_revoke", { teamId: acme.teamId, inviteId: link.invite.inviteId });
    expect((await rejects(call("invite_preview", { url: link.url }))).code).toBe("invite_invalid");
    const olu = (await call("team_get", { teamId: acme.teamId })).members.find((m) => m.name.startsWith("Olu"))!;
    expect((await rejects(call("team_transfer", { teamId: acme.teamId, userId: olu.userId }))).code).toBe("quota_exceeded");
    const sam = (await call("team_get", { teamId: acme.teamId })).members.find((m) => m.name === "Sam Lee")!;
    await call("team_transfer", { teamId: acme.teamId, userId: sam.userId });
    expect((await call("team_get", { teamId: acme.teamId })).team.role).toBe("admin");
  });

  it("join with a link: preview, accept, already a member, other server, invalid", async () => {
    const { server, call } = await signedIn();
    const url = "https://reqharbor.acme.dev/i#k3J9viewerQz";
    expect((await rejects(call("invite_accept", { url: "https://reqharbor.acme.dev/i#full" }))).code).toBe("limit_members");
    const p = await call("invite_preview", { url });
    expect(p.preview).toMatchObject({ teamName: "Globex", role: "viewer", inviterName: "Hank Scorpio" });
    const j = await call("invite_accept", { url });
    expect(j).toMatchObject({ teamName: "Globex", alreadyMember: false, projects: 2 });
    expect(server.spaces.locations(false).filter((l) => l.label === "Globex").length).toBe(2);
    expect((await call("invite_accept", { url })).alreadyMember).toBe(true);
    expect((await call("invite_preview", { url: "https://other.example/i#tok" })).otherServer).toBe(true);
    expect((await rejects(call("invite_preview", { url: "https://reqharbor.acme.dev/i#expired" }))).code).toBe("invite_invalid");
    expect((await rejects(call("invite_preview", { url: "not a link" }))).code).toBe("invite_not_a_link");
  });

  it("the owned-team limit and count come with the list (UX §7.1)", async () => {
    const { call } = await signedIn();
    for (const n of ["One", "Two", "Three"]) await call("team_create", { name: n });
    const o = await call("teams_list", {});
    expect(o.ownedCount).toBe(3);
    expect(o.limits.ownedTeams).toBe(3);
    expect(o.teams.map((t) => t.name).sort()).toEqual(["One", "Three", "Two"]);
  });

  it("an unfinished move is listed until it is finished or cancelled; another target cancels it", async () => {
    const { server, call } = await signedIn({ teams: { team: true }, spaces: { team: true }, move: "offline" });
    const def = server.projects.find((p) => p.name === "Default")!;
    expect((await rejects(call("move_project_to_space", { projectId: def.id, spaceId: TEAM_ID }))).code).toBe("sync_offline");
    const pending = (await call("get_sync_status", {})).pendingMoves;
    expect(pending).toMatchObject([{ projectId: def.id, projectName: "Default", to: TEAM_ID, targetName: "Acme" }]);
    await call("move_cancel", { projectId: def.id });
    expect((await call("get_sync_status", {})).pendingMoves).toEqual([]);
    // Fails again (first failure per project only once): resume finishes and clears it.
    const side = server.projects.find((p) => p.name === "Default")!;
    await call("move_project_to_space", { projectId: side.id, spaceId: TEAM_ID });
    expect((await call("get_sync_status", {})).pendingMoves).toEqual([]);
  });

  it("an invite for a plain-http server needs Continue anyway", async () => {
    const { call } = await signedIn();
    expect((await rejects(call("invite_preview", { url: "http://192.168.1.5:8080/i#tok" }))).code).toBe("insecure_unconfirmed");
    expect((await rejects(call("invite_ticket", { url: "http://192.168.1.5:8080/i#tok" }))).code).toBe("insecure_unconfirmed");
    expect((await call("invite_preview", { url: "http://192.168.1.5:8080/i#tok", insecureConfirmed: true })).otherServer).toBe(true);
    expect((await call("invite_preview", { url: "http://127.0.0.1:8080/i#tok" })).serverUrl).toBe("http://127.0.0.1:8080");
  });

  it("the move pre-flight scans collection auth and disabled headers", async () => {
    const { server, call } = await signedIn({ teams: { team: true }, spaces: { team: true } });
    const def = server.projects.find((p) => p.name === "Default")!;
    const col = server.collections.find((c) => c.projectId === def.id)!;
    col.auth = { type: "basic", username: "u", password: "hunter2" };
    const req = server.requests.find((r) => r.collectionId === col.id)!;
    req.headers.push({ key: "Cookie", value: "sid=1", enabled: false });
    const p = await call("move_preview", { projectId: def.id, spaceId: TEAM_ID });
    expect(p.credentials.map((c) => c.field)).toEqual(expect.arrayContaining(["Collection auth: Basic auth password", "Cookie header"]));
    expect(p.credentials.find((c) => c.tab === "collection")?.requestName).toBe(col.name);
  });

  it("teams are refused when the server doesn't advertise them", async () => {
    const { call } = await signedIn({ teams: { enabled: false }, accounts: { teams: false } });
    expect((await rejects(call("teams_list", {}))).code).toBe("teams_disabled");
    expect((await call("auth_status", {})).server?.info?.features.teams).toBe(false);
  });

  it("leaving keeps local copies (from Acme) or removes them", async () => {
    const { server, call } = await signedIn({ teams: { team: true }, spaces: { team: true } });
    const acme = (await call("teams_list", {})).teams.find((t) => t.name === "Acme")!;
    await call("team_leave", { teamId: acme.teamId, keepCopy: true });
    expect(server.projects.some((p) => p.name === "Side project (from Acme)")).toBe(true);
    expect(server.syncStatus().spaces.some((s) => s.spaceId === TEAM_ID)).toBe(false);
  });

  it("a viewer can't edit or delete team projects, and the move pre-flight lists credentials", async () => {
    const { server, call } = await signedIn({ teams: { role: "viewer", team: true }, spaces: { team: true, role: "viewer" } });
    const side = server.projects.find((p) => p.name === "Side project")!;
    expect((await rejects(call("rename_project", { projectId: side.id, name: "x" }))).code).toBe("read_only_role");
    expect((await rejects(call("delete_project", { projectId: side.id }))).code).toBe("read_only_role");
    const dup = await call("project_duplicate_to", { projectId: side.id, spaceId: server.spaces.personal()!.spaceId, name: "Mine" });
    expect(server.spaces.location(dup.project.id, false).kind).toBe("personal");
    expect((await rejects(call("move_preview", { projectId: side.id, spaceId: null }))).code).toBe("read_only_role");
  });
});

describe("app store with teams", () => {
  const server = new MockServer({ latency: 0, platform: "macos", accounts: { authDelay: 0, account: "signedIn" }, teams: { team: true, role: "editor" }, spaces: { team: true, role: "editor" } });
  const until = async (cond: () => boolean, ms = 1000) => {
    const t = Date.now();
    while (!cond()) {
      if (Date.now() - t > ms) throw new Error("timeout");
      await new Promise((r) => setTimeout(r, 5));
    }
  };
  beforeAll(async () => {
    setBackend(server.backend());
    await app.init();
  });
  afterAll(() => app.destroy());

  it("teams are on for this account; a team project knows its role", async () => {
    expect(app.teamsOn).toBe(true);
    const side = app.projects.find((p) => p.name === "Side project")!;
    await app.switchProject(side.id);
    flushSync();
    expect(app.locationById.get(side.id)?.kind).toBe("team");
    expect(app.viewerHere).toBe(false);
  });

  it("confirmEx returns the checkbox and blocks until the name is typed", async () => {
    const p = ui.confirmEx({ title: "Delete Acme?", body: "x", typeToConfirm: "Acme", checkbox: { label: "Also", checked: true } });
    await until(() => !!ui.confirmState);
    expect(ui.confirmState?.typeToConfirm).toBe("Acme");
    expect(ui.confirmState?.checkbox?.checked).toBe(true);
    ui.confirmState!.resolve({ ok: true, checked: false });
    ui.confirmState = null;
    expect(await p).toEqual({ ok: true, checked: false });
    expect(await Promise.race([ui.confirm({ title: "t", body: "b" }), (async () => (await until(() => !!ui.confirmState), ui.confirmState!.resolve({ ok: false }), "x"))()])).toBe("x");
    ui.confirmState = null;
  });

  it("a move to a team switches to the moved copy (new ids)", async () => {
    const def = app.projects.find((p) => p.name === "Default")!;
    await app.switchProject(def.id);
    const res = await app.moveProjectTo(def.id, TEAM_ID);
    flushSync();
    expect(res.projectId).not.toBe(def.id);
    expect(app.activeProjectId).toBe(res.projectId);
    expect(app.locationById.get(res.projectId)?.spaceId).toBe(TEAM_ID);
    expect(app.projects.some((p) => p.id === def.id)).toBe(false);
  });

  it("after a downgrade the banner's choice unfreezes, and a viewer's save only explains", async () => {
    server.teams.setMyRole("Acme", "viewer");
    server.emitStatus();
    await until(() => app.sync.spaces.find((s) => s.spaceId === TEAM_ID)?.role === "viewer");
    await app.loadLocations();
    flushSync();
    const side = app.projects.find((p) => p.name === "Side project")!;
    await app.switchProject(side.id);
    flushSync();
    expect(app.viewerHere).toBe(true);
    const first = app.tree!.requests[0];
    app.openRequest(first.id);
    app.request.url = "https://changed";
    const before = toasts.items.length;
    await app.save();
    expect(toasts.items.at(-1)?.message).toMatch(/Viewers can't change team projects/);
    expect(toasts.items.length).toBeGreaterThan(before);
    expect(server.requests.find((r) => r.id === first.id)?.url).not.toBe("https://changed");
    expect(await app.resolveRoleChange(TEAM_ID, "discard")).toBe(true);
    expect(app.sync.spaces.find((s) => s.spaceId === TEAM_ID)?.frozen).toBe(0);
  });
});

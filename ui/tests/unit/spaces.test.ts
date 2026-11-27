import { beforeEach, describe, expect, it } from "vitest";
import { MockServer } from "../../src/lib/api/mock";
import { makeCall, type CommandError, type Project, type ProjectLocation, type SpaceStatus, type SyncStatus } from "../../src/lib/api/types";
import { bindingIcon, chipTooltip, foreignSpaces, groupProjects, locationCanShare, locationTooltip, spaceLine, spacesWithIssues } from "../../src/lib/util/spaces";

let server: MockServer;
let call: ReturnType<typeof makeCall>;

function make(opts: ConstructorParameters<typeof MockServer>[0] = {}) {
  server = new MockServer({ latency: 0, ...opts, accounts: { authDelay: 0, ...(opts.accounts ?? {}) } });
  call = makeCall((cmd, args) => server.invoke(cmd, args));
}

async function rejects(p: Promise<unknown>): Promise<CommandError> {
  try {
    await p;
  } catch (e) {
    return e as CommandError;
  }
  throw new Error("expected rejection");
}

async function signIn() {
  await call("auth_sign_in_start", { serverUrl: "https://reqharbor.acme.dev", method: "github" });
  await server.accounts.finishBrowser();
}

const project = (id: string, name: string, sortOrder = 0): Project => ({ id, workspaceId: "w", name, sortOrder, activeEnvironmentId: null, createdAt: 1, updatedAt: 1 });
const loc = (projectId: string, kind: ProjectLocation["kind"], spaceId: string | null, extra: Partial<ProjectLocation> = {}): ProjectLocation => ({
  projectId,
  spaceId,
  kind,
  label: kind === "local" ? "This device only" : kind === "personal" ? "Personal" : kind === "legacy" ? "Legacy workspace" : "Acme",
  role: kind === "team" ? "editor" : kind === "personal" ? "owner" : null,
  state: kind === "local" ? null : "active",
  ...extra,
});
const space = (spaceId: string, kind: SpaceStatus["kind"], extra: Partial<SpaceStatus> = {}): SpaceStatus => ({
  spaceId,
  kind,
  name: kind === "personal" ? "Personal" : kind === "legacy" ? "Legacy" : "Acme",
  role: kind === "team" ? "editor" : null,
  state: "active",
  reason: null,
  issue: null,
  lastSyncAt: 1,
  pendingChanges: 0,
  projectCount: 1,
  bytesUsed: null,
  quotaBytes: null,
  unsupported: 0,
  accountEmail: "priya@acme.dev",
  ...extra,
});

describe("grouped project switcher (UX §5.1)", () => {
  const projects = [project("a", "Default"), project("b", "Payments API", 1), project("c", "Scratch", 2), project("d", "Old sync", 3)];
  const locations = [loc("a", "personal", "P"), loc("b", "team", "T"), loc("c", "local", null), loc("d", "legacy", "legacy")];

  it("groups by space in a fixed order with the team role", () => {
    const groups = groupProjects(projects, locations, [space("P", "personal"), space("T", "team"), space("G", "team", { name: "Globex", role: "viewer", projectCount: 0 })], true);
    expect(groups.map((g) => g.heading)).toEqual(["Personal", "Acme", "Globex", "On this device only", "Legacy workspace"]);
    expect(groups[1].trailing).toBe("Editor");
    expect(groups[2]).toMatchObject({ emptyRow: true, trailing: "Viewer", projects: [] });
    expect(groups[3].projects.map((p) => p.name)).toEqual(["Scratch"]);
  });

  it("hides an empty personal group but keeps On this device only while signed in", () => {
    const g = groupProjects([project("c", "Scratch")], [loc("c", "local", null)], [space("P", "personal", { projectCount: 0 })], true);
    expect(g.map((x) => x.key)).toEqual(["local"]);
    expect(groupProjects([], [], [space("P", "personal")], true).map((x) => x.key)).toEqual(["local"]);
    expect(groupProjects([], [], [space("P", "personal")], false)).toEqual([]);
  });

  it("marks paused groups when signed out or for another account", () => {
    const signedOut = groupProjects([projects[0]], [loc("a", "personal", "P", { state: "signedOut" })], [space("P", "personal", { state: "signedOut" })], false);
    expect(signedOut[0].heading).toBe("Personal · Paused: signed out");
    const foreign = groupProjects([projects[0]], [loc("a", "personal", "P", { state: "paused" })], [space("P", "personal", { state: "paused", reason: "foreignAccount" })], true);
    expect(foreign[0].heading).toBe("Personal · priya@acme.dev · Paused");
  });

  it("binding icons and tooltips", () => {
    expect(bindingIcon(locations[0], true)).toBe("cloud");
    expect(bindingIcon(locations[1], true)).toBe("users");
    expect(bindingIcon(locations[2], true)).toBe("device");
    expect(bindingIcon(locations[3], true)).toBe("key");
    expect(bindingIcon(loc("a", "personal", "P", { state: "signedOut" }), true)).toBe("cloud-off");
    // Signed out with no bindings: zero change.
    expect(bindingIcon(undefined, false)).toBe("layers");
    expect(locationTooltip("Payments API", locations[1], space("T", "team"))).toBe("Payments API · Acme · Editor · Synced");
    expect(locationTooltip("Scratch", locations[2], undefined)).toBe("Scratch · This device only");
  });
});

describe("status lines (UX §6.6, §6.7)", () => {
  const base: SyncStatus = {
    state: "idle",
    connected: true,
    workerUrl: null,
    remoteWorkspaceId: null,
    lastSyncAt: 1000,
    lastPullAt: null,
    lastError: null,
    nextRetryAt: null,
    retryAttempt: 0,
    pendingChanges: 0,
    skippedEntities: [],
    unsupportedEntities: [],
    warning: null,
    accountMode: true,
    spaces: [],
    legacy: null,
    firstSyncPending: false,
  };

  it("one line per space under the overall line", () => {
    const s = {
      ...base,
      spaces: [
        space("P", "personal"),
        space("T", "team", { issue: { code: "quota", message: "Team storage full (25 MB). Ask an admin to clean up.", at: 1 } }),
        space("legacy", "legacy", { state: "sharesOnly" }),
      ],
    };
    expect(chipTooltip(s, 61_000, null)).toBe("Synced 1m — click to sync now\nPersonal · Synced\nAcme · Team storage full (25 MB). Ask an admin to clean up.\nLegacy · Shares only · syncing stopped");
    expect(spacesWithIssues(s).map((x) => x.spaceId)).toEqual(["T"]);
    expect(chipTooltip({ ...base, accountMode: false }, 61_000, null)).toBe("Synced 1m — click to sync now");
    expect(chipTooltip({ ...s, state: "offline" }, 0, 30).split("\n")[0]).toBe("Retrying in 30 s — click to sync now");
  });

  it("words every state", () => {
    expect(spaceLine(space("P", "personal", { state: "signedOut" }))).toBe("Personal · Paused: signed out");
    expect(spaceLine(space("P", "personal", { state: "unavailable" }))).toBe("Personal · Temporarily unavailable · Retrying");
    expect(spaceLine(space("T", "team", { state: "removed" }))).toBe("Acme · Removed · Decide");
    expect(spaceLine(space("P", "personal", { pendingChanges: 3 }))).toBe("Personal · 3 pending");
    expect(spaceLine(space("P", "personal", { unsupported: 2 }))).toBe("Personal · 2 items waiting for a server update");
  });
});

describe("mock personal sync (04 §6, §7)", () => {
  beforeEach(() => make());

  it("a first sign-in with local projects asks before anything syncs", async () => {
    await signIn();
    const s = await call("get_sync_status", {});
    expect(s.accountMode).toBe(true);
    expect(s.firstSyncPending).toBe(true);
    const cands = await call("first_sync_candidates", {});
    expect(cands.map((c) => c.name)).toEqual(["Default", "Side project"]);
    expect((await call("project_locations", {})).every((l) => l.kind === "local")).toBe(true);
    const side = cands.find((c) => c.name === "Side project")!;
    const after = await call("first_sync_apply", { projectIds: [side.projectId] });
    expect(after.firstSyncPending).toBe(false);
    const locs = await call("project_locations", {});
    expect(locs.find((l) => l.projectId === side.projectId)?.kind).toBe("personal");
    expect(locs.filter((l) => l.kind === "local")).toHaveLength(1);
  });

  it("a fresh device binds its untouched Default without asking", async () => {
    make({ empty: true });
    await signIn();
    const s = await call("get_sync_status", {});
    expect(s.firstSyncPending).toBe(false);
    expect((await call("project_locations", {}))[0].kind).toBe("personal");
  });

  it("token sync migrates to the legacy space and merges into Personal", async () => {
    await call("sync_connect", { workerUrl: "https://sync.example.dev", token: "rh_ABCDEFGHIJKLMNOP" });
    await signIn();
    let s = await call("get_sync_status", {});
    expect(s.legacy).toMatchObject({ mode: "sync", projectCount: 2 });
    expect(s.firstSyncPending).toBe(false);
    expect((await call("project_locations", {})).every((l) => l.kind === "legacy")).toBe(true);
    await call("legacy_merge", {});
    s = await call("get_sync_status", {});
    expect(s.legacy?.mode).toBe("sharesOnly");
    expect((await call("project_locations", {})).every((l) => l.kind === "personal")).toBe(true);
    await call("legacy_disconnect", {});
    expect((await call("get_sync_status", {})).legacy).toBeNull();
  });

  it("new projects go to Personal by default, or stay local when asked", async () => {
    make({ accounts: { account: "signedIn" } });
    const a = await call("create_project", { name: "Synced" });
    const b = await call("create_project", { name: "Mine", location: "local" });
    const locs = await call("project_locations", {});
    expect(locs.find((l) => l.projectId === a.project.id)?.kind).toBe("personal");
    expect(locs.find((l) => l.projectId === b.project.id)?.kind).toBe("local");
    expect((await rejects(call("create_project", { name: "X", location: "nope" }))).code).toBe("not_found");
  });

  it("sync location moves between Personal and this device; team moves wait for teams", async () => {
    make({ accounts: { account: "signedIn" }, spaces: { team: true } });
    const locs = await call("project_locations", {});
    const personal = locs.find((l) => l.kind === "personal")!;
    const team = locs.find((l) => l.kind === "team")!;
    await call("project_set_location", { projectId: personal.projectId, spaceId: null });
    expect((await call("project_locations", {})).find((l) => l.projectId === personal.projectId)?.kind).toBe("local");
    await call("project_set_location", { projectId: personal.projectId, spaceId: personal.spaceId });
    expect((await call("project_locations", {})).find((l) => l.projectId === personal.projectId)?.kind).toBe("personal");
    expect((await rejects(call("project_set_location", { projectId: team.projectId, spaceId: null }))).message).toContain("arrives with teams");
    expect((await rejects(call("project_set_location", { projectId: personal.projectId, spaceId: team.spaceId! }))).message).toContain("arrives with teams");
  });

  it("another account's spaces pause; Keep makes local copies, Remove drops them", async () => {
    make({ accounts: { account: "signedIn" }, spaces: { foreign: true } });
    const s = await call("get_sync_status", {});
    const foreign = foreignSpaces(s);
    expect(foreign).toHaveLength(1);
    expect(foreign[0].accountEmail).toBe("sam@acme.dev");
    // A fresh personal space for the signed-in account exists alongside.
    expect(s.spaces.filter((x) => x.kind === "personal" && x.state === "active")).toHaveLength(1);
    const before = (await call("list_projects", {})).length;
    await call("space_notice_resolve", { spaceId: foreign[0].spaceId, keep: true });
    expect((await call("list_projects", {})).length).toBe(before);
    expect((await call("project_locations", {})).every((l) => l.kind === "local")).toBe(true);
    expect(foreignSpaces(await call("get_sync_status", {}))).toHaveLength(0);

    make({ accounts: { account: "signedIn" }, spaces: { foreign: true } });
    const f2 = foreignSpaces(await call("get_sync_status", {}))[0];
    await call("create_project", { name: "Mine", location: "local" });
    await call("space_notice_resolve", { spaceId: f2.spaceId, keep: false });
    expect((await call("list_projects", {})).map((p) => p.name)).toEqual(["Mine"]);
  });

  it("signing out pauses account spaces and keeps the bindings", async () => {
    make({ accounts: { account: "signedIn" } });
    await call("auth_sign_out", {});
    const s = await call("get_sync_status", {});
    expect(s.accountMode).toBe(true);
    expect(s.connected).toBe(false);
    expect(s.state).toBe("paused");
    expect(s.spaces[0].state).toBe("signedOut");
    expect((await call("project_locations", {}))[0].state).toBe("signedOut");
  });

  it("shares go to the project's space and need a synced project", async () => {
    make({ accounts: { account: "signedIn" } });
    const tree = await call("get_project_tree", { projectId: (await call("list_projects", {}))[0].id });
    const req = tree.requests[0];
    const info = await call("share_create", { kind: "request", entityId: req.id, includeCredentials: false });
    expect(info.spaceId).toBeTruthy();
    expect(info.url.startsWith("https://reqharbor.acme.dev/s/")).toBe(true);
    await call("project_set_location", { projectId: tree.project.id, spaceId: null });
    const e = await rejects(call("share_create", { kind: "request", entityId: req.id, includeCredentials: false }));
    expect(e).toMatchObject({ code: "sync_not_connected", message: "Sharing needs this project to sync. Change its sync location to Personal first." });
  });

  it("the Global stays local before the checklist, after Keep all and after a foreign Keep", async () => {
    await signIn();
    expect(server.spaces.globalSpace).toBeNull();
    await call("first_sync_apply", { projectIds: [] });
    expect(server.spaces.globalSpace).toBeNull();
    await call("sync_now", {});
    expect(server.spaces.globalSpace).toBeNull();

    make();
    await signIn();
    const [first] = await call("first_sync_candidates", {});
    await call("first_sync_apply", { projectIds: [first.projectId] });
    expect(server.spaces.globalSpace).toBe(server.spaces.personal()?.spaceId);

    make({ accounts: { account: "signedIn" }, spaces: { foreign: true } });
    const f = foreignSpaces(await call("get_sync_status", {}))[0];
    await call("space_notice_resolve", { spaceId: f.spaceId, keep: true });
    expect(server.spaces.globalSpace).toBeNull();
  });

  it("signing back in as the original account resumes its paused spaces", async () => {
    make({ accounts: { account: "signedIn" }, spaces: { foreign: true } });
    expect(foreignSpaces(await call("get_sync_status", {}))).toHaveLength(1);
    await call("auth_sign_out", {});
    server.accounts.identity = { userId: "01JUSERSAM000000000000000", email: "sam@acme.dev", name: "Sam" };
    await signIn();
    const s = await call("get_sync_status", {});
    // Sam's space syncs again; only Priya's empty personal space is paused now (nothing to decide).
    expect(foreignSpaces(s).filter((x) => x.projectCount > 0)).toHaveLength(0);
    expect(s.spaces.find((x) => x.accountEmail === "sam@acme.dev")?.state).toBe("active");
    expect((await call("project_locations", {})).every((l) => l.state === "active")).toBe(true);
  });

  it("a token-synced project moves to Personal only through the merge", async () => {
    await call("sync_connect", { workerUrl: "https://sync.example.dev", token: "rh_ABCDEFGHIJKLMNOP" });
    await signIn();
    const [l] = await call("project_locations", {});
    const personal = (await call("get_sync_status", {})).spaces.find((x) => x.kind === "personal")!;
    expect((await rejects(call("project_set_location", { projectId: l.projectId, spaceId: personal.spaceId }))).message).toContain("Merge into my personal space");
    await call("project_set_location", { projectId: l.projectId, spaceId: null });
    expect((await call("project_locations", {}))[0].kind).toBe("local");
  });

  it("a merge whose follow-up sync fails still reports the merge", async () => {
    make({ mergeSyncFails: true });
    await call("sync_connect", { workerUrl: "https://sync.example.dev", token: "rh_ABCDEFGHIJKLMNOP" });
    await signIn();
    const r = await call("legacy_merge", {});
    expect(r.merged).toBe(2);
    expect(r.report).toBeNull();
    expect(r.syncError?.code).toBe("sync_offline");
    expect((await call("get_sync_status", {})).legacy?.mode).toBe("sharesOnly");
  });

  it("a notice closed with × keeps the projects after 7 days", async () => {
    let now = 1_000_000_000_000;
    make({ accounts: { account: "signedIn" }, spaces: { foreign: true }, now: () => now });
    const f = foreignSpaces(await call("get_sync_status", {}))[0];
    await call("space_notice_later", { spaceIds: [f.spaceId] });
    now += 6 * 86_400_000;
    await call("sync_now", {});
    expect(foreignSpaces(await call("get_sync_status", {}))).toHaveLength(1);
    now += 2 * 86_400_000;
    const before = (await call("list_projects", {})).length;
    await call("sync_now", {});
    expect(foreignSpaces(await call("get_sync_status", {}))).toHaveLength(0);
    expect((await call("list_projects", {})).length).toBe(before);
  });

  it("a fresh device gets the 'Signed in · syncing N projects' notice", async () => {
    make({ empty: true });
    const notices: string[] = [];
    await server.backend().listen("sync://notice", (n) => notices.push((n as { message: string }).message));
    await signIn();
    expect(notices).toEqual(["Signed in · syncing 1 project from your account."]);
  });

  it("a paused or signed-out location can't share", async () => {
    expect(locationCanShare(loc("a", "personal", "P"))).toBe(true);
    expect(locationCanShare(loc("a", "personal", "P", { state: "paused" }))).toBe(false);
    expect(locationCanShare(loc("a", "personal", "P", { state: "signedOut" }))).toBe(false);
    expect(locationCanShare(loc("a", "local", null))).toBe(false);
    make({ accounts: { account: "signedIn" }, spaces: { foreign: true } });
    const tree = await call("get_project_tree", { projectId: (await call("list_projects", {}))[0].id });
    const e = await rejects(call("share_create", { kind: "request", entityId: tree.requests[0].id, includeCredentials: false }));
    expect(e.code).toBe("sync_not_connected");
    expect(e.message).toContain("paused");
  });

  it("an issue on the personal space is reported per space", async () => {
    make({ accounts: { account: "signedIn" }, spaces: { mode: "skew" } });
    const s = await call("get_sync_status", {});
    expect(s.spaces[0].issue?.message).toBe("Your computer's clock is ahead by about 7 minutes. Changes can't sync until it's fixed.");
  });
});

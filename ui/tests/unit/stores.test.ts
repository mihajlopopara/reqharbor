import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { flushSync } from "svelte";
import { setBackend } from "../../src/lib/api";
import { MockServer } from "../../src/lib/api/mock";
import { app } from "../../src/lib/stores/app.svelte";
import { ui } from "../../src/lib/stores/ui.svelte";
import { toasts } from "../../src/lib/stores/toasts.svelte";
import { ws } from "../../src/lib/stores/ws.svelte";
import { scratchRequest } from "../../src/lib/util/request";
import { switchKind } from "../../src/lib/util/ws";
import type { ProjectTree, Request } from "../../src/lib/api/types";

const server = new MockServer({ latency: 0, platform: "macos" });

beforeAll(async () => {
  setBackend(server.backend());
  await app.init();
});
afterAll(() => app.destroy());

const until = async (cond: () => boolean, ms = 1000) => {
  const t = Date.now();
  while (!cond()) {
    if (Date.now() - t > ms) throw new Error("timeout");
    await new Promise((r) => setTimeout(r, 5));
  }
};

describe("app store", () => {
  it("bootstraps workspace, tree, variables and history", () => {
    expect(app.ready).toBe(true);
    expect(app.platform).toBe("macos");
    expect(app.project?.name).toBe("Default");
    expect(app.tree?.collections.length).toBe(2);
    expect(app.activeEnv?.name).toBe("Staging");
    expect(app.lookup.get("baseUrl")?.value).toBe("https://api.staging.example.com");
    expect(app.history.length).toBe(10);
  });

  it("URL edits sync Query rows and vice versa (AC-REQ-2)", () => {
    app.newRequest();
    app.setUrl("https://x.dev/a?b=1&c=2");
    expect(app.request.params.map((p) => p.key)).toEqual(["b", "c"]);
    app.request.params[0].enabled = false;
    app.syncUrlFromParams();
    expect(app.request.url).toBe("https://x.dev/a?c=2");
    app.setUrl("https://x.dev/a?c=3&d=4");
    expect(app.request.params).toEqual([
      { key: "b", value: "1", enabled: false },
      { key: "c", value: "3", enabled: true },
      { key: "d", value: "4", enabled: true },
    ]);
  });

  it("tracks dirty state, keeps drafts per request and saves with ⌘S (AC-REQ-11)", async () => {
    const list = app.tree!.requests.find((r) => r.name === "List users")!;
    const create = app.tree!.requests.find((r) => r.name === "Create user")!;
    app.openRequest(list.id);
    expect(app.dirty).toBe(false);
    app.setUrl(list.url + "&extra=1");
    expect(app.dirty).toBe(true);
    app.openRequest(create.id);
    expect(app.isDirty(list.id)).toBe(true);
    expect(app.dirty).toBe(false);
    app.openRequest(list.id);
    expect(app.request.url).toContain("extra=1");
    await app.save();
    expect(app.dirty).toBe(false);
    expect(app.isDirty(list.id)).toBe(false);
    expect(server.requests.find((r) => r.id === list.id)?.url).toContain("extra=1");
  });

  it("sending never saves and stores the response per request", async () => {
    const before = app.history.length;
    app.setUrl(app.request.url + "&more=2");
    await app.send();
    await until(() => !!app.currentResponse?.body);
    const view = app.currentResponse!;
    expect(view.response?.status).toBe(200);
    expect(view.envName).toBe("Staging");
    expect(app.dirty).toBe(true);
    await until(() => app.history.length === before + 1);
  });

  it("blocks send without URL and on invalid GraphQL variables", async () => {
    app.newRequest();
    await app.send();
    expect(app.sendBlocked).toBe("Enter a URL");
    app.setUrl("https://x.dev/graphql");
    app.request.body = { type: "graphql", graphql: { query: "{a}", variables: "{oops" } };
    await app.send();
    expect(app.sendBlocked).toBe("Fix GraphQL variables");
  });

  it("imports cURL into the editor with Undo", async () => {
    app.newRequest({ url: "https://before.dev" });
    const ok = await app.importCurlInPlace("curl -X QUERY https://q.dev/s --data-raw '{}' -H 'Content-Type: application/json'");
    expect(ok).toBe(true);
    expect(app.request.method).toBe("QUERY");
    expect(toasts.items.some((t) => t.message === "Imported from cURL" && t.actionLabel === "Undo")).toBe(true);
    app.undoCurl();
    expect(app.request.url).toBe("https://before.dev");
    expect(await app.importCurlInPlace("not curl")).toBe(false);
  });

  it("formats bodies and surfaces format errors", async () => {
    app.newRequest();
    app.request.body = { type: "json", text: '{"a":1}' };
    expect(await app.formatBody()).toBeNull();
    expect(app.request.body.text).toBe('{\n  "a": 1\n}');
    app.request.body.text = '{\n  "a": }';
    expect(await app.formatBody()).toEqual({ line: 2, column: expect.any(Number) });
    expect(app.formatError?.message).toMatch(/^Invalid JSON at line 2, col \d+$/);
    expect(app.request.body.text).toBe('{\n  "a": }');
  });

  it("opens history entries without re-sending (AC-HIST-4)", async () => {
    const sendsBefore = server.history.length;
    const h = app.history.find((x) => x.status === 200)!;
    await app.openHistory(h.id);
    expect(app.isScratch).toBe(true);
    expect(app.currentResponse?.fromHistory).toBe(h.timestamp);
    await until(() => !!app.currentResponse?.body);
    expect(server.history.length).toBe(sendsBefore);
  });

  it("history delete is undoable", () => {
    const id = app.history[0].id;
    app.deleteHistory(id);
    expect(app.hiddenHistory.has(id)).toBe(true);
    const t = toasts.items.find((x) => x.message === "History entry deleted")!;
    t.action?.();
    expect(app.hiddenHistory.has(id)).toBe(false);
  });

  it("switches environment and reloads variables", async () => {
    const local = app.projectEnvs.find((e) => e.name === "Local")!;
    await app.setActiveEnvironment(local.id);
    expect(app.activeEnv?.name).toBe("Local");
    expect(app.lookup.get("baseUrl")?.value).toBe("http://localhost:8080");
    await app.setActiveEnvironment(null);
    expect(app.activeEnv).toBeNull();
    expect(app.lookup.has("baseUrl")).toBe(false);
  });

  it("creates, renames and deletes collections (with confirm)", async () => {
    const c = await app.createCollection("Temp");
    expect(ui.renaming).toEqual({ kind: "collection", id: c!.id });
    await app.rename("collection", c!.id, "Temp 2");
    expect(app.index?.collectionById.get(c!.id)?.name).toBe("Temp 2");
    const p = app.deleteNode("collection", c!.id);
    await until(() => !!ui.confirmState);
    expect(ui.confirmState?.title).toBe("Delete “Temp 2”?");
    ui.confirmState!.resolve({ ok: true });
    ui.confirmState = null;
    await p;
    expect(app.index?.collectionById.has(c!.id)).toBe(false);
  });

  it("switching projects scopes the tree and envs", async () => {
    const side = app.projects.find((p) => p.name === "Side project")!;
    await app.switchProject(side.id);
    flushSync();
    expect(app.tree?.collections.map((c) => c.name)).toEqual(["Playground"]);
    expect(app.projectEnvs.map((e) => e.name)).toEqual(["Production"]);
    expect(app.settings.activeProjectId).toBe(side.id);
  });

  it("rounds pane sizes before saving and toasts (once) when a save fails (QA-R2-001)", async () => {
    let sent: unknown = null;
    const orig0 = server.handlers.save_settings;
    server.handlers.save_settings = (a) => ((sent = a.settings.sidebarWidth), orig0(a));
    app.updateSettings({ sidebarWidth: 312.5 }, true);
    await until(() => server.settings.sidebarWidth === 313);
    expect(sent).toBe(313);
    server.handlers.save_settings = orig0;
    const orig = server.handlers.save_settings;
    server.handlers.save_settings = () => {
      throw new Error("disk full");
    };
    app.updateSettings({ splitRatio: 0.4 }, true);
    app.updateSettings({ splitRatio: 0.41 }, true);
    await until(() => toasts.items.some((t) => t.message.startsWith("Couldn't save settings")));
    await new Promise((r) => setTimeout(r, 20));
    expect(toasts.items.filter((t) => t.message.startsWith("Couldn't save settings")).length).toBe(1);
    server.handlers.save_settings = orig;
    app.updateSettings({ splitRatio: 0.42 }, true);
    await until(() => server.settings.splitRatio === 0.42);
  });

  it("remembers share options per share (QA-R2-002)", () => {
    app.rememberShareOptions("s1", { includeCredentials: true, environmentId: "e1" });
    expect(app.shareOptions("s1")).toEqual({ includeCredentials: true, environmentId: "e1" });
    app.rememberShareOptions("s1", null);
    expect(app.shareOptions("s1")).toBeNull();
  });

  it("reflects sync status events", async () => {
    await server.invoke("sync_create_workspace", { workerUrl: "https://w.dev" });
    expect(app.sync.connected).toBe(true);
    expect(app.sync.state).toBe("idle");
  });
});

describe("WebSocket sessions of requests that are gone (QA pass 2)", () => {
  const wsReq = (url: string): Request => {
    const r = scratchRequest({ url });
    switchKind(r, "ws");
    return r;
  };
  const open = async (id: string) => until(() => ws.session(id)?.state === "open");

  it("a sync pull that deletes a saved request closes its session; the open scratch one stays", async () => {
    const pid = app.activeProjectId!;
    const col = app.tree!.collections[0];
    const saved = (await server.invoke("create_request", { collectionId: col.id, request: wsReq("ws://localhost:9000/a") })) as Request;
    await app.loadTree();
    await ws.connect(saved, pid);
    await open(saved.id);
    const conn = server.ws.conns.get(ws.session(saved.id)!.connectionId)!;
    // The open scratch WebSocket request keeps its session.
    app.newRequest();
    app.setKind("ws");
    app.request.url = "ws://localhost:9000/scratch";
    await app.wsConnect();
    await open(app.request.id);
    // A pull removes the saved request: the tree arrives without it.
    const t = (await server.invoke("delete_request", { requestId: saved.id })) as ProjectTree;
    app.setTree(t);
    await until(() => ws.session(saved.id) === null);
    await until(() => conn.state === "closed");
    expect(ws.session(app.request.id)?.state).toBe("open");
    await ws.discard([app.request.id]);
  });

  it("deleting a project (or losing it with its space) closes its sessions", async () => {
    const other = (await server.invoke("create_project", { name: "Gone soon", location: null })) as ProjectTree;
    const r = wsReq("ws://localhost:9000/p");
    await ws.connect(r, other.project.id);
    await open(r.id);
    const kept = wsReq("ws://localhost:9000/kept");
    await ws.connect(kept, app.activeProjectId);
    await open(kept.id);
    await server.invoke("delete_project", { projectId: other.project.id });
    await app.reloadAll(); // what a sync pull / leaving a space runs
    await until(() => ws.session(r.id) === null);
    expect(ws.session(kept.id)?.state).toBe("open");
    await ws.discard([kept.id]);
  });
});

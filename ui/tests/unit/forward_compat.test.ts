/**
 * Forward compatibility (accounts plan M0, AC-FWD-1..4): requests written by a newer ReqHarbor
 * are view-only in the UI and the mock backend refuses to change or send them, like Rust does.
 */
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { setBackend } from "../../src/lib/api";
import { AUTH_SHAPE, MOCK_APP_VERSION, MockServer, REQUEST_SHAPE, dropUnknownKeys, keepUnknownKeys, renumberAround } from "../../src/lib/api/mock";
import { skippedReasonText, skippedWarning } from "../../src/lib/util/format";
import { makeCall, type CommandError, type Request } from "../../src/lib/api/types";
import { app } from "../../src/lib/stores/app.svelte";
import { toasts } from "../../src/lib/stores/toasts.svelte";
import { isContainerReadOnly, isKnownAuth, isReadOnlyNewer, NEEDS_NEWER_VERSION, newerKindLabel, parseVersion, readOnlyReason, requiresNewerClient, scratchRequest } from "../../src/lib/util/request";

async function rejects(p: Promise<unknown>): Promise<CommandError> {
  try {
    await p;
  } catch (e) {
    return e as CommandError;
  }
  throw new Error("expected rejection");
}

describe("version helpers (mirror core requires_newer_client)", () => {
  it("parses and compares versions", () => {
    expect(parseVersion("1.2.3")).toEqual([1, 2, 3]);
    expect(parseVersion("v2")).toEqual([2, 0, 0]);
    expect(parseVersion("0.2.0-beta.1+abc")).toEqual([0, 2, 0]);
    expect(parseVersion("one")).toBeNull();
    expect(parseVersion("1.2.3.4")).toBeNull();
    expect(requiresNewerClient("0.1.0", "0.1.0")).toBe(false);
    expect(requiresNewerClient("0.0.9", "0.1.0")).toBe(false);
    expect(requiresNewerClient("0.1.1", "0.1.0")).toBe(true);
    expect(requiresNewerClient("1.0", "0.9.9")).toBe(true);
    expect(requiresNewerClient("garbage", "0.1.0")).toBe(true);
  });

  it("flags non-HTTP kinds, unknown body types and newer minClientVersion", () => {
    const r = scratchRequest();
    expect(isReadOnlyNewer(r, "0.1.0")).toBe(false);
    expect(isReadOnlyNewer({ ...r, kind: "http" }, "0.1.0")).toBe(false);
    for (const kind of ["graphql", "mqtt", "socketio"]) expect(isReadOnlyNewer({ ...r, kind }, "0.1.0")).toBe(true);
    // gRPC requests are editable in this build (AC-GRPC-1).
    expect(isReadOnlyNewer({ ...r, kind: "grpc" }, "0.1.0")).toBe(false);
    // AC-WS-1: WebSocket requests are editable here, unless a part of them comes from a newer build.
    expect(isReadOnlyNewer({ ...r, kind: "ws" }, "0.1.0")).toBe(false);
    expect(isReadOnlyNewer({ ...r, kind: "ws", ws: { messages: [{ name: "m", kind: "cbor", body: "" }], protocols: [], autoReconnect: false } }, "0.1.0")).toBe(true);
    expect(readOnlyReason({ ...r, kind: "ws", ws: { messages: [], protocols: [], autoReconnect: false, socketIo: { path: "/socket.io/", namespace: "/", version: 5 } } }, "0.1.0")).toBe("It uses Socket.IO version 5");
    expect(isReadOnlyNewer({ ...r, body: { type: "protobuf" } }, "0.1.0")).toBe(true);
    expect(isReadOnlyNewer({ ...r, minClientVersion: "9.0.0" }, "0.1.0")).toBe(true);
    expect(isReadOnlyNewer({ ...r, minClientVersion: "0.1.0" }, "0.1.0")).toBe(false);
    expect(newerKindLabel({ kind: "mqtt" })).toBe("MQTT");
    expect(newerKindLabel({ kind: "graphql" })).toBe("GQL");
    expect(newerKindLabel({ kind: "websocket" })).toBe("WEBS");
    expect(newerKindLabel({})).toBe("NEW");
  });

  it("flags unknown auth schemes, API key locations and form field kinds (open enums)", () => {
    const r = scratchRequest();
    expect(isReadOnlyNewer({ ...r, auth: { type: "oauth2" } }, "0.1.0")).toBe(true);
    expect(isReadOnlyNewer({ ...r, auth: { type: "apikey", apiKey: { key: "k", value: "v", in: "cookie" } } }, "0.1.0")).toBe(true);
    expect(isReadOnlyNewer({ ...r, auth: { type: "apikey", apiKey: { key: "k", value: "v", in: "query" } } }, "0.1.0")).toBe(false);
    const form = { type: "form", form: [{ key: "a", value: "", kind: "stream", enabled: true }] };
    expect(isReadOnlyNewer({ ...r, body: form }, "0.1.0")).toBe(true);
    expect(isKnownAuth({ type: "bearer", token: "t" })).toBe(true);
    expect(isKnownAuth({ type: "oauth2" })).toBe(false);
  });

  it("gives the right reason for each case, including minClientVersion only", () => {
    const r = scratchRequest();
    expect(readOnlyReason(r, "0.1.0")).toBeNull();
    expect(readOnlyReason({ ...r, kind: "mqtt" }, "0.1.0")).toBe("This is a “mqtt” request");
    expect(readOnlyReason({ ...r, body: { type: "protobuf" } }, "0.1.0")).toBe("Its body is “protobuf”");
    expect(readOnlyReason({ ...r, auth: { type: "oauth2" } }, "0.1.0")).toBe("Its auth is “oauth2”");
    // A newer minClientVersion with an ordinary (non-"none") body names the version, not the body.
    const v = { ...r, minClientVersion: "9.1.0", body: { type: "json", text: "{}" } };
    expect(readOnlyReason(v, "0.1.0")).toBe("It needs ReqHarbor 9.1.0 or later");
  });
});

describe("mock backend parity", () => {
  const server = new MockServer({ latency: 0, platform: "linux", newer: true });
  const call = makeCall((cmd, args) => server.invoke(cmd, args));
  const grpc = () => server.requests.find((r) => r.kind === "mqtt")!;
  const proto = () => server.requests.find((r) => r.body.type === "protobuf")!;

  it("reports the app version and seeds newer requests with ?newer=1", async () => {
    const b = await call("bootstrap", {});
    expect(b.appVersion).toBe(MOCK_APP_VERSION);
    expect(grpc().name).toBe("Say hello");
    expect(proto().name).toBe("Upload protobuf");
  });

  it("refuses to modify or send read-only requests with read_only_newer", async () => {
    for (const r of [grpc(), proto()]) {
      const before = structuredClone(r);
      expect((await rejects(call("save_request", { request: { ...r, url: "https://x.dev" } }))).code).toBe("read_only_newer");
      // A UI that dropped `kind` / `body.type` can't bypass the guard: the stored version decides.
      const sneaky = { ...r, kind: undefined, body: { type: "none" } } as Request;
      expect((await rejects(call("save_request", { request: sneaky }))).code).toBe("read_only_newer");
      expect((await rejects(call("rename_request", { requestId: r.id, name: "x" }))).code).toBe("read_only_newer");
      expect((await rejects(call("duplicate_request", { requestId: r.id }))).code).toBe("read_only_newer");
      expect((await rejects(call("move_node", { kind: "request", id: r.id, targetCollectionId: r.collectionId, targetFolderId: null, index: 0 }))).code).toBe(
        "read_only_newer",
      );
      const send = await rejects(call("send_request", { executionId: "e1", request: sneaky, projectId: null }));
      expect(send).toMatchObject({ code: "read_only_newer", message: NEEDS_NEWER_VERSION });
      expect((await rejects(call("export_curl", { request: r, projectId: null, keepVariables: true }))).code).toBe("read_only_newer");
      expect((await rejects(call("create_request", { collectionId: r.collectionId, folderId: null, request: r }))).code).toBe("read_only_newer");
      expect(server.requests.find((x) => x.id === r.id)).toEqual(before);
    }
    const id = grpc().id;
    await call("delete_request", { requestId: id });
    expect(server.requests.some((r) => r.id === id)).toBe(false);
  });

  it("save_request keeps unknown keys the update lacks, at every level (tech §9.1)", async () => {
    const r = server.requests.find((x) => !isReadOnlyNewer(x, MOCK_APP_VERSION) && x.headers.length > 0)!;
    const stored = r as Request & Record<string, unknown>;
    stored.fxTop = { a: 1 };
    (stored.headers[0] as unknown as Record<string, unknown>).fxHeader = "h";
    (stored.settings as unknown as Record<string, unknown>).fxRetries = 3;
    (stored.body as unknown as Record<string, unknown>).fxBody = true;
    const update: Request = {
      ...scratchRequest(),
      id: r.id,
      collectionId: r.collectionId,
      name: r.name,
      method: r.method,
      url: r.url + "?changed=1",
      headers: r.headers.map((h) => ({ key: h.key, value: h.value, enabled: h.enabled })),
      body: { type: r.body.type, text: r.body.text },
    };
    const saved = (await call("save_request", { request: update })) as Request & Record<string, unknown>;
    expect(saved.url).toContain("changed=1");
    expect(saved.fxTop).toEqual({ a: 1 });
    expect((saved.headers[0] as unknown as Record<string, unknown>).fxHeader).toBe("h");
    expect((saved.settings as unknown as Record<string, unknown>).fxRetries).toBe(3);
    expect((saved.body as unknown as Record<string, unknown>).fxBody).toBe(true);
  });

  it("keepUnknownKeys matches list items by key and never overwrites known values", () => {
    const shape = { known: ["a", "x", "rows"], objects: { x: { known: ["y", "z"] } }, lists: { rows: { known: ["key", "note"] } } };
    const stored = { a: 1, fx: "kept", x: { y: 2, z: 3, fy: 4 }, rows: [{ key: "k1", u: 1 }, { key: "k2", u: 2, note: "n" }] };
    const merged = keepUnknownKeys({ a: 9, x: { y: 5 }, rows: [{ key: "k2" }, { key: "new" }] }, stored, shape);
    // Unknown keys come back; known ones the update dropped (x.z, rows[].note) stay dropped.
    expect(merged).toEqual({ a: 9, fx: "kept", x: { y: 5, fy: 4 }, rows: [{ key: "k2", u: 2 }, { key: "new" }] });
  });

  it("keepUnknownKeys never restores known optional request fields that the update removed", () => {
    const stored = {
      ...scratchRequest(),
      minClientVersion: "0.0.1",
      headers: [{ key: "A", value: "1", enabled: true, description: "old note", fxH: 1 }],
      auth: { type: "bearer", token: "secret", fxAuth: true },
      body: { type: "json", text: "{}", binaryPath: "/tmp/x", fxBody: 2 },
      fxTop: "t",
    };
    const update = { ...scratchRequest(), id: stored.id, headers: [{ key: "A", value: "1", enabled: true }], auth: { type: "none" }, body: { type: "none" } };
    const out = keepUnknownKeys(update, stored) as unknown as Record<string, any>;
    expect(out.headers[0].description).toBeUndefined();
    expect(out.headers[0].fxH).toBe(1);
    expect(out.auth).toEqual({ type: "none", fxAuth: true });
    expect(out.body).toEqual({ type: "none", fxBody: 2 });
    expect(out.fxTop).toBe("t");
    expect(out.minClientVersion).toBe("0.0.1"); // core restores this one too
    expect(keepUnknownKeys({ type: "basic", username: "u" }, { type: "bearer", token: "t", fx: 1 }, AUTH_SHAPE)).toEqual({ type: "basic", username: "u", fx: 1 });
  });

  it("save_request drops a removed header description instead of bringing it back", async () => {
    const r = server.requests.find((x) => !isReadOnlyNewer(x, MOCK_APP_VERSION) && x.headers.length > 0)!;
    r.headers[0].description = "to be removed";
    const update = structuredClone(r);
    delete update.headers[0].description;
    const saved = await call("save_request", { request: update });
    expect(saved.headers[0].description).toBeUndefined();
  });
});

describe("mock backend: containers never rewrite read-only requests (core parity)", () => {
  const server = new MockServer({ latency: 0, platform: "linux", newer: true });
  const call = makeCall((cmd, args) => server.invoke(cmd, args));
  const grpc = () => server.requests.find((r) => r.kind === "mqtt")!;
  const proto = () => server.requests.find((r) => r.body.type === "protobuf")!;
  const users = () => server.collections.find((c) => c.id === grpc().collectionId)!;
  const other = () => server.collections.find((c) => c.projectId === users().projectId && c.id !== users().id)!;

  it("refuses cascading deletes and copies of a container holding one", async () => {
    const snapshot = structuredClone(server.requests);
    const c = users();
    for (const p of [
      call("delete_collection", { collectionId: c.id }),
      call("duplicate_collection", { collectionId: c.id }),
      call("duplicate_project", { projectId: c.projectId }),
      call("delete_project", { projectId: c.projectId }),
    ]) {
      const e = await rejects(p);
      expect(e.code).toBe("read_only_newer");
      expect(e.message).toMatch(/newer version of ReqHarbor/);
    }
    expect(server.requests).toEqual(snapshot);
  });

  it("refuses to move a folder holding one to another collection, allows other moves", async () => {
    const f = server.folders.find((x) => x.collectionId === users().id)!;
    const g = grpc();
    g.folderId = f.id;
    const before = structuredClone(g);
    const e = await rejects(call("move_node", { kind: "folder", id: f.id, targetCollectionId: other().id, targetFolderId: null, index: 0 }));
    expect(e.code).toBe("read_only_newer");
    expect(e.message).toContain("Say hello");
    expect((await rejects(call("delete_folder", { folderId: f.id }))).code).toBe("read_only_newer");
    expect((await rejects(call("duplicate_folder", { folderId: f.id }))).code).toBe("read_only_newer");
    // Within the same collection it's fine, and the request is untouched.
    await call("move_node", { kind: "folder", id: f.id, targetCollectionId: users().id, targetFolderId: null, index: 1 });
    expect(grpc()).toEqual(before);
    g.folderId = null;
  });

  it("reorders siblings around read-only requests without touching them", async () => {
    const c = users();
    const created = await call("create_request", { collectionId: c.id, folderId: null });
    const health = server.requests.find((r) => r.name === "Health")!;
    const g = structuredClone(grpc());
    const p = structuredClone(proto());
    // Root requests without Health: grpc(90), protobuf(91), new(92). Move Health to the end ...
    await call("move_node", { kind: "request", id: health.id, targetCollectionId: c.id, targetFolderId: null, index: 3 });
    expect(grpc()).toEqual(g);
    expect(proto()).toEqual(p);
    const root = server.requests.filter((r) => r.collectionId === c.id && r.folderId === null).sort((a, b) => a.sortOrder - b.sortOrder);
    expect(root.map((r) => r.id)).toEqual([g.id, p.id, created.id, health.id]);
    // ... and to the front.
    await call("move_node", { kind: "request", id: health.id, targetCollectionId: c.id, targetFolderId: null, index: 0 });
    expect(server.requests.find((r) => r.id === health.id)!.sortOrder).toBeLessThan(90);
    expect(grpc()).toEqual(g);
    // Between two adjacent read-only requests there is no room.
    const e = await rejects(call("move_node", { kind: "request", id: health.id, targetCollectionId: c.id, targetFolderId: null, index: 1 }));
    expect(e.code).toBe("read_only_newer");
    expect(grpc()).toEqual(g);
  });

  it("renumberAround mirrors core renumber_around", () => {
    const a = { sortOrder: 0 }, b = { sortOrder: 1 }, g = { sortOrder: 5 }, h = { sortOrder: 6 };
    expect([...renumberAround([a, b], new Set())!.values()]).toEqual([0, 1]);
    expect([...renumberAround([a, g, b], new Set([g]))!.values()]).toEqual([4, 5, 6]);
    expect(renumberAround([g, a, h], new Set([g, h]))).toBeNull();
    expect([...renumberAround([g, h, a, b], new Set([g, h]))!.values()]).toEqual([5, 6, 7, 8]);
  });

  it("collection auth from a newer ReqHarbor: renames work, auth edits are refused, sends explain", async () => {
    const c = other();
    c.auth = { type: "oauth2", fxFlow: { tokenUrl: "https://id/token" } } as unknown as typeof c.auth;
    const renamed = await call("update_collection", { collection: { ...structuredClone(c), name: "Renamed" } });
    expect(renamed.name).toBe("Renamed");
    expect(renamed.auth).toEqual({ type: "oauth2", fxFlow: { tokenUrl: "https://id/token" } });
    const e = await rejects(call("update_collection", { collection: { ...structuredClone(c), auth: { type: "bearer", token: "t" } } }));
    expect(e.code).toBe("read_only_newer");
    const plain = server.collections.find((x) => x.id !== c.id && x.auth.type !== "oauth2")!;
    expect((await rejects(call("update_collection", { collection: { ...structuredClone(plain), auth: { type: "oauth2" } } }))).code).toBe("read_only_newer");
    const r = server.requests.find((x) => x.collectionId === c.id)!;
    const send = await rejects(call("send_request", { executionId: "e2", request: { ...r, auth: { type: "inherit" } }, projectId: null }));
    expect(send).toMatchObject({ code: "validation", details: { kind: "inheritedAuthNeedsNewerVersion" } });
  });
});

describe("app store: view-only requests (AC-FWD-3)", () => {
  const server = new MockServer({ latency: 0, platform: "macos", newer: true });
  const calls: string[] = [];

  beforeAll(async () => {
    const b = server.backend();
    setBackend({
      ...b,
      invoke: (cmd, args) => {
        calls.push(cmd);
        return b.invoke(cmd, args);
      },
    });
    await app.init();
  });
  afterAll(() => app.destroy());

  it("opens a gRPC request read-only and never saves, sends or renames it", async () => {
    expect(app.appVersion).toBe(MOCK_APP_VERSION);
    const g = app.tree!.requests.find((r) => r.kind === "mqtt")!;
    expect(app.isReadOnly(g.id)).toBe(true);
    app.openRequest(g.id);
    expect(app.readOnly).toBe(true);
    // Unknown keys of the opened request stay on the editor copy.
    expect((app.request as Request & { mqtt?: unknown }).mqtt).toBeTruthy();

    calls.length = 0;
    toasts.items = [];
    await app.save();
    await app.send();
    await app.rename("request", g.id, "Renamed");
    await app.duplicate("request", g.id);
    await app.moveNode("request", g.id, g.collectionId, null, 0);
    expect(calls.filter((c) => /save_request|send_request|rename_request|duplicate_request|move_node/.test(c))).toEqual([]);
    expect(app.sendBlocked).toBe(NEEDS_NEWER_VERSION);
    expect(toasts.items.some((t) => t.kind === "info" && t.message.includes(NEEDS_NEWER_VERSION))).toBe(true);
  });

  it("an ordinary request stays editable", () => {
    const r = app.tree!.requests.find((x) => x.name === "List users")!;
    app.openRequest(r.id);
    expect(app.readOnly).toBe(false);
    expect(app.sendBlocked).toBeNull();
  });

  it("the saved copy decides: an editor copy that lost `kind` is still read-only", () => {
    const g = app.tree!.requests.find((r) => r.kind === "mqtt")!;
    app.openRequest(g.id);
    delete (app.request as Partial<Request>).kind;
    expect(app.readOnly).toBe(true);
    app.openRequest(g.id); // back to the saved copy
  });

  it("a sync pull that turns the open, edited request read-only switches the editor and offers the edits as a copy", async () => {
    const target = app.tree!.requests.find((x) => x.name === "Health")!;
    const other = app.tree!.requests.find((x) => x.name === "List users")!;
    // Unsaved edits on another request (stashed as a draft) and on the open one.
    app.openRequest(other.id);
    app.setUrl("https://draft.example/other");
    app.openRequest(target.id);
    app.setUrl("https://draft.example/health");
    expect(app.dirty).toBe(true);
    expect(app.isDirty(other.id)).toBe(true);

    // Another device running a newer ReqHarbor turns both into gRPC requests; the pull lands.
    for (const id of [target.id, other.id]) {
      const stored = server.requests.find((r) => r.id === id)!;
      stored.kind = "mqtt";
      (stored as Request & { mqtt?: unknown }).mqtt = { topic: "t" };
    }
    toasts.items = [];
    server.emit("sync://changed", { pushed: 0, pulled: 2, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 1 });
    await vi.waitFor(() => expect(app.readOnly).toBe(true));
    expect(app.request.kind).toBe("mqtt");
    expect(app.request.url).not.toContain("draft.example");
    expect(app.dirty).toBe(false);
    expect(app.drafts).toEqual({});
    const t = toasts.items.find((x) => x.kind === "warning" && x.message.includes("now read-only here"))!;
    expect(t.message).toMatch(/^2 requests were changed/);
    expect(t.actionLabel).toBe("Save my edits as copies");
    expect(t.timeout).toBe(0);

    // The edits are saved as new, editable requests.
    calls.length = 0;
    t.action!();
    await vi.waitFor(() => expect(calls.filter((c) => c === "create_request")).toHaveLength(2));
    await vi.waitFor(() => expect(app.request.name).toMatch(/\(my edits\)$/));
    const copies = server.requests.filter((r) => r.name.endsWith("(my edits)"));
    expect(copies.map((r) => r.url).sort()).toEqual(["https://draft.example/health", "https://draft.example/other"]);
    expect(copies.every((r) => !isReadOnlyNewer(r, MOCK_APP_VERSION))).toBe(true);
    expect(app.readOnly).toBe(false);
  });
});

describe("mock backend: shares without credentials never leak unknown fields (core share.rs parity)", () => {
  const server = new MockServer({ latency: 0, platform: "linux", newer: true });
  const call = makeCall((cmd, args) => server.invoke(cmd, args));
  const grpc = () => server.requests.find((r) => r.kind === "mqtt")!;
  const colId = server.requests.find((r) => r.kind === "mqtt")!.collectionId;
  const col = () => server.collections.find((c) => c.id === colId)!;

  it("refuses a collection or request share holding a newer request unless credentials are included", async () => {
    (grpc() as Request & { mqtt: Record<string, unknown> }).mqtt.metadata = [{ key: "authorization", value: "Bearer grpc-s3cret" }];
    for (const [kind, entityId] of [["collection", col().id], ["request", grpc().id]] as const) {
      const e = await rejects(call("share_preview", { kind, entityId, includeCredentials: false, environmentId: null }));
      expect(e.code).toBe("read_only_newer");
      expect(e.message).toContain("Say hello");
      expect(e.message).toContain("Include credentials");
    }
    const preview = await call("share_preview", { kind: "collection", entityId: col().id, includeCredentials: true, environmentId: null });
    expect(preview.credentialsReplaced).toBe(0);
    expect(preview.credentialFields).toBeGreaterThan(0);
  });

  it("drops (and counts) unknown keys of editable requests and the collection without credentials", async () => {
    for (const r of server.requests.filter((x) => isReadOnlyNewer(x, MOCK_APP_VERSION))) await call("delete_request", { requestId: r.id });
    const r = server.requests.find((x) => x.collectionId === col().id)!;
    const extra = r as Request & Record<string, unknown>;
    extra.clientCert = { passphrase: "cert-s3cret" };
    r.headers = [{ key: "X-Trace", value: "1", enabled: true, secretRef: "hdr-s3cret" } as Request["headers"][number]];
    r.auth = { type: "bearer", token: "tok", refreshToken: "rt-s3cret" } as Request["auth"];
    (col() as ReturnType<typeof col> & Record<string, unknown>).vault = "col-s3cret";
    const before = await call("share_preview", { kind: "request", entityId: r.id, includeCredentials: true, environmentId: null });
    const preview = await call("share_preview", { kind: "request", entityId: r.id, includeCredentials: false, environmentId: null });
    // token + header extra + auth extra + request extra
    expect(preview.credentialsReplaced).toBe(4);
    expect(before.credentialFields).toBe(4);
    await call("sync_connect", { workerUrl: "https://sync.example.dev", token: "rh_" + "t".repeat(40) });
    const info = await call("share_create", { kind: "collection", entityId: col().id, includeCredentials: false, environmentId: null });
    const text = JSON.stringify(server.shares.get(info.shareId)!.snapshot);
    for (const s of ["cert-s3cret", "hdr-s3cret", "rt-s3cret", "col-s3cret"]) expect(text).not.toContain(s);
    const withCreds = await call("share_create", { kind: "collection", entityId: col().id, includeCredentials: true, environmentId: null });
    const full = JSON.stringify(server.shares.get(withCreds.shareId)!.snapshot);
    for (const s of ["cert-s3cret", "hdr-s3cret", "rt-s3cret", "col-s3cret"]) expect(full).toContain(s);
  });

  it("dropUnknownKeys mirrors the core strip: counts each object with unknown keys", () => {
    const r = { id: "x", fx: 1, headers: [{ key: "a", value: "", enabled: true, fy: 2 }], body: { type: "json", fz: 3 } };
    expect(dropUnknownKeys(r, REQUEST_SHAPE)).toBe(3);
    expect(r).toEqual({ id: "x", headers: [{ key: "a", value: "", enabled: true }], body: { type: "json" } });
    expect(dropUnknownKeys(r, REQUEST_SHAPE)).toBe(0);
  });
});

describe("mock backend: containers with a newer minClientVersion are read-only (AC-FWD-4, core parity)", () => {
  const server = new MockServer({ latency: 0, platform: "linux" });
  const call = makeCall((cmd, args) => server.invoke(cmd, args));

  it("isContainerReadOnly mirrors core extra_requires_newer", () => {
    expect(isContainerReadOnly({}, "0.1.0")).toBe(false);
    expect(isContainerReadOnly({ minClientVersion: null }, "0.1.0")).toBe(false);
    expect(isContainerReadOnly({ minClientVersion: "0.1.0" }, "0.1.0")).toBe(false);
    expect(isContainerReadOnly({ minClientVersion: "99.0.0" }, "0.1.0")).toBe(true);
    expect(isContainerReadOnly({ minClientVersion: 3 }, "0.1.0")).toBe(true);
    expect(isContainerReadOnly({ minClientVersion: "next" }, "0.1.0")).toBe(true);
  });

  it("refuses edits, copies, moves and cascades; reorders around them; allows children and deletes", async () => {
    const pid = server.projects[0].id;
    const cols = server.collections.filter((c) => c.projectId === pid).sort((a, b) => a.sortOrder - b.sortOrder);
    const [c, cr] = cols;
    cr.minClientVersion = "99.0.0";
    const fr = await call("create_folder", { collectionId: c.id, parentFolderId: null, name: "Future folder" });
    server.folders.find((f) => f.id === fr.id)!.minClientVersion = "99.0.0";
    const env = await call("create_environment", { projectId: pid, name: "Future env" });
    server.environments.find((e) => e.id === env.id)!.minClientVersion = "99.0.0";
    const snap = structuredClone({ c: server.collections, f: server.folders, e: server.environments });

    const refused = [
      call("update_collection", { collection: { ...structuredClone(cr), name: "X" } }),
      call("duplicate_collection", { collectionId: cr.id }),
      call("move_node", { kind: "collection", id: cr.id, index: 0, targetCollectionId: null, targetFolderId: null }),
      call("rename_folder", { folderId: fr.id, name: "X" }),
      call("duplicate_folder", { folderId: fr.id }),
      call("move_node", { kind: "folder", id: fr.id, targetCollectionId: cr.id, targetFolderId: null, index: 0 }),
      call("delete_collection", { collectionId: c.id }),
      call("duplicate_project", { projectId: pid }),
      call("update_environment", { environment: { ...structuredClone(server.environments.find((e) => e.id === env.id)!), name: "Y" } }),
      call("duplicate_environment", { environmentId: env.id }),
    ];
    for (const p of refused) {
      const e = await rejects(p);
      expect(e.code).toBe("read_only_newer");
      expect(e.message).toMatch(/newer version of ReqHarbor/);
    }
    expect({ c: server.collections, f: server.folders, e: server.environments }).toEqual(snap);

    // Reordering another collection around it keeps its sort order.
    const fixed = structuredClone(cr);
    const last = await call("create_collection", { projectId: pid, name: "Last" });
    await call("move_node", { kind: "collection", id: last.id, index: 0, targetCollectionId: null, targetFolderId: null });
    expect(server.collections.find((x) => x.id === cr.id)).toEqual(fixed);

    await call("create_request", { collectionId: cr.id, folderId: null });
    await call("delete_folder", { folderId: fr.id });
    await call("delete_collection", { collectionId: cr.id });
    await call("delete_environment", { environmentId: env.id });
  });

  it("a newer project can't be renamed or moved, and others move around it", async () => {
    const extra = await call("create_project", { name: "Mine" });
    const future = server.projects[0];
    future.minClientVersion = "99.0.0";
    const fixed = structuredClone(future);
    expect((await rejects(call("rename_project", { projectId: future.id, name: "X" }))).code).toBe("read_only_newer");
    expect((await rejects(call("move_project", { projectId: future.id, index: 1 }))).code).toBe("read_only_newer");
    await call("move_project", { projectId: extra.project.id, index: 0 });
    expect(server.projects.find((p) => p.id === future.id)).toEqual(fixed);
  });
});

describe("sync copy for items the server rejected (mirrors src-tauri sync_policy)", () => {
  it("words each reason", () => {
    expect(skippedReasonText("too_large").label).toBe("too large");
    expect(skippedReasonText("invalid_json_value").title).toContain("can't store one of its values");
    expect(skippedReasonText("forbidden_role").title).toContain("your role");
    expect(skippedReasonText("future_reason").title).toContain("(future_reason)");
    expect(
      skippedWarning([
        { name: "Odd", reason: "invalid_json_value" },
        { name: "Big", reason: "too_large" },
        { name: "Mine", reason: "forbidden_role" },
        { name: "X", reason: "future_reason" },
        { name: "Odd2", reason: "invalid_json_value" },
      ]),
    ).toBe(
      "1 item is too large to sync (over 512 KB): Big. " +
        "2 items aren't synced because the server can't store one of their values: Odd, Odd2. " +
        "1 item isn't synced because your role can't change it on the server: Mine. " +
        "1 item was rejected by the server (future_reason): X",
    );
    expect(skippedWarning([])).toBeNull();
  });
});

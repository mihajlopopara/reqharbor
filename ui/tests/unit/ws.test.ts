import { beforeEach, describe, expect, it } from "vitest";
import { MockServer } from "../../src/lib/api/mock";
import { MockChannel } from "../../src/lib/api/mock/ws";
import { setBackend } from "../../src/lib/api";
import { makeCall, type CommandError, type Request, type WsFrame } from "../../src/lib/api/types";
import { scratchRequest } from "../../src/lib/util/request";
import { ws } from "../../src/lib/stores/ws.svelte";
import { indexTree, requestsInSubtree } from "../../src/lib/util/tree";
import {
  EMPTY_FILTER,
  LOG_MAX_BYTES,
  FILTER_HINT,
  MAX_FRAME_DATA,
  SEARCH_PREVIEW,
  logEntry,
  trimLog,
  base64Text,
  defaultMessage,
  filterLog,
  frameDetail,
  frameSummary,
  messageError,
  messageTitle,
  nextState,
  normalizeNamespace,
  requestBadge,
  rowHeight,
  stampMinClientVersion,
  WS_MIN_CLIENT_VERSION,
  summaryText,
  switchKind,
  wsUrl,
} from "../../src/lib/util/ws";

const f = (p: Partial<WsFrame>): WsFrame => ({ dir: "in", ts: 0, kind: "text", data: "", size: 0, ...p });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

describe("ws urls (mirror core ws_url)", () => {
  it("maps schemes, drops the fragment and adds Engine.IO params", () => {
    expect(wsUrl("http://a.dev/x?y=1#f")).toBe("ws://a.dev/x?y=1");
    expect(wsUrl("https://a.dev")).toBe("wss://a.dev/");
    expect(wsUrl("WSS://a.dev:8443/s")).toBe("wss://a.dev:8443/s");
    expect(() => wsUrl("ftp://a.dev")).toThrow(/ftp/);
    expect(() => wsUrl("a.dev")).toThrow();
    expect(() => wsUrl("ws://")).toThrow(/no host/);
    expect(wsUrl("http://localhost:3000", { path: "/socket.io/" })).toBe("ws://localhost:3000/socket.io/?EIO=4&transport=websocket");
    expect(wsUrl("https://a.dev/ignored?token=t&EIO=3", { path: "/socket.io/" })).toBe("wss://a.dev/socket.io/?token=t&EIO=4&transport=websocket");
    expect(wsUrl("ws://a.dev", { path: "rt" })).toBe("ws://a.dev/rt?EIO=4&transport=websocket");
    expect(normalizeNamespace("admin")).toBe("/admin");
    expect(normalizeNamespace(" ")).toBe("/");
  });
});

describe("kind switching (AC-WS-1)", () => {
  it("switches to WS / Socket.IO and back, keeping ws and stamping minClientVersion", () => {
    const r = scratchRequest({ method: "POST" });
    switchKind(r, "ws");
    expect(r.kind).toBe("ws");
    expect(r.ws).toEqual({ messages: [], protocols: [], autoReconnect: false });
    expect(r.minClientVersion).toBe("0.1.0");
    expect(requestBadge(r)).toEqual({ label: "WS", cls: "m-ws" });
    switchKind(r, "sio");
    expect(r.ws?.socketIo).toEqual({ path: "/socket.io/", namespace: "/", version: 4 });
    expect(requestBadge(r).label).toBe("SIO");
    r.ws!.messages.push({ name: "m", kind: "text", body: "hi" });
    switchKind(r, "http", "GET");
    expect(r.kind).toBeUndefined();
    expect(r.method).toBe("GET");
    expect(r.ws?.messages).toHaveLength(1);
    expect(requestBadge(r)).toEqual({ label: "GET", cls: null });
    // The stamp never lowers a newer version.
    const n: Request = { ...scratchRequest(), kind: "ws", minClientVersion: "0.2.0" };
    stampMinClientVersion(n);
    expect(n.minClientVersion).toBe("0.2.0");
    // The stamp is the fixed first WS version, never the running one, and it raises older stamps.
    const old: Request = { ...scratchRequest(), kind: "ws", minClientVersion: "0.0.9" };
    stampMinClientVersion(old);
    expect(old.minClientVersion).toBe(WS_MIN_CLIENT_VERSION);
    expect(WS_MIN_CLIENT_VERSION).toBe("0.1.0");
  });
});

describe("message validation (mirror core resolve_message)", () => {
  it("checks JSON, base64 and Socket.IO event names", () => {
    expect(messageError({ name: "", kind: "text", body: "anything" }, false)).toBeNull();
    expect(messageError({ name: "", kind: "json", body: "{oops" }, false)).toMatch(/^Invalid JSON/);
    expect(messageError({ name: "", kind: "json", body: '{"a":"{{x}}"}' }, false)).toBeNull();
    expect(messageError({ name: "", kind: "json", body: "" }, false)).toBe("Enter a JSON message");
    expect(messageError({ name: "", kind: "binary", body: "AAEC/w==" }, false)).toBeNull();
    expect(messageError({ name: "", kind: "binary", body: "@@" }, false)).toBe("Invalid base64");
    expect(messageError({ ...defaultMessage(true), event: " " }, true)).toBe("Enter an event name");
    expect(messageError({ ...defaultMessage(true), kind: "binary" }, true)).toMatch(/Binary/);
    expect(messageError(defaultMessage(true), true)).toBeNull();
    expect(messageTitle({ name: "", kind: "json", body: '{ "type":   "ping" }' })).toBe('{ "type": "ping" }');
    expect(messageTitle({ name: "", kind: "json", body: "", event: "join" })).toBe("join");
  });
});

describe("message log (AC-WS-5)", () => {
  it("tracks the connection state from frames", () => {
    expect(nextState("connecting", f({ dir: "system", kind: "open" }))).toBe("open");
    expect(nextState("open", f({ dir: "in", kind: "text" }))).toBe("open");
    expect(nextState("open", f({ dir: "out", kind: "close" }))).toBe("closing");
    expect(nextState("open", f({ dir: "system", kind: "reconnecting" }))).toBe("connecting");
    expect(nextState("open", f({ dir: "system", kind: "error" }))).toBe("open");
    expect(nextState("closing", f({ dir: "system", kind: "close", last: true }))).toBe("closed");
  });

  it("filters by text, direction, event name and heartbeats", () => {
    const log = [
      f({ dir: "system", kind: "open", data: "Connected to ws://x" }),
      f({ dir: "out", kind: "text", data: '{"hello":1}' }),
      f({ dir: "in", kind: "text", data: '{"Hello":2}' }),
      f({ dir: "in", kind: "ping", data: "" }),
      f({ dir: "in", kind: "event", event: "chat:message", data: "[]" }),
    ];
    expect(filterLog(log, EMPTY_FILTER)).toBe(log);
    expect(filterLog(log, { ...EMPTY_FILTER, text: "hello" })).toHaveLength(2);
    expect(filterLog(log, { ...EMPTY_FILTER, dir: "in" })).toHaveLength(3);
    expect(filterLog(log, { ...EMPTY_FILTER, dir: "system" })).toHaveLength(1);
    expect(filterLog(log, { ...EMPTY_FILTER, hideHeartbeats: true })).toHaveLength(4);
    expect(filterLog(log, { ...EMPTY_FILTER, text: "CHAT:" })).toEqual([log[4]]);
    expect(filterLog(log, { ...EMPTY_FILTER, text: "hello", dir: "out" })).toEqual([log[1]]);
  });

  it("previews and expands frames", () => {
    expect(frameSummary(f({ kind: "text", data: "a\n  b" }))).toBe("a b");
    expect(frameSummary(f({ kind: "binary", data: "AAH/" }))).toBe("Binary · 00 01 ff");
    expect(frameSummary(f({ kind: "event", event: "chat", ackId: 3, data: '[{"t":"hi"},2]' }))).toBe('chat #3 {"t":"hi"},2');
    expect(frameSummary(f({ kind: "ack", ackId: 0, data: "[]" }))).toBe("Ack #0");
    expect(frameSummary(f({ kind: "close", dir: "in", code: 1000, data: "bye" }))).toBe("Close 1000 · bye");
    expect(frameSummary(f({ kind: "pong", data: "aGk=" }))).toBe("Pong · hi");
    expect(base64Text("aGk=")).toBe("hi");
    expect(base64Text("/w==")).toBeNull();
    const d = frameDetail(f({ kind: "text", data: '{"a":[1,2]}' }));
    expect(d).toEqual({ text: '{\n  "a": [\n    1,\n    2\n  ]\n}', json: true });
    expect(frameDetail(f({ kind: "binary", data: "/w==" })).text).toBe("ff");
    expect(frameDetail(f({ dir: "system", kind: "info", data: "{}" })).json).toBe(false);
    const big = f({ kind: "text", data: JSON.stringify({ a: 1, b: 2, c: 3 }) });
    expect(rowHeight(big, true)).toBeGreaterThan(rowHeight(big, false));
    expect(summaryText({ durationMs: 4200, sent: 2, received: 3 })).toBe("4.2 s · 2 sent · 3 received");
  });
});

// ───────────── mock backend ─────────────

let server: MockServer;
let call: ReturnType<typeof makeCall>;

async function rejects(p: Promise<unknown>): Promise<CommandError> {
  try {
    await p;
  } catch (e) {
    return e as CommandError;
  }
  throw new Error("expected rejection");
}

function wsRequest(url: string, sio = false): Request {
  const r = scratchRequest({ url });
  switchKind(r, sio ? "sio" : "ws");
  return r;
}

async function until(frames: WsFrame[], pred: (f: WsFrame) => boolean): Promise<WsFrame> {
  for (let i = 0; i < 200; i++) {
    const hit = frames.find(pred);
    if (hit) return hit;
    await sleep(5);
  }
  throw new Error(`no matching frame in ${JSON.stringify(frames)}`);
}

beforeEach(() => {
  server = new MockServer({ latency: 0, platform: "linux" });
  call = makeCall((cmd, args) => server.invoke(cmd, args));
});

describe("mock WebSocket server (AC-WS-2..7)", () => {
  it("connects, echoes, pings, closes and writes one history entry", async () => {
    const frames: WsFrame[] = [];
    const onEvent = new MockChannel<WsFrame>((fr) => frames.push(fr));
    const res = await call("ws_connect", { connectionId: "c1", request: wsRequest("localhost:9000/chat?x={{$uuid}}"), onEvent });
    expect(res.method).toBe("WS");
    expect(res.resolvedUrl).toMatch(/^ws:\/\/localhost:9000\/chat\?x=[0-9a-f-]{36}$/);
    await until(frames, (x) => x.kind === "open");
    expect((await rejects(call("ws_send", { connectionId: "nope", message: { name: "", kind: "text", body: "x" } }))).code).toBe("ws_not_connected");
    expect((await rejects(call("ws_send", { connectionId: "c1", message: { name: "", kind: "json", body: "{" } }))).code).toBe("ws_invalid_message");
    expect(await call("ws_send", { connectionId: "c1", message: { name: "", kind: "text", body: "hi {{$randomInt}}" } })).toEqual({ ackId: null });
    const echo = await until(frames, (x) => x.dir === "in" && x.kind === "text" && x.data.startsWith("hi "));
    expect(echo.data).toMatch(/^hi \d+$/);
    await call("ws_send", { connectionId: "c1", message: { name: "", kind: "binary", body: "AAH/" } });
    await until(frames, (x) => x.dir === "in" && x.kind === "binary" && x.data === "AAH/");
    expect(await call("ws_ping", { connectionId: "c1" })).toBe(true);
    await until(frames, (x) => x.kind === "pong");
    expect((await rejects(call("ws_close", { connectionId: "c1", code: 1006 }))).code).toBe("invalid");
    expect(await call("ws_close", { connectionId: "c1", code: 4000, reason: "done" })).toBe(true);
    const last = await until(frames, (x) => !!x.last);
    expect(last).toMatchObject({ dir: "system", kind: "close", code: 4000 });
    expect(frames.indexOf(last)).toBe(frames.length - 1);
    expect(await call("ws_close", { connectionId: "c1" })).toBe(false);
    const h = await call("list_history", {});
    expect(h[0]).toMatchObject({ method: "WS", status: null, ws: { sent: 2, received: 3, closeCode: 4000, closeReason: "done" } });
    const opened = await call("open_history_entry", { historyId: res.historyId });
    expect(opened.entry.ws?.sent).toBe(2);
    expect(opened.response).toBeNull();
  });

  it("Socket.IO: echoes events and acknowledges", async () => {
    const frames: WsFrame[] = [];
    const r = wsRequest("http://localhost:3000", true);
    r.ws!.socketIo!.namespace = "chat";
    const res = await call("ws_connect", { connectionId: "s1", request: r, onEvent: new MockChannel<WsFrame>((fr) => frames.push(fr)) });
    expect(res.method).toBe("SIO");
    expect(res.resolvedUrl).toBe("ws://localhost:3000/socket.io/?EIO=4&transport=websocket");
    const open = await until(frames, (x) => x.kind === "open");
    expect(open.data).toContain("namespace /chat");
    expect((await rejects(call("ws_send", { connectionId: "s1", message: { name: "", kind: "json", body: "{}", event: "disconnect" } }))).message).toMatch(/reserved/);
    expect(await call("ws_send", { connectionId: "s1", message: { name: "", kind: "json", body: '{"t":"hi"}', event: "say", ack: true } })).toEqual({ ackId: 0 });
    expect(await until(frames, (x) => x.kind === "ack")).toMatchObject({ dir: "in", ackId: 0, data: '[{"t":"hi"}]' });
    expect(await until(frames, (x) => x.dir === "in" && x.kind === "event")).toMatchObject({ event: "say", data: '[{"t":"hi"}]' });
    await call("ws_close", { connectionId: "s1" });
    await until(frames, (x) => !!x.last);
    expect((await call("list_history", {}))[0]).toMatchObject({ method: "SIO", ws: { sent: 1, received: 2 } });
  });

  it("reports failed connects and validation problems", async () => {
    const frames: WsFrame[] = [];
    await call("ws_connect", { connectionId: "f1", request: wsRequest("ws://refused.test/"), onEvent: new MockChannel<WsFrame>((fr) => frames.push(fr)) });
    const err = await until(frames, (x) => x.kind === "error");
    expect(err.data).toMatch(/Connection refused/);
    await until(frames, (x) => !!x.last);
    expect((await call("list_history", {}))[0].errorKind).toBe("connect");

    const ch = new MockChannel<WsFrame>(() => {});
    expect((await rejects(call("ws_connect", { connectionId: "f2", request: wsRequest("ftp://x"), onEvent: ch }))).details).toMatchObject({ kind: "invalidWsUrl" });
    expect((await rejects(call("ws_connect", { connectionId: "f3", request: scratchRequest({ url: "ws://x" }), onEvent: ch }))).details).toEqual({ kind: "notWebSocket" });
    const bad = wsRequest("ws://x", true);
    bad.ws!.socketIo!.auth = "[1]";
    expect((await rejects(call("ws_connect", { connectionId: "f4", request: bad, onEvent: ch }))).details).toMatchObject({ kind: "invalidSocketIoAuth" });
    expect((await rejects(call("send_request", { executionId: "e", request: wsRequest("ws://x") }))).details).toEqual({ kind: "notHttp" });
    expect((await rejects(call("export_curl", { request: wsRequest("ws://x"), keepVariables: true }))).details).toEqual({ kind: "notHttp" });
  });

  it("saving stamps minClientVersion and keeps ws when the UI drops it", async () => {
    const b = await call("bootstrap", {});
    const tree = await call("get_project_tree", { projectId: b.projects[0].id });
    const col = tree.collections[0];
    const r = wsRequest("ws://echo");
    delete r.minClientVersion;
    r.ws!.messages.push({ name: "hello", kind: "text", body: "hi" });
    const created = await call("create_request", { collectionId: col.id, request: r });
    expect(created.minClientVersion).toBe("0.1.0");
    const { ws: _drop, ...rest } = created;
    const saved = await call("save_request", { request: { ...rest, url: "ws://echo2" } as Request });
    expect(saved.ws?.messages[0].body).toBe("hi");
    expect(saved.url).toBe("ws://echo2");
  });
});

describe("ws store", () => {
  it("streams frames into the session log and counts messages", async () => {
    server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = wsRequest("ws://localhost:1234/");
    await ws.connect(r, null);
    let s = ws.session(r.id)!;
    expect(s.state).toBe("connecting");
    for (let i = 0; i < 100 && ws.session(r.id)!.state !== "open"; i++) await sleep(10);
    s = ws.session(r.id)!;
    expect(s.state).toBe("open");
    await ws.send(r.id, { name: "", kind: "text", body: "ping me" });
    for (let i = 0; i < 100 && ws.session(r.id)!.received < 2; i++) await sleep(10);
    expect(ws.session(r.id)!.sent).toBe(1);
    expect(ws.session(r.id)!.received).toBe(2);
    expect(ws.frames(r.id).map((x) => `${x.dir}:${x.kind}`)).toEqual(["system:open", "in:text", "out:text", "in:text"]);
    let ended = 0;
    ws.onEnded = () => ended++;
    await ws.disconnect(r.id);
    for (let i = 0; i < 100 && ws.session(r.id)!.state !== "closed"; i++) await sleep(10);
    expect(ws.session(r.id)!.state).toBe("closed");
    expect(ended).toBe(1);
    expect(ws.isLive(r.id)).toBe(false);
    ws.clear(r.id);
    expect(ws.frames(r.id)).toEqual([]);
    // Connect failure from validation: the session closes with the message.
    const bad = wsRequest("ftp://x");
    await expect(ws.connect(bad, null)).rejects.toMatchObject({ code: "validation" });
    expect(ws.session(bad.id)).toMatchObject({ state: "closed", error: expect.stringMatching(/Invalid WebSocket URL/) });
  });
});

describe("log memory (byte budget, cached search)", () => {
  it("trims the oldest frames past the byte budget or the count", () => {
    const big = "x".repeat(1024 * 1024);
    const log = Array.from({ length: 40 }, (_, i) => logEntry(f({ data: big }), i));
    const bytes = log.reduce((n, e) => n + e.cost, 0);
    expect(bytes).toBeGreaterThan(LOG_MAX_BYTES);
    const t = trimLog(log, bytes);
    expect(t.log).not.toBe(log); // a new array: rows on screen never shift under the list
    expect(t.bytes).toBeLessThanOrEqual(LOG_MAX_BYTES * 0.9);
    expect(t.bytes).toBe(t.log.reduce((n, e) => n + e.cost, 0));
    expect(t.log[t.log.length - 1].seq).toBe(39);
    expect(t.log[0].seq).toBe(40 - t.log.length);
    // Within both limits: the same array, untouched.
    const small = [logEntry(f({ data: "a" }), 0)];
    expect(trimLog(small, small[0].cost).log).toBe(small);
    const many = Array.from({ length: 12 }, (_, i) => logEntry(f({ data: "a" }), i));
    const m = trimLog(many, many.reduce((n, e) => n + e.cost, 0), LOG_MAX_BYTES, 10);
    expect(m.log.map((e) => e.seq)).toEqual([3, 4, 5, 6, 7, 8, 9, 10, 11]);
  });

  it("filters on a cached lowercased preview of the payload", () => {
    const e = logEntry(f({ kind: "event", event: "Chat:Msg", data: `["HELLO${"x".repeat(SEARCH_PREVIEW)}TAIL"]` }), 1);
    expect(e.search.length).toBeLessThanOrEqual(SEARCH_PREVIEW + 10);
    expect(filterLog([e], { ...EMPTY_FILTER, text: "chat:msg" })).toHaveLength(1);
    expect(filterLog([e], { ...EMPTY_FILTER, text: "hello" })).toHaveLength(1);
    // Past the preview isn't searched (the full payload is never re-lowercased).
    expect(filterLog([e], { ...EMPTY_FILTER, text: "tail" })).toHaveLength(0);
  });

  it("the store keeps a session log within the byte budget", async () => {
    server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = wsRequest("ws://localhost:1234/");
    await ws.connect(r, null);
    const s = ws.session(r.id)!;
    const big = "y".repeat(MAX_FRAME_DATA);
    for (let i = 0; i < 200; i++) ws.push(s.connectionId, f({ data: big }));
    ws.flush();
    const frames = ws.frames(r.id);
    expect(frames.reduce((n, e) => n + e.cost, 0)).toBeLessThanOrEqual(LOG_MAX_BYTES);
    expect(frames.length).toBeLessThan(200);
    await ws.discard([r.id]);
  });

  it("the mock cuts frames past MAX_FRAME_DATA like core", async () => {
    const frames: WsFrame[] = [];
    const r = wsRequest("ws://localhost:9000/");
    await call("ws_connect", { connectionId: "t1", request: r, onEvent: new MockChannel<WsFrame>((fr) => frames.push(fr)) });
    await until(frames, (x) => x.kind === "open");
    await call("ws_send", { connectionId: "t1", message: { name: "", kind: "text", body: "z".repeat(MAX_FRAME_DATA + 10) } });
    const echo = await until(frames, (x) => x.dir === "in" && x.kind === "text" && x.data.startsWith("zzz"));
    expect(echo.truncated).toBe(true);
    expect(echo.data.length).toBe(MAX_FRAME_DATA);
    expect(echo.size).toBe(MAX_FRAME_DATA + 10);
    await call("ws_close", { connectionId: "t1" });
  });
});

describe("session lifecycle (QA)", () => {
  it("Cancel before ws_connect resolves still closes the connection", async () => {
    server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = wsRequest("ws://localhost:1234/");
    const connecting = ws.connect(r, null);
    await ws.disconnect(r.id); // ws_connect hasn't reached the backend yet
    await connecting;
    for (let i = 0; i < 100 && ws.session(r.id)!.state !== "closed"; i++) await sleep(10);
    expect(ws.session(r.id)!.state).toBe("closed");
    expect(ws.isLive(r.id)).toBe(false);
    expect([...server.ws.conns.values()].every((c) => c.state === "closed")).toBe(true);
  });

  it("deleting requests closes and forgets their sessions; liveIds feeds the tree dot", async () => {
    server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = wsRequest("ws://localhost:1234/");
    await ws.connect(r, null);
    for (let i = 0; i < 100 && ws.session(r.id)!.state !== "open"; i++) await sleep(10);
    expect(ws.liveIds().has(r.id)).toBe(true);
    const conn = server.ws.conns.get(ws.session(r.id)!.connectionId)!;
    ws.setDraft(r.id, { name: "", kind: "text", body: "draft" });
    await ws.discard([r.id]);
    expect(ws.session(r.id)).toBeNull();
    expect(ws.drafts[r.id]).toBeUndefined();
    expect(ws.liveIds().has(r.id)).toBe(false);
    for (let i = 0; i < 100 && conn.state !== "closed"; i++) await sleep(10);
    expect(conn.state).toBe("closed");
  });

  it("requestsInSubtree lists every request a delete removes", () => {
    const req = (id: string, collectionId: string, folderId: string | null) => ({ ...scratchRequest({}), id, collectionId, folderId });
    const tree = {
      project: {},
      collections: [{ id: "c1", sortOrder: 0 }, { id: "c2", sortOrder: 1 }],
      folders: [
        { id: "f1", collectionId: "c1", parentFolderId: null, sortOrder: 0 },
        { id: "f2", collectionId: "c1", parentFolderId: "f1", sortOrder: 0 },
      ],
      requests: [req("r0", "c1", null), req("r1", "c1", "f1"), req("r2", "c1", "f2"), req("r3", "c2", null)],
    };
    const ix = indexTree(tree as never);
    expect(requestsInSubtree(ix, "collection", "c1").sort()).toEqual(["r0", "r1", "r2"]);
    expect(requestsInSubtree(ix, "folder", "f1").sort()).toEqual(["r1", "r2"]);
    expect(requestsInSubtree(ix, "request", "r3")).toEqual(["r3"]);
    expect(requestsInSubtree(ix, "request", "nope")).toEqual([]);
  });
});

describe("share without credentials (mirror core sanitize_request)", () => {
  it("replaces the Socket.IO auth payload and counts it", async () => {
    const b = await call("bootstrap", {});
    const tree = await call("get_project_tree", { projectId: b.projects[0].id });
    const r = wsRequest("ws://chat", true);
    r.ws!.socketIo!.auth = '{"token":"sio-secret"}';
    const created = await call("create_request", { collectionId: tree.collections[0].id, request: r });
    const without = await call("share_preview", { kind: "request", entityId: created.id, includeCredentials: false });
    const withCreds = await call("share_preview", { kind: "request", entityId: created.id, includeCredentials: true });
    expect(without.credentialsReplaced).toBe(withCreds.credentialFields);
    expect(without.credentialsReplaced).toBeGreaterThanOrEqual(1);
    const snap = server["snapshotFor"]("request", created.id, false, null).snapshot;
    expect(JSON.stringify(snap)).not.toContain("sio-secret");
    expect(snap.request?.ws?.socketIo?.auth).toBe("{{socketIoAuth}}");
    expect(JSON.stringify(server["snapshotFor"]("request", created.id, true, null).snapshot)).toContain("sio-secret");
  });
});

describe("QA pass 2", () => {
  it("reconnecting a live session leaves no bookkeeping behind", async () => {
    server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = wsRequest("ws://localhost:1234/");
    await sleep(100); // let earlier tests' sessions wind down
    ws.flush();
    const base = ws.debugCounts();
    for (let i = 0; i < 3; i++) {
      await ws.connect(r, null);
      for (let k = 0; k < 100 && ws.session(r.id)!.state !== "open"; k++) await sleep(10);
    }
    await ws.disconnect(r.id);
    for (let k = 0; k < 100 && ws.session(r.id)!.state !== "closed"; k++) await sleep(10);
    await sleep(50);
    ws.flush();
    expect(ws.debugCounts().registered).toBe(base.registered);
    expect(ws.debugCounts().discarded).toBe(base.discarded);
    expect(ws.debugCounts().cancelPending).toBe(base.cancelPending);
    expect([...server.ws.conns.values()].every((c) => c.state === "closed")).toBe(true);
    ws.forget(r.id);
  });

  it("an undefined variable in the Socket.IO auth payload is named (mirror core)", async () => {
    const r = wsRequest("ws://x", true);
    r.ws!.socketIo!.auth = "{{socketIoAuth}}";
    const e = await rejects(call("ws_connect", { connectionId: "u1", request: r, onEvent: new MockChannel<WsFrame>(() => {}) }));
    expect(e.message).toBe("Define {{socketIoAuth}} (the shared Socket.IO auth payload) in your environment");
    expect(e.details).toEqual({ kind: "undefinedSocketIoAuthVariable", message: "socketIoAuth" });
    r.ws!.socketIo!.auth = '{"t":"{{tok}}"}';
    expect((await rejects(call("ws_connect", { connectionId: "u2", request: r, onEvent: new MockChannel<WsFrame>(() => {}) }))).details).toEqual({
      kind: "undefinedSocketIoAuthVariable",
      message: "tok",
    });
  });

  it("the filter hint says only the first 8 KB is searched", () => {
    expect(FILTER_HINT).toContain("first 8 KB of each message");
    expect(SEARCH_PREVIEW).toBe(8 * 1024);
  });
});

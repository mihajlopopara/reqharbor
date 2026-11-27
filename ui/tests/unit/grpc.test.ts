import { describe, expect, it } from "vitest";
import { MockServer } from "../../src/lib/api/mock";
import { MockChannel } from "../../src/lib/api/mock/ws";
import { demoDescriptor } from "../../src/lib/api/mock/grpc";
import { setBackend } from "../../src/lib/api";
import { makeCall, type CommandError, type GrpcFrame, type Request } from "../../src/lib/api/types";
import { readOnlyReason, scratchRequest } from "../../src/lib/util/request";
import { requestBadge, stampMinClientVersion, switchKind, nextState, frameSummary } from "../../src/lib/util/ws";
import {
  GRPC_MIN_CLIENT_VERSION,
  defaultGrpc,
  filterMethods,
  findMethod,
  grpcTarget,
  isStreaming,
  sendsMessages,
  shortName,
  statusName,
  stampAtLeast,
  tryTarget,
} from "../../src/lib/util/grpc";
import { exampleMessage, exampleValue, fieldTypeText, jsonCompletions } from "../../src/lib/grpc/schema";
import { grpc as grpcStore, descriptorError } from "../../src/lib/stores/grpc.svelte";
import { ws } from "../../src/lib/stores/ws.svelte";

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const D = demoDescriptor("grpc://localhost:50051", 1, "reflection");

function grpcRequest(url = "grpc://localhost:50051", method = "SayHello", message = '{"name":"Ada"}'): Request {
  const r = scratchRequest({ url });
  switchKind(r, "grpc");
  r.grpc!.service = method === "Fail" ? "demo.Failing" : "demo.Greeter";
  r.grpc!.method = method;
  r.grpc!.message = message;
  return r;
}

async function until(ok: () => boolean, ms = 2000) {
  for (let i = 0; i < ms / 10 && !ok(); i++) await sleep(10);
}

describe("gRPC helpers (AC-GRPC-1/2)", () => {
  it("switches kind, keeps grpc and stamps a fixed minClientVersion", () => {
    const r = scratchRequest();
    switchKind(r, "grpc");
    expect(r.kind).toBe("grpc");
    expect(r.grpc).toEqual(defaultGrpc());
    expect(r.minClientVersion).toBe(GRPC_MIN_CLIENT_VERSION);
    r.grpc!.service = "demo.Greeter";
    switchKind(r, "http", "POST");
    expect(r.kind).toBeUndefined();
    expect(r.grpc?.service).toBe("demo.Greeter");
    switchKind(r, "grpc");
    expect(r.grpc?.service).toBe("demo.Greeter");
    expect(stampAtLeast("9.0.0", "0.1.0")).toBe("9.0.0");
    expect(stampAtLeast("0.0.1", "0.1.0")).toBe("0.1.0");
    const g = grpcRequest();
    delete g.minClientVersion;
    stampMinClientVersion(g);
    expect(g.minClientVersion).toBe("0.1.0");
    expect(requestBadge(g)).toEqual({ label: "gRPC", cls: "m-grpc" });
  });

  it("is editable unless the proto source comes from a newer build", () => {
    const r = grpcRequest();
    expect(readOnlyReason(r, "0.1.0")).toBeNull();
    r.grpc!.protoSource = "registry";
    expect(readOnlyReason(r, "0.1.0")).toMatch(/registry/);
  });

  it("derives TLS from the scheme like core", () => {
    expect(grpcTarget("grpcs://api.dev")).toMatchObject({ tls: true, port: 443, origin: "grpcs://api.dev:443" });
    expect(grpcTarget("grpc://localhost:50051")).toMatchObject({ tls: false, port: 50051 });
    expect(grpcTarget("https://x.dev/prefix").extra).toBe(true);
    expect(grpcTarget("localhost:9000").tls).toBe(false);
    expect(grpcTarget("api.dev").tls).toBe(true);
    expect(() => grpcTarget("ws://x")).toThrow(/Use grpc/);
    // Userinfo is dropped like core does.
    expect(grpcTarget("grpc://user@api.dev:9000")).toMatchObject({ host: "api.dev", port: 9000, origin: "grpc://api.dev:9000" });
    expect(grpcTarget("grpcs://user:pw@api.dev/x").origin).toBe("grpcs://api.dev:443");
    expect(grpcTarget("grpc://u@[::1]:7").host).toBe("[::1]");
    expect(tryTarget("")).toBeNull();
  });

  it("names statuses and call kinds", () => {
    expect(statusName(0)).toBe("OK");
    expect(statusName(14)).toBe("UNAVAILABLE");
    expect(statusName(42)).toBe("CODE_42");
    expect(sendsMessages("bidiStreaming")).toBe(true);
    expect(sendsMessages("serverStreaming")).toBe(false);
    expect(isStreaming("serverStreaming")).toBe(true);
    expect(isStreaming("unary")).toBe(false);
    expect(shortName("demo.Greeter")).toBe("Greeter");
  });

  it("searches services and methods for the picker (AC-GRPC-5)", () => {
    expect(filterMethods(D.services, "").map((g) => g.service)).toEqual(["demo.Failing", "demo.Greeter"]);
    const hits = filterMethods(D.services, "lots");
    expect(hits).toEqual([{ service: "demo.Greeter", methods: [expect.objectContaining({ name: "LotsOfGreetings" }), expect.objectContaining({ name: "LotsOfReplies" })] }]);
    expect(filterMethods(D.services, "GREETER")[0].methods).toHaveLength(4);
    expect(filterMethods(D.services, "nothing")).toEqual([]);
    expect(findMethod(D, "demo.Greeter", "Chat")?.callKind).toBe("bidiStreaming");
    expect(findMethod(D, "demo.Greeter", "Nope")).toBeNull();
  });
});

describe("example message and completion (AC-GRPC-6)", () => {
  it("fills every field with a sample", () => {
    const now = new Date("2026-11-27T10:00:00Z");
    const v = exampleValue(D, "demo.HelloRequest", now) as Record<string, unknown>;
    expect(v.name).toBe("");
    expect(v.times).toBe(0);
    expect(v.mood).toBe("MOOD_UNSPECIFIED");
    expect(v.tags).toEqual([""]);
    expect(v.homeAddress).toEqual({ street: "", city: "" });
    expect(Object.keys(v.labels as object)).toHaveLength(1);
    expect(v.sentAt).toBe("2026-11-27T10:00:00.000Z");
    expect(JSON.parse(exampleMessage(D, "demo.HelloRequest", now))).toEqual(v);
  });

  it("offers field names at key positions, without the ones present", () => {
    const text = '{"name": "x", ';
    const c = jsonCompletions(D, "demo.HelloRequest", text, text.length)!;
    const labels = c.options.map((o) => o.label);
    expect(labels).toContain("homeAddress");
    expect(labels).not.toContain("name");
    const nested = '{"homeAddress": {"';
    const n = jsonCompletions(D, "demo.HelloRequest", nested, nested.length)!;
    expect(n.options.map((o) => o.label).sort()).toEqual(["city", "street"]);
  });

  it("offers enum names at value positions", () => {
    const text = '{"mood": ';
    const c = jsonCompletions(D, "demo.HelloRequest", text, text.length)!;
    expect(c.options.some((o) => o.label.includes("HAPPY"))).toBe(true);
    expect(fieldTypeText(D.messages["demo.HelloRequest"].fields.find((f) => f.name === "labels")!)).toBe("map<string, string>");
    expect(fieldTypeText(D.messages["demo.HelloRequest"].fields.find((f) => f.name === "tags")!)).toBe("repeated string");
  });
});

describe("mock backend (API_CONTRACT §3.16)", () => {
  const setup = () => {
    const server = new MockServer({ latency: 0 });
    const call = makeCall((cmd, args) => server.backend().invoke(cmd, args));
    return { server, call };
  };

  it("reflects, caches and forgets services", async () => {
    const { call } = setup();
    const r = grpcRequest();
    expect((await call("grpc_cached_descriptor", { request: r })).descriptor).toBeNull();
    const d = await call("grpc_fetch_descriptor", { request: r });
    expect(d.services.map((s) => s.name)).toContain("demo.Greeter");
    const again = await call("grpc_cached_descriptor", { request: r, known: { url: d.url, fetchedAt: d.fetchedAt } });
    expect(again).toMatchObject({ unchanged: true, descriptor: null, url: "grpc://localhost:50051" });
    expect(await call("grpc_clear_descriptor", { request: r })).toBe(true);
    const none = grpcRequest("grpc://noreflection.test:50051");
    await expect(call("grpc_fetch_descriptor", { request: none })).rejects.toMatchObject({ code: "grpc_reflection_unsupported" });
    const files = grpcRequest();
    files.grpc!.protoSource = "files";
    files.grpc!.protoFiles = ["/p/greeter.proto"];
    expect((await call("grpc_cached_descriptor", { request: files })).descriptor?.source).toBe("files");
    files.grpc!.protoFiles = ["/p/invalid.proto"];
    await expect(call("grpc_cached_descriptor", { request: files })).rejects.toMatchObject({ code: "grpc_proto_invalid" });
  });

  it("invokes unary calls with status, headers and history", async () => {
    const { call } = setup();
    const res = await call("grpc_invoke", { executionId: "e1", request: grpcRequest() });
    expect(res.status).toMatchObject({ code: 0, name: "OK" });
    expect(JSON.parse(res.response!).message).toMatch(/Ada/);
    expect(res.headers.length).toBeGreaterThan(0);
    const fail = await call("grpc_invoke", { executionId: "e2", request: grpcRequest(undefined, "Fail", "{}") });
    expect(fail.status).toMatchObject({ code: 5, name: "NOT_FOUND" });
    expect(fail.status!.details[0].typeUrl).toMatch(/BadRequest/);
    const refused = await call("grpc_invoke", { executionId: "e3", request: grpcRequest("grpc://refused.test:9") });
    expect(refused.status).toBeNull();
    expect(refused.error?.kind).toBe("connect");
    const list = await call("list_history", {});
    expect(list[0]).toMatchObject({ method: "GRPC", grpc: expect.objectContaining({ callKind: "unary" }) });
    expect(list[0].grpc).not.toHaveProperty("response");
    const opened = await call("open_history_entry", { historyId: list[2].id });
    expect(opened.entry.grpc?.response).toMatch(/Ada/);
  });

  it("refuses to invoke a streaming method, like the core (grpcStreamingMethod)", async () => {
    const { call } = setup();
    for (const [method, callKind] of [["Chat", "bidiStreaming"], ["LotsOfGreetings", "clientStreaming"], ["LotsOfReplies", "serverStreaming"]] as const) {
      const err = (await call("grpc_invoke", { executionId: "s", request: grpcRequest(undefined, method, "{}") }).catch((e) => e)) as CommandError;
      expect(err.code).toBe("validation");
      expect(err.details).toEqual({ kind: "grpcStreamingMethod", callKind });
    }
    expect((await call("list_history", {})).filter((h) => h.method === "GRPC")).toHaveLength(0);
    // The services it reflected are cached now.
    expect((await call("grpc_cached_descriptor", { request: grpcRequest() })).descriptor).not.toBeNull();
  });

  it("refuses bad input before sending", async () => {
    const { call } = setup();
    const noMethod = grpcRequest();
    noMethod.grpc!.method = "";
    await expect(call("grpc_invoke", { executionId: "x", request: noMethod })).rejects.toMatchObject({ details: { kind: "noGrpcMethod" } });
    const badMsg = grpcRequest(undefined, "SayHello", '{"nmae":1}');
    await expect(call("grpc_invoke", { executionId: "x", request: badMsg })).rejects.toMatchObject({ details: { kind: "invalidGrpcMessage" } });
    const md = grpcRequest();
    md.headers = [{ key: "te", value: "trailers", enabled: true }];
    await expect(call("grpc_invoke", { executionId: "x", request: md })).rejects.toMatchObject({ details: { kind: "invalidGrpcMetadata" } });
    await expect(call("send_request", { executionId: "x", request: grpcRequest() })).rejects.toMatchObject({ details: { kind: "notHttp" } });
    const err = (await call("grpc_send", { connectionId: "nope", message: "{}" }).catch((e) => e)) as CommandError;
    expect(err.code).toBe("grpc_not_connected");
  });

  it("streams a bidi call over the channel and writes one history entry", async () => {
    const { call, server } = setup();
    const frames: GrpcFrame[] = [];
    const onEvent = new MockChannel((f) => frames.push(f as GrpcFrame));
    const r = grpcRequest(undefined, "Chat", "");
    const res = await call("grpc_start", { connectionId: "c1", request: r, onEvent: onEvent as never });
    expect(res.callKind).toBe("bidiStreaming");
    await until(() => frames.some((f) => f.kind === "open"));
    await call("grpc_send", { connectionId: "c1", message: '{"user":"me","text":"hi"}' });
    await until(() => frames.filter((f) => f.kind === "message").length >= 2);
    expect(await call("grpc_end", { connectionId: "c1" })).toBe(true);
    await until(() => frames.some((f) => f.last));
    const kinds = frames.map((f) => `${f.dir}:${f.kind}`);
    expect(kinds[0]).toBe("system:open");
    expect(kinds).toContain("out:message");
    expect(kinds).toContain("in:message");
    expect(kinds).toContain("out:end");
    expect(kinds.slice(-2)).toEqual(["system:status", "system:close"]);
    expect(frames.at(-1)!.data).toBe("Finished · 0 OK");
    const h = (await call("list_history", {}))[0];
    expect(h.grpc).toMatchObject({ callKind: "bidiStreaming", sent: 1, received: 1 });
    expect(server).toBeTruthy();
  });

  it("exports grpcurl and empties proto paths in shares", async () => {
    const { call } = setup();
    const r = grpcRequest("grpcs://api.dev");
    r.auth = { type: "bearer", token: "tok" };
    const out = await call("export_grpcurl", { request: r, keepVariables: false });
    expect(out.split(" \\\n  ")).toEqual(["grpcurl", "-H 'authorization: Bearer tok'", `-d '{"name":"Ada"}'`, "-max-time 30", "'api.dev:443'", "'demo.Greeter/SayHello'"]);
  });
});

describe("gRPC streams in the session store (AC-GRPC-8)", () => {
  it("frames drive state, counters and the half-closed flag", () => {
    const f = (p: Partial<GrpcFrame>): GrpcFrame => ({ dir: "system", ts: 0, kind: "info", data: "", size: 0, ...p });
    expect(nextState("connecting", f({ kind: "open" }))).toBe("open");
    expect(nextState("open", f({ dir: "out", kind: "end" }))).toBe("open");
    expect(nextState("open", f({ kind: "close", last: true }))).toBe("closed");
    expect(frameSummary(f({ dir: "in", kind: "message", data: '{\n  "a": 1\n}' }))).toBe('{ "a": 1 }');
  });

  it("runs a server stream end to end with the mock", async () => {
    const server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = grpcRequest(undefined, "LotsOfReplies", '{"name":"Bo"}');
    await ws.startGrpc(r, null, "serverStreaming");
    await until(() => ws.session(r.id)?.state === "closed");
    const s = ws.session(r.id)!;
    expect(s).toMatchObject({ transport: "grpc", method: "GRPC", callKind: "serverStreaming", sent: 1, received: 3, halfClosed: true });
    expect(ws.frames(r.id).at(-1)?.data).toBe("Finished · 0 OK");
  });

  it("Disconnect while grpc_start is still reflecting cancels it at once", async () => {
    const server = new MockServer({ latency: 1 });
    setBackend(server.backend());
    // Nothing cached: the start reflects first (the mock takes ~120 ms).
    const r = grpcRequest("grpc://fresh.test:50051", "Chat", "{}");
    const starting = ws.startGrpc(r, null, "bidiStreaming");
    await sleep(10);
    expect(ws.session(r.id)?.state).toBe("connecting");
    const t0 = Date.now();
    await ws.disconnect(r.id);
    const rejected = (await starting.then(() => null, (e) => e)) as CommandError | null;
    expect(rejected).toMatchObject({ code: "grpc_network", details: { kind: "cancelled" } });
    await until(() => ws.session(r.id)?.state === "closed");
    // It never got to open: the pending reflection was aborted, not waited out.
    expect(Date.now() - t0).toBeLessThan(110);
    expect(ws.session(r.id)?.error ?? ws.session(r.id)?.state).toBeTruthy();
    expect(ws.frames(r.id).some((f) => f.kind === "open")).toBe(false);
  });

  it("keeps descriptor errors readable", async () => {
    expect(descriptorError({ code: "grpc_network", message: "x", details: { kind: "cancelled" } })).toBeNull();
    expect(descriptorError({ code: "grpc_reflection_unsupported", message: "m" })?.kind).toBe("unsupported");
    const server = new MockServer({ latency: 0 });
    setBackend(server.backend());
    const r = grpcRequest();
    await grpcStore.fetch(r, null);
    expect(grpcStore.callKind(r)).toBe("unary");
    await grpcStore.refreshCached(grpcRequest("grpc://other:1"), null);
  });
});

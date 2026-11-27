import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { setBackend } from "../../src/lib/api";
import { MockServer } from "../../src/lib/api/mock";
import { app } from "../../src/lib/stores/app.svelte";
import { grpc } from "../../src/lib/stores/grpc.svelte";
import { ws } from "../../src/lib/stores/ws.svelte";
import { switchKind } from "../../src/lib/util/ws";

const server = new MockServer({ latency: 0, platform: "macos" });

beforeAll(async () => {
  setBackend(server.backend());
  await app.init();
});
afterAll(() => app.destroy());

const until = async (cond: () => boolean, ms = 2000) => {
  const t = Date.now();
  while (!cond()) {
    if (Date.now() - t > ms) throw new Error("timeout");
    await new Promise((r) => setTimeout(r, 5));
  }
};

/** A new gRPC request whose services aren't cached (a fresh device). */
function open(method: string, message = '{"name":"Ada"}') {
  app.newRequest();
  switchKind(app.request, "grpc");
  app.request.url = "grpc://localhost:50051";
  app.request.grpc!.service = "demo.Greeter";
  app.request.grpc!.method = method;
  app.request.grpc!.message = message;
}

describe("Invoke without cached services (AC-GRPC-7/8)", () => {
  it("a bidi method isn't invoked: the button becomes Connect", async () => {
    open("Chat");
    const id = app.request.id;
    expect(app.grpcCallKind()).toBeNull();
    await app.grpcRun();
    expect(app.inflight[id]).toBeUndefined();
    expect(ws.session(id)).toBeNull();
    await until(() => app.grpcCallKind() === "bidiStreaming");
    expect(app.sendBlocked).toMatch(/Connect/);
    // The services the call fetched are shown.
    await until(() => !!grpc.descriptor(id));
    // The next press connects.
    await app.grpcRun();
    await until(() => ws.session(id)?.transport === "grpc" && ws.isLive(id));
    expect(ws.session(id)?.callKind).toBe("bidiStreaming");
    await ws.disconnect(id);
  });

  it("a server-streaming method starts as a stream by itself", async () => {
    open("LotsOfReplies");
    const id = app.request.id;
    await app.grpcRun();
    await until(() => ws.session(id)?.state === "closed");
    expect(ws.session(id)).toMatchObject({ transport: "grpc", callKind: "serverStreaming", received: 3 });
  });

  it("a unary method still invokes and refreshes the services", async () => {
    open("SayHello");
    const id = app.request.id;
    await app.grpcRun();
    expect(app.responses[id]?.grpc?.status?.code).toBe(0);
    await until(() => grpc.callKind(app.request) === "unary");
    expect(ws.session(id)).toBeNull();
  });
});

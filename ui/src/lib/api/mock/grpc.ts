/**
 * Mock gRPC server (browser mode, Playwright; API_CONTRACT §3.16 "Mock"). Every grpc:// / grpcs://
 * URL serves `demo.Greeter` (SayHello unary, LotsOfReplies server stream, LotsOfGreetings client
 * stream, Chat bidi) and `demo.Failing` (Fail → NOT_FOUND with a google.rpc.BadRequest detail).
 * Streams go through the mock channel with core's frame sequence: open, (out messages, end),
 * headers, in messages, trailers, status, close (last).
 */
import type { EventChannel, GrpcCallKind, GrpcCallSummary, GrpcDescriptor, GrpcFrame, GrpcStatus, GrpcStatusDetail, SendError } from "../types";
import { CALL_KIND_LONG, findMethod, statusName } from "../../util/grpc";
import { MAX_FRAME_DATA } from "../../util/ws";

const STRING = (name: string, number: number) => ({ name, jsonName: name, number, kind: "string" as const, repeated: false });

/** The fake services (what reflection or compiling the .proto files gives). */
export function demoDescriptor(url: string, fetchedAt: number, source: "reflection" | "files", files: string[] = []): GrpcDescriptor {
  return {
    url,
    fetchedAt,
    source,
    files,
    services: [
      { name: "demo.Failing", methods: [{ name: "Fail", callKind: "unary", inputType: "demo.HelloRequest", outputType: "demo.HelloReply" }] },
      {
        name: "demo.Greeter",
        methods: [
          { name: "Chat", callKind: "bidiStreaming", inputType: "demo.ChatMessage", outputType: "demo.ChatMessage" },
          { name: "LotsOfGreetings", callKind: "clientStreaming", inputType: "demo.HelloRequest", outputType: "demo.HelloReply" },
          { name: "LotsOfReplies", callKind: "serverStreaming", inputType: "demo.HelloRequest", outputType: "demo.HelloReply" },
          { name: "SayHello", callKind: "unary", inputType: "demo.HelloRequest", outputType: "demo.HelloReply" },
        ],
      },
    ],
    messages: {
      "demo.HelloRequest": {
        fields: [
          STRING("name", 1),
          { name: "times", jsonName: "times", number: 2, kind: "int32", repeated: false },
          { name: "mood", jsonName: "mood", number: 3, kind: "enum", typeName: "demo.Mood", repeated: false },
          { name: "tags", jsonName: "tags", number: 4, kind: "string", repeated: true },
          { name: "home_address", jsonName: "homeAddress", number: 5, kind: "message", typeName: "demo.Address", repeated: false },
          { name: "labels", jsonName: "labels", number: 6, kind: "string", repeated: false, mapKey: "string" },
          { name: "sent_at", jsonName: "sentAt", number: 7, kind: "message", typeName: "google.protobuf.Timestamp", repeated: false },
        ],
      },
      "demo.HelloReply": {
        fields: [STRING("message", 1), { name: "count", jsonName: "count", number: 2, kind: "int64", repeated: false }],
      },
      "demo.Address": { fields: [STRING("street", 1), STRING("city", 2)] },
      "demo.ChatMessage": { fields: [STRING("user", 1), STRING("text", 2)] },
    },
    enums: { "demo.Mood": { values: ["MOOD_UNSPECIFIED", "HAPPY", "SAD"] } },
    sizeBytes: 1342,
  };
}

/** Every field of the type, defaults included (core prints responses that way). */
function reply(message: string, count: number): string {
  return JSON.stringify({ message, count: String(count) }, null, 2);
}
function chat(user: string, text: string): string {
  return JSON.stringify({ user, text }, null, 2);
}

const BAD_REQUEST: GrpcStatusDetail = {
  typeUrl: "type.googleapis.com/google.rpc.BadRequest",
  json: JSON.stringify({ fieldViolations: [{ field: "name", description: "No greeter with this name" }] }, null, 2),
  base64: "CiIKBG5hbWUSGk5vIGdyZWV0ZXIgd2l0aCB0aGlzIG5hbWU=",
};

export function okStatus(): GrpcStatus {
  return { code: 0, name: "OK", message: "", details: [] };
}

function status(code: number, message: string, details: GrpcStatusDetail[] = []): GrpcStatus {
  return { code, name: statusName(code), message, details };
}

const HEADERS: [string, string][] = [
  ["content-type", "application/grpc"],
  ["x-served-by", "reqharbor-mock"],
];
const TRAILERS: [string, string][] = [["x-request-cost", "1"]];

/** Unary answer of a method (streams reduced to one message each way). */
export function unaryAnswer(service: string, method: string, input: Record<string, unknown>): { status: GrpcStatus; response: string | null } {
  const name = typeof input.name === "string" && input.name ? input.name : "world";
  if (service === "demo.Failing") return { status: status(5, `No greeter named “${name}”`, [BAD_REQUEST]), response: null };
  switch (method) {
    case "SayHello":
    case "LotsOfReplies":
      return { status: okStatus(), response: reply(`Hello, ${name}!`, 1) };
    case "LotsOfGreetings":
      return { status: okStatus(), response: reply(`Hello, ${name}!`, 1) };
    case "Chat":
      return { status: okStatus(), response: chat(String(input.user ?? ""), String(input.text ?? "")) };
    default:
      return { status: status(12, `Method ${service}/${method} isn't implemented`), response: null };
  }
}

/** A connect failure for mock hosts (`*.invalid` DNS, `refused.*` / port 1 connect). */
export function connectError(host: string, port: number): SendError | null {
  if (host.endsWith(".invalid")) return { kind: "dns", message: `Couldn't resolve ${host}: no such host` };
  if (host.startsWith("refused.") || port === 1) return { kind: "connect", message: `Couldn't connect to ${host}:${port}: Connection refused (os error 111)` };
  return null;
}

export const RESERVED_METADATA = ["content-type", "te", "host", "connection", "user-agent"];

/** Mirrors core's metadata check (AC-GRPC-2); the inline message, or null. */
export function metadataProblem(name: string, value: string): string | null {
  const k = name.trim().toLowerCase();
  if (!/^[a-z0-9_.-]+$/.test(k)) return `“${name}” isn't a valid metadata name`;
  if (RESERVED_METADATA.includes(k) || k.startsWith("grpc-") || k.startsWith(":")) return `“${k}” is set by ReqHarbor: remove it from Metadata`;
  if (k.endsWith("-bin")) {
    try {
      atob(value.trim());
    } catch {
      return `The value of “${k}” isn't base64`;
    }
  }
  return null;
}

/** Mirrors core's message check: JSON, an object, only fields of the input type (top level). */
export function messageProblem(d: GrpcDescriptor, inputType: string, text: string): { input: Record<string, unknown> } | { problem: string } {
  const t = text.trim() || "{}";
  let v: unknown;
  try {
    v = JSON.parse(t);
  } catch (e) {
    return { problem: `Invalid JSON: ${(e as Error).message}` };
  }
  if (!v || typeof v !== "object" || Array.isArray(v)) return { problem: `The message doesn't match ${inputType}: expected an object` };
  const fields = d.messages[inputType]?.fields ?? [];
  for (const k of Object.keys(v)) {
    if (!fields.some((f) => f.jsonName === k || f.name === k)) return { problem: `The message doesn't match ${inputType}: unknown field “${k}”` };
  }
  return { input: v as Record<string, unknown> };
}

// ───────────── streams ─────────────

export interface MockGrpcHooks {
  latency(): number;
  finished(connectionId: string, summary: GrpcCallSummary, error: SendError | null): void;
}

interface Call {
  id: string;
  service: string;
  method: string;
  kind: GrpcCallKind;
  channel: EventChannel<GrpcFrame>;
  state: "connecting" | "open" | "closed";
  halfClosed: boolean;
  startedAt: number;
  sent: number;
  received: number;
  greetings: string[];
  headersSent: boolean;
  timers: Set<ReturnType<typeof setTimeout>>;
}

const utf8 = new TextEncoder();

export class MockGrpc {
  calls = new Map<string, Call>();

  constructor(private hooks: MockGrpcHooks) {}

  private frame(dir: GrpcFrame["dir"], kind: GrpcFrame["kind"], data: string, extra: Partial<GrpcFrame> = {}): GrpcFrame {
    const cut = data.length > MAX_FRAME_DATA;
    return { dir, ts: Date.now(), kind, data: cut ? data.slice(0, MAX_FRAME_DATA) : data, size: 0, ...extra, ...(cut ? { truncated: true } : {}) };
  }

  private emit(c: Call, f: GrpcFrame) {
    if (c.state === "closed") return;
    c.channel.onmessage(structuredClone(f));
  }

  private later(c: Call, ms: number, fn: () => void) {
    const t = setTimeout(() => {
      c.timers.delete(t);
      if (c.state !== "closed") fn();
    }, ms * this.hooks.latency());
    c.timers.add(t);
  }

  private message(c: Call, dir: "in" | "out", json: string) {
    if (dir === "in") c.received++;
    else c.sent++;
    this.emit(c, this.frame(dir, "message", json, { size: utf8.encode(JSON.stringify(JSON.parse(json))).length }));
  }

  private headers(c: Call) {
    if (c.headersSent) return;
    c.headersSent = true;
    this.emit(c, this.frame("in", "headers", HEADERS.map(([k, v]) => `${k}: ${v}`).join("\n")));
  }

  private finish(c: Call, st: GrpcStatus | null, error: SendError | null, closing: string) {
    if (c.state === "closed") return;
    if (st) {
      if (st.code === 0 || c.headersSent) this.emit(c, this.frame("in", "trailers", TRAILERS.map(([k, v]) => `${k}: ${v}`).join("\n")));
      this.emit(c, this.frame("system", "status", st.message ? `${st.name}: ${st.message}` : st.name, { code: st.code, details: st.details }));
    }
    this.emit(c, this.frame("system", "close", closing, { last: true, ...(st ? { code: st.code } : {}) }));
    c.state = "closed";
    for (const t of c.timers) clearTimeout(t);
    this.calls.delete(c.id);
    const summary: GrpcCallSummary = { service: c.service, method: c.method, callKind: c.kind, durationMs: Date.now() - c.startedAt, sent: c.sent, received: c.received };
    if (st) summary.status = st;
    this.hooks.finished(c.id, summary, error);
  }

  private done(c: Call, st: GrpcStatus) {
    this.finish(c, st, null, st.code === 0 ? "Finished · 0 OK" : `Failed · ${st.code} ${st.name}`);
  }

  /**
   * Start a call: `first` = the request's message (sent at once for unary and server streaming).
   * `origin` is the display origin for the open frame.
   */
  start(id: string, a: { service: string; method: string; kind: GrpcCallKind; origin: string; host: string; port: number; first: string; input: Record<string, unknown> }, channel: EventChannel<GrpcFrame>) {
    const old = this.calls.get(id);
    if (old) this.cancel(id);
    const c: Call = {
      id,
      service: a.service,
      method: a.method,
      kind: a.kind,
      channel,
      state: "connecting",
      halfClosed: false,
      startedAt: Date.now(),
      sent: 0,
      received: 0,
      greetings: [],
      headersSent: false,
      timers: new Set(),
    };
    this.calls.set(id, c);
    this.later(c, 100, () => {
      const err = connectError(a.host, a.port);
      if (err) {
        this.emit(c, this.frame("system", "error", err.message));
        this.finish(c, null, err, "Connection failed");
        return;
      }
      c.state = "open";
      this.emit(c, this.frame("system", "open", `Connected to ${a.origin} · ${a.service}/${a.method} (${CALL_KIND_LONG[a.kind]})`));
      if (a.kind === "unary" || a.kind === "serverStreaming") {
        this.message(c, "out", a.first);
        c.halfClosed = true;
        this.emit(c, this.frame("out", "end", "End of stream"));
        this.later(c, 40, () => {
          const ans = unaryAnswer(a.service, a.method, a.input);
          if (ans.status.code !== 0) return this.done(c, ans.status);
          this.headers(c);
          const name = typeof a.input.name === "string" && a.input.name ? a.input.name : "world";
          const n = a.kind === "serverStreaming" ? 3 : 1;
          for (let i = 1; i <= n; i++) this.later(c, 40 * i, () => this.message(c, "in", a.kind === "serverStreaming" ? reply(`Hello, ${name}! (${i} of ${n})`, i) : (ans.response ?? "{}")));
          this.later(c, 40 * (n + 1), () => this.done(c, okStatus()));
        });
      } else if (a.kind === "bidiStreaming") {
        this.later(c, 20, () => this.headers(c));
      }
    });
  }

  send(id: string, json: string) {
    const c = this.calls.get(id);
    if (!c || c.state !== "open" || c.halfClosed) throw new Error("The stream isn't open");
    this.message(c, "out", json);
    const input = JSON.parse(json) as Record<string, unknown>;
    if (c.kind === "bidiStreaming") {
      this.later(c, 40, () => {
        this.headers(c);
        this.message(c, "in", chat("echo", String(input.text ?? "")));
      });
    } else if (c.kind === "clientStreaming") {
      c.greetings.push(typeof input.name === "string" && input.name ? input.name : "world");
    }
  }

  end(id: string): boolean {
    const c = this.calls.get(id);
    if (!c || c.state !== "open" || c.halfClosed) return false;
    c.halfClosed = true;
    this.emit(c, this.frame("out", "end", "End of stream"));
    this.later(c, 40, () => {
      if (c.kind === "clientStreaming") {
        this.headers(c);
        this.message(c, "in", reply(`Hello, ${c.greetings.join(", ") || "nobody"}!`, c.greetings.length));
      }
      this.later(c, 20, () => this.done(c, okStatus()));
    });
    return true;
  }

  cancel(id: string): boolean {
    const c = this.calls.get(id);
    if (!c) return false;
    this.finish(c, status(1, "Cancelled by the client"), c.state === "open" ? null : { kind: "cancelled", message: "Cancelled" }, "Cancelled");
    return true;
  }
}

/** True when the method exists in the demo services. */
export function hasMethod(d: GrpcDescriptor, service: string, method: string): boolean {
  return !!findMethod(d, service, method);
}

const sq = (s: string) => `'${s.replace(/'/g, "'\\''")}'`;

/** AC-GRPC-11 (mirrors core `export_grpcurl`): one part per line. */
export function grpcurl(a: {
  tls: boolean;
  verifyTls: boolean;
  metadata: [string, string][];
  protoFiles: string[];
  importPaths: string[];
  files: boolean;
  message: string;
  timeoutMs: number;
  host: string;
  port: number;
  service: string;
  method: string;
}): string {
  const parts = ["grpcurl"];
  if (!a.tls) parts.push("-plaintext");
  else if (!a.verifyTls) parts.push("-insecure");
  for (const [k, v] of a.metadata) parts.push(`-H ${sq(`${k}: ${v}`)}`);
  if (a.files) {
    for (const p of a.importPaths) parts.push(`-import-path ${sq(p)}`);
    for (const p of a.protoFiles) parts.push(`-proto ${sq(p)}`);
  }
  parts.push(`-d ${sq(a.message.trim() || "{}")}`);
  if (a.timeoutMs > 0) parts.push(`-max-time ${Number((a.timeoutMs / 1000).toFixed(3))}`);
  parts.push(sq(`${a.host}:${a.port}`), sq(`${a.service}/${a.method}`));
  return parts.join(" \\\n  ");
}

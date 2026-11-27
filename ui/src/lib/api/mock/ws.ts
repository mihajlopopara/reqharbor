/**
 * Mock WebSocket / Socket.IO server (browser mode, Playwright). Mirrors core `ws::run` closely
 * enough for the UI: the same frames, in the same order, through the same channel shape.
 *
 * - Plain WebSocket: echoes text and binary; the text `close:<code>` makes the server close with
 *   that code; pings are answered with a pong.
 * - Socket.IO: joins any namespace; every event is echoed back as the same event, and an event that
 *   asks for an ack is acknowledged with its arguments.
 * - Connect fails for host `refused.test` / port 1 (connect), `/forbidden` paths (403) and hosts
 *   ending in `.invalid` (DNS).
 */
import type { EventChannel, SendError, WsFrame, WsSessionSummary } from "../types";
import { MAX_FRAME_DATA } from "../../util/ws";

/** A resolved, validated message (mirrors core `OutMessage`). */
export type OutMessageLike =
  | { type: "text"; text: string }
  | { type: "binary"; base64: string }
  | { type: "event"; name: string; args: unknown[]; ack: boolean };

export class MockChannel<T> implements EventChannel<T> {
  constructor(public onmessage: (message: T) => void) {}
}

export function isMockChannel(v: unknown): v is MockChannel<unknown> {
  return v instanceof MockChannel;
}

interface Conn {
  id: string;
  sio: boolean;
  namespace: string;
  channel: EventChannel<WsFrame>;
  state: "connecting" | "open" | "closed";
  startedAt: number;
  sent: number;
  received: number;
  nextAck: number;
  timers: Set<ReturnType<typeof setTimeout>>;
  done: () => void;
  displayUrl: string;
}

export interface MockWsHooks {
  /** Latency multiplier applied to every delay (the mock's `latency` option). */
  latency(): number;
  /** Called once per session when it ends: write the history entry. */
  finished(connectionId: string, summary: WsSessionSummary, error: SendError | null): void;
}

const utf8 = new TextEncoder();
const size = (s: string) => utf8.encode(s).length;
const b64Size = (b: string) => {
  try {
    return atob(b).length;
  } catch {
    return 0;
  }
};

export class MockWs {
  conns = new Map<string, Conn>();

  constructor(private hooks: MockWsHooks) {}

  private frame(dir: WsFrame["dir"], kind: WsFrame["kind"], data: string, bytes: number, extra: Partial<WsFrame> = {}): WsFrame {
    // Like core WsFrame::new: data past MAX_FRAME_DATA is cut and flagged.
    const cut = data.length > MAX_FRAME_DATA;
    return { dir, ts: Date.now(), kind, data: cut ? data.slice(0, MAX_FRAME_DATA) : data, size: bytes, ...extra, ...(cut ? { truncated: true } : {}) };
  }

  private emit(c: Conn, f: WsFrame) {
    if (c.state === "closed") return;
    c.channel.onmessage(structuredClone(f));
  }

  private later(c: Conn, ms: number, fn: () => void) {
    const t = setTimeout(() => {
      c.timers.delete(t);
      if (c.state !== "closed") fn();
    }, ms * this.hooks.latency());
    c.timers.add(t);
  }

  /** Start a session; the URL is the resolved ws(s):// URL. */
  connect(id: string, url: string, displayUrl: string, sio: { namespace: string; auth: unknown } | null, channel: EventChannel<WsFrame>): void {
    this.conns.get(id) && this.close(id, null, "");
    const c: Conn = {
      id,
      sio: !!sio,
      namespace: sio?.namespace ?? "/",
      channel,
      state: "connecting",
      startedAt: Date.now(),
      sent: 0,
      received: 0,
      nextAck: 0,
      timers: new Set(),
      done: () => {},
      displayUrl,
    };
    this.conns.set(id, c);
    let u: URL | null = null;
    try {
      u = new URL(url.replace(/^ws/, "http"));
    } catch {
      /* reported below */
    }
    this.later(c, 120, () => {
      let error: SendError | null = null;
      if (!u) error = { kind: "other", message: `Invalid URL "${url}"` };
      else if (u.hostname.endsWith(".invalid")) error = { kind: "dns", message: `Couldn't resolve ${u.hostname}: no such host` };
      else if (u.hostname === "refused.test" || u.port === "1") error = { kind: "connect", message: `Couldn't connect to ${u.host}: Connection refused (os error 111)` };
      else if (u.pathname.includes("/forbidden")) error = { kind: "other", message: "The server answered 403 Forbidden instead of switching to WebSocket" };
      if (error) {
        this.emit(c, this.frame("system", "error", error.message, 0));
        this.end(c, null, "Connection failed", error);
        return;
      }
      c.state = "open";
      const summary = sio ? `Connected to ${displayUrl} · Socket.IO namespace ${c.namespace} · sid mock-${id.slice(-6)}` : `Connected to ${displayUrl} (101 Switching Protocols)`;
      this.emit(c, this.frame("system", "open", summary, 0));
      if (!sio) {
        this.later(c, 60, () => {
          const hello = JSON.stringify({ type: "welcome", server: "ReqHarbor mock echo" });
          c.received++;
          this.emit(c, this.frame("in", "text", hello, size(hello)));
        });
      }
    });
  }

  send(id: string, msg: OutMessageLike): { ackId: number | null } {
    const c = this.conns.get(id);
    if (!c || c.state !== "open") throw new Error("Not connected");
    if (msg.type === "event") {
      if (!c.sio) throw new Error("Socket.IO events need Socket.IO mode");
      const ackId = msg.ack ? c.nextAck++ : undefined;
      const args = JSON.stringify(msg.args);
      c.sent++;
      this.emit(c, this.frame("out", "event", args, size(args) + msg.name.length + 4, { event: msg.name, ...(ackId !== undefined ? { ackId } : {}) }));
      this.later(c, 40, () => {
        if (ackId !== undefined) {
          c.received++;
          this.emit(c, this.frame("in", "ack", args, size(args) + 4, { ackId }));
        }
        c.received++;
        this.emit(c, this.frame("in", "event", args, size(args) + msg.name.length + 4, { event: msg.name }));
      });
      return { ackId: ackId ?? null };
    }
    if (c.sio) throw new Error("Socket.IO mode sends events: enter an event name");
    if (msg.type === "text") {
      c.sent++;
      this.emit(c, this.frame("out", "text", msg.text, size(msg.text)));
      const close = /^close:(\d{4})$/.exec(msg.text);
      this.later(c, 40, () => {
        if (close) {
          const code = Number(close[1]);
          this.emit(c, this.frame("in", "close", "server says bye", 15, { code }));
          this.end(c, code, "server says bye", null);
          return;
        }
        c.received++;
        this.emit(c, this.frame("in", "text", msg.text, size(msg.text)));
      });
    } else {
      c.sent++;
      this.emit(c, this.frame("out", "binary", msg.base64, b64Size(msg.base64)));
      this.later(c, 40, () => {
        c.received++;
        this.emit(c, this.frame("in", "binary", msg.base64, b64Size(msg.base64)));
      });
    }
    return { ackId: null };
  }

  ping(id: string): boolean {
    const c = this.conns.get(id);
    if (!c || c.state !== "open") return false;
    this.emit(c, this.frame("out", "ping", "", 0));
    this.later(c, 20, () => this.emit(c, this.frame("in", "pong", "", 0)));
    return true;
  }

  close(id: string, code: number | null, reason: string): boolean {
    const c = this.conns.get(id);
    if (!c || c.state === "closed") return false;
    if (c.state === "connecting") {
      this.end(c, null, "Connection cancelled", null);
      return true;
    }
    const cc = code ?? 1000;
    this.emit(c, this.frame("out", "close", reason, size(reason), { code: cc }));
    this.later(c, 30, () => {
      this.emit(c, this.frame("in", "close", reason, size(reason), { code: cc }));
      this.end(c, cc, reason, null);
    });
    return true;
  }

  /** App exit / test reset. */
  closeAll(): void {
    for (const c of [...this.conns.values()]) this.end(c, 1001, "ReqHarbor closed", null);
  }

  private end(c: Conn, code: number | null, reason: string, error: SendError | null) {
    if (c.state === "closed") return;
    for (const t of c.timers) clearTimeout(t);
    c.timers.clear();
    const text = code !== null ? `Disconnected · ${code}${reason ? ` ${reason}` : ""}` : reason || "Disconnected";
    this.emit(c, this.frame("system", "close", text, 0, { ...(code !== null ? { code } : {}), last: true }));
    c.state = "closed";
    this.conns.delete(c.id);
    const summary: WsSessionSummary = { durationMs: Date.now() - c.startedAt, sent: c.sent, received: c.received };
    if (code !== null) summary.closeCode = code;
    if (reason) summary.closeReason = reason;
    this.hooks.finished(c.id, summary, error);
  }
}

const RESERVED_EVENTS = ["connect", "connect_error", "disconnect", "disconnecting", "newListener", "removeListener"];

/** Mirrors core `resolve_message` for an already variable-resolved body / event. Returns an error text on failure. */
export function toOutMessage(kind: string, body: string, event: string, ack: boolean, sio: boolean): OutMessageLike | string {
  if (sio) {
    const name = event.trim();
    if (!name) return "Enter an event name";
    if (RESERVED_EVENTS.includes(name)) return `“${name}” is a reserved Socket.IO event name`;
    if (kind === "binary") return "Binary arguments aren't supported in Socket.IO mode";
    if (kind === "text") return { type: "event", name, args: [body], ack };
    if (kind !== "json") return `Message kind “${kind}” needs a newer version of ReqHarbor`;
    if (!body.trim()) return { type: "event", name, args: [], ack };
    try {
      return { type: "event", name, args: [JSON.parse(body) as unknown], ack };
    } catch (e) {
      return `Invalid JSON: ${(e as Error).message}`;
    }
  }
  if (kind === "text") return { type: "text", text: body };
  if (kind === "json") {
    try {
      JSON.parse(body);
    } catch (e) {
      return `Invalid JSON: ${(e as Error).message}`;
    }
    return { type: "text", text: body };
  }
  if (kind === "binary") {
    const compact = body.replace(/\s+/g, "");
    try {
      atob(compact);
      return { type: "binary", base64: compact };
    } catch {
      return "Invalid base64";
    }
  }
  return `Message kind “${kind}” needs a newer version of ReqHarbor`;
}

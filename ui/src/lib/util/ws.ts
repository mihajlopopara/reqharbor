/**
 * WebSocket / Socket.IO helpers shared by the UI and the mock (AC-WS-*): kind switching, message
 * defaults and validation, and the message log (state machine, filter, previews).
 */
import type { GrpcFrame, GrpcStatusDetail, Request, SavedMessage, SocketIoConfig, WsConfig, WsFrame, WsSessionSummary } from "../api/types";
import { GRPC_MIN_CLIENT_VERSION, defaultGrpc, stampAtLeast } from "./grpc";
import { prettyJson } from "./json";
import { formatDuration } from "./format";

export const DEFAULT_SIO_PATH = "/socket.io/";
export const SOCKET_IO_VERSION = 4;
/** The log keeps at most this many frames per connection (oldest dropped). */
export const LOG_MAX = 10_000;
/** …and at most about this many bytes of payload text (oldest dropped), so big frames can't pile up. */
export const LOG_MAX_BYTES = 32 * 1024 * 1024;
/** Rust cuts frame data at this many bytes (`truncated: true`); mirrors core `MAX_FRAME_DATA`. */
export const MAX_FRAME_DATA = 256 * 1024;
/** The filter searches this much of each payload (a lowercased copy cached per frame). */
export const SEARCH_PREVIEW = 8 * 1024;
/** The filter field's tooltip: what it searches, and what it doesn't. */
export const FILTER_HINT = `Matches the event name, the kind, or text in the first ${SEARCH_PREVIEW / 1024} KB of each message; text further into a longer message isn't searched`;

/**
 * A message-log frame: a WebSocket / Socket.IO frame or a gRPC stream frame (AC-GRPC-8), which
 * share one shape so the same log renders both.
 */
export type StreamFrame = Omit<WsFrame, "kind"> & { kind: WsFrame["kind"] | GrpcFrame["kind"]; details?: GrpcStatusDetail[] };

/** A frame as the log keeps it: with a stable key and its cached search text. */
export interface LogEntry extends StreamFrame {
  /** Stable key within a session's log. */
  seq: number;
  /** Lowercased event name + the start of the payload: what the text filter matches. */
  search: string;
  /** Approximate memory cost (payload + search text), for the byte budget. */
  cost: number;
}

/** Wrap a frame for the log: computes the cached search text once. */
export function logEntry(f: StreamFrame, seq: number): LogEntry {
  const search = `${f.event ? `${f.event.toLowerCase()} ` : ""}${f.data.slice(0, SEARCH_PREVIEW).toLowerCase()}`;
  return { ...f, seq, search, cost: (f.data.length + search.length) * 2 + 64 };
}

/**
 * Keep the log within LOG_MAX frames and `maxBytes` of payload. Over a limit, the oldest entries are
 * dropped down to 90 % of both (so this happens now and then, not on every flush) and a new array is
 * returned; otherwise the same array. Appends happen in place, so rows already on screen never change.
 */
export function trimLog(log: LogEntry[], bytes: number, maxBytes = LOG_MAX_BYTES, maxCount = LOG_MAX): { log: LogEntry[]; bytes: number } {
  if (log.length <= maxCount && bytes <= maxBytes) return { log, bytes };
  const count = Math.floor(maxCount * 0.9);
  const budget = maxBytes * 0.9;
  let drop = 0;
  while (drop < log.length && (log.length - drop > count || bytes > budget)) {
    bytes -= log[drop].cost;
    drop++;
  }
  return { log: log.slice(drop), bytes };
}

export function defaultSocketIo(): SocketIoConfig {
  return { path: DEFAULT_SIO_PATH, namespace: "/", version: SOCKET_IO_VERSION };
}

export function defaultWs(): WsConfig {
  return { messages: [], protocols: [], autoReconnect: false };
}

export function isWs(r: Pick<Request, "kind">): boolean {
  return r.kind === "ws";
}

export function isSocketIo(r: Pick<Request, "kind" | "ws">): boolean {
  return r.kind === "ws" && !!r.ws?.socketIo;
}

/** Badge for the method column: "WS" / "SIO" for WebSocket requests, "gRPC" for gRPC ones, else the HTTP method. */
export function requestBadge(r: Pick<Request, "kind" | "ws" | "method">): { label: string; cls: string | null } {
  if (r.kind === "ws") return { label: r.ws?.socketIo ? "SIO" : "WS", cls: "m-ws" };
  if (r.kind === "grpc") return { label: "gRPC", cls: "m-grpc" };
  return { label: r.method, cls: null };
}

/**
 * The first ReqHarbor that edits and runs WebSocket / Socket.IO requests: the `minClientVersion` every
 * `ws` request carries (AC-WS-1). Mirrors core `WS_MIN_CLIENT_VERSION`. Fixed, never the running
 * version, so a newer patch build doesn't lock out older WebSocket-capable builds.
 */
export const WS_MIN_CLIENT_VERSION = "0.1.0";

/** Keeps the newer of the current stamp and WS_MIN_CLIENT_VERSION (never lowers a `minClientVersion`). */
function stamp(current: string | undefined): string {
  return stampAtLeast(current, WS_MIN_CLIENT_VERSION);
}

/**
 * Mirrors core `Request::stamp_min_client_version`: WebSocket requests carry at least
 * WS_MIN_CLIENT_VERSION, gRPC requests at least GRPC_MIN_CLIENT_VERSION.
 */
export function stampMinClientVersion(r: Request): void {
  if (r.kind === "ws") r.minClientVersion = stamp(r.minClientVersion);
  else if (r.kind === "grpc") r.minClientVersion = stampAtLeast(r.minClientVersion, GRPC_MIN_CLIENT_VERSION);
}

export type KindTarget = "http" | "ws" | "sio" | "grpc";

/**
 * Switch a request (in place) to WebSocket ("ws"), Socket.IO ("sio"), gRPC ("grpc") or back to HTTP
 * with `method`. `ws` and `grpc` are kept when switching away; WebSocket and gRPC stamp
 * `minClientVersion` (AC-WS-1, AC-GRPC-1).
 */
export function switchKind(r: Request, target: KindTarget, method = "GET"): void {
  if (target === "http") {
    delete r.kind;
    r.method = method;
    return;
  }
  if (target === "grpc") {
    r.kind = "grpc";
    r.grpc ??= defaultGrpc();
    r.minClientVersion = stampAtLeast(r.minClientVersion, GRPC_MIN_CLIENT_VERSION);
    return;
  }
  r.kind = "ws";
  r.ws ??= defaultWs();
  if (target === "sio") r.ws.socketIo ??= defaultSocketIo();
  else delete r.ws.socketIo;
  r.minClientVersion = stamp(r.minClientVersion);
}

export function defaultMessage(sio: boolean): SavedMessage {
  return sio ? { name: "", kind: "json", body: "", event: "message", ack: false } : { name: "", kind: "text", body: "" };
}

/** Client-side check mirroring core `resolve_message` (variables aside). Null when sendable. */
export function messageError(m: SavedMessage, sio: boolean): string | null {
  const hasVars = /\{\{/.test(m.body);
  if (sio) {
    if (!(m.event ?? "").trim()) return "Enter an event name";
    if (m.kind === "binary") return "Binary arguments aren't supported in Socket.IO mode";
  }
  if (m.kind === "json" && m.body.trim() && !hasVars) {
    try {
      JSON.parse(m.body);
    } catch (e) {
      return `Invalid JSON: ${(e as Error).message}`;
    }
  } else if (m.kind === "json" && !sio && !m.body.trim()) return "Enter a JSON message";
  if (m.kind === "binary" && !hasVars && !/^[A-Za-z0-9+/\s]*={0,2}\s*$/.test(m.body)) return "Invalid base64";
  return null;
}

/** A default name for a saved message: the event, or the first words of the body. */
export function messageTitle(m: SavedMessage): string {
  if (m.name.trim()) return m.name.trim();
  if (m.event) return m.event;
  const t = m.body.replace(/\s+/g, " ").trim();
  return t ? (t.length > 32 ? t.slice(0, 31) + "…" : t) : "Untitled message";
}

// ───────────── the message log ─────────────

export type WsConnState = "idle" | "connecting" | "open" | "closing" | "closed";

/** Connection state after `f` arrives. */
export function nextState(state: WsConnState, f: StreamFrame): WsConnState {
  if (f.dir !== "system") {
    if (f.dir === "out" && f.kind === "close") return "closing";
    return state;
  }
  if (f.last) return "closed";
  if (f.kind === "open") return "open";
  if (f.kind === "reconnecting") return "connecting";
  return state;
}

export type DirFilter = "all" | "in" | "out" | "system";

export interface LogFilter {
  text: string;
  dir: DirFilter;
  /** Hide ping / pong frames. */
  hideHeartbeats: boolean;
}

export const EMPTY_FILTER: LogFilter = { text: "", dir: "all", hideHeartbeats: false };

/**
 * Frames matching the filter. Text matches case-insensitively against the event name and the first
 * SEARCH_PREVIEW bytes of the payload (the cached `search` of a log entry), or the kind.
 */
export function filterLog<T extends StreamFrame & { search?: string }>(frames: readonly T[], f: LogFilter): T[] {
  const q = f.text.trim().toLowerCase();
  if (!q && f.dir === "all" && !f.hideHeartbeats) return frames as T[];
  return frames.filter((fr) => {
    if (f.dir !== "all" && fr.dir !== f.dir) return false;
    if (f.hideHeartbeats && (fr.kind === "ping" || fr.kind === "pong")) return false;
    if (!q) return true;
    const search = fr.search ?? `${(fr.event ?? "").toLowerCase()} ${fr.data.slice(0, SEARCH_PREVIEW).toLowerCase()}`;
    return search.includes(q) || fr.kind === q;
  });
}

/** Base64 → text, when it is valid UTF-8 (else null). */
export function base64Text(b64: string): string | null {
  try {
    const bin = atob(b64);
    const bytes = Uint8Array.from(bin, (c) => c.charCodeAt(0));
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    return null;
  }
}

/** Base64 → "00 01 ff …" (first `max` bytes). */
export function base64Hex(b64: string, max = 64): string {
  try {
    const bin = atob(b64);
    const out: string[] = [];
    for (let i = 0; i < Math.min(bin.length, max); i++) out.push(bin.charCodeAt(i).toString(16).padStart(2, "0"));
    return out.join(" ") + (bin.length > max ? " …" : "");
  } catch {
    return b64;
  }
}

/** One-line preview of a frame for the log row. */
export function frameSummary(f: StreamFrame): string {
  switch (f.kind) {
    case "binary":
      return `Binary · ${base64Hex(f.data, 16)}`;
    case "ping":
    case "pong": {
      const t = f.data ? (base64Text(f.data) ?? f.data) : "";
      return t ? `${f.kind === "ping" ? "Ping" : "Pong"} · ${t}` : f.kind === "ping" ? "Ping" : "Pong";
    }
    case "close":
      if (f.dir === "system") return f.data;
      return `Close${f.code !== undefined ? ` ${f.code}` : ""}${f.data ? ` · ${f.data}` : ""}`;
    case "event":
      return `${f.event ?? "event"}${f.ackId !== undefined ? ` #${f.ackId}` : ""} ${argsPreview(f.data)}`.trim();
    case "ack":
      return `Ack #${f.ackId ?? "?"} ${argsPreview(f.data)}`.trim();
    case "headers":
    case "trailers":
      return f.data ? f.data.split("\n").join(" · ") : "(none)";
    case "end":
      return f.data || "End of stream";
    case "status":
      return f.code !== undefined ? `${f.code} ${f.data}` : f.data;
    default:
      return f.data.replace(/\s+/g, " ");
  }
}

/** `[1,"a"]` → `1, "a"`; `[]` → "". */
function argsPreview(data: string): string {
  const t = data.trim();
  if (t === "[]") return "";
  return t.startsWith("[") && t.endsWith("]") ? t.slice(1, -1).replace(/\s+/g, " ") : t;
}

/** The expanded view of a frame: pretty JSON when the payload is JSON, decoded text, or hex. */
export function frameDetail(f: StreamFrame): { text: string; json: boolean } {
  if (f.kind === "binary" || f.kind === "ping" || f.kind === "pong") {
    const t = f.data ? base64Text(f.data) : "";
    return { text: t !== null && t !== "" ? t : base64Hex(f.data, 4096), json: false };
  }
  if (f.kind === "status" && f.details?.length) {
    const lines = f.details.map((d) => `${d.typeUrl}\n${d.json ?? `base64 ${d.base64}`}`);
    return { text: `${frameSummary(f)}\n\n${lines.join("\n\n")}`, json: false };
  }
  const pretty = f.dir !== "system" ? prettyJson(f.data) : null;
  if (pretty !== null) return { text: pretty, json: true };
  return { text: f.data, json: false };
}

/** What Copy puts on the clipboard: the raw payload (decoded text for UTF-8 binary). */
export function frameCopyText(f: StreamFrame): string {
  if (f.kind === "event") return `${f.event ?? ""} ${f.data}`.trim();
  return f.data;
}

/** Height of a log row; expanded rows grow with their content (capped, then they scroll). */
export function rowHeight(f: StreamFrame, expanded: boolean): number {
  const ROW = 30;
  if (!expanded) return ROW;
  const lines = frameDetail(f).text.split("\n").length;
  return ROW + 16 + Math.min(lines, 18) * 18 + (f.truncated ? 20 : 0);
}

export function summaryText(s: WsSessionSummary): string {
  return `${formatDuration(s.durationMs)} · ${s.sent} sent · ${s.received} received`;
}

export function timeOfDay(ts: number): string {
  const d = new Date(ts);
  const p = (n: number, w = 2) => String(n).padStart(w, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}.${p(d.getMilliseconds(), 3)}`;
}

// ───────────── URLs (mirror core `ws_url` / `normalize_namespace`) ─────────────

/** `chat` → `/chat`, `` → `/`. */
export function normalizeNamespace(ns: string): string {
  const t = ns.trim();
  if (!t || t === "/") return "/";
  return t.startsWith("/") ? t : `/${t}`;
}

/**
 * The URL a WebSocket request opens: http(s) → ws(s), fragment dropped; in Socket.IO mode the path
 * becomes the Engine.IO path and `EIO=4&transport=websocket` is appended. Throws with a message for
 * URLs that can't be opened.
 */
export function wsUrl(url: string, sio?: Pick<SocketIoConfig, "path"> | null): string {
  const u = url.trim();
  const i = u.indexOf("://");
  if (i < 0) throw new Error(`“${u}” is not a ws://, wss://, http:// or https:// URL`);
  const scheme0 = u.slice(0, i).toLowerCase();
  const scheme = scheme0 === "ws" || scheme0 === "http" ? "ws" : scheme0 === "wss" || scheme0 === "https" ? "wss" : null;
  if (!scheme) throw new Error(`Use ws://, wss://, http:// or https:// (not ${scheme0}://)`);
  const rest = u.slice(i + 3).split("#")[0];
  const m = /[/?]/.exec(rest);
  const end = m ? m.index : rest.length;
  const authority = rest.slice(0, end);
  if (!authority) throw new Error("The URL has no host");
  const tail = rest.slice(end);
  const q = tail.indexOf("?");
  const path = q >= 0 ? tail.slice(0, q) : tail;
  const query = q >= 0 ? tail.slice(q + 1) : null;
  if (!sio) return `${scheme}://${authority}${path || "/"}${query !== null ? `?${query}` : ""}`;
  let p = sio.path.trim() || DEFAULT_SIO_PATH;
  if (!p.startsWith("/")) p = `/${p}`;
  const parts = (query ?? "").split("&").filter((x) => x && !["EIO", "transport"].includes(x.split("=")[0]));
  parts.push("EIO=4", "transport=websocket");
  return `${scheme}://${authority}${p}?${parts.join("&")}`;
}

/**
 * WebSocket / Socket.IO sessions (AC-WS-*), one per open request: connection state, the message
 * log (streamed over ws_connect's channel) and the composer draft. Frames are buffered and flushed
 * at most once per animation frame so a chatty server can't stall the UI.
 */
import { call, channel, toCommandError, type Request, type SavedMessage, type WsFrame } from "../api";
import { ulid } from "../util/ulid";
import { defaultMessage, isSocketIo, logEntry, nextState, trimLog, type LogEntry, type WsConnState } from "../util/ws";

export type { LogEntry };

export interface WsSession {
  requestId: string;
  /** Project whose variables it uses (sessions of a deleted project are discarded). */
  projectId: string | null;
  /** The request was saved in a collection (a deleted saved request discards its session). */
  saved: boolean;
  connectionId: string;
  state: WsConnState;
  method: "WS" | "SIO";
  resolvedUrl: string;
  unresolved: string[];
  startedAt: number;
  /** Set when the session ended (last frame). */
  endedAt: number | null;
  /** First error of a failed connect (shown in the log header). */
  error: string | null;
  sent: number;
  received: number;
  /** Bumped on every flush; the log re-renders from `frames`. */
  rev: number;
}

const DATA_KINDS = new Set(["text", "binary", "event", "ack"]);

class WsStore {
  /** Session per request id (the last one stays after it closes, with its log). */
  sessions = $state<Record<string, WsSession>>({});
  /** Composer draft per request id. */
  drafts = $state<Record<string, SavedMessage>>({});
  /** Log frames per connection id (not deeply reactive: see `rev`). */
  private logs = new Map<string, LogEntry[]>();
  /** Approximate bytes held by each connection's log (LOG_MAX_BYTES budget). */
  private logBytes = new Map<string, number>();
  /** Connections whose Cancel came before ws_connect resolved: closed as soon as it does. */
  private cancelPending = new Set<string>();
  /** Connections ws_connect has accepted (ws_close can reach them). */
  private registered = new Set<string>();
  /** Connections of discarded sessions still winding down: their last frame refreshes history. */
  private discarded = new Set<string>();
  private pending = new Map<string, WsFrame[]>();
  private flushScheduled = false;
  private seq = 0;
  /** Called when a session ends (history reload). */
  onEnded: (() => void) | null = null;

  session(requestId: string): WsSession | null {
    return this.sessions[requestId] ?? null;
  }

  frames(requestId: string): LogEntry[] {
    const s = this.sessions[requestId];
    if (!s) return [];
    void s.rev;
    return this.logs.get(s.connectionId) ?? [];
  }

  isLive(requestId: string): boolean {
    const st = this.sessions[requestId]?.state;
    return st === "connecting" || st === "open" || st === "closing";
  }

  /** The composer draft of `r` (a default one until the first edit). Safe to call while rendering. */
  draftOf(r: Request): SavedMessage {
    return this.drafts[r.id] ?? defaultMessage(isSocketIo(r));
  }

  /** The stored, editable draft of `r` (created on first use; call from handlers or effects). */
  ensureDraft(r: Request): SavedMessage {
    const sio = isSocketIo(r);
    if (!this.drafts[r.id]) this.drafts[r.id] = defaultMessage(sio);
    const d = this.drafts[r.id];
    if (sio && d.event === undefined) {
      d.event = "message";
      if (d.kind === "binary") d.kind = "json";
    }
    return d;
  }

  setDraft(requestId: string, m: SavedMessage) {
    this.drafts[requestId] = structuredClone(m);
  }

  /** Open a connection for `req` (validation problems reject with a CommandError). */
  async connect(req: Request, projectId: string | null): Promise<void> {
    const prev = this.sessions[req.id];
    if (prev && this.isLive(req.id)) {
      // Its last frame still arrives after its log is gone: that clears its bookkeeping.
      this.discarded.add(prev.connectionId);
      await this.disconnect(req.id);
    }
    const connectionId = ulid();
    this.logs.set(connectionId, []);
    const session: WsSession = {
      requestId: req.id,
      projectId,
      saved: !!req.collectionId,
      connectionId,
      state: "connecting",
      method: isSocketIo(req) ? "SIO" : "WS",
      resolvedUrl: req.url,
      unresolved: [],
      startedAt: Date.now(),
      endedAt: null,
      error: null,
      sent: 0,
      received: 0,
      rev: 0,
    };
    if (prev) this.dropLog(prev.connectionId);
    this.sessions[req.id] = session;
    const onEvent = await channel<WsFrame>((f) => this.push(connectionId, f));
    try {
      const res = await call("ws_connect", { connectionId, request: req, projectId, onEvent });
      this.registered.add(connectionId);
      const s = this.byConnection(connectionId);
      if (s) {
        s.resolvedUrl = res.resolvedUrl;
        s.unresolved = res.unresolvedVariables;
        s.method = res.method;
      }
      // Cancel (or a delete) came while ws_connect was in flight: stop it now that Rust knows it.
      if (this.cancelPending.delete(connectionId)) {
        await call("ws_close", { connectionId, code: null, reason: null }).catch(() => false);
      }
    } catch (e) {
      this.cancelPending.delete(connectionId);
      const s = this.byConnection(connectionId);
      if (s) {
        s.state = "closed";
        s.endedAt = Date.now();
        s.error = toCommandError(e).message;
      }
      throw e;
    }
  }

  async send(requestId: string, message: SavedMessage): Promise<{ ackId: number | null }> {
    const s = this.sessions[requestId];
    if (!s || s.state !== "open") throw { code: "ws_not_connected", message: "Not connected" };
    return call("ws_send", { connectionId: s.connectionId, message });
  }

  async ping(requestId: string): Promise<boolean> {
    const s = this.sessions[requestId];
    if (!s) return false;
    return call("ws_ping", { connectionId: s.connectionId });
  }

  async disconnect(requestId: string, code?: number, reason?: string): Promise<void> {
    const s = this.sessions[requestId];
    if (!s || !this.isLive(requestId)) return;
    if (s.state === "open") s.state = "closing";
    if (!this.registered.has(s.connectionId)) {
      // ws_connect hasn't resolved yet: Rust doesn't know the id, so close once it does.
      this.cancelPending.add(s.connectionId);
      return;
    }
    await call("ws_close", { connectionId: s.connectionId, code: code ?? null, reason: reason ?? null }).catch(() => false);
  }

  /**
   * The requests were deleted (or their folder / collection): close their connections and forget
   * their sessions and drafts, so nothing keeps running (or reconnecting) without a request.
   */
  async discard(requestIds: Iterable<string>): Promise<void> {
    const waits: Promise<void>[] = [];
    for (const id of requestIds) {
      const s = this.sessions[id];
      if (s) {
        if (this.isLive(id)) {
          this.discarded.add(s.connectionId);
          waits.push(this.disconnect(id));
        }
        this.dropLog(s.connectionId);
        delete this.sessions[id];
      }
      delete this.drafts[id];
    }
    await Promise.all(waits);
  }

  /** Discard every session `keep` rejects (see discard). */
  prune(keep: (s: WsSession) => boolean): Promise<void> {
    return this.discard(Object.values(this.sessions).filter((s) => !keep(s)).map((s) => s.requestId));
  }

  /** Request ids with a live (connecting, open or closing) connection: the tree's "live" dot. */
  liveIds(): Set<string> {
    const out = new Set<string>();
    for (const [id, s] of Object.entries(this.sessions)) if (s.state === "connecting" || s.state === "open" || s.state === "closing") out.add(id);
    return out;
  }

  private dropLog(connectionId: string) {
    this.logs.delete(connectionId);
    this.logBytes.delete(connectionId);
    this.pending.delete(connectionId);
  }

  /** Empty the visible log (the connection stays as it is). */
  clear(requestId: string) {
    const s = this.sessions[requestId];
    if (!s) return;
    this.logs.set(s.connectionId, []);
    this.logBytes.set(s.connectionId, 0);
    this.pending.delete(s.connectionId);
    s.rev++;
  }

  /** Forget a closed session (e.g. its request was deleted). */
  forget(requestId: string) {
    const s = this.sessions[requestId];
    if (!s || this.isLive(requestId)) return;
    this.dropLog(s.connectionId);
    delete this.sessions[requestId];
  }

  /** A saved request got a new id (scratch saved to a collection): move its session along. */
  rekey(fromId: string, toId: string) {
    const s = this.sessions[fromId];
    if (s) {
      s.requestId = toId;
      s.saved = true;
      this.sessions[toId] = s;
      delete this.sessions[fromId];
    }
    const d = this.drafts[fromId];
    if (d) {
      this.drafts[toId] = d;
      delete this.drafts[fromId];
    }
  }

  /** Bookkeeping sizes (tests: nothing leaks across reconnects). */
  debugCounts(): { registered: number; discarded: number; cancelPending: number; logs: number } {
    return { registered: this.registered.size, discarded: this.discarded.size, cancelPending: this.cancelPending.size, logs: this.logs.size };
  }

  private byConnection(connectionId: string): WsSession | null {
    for (const s of Object.values(this.sessions)) if (s.connectionId === connectionId) return s;
    return null;
  }

  /** Channel callback: frames arrive in order; they are applied in batches. */
  push(connectionId: string, f: WsFrame) {
    const q = this.pending.get(connectionId);
    if (q) q.push(f);
    else this.pending.set(connectionId, [f]);
    if (this.flushScheduled) return;
    this.flushScheduled = true;
    const run = () => this.flush();
    if (typeof requestAnimationFrame === "function" && typeof document !== "undefined" && !document.hidden) requestAnimationFrame(run);
    else setTimeout(run, 16);
  }

  /** Apply buffered frames (exposed for tests). */
  flush() {
    this.flushScheduled = false;
    let ended = false;
    for (const [connectionId, frames] of this.pending) {
      const s = this.byConnection(connectionId);
      const log = this.logs.get(connectionId);
      if (!s || !log) {
        if (frames.some((f) => f.last)) {
          // A replaced or discarded session ended in the backend: forget it; its history entry is written now.
          this.registered.delete(connectionId);
          this.cancelPending.delete(connectionId);
          if (this.discarded.delete(connectionId)) ended = true;
        }
        continue;
      }
      let bytes = this.logBytes.get(connectionId) ?? 0;
      for (const f of frames) {
        const entry = logEntry(f, ++this.seq);
        bytes += entry.cost;
        log.push(entry);
        s.state = nextState(s.state, f);
        if (DATA_KINDS.has(f.kind)) {
          if (f.dir === "out") s.sent++;
          else if (f.dir === "in") s.received++;
        }
        if (f.dir === "system" && f.kind === "error" && !s.error && s.state === "connecting") s.error = f.data;
        if (f.kind === "open") s.error = null;
        if (f.last) {
          s.endedAt = f.ts;
          ended = true;
          this.registered.delete(connectionId);
          this.cancelPending.delete(connectionId);
        }
      }
      const trimmed = trimLog(log, bytes);
      if (trimmed.log !== log) this.logs.set(connectionId, trimmed.log);
      this.logBytes.set(connectionId, trimmed.bytes);
      s.rev++;
    }
    this.pending.clear();
    if (ended) this.onEnded?.();
  }
}

export const ws = new WsStore();

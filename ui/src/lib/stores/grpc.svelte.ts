/**
 * gRPC services per open request (AC-GRPC-3..5): the cached reflection result or the compiled
 * .proto files for the request's origin, a reflection fetch (cancellable), and its errors.
 * Calls themselves live in the app store (unary Invoke) and the stream store (`ws`, AC-GRPC-8).
 */
import { call, toCommandError, type GrpcCallKind, type GrpcDescriptor, type Request } from "../api";
import { ulid } from "../util/ulid";
import { findMethod } from "../util/grpc";

export type DescriptorErrorKind = "unsupported" | "network" | "failed" | "proto" | "blocked";

export interface DescriptorError {
  kind: DescriptorErrorKind;
  message: string;
}

export interface DescriptorState {
  status: "idle" | "loading" | "fetching" | "ready";
  descriptor: GrpcDescriptor | null;
  error: DescriptorError | null;
  executionId: string | null;
}

const EMPTY: DescriptorState = { status: "idle", descriptor: null, error: null, executionId: null };

/** UI copy for a failed fetch / compile (AC-GRPC-3/4); null for a cancel. */
export function descriptorError(e: unknown): DescriptorError | null {
  const err = toCommandError(e);
  const d = (err.details ?? {}) as { kind?: string };
  switch (err.code) {
    case "grpc_reflection_unsupported":
      return { kind: "unsupported", message: "This server doesn't support reflection. Use .proto files instead." };
    case "grpc_network":
      if (d.kind === "cancelled") return null;
      return { kind: "network", message: `Couldn't reach the server: ${err.message}` };
    case "grpc_proto_invalid":
      return { kind: "proto", message: err.message };
    case "validation":
    case "read_only_newer":
      return { kind: "blocked", message: err.message };
    default:
      return { kind: "failed", message: err.message };
  }
}

class GrpcStore {
  /** requestId → state */
  states = $state<Record<string, DescriptorState>>({});
  /** requestId → serial of the latest lookup (stale answers are dropped). */
  private lookups = new Map<string, number>();
  private deferred = new Map<string, { request: Request; projectId: string | null }>();

  state(requestId: string): DescriptorState {
    return this.states[requestId] ?? EMPTY;
  }

  descriptor(requestId: string): GrpcDescriptor | null {
    return this.state(requestId).descriptor;
  }

  /** The call kind of the request's picked method, when the services list it. */
  callKind(r: Pick<Request, "id" | "grpc">): GrpcCallKind | null {
    const g = r.grpc;
    return g ? (findMethod(this.descriptor(r.id), g.service, g.method)?.callKind ?? null) : null;
  }

  private set(requestId: string, patch: Partial<DescriptorState>) {
    this.states = { ...this.states, [requestId]: { ...this.state(requestId), ...patch } };
  }

  /** A request got a new id (scratch saved to a collection). */
  rekey(fromId: string, toId: string) {
    const st = this.states[fromId];
    if (!st) return;
    const { [fromId]: _old, ...rest } = this.states;
    this.states = { ...rest, [toId]: st };
  }

  /** The cached (reflection) or compiled (files) services, without a network call. */
  async refreshCached(request: Request, projectId: string | null) {
    const id = request.id;
    if (this.state(id).status === "fetching") {
      this.deferred.set(id, { request, projectId });
      return;
    }
    const serial = (this.lookups.get(id) ?? 0) + 1;
    this.lookups.set(id, serial);
    const cur = this.state(id).descriptor;
    const files = request.grpc?.protoSource === "files";
    if (files) this.set(id, { status: "loading" });
    try {
      const res = await call("grpc_cached_descriptor", { request, projectId, known: cur && !files ? { url: cur.url, fetchedAt: cur.fetchedAt } : null });
      if (this.lookups.get(id) !== serial || this.state(id).status === "fetching") return;
      if (res.unchanged) {
        this.set(id, { status: cur ? "ready" : "idle" });
        return;
      }
      // Keep a fetch error on screen (it describes the same origin); a compile error is replaced.
      const keep = !files && this.state(id).error?.kind !== "blocked" ? this.state(id).error : null;
      this.set(id, { status: res.descriptor ? "ready" : "idle", descriptor: res.descriptor, error: res.descriptor ? null : keep });
    } catch (e) {
      if (this.lookups.get(id) !== serial) return;
      const err = descriptorError(e);
      // Not resolvable yet (empty URL…): nothing to show; a compile error is shown, the last services kept.
      if (err?.kind === "proto") this.set(id, { status: this.state(id).descriptor ? "ready" : "idle", error: err });
      else this.set(id, { status: "idle", descriptor: null, error: null });
    }
  }

  /** AC-GRPC-3: ask the server (reflection) or compile the files; the shown services stay on failure. */
  async fetch(request: Request, projectId: string | null): Promise<GrpcDescriptor | null> {
    const id = request.id;
    const executionId = ulid();
    this.set(id, { status: "fetching", error: null, executionId });
    try {
      const d = await call("grpc_fetch_descriptor", { request, projectId, executionId });
      if (this.state(id).executionId !== executionId) return null;
      this.set(id, { status: "ready", descriptor: d, error: null, executionId: null });
      return d;
    } catch (e) {
      if (this.state(id).executionId === executionId) this.set(id, { status: this.state(id).descriptor ? "ready" : "idle", error: descriptorError(e), executionId: null });
      return null;
    } finally {
      const next = this.deferred.get(id);
      this.deferred.delete(id);
      if (next) void this.refreshCached(next.request, next.projectId);
    }
  }

  async cancelFetch(requestId: string) {
    const ex = this.state(requestId).executionId;
    if (!ex) return;
    this.set(requestId, { status: this.state(requestId).descriptor ? "ready" : "idle", executionId: null, error: null });
    await call("cancel_request", { executionId: ex }).catch(() => false);
  }

  /** Forget the cached reflection result of the request's origin. */
  async forget(request: Request, projectId: string | null) {
    await call("grpc_clear_descriptor", { request, projectId }).catch(() => false);
    this.set(request.id, { status: "idle", descriptor: null, error: null });
  }

  dismissError(requestId: string) {
    this.set(requestId, { error: null });
  }

  /** Show a descriptor error that came from a call (Invoke / Connect). */
  reportError(requestId: string, e: unknown) {
    const err = descriptorError(e);
    if (err) this.set(requestId, { error: err });
  }
}

export const grpc = new GrpcStore();

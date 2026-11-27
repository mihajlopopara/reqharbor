/**
 * gRPC helpers shared by the UI and the mock (AC-GRPC-*): defaults, kind stamping, the call target
 * (TLS from the scheme), status names and descriptor lookups. The heavier pieces (example message,
 * JSON completion) live in `../grpc/schema.ts`, loaded with the gRPC editor.
 */
import type { GrpcCallKind, GrpcConfig, GrpcDescriptor, GrpcMethodInfo, GrpcServiceInfo, Request } from "../api/types";
import { parseVersion } from "./request";

/**
 * The first ReqHarbor that edits and runs gRPC requests: the `minClientVersion` every `grpc`
 * request carries (AC-GRPC-1). Mirrors core `GRPC_MIN_CLIENT_VERSION`; fixed, never the running version.
 */
export const GRPC_MIN_CLIENT_VERSION = "0.1.0";

export const KNOWN_PROTO_SOURCES: readonly string[] = ["reflection", "files"];

export function defaultGrpc(): GrpcConfig {
  return { service: "", method: "", message: "{}", protoSource: "reflection", protoFiles: [], importPaths: [], messages: [] };
}

export function isGrpc(r: Pick<Request, "kind">): boolean {
  return r.kind === "grpc";
}

/** Keeps the newer of `current` and `min` (never lowers a `minClientVersion`; an unparseable one stays). */
export function stampAtLeast(current: string | undefined, min: string): string {
  if (current === undefined || current === null) return min;
  const a = parseVersion(current);
  const b = parseVersion(min);
  if (!a || !b) return current;
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i] ? current : min;
  return current;
}

export const CALL_KIND_LABEL: Record<GrpcCallKind, string> = {
  unary: "unary",
  serverStreaming: "server stream",
  clientStreaming: "client stream",
  bidiStreaming: "bidi",
};

/** Long form, as the log's open frame says it. */
export const CALL_KIND_LONG: Record<GrpcCallKind, string> = {
  unary: "unary",
  serverStreaming: "server streaming",
  clientStreaming: "client streaming",
  bidiStreaming: "bidi streaming",
};

/** Client and bidi streams open first (Connect) and send messages one by one. */
export function sendsMessages(kind: GrpcCallKind | null | undefined): boolean {
  return kind === "clientStreaming" || kind === "bidiStreaming";
}

/** Anything but unary answers in the message log. */
export function isStreaming(kind: GrpcCallKind | null | undefined): boolean {
  return !!kind && kind !== "unary";
}

export const STATUS_NAMES = [
  "OK",
  "CANCELLED",
  "UNKNOWN",
  "INVALID_ARGUMENT",
  "DEADLINE_EXCEEDED",
  "NOT_FOUND",
  "ALREADY_EXISTS",
  "PERMISSION_DENIED",
  "RESOURCE_EXHAUSTED",
  "FAILED_PRECONDITION",
  "ABORTED",
  "OUT_OF_RANGE",
  "UNIMPLEMENTED",
  "INTERNAL",
  "UNAVAILABLE",
  "DATA_LOSS",
  "UNAUTHENTICATED",
] as const;

/** "NOT_FOUND" for 5; "CODE_17" for codes this build doesn't know. */
export function statusName(code: number): string {
  return STATUS_NAMES[code] ?? `CODE_${code}`;
}

// ───────────── target (mirrors core) ─────────────

export interface GrpcTarget {
  tls: boolean;
  host: string;
  port: number;
  /** `grpcs://host:443` / `grpc://host:80`: the descriptor cache key (before secret masking). */
  origin: string;
  /** The URL had a path, query or fragment (ignored: "gRPC uses only scheme, host and port"). */
  extra: boolean;
}

const LOOPBACK = /^(localhost|127(?:\.\d+){3}|\[::1\])$/i;

/**
 * Where a gRPC URL goes: `grpcs://` / `https://` = TLS, `grpc://` / `http://` = plaintext HTTP/2,
 * no scheme = TLS except for loopback hosts (AC-REQ-10). Throws with the inline message otherwise.
 */
export function grpcTarget(url: string): GrpcTarget {
  const u = url.trim();
  if (!u) throw new Error("Enter a URL");
  const i = u.indexOf("://");
  let scheme: string | null = null;
  let rest = u;
  if (i >= 0) {
    scheme = u.slice(0, i).toLowerCase();
    rest = u.slice(i + 3);
  }
  const m = /[/?#]/.exec(rest);
  // Userinfo (`user:pw@`) is dropped, like the core does.
  const rawAuthority = m ? rest.slice(0, m.index) : rest;
  const authority = rawAuthority.slice(rawAuthority.lastIndexOf("@") + 1);
  const tail = m ? rest.slice(m.index) : "";
  if (!authority) throw new Error("The URL has no host");
  const pm = /^(\[[^\]]*\]|[^:]*)(?::(\d*))?$/.exec(authority);
  const host = pm?.[1] ?? authority;
  if (!host) throw new Error("The URL has no host");
  let tls: boolean;
  if (scheme === null) tls = !LOOPBACK.test(host);
  else if (scheme === "grpcs" || scheme === "https") tls = true;
  else if (scheme === "grpc" || scheme === "http") tls = false;
  else throw new Error("Use grpc://, grpcs://, http:// or https://");
  const port = pm?.[2] ? Number(pm[2]) : tls ? 443 : 80;
  if (!Number.isInteger(port) || port < 1 || port > 65535) throw new Error(`Invalid port in “${authority}”`);
  return { tls, host, port, origin: `${tls ? "grpcs" : "grpc"}://${host}:${port}`, extra: tail !== "" && tail !== "/" };
}

/** The target, or null when the URL isn't usable yet (for hints and the Settings TLS line). */
export function tryTarget(url: string): GrpcTarget | null {
  try {
    return grpcTarget(url);
  } catch {
    return null;
  }
}

// ───────────── descriptors ─────────────

/** "demo.Greeter" → "Greeter". */
export function shortName(full: string): string {
  const i = full.lastIndexOf(".");
  return i >= 0 ? full.slice(i + 1) : full;
}

export function findMethod(d: GrpcDescriptor | null | undefined, service: string, method: string): GrpcMethodInfo | null {
  if (!d || !service || !method) return null;
  return d.services.find((s) => s.name === service)?.methods.find((m) => m.name === method) ?? null;
}

export interface MethodGroup {
  service: string;
  methods: GrpcMethodInfo[];
}

/** Picker search: substring of the service or method name, case-insensitive; empty groups left out. */
export function filterMethods(services: readonly GrpcServiceInfo[], query: string): MethodGroup[] {
  const q = query.trim().toLowerCase();
  const out: MethodGroup[] = [];
  for (const s of services) {
    const serviceHit = !q || s.name.toLowerCase().includes(q);
    const methods = serviceHit ? s.methods : s.methods.filter((m) => m.name.toLowerCase().includes(q) || `${s.name}/${m.name}`.toLowerCase().includes(q));
    if (methods.length) out.push({ service: s.name, methods });
  }
  return out;
}

export function methodCount(d: GrpcDescriptor | null | undefined): number {
  return d ? d.services.reduce((n, s) => n + s.methods.length, 0) : 0;
}

/** "12 services" / "1 service". */
export function servicesLabel(n: number): string {
  return `${n} service${n === 1 ? "" : "s"}`;
}

/** Response headers / trailers as the log shows them. */
export function metadataLines(pairs: readonly [string, string][]): string {
  return pairs.map(([k, v]) => `${k}: ${v}`).join("\n");
}

/** Placeholder of the URL field. */
export const GRPC_URL_PLACEHOLDER = "grpc://localhost:50051 or grpcs://api.example.com";

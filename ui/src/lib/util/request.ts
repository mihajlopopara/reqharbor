import type { Auth, Body, KnownAuthType, KnownBodyType, Request } from "../api/types";
import { ulid } from "./ulid";

/** Mirrors core looks_like_curl: does this text look like a pasted cURL command? */
export function looksLikeCurl(text: string): boolean {
  let t = text.replace(/^[\s﻿​‌‍⁠]+/, "");
  if (t.startsWith("$ ")) t = t.slice(2);
  return /^curl(?:\.exe)?[\s\\]/.test(t);
}

export function scratchRequest(partial: Partial<Request> = {}): Request {
  return {
    id: ulid(),
    collectionId: "",
    folderId: null,
    name: "Untitled request",
    method: "GET",
    url: "",
    params: [],
    headers: [],
    auth: { type: "none" },
    body: { type: "none" },
    settings: { followRedirects: true, timeoutMs: 30000, verifyTls: true },
    sortOrder: 0,
    createdAt: 0,
    updatedAt: 0,
    ...partial,
  };
}

/** Comparable form of a request for dirty tracking (ignores timestamps/placement). */
export function requestFingerprint(r: Request): string {
  const { updatedAt: _u, createdAt: _c, sortOrder: _s, ...rest } = r;
  return JSON.stringify(rest);
}

export function bodySummary(body: Body): string | null {
  switch (body.type) {
    case "none":
      return null;
    case "json":
      return "JSON body";
    case "xml":
      return "XML body";
    case "text":
      return "Text body";
    case "form":
      return `Form body (${body.form?.length ?? 0} fields)`;
    case "formEncoded":
      return `Form-encode body (${body.formEncoded?.length ?? 0} fields)`;
    case "graphql":
      return "GraphQL body";
    case "binary":
      return "Binary body";
    default:
      return "Body from a newer ReqHarbor";
  }
}

// ───────────── forward compatibility (AC-FWD-2..4) ─────────────

/** Shown wherever a request from a newer ReqHarbor can't be edited or sent. */
export const NEEDS_NEWER_VERSION = "This request needs a newer version of ReqHarbor";
/** Tree badge tooltip / aria-label (UX §6.3). */
export const REQUIRES_NEWER = "Requires a newer ReqHarbor";

export const KNOWN_BODY_TYPES: readonly KnownBodyType[] = ["none", "json", "xml", "text", "form", "formEncoded", "graphql", "binary"];

/** `major.minor.patch` (missing parts 0; leading "v" and -pre/+build ignored); null if unparseable. */
export function parseVersion(v: string): [number, number, number] | null {
  const core = v.trim().replace(/^v/, "").split(/[-+]/)[0];
  const parts = core.split(".");
  if (parts.length > 3 || parts.some((p) => !/^\d+$/.test(p))) return null;
  const n = parts.map(Number);
  return [n[0] ?? 0, n[1] ?? 0, n[2] ?? 0];
}

/** Mirrors core `requires_newer_client`: unparseable versions count as newer. */
export function requiresNewerClient(required: string, current: string): boolean {
  const a = parseVersion(required);
  const b = parseVersion(current);
  if (!a || !b) return true;
  for (let i = 0; i < 3; i++) if (a[i] !== b[i]) return a[i] > b[i];
  return false;
}

export const KNOWN_AUTH_TYPES: readonly KnownAuthType[] = ["none", "inherit", "basic", "bearer", "apikey"];
const KNOWN_API_KEY_IN = ["header", "query"];
const KNOWN_FORM_KINDS = ["text", "file"];

/** Mirrors core `Auth::is_known`: the scheme and the API key location are ones this build knows. */
export function isKnownAuth(a: Auth | null | undefined): boolean {
  if (!a) return true;
  if (!(KNOWN_AUTH_TYPES as readonly string[]).includes(a.type)) return false;
  return !a.apiKey || KNOWN_API_KEY_IN.includes(a.apiKey.in);
}

type ReadOnlyFields = Pick<Request, "kind" | "minClientVersion" | "body"> & { auth?: Auth; ws?: Request["ws"]; grpc?: Request["grpc"] };

/** Request kinds this build edits and runs (AC-FWD-2, AC-WS-1, AC-GRPC-1). */
export const EDITABLE_KINDS: readonly string[] = ["http", "ws", "grpc"];
const KNOWN_PROTO_SOURCES = ["reflection", "files"];
const KNOWN_MESSAGE_KINDS = ["text", "json", "binary"];

/**
 * Why a request is read-only here (AC-FWD-3/4), as the end of "This request needs a newer version
 * of ReqHarbor: …" — or null when it is editable. Mirrors core `Request::is_editable`.
 */
export function readOnlyReason(r: ReadOnlyFields, appVersion: string): string | null {
  const kind = r.kind ?? "http";
  if (!EDITABLE_KINDS.includes(kind)) return `This is a “${kind}” request`;
  const msg = (r.ws?.messages ?? []).find((m) => !KNOWN_MESSAGE_KINDS.includes(m.kind));
  if (msg) return `Its saved message “${msg.name}” is of kind “${msg.kind}”`;
  if (r.ws?.socketIo && r.ws.socketIo.version !== 4) return `It uses Socket.IO version ${r.ws.socketIo.version}`;
  if (r.grpc && r.grpc.protoSource !== undefined && !KNOWN_PROTO_SOURCES.includes(r.grpc.protoSource)) return `Its gRPC services come from “${r.grpc.protoSource}”`;
  if (!(KNOWN_BODY_TYPES as readonly string[]).includes(r.body.type)) return `Its body is “${r.body.type}”`;
  const field = (r.body.form ?? []).find((f) => !KNOWN_FORM_KINDS.includes(f.kind));
  if (field) return `Its form field “${field.key}” is of kind “${field.kind}”`;
  if (r.auth && !(KNOWN_AUTH_TYPES as readonly string[]).includes(r.auth.type)) return `Its auth is “${r.auth.type}”`;
  if (r.auth?.apiKey && !KNOWN_API_KEY_IN.includes(r.auth.apiKey.in)) return `Its API key goes in “${r.auth.apiKey.in}”`;
  if (r.minClientVersion !== undefined && r.minClientVersion !== null && requiresNewerClient(r.minClientVersion, appVersion)) {
    return `It needs ReqHarbor ${r.minClientVersion} or later`;
  }
  return null;
}

/**
 * Mirrors core `extra_requires_newer` for projects, collections, folders and environments
 * (AC-FWD-4): a `minClientVersion` above this build (or one that isn't a string) makes the item
 * read-only here.
 */
export function isContainerReadOnly(e: { minClientVersion?: unknown } | null | undefined, appVersion: string): boolean {
  const v = e?.minClientVersion;
  if (v === undefined || v === null) return false;
  return typeof v !== "string" || requiresNewerClient(v, appVersion);
}

/** Mirrors core `Request::is_editable` (negated): see [`readOnlyReason`]. */
export function isReadOnlyNewer(r: ReadOnlyFields, appVersion: string): boolean {
  return readOnlyReason(r, appVersion) !== null;
}

/** Tree method column for a read-only request: the kind cut to 4 chars (GQL, GRPC), or NEW when absent (UX §6.3). */
export function newerKindLabel(r: Pick<Request, "kind">): string {
  const k = r.kind ?? "http";
  if (k === "http") return "NEW";
  if (k === "graphql") return "GQL";
  return k.toUpperCase().slice(0, 4);
}

export const AUTH_LABEL: Record<KnownAuthType, string> = {
  none: "No auth",
  inherit: "Inherit auth",
  basic: "Basic auth",
  bearer: "Bearer auth",
  apikey: "API key auth",
};

/** AUTH_LABEL, with a fallback for schemes from a newer ReqHarbor. */
export function authLabel(type: Auth["type"]): string {
  return (AUTH_LABEL as Record<string, string>)[type] ?? `${type} auth`;
}

export function requestSummary(r: Request): string {
  const parts: string[] = [];
  const h = r.headers.filter((x) => x.enabled).length;
  if (h) parts.push(`${h} header${h === 1 ? "" : "s"}`);
  const q = r.params.filter((x) => x.enabled).length;
  if (q) parts.push(`${q} query param${q === 1 ? "" : "s"}`);
  const b = bodySummary(r.body);
  if (b) parts.push(b);
  if (r.auth.type !== "none") parts.push(authLabel(r.auth.type));
  return parts.join(" · ") || "No headers or body";
}

export const BODY_CONTENT_TYPE: Partial<Record<Body["type"], string>> = {
  json: "application/json",
  xml: "application/xml",
  text: "text/plain",
  formEncoded: "application/x-www-form-urlencoded",
  form: "multipart/form-data",
  graphql: "application/json",
  binary: "application/octet-stream",
};

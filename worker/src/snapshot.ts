// Share snapshot format, validation, and defensive normalization for rendering.
//
// Snapshot (built and sanitized by the client, §5.11):
//   {
//     schemaVersion: 1,
//     collection?: Collection & { folders: Folder[], requests: Request[] },   // kind = "collection"
//     request?:    Request,                                                    // kind = "request"
//     environment?: { name, variables: { key, value, enabled, secret }[] } | null
//   }
// Entities use the PRODUCT.md §3 data model (flat folders/requests linked by ids).

import type { AuthType, BodyType, CurlAuth, CurlBody, CurlFormField, CurlKeyValue, CurlRequest } from "./curl";
import { LIMITS, badRequest, isPlainObject, tooLarge, utf8Length } from "./http";
import { removeSecretVariables } from "./sanitize";

export type ShareKind = "collection" | "request";
export const SHARE_KINDS: readonly ShareKind[] = ["collection", "request"];

/** Validates a client snapshot for `kind`, strips secret variables, returns the JSON to store. */
export function prepareSnapshot(kind: ShareKind, raw: unknown): string {
  if (!isPlainObject(raw)) throw badRequest("`snapshot` must be an object");
  const v = raw.schemaVersion;
  if (typeof v !== "number" || !Number.isSafeInteger(v) || v < 1) {
    throw badRequest("`snapshot.schemaVersion` must be a positive integer");
  }
  if (!isPlainObject(raw[kind])) throw badRequest(`\`snapshot.${kind}\` must be an object for a ${kind} share`);
  if (kind === "collection") {
    const c = raw.collection as Record<string, unknown>;
    for (const k of ["folders", "requests"] as const) {
      if (c[k] !== undefined && !Array.isArray(c[k])) throw badRequest(`\`snapshot.collection.${k}\` must be an array`);
    }
  }
  const envRaw = raw.environment;
  if (envRaw !== undefined && envRaw !== null && !isPlainObject(envRaw)) {
    throw badRequest("`snapshot.environment` must be an object or null");
  }
  const clean: Record<string, unknown> = { ...raw };
  if (isPlainObject(envRaw)) clean.environment = removeSecretVariables(envRaw);
  const text = JSON.stringify(clean);
  if (utf8Length(text) > LIMITS.snapshotBytes) {
    throw tooLarge(`Snapshot exceeds ${LIMITS.snapshotBytes} bytes`, "snapshot_too_large");
  }
  return text;
}

// ---------------------------------------------------------------------------------------------
// Normalization: never trust shapes in stored snapshots when rendering.

export interface ViewRequest extends CurlRequest {
  key: string;
  name: string;
  folderId: string | null;
  sortOrder: number;
  params: CurlKeyValue[];
  settings: { followRedirects: boolean; timeoutMs: number; verifyTls: boolean };
}

export interface ViewFolder {
  id: string;
  name: string;
  parentFolderId: string | null;
  sortOrder: number;
}

export interface ViewSnapshot {
  kind: ShareKind;
  name: string;
  description: string;
  collectionAuth?: CurlAuth;
  folders: ViewFolder[];
  requests: ViewRequest[];
  environment: { name: string; variables: { key: string; value: string; enabled: boolean }[] } | null;
}

const str = (v: unknown, d = ""): string => (typeof v === "string" ? v : typeof v === "number" ? String(v) : d);
const bool = (v: unknown, d: boolean): boolean => (typeof v === "boolean" ? v : d);
const num = (v: unknown, d: number): number => (typeof v === "number" && Number.isFinite(v) ? v : d);
const arr = (v: unknown): unknown[] => (Array.isArray(v) ? v : []);
const obj = (v: unknown): Record<string, unknown> => (isPlainObject(v) ? v : {});

const AUTH_TYPES: readonly AuthType[] = ["none", "inherit", "basic", "bearer", "apikey"];
const BODY_TYPES: readonly BodyType[] = ["none", "json", "xml", "text", "form", "formEncoded", "graphql", "binary"];

function kvList(v: unknown): CurlKeyValue[] {
  return arr(v).map((x) => {
    const o = obj(x);
    return { key: str(o.key), value: str(o.value), enabled: bool(o.enabled, true) };
  });
}

export function normalizeAuth(v: unknown): CurlAuth {
  const o = obj(v);
  const type = AUTH_TYPES.includes(o.type as AuthType) ? (o.type as AuthType) : "none";
  const ak = obj(o.apiKey);
  return {
    type,
    username: str(o.username),
    password: str(o.password),
    token: str(o.token),
    apiKey: { key: str(ak.key), value: str(ak.value), in: ak.in === "query" ? "query" : "header" },
  };
}

function normalizeBody(v: unknown): CurlBody {
  const o = obj(v);
  const type = BODY_TYPES.includes(o.type as BodyType) ? (o.type as BodyType) : "none";
  const gq = obj(o.graphql);
  const form: CurlFormField[] = arr(o.form).map((x) => {
    const f = obj(x);
    const field: CurlFormField = {
      key: str(f.key),
      value: str(f.value),
      enabled: bool(f.enabled, true),
      kind: f.kind === "file" ? "file" : "text",
    };
    if (typeof f.contentType === "string" && f.contentType) field.contentType = f.contentType;
    return field;
  });
  return {
    type,
    text: str(o.text),
    form,
    formEncoded: kvList(o.formEncoded),
    graphql: { query: str(gq.query), variables: gq.variables },
    binaryPath: str(o.binaryPath),
  };
}

function normalizeRequest(v: unknown, index: number): ViewRequest {
  const o = obj(v);
  const s = obj(o.settings);
  // Kept raw like the core model (default "GET" only when absent); the exporter trims/validates.
  const method = typeof o.method === "string" ? o.method : "GET";
  return {
    key: `r${index}`,
    name: str(o.name) || "Untitled request",
    folderId: typeof o.folderId === "string" ? o.folderId : null,
    sortOrder: num(o.sortOrder, index),
    method,
    url: str(o.url),
    params: kvList(o.params),
    headers: kvList(o.headers),
    auth: normalizeAuth(o.auth),
    body: normalizeBody(o.body),
    settings: {
      followRedirects: bool(s.followRedirects, true),
      timeoutMs: num(s.timeoutMs, 30000),
      verifyTls: bool(s.verifyTls, true),
    },
  };
}

export function normalizeSnapshot(kind: ShareKind, snapshot: unknown): ViewSnapshot {
  const root = obj(snapshot);
  const envRaw = isPlainObject(root.environment) ? root.environment : null;
  const environment = envRaw
    ? {
        name: str(envRaw.name) || "Environment",
        variables: arr(envRaw.variables)
          .filter((x) => !(isPlainObject(x) && x.secret === true))
          .map((x) => {
            const o = obj(x);
            return { key: str(o.key), value: str(o.value), enabled: bool(o.enabled, true) };
          }),
      }
    : null;

  if (kind === "request") {
    const req = normalizeRequest(root.request, 0);
    req.folderId = null;
    return { kind, name: req.name, description: "", folders: [], requests: [req], environment };
  }
  const c = obj(root.collection);
  return {
    kind,
    name: str(c.name) || "Untitled collection",
    description: str(c.description),
    collectionAuth: normalizeAuth(c.auth),
    folders: arr(c.folders).map((x, i) => {
      const f = obj(x);
      return {
        id: str(f.id) || `folder-${i}`,
        name: str(f.name) || "Untitled folder",
        parentFolderId: typeof f.parentFolderId === "string" ? f.parentFolderId : null,
        sortOrder: num(f.sortOrder, i),
      };
    }),
    requests: arr(c.requests).map(normalizeRequest),
    environment,
  };
}

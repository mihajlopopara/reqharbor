import type { Auth, Body, Request } from "../api/types";
import { ulid } from "./ulid";

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
  }
}

export const AUTH_LABEL: Record<Auth["type"], string> = {
  none: "No auth",
  inherit: "Inherit auth",
  basic: "Basic auth",
  bearer: "Bearer auth",
  apikey: "API key auth",
};

export function requestSummary(r: Request): string {
  const parts: string[] = [];
  const h = r.headers.filter((x) => x.enabled).length;
  if (h) parts.push(`${h} header${h === 1 ? "" : "s"}`);
  const q = r.params.filter((x) => x.enabled).length;
  if (q) parts.push(`${q} query param${q === 1 ? "" : "s"}`);
  const b = bodySummary(r.body);
  if (b) parts.push(b);
  if (r.auth.type !== "none") parts.push(AUTH_LABEL[r.auth.type]);
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

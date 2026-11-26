/** Browser-mock OpenAPI/Swagger importer (AC-OAS-1..8, simplified). */
import type { Auth, Body, FormField, ImportReport, KeyValue, SpecInfo, Variable } from "../types";
import { fail } from "./errors";
import { parseYaml } from "./yaml";

type Obj = Record<string, unknown>;
const isObj = (v: unknown): v is Obj => !!v && typeof v === "object" && !Array.isArray(v);

export interface ConvertedRequest {
  name: string;
  method: string;
  url: string;
  params: KeyValue[];
  headers: KeyValue[];
  body: Body;
  auth: Auth;
  folder: string | null;
}

export interface Converted {
  spec: SpecInfo;
  title: string;
  baseUrl: string;
  auth: Auth;
  requests: ConvertedRequest[];
  envVariables: Variable[];
  warnings: string[];
}

const METHODS = ["get", "put", "post", "delete", "options", "head", "patch", "trace", "query"];

export function parseSpec(text: string): { doc: Obj; format: "json" | "yaml" } {
  const t = text.replace(/^﻿/, "").trim();
  if (!t) fail("openapi_parse", "The document is empty");
  let doc: unknown;
  let format: "json" | "yaml";
  if (t.startsWith("{") || t.startsWith("[")) {
    format = "json";
    try {
      doc = JSON.parse(t);
    } catch (e) {
      return fail("openapi_parse", `Invalid JSON: ${(e as Error).message}`);
    }
  } else {
    format = "yaml";
    try {
      doc = parseYaml(t);
    } catch (e) {
      return fail("openapi_parse", `Invalid YAML: ${(e as Error).message}`);
    }
  }
  if (!isObj(doc)) return fail("openapi_invalid", "The document is not an object");
  return { doc, format };
}

export function convertSpec(text: string): Converted {
  const { doc, format } = parseSpec(text);
  let kind: SpecInfo["kind"];
  let version: string;
  if (typeof doc.swagger === "string" || typeof doc.swagger === "number") {
    version = String(doc.swagger);
    if (!version.startsWith("2.0")) fail("openapi_unsupported", `Unsupported Swagger version ${version}`);
    kind = "swagger2";
  } else if (typeof doc.openapi === "string") {
    version = doc.openapi;
    if (!/^3\.[0-2](\.|$)/.test(version)) fail("openapi_unsupported", `Unsupported OpenAPI version ${version}`);
    kind = "openapi3";
  } else {
    return fail("openapi_unsupported", "Not an OpenAPI or Swagger document (no `openapi` / `swagger` field)");
  }
  if (!isObj(doc.paths)) fail("openapi_invalid", "The document has no `paths` object");
  const short = kind === "swagger2" ? "Swagger 2.0" : `OpenAPI ${version.split(".").slice(0, 2).join(".")}`;
  const spec: SpecInfo = { format, kind, version, label: `${short} · ${format.toUpperCase()}` };
  const warnings: string[] = [];
  const info = isObj(doc.info) ? doc.info : {};
  const title = typeof info.title === "string" && info.title.trim() ? info.title.trim() : "Imported API";

  const resolve = (v: unknown, depth = 0): unknown => {
    if (!isObj(v) || typeof v.$ref !== "string") return v;
    const ref = v.$ref;
    if (!ref.startsWith("#/")) {
      if (!warnings.includes(`Skipped external $ref: ${ref}`)) warnings.push(`Skipped external $ref: ${ref}`);
      return undefined;
    }
    if (depth > 20) return undefined;
    let cur: unknown = doc;
    for (const p of ref.slice(2).split("/")) {
      const key = p.replace(/~1/g, "/").replace(/~0/g, "~");
      cur = isObj(cur) ? cur[key] : undefined;
    }
    return resolve(cur, depth + 1);
  };

  let baseUrl = "";
  if (kind === "openapi3") {
    const servers = Array.isArray(doc.servers) ? doc.servers : [];
    const s = servers[0];
    if (isObj(s) && typeof s.url === "string") {
      baseUrl = s.url.replace(/\{([^}]+)\}/g, (_m, name: string) => {
        const vars = isObj(s.variables) ? s.variables : {};
        const d = isObj(vars[name]) ? (vars[name] as Obj).default : undefined;
        return d !== undefined ? String(d) : "";
      });
    }
  } else {
    const schemes = Array.isArray(doc.schemes) && doc.schemes.length ? String(doc.schemes[0]) : "https";
    if (typeof doc.host === "string") baseUrl = `${schemes}://${doc.host}${typeof doc.basePath === "string" ? doc.basePath : ""}`;
    else if (typeof doc.basePath === "string") baseUrl = doc.basePath;
  }
  baseUrl = baseUrl.replace(/\/$/, "");

  // Security → collection auth
  const schemes = (kind === "openapi3"
    ? isObj(doc.components) && isObj((doc.components as Obj).securitySchemes)
      ? ((doc.components as Obj).securitySchemes as Obj)
      : {}
    : isObj(doc.securityDefinitions)
      ? doc.securityDefinitions
      : {}) as Obj;
  const authVars = new Set<string>();
  const schemeToAuth = (name: string): Auth | null => {
    const s = resolve(schemes[name]);
    if (!isObj(s)) return null;
    const type = String(s.type ?? "");
    const scheme = String(s.scheme ?? "").toLowerCase();
    if (type === "http" && scheme === "bearer") {
      authVars.add("token");
      return { type: "bearer", token: "{{token}}" };
    }
    if ((type === "http" && scheme === "basic") || type === "basic") {
      authVars.add("username");
      authVars.add("password");
      return { type: "basic", username: "{{username}}", password: "{{password}}" };
    }
    if (type === "apiKey") {
      authVars.add("apiKey");
      const where = s.in === "query" ? "query" : "header";
      return { type: "apikey", apiKey: { key: String(s.name ?? "X-API-Key"), value: "{{apiKey}}", in: where } };
    }
    if (type === "oauth2" || type === "openIdConnect") {
      warnings.push(`Security scheme "${name}" (${type}) is not supported; imported as no auth`);
      return { type: "none" };
    }
    return null;
  };
  let auth: Auth = { type: "none" };
  const globalSec = Array.isArray(doc.security) ? doc.security : [];
  const firstReq = globalSec.find(isObj);
  const secNames = firstReq ? Object.keys(firstReq) : Object.keys(schemes).slice(0, 1);
  if (secNames.length) auth = schemeToAuth(secNames[0]) ?? { type: "none" };

  const pathParams = new Set<string>();
  const requests: ConvertedRequest[] = [];
  const skeleton = (schema: unknown, depth: number, seen: Set<unknown>): unknown => {
    const s = resolve(schema);
    if (!isObj(s) || depth > 5 || seen.has(s)) return {};
    if (s.example !== undefined) return s.example;
    if (s.default !== undefined) return s.default;
    const next = new Set(seen).add(s);
    if (Array.isArray(s.allOf)) return Object.assign({}, ...s.allOf.map((x) => skeleton(x, depth + 1, next) as object));
    if (Array.isArray(s.oneOf) && s.oneOf.length) return skeleton(s.oneOf[0], depth + 1, next);
    if (Array.isArray(s.anyOf) && s.anyOf.length) return skeleton(s.anyOf[0], depth + 1, next);
    if (Array.isArray(s.enum) && s.enum.length) return s.enum[0];
    const type = Array.isArray(s.type) ? s.type[0] : s.type;
    if (type === "object" || isObj(s.properties)) {
      const out: Obj = {};
      for (const [k, v] of Object.entries((s.properties as Obj) ?? {})) out[k] = skeleton(v, depth + 1, next);
      return out;
    }
    if (type === "array") return [skeleton(s.items, depth + 1, next)];
    if (type === "integer" || type === "number") return 0;
    if (type === "boolean") return false;
    if (type === "string") return s.format === "date-time" ? "2026-01-01T00:00:00Z" : "string";
    return {};
  };
  const exampleOf = (p: Obj): string => {
    const schema = resolve(p.schema);
    const v =
      p.example ??
      p.default ??
      (isObj(schema) ? (schema.example ?? schema.default ?? (Array.isArray(schema.examples) ? schema.examples[0] : undefined)) : undefined);
    return v === undefined ? "" : String(v);
  };
  const propsRows = (schema: unknown): { key: string; value: string; file: boolean }[] => {
    const s = resolve(schema);
    if (!isObj(s) || !isObj(s.properties)) return [];
    return Object.entries(s.properties).map(([k, v]) => {
      const pv = resolve(v);
      const file = isObj(pv) && (pv.format === "binary" || pv.type === "file");
      const ex = isObj(pv) ? (pv.example ?? pv.default) : undefined;
      return { key: k, value: ex === undefined ? "" : String(ex), file };
    });
  };

  for (const [path, rawItem] of Object.entries(doc.paths as Obj)) {
    const item = resolve(rawItem);
    if (!isObj(item)) continue;
    const common = Array.isArray(item.parameters) ? item.parameters : [];
    for (const m of METHODS) {
      const op = item[m];
      if (!isObj(op)) continue;
      const method = m.toUpperCase();
      const name =
        (typeof op.summary === "string" && op.summary.trim()) ||
        (typeof op.operationId === "string" && op.operationId) ||
        `${method} ${path}`;
      const url = "{{baseUrl}}" + path.replace(/\{([^}]+)\}/g, (_x, p: string) => {
        pathParams.add(p);
        return `{{${p}}}`;
      });
      const params: KeyValue[] = [];
      const headers: KeyValue[] = [];
      let body: Body = { type: "none" };
      const byKey = new Map<string, Obj>();
      for (const p of [...common, ...(Array.isArray(op.parameters) ? op.parameters : [])]) {
        const rp = resolve(p);
        if (isObj(rp)) byKey.set(`${rp.in}:${rp.name}`, rp);
      }
      const formData: Obj[] = [];
      for (const p of byKey.values()) {
        const row = { key: String(p.name), value: exampleOf(p), enabled: p.required === true };
        if (p.in === "query") params.push(row);
        else if (p.in === "header") headers.push({ ...row, enabled: true });
        else if (p.in === "body") body = { type: "json", text: JSON.stringify(skeleton(p.schema, 0, new Set()), null, 2) };
        else if (p.in === "formData") formData.push(p);
      }
      if (formData.length) {
        const consumes = (Array.isArray(op.consumes) ? op.consumes : Array.isArray(doc.consumes) ? doc.consumes : []).map(String);
        const multipart = consumes.includes("multipart/form-data") || formData.some((p) => p.type === "file");
        body = multipart
          ? {
              type: "form",
              form: formData.map<FormField>((p) => ({ key: String(p.name), value: p.type === "file" ? "" : exampleOf(p), kind: p.type === "file" ? "file" : "text", enabled: true })),
            }
          : { type: "formEncoded", formEncoded: formData.map((p) => ({ key: String(p.name), value: exampleOf(p), enabled: true })) };
      }
      const rb = resolve(op.requestBody);
      if (isObj(rb) && isObj(rb.content)) {
        const types = Object.keys(rb.content);
        const mt = types.find((t) => t === "application/json") ?? types.find((t) => t.includes("json")) ?? types[0];
        const media = resolve((rb.content as Obj)[mt]);
        if (isObj(media)) {
          if (mt === "multipart/form-data") {
            body = { type: "form", form: propsRows(media.schema).map((r) => ({ key: r.key, value: r.value, kind: r.file ? "file" : "text", enabled: true })) };
          } else if (mt === "application/x-www-form-urlencoded") {
            body = { type: "formEncoded", formEncoded: propsRows(media.schema).map((r) => ({ key: r.key, value: r.value, enabled: true })) };
          } else {
            let ex: unknown = media.example;
            if (ex === undefined && isObj(media.examples)) {
              const first = resolve(Object.values(media.examples)[0]);
              if (isObj(first)) ex = first.value;
            }
            if (ex === undefined) ex = skeleton(media.schema, 0, new Set());
            const t = typeof ex === "string" ? ex : JSON.stringify(ex, null, 2);
            body = mt.includes("json") ? { type: "json", text: t } : mt.includes("xml") ? { type: "xml", text: t } : { type: "text", text: t };
          }
        }
      }
      let reqAuth: Auth = { type: "inherit" };
      if (Array.isArray(op.security)) {
        if (op.security.length === 0) reqAuth = { type: "none" };
        else {
          const first = op.security.find(isObj);
          const n = first ? Object.keys(first)[0] : undefined;
          if (n && n !== secNames[0]) reqAuth = schemeToAuth(n) ?? { type: "inherit" };
        }
      }
      const tags = Array.isArray(op.tags) ? op.tags : [];
      requests.push({ name, method, url, params, headers, body, auth: reqAuth, folder: tags.length ? String(tags[0]) : null });
    }
  }

  const envVariables: Variable[] = [{ key: "baseUrl", value: baseUrl, enabled: true, secret: false }];
  for (const p of pathParams) envVariables.push({ key: p, value: "", enabled: true, secret: false });
  for (const v of authVars) envVariables.push({ key: v, value: "", enabled: true, secret: v !== "username" });
  return { spec, title, baseUrl, auth, requests, envVariables, warnings };
}

export function reportOf(c: Converted): ImportReport {
  const folders = new Map<string, number>();
  let root = 0;
  for (const r of c.requests) {
    if (r.folder) folders.set(r.folder, (folders.get(r.folder) ?? 0) + 1);
    else root++;
  }
  return {
    spec: c.spec,
    collectionName: c.title,
    operationsCreated: c.requests.length,
    folders: [...folders].map(([name, requestCount]) => ({ name, requestCount })),
    rootRequestCount: root,
    baseUrl: c.baseUrl,
    warnings: c.warnings,
  };
}

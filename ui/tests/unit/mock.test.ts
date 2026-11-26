import { beforeEach, describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { MockServer } from "../../src/lib/api/mock";
import { makeCall, type CommandError, type Request } from "../../src/lib/api/types";
import { scratchRequest } from "../../src/lib/util/request";

let server: MockServer;
let call: ReturnType<typeof makeCall>;

async function rejects(p: Promise<unknown>): Promise<CommandError> {
  try {
    await p;
  } catch (e) {
    return e as CommandError;
  }
  throw new Error("expected rejection");
}

beforeEach(() => {
  server = new MockServer({ latency: 0, platform: "linux" });
  call = makeCall((cmd, args) => server.invoke(cmd, args));
});

describe("bootstrap & settings", () => {
  it("returns a seeded workspace with Global first", async () => {
    const b = await call("bootstrap", {});
    expect(b.workspace.name).toBe("My Workspace");
    expect(b.projects.map((p) => p.name)).toEqual(["Default", "Side project"]);
    expect(b.environments[0].projectId).toBeNull();
    expect(b.platform).toBe("linux");
    expect(b.sync.state).toBe("disconnected");
    expect(b.sync.skippedEntities).toEqual([]);
    expect(b.sync.warning).toBeNull();
    expect(b.corruptFiles).toEqual([]);
  });
  it("first launch seed has Default project and Global env only", async () => {
    const s = new MockServer({ empty: true, latency: 0 });
    const b = (await s.invoke("bootstrap", {})) as { projects: unknown[]; environments: unknown[] };
    expect(b.projects).toHaveLength(1);
    expect(b.environments).toHaveLength(1);
    expect(await s.invoke("list_history", {})).toEqual([]);
  });
  it("save_settings preserves unknown keys", async () => {
    const s = await call("get_settings", {});
    const saved = await call("save_settings", { settings: { ...s, sidebarTab: "env", sidebarWidth: 320 } });
    expect(saved.sidebarTab).toBe("env");
    expect((await call("get_settings", {})).sidebarWidth).toBe(320);
  });
  it("results are copies (IPC boundary)", async () => {
    const t1 = await call("get_project_tree", { projectId: server.projects[0].id });
    t1.requests[0].name = "mutated";
    const t2 = await call("get_project_tree", { projectId: server.projects[0].id });
    expect(t2.requests[0].name).not.toBe("mutated");
  });
});

describe("errors follow the contract", () => {
  it("not_found / invalid", async () => {
    expect((await rejects(call("get_project_tree", { projectId: "nope" }))).code).toBe("not_found");
    expect((await rejects(call("create_collection", { projectId: server.projects[0].id, name: "  " }))).code).toBe("invalid");
    const g = server.environments.find((e) => e.projectId === null)!;
    expect((await rejects(call("delete_environment", { environmentId: g.id }))).code).toBe("invalid");
    const single = new MockServer({ empty: true, latency: 0 });
    const e = (await rejects(single.invoke("delete_project", { projectId: single.projects[0].id }))) as CommandError;
    expect(e.code).toBe("invalid");
  });
  it("unknown command -> internal", async () => {
    expect((await rejects(server.invoke("nope", {}))).code).toBe("internal");
  });
});

describe("collections, folders, requests", () => {
  it("create / rename / duplicate / delete with child counts", async () => {
    const pid = server.projects[0].id;
    const c = await call("create_collection", { projectId: pid, name: "Pets" });
    const f = await call("create_folder", { collectionId: c.id, name: "Cats" });
    const sub = await call("create_folder", { collectionId: c.id, parentFolderId: f.id, name: "Kittens" });
    const r = await call("create_request", { collectionId: c.id, folderId: sub.id });
    expect(r.name).toBe("Untitled request");
    expect(await call("child_counts", { kind: "collection", id: c.id })).toEqual({ collections: 0, folders: 2, requests: 1 });
    expect(await call("child_counts", { kind: "folder", id: f.id })).toEqual({ collections: 0, folders: 1, requests: 1 });
    await call("rename_folder", { folderId: f.id, name: "Felines" });
    const dup = await call("duplicate_folder", { folderId: f.id });
    expect(dup.folders.filter((x) => x.name === "Felines copy")).toHaveLength(1);
    expect(dup.requests.filter((x) => x.collectionId === c.id)).toHaveLength(2);
    const after = await call("delete_folder", { folderId: f.id });
    expect(after.folders.some((x) => x.id === sub.id)).toBe(false);
    expect(after.requests.some((x) => x.id === r.id)).toBe(false);
  });

  it("save_request ignores placement and validates the method", async () => {
    const r = structuredClone(server.requests[0]);
    const saved = await call("save_request", { request: { ...r, name: "Renamed", collectionId: "other", sortOrder: 99 } });
    expect(saved.name).toBe("Renamed");
    expect(saved.collectionId).toBe(r.collectionId);
    expect(saved.updatedAt).toBeGreaterThan(r.updatedAt);
    expect((await rejects(call("save_request", { request: { ...r, method: "bad method" } }))).code).toBe("invalid");
  });

  it("move_node reorders and moves; rejects moving a folder into itself", async () => {
    const users = server.collections.find((c) => c.name === "Users API")!;
    const billing = server.collections.find((c) => c.name === "Billing")!;
    const health = server.requests.find((r) => r.name === "Health")!;
    const tree = await call("move_node", { kind: "request", id: health.id, targetCollectionId: billing.id, targetFolderId: null, index: 0 });
    const moved = tree.requests.find((r) => r.id === health.id)!;
    expect(moved.collectionId).toBe(billing.id);
    const admin = server.folders.find((f) => f.name === "Admin")!;
    const audit = server.folders.find((f) => f.name === "Audit")!;
    expect((await rejects(call("move_node", { kind: "folder", id: admin.id, targetCollectionId: users.id, targetFolderId: audit.id, index: 0 }))).code).toBe("invalid");
    const t2 = await call("move_node", { kind: "collection", id: billing.id, index: 0 });
    expect([...t2.collections].sort((a, b) => a.sortOrder - b.sortOrder)[0].id).toBe(billing.id);
  });
});

describe("environments & variables", () => {
  it("get_variables: active project env first, then Global; disabled skipped", async () => {
    const vars = await call("get_variables", { projectId: server.projects[0].id });
    const base = vars.find((v) => v.key === "baseUrl")!;
    expect(base.environmentName).toBe("Staging");
    expect(vars.find((v) => v.key === "apiVersion")?.environmentName).toBe("Global");
    expect(vars.some((v) => v.key === "debug")).toBe(false);
    expect(vars.find((v) => v.key === "token")?.secret).toBe(true);
  });
  it("set_active_environment validates ownership", async () => {
    const side = server.projects[1];
    const staging = server.environments.find((e) => e.name === "Staging")!;
    expect((await rejects(call("set_active_environment", { projectId: side.id, environmentId: staging.id }))).code).toBe("invalid");
    const p = await call("set_active_environment", { projectId: server.projects[0].id, environmentId: null });
    expect(p.activeEnvironmentId).toBeNull();
  });
});

describe("send_request", () => {
  const req = (p: Partial<Request>): Request => scratchRequest(p);

  it("returns a JSON 200 with metadata and a fetchable body", async () => {
    const res = await call("send_request", { executionId: "e1", request: req({ url: "{{baseUrl}}/v1/users?limit=2" }), projectId: server.projects[0].id });
    expect(res.error).toBeNull();
    expect(res.response?.status).toBe(200);
    expect(res.response?.statusText).toBe("OK");
    expect(res.response?.bodyKind).toBe("json");
    expect(res.resolvedUrl).toBe("https://api.staging.example.com/v1/users?limit=2");
    expect(res.response?.cookies[0]?.httpOnly).toBe(true);
    const buf = await call("get_response_body", { responseId: res.response!.responseId });
    expect(buf).toBeInstanceOf(ArrayBuffer);
    expect(JSON.parse(new TextDecoder().decode(buf)).data[0].name).toBe("Ada Lovelace");
    const limited = await call("get_response_body", { responseId: res.response!.responseId, limit: 10 });
    expect(limited.byteLength).toBe(10);
  });

  it("reports unresolved variables and keeps secrets out of resolvedUrl/history", async () => {
    const res = await call("send_request", { executionId: "e2", request: req({ url: "https://x.dev/a?t={{token}}&m={{missing}}" }), projectId: server.projects[0].id });
    expect(res.unresolvedVariables).toEqual(["missing"]);
    expect(res.resolvedUrl).toContain("t={{token}}");
    const hist = await call("list_history", {});
    expect(hist[0].id).toBe(res.historyId);
    expect(hist[0].resolvedUrl).not.toContain("stg_");
  });

  it("validation errors carry details", async () => {
    const e1 = await rejects(call("send_request", { executionId: "v1", request: req({ url: "" }) }));
    expect(e1).toMatchObject({ code: "validation", details: { kind: "emptyUrl" } });
    const e2 = await rejects(call("send_request", { executionId: "v2", request: req({ url: "https://x.dev", method: "bad" }) }));
    expect(e2).toMatchObject({ code: "validation", details: { kind: "invalidMethod" } });
    const e3 = await rejects(call("send_request", { executionId: "v3", request: req({ url: "https://x.dev", method: "POST", body: { type: "graphql", graphql: { query: "{a}", variables: "{bad" } } }) }));
    expect(e3).toMatchObject({ code: "validation", details: { kind: "invalidGraphqlVariables" } });
  });

  it("typed network errors and cancel", async () => {
    const dns = await call("send_request", { executionId: "d", request: req({ url: "https://bad-host.invalid/" }) });
    expect(dns.response).toBeNull();
    expect(dns.error?.kind).toBe("dns");
    const slow = new MockServer({ latency: 1 });
    const c = makeCall((cmd, args) => slow.invoke(cmd, args));
    const p = c("send_request", { executionId: "slow", request: req({ url: "https://api.dev/delay/5" }) });
    await new Promise((r) => setTimeout(r, 20));
    expect(await c("cancel_request", { executionId: "slow" })).toBe(true);
    const res = await p;
    expect(res.error?.kind).toBe("cancelled");
    expect((await c("list_history", {}))[0].errorKind).toBe("cancelled");
    expect(await c("cancel_request", { executionId: "slow" })).toBe(false);
  });

  it("QUERY responses advertise Accept-Query", async () => {
    const res = await call("send_request", { executionId: "q", request: req({ method: "QUERY", url: "https://api.dev/v1/users/search", body: { type: "json", text: "{}" } }) });
    expect(res.response?.acceptQuery).toBe("application/json");
  });

  it("history entries reopen with a response body", async () => {
    const [first] = await call("list_history", {});
    const opened = await call("open_history_entry", { historyId: first.id });
    expect(opened.entry.response?.body ?? "").toBe("");
    expect(opened.response?.responseId).toBe(`history:${first.id}`);
    expect(opened.response?.httpVersion).toBe("");
    const body = await call("get_response_body", { responseId: opened.response!.responseId });
    expect(body.byteLength).toBeGreaterThan(0);
    await call("delete_history_entry", { historyId: first.id });
    expect((await rejects(call("open_history_entry", { historyId: first.id }))).code).toBe("not_found");
    await call("clear_history", {});
    expect(await call("list_history", {})).toEqual([]);
  });
});

describe("format_body / url helpers", () => {
  it("formats JSON and reports positions", async () => {
    expect(await call("format_body", { kind: "json", text: '{"a":[1,2]}' })).toBe('{\n  "a": [\n    1,\n    2\n  ]\n}');
    const e = await rejects(call("format_body", { kind: "json", text: '{\n"a": }' }));
    expect(e.code).toBe("format_error");
    expect((e.details as { line: number }).line).toBe(2);
    expect(await call("format_body", { kind: "xml", text: "<a><b>x</b></a>" })).toBe("<a>\n  <b>x</b>\n</a>");
    expect((await rejects(call("format_body", { kind: "xml", text: "<a><b></a>" }))).code).toBe("format_error");
  });
  it("url_to_params / params_to_url", async () => {
    const rows = await call("url_to_params", { url: "https://x?a=1&b", params: [] });
    expect(rows).toEqual([
      { key: "a", value: "1", enabled: true },
      { key: "b", value: "", enabled: true },
    ]);
    expect(await call("params_to_url", { url: "https://x?z=0", params: rows })).toBe("https://x?a=1&b");
  });
});

describe("cURL import/export (mock parity with AC-CURL)", () => {
  it("rejects non-curl input", async () => {
    expect((await rejects(call("import_curl", { text: "wget x" }))).code).toBe("not_curl");
    expect((await rejects(call("import_curl", { text: "curl -X POST" }))).code).toBe("curl_missing_url");
    expect((await rejects(call("import_curl", { text: "curl 'https://x" }))).code).toBe("curl_syntax");
    expect(await call("looks_like_curl", { text: "  curl https://x" })).toBe(true);
  });
  it("multi-line, quoting, headers, data, auth", async () => {
    const text = "curl 'https://api.dev/users?x=1' \\\r\n  -H 'Accept: application/json' -H 'X-Empty;' -H \"X-Q: \\\"q\\\"\" \\\n  -u ada:pw -d 'a=1' -d 'b=2' -sSLk --bogus";
    const { request: r, unknownFlags } = await call("import_curl", { text });
    expect(r.method).toBe("POST");
    expect(r.url).toBe("https://api.dev/users?x=1");
    expect(r.params).toEqual([{ key: "x", value: "1", enabled: true }]);
    expect(r.headers).toEqual([
      { key: "Accept", value: "application/json", enabled: true },
      { key: "X-Empty", value: "", enabled: true },
      { key: "X-Q", value: '"q"', enabled: true },
    ]);
    expect(r.auth).toEqual({ type: "basic", username: "ada", password: "pw" });
    expect(r.body.type).toBe("formEncoded");
    expect(r.settings.verifyTls).toBe(false);
    expect(unknownFlags).toEqual(["--bogus"]);
    expect(r.collectionId).toBe("");
  });
  it("-X QUERY with JSON body; $'…' quoting; -G; --json; -F", async () => {
    const q = (await call("import_curl", { text: `curl -X QUERY https://a.dev/s -H 'Content-Type: application/json' --data-raw $'{"a":"it\\'s"}'` })).request;
    expect(q.method).toBe("QUERY");
    expect(q.body).toEqual({ type: "json", text: `{"a":"it's"}` });
    const g = (await call("import_curl", { text: "curl -G https://a.dev/s -d q=1 -d r=2" })).request;
    expect(g.method).toBe("GET");
    expect(g.url).toBe("https://a.dev/s?q=1&r=2");
    const j = (await call("import_curl", { text: `curl https://a.dev --json '{"x":1}'` })).request;
    expect(j.method).toBe("POST");
    expect(j.headers.map((h) => h.key)).toEqual(["Content-Type", "Accept"]);
    const f = (await call("import_curl", { text: "curl https://a.dev -F name=Ada -F 'pic=@/tmp/a.png;type=image/png'" })).request;
    expect(f.body.form).toEqual([
      { key: "name", value: "Ada", kind: "text", enabled: true },
      { key: "pic", value: "/tmp/a.png", kind: "file", enabled: true, contentType: "image/png" },
    ]);
  });
  it("export round-trips method, headers and body", async () => {
    const r = scratchRequest({
      method: "QUERY",
      url: "https://a.dev/s",
      headers: [{ key: "X-It", value: "it's", enabled: true }, { key: "Off", value: "x", enabled: false }],
      body: { type: "json", text: '{"a":1}' },
    });
    const out = await call("export_curl", { request: r, keepVariables: false });
    expect(out.split(" \\\n  ")[0]).toBe("curl");
    expect(out).toContain("-X QUERY");
    expect(out).toContain("'X-It: it'\\''s'");
    expect(out).not.toContain("Off");
    const back = (await call("import_curl", { text: out })).request;
    expect(back.method).toBe("QUERY");
    expect(back.body).toEqual({ type: "json", text: '{"a":1}' });
    expect(back.headers.find((h) => h.key === "X-It")?.value).toBe("it's");
  });
  it("resolves variables unless keepVariables", async () => {
    const r = scratchRequest({ url: "{{baseUrl}}/v1", headers: [{ key: "Authorization", value: "Bearer {{token}}", enabled: true }] });
    const pid = server.projects[0].id;
    expect(await call("export_curl", { request: r, projectId: pid, keepVariables: false })).toContain("https://api.staging.example.com/v1");
    expect(await call("export_curl", { request: r, projectId: pid, keepVariables: false })).toContain("Bearer stg_9f8e7d6c5b4a");
    const raw = await call("export_curl", { request: r, projectId: pid, keepVariables: true });
    expect(raw).toContain("{{baseUrl}}/v1");
    expect(raw).toContain("'{{baseUrl}}/v1' \\\n  --globoff");
  });
});

describe("OpenAPI", () => {
  const yaml = `openapi: 3.2.0
info:
  title: Pets API
servers:
  - url: https://{env}.pets.dev/v1
    variables:
      env:
        default: api
components:
  securitySchemes:
    bearer:
      type: http
      scheme: bearer
security:
  - bearer: []
paths:
  /pets/{petId}:
    get:
      tags: [pets]
      summary: Get pet
      parameters:
        - name: petId
          in: path
          required: true
        - name: expand
          in: query
          example: owner
  /pets/search:
    query:
      tags: [pets]
      operationId: searchPets
      requestBody:
        content:
          application/json:
            schema:
              type: object
              properties:
                species:
                  type: string
  /health:
    get:
      responses: {}
`;
  it("previews YAML 3.2 with QUERY operations", async () => {
    const p = await call("preview_openapi", { text: yaml });
    expect(p.spec.label).toBe("OpenAPI 3.2 · YAML");
    expect(p.report.collectionName).toBe("Pets API");
    expect(p.report.operationsCreated).toBe(3);
    expect(p.report.folders).toEqual([{ name: "pets", requestCount: 2 }]);
    expect(p.report.rootRequestCount).toBe(1);
    expect(p.report.baseUrl).toBe("https://api.pets.dev/v1");
  });
  it("imports a collection + environment", async () => {
    const pid = server.projects[0].id;
    const res = await call("import_openapi", { projectId: pid, text: yaml });
    const reqs = res.tree.requests.filter((r) => r.collectionId === res.collectionId);
    const get = reqs.find((r) => r.name === "Get pet")!;
    expect(get.url).toBe("{{baseUrl}}/pets/{{petId}}");
    expect(get.params).toEqual([{ key: "expand", value: "owner", enabled: false }]);
    const q = reqs.find((r) => r.name === "searchPets")!;
    expect(q.method).toBe("QUERY");
    expect(q.body.type).toBe("json");
    expect(res.tree.collections.find((c) => c.id === res.collectionId)?.auth).toEqual({ type: "bearer", token: "{{token}}" });
    expect(res.environment.variables.map((v) => v.key)).toEqual(["baseUrl", "petId", "token"]);
  });
  it("rejects unsupported / invalid input without creating anything", async () => {
    const before = server.collections.length;
    expect((await rejects(call("preview_openapi", { text: '{"openapi":"4.0.0","paths":{}}' }))).code).toBe("openapi_unsupported");
    expect((await rejects(call("preview_openapi", { text: '{"hello":1}' }))).code).toBe("openapi_unsupported");
    expect((await rejects(call("preview_openapi", { text: "{not json" }))).code).toBe("openapi_parse");
    expect((await rejects(call("import_openapi", { projectId: server.projects[0].id, text: '{"swagger":"2.0"}' }))).code).toBe("openapi_invalid");
    expect(server.collections.length).toBe(before);
  });
});

describe("sync & sharing", () => {
  it("share requires sync; preview works offline", async () => {
    const col = server.collections[0];
    const pv = await call("share_preview", { kind: "collection", entityId: col.id, includeCredentials: false, environmentId: server.environments[1].id });
    expect(pv.secretVariablesRemoved).toBe(1);
    expect(pv.credentialsReplaced).toBeGreaterThan(0);
    expect((await rejects(call("share_create", { kind: "collection", entityId: col.id, includeCredentials: false }))).code).toBe("sync_not_connected");
    expect((await rejects(call("sync_workspace_info", {}))).code).toBe("sync_not_connected");
  });

  it("connect, share, preview/import with warnings, revoke", async () => {
    expect((await rejects(call("sync_connect", { workerUrl: "https://w.dev", token: "nope" }))).code).toBe("sync_unauthorized");
    const created = await call("sync_create_workspace", { workerUrl: "https://w.dev/" });
    expect(created.token.startsWith("rh_")).toBe(true);
    const st = await call("get_sync_status", {});
    expect(st).toMatchObject({ connected: true, state: "idle", workerUrl: "https://w.dev", warning: null });
    const info = await call("sync_workspace_info", {});
    expect(info.quotaBytes).toBe(50 * 1024 * 1024);
    expect(info.bytesUsed).toBeGreaterThan(0);

    const receipt = server.requests.find((r) => r.name === "Upload receipt")!;
    const share = await call("share_create", { kind: "request", entityId: receipt.id, includeCredentials: false });
    expect(share.url).toBe(`https://w.dev/s/${share.shareId}`);
    expect((await call("share_list", {})).map((s) => s.shareId)).toEqual([share.shareId]);
    const prev = await call("preview_share", { url: share.url });
    expect(prev.kind).toBe("request");
    expect(prev.warnings.length).toBe(1);
    const imported = await call("import_share", { url: share.url, projectId: server.projects[1].id });
    expect(imported.warnings.length).toBe(1);
    const copy = imported.tree.requests.find((r) => r.name === "Upload receipt")!;
    expect(copy.id).not.toBe(receipt.id);
    expect(copy.body.form?.find((f) => f.kind === "file")).toMatchObject({ value: "", enabled: false });
    await call("share_revoke", { shareId: share.shareId });
    expect((await rejects(call("preview_share", { url: share.url }))).code).toBe("share_gone");
  });

  it("too-large requests are reported in skippedEntities / tooLarge", async () => {
    await call("sync_create_workspace", { workerUrl: "https://w.dev" });
    const r = server.requests[0];
    await call("save_request", { request: { ...r, body: { type: "text", text: "x".repeat(600 * 1024) } } });
    const st = await call("get_sync_status", {});
    expect(st.skippedEntities).toEqual([{ type: "request", id: r.id, name: r.name, reason: "too_large" }]);
    expect(st.warning).toContain("too large");
    const rep = await call("sync_now", {});
    expect(rep.tooLarge.map((s) => s.id)).toEqual([r.id]);
  });

  it("quota exceeded and rotate-token-with-details paths", async () => {
    await call("sync_create_workspace", { workerUrl: "https://full.example.dev" });
    expect((await rejects(call("sync_now", {}))).code).toBe("sync_quota_exceeded");
    await call("sync_disconnect", {});
    await call("sync_create_workspace", { workerUrl: "https://rotate-fail.example.dev" });
    const e = await rejects(call("sync_rotate_token", {}));
    expect((e.details as { token?: string }).token?.startsWith("rh_")).toBe(true);
  });

  it("emits sync://status events", async () => {
    const seen: string[] = [];
    await server.backend().listen("sync://status", (s) => seen.push(s.state));
    await call("sync_create_workspace", { workerUrl: "https://w.dev" });
    await call("sync_now", {});
    expect(seen).toContain("idle");
    expect(seen).toContain("syncing");
  });
});

describe("QA fixtures and round-2 parity", () => {
  const fixture = (f: string) => readFileSync(fileURLToPath(new URL(`../../../qa/fixtures/openapi/${f}`, import.meta.url)), "utf8");

  it("parses the OAI-style 3.1 YAML fixture (QA-R2-006)", async () => {
    const p = await call("preview_openapi", { text: fixture("webhooks_v31.yaml") });
    expect(p.spec.label).toBe("OpenAPI 3.1 · YAML");
    expect(p.report.collectionName).toBe("Webhook Example");
    expect(p.report.operationsCreated).toBe(2);
    expect(p.report.baseUrl).toBe("https://eu-west-1.api.example.com/v2");
    const res = await call("import_openapi", { projectId: server.projects[0].id, text: fixture("webhooks_v31.yaml") });
    const file = res.tree.requests.find((r) => r.name === "Fetch a pet file 🐶")!;
    expect(file.url).toBe("{{baseUrl}}/pets/{{petId}}/files/{{name}}.{{ext}}?since=2024-01-01T00:00:00Z");
    expect(file.params[0]).toMatchObject({ key: "since", value: "2024-01-01T00:00:00Z", enabled: true });
  });

  it("parses the 3.0 YAML and Swagger 2.0 fixtures", async () => {
    const a = await call("preview_openapi", { text: fixture("petstore3_v30.yaml") });
    expect(a.spec.kind).toBe("openapi3");
    expect(a.report.operationsCreated).toBeGreaterThan(0);
    const b = await call("preview_openapi", { text: fixture("petstore_swagger2.json") });
    expect(b.spec.label).toBe("Swagger 2.0 · JSON");
  });

  it("save_settings accepts fractional u32 fields and rounds them like the real backend (QA-R3-003)", async () => {
    const s = await call("get_settings", {});
    expect((await call("save_settings", { settings: { ...s, sidebarWidth: 320.6 } })).sidebarWidth).toBe(321);
    expect((await call("save_settings", { settings: { ...s, sidebarWidth: 321, splitRatio: 0.437 } })).splitRatio).toBe(0.437);
  });

  it("share_update keeps kind/entityId/name populated (QA-R2-003)", async () => {
    await call("sync_create_workspace", { workerUrl: "https://w.dev" });
    const col = server.collections[0];
    const s = await call("share_create", { kind: "collection", entityId: col.id, includeCredentials: false });
    const u = await call("share_update", { shareId: s.shareId, kind: "collection", entityId: col.id, includeCredentials: true });
    expect(u).toMatchObject({ kind: "collection", entityId: col.id, name: col.name, url: s.url });
  });

  it("cURL export puts curl, -X, URL and each flag on its own line (AC-CEXP-2)", async () => {
    const out = await call("export_curl", { request: scratchRequest({ method: "DELETE", url: "https://a.dev/x" }), keepVariables: false });
    expect(out).toBe("curl \\\n  -X DELETE \\\n  'https://a.dev/x'");
  });
});

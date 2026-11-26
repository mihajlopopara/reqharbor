import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { call, callJson, newWorkspace, sampleCollectionSnapshot, sampleRequest } from "./helpers";
import { PAGE_CSS, PAGE_JS } from "../src/share-page";

async function createShare(token: string, snapshot: unknown = sampleCollectionSnapshot(), kind = "collection") {
  const { res, data } = await callJson("/v1/shares", {
    token,
    body: { kind, entityId: "01COL0000000000000000000001", snapshot },
  });
  expect(res.status).toBe(201);
  return data as { shareId: string; url: string };
}

const html = async (path: string) => {
  const res = await call(path);
  return { res, text: await res.text() };
};

describe("shares", () => {
  it("AC-SHARE-1: uploading a snapshot returns an unguessable share id and URL", async () => {
    const { token } = await newWorkspace();
    const a = await createShare(token);
    const b = await createShare(token);
    expect(a.shareId).toMatch(/^[0-9A-Za-z]{22}$/); // 128 bits, base62
    expect(a.shareId).not.toBe(b.shareId);
    expect(a.url).toBe(`https://worker.test/s/${a.shareId}`);
  });

  it("AC-SHARE-1: sharing requires a workspace token", async () => {
    const res = await call("/v1/shares", { body: { kind: "request", entityId: "x", snapshot: { schemaVersion: 1, request: {} } } });
    expect(res.status).toBe(401);
  });

  it("AC-SHARE-1: snapshot is validated (kind, entityId, schemaVersion, root entity)", async () => {
    const { token } = await newWorkspace();
    const bad = [
      { kind: "folder", entityId: "x", snapshot: { schemaVersion: 1, collection: {} } },
      { kind: "request", entityId: "", snapshot: { schemaVersion: 1, request: {} } },
      { kind: "request", entityId: "x", snapshot: { request: {} } },
      { kind: "request", entityId: "x", snapshot: { schemaVersion: 1, collection: {} } },
      { kind: "collection", entityId: "x", snapshot: { schemaVersion: 1, collection: { requests: "no" } } },
      { kind: "collection", entityId: "x", snapshot: { schemaVersion: 1, collection: {}, environment: [] } },
      { kind: "collection", entityId: "x", snapshot: "nope" },
    ];
    for (const body of bad) {
      const { res, data } = await callJson("/v1/shares", { token, body });
      expect(res.status, JSON.stringify(body)).toBe(400);
      expect(data.error.code).toBe("validation_failed");
    }
  });

  it("AC-SHARE-2 / AC-ENV-7: secret variables are always removed from shared environments", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    const { data } = await callJson(`/s/${shareId}.json`);
    expect(data.snapshot.environment.variables).toEqual([
      { key: "baseUrl", value: "https://staging.example.com", enabled: true, secret: false },
    ]);
    const stored = await env.DB.prepare("SELECT snapshot FROM shares WHERE id = ?").bind(shareId).first<{ snapshot: string }>();
    expect(stored?.snapshot).not.toContain("super-secret-token");
    const page = await html(`/s/${shareId}`);
    expect(page.text).not.toContain("super-secret-token");
  });

  it("AC-SHARE-3: read-only HTML page with tree, method badges, URLs, headers, bodies and Copy as cURL", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    const { res, text } = await html(`/s/${shareId}`);
    expect(res.status).toBe(200);
    expect(res.headers.get("Content-Type")).toBe("text/html; charset=utf-8");
    // tree: collection > Admin > Danger zone
    expect(text).toContain('<nav class="tree"');
    expect(text).toMatch(/Admin[\s\S]*Danger zone/);
    expect(text).toContain('Users API <span aria-hidden="true">›</span> Admin <span aria-hidden="true">›</span> Danger zone');
    // badges, URLs, headers, bodies
    expect(text).toContain('<span class="m m-get">GET</span>');
    expect(text).toContain('<span class="m m-delete">DELETE</span>');
    expect(text).toContain('<span class="var">{{baseUrl}}</span>/users?page=1');
    expect(text).toContain("<td>Accept</td><td>application/json</td>");
    expect(text).toContain('<tr class="off"><td>X-Debug</td>');
    expect(text).toContain("&quot;q&quot;: &quot;ada&quot;"); // pretty JSON body, escaped
    // one Copy-as-cURL button per request, with a pre-rendered command
    expect(text.match(/data-copy="r\d+"/g)).toHaveLength(3);
    // Same format as the app's exporter: URL rebuilt from params, every part on its own line.
    expect(text).toContain("curl \\\n  -X QUERY \\\n  &#39;https://staging.example.com/users/search?page=1&#39; \\\n  -H &#39;Accept: application/json&#39;");
    // environment attached → raw variant for the "Keep {{variables}}" toggle
    expect(text).toContain("&#39;{{baseUrl}}/users/search?page=1&#39; \\\n  --globoff");
    expect(text).toContain("Authorization: Bearer {{token}}"); // inherited auth; secret token never resolved
    // read-only: no forms or editable inputs other than the variables toggle
    expect(text).not.toMatch(/<form|<textarea|contenteditable/i);
    expect(text.match(/<input/g)).toHaveLength(1);
  });

  it("AC-QRY-1: QUERY gets the teal badge in the share view", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    const { text } = await html(`/s/${shareId}`);
    expect(text).toContain('<span class="m m-query">QUERY</span>');
    expect(PAGE_CSS).toContain("--method-query:#2fc9c0");
    expect(PAGE_CSS).toContain(".m-query{color:var(--method-query)}");
  });

  it("AC-SHARE-3: requests the core cannot export show the error instead of a Copy button", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token, { schemaVersion: 1, request: sampleRequest({ name: "Bad", method: "get" }) }, "request");
    const { res, text } = await html(`/s/${shareId}`);
    expect(res.status).toBe(200);
    expect(text).toContain("Can't export as cURL: Invalid method &quot;get&quot;");
    expect(text).not.toContain("data-copy=");
  });

  it("AC-SHARE-3: a single shared request renders without the tree", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token, { schemaVersion: 1, request: sampleRequest({ name: "Solo" }) }, "request");
    const { res, text } = await html(`/s/${shareId}`);
    expect(res.status).toBe(200);
    expect(text).toContain("<h1>Solo</h1>");
    expect(text).not.toContain('<nav class="tree"');
    expect(text.match(/data-copy=/g)).toHaveLength(1);
  });

  it("AC-SHARE-3: share pages send noindex, strict CSP, no cookies, and public max-age=60", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    for (const path of [`/s/${shareId}`, `/s/${shareId}.json`]) {
      const res = await call(path);
      expect(res.headers.get("X-Robots-Tag")).toContain("noindex");
      expect(res.headers.get("Cache-Control")).toBe("public, max-age=60");
      expect(res.headers.get("Set-Cookie")).toBeNull();
      expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
    }
    const csp = (await call(`/s/${shareId}`)).headers.get("Content-Security-Policy")!;
    expect(csp).toContain("default-src 'none'");
    expect(csp).toContain("frame-ancestors 'none'");
    expect(csp).toMatch(/script-src 'sha256-[A-Za-z0-9+/]+=*'/);
    expect(csp).toMatch(/style-src 'sha256-[A-Za-z0-9+/]+=*'/);
    expect(csp).not.toContain("unsafe-inline");
    expect(csp).not.toContain("unsafe-eval");

    // The hashes match exactly the inline script and style in the page.
    const b64 = async (s: string) => {
      const d = new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s)));
      return btoa(String.fromCharCode(...d));
    };
    expect(csp).toContain(`'sha256-${await b64(PAGE_JS)}'`);
    expect(csp).toContain(`'sha256-${await b64(PAGE_CSS)}'`);
    const page = await (await call(`/s/${shareId}`)).text();
    expect(page.match(/<script>/g)).toHaveLength(1);
    expect(page).toContain(`<script>${PAGE_JS}</script>`);
    expect(page).toContain(`<style>${PAGE_CSS}</style>`);
    // No external resources at all.
    expect(page).not.toMatch(/<(link|img|iframe)\b|src=|https?:\/\/(?!staging\.example\.com|api\.example\.com)/);
  });

  it("XSS: every piece of user content is HTML-escaped on the share page", async () => {
    const { token } = await newWorkspace();
    const x = `<script>alert(1)</script>"><img src=x onerror=alert(2)>'`;
    const snapshot = {
      schemaVersion: 1,
      collection: {
        name: x,
        description: x,
        auth: { type: "basic", username: x, password: x },
        folders: [{ id: "f", name: x, parentFolderId: null }],
        requests: [
          {
            id: "r",
            folderId: "f",
            name: x,
            method: x,
            url: `javascript:alert(3)//${x}`,
            params: [{ key: x, value: x, enabled: true }],
            headers: [{ key: x, value: x, enabled: true }],
            auth: { type: "inherit" },
            body: { type: "json", text: x },
          },
          {
            id: "r2",
            name: "form",
            method: "POST",
            url: "https://a.test",
            headers: [],
            auth: { type: "apikey", apiKey: { key: x, value: x, in: "header" } },
            body: { type: "form", form: [{ key: x, value: x, kind: "file", enabled: true }] },
          },
          {
            id: "r3",
            name: "gql",
            method: "QUERY",
            url: "https://a.test",
            body: { type: "graphql", graphql: { query: x, variables: x } },
          },
        ],
      },
      environment: { name: x, variables: [{ key: x, value: x, enabled: true, secret: false }] },
    };
    const { shareId } = await createShare(token, snapshot);
    const { res, text } = await html(`/s/${shareId}`);
    expect(res.status).toBe(200);
    expect(text).not.toContain("<script>alert");
    expect(text).not.toContain("<img");
    // No injected attribute: drop quoted attribute values (user text inside them is escaped), then scan tags.
    expect(text.replace(/"[^"]*"/g, '""')).not.toMatch(/<[^>]*\sonerror=/i);
    expect(text).not.toMatch(/href="javascript:/i);
    expect(text).toContain("&lt;script&gt;alert(1)&lt;/script&gt;&quot;&gt;&lt;img src=x onerror=alert(2)&gt;&#39;");
    expect(text).toContain(`<title>&lt;script&gt;`);
    // The only <script> element is the static, hash-allowed one; all hrefs are internal anchors.
    expect(text.match(/<script\b/g)).toHaveLength(1);
    for (const m of text.matchAll(/href="([^"]*)"/g)) expect(m[1]).toMatch(/^#r\d+$/);
    // Custom-method tooltip attribute is escaped too.
    expect(text).toContain('title="&lt;script&gt;');
  });

  it("XSS: malformed snapshot shapes never break rendering", async () => {
    const { token } = await newWorkspace();
    const snapshot = {
      schemaVersion: 1,
      collection: {
        name: 42,
        folders: [{ id: "a", parentFolderId: "b" }, { id: "b", parentFolderId: "a" }, "junk"],
        requests: [null, { headers: "x", params: [1, null], body: { type: "nope" }, auth: 5 }],
      },
    };
    const { shareId } = await createShare(token, snapshot);
    const { res, text } = await html(`/s/${shareId}`);
    expect(res.status).toBe(200);
    expect(text).toContain("Untitled request");
  });

  it("AC-SHARE-4: Update share replaces the snapshot behind the same URL", async () => {
    const { token } = await newWorkspace();
    const created = await createShare(token);
    const next = sampleCollectionSnapshot();
    next.collection.name = "Users API v2";
    const upd = await callJson(`/v1/shares/${created.shareId}`, { token, method: "PUT", body: { snapshot: next } });
    expect(upd.res.status).toBe(200);
    expect(upd.data.url).toBe(created.url);
    expect(upd.data.updatedAt).toBeGreaterThanOrEqual(upd.data.createdAt);
    const { data } = await callJson(`/s/${created.shareId}.json`);
    expect(data.snapshot.collection.name).toBe("Users API v2");
    expect((await html(`/s/${created.shareId}`)).text).toContain("<h1>Users API v2</h1>");
  });

  it("AC-SHARE-4: Revoke makes the URL return 410 Gone (HTML and JSON) and discards the snapshot", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    const del = await callJson(`/v1/shares/${shareId}`, { token, method: "DELETE" });
    expect(del.res.status).toBe(200);
    expect(del.data).toEqual({ shareId, revokedAt: expect.any(Number) });

    const page = await html(`/s/${shareId}`);
    expect(page.res.status).toBe(410);
    expect(page.text).toContain("This link has been revoked");
    expect(page.text).not.toContain("Users API");
    const j = await callJson(`/s/${shareId}.json`);
    expect(j.res.status).toBe(410);
    expect(j.data.error.code).toBe("gone");

    const stored = await env.DB.prepare("SELECT snapshot FROM shares WHERE id = ?").bind(shareId).first<{ snapshot: string | null }>();
    expect(stored?.snapshot).toBeNull();
    // Updating a revoked share is also 410; revoking again is idempotent.
    const upd = await call(`/v1/shares/${shareId}`, { token, method: "PUT", body: { snapshot: sampleCollectionSnapshot() } });
    expect(upd.status).toBe(410);
    expect((await call(`/v1/shares/${shareId}`, { token, method: "DELETE" })).status).toBe(200);
  });

  it("AC-SHARE-4: other workspaces cannot update or revoke a share", async () => {
    const owner = await newWorkspace();
    const other = await newWorkspace();
    const { shareId } = await createShare(owner.token);
    expect((await call(`/v1/shares/${shareId}`, { token: other.token, method: "DELETE" })).status).toBe(404);
    expect(
      (await call(`/v1/shares/${shareId}`, { token: other.token, method: "PUT", body: { snapshot: sampleCollectionSnapshot() } })).status,
    ).toBe(404);
    expect((await call(`/s/${shareId}`)).status).toBe(200);
  });

  it("AC-SHARE-5: /s/:id.json returns the snapshot for Import from link (CORS-readable)", async () => {
    const { token } = await newWorkspace();
    const { shareId } = await createShare(token);
    const { res, data } = await callJson(`/s/${shareId}.json`, { headers: { Origin: "tauri://localhost" } });
    expect(res.status).toBe(200);
    expect(res.headers.get("Access-Control-Allow-Origin")).toBe("*");
    expect(data).toEqual({
      shareId,
      kind: "collection",
      createdAt: expect.any(Number),
      updatedAt: expect.any(Number),
      snapshot: expect.objectContaining({ schemaVersion: 1 }),
    });
    expect(data.snapshot.collection.requests).toHaveLength(3);
    const pre = await call(`/s/${shareId}.json`, { method: "OPTIONS" });
    expect(pre.status).toBe(204);
    expect(pre.headers.get("Access-Control-Allow-Origin")).toBe("*");
  });

  it("AC-SHARE-6: GET /v1/shares lists active shares with name, created and updated time", async () => {
    const { token } = await newWorkspace();
    const a = await createShare(token);
    const b = await createShare(token, { schemaVersion: 1, request: sampleRequest({ name: "Solo" }) }, "request");
    const c = await createShare(token);
    await call(`/v1/shares/${c.shareId}`, { token, method: "DELETE" });
    const { res, data } = await callJson("/v1/shares", { token });
    expect(res.status).toBe(200);
    expect(data.shares.map((s: any) => s.shareId).sort()).toEqual([a.shareId, b.shareId].sort());
    const solo = data.shares.find((s: any) => s.shareId === b.shareId);
    expect(solo).toEqual({
      shareId: b.shareId,
      url: b.url,
      kind: "request",
      entityId: "01COL0000000000000000000001",
      name: "Solo",
      createdAt: expect.any(Number),
      updatedAt: expect.any(Number),
    });
    expect(data.shares.find((s: any) => s.shareId === a.shareId).name).toBe("Users API");
    // Other workspaces see none of them.
    const other = await newWorkspace();
    expect((await callJson("/v1/shares", { token: other.token })).data.shares).toEqual([]);
  });

  it("QA-R1-024: share snapshots count toward the workspace quota; revoking frees space", async () => {
    const { token } = await newWorkspace();
    const small = await createShare(token);
    const big = sampleCollectionSnapshot();
    big.collection.description = "x".repeat(1000 * 1024);
    const bigIds: string[] = [];
    let last: Response | undefined;
    for (let i = 0; i < 60; i++) {
      last = await call("/v1/shares", { token, body: { kind: "collection", entityId: `C${i}`, snapshot: big } });
      if (last.status !== 201) break;
      bigIds.push(((await last.json()) as { shareId: string }).shareId);
    }
    expect(last!.status).toBe(413);
    expect(((await last!.json()) as any).error.code).toBe("quota_exceeded");
    expect(bigIds.length).toBeGreaterThanOrEqual(45);
    expect(bigIds.length).toBeLessThan(52);
    // Growing an existing share past the quota is refused too.
    const grow = await call(`/v1/shares/${small.shareId}`, { token, method: "PUT", body: { snapshot: big } });
    expect(grow.status).toBe(413);
    // Revoking frees its space.
    await call(`/v1/shares/${bigIds[0]}`, { token, method: "DELETE" });
    expect((await call("/v1/shares", { token, body: { kind: "collection", entityId: "again", snapshot: big } })).status).toBe(201);
  });

  it("unknown or malformed share ids return 404", async () => {
    for (const path of ["/s/AAAAAAAAAAAAAAAAAAAAAA", "/s/short", "/s/AAAAAAAAAAAAAAAAAAAAAA.json"]) {
      const res = await call(path);
      expect(res.status).toBe(404);
      expect(res.headers.get("X-Robots-Tag")).toContain("noindex");
    }
  });

  it("snapshots over 1 MB are rejected with 413", async () => {
    const { token } = await newWorkspace();
    const snapshot = { schemaVersion: 1, request: sampleRequest({ body: { type: "text", text: "x".repeat(1024 * 1024) } }) };
    const { res, data } = await callJson("/v1/shares", { token, body: { kind: "request", entityId: "x", snapshot } });
    expect(res.status).toBe(413);
    expect(data.error.code).toBe("snapshot_too_large");
  });
});

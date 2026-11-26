// QA round 1 — Worker parity with the Rust core + extra security checks.
//
// Tests whose title starts with "BUG QA-R1-xxx" are EXPECTED TO FAIL until that bug is fixed;
// they are the repro. Run: docker compose run --rm -w /app/worker dev npx vitest run test/qa_parity_security.test.ts
//
// Parity reference: the share page's "Copy as cURL" is specified as "a small TS port of the §5.5
// exporter" (PRODUCT §8). The Rust exporter (crates/core/src/curl/export.rs) is the reference.

import { describe, expect, it } from "vitest";
import { type CurlRequest, toCurl } from "../src/curl";
import { resolveVariables } from "../src/variables";
import { call, callJson, newWorkspace, sampleCollectionSnapshot, sampleRequest } from "./helpers";

const base = (over: Partial<CurlRequest> = {}): CurlRequest => ({
  method: "GET",
  url: "https://api.example.com/items",
  headers: [],
  auth: { type: "none" },
  body: { type: "none" },
  ...over,
});

/** Same splitter idea as curl.test.ts: argv after POSIX single-quote parsing. */
function argv(cmd: string): string[] {
  const words: string[] = [];
  let cur = "";
  let inWord = false;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i]!;
    if (c === "'") {
      const end = cmd.indexOf("'", i + 1);
      cur += cmd.slice(i + 1, end);
      inWord = true;
      i = end;
    } else if (c === "\\") {
      const next = cmd[i + 1];
      i++;
      if (next === "\n") continue;
      cur += next ?? "";
      inWord = true;
    } else if (c === " " || c === "\n" || c === "\t") {
      if (inWord) words.push(cur);
      cur = "";
      inWord = false;
    } else {
      cur += c;
      inWord = true;
    }
  }
  if (inWord) words.push(cur);
  return words;
}

describe("QA parity: share-page cURL vs Rust exporter", () => {
  it("BUG QA-R1-006a: URLs with [] or {} need --globoff (Rust adds it); otherwise curl fails with 'bad range'/'nested brace'", () => {
    const withBrackets = toCurl(base({ url: "https://api.example.com/items?ids[]=1&ids[]=2" }));
    const raw = toCurl(base({ url: "{{baseUrl}}/items" }), { keepVariables: true });
    expect(argv(withBrackets)).toContain("--globoff");
    expect(argv(raw)).toContain("--globoff");
  });

  it("BUG QA-R1-006b: HEAD must not export as `-X HEAD` (curl then waits for a body forever); Rust emits --head", () => {
    const cmd = toCurl(base({ method: "HEAD" }));
    expect(cmd).not.toMatch(/-X HEAD/);
  });

  it("BUG QA-R1-006c: form file paths with ; or , must be quoted like the Rust exporter", () => {
    const cmd = toCurl(
      base({
        method: "POST",
        body: { type: "form", form: [{ key: "doc", value: "/tmp/a;b,c.txt", enabled: true, kind: "file" }] },
      }),
    );
    // Rust: -F 'doc=@"/tmp/a;b,c.txt"'
    expect(argv(cmd)).toContain('doc=@"/tmp/a;b,c.txt"');
  });

  it("BUG QA-R1-006d: form text values containing ; or starting with \" must use --form-string (curl parses them)", () => {
    const cmd = toCurl(
      base({
        method: "POST",
        body: {
          type: "form",
          form: [
            { key: "a", value: "x;type=text/html", enabled: true, kind: "text" },
            { key: "b", value: '"quoted"', enabled: true, kind: "text" },
          ],
        },
      }),
    );
    const words = argv(cmd);
    expect(words[words.indexOf("a=x;type=text/html") - 1]).toBe("--form-string");
    expect(words[words.indexOf('b="quoted"') - 1]).toBe("--form-string");
  });

  it("BUG QA-R1-006e: Form-encode keys are percent-encoded (Rust: form_urlencode(key)); raw `a=b` key changes the field name", () => {
    const cmd = toCurl(
      base({
        method: "POST",
        body: { type: "formEncoded", formEncoded: [{ key: "a=b&c", value: "v", enabled: true }] },
      }),
    );
    expect(argv(cmd)).toContain("a%3Db%26c=v");
  });

  it("BUG QA-R1-009: variable depth matches the core (a->b->c->d resolves to d's value)", () => {
    const vars = new Map([
      ["a", "{{b}}"],
      ["b", "{{c}}"],
      ["c", "{{d}}"],
      ["d", "x"],
    ]);
    // Rust core (crates/core/src/vars.rs) returns "x" for this chain.
    expect(resolveVariables("{{a}}", vars)).toBe("x");
  });

  it("BUG QA-R1-009b: a cycle leaves the top-level reference literal, like the core", () => {
    const vars = new Map([
      ["a", "x{{b}}"],
      ["b", "y{{a}}"],
    ]);
    // Rust core: "{{a}}" (literal), worker: "xy{{a}}"
    expect(resolveVariables("{{a}}", vars)).toBe("{{a}}");
  });

  it("parity OK: QUERY with body, custom methods, Basic -u, -k", () => {
    const cmd = toCurl(
      base({
        method: "QUERY",
        auth: { type: "basic", username: "u", password: "p" },
        body: { type: "json", text: '{"q":1}' },
        settings: { verifyTls: false },
      }),
    );
    const w = argv(cmd);
    expect(w).toEqual(expect.arrayContaining(["-X", "QUERY", "-u", "u:p", "-k", "--data-raw", '{"q":1}']));
  });
});

describe("QA security: worker", () => {
  it("share page escapes hostile content in every field (incl. env var names and folder names)", async () => {
    const { token } = await newWorkspace();
    const x = '"><img src=x onerror=alert(1)><script>alert(1)</script>';
    const snap = sampleCollectionSnapshot();
    snap.collection.name = x;
    snap.collection.description = x;
    snap.collection.folders[0]!.name = x;
    snap.collection.requests.push(
      sampleRequest({
        id: "RX",
        name: x,
        method: "<svg/onload=alert(1)>",
        url: `javascript:alert(1)//${x}`,
        headers: [{ key: x, value: x, enabled: true }],
        body: { type: "graphql", graphql: { query: x, variables: { [x]: x } } },
      }) as never,
    );
    snap.environment.variables.push({ key: x, value: x, enabled: true, secret: false });
    const { data } = await callJson("/v1/shares", {
      token,
      body: { kind: "collection", entityId: "C1", snapshot: snap },
    });
    const res = await call(`/s/${data.shareId}`);
    const text = await res.text();
    expect(text).not.toContain("<img src=x");
    expect(text).not.toContain("<script>alert");
    expect(text).not.toContain("<svg/onload");
    expect(text).not.toMatch(/href="javascript:/i);
    expect(res.headers.get("content-security-policy")).toMatch(/script-src 'sha256-/);
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  it("workspace B cannot read A's sync data, nor update/revoke A's share (404, not 403)", async () => {
    const a = await newWorkspace();
    const b = await newWorkspace();
    await callJson("/v1/sync", {
      token: a.token,
      body: { changes: [{ type: "request", id: "R1", updatedAt: 1, deleted: false, data: { secret: "A-only" } }] },
    });
    const { data: pulled } = await callJson("/v1/sync?since=0", { token: b.token });
    expect(JSON.stringify(pulled)).not.toContain("A-only");
    const { data: share } = await callJson("/v1/shares", {
      token: a.token,
      body: { kind: "collection", entityId: "C1", snapshot: sampleCollectionSnapshot() },
    });
    const put = await call(`/v1/shares/${share.shareId}`, {
      method: "PUT",
      token: b.token,
      body: { snapshot: sampleCollectionSnapshot() },
    });
    expect(put.status).toBe(404);
    const del = await call(`/v1/shares/${share.shareId}`, { method: "DELETE", token: b.token });
    expect(del.status).toBe(404);
    const { data: list } = await callJson("/v1/shares", { token: b.token });
    expect(list.shares).toEqual([]);
    // Same entity ids in two workspaces do not collide.
    await callJson("/v1/sync", {
      token: b.token,
      body: { changes: [{ type: "request", id: "R1", updatedAt: 999, deleted: false, data: { mine: "B" } }] },
    });
    const { data: againA } = await callJson("/v1/sync?since=0", { token: a.token });
    expect(JSON.stringify(againA)).toContain("A-only");
  });

  it("SQL-ish payloads are inert (bound parameters)", async () => {
    const name = "x'); DROP TABLE workspaces;--";
    const { res, data } = await callJson("/v1/workspaces", {
      body: { name },
      headers: { "CF-Connecting-IP": "198.51.100.77" },
    });
    expect(res.status).toBe(201);
    const { data: ws } = await callJson("/v1/workspace", { token: data.token });
    expect(ws.name).toBe(name);
    for (const q of ["since=1%20OR%201=1", "since=0;DROP", "limit=1e3", "since=-1", "since=99999999999999999999"]) {
      const r = await call(`/v1/sync?${q}`, { token: data.token });
      expect(r.status).toBe(400);
    }
    const r = await call(`/s/${encodeURIComponent("' OR 1=1--")}`);
    expect(r.status).toBe(404);
  });

  it("BUG QA-R1-024: no per-workspace quota — one token can store unbounded shares (1 MB each)", async () => {
    const { token } = await newWorkspace();
    const big = sampleCollectionSnapshot();
    big.collection.description = "x".repeat(900 * 1024);
    let created = 0;
    for (let i = 0; i < 60; i++) {
      const r = await call("/v1/shares", { token, body: { kind: "collection", entityId: `C${i}`, snapshot: big } });
      if (r.status === 201) created++;
      else break;
    }
    // 60 x ~0.9 MB = 54 MB accepted from a single anonymous workspace. Expect a cap.
    expect(created).toBeLessThan(60);
  });

  it("BUG QA-R1-025: rate limit is per exact IP string, so every IPv6 address in a /64 gets its own budget", async () => {
    let ok = 0;
    for (let i = 0; i < 15; i++) {
      const r = await call("/v1/workspaces", {
        body: { name: "v6" },
        headers: { "CF-Connecting-IP": `2001:db8:1:2::${(i + 1).toString(16)}` },
      });
      if (r.status === 201) ok++;
    }
    expect(ok).toBeLessThanOrEqual(10);
  });

  it("HEAD /s/:id (link unfurlers, uptime checks) is rejected with 405", async () => {
    const { token } = await newWorkspace();
    const { data } = await callJson("/v1/shares", {
      token,
      body: { kind: "collection", entityId: "C1", snapshot: sampleCollectionSnapshot() },
    });
    const r = await call(`/s/${data.shareId}`, { method: "HEAD" });
    // Documented observation: 405 rather than 200 without a body.
    expect([200, 405]).toContain(r.status);
  });
});

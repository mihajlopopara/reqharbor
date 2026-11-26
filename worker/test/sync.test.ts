import { env } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { rateLimitBucket } from "../src/workspaces";
import { call, callJson, change, newWorkspace, randomIp } from "./helpers";

describe("workspaces & auth", () => {
  it("AC-SYNC-1: Create workspace returns {workspaceId, token} once; Connect with the token works", async () => {
    const { res, data } = await callJson("/v1/workspaces", {
      body: { name: "  My Workspace  " },
      headers: { "CF-Connecting-IP": randomIp() },
    });
    expect(res.status).toBe(201);
    expect(Object.keys(data).sort()).toEqual(["token", "workspaceId"]);
    expect(data.token).toMatch(/^rh_[A-Za-z0-9_-]{43}$/);
    expect(data.workspaceId).toMatch(/^[0-9A-HJKMNP-TV-Z]{26}$/);

    const me = await callJson("/v1/workspace", { token: data.token });
    expect(me.res.status).toBe(200);
    expect(me.data).toEqual({
      workspaceId: data.workspaceId,
      name: "My Workspace",
      createdAt: expect.any(Number),
      entityCount: 0,
      bytesUsed: 0,
      quotaBytes: 50 * 1024 * 1024,
    });
    // The token is never readable again through the API.
    expect(JSON.stringify(me.data)).not.toContain(data.token);
  });

  it("AC-SYNC-1: only SHA-256(token) is stored", async () => {
    const { workspaceId, token } = await newWorkspace();
    const row = await env.DB.prepare("SELECT token_hash FROM workspaces WHERE id = ?").bind(workspaceId).first<{ token_hash: string }>();
    const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(token));
    const hex = [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
    expect(row?.token_hash).toBe(hex);
    const dump = await env.DB.prepare("SELECT * FROM workspaces WHERE id = ?").bind(workspaceId).first();
    expect(JSON.stringify(dump)).not.toContain(token);
  });

  it("AC-SYNC-1: workspace name is validated", async () => {
    for (const body of [{}, { name: "" }, { name: "   " }, { name: 42 }, { name: "x".repeat(201) }]) {
      const { res, data } = await callJson("/v1/workspaces", { body, headers: { "CF-Connecting-IP": randomIp() } });
      expect(res.status).toBe(400);
      expect(data.error.code).toBe("validation_failed");
    }
  });

  it("AC-SYNC-1: POST /v1/workspaces is rate-limited to 10 per hour per IP", async () => {
    const ip = randomIp();
    for (let i = 0; i < 10; i++) {
      const res = await call("/v1/workspaces", { body: { name: `w${i}` }, headers: { "CF-Connecting-IP": ip } });
      expect(res.status).toBe(201);
    }
    const blocked = await callJson("/v1/workspaces", { body: { name: "w11" }, headers: { "CF-Connecting-IP": ip } });
    expect(blocked.res.status).toBe(429);
    expect(blocked.data.error.code).toBe("rate_limited");
    expect(Number(blocked.res.headers.get("Retry-After"))).toBeGreaterThan(0);
    // Another IP is unaffected.
    const other = await call("/v1/workspaces", { body: { name: "ok" }, headers: { "CF-Connecting-IP": randomIp() } });
    expect(other.status).toBe(201);
  });

  it("AC-SYNC-7: missing, malformed, and unknown tokens get 401 with a JSON error", async () => {
    const unknown = "rh_" + "A".repeat(43);
    for (const headers of [{} as Record<string, string>, { Authorization: "Bearer nope" }, { Authorization: `Basic ${unknown}` }, { Authorization: `Bearer ${unknown}` }]) {
      const { res, data } = await callJson("/v1/sync", { headers });
      expect(res.status).toBe(401);
      expect(res.headers.get("WWW-Authenticate")).toContain("Bearer");
      expect(data).toEqual({ error: { code: "unauthorized", message: expect.any(String) } });
    }
  });

  it("AC-SYNC-7: rotating the token invalidates the old one immediately; data is untouched", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", { token, body: { changes: [change("project", "p1", 1000)] } });
    const rotated = await callJson("/v1/workspace/token/rotate", { token, method: "POST" });
    expect(rotated.res.status).toBe(200);
    expect(rotated.data.token).toMatch(/^rh_/);
    expect(rotated.data.token).not.toBe(token);
    expect((await call("/v1/workspace", { token })).status).toBe(401);
    const me = await callJson("/v1/workspace", { token: rotated.data.token });
    expect(me.data.entityCount).toBe(1);
  });

  it("QA-R1-007 / contract C4: rotation is atomic — concurrent rotations with one token: exactly one wins", async () => {
    const { token } = await newWorkspace();
    const results = await Promise.all([1, 2, 3].map(() => call("/v1/workspace/token/rotate", { token, method: "POST" })));
    const ok = results.filter((r) => r.status === 200);
    expect(ok).toHaveLength(1);
    expect(results.filter((r) => r.status === 401)).toHaveLength(2);
    const { token: fresh } = (await ok[0]!.json()) as { token: string };
    expect((await call("/v1/sync", { token })).status).toBe(401);
    expect((await call("/v1/sync", { token: fresh })).status).toBe(200);
  });

  it("QA-R1-025: rate-limit buckets — IPv4 per address, IPv6 per /64, IPv4-mapped IPv6 as IPv4", () => {
    expect(rateLimitBucket("203.0.113.7")).toBe("203.0.113.7");
    expect(rateLimitBucket("2001:db8:1:2::1")).toBe("2001:db8:1:2::/64");
    expect(rateLimitBucket("2001:0db8:0001:0002:ffff:ffff:ffff:ffff")).toBe("2001:db8:1:2::/64");
    expect(rateLimitBucket("[2001:DB8:1:2:3:4:5:6]")).toBe("2001:db8:1:2::/64");
    expect(rateLimitBucket("2001:db8:1:3::1")).toBe("2001:db8:1:3::/64");
    expect(rateLimitBucket("::ffff:198.51.100.9")).toBe("198.51.100.9");
    expect(rateLimitBucket("::1")).toBe("0:0:0:0::/64");
    expect(rateLimitBucket("not-an-ip:zz")).toBe("not-an-ip:zz");
  });

  it("tokens are scoped to their own workspace", async () => {
    const a = await newWorkspace("A");
    const b = await newWorkspace("B");
    await call("/v1/sync", { token: a.token, body: { changes: [change("request", "r1", 1)] } });
    const pullB = await callJson("/v1/sync", { token: b.token });
    expect(pullB.data.changes).toEqual([]);
  });
});

describe("sync", () => {
  it("AC-SYNC-2: push then pull returns changes ordered by rev with a cursor (per-workspace revs)", async () => {
    const { token } = await newWorkspace();
    const pushed = await callJson("/v1/sync", {
      token,
      body: {
        changes: [
          change("project", "p1", 1000, { id: "p1", name: "Default" }),
          change("collection", "c1", 1001, { id: "c1", projectId: "p1", name: "Users" }),
          change("request", "r1", 1002, { id: "r1", collectionId: "c1", method: "QUERY", url: "/x" }),
        ],
      },
    });
    expect(pushed.res.status).toBe(200);
    expect(pushed.data).toEqual({
      results: [
        { type: "project", id: "p1", status: "applied", rev: 1 },
        { type: "collection", id: "c1", status: "applied", rev: 2 },
        { type: "request", id: "r1", status: "applied", rev: 3 },
      ],
      cursor: 3,
    });

    const all = await callJson("/v1/sync?since=0", { token });
    expect(all.data.hasMore).toBe(false);
    expect(all.data.cursor).toBe(3);
    expect(all.data.changes.map((c: any) => [c.type, c.id, c.rev])).toEqual([
      ["project", "p1", 1],
      ["collection", "c1", 2],
      ["request", "r1", 3],
    ]);
    expect(all.data.changes[2]).toEqual({
      type: "request",
      id: "r1",
      updatedAt: 1002,
      deleted: false,
      deviceId: "",
      data: { id: "r1", collectionId: "c1", method: "QUERY", url: "/x" },
      rev: 3,
    });

    const after = await callJson("/v1/sync?since=2", { token });
    expect(after.data.changes.map((c: any) => c.id)).toEqual(["r1"]);
    const caughtUp = await callJson("/v1/sync?since=3", { token });
    expect(caughtUp.data).toEqual({ changes: [], cursor: 3, hasMore: false });
  });

  it("AC-SYNC-2: pull pages with limit/hasMore; 500 changes push in one call", async () => {
    const { token } = await newWorkspace();
    const changes = Array.from({ length: 500 }, (_, i) => change("request", `r${i}`, 1000 + i));
    const pushed = await callJson("/v1/sync", { token, body: { changes } });
    expect(pushed.res.status).toBe(200);
    expect(pushed.data.results).toHaveLength(500);
    expect(pushed.data.results.every((r: any) => r.status === "applied")).toBe(true);
    expect(pushed.data.results.map((r: any) => r.rev)).toEqual(Array.from({ length: 500 }, (_, i) => i + 1));

    const page1 = await callJson("/v1/sync?since=0&limit=200", { token });
    expect(page1.data.changes).toHaveLength(200);
    expect(page1.data.hasMore).toBe(true);
    expect(page1.data.cursor).toBe(200);
    const page2 = await callJson(`/v1/sync?since=${page1.data.cursor}&limit=500`, { token });
    expect(page2.data.changes).toHaveLength(300);
    expect(page2.data.hasMore).toBe(false);
    expect(page2.data.cursor).toBe(500);

    const over = await callJson("/v1/sync", { token, body: { changes: [...changes, change("request", "x", 1)] } });
    expect(over.res.status).toBe(413);
    expect(over.data.error.code).toBe("too_many_changes");
  });

  it("AC-SYNC-3: only project, collection, folder, request, environment are accepted (no history/settings)", async () => {
    const { token } = await newWorkspace();
    const ok = await callJson("/v1/sync", {
      token,
      body: { changes: ["project", "collection", "folder", "request", "environment"].map((t, i) => change(t, `id${i}`, 1)) },
    });
    expect(ok.res.status).toBe(200);
    for (const type of ["history", "settings", "workspace", "Request"]) {
      const bad = await callJson("/v1/sync", { token, body: { changes: [change(type, "x", 1)] } });
      expect(bad.res.status).toBe(400);
      expect(bad.data.error.code).toBe("unsupported_type");
    }
    // Rejected pushes are atomic: nothing was written.
    const me = await callJson("/v1/workspace", { token });
    expect(me.data.entityCount).toBe(5);
  });

  it("AC-SYNC-4: last-write-wins by updatedAt; an older push is stale and the server copy comes back in the next pull", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", { token, body: { changes: [change("request", "r1", 2000, { name: "device B (newer)" })] } });
    const pulled = await callJson("/v1/sync?since=0", { token });
    const cursor = pulled.data.cursor;

    const stale = await callJson("/v1/sync", {
      token,
      body: { changes: [change("request", "r1", 1500, { name: "device A (older)" })] },
    });
    expect(stale.data.results).toEqual([{ type: "request", id: "r1", status: "stale", rev: cursor + 1 }]);

    // The client's cursor was already past the winning copy; it is re-delivered anyway.
    const next = await callJson(`/v1/sync?since=${cursor}`, { token });
    expect(next.data.changes).toEqual([
      { type: "request", id: "r1", updatedAt: 2000, deleted: false, deviceId: "", data: { name: "device B (newer)" }, rev: cursor + 1 },
    ]);

    const newer = await callJson("/v1/sync", { token, body: { changes: [change("request", "r1", 2500, { name: "A again" })] } });
    expect(newer.data.results[0].status).toBe("applied");
    const final = await callJson("/v1/sync?since=0", { token });
    expect(final.data.changes).toHaveLength(1);
    expect(final.data.changes[0].data).toEqual({ name: "A again" });
  });

  it("AC-SYNC-4: equal updatedAt is applied (only strictly older pushes are stale)", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", { token, body: { changes: [change("folder", "f1", 1000, { name: "one" })] } });
    const tie = await callJson("/v1/sync", { token, body: { changes: [change("folder", "f1", 1000, { name: "two" })] } });
    expect(tie.data.results[0].status).toBe("applied");
  });

  it("AC-SYNC-4: mixed push reports applied and stale per change", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", { token, body: { changes: [change("request", "a", 5000), change("request", "b", 5000)] } });
    const res = await callJson("/v1/sync", {
      token,
      body: { changes: [change("request", "a", 4000), change("request", "b", 6000), change("request", "c", 1)] },
    });
    expect(res.data.results.map((r: any) => [r.id, r.status])).toEqual([
      ["a", "stale"],
      ["b", "applied"],
      ["c", "applied"],
    ]);
    expect(res.data.cursor).toBe(5);
  });

  it("AC-SYNC-5: deletes propagate as tombstones (and lose to newer edits)", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", {
      token,
      body: { changes: [change("collection", "c1", 1000), change("request", "r1", 1000, { collectionId: "c1" })] },
    });
    const del = await callJson("/v1/sync", { token, body: { changes: [change("collection", "c1", 2000, null, true)] } });
    expect(del.data.results[0].status).toBe("applied");

    const pulled = await callJson("/v1/sync?since=2", { token });
    expect(pulled.data.changes).toEqual([
      { type: "collection", id: "c1", updatedAt: 2000, deleted: true, deviceId: "", data: null, rev: 3 },
    ]);
    expect((await callJson("/v1/workspace", { token })).data.entityCount).toBe(1);

    // An older edit cannot resurrect it; a newer one can (LWW).
    const old = await callJson("/v1/sync", { token, body: { changes: [change("collection", "c1", 1500)] } });
    expect(old.data.results[0].status).toBe("stale");
    const revive = await callJson("/v1/sync", { token, body: { changes: [change("collection", "c1", 3000)] } });
    expect(revive.data.results[0].status).toBe("applied");
  });

  it("AC-SYNC-5: tombstones must not carry data", async () => {
    const { token } = await newWorkspace();
    const bad = await callJson("/v1/sync", {
      token,
      body: { changes: [{ type: "request", id: "r", updatedAt: 1, deleted: true, data: { x: 1 } }] },
    });
    expect(bad.res.status).toBe(400);
  });

  it("AC-ENV-7: secret environment values are never stored by sync", async () => {
    const { token, workspaceId } = await newWorkspace();
    const envData = {
      id: "e1",
      name: "Staging",
      projectId: "p1",
      variables: [
        { key: "baseUrl", value: "https://staging.example.com", enabled: true, secret: false },
        { key: "token", value: "super-secret-value", enabled: true, secret: true },
      ],
    };
    await call("/v1/sync", { token, body: { changes: [change("environment", "e1", 1000, envData)] } });
    const pulled = await callJson("/v1/sync", { token });
    expect(pulled.data.changes[0].data.variables).toEqual([
      { key: "baseUrl", value: "https://staging.example.com", enabled: true, secret: false },
      { key: "token", value: "", enabled: true, secret: true },
    ]);
    const raw = await env.DB.prepare("SELECT data FROM entities WHERE workspace_id = ?").bind(workspaceId).first<{ data: string }>();
    expect(raw?.data).not.toContain("super-secret-value");
  });

  it("validates change shape and query params with consistent 400 errors", async () => {
    const { token } = await newWorkspace();
    const cases: unknown[] = [
      {},
      { changes: "nope" },
      { changes: [null] },
      { changes: [{ type: "request", id: "", updatedAt: 1, deleted: false, data: {} }] },
      { changes: [{ type: "request", id: "a/b", updatedAt: 1, deleted: false, data: {} }] },
      { changes: [{ type: "request", id: "a", updatedAt: -1, deleted: false, data: {} }] },
      { changes: [{ type: "request", id: "a", updatedAt: 1.5, deleted: false, data: {} }] },
      { changes: [{ type: "request", id: "a", updatedAt: 1, deleted: "no", data: {} }] },
      { changes: [{ type: "request", id: "a", updatedAt: 1, deleted: false, data: null }] },
      { changes: [{ type: "request", id: "a", updatedAt: 1, deleted: false, data: [1] }] },
      { changes: [change("request", "dup", 1), change("request", "dup", 2)] },
    ];
    for (const body of cases) {
      const { res, data } = await callJson("/v1/sync", { token, body });
      expect(res.status, JSON.stringify(body)).toBe(400);
      expect(data.error).toEqual({ code: expect.any(String), message: expect.any(String) });
    }
    for (const qs of ["since=-1", "since=abc", "limit=0", "limit=1.5"]) {
      expect((await call(`/v1/sync?${qs}`, { token })).status).toBe(400);
    }
    // limit above the max is clamped, not rejected
    expect((await call("/v1/sync?limit=10000", { token })).status).toBe(200);
  });

  it("QA-R1-005 / contract C2: an entity > 512 KB is rejected per change while the rest of the push applies", async () => {
    const { token } = await newWorkspace();
    const big = change("request", "big", 1, { blob: "x".repeat(512 * 1024) });
    const fits = change("request", "fits", 1, { blob: "x".repeat(500 * 1024) });
    const small = change("request", "small", 1, { name: "small" });
    const { res, data } = await callJson("/v1/sync", { token, body: { changes: [small, big, fits] } });
    expect(res.status).toBe(200);
    expect(data.results).toEqual([
      { type: "request", id: "small", status: "applied", rev: 1 },
      { type: "request", id: "big", status: "rejected", reason: "entity_too_large" },
      { type: "request", id: "fits", status: "applied", rev: 2 },
    ]);
    expect(data.cursor).toBe(2);
    const pulled = await callJson("/v1/sync?since=0", { token });
    expect(pulled.data.changes.map((c: any) => c.id)).toEqual(["small", "fits"]);

    // A push of only oversized entities is still a 200 with every change rejected; nothing is written.
    const only = await callJson("/v1/sync", { token, body: { changes: [change("request", "big2", 1, { blob: "y".repeat(600 * 1024) })] } });
    expect(only.res.status).toBe(200);
    expect(only.data).toEqual({ results: [{ type: "request", id: "big2", status: "rejected", reason: "entity_too_large" }], cursor: 2 });

    // Oversize is measured after secret stripping, on the serialized UTF-8 bytes.
    const multibyte = change("request", "mb", 1, { blob: "é".repeat(300 * 1024) }); // 600 KB of UTF-8
    const mb = await callJson("/v1/sync", { token, body: { changes: [multibyte] } });
    expect(mb.data.results[0].status).toBe("rejected");
  });

  it("QA-R1-004 / contract C1: a body over 5 MB is still a whole-request 413 payload_too_large", async () => {
    const { token } = await newWorkspace();
    const changes = Array.from({ length: 11 }, (_, i) => change("request", `r${i}`, 1, { blob: "x".repeat(500 * 1024) }));
    const { res, data } = await callJson("/v1/sync", { token, body: { changes } });
    expect(res.status).toBe(413);
    expect(data.error.code).toBe("payload_too_large");
    // The same changes split under 4 MB per request (client batching rule) all apply.
    for (const batch of [changes.slice(0, 6), changes.slice(6)]) {
      const r = await callJson("/v1/sync", { token, body: { changes: batch } });
      expect(r.res.status).toBe(200);
      expect(r.data.results.every((x: any) => x.status === "applied")).toBe(true);
    }
  });

  it("QA-R1-026 / contract C3: equal updatedAt is resolved by the greater deviceId, in either push order", async () => {
    for (const order of [["A", "B"], ["B", "A"]] as const) {
      const { token } = await newWorkspace();
      const push = (dev: string) =>
        callJson("/v1/sync", { token, body: { changes: [{ ...change("request", "r1", 1000, { from: dev }), deviceId: `01DEVICE${dev}` }] } });
      const first = await push(order[0]);
      const second = await push(order[1]);
      expect(first.data.results[0].status).toBe("applied");
      // B > A: B always ends up stored; A arriving second is stale.
      expect(second.data.results[0].status).toBe(order[1] === "B" ? "applied" : "stale");
      const pulled = await callJson("/v1/sync?since=0", { token });
      expect(pulled.data.changes).toEqual([
        { type: "request", id: "r1", updatedAt: 1000, deleted: false, deviceId: "01DEVICEB", data: { from: "B" }, rev: expect.any(Number) },
      ]);
    }
  });

  it("QA-R1-026 / contract C3: a missing deviceId counts as \"\"; same device re-push at the same updatedAt applies", async () => {
    const { token } = await newWorkspace();
    await call("/v1/sync", { token, body: { changes: [{ ...change("folder", "f1", 5, { v: 1 }), deviceId: "01DEV" }] } });
    const old = await callJson("/v1/sync", { token, body: { changes: [change("folder", "f1", 5, { v: "old client" })] } });
    expect(old.data.results[0].status).toBe("stale"); // "" < "01DEV"
    const again = await callJson("/v1/sync", { token, body: { changes: [{ ...change("folder", "f1", 5, { v: 2 }), deviceId: "01DEV" }] } });
    expect(again.data.results[0].status).toBe("applied");
    const newer = await callJson("/v1/sync", { token, body: { changes: [change("folder", "f1", 6, { v: "newer wins" })] } });
    expect(newer.data.results[0].status).toBe("applied");
    const pulled = await callJson("/v1/sync?since=0", { token });
    expect(pulled.data.changes[0]).toMatchObject({ deviceId: "", data: { v: "newer wins" } });
    for (const bad of [5, "has space", "x".repeat(129)]) {
      const r = await call("/v1/sync", { token, body: { changes: [{ ...change("folder", "f2", 1), deviceId: bad }] } });
      expect(r.status).toBe(400);
    }
  });

  it("QA-R1-024: entity pushes count toward the 50 MB workspace quota (413 quota_exceeded); deletes always pass", async () => {
    const { token } = await newWorkspace();
    const blob = "x".repeat(500 * 1024);
    let rev = 0;
    for (let batch = 0; batch < 14; batch++) {
      const changes = Array.from({ length: 8 }, (_, i) => change("request", `r${batch}-${i}`, 1, { blob }));
      const r = await callJson("/v1/sync", { token, body: { changes } });
      if (r.res.status !== 200) {
        expect(r.res.status).toBe(413);
        expect(r.data.error.code).toBe("quota_exceeded");
        break;
      }
      rev = r.data.cursor;
    }
    const me = await callJson("/v1/workspace", { token });
    expect(me.data.bytesUsed).toBeGreaterThan(45 * 1024 * 1024);
    expect(me.data.bytesUsed).toBeLessThanOrEqual(50 * 1024 * 1024);
    // Replacing an entity with a smaller one and deleting are allowed even near the cap.
    const del = await callJson("/v1/sync", { token, body: { changes: [change("request", "r0-0", 2, null, true), change("request", "r0-1", 2, { small: 1 })] } });
    expect(del.res.status).toBe(200);
    expect((await callJson("/v1/workspace", { token })).data.bytesUsed).toBeLessThan(me.data.bytesUsed);
    expect(rev).toBeGreaterThan(0);
  });
});

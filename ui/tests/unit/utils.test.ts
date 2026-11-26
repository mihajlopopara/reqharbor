import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { prettyJson, jsonErrorPosition } from "../../src/lib/util/json";
import { formatSize, formatMs, relativeTime, historyGroup, statusClass, methodClass, methodLabel } from "../../src/lib/util/format";
import { flattenTree, indexTree, ancestorsOf } from "../../src/lib/util/tree";
import { requestFingerprint, requestSummary, scratchRequest } from "../../src/lib/util/request";
import { seed } from "../../src/lib/api/mock/seed";

describe("types.ts copy", () => {
  it("is identical to docs/types.ts (the contract)", () => {
    const docs = readFileSync(fileURLToPath(new URL("../../../docs/types.ts", import.meta.url)), "utf8");
    const ours = readFileSync(fileURLToPath(new URL("../../src/lib/api/types.ts", import.meta.url)), "utf8");
    expect(ours).toBe(docs);
  });
});

describe("tokens.css copy", () => {
  it("is identical to docs/design/tokens.css", () => {
    const docs = readFileSync(fileURLToPath(new URL("../../../docs/design/tokens.css", import.meta.url)), "utf8");
    const ours = readFileSync(fileURLToPath(new URL("../../src/tokens.css", import.meta.url)), "utf8");
    expect(ours).toBe(docs);
  });
});

describe("prettyJson", () => {
  it("indents with 2 spaces and preserves number literals", () => {
    expect(prettyJson('{"a":1,"big":12345678901234567890,"s":"x,{y}","e":[],"o":{}}')).toBe(
      '{\n  "a": 1,\n  "big": 12345678901234567890,\n  "s": "x,{y}",\n  "e": [],\n  "o": {}\n}',
    );
  });
  it("returns null for invalid JSON and reports a position", () => {
    expect(prettyJson("{nope}")).toBeNull();
    const text = '{\n  "a": 1,\n  "b": }';
    let err: unknown;
    try {
      JSON.parse(text);
    } catch (e) {
      err = e;
    }
    const pos = jsonErrorPosition(text, err);
    expect(pos.line).toBe(3);
    expect(pos.column).toBeGreaterThan(1);
  });
});

describe("format helpers", () => {
  it("formats sizes and times", () => {
    expect(formatSize(247)).toBe("247 B");
    expect(formatSize(2048)).toBe("2 KB");
    expect(formatSize(1536)).toBe("1.5 KB");
    expect(formatSize(5 * 1024 * 1024)).toBe("5 MB");
    expect(formatMs(1234.4)).toBe("1,234 ms");
  });
  it("relative times (DESIGN §5.2)", () => {
    const now = new Date(2026, 10, 26, 15, 0, 0).getTime();
    expect(relativeTime(now - 10_000, now)).toBe("now");
    expect(relativeTime(now - 2 * 60_000, now)).toBe("2m");
    expect(relativeTime(now - 3 * 3600_000, now)).toBe("3h");
    expect(relativeTime(new Date(2026, 10, 25, 9).getTime(), now)).toBe("Yesterday");
    expect(relativeTime(new Date(2026, 10, 12, 9).getTime(), now)).toBe("Nov 12");
    expect(historyGroup(now - 1000, now)).toBe("Today");
    expect(historyGroup(new Date(2026, 10, 25, 9).getTime(), now)).toBe("Yesterday");
    expect(historyGroup(new Date(2026, 9, 1).getTime(), now)).toBe("Earlier");
  });
  it("status and method classes", () => {
    expect([101, 200, 301, 404, 503].map(statusClass)).toEqual(["s1", "s2", "s3", "s4", "s5"]);
    expect(methodClass("QUERY")).toBe("m-query");
    expect(methodClass("PURGE")).toBe("m-custom");
    expect(methodLabel("PROPPATCH")).toBe("PROPPA…");
  });
});

describe("tree flattening + filter (AC-COL-4)", () => {
  const s = seed(Date.now());
  const def = s.projects[0];
  const cols = s.collections.filter((c) => c.projectId === def.id);
  const ids = new Set(cols.map((c) => c.id));
  const tree = { project: def, collections: cols, folders: s.folders.filter((f) => ids.has(f.collectionId)), requests: s.requests.filter((r) => ids.has(r.collectionId)) };
  const ix = indexTree(tree);

  it("shows only collections when nothing is expanded", () => {
    expect(flattenTree(ix, new Set(), "").map((r) => r.name)).toEqual(["Users API", "Billing"]);
  });
  it("folders first, then requests, by sortOrder", () => {
    const users = cols.find((c) => c.name === "Users API")!;
    const rows = flattenTree(ix, new Set([users.id]), "");
    expect(rows.map((r) => r.name)).toEqual(["Users API", "Users", "Admin", "Health", "Billing"]);
    expect(rows[0].count).toBe(6);
  });
  it("filter matches name or URL case-insensitively and keeps ancestors open", () => {
    const rows = flattenTree(ix, new Set(), "AUDIT");
    expect(rows.map((r) => [r.kind, r.name])).toEqual([
      ["collection", "Users API"],
      ["folder", "Admin"],
      ["folder", "Audit"],
      ["request", "Audit log"],
    ]);
    expect(flattenTree(ix, new Set(), "/v1/invoices").filter((r) => r.kind === "request").map((r) => r.name)).toEqual(["List invoices", "Update invoice"]);
  });
  it("ancestorsOf returns collection and folder chain", () => {
    const audit = tree.requests.find((r) => r.name === "Audit log")!;
    expect(ancestorsOf(ix, audit.id).length).toBe(3);
  });
});

describe("request helpers", () => {
  it("fingerprint ignores timestamps; summary describes the request", () => {
    const a = scratchRequest({ url: "x" });
    const b = { ...a, updatedAt: 99, createdAt: 5 };
    expect(requestFingerprint(a)).toBe(requestFingerprint(b));
    expect(requestSummary({ ...a, headers: [{ key: "A", value: "1", enabled: true }, { key: "B", value: "", enabled: true }], body: { type: "json", text: "{}" }, auth: { type: "basic" } })).toBe(
      "2 headers · JSON body · Basic auth",
    );
  });
});

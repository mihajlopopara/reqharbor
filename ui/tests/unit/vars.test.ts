import { describe, expect, it } from "vitest";
import { makeLookup, segmentAt, tokenizeVars, unresolvedNames } from "../../src/lib/util/vars";
import { highlightParts, historyMatches } from "../../src/lib/util/highlight";
import type { VariableInfo } from "../../src/lib/api/types";

const info = (key: string, value: string, env = "Staging", secret = false): VariableInfo => ({
  key,
  value,
  secret,
  environmentId: env.toLowerCase(),
  environmentName: env,
});

describe("{{var}} highlighter", () => {
  const lookup = makeLookup([info("baseUrl", "https://api.dev"), info("token", "s3cr3t", "Staging", true), info("baseUrl", "shadowed", "Global")]);

  it("splits text into plain, resolved and unresolved segments with offsets", () => {
    const segs = tokenizeVars("{{baseUrl}}/users/{{ id }}?t={{token}}", lookup);
    expect(segs.map((s) => [s.kind, s.text, s.start, s.end])).toEqual([
      ["var", "{{baseUrl}}", 0, 11],
      ["text", "/users/", 11, 18],
      ["unresolved", "{{ id }}", 18, 26],
      ["text", "?t=", 26, 29],
      ["var", "{{token}}", 29, 38],
    ]);
  });

  it("first definition per key wins (project env before Global)", () => {
    const seg = tokenizeVars("{{baseUrl}}", lookup)[0];
    expect(seg.kind === "var" && seg.info.environmentName).toBe("Staging");
  });

  it("ignores names outside [A-Za-z0-9_.-] and unbalanced braces", () => {
    expect(tokenizeVars("{{a b}} {{x}", lookup).every((s) => s.kind === "text")).toBe(true);
    expect(tokenizeVars("{{a.b-c_1}}", lookup)[0].kind).toBe("unresolved");
  });

  it("finds the segment under an offset (hover)", () => {
    const segs = tokenizeVars("ab{{token}}", lookup);
    expect(segmentAt(segs, 0)?.kind).toBe("text");
    expect(segmentAt(segs, 5)?.kind).toBe("var");
    expect(segmentAt(segs, 11)).toBeNull();
  });

  it("collects unresolved names across fields, deduplicated", () => {
    expect(unresolvedNames(["{{a}}/{{baseUrl}}", "{{a}} {{b}}"], lookup)).toEqual(["a", "b"]);
  });

  it("returns a single text segment for plain text", () => {
    expect(tokenizeVars("plain", lookup)).toEqual([{ kind: "text", text: "plain", start: 0, end: 5 }]);
    expect(tokenizeVars("", lookup)).toEqual([]);
  });
});

describe("filter helpers", () => {
  it("marks case-insensitive matches", () => {
    expect(highlightParts("Create User", "user")).toEqual([
      { t: "Create ", m: false },
      { t: "User", m: true },
    ]);
  });
  it("history filter matches URL, method or status (AC-HIST-3)", () => {
    const h = { resolvedUrl: "https://api.dev/users", method: "QUERY", status: 404, errorKind: null };
    expect(historyMatches(h, "404")).toBe(true);
    expect(historyMatches(h, "query")).toBe(true);
    expect(historyMatches(h, "USERS")).toBe(true);
    expect(historyMatches(h, "500")).toBe(false);
  });
});

// QA-R1-006: share-page cURL must match the Rust exporter byte for byte.
// Every case in fixtures/curl_parity.json carries `expected` produced by the real Rust exporter
// (`npm run parity:gen` runs crates/core's export_curl over the same file). Requests go through
// the share page's own path: snapshot normalization -> toCurl.

import { describe, expect, it } from "vitest";
import { CurlExportError, toCurl } from "../src/curl";
import { normalizeAuth, normalizeSnapshot } from "../src/snapshot";
import { variableMap } from "../src/variables";
import fixture from "./fixtures/curl_parity.json";

interface Case {
  name: string;
  request: Record<string, unknown>;
  variables?: { key: string; value: string; enabled: boolean }[];
  collectionAuth?: unknown;
  keepVariables?: boolean;
  expected: { curl?: string; error?: string };
}

const cases = (fixture as { cases: Case[] }).cases;

describe("cURL export parity with the Rust exporter (shared fixtures)", () => {
  it("fixture file has generated expectations for every case", () => {
    expect(cases.length).toBeGreaterThan(30);
    for (const c of cases) expect(c.expected, c.name).toBeDefined();
  });

  for (const c of cases) {
    it(`QA-R1-006 parity: ${c.name}`, () => {
      const [req] = normalizeSnapshot("request", { schemaVersion: 1, request: c.request }).requests;
      const run = () =>
        toCurl(req!, {
          variables: variableMap((c.variables ?? []).map((v) => ({ ...v, secret: false }))),
          keepVariables: c.keepVariables ?? false,
          inheritedAuth: c.collectionAuth === undefined ? undefined : normalizeAuth(c.collectionAuth),
        });
      if (c.expected.curl !== undefined) {
        expect(run()).toBe(c.expected.curl);
      } else {
        expect(run).toThrow(CurlExportError);
        expect(run).toThrow(c.expected.error!);
      }
    });
  }
});

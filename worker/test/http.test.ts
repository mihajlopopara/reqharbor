import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import worker from "../src/index";
import { BASE, call, callJson, newWorkspace } from "./helpers";

describe("HTTP contract", () => {
  it("request bodies over 5 MB get 413 (declared and streamed)", async () => {
    const { token } = await newWorkspace();
    const big = JSON.stringify({ changes: [], pad: "x".repeat(5 * 1024 * 1024) });
    const { res, data } = await callJson("/v1/sync", {
      token,
      rawBody: big,
      headers: { "Content-Type": "application/json" },
      method: "POST",
    });
    expect(res.status).toBe(413);
    expect(data.error.code).toBe("payload_too_large");

    // Chunked body without Content-Length is cut off while streaming.
    const chunk = new TextEncoder().encode("x".repeat(1024 * 1024));
    let sent = 0;
    const stream = new ReadableStream<Uint8Array>({
      pull(ctrl) {
        if (sent++ < 6) ctrl.enqueue(chunk);
        else ctrl.close();
      },
    });
    const streamed = await SELF.fetch(`${BASE}/v1/sync`, {
      method: "POST",
      headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" },
      body: stream,
    });
    expect(streamed.status).toBe(413);
  });

  it("invalid JSON, wrong content type, and non-object bodies return JSON errors", async () => {
    const { token } = await newWorkspace();
    const cases: [Record<string, string>, string, number, string][] = [
      [{ "Content-Type": "application/json" }, "{not json", 400, "invalid_json"],
      [{ "Content-Type": "application/json" }, "", 400, "invalid_json"],
      [{ "Content-Type": "text/plain" }, "{}", 415, "unsupported_media_type"],
      [{ "Content-Type": "application/json; charset=utf-8" }, "[]", 400, "validation_failed"],
      [{ "Content-Type": "application/json" }, "null", 400, "validation_failed"],
    ];
    for (const [headers, rawBody, status, code] of cases) {
      const { res, data } = await callJson("/v1/sync", { token, method: "POST", headers, rawBody });
      expect(res.status, rawBody).toBe(status);
      expect(res.headers.get("Content-Type")).toBe("application/json; charset=utf-8");
      expect(data).toEqual({ error: { code, message: expect.any(String) } });
    }
  });

  it("unknown routes are 404 and wrong methods are 405 with Allow, both as JSON errors", async () => {
    const nf = await callJson("/v2/nope");
    expect(nf.res.status).toBe(404);
    expect(nf.data.error.code).toBe("not_found");
    const na = await callJson("/v1/workspace", { method: "DELETE" });
    expect(na.res.status).toBe(405);
    expect(na.res.headers.get("Allow")).toBe("GET");
    expect(na.data.error.code).toBe("method_not_allowed");
  });

  it("security headers on every response; API responses are no-store with a deny-all CSP", async () => {
    const { token } = await newWorkspace();
    for (const res of [await call("/v1/workspace", { token }), await call("/nope"), await call("/v1/sync")]) {
      expect(res.headers.get("X-Content-Type-Options")).toBe("nosniff");
      expect(res.headers.get("Referrer-Policy")).toBe("no-referrer");
      expect(res.headers.get("X-Frame-Options")).toBe("DENY");
      expect(res.headers.get("Strict-Transport-Security")).toContain("max-age=");
      expect(res.headers.get("Content-Security-Policy")).toContain("default-src 'none'");
      expect(res.headers.get("Cache-Control")).toBe("no-store");
      expect(res.headers.get("Set-Cookie")).toBeNull();
    }
  });

  it("no CORS on /v1/* (called from the Rust core, not browsers)", async () => {
    const { token } = await newWorkspace();
    const res = await call("/v1/workspace", { token, headers: { Origin: "https://evil.example" } });
    expect(res.headers.get("Access-Control-Allow-Origin")).toBeNull();
    const pre = await call("/v1/sync", { method: "OPTIONS", headers: { Origin: "https://evil.example" } });
    expect(pre.status).toBe(405);
    expect(pre.headers.get("Access-Control-Allow-Origin")).toBeNull();
  });

  it("AC-SYNC-6: internal failures return a 5xx JSON error (retryable) without leaking details", async () => {
    const broken = {
      DB: {
        prepare() {
          throw new Error("D1_ERROR: secret internal detail");
        },
      },
    } as unknown as Env;
    const token = "rh_" + "A".repeat(43);
    const res = await worker.fetch(new Request(`${BASE}/v1/sync`, { headers: { Authorization: `Bearer ${token}` } }), broken);
    expect(res.status).toBe(500);
    expect(res.headers.get("Content-Type")).toBe("application/json; charset=utf-8");
    const body = await res.text();
    expect(JSON.parse(body)).toEqual({ error: { code: "internal_error", message: "Internal server error" } });
    expect(body).not.toContain("secret internal detail");
  });
});

import { describe, expect, it } from "vitest";
import { applyDefaultScheme, buildQuery, paramsToUrl, parseQuery, splitUrl, urlToParams } from "../../src/lib/util/urlx";
import type { KeyValue } from "../../src/lib/api/types";

const kv = (key: string, value: string, enabled = true, description?: string): KeyValue =>
  description === undefined ? { key, value, enabled } : { key, value, enabled, description };

describe("splitUrl / parseQuery", () => {
  it("splits base, query and fragment", () => {
    expect(splitUrl("https://x.dev/a?b=1&c#frag?x")).toEqual({ base: "https://x.dev/a", query: "b=1&c", fragment: "frag?x" });
    expect(splitUrl("https://x.dev/a")).toEqual({ base: "https://x.dev/a", query: null, fragment: null });
  });
  it("keeps order, duplicates and raw (undecoded) values", () => {
    expect(parseQuery("a=1&a=2&b&c=%20x&&d=")).toEqual([kv("a", "1"), kv("a", "2"), kv("b", ""), kv("c", "%20x"), kv("d", "")]);
  });
});

describe("Query tab -> URL (AC-REQ-2)", () => {
  it("writes enabled rows in order, skipping disabled ones", () => {
    const url = paramsToUrl("https://api.dev/users?old=1#top", [kv("a", "1"), kv("b", "2", false), kv("a", "3")]);
    expect(url).toBe("https://api.dev/users?a=1&a=3#top");
  });
  it("drops the ? when nothing is enabled", () => {
    expect(paramsToUrl("https://api.dev/x?y=1", [kv("y", "1", false)])).toBe("https://api.dev/x");
  });
  it("encodes only structure-breaking characters and keeps {{vars}} and %XX", () => {
    expect(buildQuery([kv("q", "a b&c#d"), kv("k=1", "v"), kv("t", "{{token}}"), kv("p", "%41é")])).toBe("q=a%20b%26c%23d&k%3D1=v&t={{token}}&p=%41%C3%A9");
  });
  it("keys without value are written bare", () => {
    expect(buildQuery([kv("flag", ""), kv("", "")])).toBe("flag");
  });
});

describe("URL -> Query tab (AC-REQ-2)", () => {
  it("replaces enabled rows positionally and keeps disabled rows in place", () => {
    const existing = [kv("a", "1"), kv("off", "x", false), kv("b", "2", true, "desc")];
    expect(urlToParams("https://d.dev/?a=9&b=8&c=7", existing)).toEqual([kv("a", "9"), kv("off", "x", false), kv("b", "8", true, "desc"), kv("c", "7")]);
  });
  it("removes enabled rows that vanished from the URL", () => {
    expect(urlToParams("https://d.dev/", [kv("a", "1"), kv("z", "0", false)])).toEqual([kv("z", "0", false)]);
  });
  it("round-trips text unchanged", () => {
    const url = "https://d.dev/s?q=a%20b&q=c&x={{v}}";
    expect(paramsToUrl(url, urlToParams(url, []))).toBe(url);
  });
});

describe("default scheme (AC-REQ-10)", () => {
  it("uses http for loopback hosts and https otherwise", () => {
    expect(applyDefaultScheme("localhost:8080/health")).toBe("http://localhost:8080/health");
    expect(applyDefaultScheme("127.0.0.1/x")).toBe("http://127.0.0.1/x");
    expect(applyDefaultScheme("[::1]:3000")).toBe("http://[::1]:3000");
    expect(applyDefaultScheme("api.example.com/v1")).toBe("https://api.example.com/v1");
    expect(applyDefaultScheme("{{baseUrl}}/v1")).toBe("{{baseUrl}}/v1");
    expect(applyDefaultScheme("ftp://x")).toBe("ftp://x");
  });
});

import { describe, expect, it } from "vitest";
import { withBaseHref } from "../../src/lib/util/preview";
import { looksLikeCurl } from "../../src/lib/util/request";

describe("withBaseHref", () => {
  const url = "https://example.com/app/page?x=1";

  it("injects <base> right after <head>", () => {
    expect(withBaseHref('<!doctype html><html><head><link rel="stylesheet" href="/a.css"></head></html>', url)).toBe(
      '<!doctype html><html><head><base href="https://example.com/app/page?x=1"><link rel="stylesheet" href="/a.css"></head></html>',
    );
  });

  it("adds a <head> when there is none", () => {
    expect(withBaseHref("<html><body>hi</body></html>", url)).toBe('<html><head><base href="https://example.com/app/page?x=1"></head><body>hi</body></html>');
  });

  it("handles fragments with and without a doctype", () => {
    expect(withBaseHref("<!DOCTYPE html><p>x</p>", url)).toBe('<!DOCTYPE html><base href="https://example.com/app/page?x=1"><p>x</p>');
    expect(withBaseHref("<p>x</p>", url)).toBe('<base href="https://example.com/app/page?x=1"><p>x</p>');
  });

  it("resolves the page's own relative <base> against the response URL", () => {
    const out = withBaseHref('<head><base href="/static/"></head>', url);
    expect(out.startsWith('<head><base href="https://example.com/static/">')).toBe(true);
  });

  it("leaves the html alone without a usable http(s) URL", () => {
    expect(withBaseHref("<p>x</p>", null)).toBe("<p>x</p>");
    expect(withBaseHref("<p>x</p>", "{{host}}/x")).toBe("<p>x</p>");
    expect(withBaseHref("<p>x</p>", "file:///etc/passwd")).toBe("<p>x</p>");
  });

  it("escapes the URL in the attribute", () => {
    expect(withBaseHref("<p>x</p>", 'https://e.com/"><script>')).toBe('<base href="https://e.com/%22%3E%3Cscript%3E"><p>x</p>');
  });
});

describe("looksLikeCurl", () => {
  it("matches the core detector", () => {
    expect(looksLikeCurl("curl https://x")).toBe(true);
    expect(looksLikeCurl("  ﻿curl -X POST x")).toBe(true);
    expect(looksLikeCurl("$ curl x")).toBe(true);
    expect(looksLikeCurl("curl.exe x")).toBe(true);
    expect(looksLikeCurl("curl\\\n  x")).toBe(true);
    expect(looksLikeCurl("curlx")).toBe(false);
    expect(looksLikeCurl("https://curl.se")).toBe(false);
  });
});

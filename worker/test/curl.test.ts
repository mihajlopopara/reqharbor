import { describe, expect, it } from "vitest";
import { type CurlRequest, CurlExportError, LINE_JOIN, shellQuote, toCurl, withDefaultScheme } from "../src/curl";
import { resolveVariables, variableMap } from "../src/variables";

/**
 * Minimal POSIX sh word splitter (single quotes, double quotes, backslash, `\`+newline
 * continuation) — enough to prove our output parses back to the intended argv.
 */
function shellWords(cmd: string): string[] {
  const words: string[] = [];
  let cur = "";
  let inWord = false;
  for (let i = 0; i < cmd.length; i++) {
    const c = cmd[i]!;
    if (c === "'") {
      const end = cmd.indexOf("'", i + 1);
      if (end === -1) throw new Error("unterminated single quote");
      cur += cmd.slice(i + 1, end);
      inWord = true;
      i = end;
    } else if (c === "\\") {
      const next = cmd[i + 1];
      i++;
      if (next === "\n") continue; // line continuation
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

const base = (o: Partial<CurlRequest> = {}): CurlRequest => ({
  method: "GET",
  url: "https://api.example.com/users",
  headers: [],
  auth: { type: "none" },
  body: { type: "none" },
  ...o,
});

describe("curl exporter", () => {
  it("AC-CEXP-2: GET omits -X; multi-line, one -H per enabled header, joined with ` \\` + newline (+2-space indent, as the core)", () => {
    const out = toCurl(
      base({
        headers: [
          { key: "Accept", value: "application/json", enabled: true },
          { key: "X-Disabled", value: "no", enabled: false },
          { key: "X-Trace", value: "1", enabled: true },
        ],
      }),
    );
    expect(out).toBe(["curl", "'https://api.example.com/users'", "-H 'Accept: application/json'", "-H 'X-Trace: 1'"].join(LINE_JOIN));
    expect(LINE_JOIN).toBe(" \\\n  ");
  });

  it("AC-CEXP-2: POST with a body omits -X; POST without body and other methods emit -X", () => {
    const withBody = toCurl(base({ method: "POST", body: { type: "text", text: "hi" } }));
    expect(withBody).not.toContain("-X");
    expect(withBody.split(LINE_JOIN)).toEqual(["curl", "'https://api.example.com/users'", "-H 'Content-Type: text/plain'", "--data-raw 'hi'"]);
    expect(toCurl(base({ method: "POST" }))).toBe(["curl", "-X POST", "'https://api.example.com/users'"].join(LINE_JOIN));
    expect(toCurl(base({ method: "PUT", body: { type: "json", text: "{}" } })).split(LINE_JOIN)[1]).toBe("-X PUT");
    // A GET that carries a body must keep -X GET, or curl would send POST.
    expect(toCurl(base({ body: { type: "text", text: "x" } }))).toContain("-X GET");
  });

  it("AC-QRY-5: QUERY emits -X QUERY plus the body flag", () => {
    const out = toCurl(base({ method: "QUERY", body: { type: "json", text: '{"q":"ada"}' } }));
    expect(out.split(LINE_JOIN)).toEqual([
      "curl",
      "-X QUERY",
      "'https://api.example.com/users'",
      "-H 'Content-Type: application/json'",
      `--data-raw '{"q":"ada"}'`,
    ]);
  });

  it("AC-CEXP-3: every value is single-quoted with ' escaped as '\\'' and parses back verbatim", () => {
    expect(shellQuote("it's")).toBe("'it'\\''s'");
    const nasty = `O'Brien "quoted" $HOME \`whoami\` $(rm -rf /) \\ ; | & > < * ? ~ !\nnew line\ttab`;
    const out = toCurl(
      base({
        method: "POST",
        url: `https://x.test/?q=${nasty}`,
        headers: [{ key: "X-Nasty", value: nasty, enabled: true }],
        body: { type: "text", text: nasty },
      }),
    );
    expect(shellWords(out)).toEqual([
      "curl",
      `https://x.test/?q=${nasty}`,
      "-H",
      `X-Nasty: ${nasty}`,
      "-H",
      "Content-Type: text/plain",
      "--data-raw",
      nasty,
    ]);
    // Shell metacharacters only ever appear inside single quotes (argv above is exactly the input).
    const outsideQuotes = out.replace(/'\\''/g, "Q").replace(/'[^']*'/g, "");
    expect(outsideQuotes).not.toMatch(/[$`;|&<>()"]/);
  });

  it("AC-CEXP-4: JSON/XML/Text/GraphQL use --data-raw", () => {
    expect(toCurl(base({ method: "POST", body: { type: "xml", text: "<a/>" } }))).toContain("--data-raw '<a/>'");
    expect(toCurl(base({ method: "POST", body: { type: "xml", text: "<a/>" } }))).toContain("-H 'Content-Type: application/xml'");
    const gql = toCurl(
      base({ method: "POST", body: { type: "graphql", graphql: { query: "{ me { id } }", variables: '{"a": 1}' } } }),
    );
    const words = shellWords(gql);
    expect(JSON.parse(words[words.indexOf("--data-raw") + 1]!)).toEqual({ query: "{ me { id } }", variables: { a: 1 } });
    expect(gql).toContain("-H 'Content-Type: application/json'");
  });

  it("AC-CEXP-4: an explicit Content-Type header is not duplicated", () => {
    const out = toCurl(
      base({
        method: "POST",
        headers: [{ key: "content-type", value: "application/vnd.api+json", enabled: true }],
        body: { type: "json", text: "{}" },
      }),
    );
    expect(out.match(/content-type/gi)).toHaveLength(1);
  });

  it("AC-CEXP-4: Form-encode uses one --data-urlencode per enabled field", () => {
    const out = toCurl(
      base({
        method: "POST",
        body: {
          type: "formEncoded",
          formEncoded: [
            { key: "a", value: "1 2", enabled: true },
            { key: "b", value: "x&y", enabled: true },
            { key: "c", value: "off", enabled: false },
          ],
        },
      }),
    );
    expect(out.split(LINE_JOIN).slice(-2)).toEqual(["--data-urlencode 'a=1 2'", "--data-urlencode 'b=x&y'"]);
    expect(out).not.toContain("Content-Type");
  });

  it("AC-CEXP-4: Form uses -F, files as @path, ;type= kept; literal @ text uses --form-string", () => {
    const out = toCurl(
      base({
        method: "POST",
        body: {
          type: "form",
          form: [
            { key: "name", value: "Ada", enabled: true, kind: "text" },
            { key: "avatar", value: "/tmp/me.png", enabled: true, kind: "file", contentType: "image/png" },
            { key: "handle", value: "@ada", enabled: true, kind: "text" },
            { key: "skip", value: "x", enabled: false, kind: "text" },
          ],
        },
      }),
    );
    expect(out.split(LINE_JOIN).slice(2)).toEqual([
      "-F 'name=Ada'",
      "-F 'avatar=@/tmp/me.png;type=image/png'",
      "--form-string 'handle=@ada'",
    ]);
  });

  it("AC-CEXP-4: Binary uses --data-binary @path with octet-stream", () => {
    const out = toCurl(base({ method: "POST", body: { type: "binary", binaryPath: "/data/it's.bin" } }));
    expect(out).toContain("--data-binary '@/data/it'\\''s.bin'");
    expect(out).toContain("-H 'Content-Type: application/octet-stream'");
  });

  it("AC-CEXP-4: Basic uses -u; Bearer and API key become headers/query params; verifyTls=false adds -k", () => {
    expect(toCurl(base({ auth: { type: "basic", username: "ada", password: "p:w'd" } }))).toContain("-u 'ada:p:w'\\''d'");
    expect(toCurl(base({ auth: { type: "bearer", token: "abc" } }))).toContain("-H 'Authorization: Bearer abc'");
    expect(toCurl(base({ auth: { type: "apikey", apiKey: { key: "X-Key", value: "k1", in: "header" } } }))).toContain(
      "-H 'X-Key: k1'",
    );
    const q = toCurl(base({ url: "https://a.test/x?y=1#frag", auth: { type: "apikey", apiKey: { key: "api key", value: "a&b", in: "query" } } }));
    expect(q).toContain("'https://a.test/x?y=1&api%20key=a%26b#frag'");
    expect(toCurl(base({ settings: { verifyTls: false } })).split(LINE_JOIN)).toContain("-k");
    expect(toCurl(base())).not.toContain("-k");
  });

  it("AC-REQ-7: an explicit Authorization header wins over the Auth tab", () => {
    const out = toCurl(
      base({
        headers: [{ key: "Authorization", value: "Token xyz", enabled: true }],
        auth: { type: "bearer", token: "abc" },
      }),
    );
    expect(out).toContain("-H 'Authorization: Token xyz'");
    expect(out).not.toContain("Bearer abc");
    const basic = toCurl(base({ headers: [{ key: "authorization", value: "x", enabled: true }], auth: { type: "basic", username: "u" } }));
    expect(basic).not.toContain("-u");
  });

  it("Auth inherit uses the collection auth", () => {
    const out = toCurl(base({ auth: { type: "inherit" } }), { inheritedAuth: { type: "bearer", token: "{{token}}" } });
    expect(out).toContain("-H 'Authorization: Bearer {{token}}'");
    expect(toCurl(base({ auth: { type: "inherit" } }))).not.toContain("Authorization");
  });

  it("AC-CURL-4 round-trip: empty header values export as `Name;`", () => {
    expect(toCurl(base({ headers: [{ key: "X-Empty", value: "", enabled: true }] }))).toContain("-H 'X-Empty;'");
  });

  it("AC-CEXP-5: variables resolve by default; Keep {{variables}} exports raw", () => {
    const vars = variableMap([
      { key: "baseUrl", value: "https://staging.example.com", enabled: true },
      { key: "token", value: "t0k", enabled: true },
      { key: "secret", value: "never", enabled: true, secret: true },
    ]);
    const req = base({
      url: "{{baseUrl}}/users",
      headers: [{ key: "X-S", value: "{{secret}}", enabled: true }],
      auth: { type: "bearer", token: "{{ token }}" },
    });
    const resolved = toCurl(req, { variables: vars });
    expect(resolved).toContain("'https://staging.example.com/users'");
    expect(resolved).toContain("Bearer t0k");
    expect(resolved).toContain("-H 'X-S: {{secret}}'"); // secret variables are never resolved into shares
    const raw = toCurl(req, { variables: vars, keepVariables: true });
    expect(raw).toContain("'{{baseUrl}}/users'");
    expect(raw).toContain("Bearer {{ token }}");
  });

  it("AC-REQ-10: schemeless URLs get http:// for loopback and https:// otherwise", () => {
    expect(withDefaultScheme("localhost:3000/x")).toBe("http://localhost:3000/x");
    expect(withDefaultScheme("127.0.0.1/x")).toBe("http://127.0.0.1/x");
    expect(withDefaultScheme("[::1]:8080")).toBe("http://[::1]:8080");
    expect(withDefaultScheme("api.example.com/x")).toBe("https://api.example.com/x");
    expect(withDefaultScheme("http://a.test")).toBe("http://a.test");
    expect(withDefaultScheme("{{baseUrl}}/x")).toBe("{{baseUrl}}/x");
  });

  it("custom methods are emitted verbatim; invalid methods and empty URLs are export errors (as in the core)", () => {
    expect(toCurl(base({ method: "PURGE" }))).toContain("-X PURGE");
    expect(() => toCurl(base({ method: "BAD;X" }))).toThrow(CurlExportError);
    expect(() => toCurl(base({ method: "get" }))).toThrow('Invalid method "get"');
    expect(() => toCurl(base({ url: "  " }))).toThrow("URL is empty");
  });
});

describe("variable resolution", () => {
  it("AC-ENV-1: {{ name }} trims whitespace; unknown names stay literal", () => {
    const m = new Map([["a", "1"]]);
    expect(resolveVariables("{{ a }}-{{a}}-{{missing}}", m)).toBe("1-1-{{missing}}");
  });

  it("AC-ENV-2: nested up to depth 3 below the top level (core semantics), cycles leave the literal text, disabled skipped", () => {
    const m = variableMap([
      { key: "a", value: "{{b}}", enabled: true },
      { key: "b", value: "{{c}}", enabled: true },
      { key: "c", value: "{{d}}", enabled: true },
      { key: "d", value: "deep", enabled: true },
      { key: "x", value: "{{y}}", enabled: true },
      { key: "y", value: "{{x}}", enabled: true },
      { key: "off", value: "nope", enabled: false },
    ]);
    expect(resolveVariables("{{c}}", m)).toBe("deep");
    expect(resolveVariables("{{a}}", m)).toBe("deep");
    expect(resolveVariables("{{a}}", new Map([...m, ["d", "{{e}}"], ["e", "too deep"]]))).toBe("{{e}}");
    expect(resolveVariables("{{x}}", m)).toBe("{{x}}");
    expect(resolveVariables("{{off}}", m)).toBe("{{off}}");
  });
});

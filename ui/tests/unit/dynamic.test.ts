import { beforeEach, describe, expect, it } from "vitest";
import { MockServer } from "../../src/lib/api/mock";
import { makeCall, type CommandError, type VariableInfo } from "../../src/lib/api/types";
import { dynamicCatalogue, dynamicMemo, generateDynamic } from "../../src/lib/util/dynamic";
import { completeVar, dynamicTip, isReservedVarName, makeLookup, tokenizeVars, unresolvedNames } from "../../src/lib/util/vars";
import { scratchRequest } from "../../src/lib/util/request";

const info = (key: string, value: string, env = "Staging", secret = false): VariableInfo => ({
  key,
  value,
  secret,
  environmentId: env.toLowerCase(),
  environmentName: env,
});
const lookup = makeLookup([info("baseUrl", "https://api.dev"), info("token", "s3cr3t", "Staging", true), info("userId", "7", "Global")]);

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const EMAIL = /^[a-z]+\.[a-z]+@example\.(com|org|net)$/;
const ISO = /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z$/;

describe("dynamic variable generators", () => {
  const SHAPES: Record<string, RegExp> = {
    $guid: UUID,
    $uuid: UUID,
    $randomUUID: UUID,
    $timestamp: /^\d{10}$/,
    $isoTimestamp: ISO,
    $randomInt: /^\d{1,4}$/,
    $randomBoolean: /^(true|false)$/,
    $randomAlphaNumeric: /^[A-Za-z0-9]$/,
    $randomPassword: /^[A-Za-z0-9]{15}$/,
    $randomEmail: EMAIL,
    $randomUserName: /^[a-z]+\.[a-z]+\d{1,2}$/,
    $randomPhoneNumber: /^555-\d{3}-\d{4}$/,
    $randomCountryCode: /^[A-Z]{2}$/,
    $randomCurrencyCode: /^[A-Z]{3}$/,
    $randomCreditCardMask: /^\d{4}$/,
    $randomPrice: /^\d{1,4}\.\d{2}$/,
    $randomIP: /^\d{1,3}(\.\d{1,3}){3}$/,
    $randomIPV6: /^[0-9a-f]{1,4}(:[0-9a-f]{1,4}){7}$/,
    $randomMACAddress: /^[0-9a-f]{2}(:[0-9a-f]{2}){5}$/,
    $randomHexColor: /^#[0-9a-f]{6}$/,
    $randomUrl: /^https:\/\/[a-z]+-[a-z]+\.example\.(com|org|net)$/,
    $randomImageUrl: /^https:\/\/picsum\.photos\/seed\/\d+\/640\/480$/,
    $randomLatitude: /^-?\d{1,2}\.\d{6}$/,
    $randomLongitude: /^-?\d{1,3}\.\d{6}$/,
    $randomDateFuture: ISO,
    $randomDatePast: ISO,
    $randomDateRecent: ISO,
    $randomSemver: /^\d+\.\d+\.\d+$/,
    $randomLoremSentence: /^[A-Z][a-z ]+\.$/,
  };

  it("every catalogue entry generates a non-empty value matching its example's shape", () => {
    const cat = dynamicCatalogue();
    expect(cat.length).toBeGreaterThanOrEqual(40);
    for (const d of cat) {
      expect(d.name.startsWith("$")).toBe(true);
      expect(d.description && d.example).toBeTruthy();
      const shape = SHAPES[d.name];
      if (shape) expect(d.example, d.name).toMatch(shape);
      for (let i = 0; i < 30; i++) {
        const v = generateDynamic(d.name)!;
        expect(v, d.name).toBeTruthy();
        if (shape) expect(v, d.name).toMatch(shape);
      }
    }
    expect(generateDynamic("$nope")).toBeUndefined();
  });

  it("dates move in the right direction", () => {
    const now = Date.now();
    expect(Date.parse(generateDynamic("$randomDateFuture")!)).toBeGreaterThan(now);
    expect(Date.parse(generateDynamic("$randomDatePast")!)).toBeLessThan(now);
  });

  it("a memo returns one value per name", () => {
    const m = dynamicMemo();
    const a = m("$uuid");
    expect(m("$uuid")).toBe(a);
    expect(m("$guid")).not.toBe(a);
    expect(dynamicMemo()("$uuid")).not.toBe(a);
    expect(m("$nope")).toBeUndefined();
  });
});

describe("{{$dynamic}} tokenizing", () => {
  it("marks known dynamic variables with their own kind, unknown ones as unresolved", () => {
    const segs = tokenizeVars("{{baseUrl}}/u/{{ $uuid }}?x={{$nope}}", lookup);
    expect(segs.map((s) => [s.kind, s.text])).toEqual([
      ["var", "{{baseUrl}}"],
      ["text", "/u/"],
      ["dynamic", "{{ $uuid }}"],
      ["text", "?x="],
      ["unresolved", "{{$nope}}"],
    ]);
    const d = segs[2];
    expect(d.kind === "dynamic" && dynamicTip(d.dyn)).toBe("A v4 UUID · e.g. 3e6a4f26-8e0b-4b1c-9a35-0f1d7f0c2a9e · new value each send");
  });

  it("allows $ only as the first character", () => {
    expect(tokenizeVars("{{a$b}} {{$}} {{$$uuid}}", lookup).every((s) => s.kind === "text")).toBe(true);
  });

  it("dynamic names never count as unresolved", () => {
    expect(unresolvedNames(["{{$randomEmail}} {{missing}} {{$nope}}"], lookup)).toEqual(["missing", "$nope"]);
  });

  it("environment variable names may not start with $", () => {
    expect(isReservedVarName("$uuid")).toBe(true);
    expect(isReservedVarName(" $x")).toBe(true);
    expect(isReservedVarName("a$")).toBe(false);
  });
});

describe("{{ autocomplete source", () => {
  it("is closed outside an open {{", () => {
    expect(completeVar("https://x", "", lookup)).toBeNull();
    expect(completeVar("{{baseUrl}}", "", lookup)).toBeNull();
    expect(completeVar("{", "", lookup)).toBeNull();
  });

  it("lists environment variables first, then the $ catalogue", () => {
    const r = completeVar("https://{{", "", lookup)!;
    expect(r.from).toBe(10);
    expect(r.to).toBe(10);
    expect(r.options.slice(0, 3).map((o) => o.label)).toEqual(["baseUrl", "token", "userId"]);
    expect(r.options[0]).toMatchObject({ insert: "baseUrl}}", detail: "https://api.dev · Staging", kind: "var" });
    expect(r.options[1].detail).toBe("•••••••• · Staging");
    expect(r.options[3]).toMatchObject({ label: "$guid", kind: "dynamic", insert: "$guid}}" });
  });

  it("filters by prefix before substring; $ narrows to dynamic variables", () => {
    const r = completeVar("{{ $randomE", "", lookup)!;
    expect(r.from).toBe(3);
    expect(r.options.map((o) => o.label)).toEqual(["$randomEmail"]);
    const email = completeVar("{{email", "", lookup)!;
    expect(email.options.map((o) => o.label)).toContain("$randomEmail");
    const uu = completeVar("{{uu", "", lookup)!;
    expect(uu.options[0].label).toBe("$uuid");
    expect(completeVar("{{$", "", lookup)!.options.every((o) => o.kind === "dynamic")).toBe(true);
    expect(completeVar("{{zzz", "", lookup)).toBeNull();
  });

  it("replaces the rest of a name and closing braces already typed", () => {
    const r = completeVar("{{base", "Url}}/v1", lookup)!;
    expect(r.from).toBe(2);
    expect(r.to).toBe(11);
    expect(r.options[0].insert).toBe("baseUrl}}");
  });
});

describe("mock backend", () => {
  let server: MockServer;
  let call: ReturnType<typeof makeCall>;
  beforeEach(() => {
    server = new MockServer({ latency: 0, platform: "linux" });
    call = makeCall((cmd, args) => server.invoke(cmd, args));
  });

  it("get_dynamic_variables returns the catalogue", async () => {
    const cat = await call("get_dynamic_variables", {});
    expect(cat).toEqual(dynamicCatalogue());
    expect(cat.find((d) => d.name === "$randomEmail")).toEqual({ name: "$randomEmail", description: "Random email", example: "jane.doe@example.com" });
  });

  it("send generates one value per name; history shows what was sent", async () => {
    const request = scratchRequest({
      method: "POST",
      url: "https://api.example.com/v1/users?rid={{$uuid}}",
      body: { type: "json", text: '{"email":"{{$randomEmail}}","rid":"{{$uuid}}"}' },
    });
    const res = await call("send_request", { executionId: "d1", request, projectId: server.projects[0].id });
    expect(res.unresolvedVariables).toEqual([]);
    const rid = new URL(res.resolvedUrl).searchParams.get("rid")!;
    expect(rid).toMatch(UUID);
    const body = JSON.parse(new TextDecoder().decode(await call("get_response_body", { responseId: res.response!.responseId })));
    expect(body.email).toMatch(EMAIL);
    expect(body.rid).toBe(rid);
    const hist = await call("list_history", {});
    expect(hist[0].resolvedUrl).toBe(res.resolvedUrl);
  });

  it("export keeps {{$…}} literal with keepVariables, generates once without", async () => {
    const request = scratchRequest({ url: "https://x.dev/{{$uuid}}", headers: [{ key: "X-Id", value: "{{$uuid}}", enabled: true }] });
    const kept = await call("export_curl", { request, keepVariables: true });
    expect(kept).toContain("{{$uuid}}");
    const made = await call("export_curl", { request, keepVariables: false });
    const id = /x\.dev\/([0-9a-f-]{36})/.exec(made)![1];
    expect(id).toMatch(UUID);
    expect(made).toContain(`X-Id: ${id}`);
  });

  it("rejects environment variables named with a leading $", async () => {
    const g = server.environments.find((e) => e.projectId === null)!;
    const err = await call("update_environment", { environment: { ...g, variables: [{ key: "$uuid", value: "x", enabled: true, secret: false }] } }).catch((e: CommandError) => e);
    expect((err as CommandError).code).toBe("invalid");
  });
});

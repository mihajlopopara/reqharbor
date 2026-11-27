import { beforeEach, describe, expect, it } from "vitest";
import { setBackend } from "../../src/lib/api";
import { graphql as gqlStore } from "../../src/lib/stores/graphql.svelte";
import { EditorState } from "@codemirror/state";
import { CompletionContext, type CompletionResult } from "@codemirror/autocomplete";
import { MockServer } from "../../src/lib/api/mock";
import { MOCK_SDL, mockSchemaFile, readIntrospection } from "../../src/lib/api/mock/graphql";
import { makeCall, type CommandError, type GraphqlSchema, type Request } from "../../src/lib/api/types";
import { scratchRequest } from "../../src/lib/util/request";
import {
  argsSignature,
  hasComments,
  withOperation,
  graphqlErrorsLabel,
  graphqlErrorsOf,
  insertTextForField,
  jsonTopLevelKeys,
  keyCompletionAt,
  operationLabel,
  pickOperationName,
  rootTypes,
  scanOperations,
  searchSchema,
  validateVariables,
  variableCompletionText,
  type FieldModel,
  type VariableDef,
} from "../../src/lib/util/graphql";
import {
  GraphqlSyntaxError,
  buildFrom,
  prettifyQuery,
  queryCompletionSource,
  queryDiagnostics,
  valueChecker,
  variableDefinitions,
  variablesCompletionSource,
} from "../../src/lib/graphql/gql";

const sdl = (): Pick<GraphqlSchema, "format" | "schema"> => ({ format: "sdl", schema: MOCK_SDL });

describe("operation scanner (AC-GQL-9)", () => {
  it("finds named, anonymous and shorthand operations", () => {
    expect(scanOperations("{ me { id } }")).toEqual([{ type: "query", name: null, from: 0 }]);
    expect(scanOperations("query A { a } mutation B($x: Int) { b } subscription { c }").map((o) => [o.type, o.name])).toEqual([
      ["query", "A"],
      ["mutation", "B"],
      ["subscription", null],
    ]);
    expect(scanOperations("")).toEqual([]);
  });
  it("skips fragments, strings, comments and braces in headers", () => {
    const doc = `# query Hidden { x }
fragment F on User { id name }
query A($in: Input = {a: 1, b: "}{"}) @dir(arg: {x: 1}) {
  me { ...F note(text: """ query Fake { } """) }
}
query B { other(s: "query C { }") }`;
    expect(scanOperations(doc).map((o) => o.name)).toEqual(["A", "B"]);
  });
  it("works on half-typed documents", () => {
    expect(scanOperations("query A { me { na").map((o) => o.name)).toEqual(["A"]);
    expect(scanOperations("query A { a }\nquery B").map((o) => o.name)).toEqual(["A", "B"]);
  });
  it("keeps operationName only with several operations", () => {
    const two = scanOperations("query A { a } query B { b }");
    expect(pickOperationName(two, undefined)).toBe("A");
    expect(pickOperationName(two, "B")).toBe("B");
    expect(pickOperationName(two, "Gone")).toBe("A");
    expect(pickOperationName(scanOperations("query A { a }"), "A")).toBeUndefined();
    // Only named operations count (an anonymous one among several is invalid anyway).
    expect(pickOperationName(scanOperations("{ a } query B { b }"), undefined)).toBeUndefined();
    expect(pickOperationName(scanOperations("{ a } query B { b } query C { c }"), "C")).toBe("C");
  });
  it("normalises the request that is sent and exported (AC-GQL-9)", () => {
    const req = (query: string, operationName?: string) => ({ body: { type: "graphql", graphql: { query, variables: "", ...(operationName ? { operationName } : {}) } } });
    // Stored / synced / imported without a choice: the first named operation, like the picker.
    expect(withOperation(req("query A { a } query B { b }")).body.graphql?.operationName).toBe("A");
    // A stale name: with two operations the first, with one none at all.
    expect(withOperation(req("query A { a } query B { b }", "Gone")).body.graphql?.operationName).toBe("A");
    const single = withOperation(req("query A { a }", "Old"));
    expect(single.body.graphql).not.toHaveProperty("operationName");
    // Nothing to change: the same object, and the input is never mutated.
    const ok = req("query A { a } query B { b }", "B");
    expect(withOperation(ok)).toBe(ok);
    const stale = req("query A { a }", "Old");
    withOperation(stale);
    expect(stale.body.graphql.operationName).toBe("Old");
    const json = { body: { type: "json", text: "{}" } };
    expect(withOperation(json)).toBe(json);
  });
  it("detects comments outside strings (AC-GQL-7)", () => {
    expect(hasComments("{ a } # note")).toBe(true);
    expect(hasComments('{ a(s: "# not a comment") }')).toBe(false);
    expect(hasComments('{ a(s: """ # nor this """) }')).toBe(false);
    expect(hasComments("{ a }")).toBe(false);
  });
});

describe("response errors (AC-GQL-11)", () => {
  it("reads errors with path and location", () => {
    const body = JSON.stringify({ errors: [{ message: "Boom", path: ["user", 0, "name"], locations: [{ line: 3, column: 5 }] }, { message: "Two" }], data: null });
    expect(graphqlErrorsOf(body)).toEqual([
      { message: "Boom", path: "user.0.name", location: "3:5" },
      { message: "Two", path: null, location: null },
    ]);
    expect(graphqlErrorsLabel(1)).toBe("1 GraphQL error");
    expect(graphqlErrorsLabel(2)).toBe("2 GraphQL errors");
  });
  it("ignores anything else", () => {
    for (const t of [null, "", "not json", '{"data":{}}', '{"errors":[]}'.replace("[]", "null"), "[1,2]", '{"errors": "x"}']) expect(graphqlErrorsOf(t)).toEqual([]);
    expect(graphqlErrorsOf('{"errors":[]}')).toEqual([]);
  });
});

describe("variables JSON (AC-GQL-8)", () => {
  const defs: VariableDef[] = [
    { name: "id", type: "ID!", required: true, defaultValue: null },
    { name: "first", type: "Int", required: false, defaultValue: "10" },
  ];
  it("finds top-level keys and value ranges", () => {
    const t = '{ "id": "u1", "nested": {"id": 1}, "first": 5 }';
    const keys = jsonTopLevelKeys(t)!;
    expect(keys.map((k) => k.name)).toEqual(["id", "nested", "first"]);
    expect(t.slice(keys[0].from, keys[0].to)).toBe('"id"');
    expect(t.slice(keys[0].valueFrom, keys[0].valueTo)).toBe('"u1"');
    expect(t.slice(keys[1].valueFrom, keys[1].valueTo)).toBe('{"id": 1}');
    expect(t.slice(keys[2].valueFrom, keys[2].valueTo)).toBe("5");
    expect(jsonTopLevelKeys("[1]")).toBeNull();
    expect(jsonTopLevelKeys('{"a": ')!.map((k) => k.name)).toEqual(["a"]);
  });
  it("completes names at key positions only", () => {
    expect(keyCompletionAt("", 0)).toMatchObject({ from: 0, to: 0, wrap: true });
    expect(keyCompletionAt("{ }", 2)).toMatchObject({ from: 2, to: 2, prefix: "", wrap: false, hasColon: false });
    expect(keyCompletionAt('{"a": 1, }', 9)).toMatchObject({ from: 9, prefix: "" });
    expect(keyCompletionAt('{"i', 3)).toMatchObject({ from: 1, to: 3, prefix: "i" });
    expect(keyCompletionAt('{"id": 1}', 3)).toMatchObject({ from: 1, to: 5, prefix: "i", hasColon: true });
    expect(keyCompletionAt("{ fi", 4)).toMatchObject({ from: 2, to: 4, prefix: "fi" });
    expect(keyCompletionAt('{"a": 1', 7)).toBeNull(); // in a value
    expect(keyCompletionAt('{"a": {', 7)).toBeNull(); // nested object
    expect(keyCompletionAt('{"a": "x, y"', 10)).toBeNull(); // comma inside a string
    expect(variableCompletionText("id", { wrap: false, hasColon: false })).toBe('"id": ');
    expect(variableCompletionText("id", { wrap: true, hasColon: false })).toBe('{\n  "id": \n}');
    expect(variableCompletionText("id", { wrap: false, hasColon: true })).toBe('"id"');
  });
  it("reports undeclared, missing and mistyped variables", () => {
    const label = operationLabel("GetUser");
    expect(label).toBe('operation "GetUser"');
    expect(operationLabel(null)).toBe("the query");
    expect(validateVariables("", [], label)).toEqual([]);
    expect(validateVariables('{"id": "1"}', defs, label)).toEqual([]);
    const t = '{"first": 5, "extra": true}';
    const d = validateVariables(t, defs, label);
    expect(d).toHaveLength(2);
    expect(d[0]).toMatchObject({ severity: "warning", message: 'Variable "$extra" isn\'t defined by operation "GetUser"' });
    expect(t.slice(d[0].from, d[0].to)).toBe('"extra"');
    expect(d[1]).toMatchObject({ severity: "error", message: 'Missing required variable "$id" (ID!)', from: 0, to: 1 });
    expect(validateVariables("", defs, label).map((x) => x.message)).toEqual(['Missing required variable "$id" (ID!)']);
    // Type checks come from the schema-aware checker; {{templates}} resolve at send time.
    const check = (def: VariableDef, v: unknown) => (def.type === "Int" && typeof v !== "number" ? "Int cannot represent non-integer value" : null);
    const bad = '{"id": "1", "first": "x"}';
    const d2 = validateVariables(bad, defs, label, check);
    expect(d2).toHaveLength(1);
    expect(bad.slice(d2[0].from, d2[0].to)).toBe('"x"');
    expect(validateVariables('{"id": "1", "first": "{{n}}"}', defs, label, check)).toEqual([]);
    // Invalid JSON is left to the editor's own hint; a non-object is an error.
    expect(validateVariables("{oops", defs, label)).toEqual([]);
    expect(validateVariables("[1]", defs, label)[0].message).toBe("Variables must be a JSON object");
  });
});

describe("schema model helpers (AC-GQL-10)", () => {
  const built = buildFrom(sdl());
  const m = built.model;
  it("builds from SDL and from introspection JSON alike", () => {
    expect(built.problems).toEqual([]);
    const fromJson = buildFrom({ format: "introspection", schema: mockSchemaFile("schema.json") });
    expect(fromJson.model.types.map((t) => t.name)).toEqual(m.types.map((t) => t.name));
    expect(rootTypes(m)).toEqual([
      { operation: "query", type: "Query" },
      { operation: "mutation", type: "Mutation" },
    ]);
    expect(m.types.some((t) => t.name.startsWith("__"))).toBe(false);
    const user = m.byName.User;
    expect(user.kind).toBe("OBJECT");
    expect(user.interfaces).toEqual(["Node"]);
    expect(user.description).toBe("A person with an account.");
    const posts = user.fields.find((f) => f.name === "posts")!;
    expect(posts).toMatchObject({ type: "[Post!]!", namedType: "Post", composite: true });
    expect(posts.args[0]).toMatchObject({ name: "first", type: "Int", defaultValue: "10", required: false });
    expect(user.fields.find((f) => f.name === "username")!.deprecated).toBe("Use email");
    expect(m.byName.Role.enumValues.find((v) => v.name === "GUEST")!.deprecated).toBe("Use VIEWER");
    expect(m.byName.SearchResult.possibleTypes.sort()).toEqual(["Post", "User"]);
    expect(m.byName.CreateUserInput.fields.map((f) => f.name)).toEqual(["name", "email", "role"]);
    expect(() => buildFrom({ format: "sdl", schema: "type {" })).toThrow();
  });
  it("searches names: exact, prefix, then substring", () => {
    const hits = searchSchema(m, "user");
    expect(hits[0]).toMatchObject({ type: "User", field: null, label: "User" });
    expect(hits[1].label).toBe("Query.user");
    expect(hits.some((h) => h.label === "Query.user(id)") || hits.some((h) => h.label === "CreateUserInput")).toBe(true);
    expect(searchSchema(m, "ID").find((h) => h.arg === "id")?.label).toBe("Query.user(id)");
    expect(searchSchema(m, "admin")[0]).toMatchObject({ type: "Role", field: "ADMIN", detail: "enum value" });
    expect(searchSchema(m, "  ")).toEqual([]);
    expect(searchSchema(m, "zzz")).toEqual([]);
  });
  it("Insert text: required args, braces for objects, caret", () => {
    const q = m.byName.Query.fields;
    const f = (n: string) => q.find((x) => x.name === n) as FieldModel;
    expect(insertTextForField(f("failing"))).toEqual({ text: "failing", caret: 7 });
    expect(insertTextForField(f("me"))).toEqual({ text: "me {  }", caret: 5 });
    expect(insertTextForField(f("user"))).toEqual({ text: "user(id: ) {  }", caret: 9 });
    expect(insertTextForField(f("users"))).toEqual({ text: "users {  }", caret: 8 });
    expect(argsSignature(f("users").args)).toBe("(role: Role, first: Int = 20)");
  });
});

describe("documents and editor sources (AC-GQL-6/7/8)", () => {
  const { schema } = buildFrom(sdl());
  it("prettifies and reports syntax errors with a position", () => {
    expect(prettifyQuery("{me{name}}")).toBe("{\n  me {\n    name\n  }\n}");
    try {
      prettifyQuery("{\n  me(");
      throw new Error("expected a syntax error");
    } catch (e) {
      expect(e).toBeInstanceOf(GraphqlSyntaxError);
      expect((e as GraphqlSyntaxError).line).toBe(2);
      expect((e as GraphqlSyntaxError).column).toBe(6);
    }
  });
  it("reads the chosen operation's variable definitions", () => {
    const q = "query A($a: Int) { a } query B($id: ID!, $n: Int = 3, $list: [String!]!) { b }";
    expect(variableDefinitions(q, undefined)).toEqual([{ name: "a", type: "Int", required: false, defaultValue: null }]);
    expect(variableDefinitions(q, "B")).toEqual([
      { name: "id", type: "ID!", required: true, defaultValue: null },
      { name: "n", type: "Int", required: false, defaultValue: "3" },
      { name: "list", type: "[String!]!", required: true, defaultValue: null },
    ]);
    expect(variableDefinitions("{ a", undefined)).toBeNull();
  });
  it("checks values against the schema", () => {
    const check = valueChecker(schema);
    const def = (type: string): VariableDef => ({ name: "v", type, required: false, defaultValue: null });
    expect(check(def("ID!"), "u1")).toBeNull();
    expect(check(def("Int"), "x")).toMatch(/Int cannot represent/);
    expect(check(def("ID!"), null)).toMatch(/non-nullable/);
    expect(check(def("CreateUserInput!"), { name: "a", email: "b", role: "ADMIN" })).toBeNull();
    expect(check(def("CreateUserInput!"), { name: "a" })).toMatch(/email/);
    expect(check(def("Role"), "BOSS")).toMatch(/Role/);
    expect(check(def("Nope"), 1)).toBe('Unknown type "Nope"');
  });
  it("lints syntax without a schema, validation with one, never on {{variables}}", () => {
    expect(queryDiagnostics("{ me(", { schema: null, schemaValid: false })[0].message).toMatch(/^Syntax error/);
    expect(queryDiagnostics("{ me { nope } }", { schema: null, schemaValid: false })).toEqual([]);
    const d = queryDiagnostics("{ me { nope } }", { schema, schemaValid: true });
    expect(d).toHaveLength(1);
    expect("{ me { nope } }".slice(d[0].from, d[0].to)).toBe("nope");
    expect(d[0].severity).toBe("error");
    expect(queryDiagnostics('{ user(id: "{{userId}}") { name } }', { schema, schemaValid: true })).toEqual([]);
    expect(queryDiagnostics("   ", { schema, schemaValid: true })).toEqual([]);
  });
  it("completes fields from the schema and variable names in the variables editor", async () => {
    const qState = EditorState.create({ doc: "{ me { na" });
    const qSource = queryCompletionSource(() => ({ schema, schemaValid: true }));
    const r = (await qSource(new CompletionContext(qState, qState.doc.length, false))) as CompletionResult;
    expect(r.from).toBe(7);
    expect(r.options.map((o) => o.label)).toContain("name");
    expect(r.options.find((o) => o.label === "name")?.detail).toBe("String!");
    expect(await queryCompletionSource(() => ({ schema: null, schemaValid: false }))(new CompletionContext(qState, qState.doc.length, true))).toBeNull();
    // `{{` belongs to the environment-variable completion.
    const tpl = EditorState.create({ doc: '{ user(id: "{{us' });
    expect(await qSource(new CompletionContext(tpl, tpl.doc.length, false))).toBeNull();

    const defs = variableDefinitions("query B($id: ID!, $n: Int) { b }", "B")!;
    const vSource = variablesCompletionSource(() => ({ defs, label: "B", check: undefined }));
    const vState = EditorState.create({ doc: '{"n": 1, }' });
    const v = (await vSource(new CompletionContext(vState, 9, true))) as CompletionResult;
    expect(v.options.map((o) => o.label)).toEqual(["id"]); // n is already there
    expect(v.options[0].detail).toBe("ID!");
    const inValue = EditorState.create({ doc: '{"n": ' });
    expect(await vSource(new CompletionContext(inValue, inValue.doc.length, true))).toBeNull();
  });
});

describe("mock backend (AC-GQL-2..5, 9, 12)", () => {
  let server: MockServer;
  let call: ReturnType<typeof makeCall>;
  const gqlReq = (url: string, extra: Partial<Request> = {}): Request => ({
    ...scratchRequest({ method: "POST", url }),
    body: { type: "graphql", graphql: { query: "{ me { name } }", variables: "" } },
    ...extra,
  });
  async function rejects(p: Promise<unknown>): Promise<CommandError> {
    try {
      await p;
    } catch (e) {
      return e as CommandError;
    }
    throw new Error("expected rejection");
  }
  beforeEach(() => {
    server = new MockServer({ latency: 0, platform: "linux" });
    call = makeCall((cmd, args) => server.invoke(cmd, args));
  });

  it("fetches, caches per resolved URL and forgets", async () => {
    const r = gqlReq("https://api.example.com/graphql#frag");
    expect((await call("graphql_cached_schema", { request: r })).schema).toBeNull();
    const s = await call("graphql_fetch_schema", { request: r, executionId: "x1" });
    expect(s).toMatchObject({ url: "https://api.example.com/graphql", source: "introspection", format: "introspection" });
    expect(s.sizeBytes).toBe(s.schema.length);
    expect(buildFrom(s).model.byName.User).toBeTruthy();
    // Another request to the same URL shares it; history is untouched.
    const other = gqlReq("https://api.example.com/graphql");
    const hit = await call("graphql_cached_schema", { request: other });
    expect(hit).toMatchObject({ url: s.url, unchanged: false });
    expect(hit.schema?.fetchedAt).toBe(s.fetchedAt);
    // The schema already shown isn't sent again.
    expect(await call("graphql_cached_schema", { request: other, known: { url: s.url, fetchedAt: s.fetchedAt } })).toEqual({ url: s.url, unchanged: true, schema: null });
    expect((await call("list_history", {})).some((h) => h.resolvedUrl.includes("api.example.com/graphql"))).toBe(false);
    expect(await call("graphql_clear_schema", { request: other })).toBe(true);
    expect((await call("graphql_cached_schema", { request: r })).schema).toBeNull();
  });

  it("store: false caches nothing until graphql_store_schema; {{$dynamic}} stays literal in the key", async () => {
    const r = gqlReq("https://api.example.com/graphql?n={{$uuid}}");
    const s = await call("graphql_fetch_schema", { request: r, store: false });
    expect(s.url).toBe("https://api.example.com/graphql?n={{$uuid}}");
    expect((await call("graphql_cached_schema", { request: r })).schema).toBeNull();
    const f = await call("graphql_load_schema_file", { request: r, text: "type Query { a: Int }", fileName: "a.graphql", store: false });
    expect((await call("graphql_cached_schema", { request: r })).schema).toBeNull();
    // Fetched for another URL than the request has now (environment / host changed): not cached.
    const moved = { ...r, url: "https://prod.example.com/graphql" };
    expect((await call("graphql_store_schema", { request: moved, schema: f })).url).toBe(f.url);
    expect((await call("graphql_cached_schema", { request: moved })).schema).toBeNull();
    expect((await call("graphql_cached_schema", { request: r })).schema).toBeNull();
    await call("graphql_store_schema", { request: r, schema: f });
    expect((await call("graphql_cached_schema", { request: r })).schema).toMatchObject({ url: s.url, format: "sdl" });
    expect((await rejects(call("graphql_store_schema", { request: r, schema: { ...f, schema: " " } }))).code).toBe("graphql_schema_invalid");
  });

  it("a fetch is cancellable and never adds history", async () => {
    const slow = new MockServer({ latency: 50, platform: "linux" });
    const c = makeCall((cmd, args) => slow.invoke(cmd, args));
    const before = (await c("list_history", {})).length;
    const p = c("graphql_fetch_schema", { request: gqlReq("https://api.example.com/graphql"), executionId: "ex-1" });
    await new Promise((res) => setTimeout(res, 20));
    expect(await c("cancel_request", { executionId: "ex-1" })).toBe(true);
    expect(await rejects(p)).toMatchObject({ code: "graphql_network", details: { kind: "cancelled" } });
    await c("graphql_fetch_schema", { request: gqlReq("https://api.example.com/graphql") });
    expect((await c("list_history", {})).length).toBe(before);
  });

  it("answers request errors with 400 + errors, and a broken schema core accepts but graphql-js can't build", async () => {
    const bad = await call("send_request", { executionId: "e4", request: gqlReq("https://api.example.com/graphql", { body: { type: "graphql", graphql: { query: "{ me { nope } }", variables: "" } } }) });
    expect(bad.response?.status).toBe(400);
    const text = new TextDecoder().decode(await call("get_response_body", { responseId: bad.response!.responseId }));
    expect(graphqlErrorsOf(text)[0].message).toMatch(/Cannot query field "nope"/);
    const broken = gqlReq("https://api.example.com/graphql", { headers: [{ key: "X-Mock-Schema", value: "broken", enabled: true }] });
    const s = await call("graphql_fetch_schema", { request: broken, store: false });
    expect(() => buildFrom(s)).toThrow(/Missing/);
  });

  it("reports disabled introspection, failures and network errors", async () => {
    const disabled = await rejects(call("graphql_fetch_schema", { request: gqlReq("https://nointrospection.example.com/graphql") }));
    expect(disabled.code).toBe("graphql_introspection_disabled");
    expect((disabled.details as { serverMessage: string }).serverMessage).toMatch(/introspection is not allowed/);
    const notGql = await rejects(call("graphql_fetch_schema", { request: gqlReq("https://api.example.com/v1/users") }));
    expect(notGql.code).toBe("graphql_introspection_failed");
    expect(notGql.message).toMatch(/data\.__schema/);
    const dns = await rejects(call("graphql_fetch_schema", { request: gqlReq("https://nope.invalid/graphql") }));
    expect(dns).toMatchObject({ code: "graphql_network", details: { kind: "dns" } });
    expect((await rejects(call("graphql_fetch_schema", { request: gqlReq("  ") }))).code).toBe("validation");
    expect(readIntrospection(404, "Not Found", "nope")).toMatchObject({ ok: false, message: "The server answered 404 Not Found" });
  });

  it("loads schema files and refuses others", async () => {
    const r = gqlReq("https://api.example.com/graphql");
    const s = await call("graphql_load_schema_file", { request: r, text: "type Query { a: Int }", fileName: "a.graphql" });
    expect(s).toMatchObject({ source: "file", format: "sdl", fileName: "a.graphql", schema: "type Query { a: Int }" });
    const j = await call("graphql_load_schema_file", { request: r, path: "/tmp/schema.json" });
    expect(j).toMatchObject({ format: "introspection", fileName: "schema.json" });
    expect((await rejects(call("graphql_load_schema_file", { request: r, text: "hello", fileName: "x.txt" }))).code).toBe("graphql_schema_invalid");
    expect((await rejects(call("graphql_load_schema_file", { request: r, text: '{"a":1}', fileName: "x.json" }))).message).toMatch(/__schema/);
    expect((await call("graphql_cached_schema", { request: r })).schema?.format).toBe("introspection");
  });

  it("sends operationName and runs the chosen operation; cURL export keeps it", async () => {
    const r = gqlReq("https://api.example.com/graphql", {
      body: { type: "graphql", graphql: { query: "query A { me { name } } query B($id: ID!) { user(id: $id) { email } }", variables: '{"id":"u2"}', operationName: "B" } },
    });
    const res = await call("send_request", { executionId: "e1", request: r });
    const body = new TextDecoder().decode(await call("get_response_body", { responseId: res.response!.responseId }));
    expect(JSON.parse(body)).toEqual({ data: { user: { email: "grace@example.com" } } });
    const curl = await call("export_curl", { request: r, keepVariables: true });
    expect(curl).toContain('"operationName":"B"');
    // Unknown keys next to it survive a save (forward compat).
    const echo = await call("send_request", { executionId: "e2", request: { ...r, url: "https://x.dev/anything" } });
    const sent = JSON.parse(new TextDecoder().decode(await call("get_response_body", { responseId: echo.response!.responseId })));
    expect(sent.json).toMatchObject({ operationName: "B", variables: { id: "u2" } });
    // A failing field: 200 with errors.
    const failing = await call("send_request", { executionId: "e3", request: gqlReq("https://api.example.com/graphql", { body: { type: "graphql", graphql: { query: "{ failing }", variables: "" } } }) });
    expect(failing.response?.status).toBe(200);
    const ftext = new TextDecoder().decode(await call("get_response_body", { responseId: failing.response!.responseId }));
    expect(graphqlErrorsOf(ftext)).toEqual([{ message: "Something went wrong in the resolver", path: "failing", location: "1:3" }]);
  });
});

describe("schema store never replaces a good schema with a bad one (AC-GQL-3/5)", () => {
  const server = new MockServer({ latency: 0, platform: "linux" });
  const request = (id: string, headers: Request["headers"] = []): Request => ({
    ...scratchRequest({ method: "POST", url: "https://api.example.com/graphql" }),
    id,
    headers,
    body: { type: "graphql", graphql: { query: "{ me { name } }", variables: "" } },
  });
  beforeEach(() => {
    setBackend(server.backend());
    server.graphqlSchemas.clear();
  });

  it("good schema, then a bad SDL file: the good one stays shown and cached", async () => {
    const r = request("store-1");
    expect(await gqlStore.fetch(r, null)).toBe(true);
    const good = gqlStore.state("store-1");
    expect(good.status).toBe("ready");
    const fetchedAt = good.info!.fetchedAt;
    expect(await gqlStore.loadFrom(r, null, { text: "type Query { a: Missing }", fileName: "bad.graphql" })).toBe(false);
    const after = gqlStore.state("store-1");
    expect(after.status).toBe("ready");
    expect(after.info?.fetchedAt).toBe(fetchedAt);
    expect(after.error).toMatchObject({ kind: "invalid" });
    expect(after.error?.message).toMatch(/^This file isn't a GraphQL schema: /);
    expect(gqlStore.schemaOf("store-1")?.model.byName.User).toBeTruthy();
    const cached = await server.invoke("graphql_cached_schema", { request: r });
    expect((cached as { schema: GraphqlSchema }).schema.fetchedAt).toBe(fetchedAt);
  });

  it("good schema, then a fetched introspection graphql-js can't build: the good one stays", async () => {
    const r = request("store-2");
    expect(await gqlStore.fetch(r, null)).toBe(true);
    const fetchedAt = gqlStore.state("store-2").info!.fetchedAt;
    const broken = request("store-2", [{ key: "X-Mock-Schema", value: "broken", enabled: true }]);
    expect(await gqlStore.fetch(broken, null)).toBe(false);
    const after = gqlStore.state("store-2");
    expect(after.status).toBe("ready");
    expect(after.info?.fetchedAt).toBe(fetchedAt);
    expect(after.error?.message).toMatch(/^The server's schema can't be used: /);
    const cached = await server.invoke("graphql_cached_schema", { request: r });
    expect((cached as { schema: GraphqlSchema }).schema.fetchedAt).toBe(fetchedAt);
  });

  it("Cancel after the response arrived (while building) shows and stores nothing", async () => {
    const r = request("store-4");
    const real = server.backend();
    const seen: string[] = [];
    setBackend({
      ...real,
      invoke: async (cmd, args) => {
        seen.push(cmd);
        const res = await real.invoke(cmd, args);
        // The user hits Cancel just as the introspection answer lands.
        if (cmd === "graphql_fetch_schema") void gqlStore.cancelFetch("store-4");
        return res;
      },
    });
    expect(await gqlStore.fetch(r, null)).toBe(false);
    const st = gqlStore.state("store-4");
    expect(st.status).toBe("idle");
    expect(st.info).toBeNull();
    expect(st.error).toBeNull();
    expect(seen).not.toContain("graphql_store_schema");
    expect((await server.invoke("graphql_cached_schema", { request: r }) as { schema: unknown }).schema).toBeNull();
  });

  it("a cache lookup during a fetch runs after it, for the new URL", async () => {
    const r = request("store-3");
    const fetching = gqlStore.fetch(r, null);
    await gqlStore.refreshCached({ ...r, url: "https://other.example.com/graphql" }, null);
    expect(await fetching).toBe(true);
    await new Promise((res) => setTimeout(res, 20));
    expect(gqlStore.state("store-3").info).toBeNull();
  });
});

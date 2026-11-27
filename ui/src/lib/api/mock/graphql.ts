/**
 * Browser-mock GraphQL endpoint (any `…/graphql` URL): a small fake schema executed with
 * graphql-js, so introspection, autocomplete, operations and `errors` work in Playwright
 * (AC-GQL-*). Hosts containing `nointrospection` refuse introspection like Apollo Server; the header
 * `X-Mock-Schema: broken` makes introspection answer a schema graphql-js can't build.
 * Also mirrors core `graphql.rs`: the introspection request, reading its answer, schema files.
 */
import { buildSchema, getIntrospectionQuery, graphql as execute, introspectionFromSchema, printSchema, type GraphQLSchema } from "graphql";
import type { GraphqlErrorDetails, GraphqlSchema, Request } from "../types";
import { STATUS_TEXT, type FakeResponse, type PreparedRequest } from "./http";

export const MOCK_SDL = `"""An ISO-8601 date and time."""
scalar DateTime

"""Anything with a global id."""
interface Node {
  "Globally unique id"
  id: ID!
}

"""What a user may do in the workspace."""
enum Role {
  "Full access"
  ADMIN
  "Can edit"
  MEMBER
  "Read only"
  VIEWER
  GUEST @deprecated(reason: "Use VIEWER")
}

"""A person with an account."""
type User implements Node {
  id: ID!
  "Display name"
  name: String!
  email: String!
  role: Role!
  active: Boolean!
  "Posts written by the user, newest first"
  posts(first: Int = 10): [Post!]!
  createdAt: DateTime!
  username: String @deprecated(reason: "Use email")
}

"""A blog post."""
type Post implements Node {
  id: ID!
  title: String!
  body: String
  author: User!
  tags: [String!]!
}

"""A user or a post."""
union SearchResult = User | Post

"""Fields for a new user."""
input CreateUserInput {
  name: String!
  email: String!
  role: Role = MEMBER
}

type Query {
  "The signed-in user"
  me: User
  "One user by id"
  user(id: ID!): User
  "Users, optionally by role"
  users(role: Role, first: Int = 20): [User!]!
  "Full-text search over users and posts"
  search(term: String!): [SearchResult!]!
  "Always fails: shows how GraphQL errors look"
  failing: String
}

type Mutation {
  "Create a user"
  createUser(input: CreateUserInput!): User!
}
`;

interface UserRow {
  id: string;
  name: string;
  email: string;
  role: string;
  active: boolean;
  createdAt: string;
}

const USERS: UserRow[] = [
  { id: "u1", name: "Ada Lovelace", email: "ada@example.com", role: "ADMIN", active: true, createdAt: "2024-01-10T09:00:00Z" },
  { id: "u2", name: "Grace Hopper", email: "grace@example.com", role: "MEMBER", active: true, createdAt: "2024-02-01T12:30:00Z" },
  { id: "u3", name: "Alan Turing", email: "alan@example.com", role: "VIEWER", active: false, createdAt: "2024-03-15T16:45:00Z" },
];
const POSTS = [
  { id: "p1", title: "Notes on the Analytical Engine", body: "…", authorId: "u1", tags: ["history", "computing"] },
  { id: "p2", title: "The first compiler", body: null, authorId: "u2", tags: ["compilers"] },
];

let schemaCache: GraphQLSchema | null = null;
export function mockSchema(): GraphQLSchema {
  return (schemaCache ??= buildSchema(MOCK_SDL));
}

const user = (u: UserRow) => ({
  ...u,
  __typename: "User",
  username: u.email.split("@")[0],
  posts: ({ first }: { first: number }) => POSTS.filter((p) => p.authorId === u.id).slice(0, first).map(post),
});
const post = (p: (typeof POSTS)[number]) => ({ ...p, __typename: "Post", author: () => user(USERS.find((u) => u.id === p.authorId)!) });

const rootValue = {
  me: () => user(USERS[0]),
  user: ({ id }: { id: string }) => {
    const u = USERS.find((x) => x.id === id);
    return u ? user(u) : null;
  },
  users: ({ role, first }: { role?: string; first: number }) => USERS.filter((u) => !role || u.role === role).slice(0, first).map(user),
  search: ({ term }: { term: string }) => {
    const t = term.toLowerCase();
    return [...USERS.filter((u) => u.name.toLowerCase().includes(t)).map(user), ...POSTS.filter((p) => p.title.toLowerCase().includes(t)).map(post)];
  },
  failing: () => {
    throw new Error("Something went wrong in the resolver");
  },
  createUser: ({ input }: { input: { name: string; email: string; role: string } }) =>
    user({ id: `u${USERS.length + 1}`, active: true, createdAt: new Date(0).toISOString(), ...input }),
};

export function isGraphqlEndpoint(url: string): boolean {
  try {
    return /\/graphql\/?$/.test(new URL(url).pathname);
  } catch {
    return false;
  }
}

const enc = new TextEncoder();

function jsonResponse(status: number, value: unknown, base: Pick<FakeResponse, "latencyMs" | "finalUrl">): FakeResponse {
  const body = enc.encode(JSON.stringify(value));
  return {
    status,
    statusText: STATUS_TEXT[status] ?? "",
    httpVersion: "HTTP/2.0",
    headers: [
      ["content-type", "application/graphql-response+json; charset=utf-8"],
      ["content-length", String(body.length)],
      ["date", new Date().toUTCString()],
      ["server", "mock-harbor/1.0"],
    ],
    body,
    wireSize: body.length,
    contentEncoding: null,
    redirects: [],
    ...base,
  };
}

/** The fake endpoint's answer to a prepared POST. */
export async function graphqlResponse(req: PreparedRequest): Promise<FakeResponse> {
  const u = new URL(req.url);
  const base = { latencyMs: 30 + (req.bodyText?.length ?? 0) / 2000, finalUrl: req.url };
  let payload: { query?: unknown; variables?: unknown; operationName?: unknown } = {};
  try {
    payload = req.bodyText ? JSON.parse(req.bodyText) : {};
  } catch {
    return jsonResponse(400, { errors: [{ message: "Body is not valid JSON." }] }, base);
  }
  if (typeof payload.query !== "string" || !payload.query.trim()) {
    return jsonResponse(400, { errors: [{ message: "Must provide query string." }] }, base);
  }
  const broken = req.headers.some(([k, v]) => k.toLowerCase() === "x-mock-schema" && v === "broken");
  if (broken && /__schema/.test(payload.query)) {
    // Introspection core accepts but graphql-js can't build: a field of an undefined type.
    const intro = introspectionFromSchema(mockSchema());
    const broken = JSON.parse(JSON.stringify(intro)) as { __schema: { types: { name: string; fields?: { name: string; type: unknown }[] | null }[] } };
    const q = broken.__schema.types.find((t) => t.name === "Query")!;
    q.fields!.push({ ...q.fields![0], name: "ghost", type: { kind: "OBJECT", name: "Missing", ofType: null } });
    return jsonResponse(200, { data: broken }, base);
  }
  if (u.hostname.includes("nointrospection") && /__schema|__type\b/.test(payload.query)) {
    return jsonResponse(
      400,
      {
        errors: [
          {
            message:
              "GraphQL introspection is not allowed by Apollo Server, but the query contained __schema or __type. To enable introspection, pass introspection: true to ApolloServer in production",
            extensions: { code: "GRAPHQL_VALIDATION_FAILED" },
          },
        ],
      },
      base,
    );
  }
  const result = await execute({
    schema: mockSchema(),
    source: payload.query,
    rootValue,
    variableValues: payload.variables && typeof payload.variables === "object" ? (payload.variables as Record<string, unknown>) : undefined,
    operationName: typeof payload.operationName === "string" ? payload.operationName : undefined,
  });
  // GraphQL over HTTP (application/graphql-response+json): a request error (parse / validation,
  // no `data`) is a 4xx; field errors come with `data` and a 200.
  return jsonResponse("data" in result ? 200 : 400, JSON.parse(JSON.stringify(result)), base);
}

// ───────────── mirrors of core graphql.rs ─────────────

export const INTROSPECTION_OPERATION = "IntrospectionQuery";

/** Core `introspection_request`: POST, the introspection query, no explicit Content-Type. */
export function introspectionRequest(r: Request): Request {
  return {
    ...r,
    method: "POST",
    headers: r.headers.filter((h) => !(h.enabled && h.key.trim().toLowerCase() === "content-type")),
    body: { type: "graphql", graphql: { query: getIntrospectionQuery(), variables: "", operationName: INTROSPECTION_OPERATION } },
  };
}

export function schemaKey(displayUrl: string): string {
  const i = displayUrl.indexOf("#");
  return i < 0 ? displayUrl : displayUrl.slice(0, i);
}

export const SCHEMA_MAX_BYTES = 10 * 1024 * 1024;

export function isIntrospectionRefusal(message: string): boolean {
  const m = message.toLowerCase();
  return m.includes("introspection") || m.includes("__schema") || m.includes("__type");
}

function schemaObject(v: unknown): string | null {
  if (!v || typeof v !== "object") return null;
  const o = v as { data?: { __schema?: unknown }; __schema?: unknown };
  const s = (o.data && typeof o.data === "object" ? o.data.__schema : undefined) ?? o.__schema;
  if (!s || typeof s !== "object" || !Array.isArray((s as { types?: unknown }).types)) return null;
  return JSON.stringify({ __schema: s });
}

export type ReadResult =
  | { ok: true; schema: string }
  | { ok: false; code: "graphql_introspection_disabled" | "graphql_introspection_failed" | "graphql_schema_too_large"; message: string; details: GraphqlErrorDetails };

/** Core `read_introspection` (AC-GQL-3). */
export function readIntrospection(status: number, statusText: string, text: string): ReadResult {
  let parsed: unknown = undefined;
  try {
    parsed = JSON.parse(text);
  } catch {
    /* not JSON */
  }
  const schema = schemaObject(parsed);
  if (schema) {
    if (schema.length > SCHEMA_MAX_BYTES) return { ok: false, code: "graphql_schema_too_large", message: "The schema is larger than 10 MB", details: { sizeBytes: schema.length } };
    return { ok: true, schema };
  }
  const errors = parsed && typeof parsed === "object" && Array.isArray((parsed as { errors?: unknown }).errors) ? ((parsed as { errors: { message?: unknown }[] }).errors) : [];
  const messages = errors.map((e) => e?.message).filter((m): m is string => typeof m === "string");
  const refusal = messages.find(isIntrospectionRefusal);
  if (refusal) return { ok: false, code: "graphql_introspection_disabled", message: "Introspection is disabled on this server", details: { serverMessage: refusal } };
  const failed = (message: string): ReadResult => ({ ok: false, code: "graphql_introspection_failed", message, details: { status } });
  if (messages.length) return failed(messages[0]);
  if (status < 200 || status >= 300) return failed(`The server answered ${status}${statusText ? ` ${statusText}` : ""}`);
  return failed(parsed === undefined ? "The response isn't JSON: is this a GraphQL endpoint?" : "The response has no schema (data.__schema): is this a GraphQL endpoint?");
}

const SDL_KEYWORDS = new Set(["type", "schema", "scalar", "enum", "input", "interface", "union", "extend", "directive"]);

/** Core `load_schema_file` (AC-GQL-5). Throws `{ code, message }`. */
export function schemaFromText(text: string, fileName: string | null, url: string, now: number): GraphqlSchema {
  if (text.length > SCHEMA_MAX_BYTES) throw { code: "graphql_schema_too_large", message: "The schema is larger than 10 MB" };
  const trimmed = text.replace(/^﻿/, "").trim();
  const invalid = (why: string) => ({ code: "graphql_schema_invalid", message: `This file isn't a GraphQL schema: ${why}` });
  const base = { url, fetchedAt: now, source: "file" as const, ...(fileName ? { fileName } : {}) };
  if (trimmed.startsWith("{") || fileName?.toLowerCase().endsWith(".json")) {
    let v: unknown;
    try {
      v = JSON.parse(trimmed);
    } catch (e) {
      throw invalid(`invalid JSON (${(e as Error).message})`);
    }
    const schema = schemaObject(v);
    if (!schema) throw invalid("JSON without __schema: expected an introspection result");
    return { ...base, format: "introspection", schema, sizeBytes: schema.length };
  }
  if (!trimmed) throw invalid("the file is empty");
  if (!trimmed.split(/[^_0-9A-Za-z]/).some((w) => SDL_KEYWORDS.has(w))) throw invalid("no type definitions found (expected SDL or introspection JSON)");
  return { ...base, format: "sdl", schema: trimmed, sizeBytes: trimmed.length };
}

/** A "file" for mock paths: `.json` → introspection JSON of the fake schema, else its SDL. */
export function mockSchemaFile(path: string): string {
  if (/invalid/i.test(path)) return "hello world";
  return path.toLowerCase().endsWith(".json") ? JSON.stringify({ data: introspectionFromSchema(mockSchema()) }) : printSchema(mockSchema());
}

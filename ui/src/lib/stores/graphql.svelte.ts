/**
 * GraphQL schema state per open request (AC-GQL-2..5, 10): the cached / fetched / loaded
 * schema for the request's schema URL, its plain-data model for the explorer, and the
 * explorer's open state. The GraphQLSchema objects (graphql-js) are kept outside reactivity.
 *
 * A fetched or loaded schema is only cached after graphql-js has built it: the commands run
 * with `store: false`, then `graphql_store_schema` commits a good one, so a schema the editor
 * can't use never replaces the one already shown or cached.
 *
 * TODO(perf): building a very large schema (several MB of introspection JSON) blocks the main
 * thread for a moment. A Web Worker can't hand the built GraphQLSchema (class instances) back
 * to the editor's language service, so that needs the language service in the worker too.
 */
import { call, toCommandError, type GraphqlSchema, type Request } from "../api";
import { pickFile } from "../api/platform";
import { ulid } from "../util/ulid";
import type { SchemaModel } from "../util/graphql";
import type { BuiltSchema } from "../graphql/gql";

type Gql = typeof import("../graphql/gql");
let gqlModule: Promise<Gql> | null = null;
/** The lazy GraphQL chunk (graphql-js, language service, CodeMirror pieces). */
export function loadGql(): Promise<Gql> {
  return (gqlModule ??= import("../graphql/gql"));
}

export type SchemaErrorKind = "disabled" | "network" | "failed" | "invalid" | "blocked";

export interface SchemaError {
  kind: SchemaErrorKind;
  message: string;
  /** The server's own words, when it gave some. */
  detail: string | null;
}

export interface SchemaState {
  /** What the schema bar shows. */
  status: "idle" | "loading" | "fetching" | "ready";
  /** Cache / fetch metadata (without the large `schema` text). */
  info: Omit<GraphqlSchema, "schema"> | null;
  model: SchemaModel | null;
  typeCount: number;
  /** validateSchema problems: completion works, query validation is skipped. */
  problems: string[];
  error: SchemaError | null;
  executionId: string | null;
}

const EMPTY: SchemaState = { status: "idle", info: null, model: null, typeCount: 0, problems: [], error: null, executionId: null };

export const SCHEMA_FILE_FILTERS = [
  { name: "GraphQL schema", extensions: ["graphql", "graphqls", "gql", "sdl", "json"] },
];

/** UI copy for a failed fetch / load (AC-GQL-3, AC-GQL-5). */
export function schemaError(e: unknown): SchemaError | null {
  const err = toCommandError(e);
  const d = (err.details ?? {}) as { serverMessage?: string; kind?: string };
  switch (err.code) {
    case "graphql_introspection_disabled":
      return { kind: "disabled", message: "Introspection is disabled on this server.", detail: d.serverMessage ?? null };
    case "graphql_network":
      if (d.kind === "cancelled") return null;
      return { kind: "network", message: `Couldn't reach the server: ${err.message}`, detail: null };
    case "graphql_introspection_failed":
      return { kind: "failed", message: `Couldn't fetch the schema: ${err.message}`, detail: null };
    case "graphql_schema_invalid":
    case "graphql_schema_too_large":
      return { kind: "invalid", message: err.message, detail: null };
    case "validation":
    case "read_only_newer":
      return { kind: "blocked", message: err.message, detail: null };
    default:
      return { kind: "failed", message: err.message, detail: null };
  }
}

type Built = { ok: true; key: string; built: BuiltSchema } | { ok: false; message: string };

class GraphqlStore {
  /** requestId → state */
  states = $state<Record<string, SchemaState>>({});
  /** The explorer (docs pane) is open. */
  docsOpen = $state(false);
  /** Explorer navigation asked for from outside (Mod-click in the query). */
  docsTarget = $state<{ type: string; field: string | null; seq: number } | null>(null);
  /** url|fetchedAt|size → built schema (not reactive: graphql-js objects). */
  private built = new Map<string, BuiltSchema>();
  /** requestId → the key of the built schema in use. */
  private current = new Map<string, string>();
  /** requestId → serial of the latest cache lookup (stale answers are dropped). */
  private lookups = new Map<string, number>();
  /** requestId → a cache lookup asked for while a fetch ran (the URL changed meanwhile). */
  private deferred = new Map<string, { request: Request; projectId: string | null }>();

  state(requestId: string): SchemaState {
    return this.states[requestId] ?? EMPTY;
  }

  /** The graphql-js schema for a request, if one is ready. */
  schemaOf(requestId: string): BuiltSchema | null {
    const key = this.current.get(requestId);
    return (key && this.built.get(key)) || null;
  }

  private set(requestId: string, patch: Partial<SchemaState>) {
    this.states = { ...this.states, [requestId]: { ...this.state(requestId), ...patch } };
  }

  /** Build a payload with graphql-js (memoised); never touches the shown state. */
  private async build(payload: GraphqlSchema): Promise<Built> {
    const key = `${payload.url}|${payload.fetchedAt}|${payload.sizeBytes}`;
    const hit = this.built.get(key);
    if (hit) return { ok: true, key, built: hit };
    const gql = await loadGql();
    try {
      const built = gql.buildFrom(payload);
      // Keep a few: switching between requests to different endpoints stays instant.
      if (this.built.size >= 8) this.built.delete(this.built.keys().next().value!);
      this.built.set(key, built);
      return { ok: true, key, built };
    } catch (e) {
      return { ok: false, message: (e as Error).message || String(e) };
    }
  }

  private show(requestId: string, payload: GraphqlSchema, b: { key: string; built: BuiltSchema }) {
    this.current.set(requestId, b.key);
    const { schema: _text, ...info } = payload;
    this.set(requestId, { status: "ready", info, model: b.built.model, typeCount: b.built.model.types.length, problems: b.built.problems, error: null, executionId: null });
  }

  private clearSchema(requestId: string, patch: Partial<SchemaState> = {}) {
    this.current.delete(requestId);
    this.set(requestId, { status: "idle", info: null, model: null, typeCount: 0, problems: [], ...patch });
  }

  /** Keep what is shown, with an error under the bar. */
  private fail(requestId: string, error: SchemaError | null) {
    this.set(requestId, { status: this.state(requestId).info ? "ready" : "idle", error, executionId: null });
  }

  /** Load the cached schema for the request's schema URL (no network; no re-send of the shown one). */
  async refreshCached(request: Request, projectId: string | null) {
    const id = request.id;
    if (this.state(id).status === "fetching") {
      this.deferred.set(id, { request, projectId });
      return;
    }
    const serial = (this.lookups.get(id) ?? 0) + 1;
    this.lookups.set(id, serial);
    const info = this.state(id).info;
    try {
      const res = await call("graphql_cached_schema", { request, projectId, known: info ? { url: info.url, fetchedAt: info.fetchedAt } : null });
      if (this.lookups.get(id) !== serial || this.state(id).status === "fetching") return;
      if (res.unchanged) return;
      if (!res.schema) {
        // Keep a fetch / file error on screen; otherwise show "no schema".
        this.clearSchema(id, { error: this.state(id).error?.kind === "blocked" ? null : this.state(id).error });
        return;
      }
      const b = await this.build(res.schema);
      if (this.lookups.get(id) !== serial) return;
      if (b.ok) this.show(id, res.schema, b);
      else this.clearSchema(id, { error: { kind: "invalid", message: `The cached schema can't be used: ${b.message}`, detail: null } });
    } catch {
      // Not resolvable yet (empty URL, …): nothing cached to show.
      if (this.lookups.get(id) === serial) this.clearSchema(id, { error: null });
    }
  }

  /** AC-GQL-2: fetch by introspection (Fetch schema / Refresh); cached only once it builds. */
  async fetch(request: Request, projectId: string | null): Promise<boolean> {
    const id = request.id;
    const executionId = ulid();
    this.set(id, { status: "fetching", error: null, executionId });
    try {
      const s = await call("graphql_fetch_schema", { request, projectId, executionId, store: false });
      if (this.state(id).executionId !== executionId) return false;
      const b = await this.build(s);
      // Cancel may land after the response, while building: then nothing is shown or stored.
      if (this.state(id).executionId !== executionId) return false;
      if (!b.ok) {
        this.fail(id, { kind: "invalid", message: `The server's schema can't be used: ${b.message}`, detail: null });
        return false;
      }
      const stored = await call("graphql_store_schema", { request, projectId, schema: s }).catch(() => s);
      if (this.state(id).executionId !== executionId) return false;
      this.show(id, stored, b);
      return true;
    } catch (e) {
      if (this.state(id).executionId === executionId) this.fail(id, schemaError(e));
      return false;
    } finally {
      // The URL (or environment) changed while fetching: show what belongs to it now.
      const next = this.deferred.get(id);
      this.deferred.delete(id);
      if (next) void this.refreshCached(next.request, next.projectId);
    }
  }

  /** Stop a fetch at once: the previous schema (or none) is back, whatever arrives later is dropped. */
  async cancelFetch(requestId: string) {
    const ex = this.state(requestId).executionId;
    if (!ex) return;
    this.fail(requestId, null);
    await call("cancel_request", { executionId: ex }).catch(() => false);
  }

  /** AC-GQL-5: SDL or introspection JSON from a file. */
  async loadFile(request: Request, projectId: string | null): Promise<boolean> {
    const f = await pickFile({ title: "Load GraphQL schema", filters: SCHEMA_FILE_FILTERS, readText: true });
    if (!f) return false;
    return this.loadFrom(request, projectId, f.path.startsWith("browser://") ? { text: f.text ?? "", fileName: f.name } : { path: f.path, fileName: f.name });
  }

  /** Read, build, and only then cache: a file that isn't a usable schema changes nothing. */
  async loadFrom(request: Request, projectId: string | null, src: { path?: string; text?: string; fileName: string }): Promise<boolean> {
    const id = request.id;
    this.set(id, { status: "loading", error: null });
    try {
      const s = await call("graphql_load_schema_file", { request, projectId, path: src.path ?? null, text: src.text ?? null, fileName: src.fileName, store: false });
      const b = await this.build(s);
      if (!b.ok) {
        this.fail(id, { kind: "invalid", message: `This file isn't a GraphQL schema: ${b.message}`, detail: null });
        return false;
      }
      const stored = await call("graphql_store_schema", { request, projectId, schema: s });
      this.show(id, stored, b);
      return true;
    } catch (e) {
      this.fail(id, schemaError(e));
      return false;
    }
  }

  /** Forget the cached schema for the request's URL. */
  async forget(request: Request, projectId: string | null) {
    await call("graphql_clear_schema", { request, projectId }).catch(() => false);
    this.clearSchema(request.id, { error: null });
  }

  dismissError(requestId: string) {
    this.set(requestId, { error: null });
  }

  /** Open the explorer on a type (and field). */
  showInDocs(type: string, field: string | null) {
    this.docsOpen = true;
    this.docsTarget = { type, field, seq: (this.docsTarget?.seq ?? 0) + 1 };
  }
}

export const graphql = new GraphqlStore();

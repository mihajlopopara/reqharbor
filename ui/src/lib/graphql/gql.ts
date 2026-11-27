/**
 * GraphQL editor support (lazy chunk: graphql-js, graphql-language-service, cm6-graphql).
 * Builds schemas from GraphqlSchema payloads, prettifies queries, reads variable definitions,
 * and provides the CodeMirror pieces: query completion / hover / lint / Mod-click to docs
 * (AC-GQL-6), variables completion / lint (AC-GQL-8).
 */
import {
  GraphQLError,
  Kind,
  buildClientSchema,
  buildSchema,
  coerceInputValue,
  getNamedType,
  isCompositeType,
  isEnumType,
  isInputObjectType,
  isInputType,
  isInterfaceType,
  isNonNullType,
  isObjectType,
  isUnionType,
  parse,
  parseType,
  print,
  typeFromAST,
  validateSchema,
  type GraphQLArgument,
  type GraphQLField,
  type GraphQLInputField,
  type GraphQLNamedType,
  type GraphQLSchema,
  type OperationDefinitionNode,
} from "graphql";
import { getAutocompleteSuggestions, getDiagnostics, getHoverInformation, getTokenAtPosition, getTypeInfo } from "graphql-language-service";
import { offsetToPos } from "cm6-graphql";
import type { Extension } from "@codemirror/state";
import { EditorView, hoverTooltip } from "@codemirror/view";
import { forceLinting, linter, type Diagnostic as CmDiagnostic } from "@codemirror/lint";
import type { Completion, CompletionContext, CompletionResult, CompletionSource } from "@codemirror/autocomplete";
import type { GraphqlSchema } from "../api/types";
import { VAR_RE } from "../util/vars";
import {
  jsonTopLevelKeys,
  keyCompletionAt,
  validateVariables,
  variableCompletionText,
  type ArgModel,
  type FieldModel,
  type SchemaModel,
  type TypeKind,
  type TypeModel,
  type VariableDef,
} from "../util/graphql";

// ───────────── schemas ─────────────

export interface BuiltSchema {
  schema: GraphQLSchema;
  model: SchemaModel;
  /** validateSchema problems: completion still works, query validation is skipped. */
  problems: string[];
}

/** Build a schema from a fetched / loaded payload. Throws an Error with a readable message. */
export function buildFrom(s: Pick<GraphqlSchema, "format" | "schema">): BuiltSchema {
  let schema: GraphQLSchema;
  try {
    if (s.format === "sdl") schema = buildSchema(s.schema);
    else {
      const parsed = JSON.parse(s.schema) as { __schema?: unknown; data?: { __schema?: unknown } };
      const intro = parsed.__schema ? parsed : parsed.data;
      schema = buildClientSchema(intro as never);
    }
  } catch (e) {
    throw new Error((e as Error).message || String(e));
  }
  const problems = validateSchema(schema).map((e) => e.message);
  return { schema, model: modelOf(schema), problems };
}

const desc = (d: string | null | undefined): string | null => (d && d.trim() ? d.trim() : null);
const deprecation = (x: { deprecationReason?: string | null }): string | null =>
  x.deprecationReason === undefined || x.deprecationReason === null ? null : x.deprecationReason;

function argModel(a: GraphQLArgument | GraphQLInputField): ArgModel {
  const def = a.defaultValue === undefined ? null : printDefault(a);
  return {
    name: a.name,
    type: String(a.type),
    namedType: getNamedType(a.type).name,
    description: desc(a.description),
    defaultValue: def,
    required: isNonNullType(a.type) && a.defaultValue === undefined,
    deprecated: deprecation(a),
  };
}

function printDefault(a: GraphQLArgument | GraphQLInputField): string | null {
  const ast = a.astNode?.defaultValue;
  if (ast) return print(ast);
  try {
    return JSON.stringify(a.defaultValue);
  } catch {
    return null;
  }
}

function fieldModel(f: GraphQLField<unknown, unknown>): FieldModel {
  return {
    name: f.name,
    type: String(f.type),
    namedType: getNamedType(f.type).name,
    description: desc(f.description),
    args: f.args.map(argModel),
    deprecated: deprecation(f),
    composite: isCompositeType(getNamedType(f.type)),
  };
}

function kindOf(t: GraphQLNamedType): TypeKind {
  if (isObjectType(t)) return "OBJECT";
  if (isInterfaceType(t)) return "INTERFACE";
  if (isUnionType(t)) return "UNION";
  if (isEnumType(t)) return "ENUM";
  if (isInputObjectType(t)) return "INPUT_OBJECT";
  return "SCALAR";
}

/** Plain-data model of a schema for the explorer (AC-GQL-10). */
export function modelOf(schema: GraphQLSchema): SchemaModel {
  const types: TypeModel[] = [];
  for (const t of Object.values(schema.getTypeMap())) {
    if (t.name.startsWith("__")) continue;
    const m: TypeModel = {
      name: t.name,
      kind: kindOf(t),
      description: desc(t.description),
      fields: [],
      interfaces: [],
      possibleTypes: [],
      enumValues: [],
    };
    if (isObjectType(t) || isInterfaceType(t)) {
      m.fields = Object.values(t.getFields()).map(fieldModel);
      m.interfaces = t.getInterfaces().map((i) => i.name);
    }
    if (isInterfaceType(t) || isUnionType(t)) m.possibleTypes = schema.getPossibleTypes(t).map((p) => p.name);
    if (isInputObjectType(t)) {
      m.fields = Object.values(t.getFields()).map((f) => {
        const a = argModel(f);
        return { name: a.name, type: a.type, namedType: a.namedType, description: a.description, args: [], deprecated: a.deprecated, composite: false };
      });
    }
    if (isEnumType(t)) m.enumValues = t.getValues().map((v) => ({ name: v.name, description: desc(v.description), deprecated: deprecation(v) }));
    types.push(m);
  }
  types.sort((a, b) => a.name.localeCompare(b.name));
  return {
    queryType: schema.getQueryType()?.name ?? null,
    mutationType: schema.getMutationType()?.name ?? null,
    subscriptionType: schema.getSubscriptionType()?.name ?? null,
    types,
    byName: Object.fromEntries(types.map((t) => [t.name, t])),
  };
}

// ───────────── documents ─────────────

export class GraphqlSyntaxError extends Error {
  constructor(
    message: string,
    public line: number,
    public column: number,
  ) {
    super(message);
  }
}

/** AC-GQL-7: canonical GraphQL print (2-space indent; comments aren't kept). */
export function prettifyQuery(query: string): string {
  try {
    return print(parse(query));
  } catch (e) {
    const loc = e instanceof GraphQLError ? e.locations?.[0] : undefined;
    throw new GraphqlSyntaxError(e instanceof GraphQLError ? e.message.replace(/^Syntax Error: /, "") : String(e), loc?.line ?? 1, loc?.column ?? 1);
  }
}

/**
 * Variables declared by the chosen operation (AC-GQL-8): the one named `operationName`, else
 * the first. null when the document doesn't parse.
 */
export function variableDefinitions(query: string, operationName: string | undefined): VariableDef[] | null {
  let ops: OperationDefinitionNode[];
  try {
    ops = parse(query, { noLocation: true }).definitions.filter((d): d is OperationDefinitionNode => d.kind === Kind.OPERATION_DEFINITION);
  } catch {
    return null;
  }
  const op = (operationName && ops.find((o) => o.name?.value === operationName)) || ops[0];
  if (!op) return [];
  return (op.variableDefinitions ?? []).map((v) => ({
    name: v.variable.name.value,
    type: print(v.type),
    required: v.type.kind === Kind.NON_NULL_TYPE && !v.defaultValue,
    defaultValue: v.defaultValue ? print(v.defaultValue) : null,
  }));
}

/** Schema-aware check of one variable value: a message, or null when it fits (AC-GQL-8). */
export function valueChecker(schema: GraphQLSchema): (def: VariableDef, value: unknown) => string | null {
  return (def, value) => {
    let type;
    try {
      type = typeFromAST(schema, parseType(def.type) as never);
    } catch {
      return null;
    }
    if (!type) return `Unknown type "${def.type}"`;
    if (!isInputType(type)) return null;
    let problem: string | null = null;
    coerceInputValue(value, type, (path, _invalid, err) => {
      if (problem) return;
      const at = path.length ? ` at ${path.join(".")}` : "";
      problem = `${err.message}${at}`;
    });
    return problem;
  };
}

// ───────────── CodeMirror: query editor ─────────────

export interface QueryContext {
  schema: GraphQLSchema | null;
  /** validateSchema problems: then only syntax is checked. */
  schemaValid: boolean;
  /** Mod-click on a name: open it in the explorer. */
  onShowInDocs?: (typeName: string, fieldName: string | null) => void;
}

const SEVERITY = ["error", "warning", "info", "info"] as const;

/** Ranges of `{{var}}` in the text: GraphQL diagnostics there are noise (they resolve at send). */
function templateRanges(text: string): [number, number][] {
  const out: [number, number][] = [];
  for (const m of text.matchAll(VAR_RE)) out.push([m.index!, m.index! + m[0].length]);
  return out;
}

/** Query diagnostics (AC-GQL-6): syntax always, validation with a valid schema. */
export function queryDiagnostics(text: string, ctx: Pick<QueryContext, "schema" | "schemaValid">): CmDiagnostic[] {
  if (!text.trim()) return [];
  let results;
  try {
    results = getDiagnostics(text, ctx.schemaValid ? (ctx.schema ?? undefined) : undefined);
  } catch {
    results = getDiagnostics(text);
  }
  const templates = templateRanges(text);
  const out: CmDiagnostic[] = [];
  for (const d of results) {
    const from = Math.min(text.length, lineAt(text, d.range.start.line) + d.range.start.character);
    // The language service's end is one past the last character (cm6-graphql does the same - 1).
    let to = Math.min(text.length, lineAt(text, d.range.end.line) + d.range.end.character - 1);
    if (to <= from) to = Math.min(text.length, from + 1);
    if (templates.some(([a, b]) => from < b && to > a)) continue;
    out.push({ from, to: Math.max(from, to), severity: SEVERITY[(d.severity ?? 1) - 1] ?? "error", message: d.message.replace(/^Syntax Error: /, "Syntax error: ") });
  }
  return out;
}

/** Offset of the start of 0-based line `n`. */
function lineAt(text: string, n: number): number {
  let off = 0;
  for (let i = 0; i < n; i++) {
    const j = text.indexOf("\n", off);
    if (j < 0) return text.length;
    off = j + 1;
  }
  return off;
}

/** Query completion: fields, arguments, types, enum values, variables, directives (AC-GQL-6). */
export function queryCompletionSource(getCtx: () => QueryContext): CompletionSource {
  return (ctx: CompletionContext): CompletionResult | null => {
    const { schema } = getCtx();
    if (!schema) return null;
    const line = ctx.state.doc.lineAt(ctx.pos);
    if (/\{\{[^}]*$/.test(line.text.slice(0, ctx.pos - line.from))) return null; // {{var}} completion owns this
    const word = ctx.matchBefore(/[_A-Za-z0-9$@]*/);
    if (!word) return null;
    if (!ctx.explicit && word.from === word.to && !/[({:\s]$/.test(ctx.state.sliceDoc(Math.max(0, ctx.pos - 1), ctx.pos))) return null;
    let items;
    try {
      items = getAutocompleteSuggestions(schema, ctx.state.doc.toString(), offsetToPos(ctx.state.doc, ctx.pos));
    } catch {
      return null;
    }
    if (!items.length) return null;
    return {
      from: word.from,
      validFor: /^[_A-Za-z0-9$@]*$/,
      options: items.map((item): Completion => {
        const detail = item.detail || (item.type ? String(item.type) : "");
        const info = item.documentation || (item.isDeprecated ? `Deprecated${item.deprecationReason ? `: ${item.deprecationReason}` : ""}` : "");
        return {
          label: item.label,
          detail,
          type: "gql",
          boost: item.isDeprecated ? -1 : 0,
          info: info ? () => tipText(info) : undefined,
        };
      }),
    };
  };
}

function tipText(text: string): HTMLElement {
  const el = document.createElement("div");
  el.className = "cm-gql-doc";
  el.textContent = text;
  return el;
}

/** Hover docs on fields, arguments, types, enum values (AC-GQL-6). */
function queryHover(getCtx: () => QueryContext): Extension {
  return hoverTooltip((view, pos) => {
    const { schema } = getCtx();
    if (!schema) return null;
    const word = view.state.wordAt(pos);
    if (!word) return null;
    const { from, to } = word;
    let info = "";
    try {
      // Ask in the middle of the word: the language service reads the token under the cursor.
      info = String(getHoverInformation(schema, view.state.doc.toString(), offsetToPos(view.state.doc, Math.min(to, from + 1))) ?? "");
    } catch {
      return null;
    }
    if (!info.trim()) return null;
    return {
      pos: from,
      end: to,
      above: true,
      create() {
        const dom = document.createElement("div");
        dom.className = "cm-gql-tip";
        const [sig, ...rest] = info.split(/\n\n/);
        const s = document.createElement("div");
        s.className = "cm-gql-sig";
        s.textContent = sig.replace(/^```graphql\n?|```$/g, "").trim();
        dom.append(s);
        const body = rest.join("\n\n").trim();
        if (body) {
          const d = document.createElement("div");
          d.className = "cm-gql-doc";
          d.textContent = body;
          dom.append(d);
        }
        return { dom };
      },
    };
  });
}

/** Mod-click (⌘ / Ctrl) on a name opens it in the explorer. */
function queryJump(getCtx: () => QueryContext): Extension {
  return EditorView.domEventHandlers({
    click(e, view) {
      const ctx = getCtx();
      if (!ctx.schema || !ctx.onShowInDocs || !(e.metaKey || e.ctrlKey)) return false;
      const pos = view.posAtCoords({ x: e.clientX, y: e.clientY });
      if (pos === null) return false;
      try {
        const doc = view.state.doc;
        const token = getTokenAtPosition(doc.toString(), offsetToPos(doc, Math.min(doc.length, pos + 1)));
        const t = getTypeInfo(ctx.schema, token.state);
        if (t.fieldDef && t.parentType) ctx.onShowInDocs(getNamedType(t.parentType).name, t.fieldDef.name);
        else if (t.type) ctx.onShowInDocs(getNamedType(t.type).name, null);
        else return false;
        e.preventDefault();
        return true;
      } catch {
        return false;
      }
    },
  });
}

/** Lint + hover + Mod-click for the query editor; completion goes through `queryCompletionSource`. */
export function queryExtensions(getCtx: () => QueryContext): Extension {
  return [linter((view) => queryDiagnostics(view.state.doc.toString(), getCtx()), { delay: 300 }), queryHover(getCtx), queryJump(getCtx)];
}

// ───────────── CodeMirror: variables editor ─────────────

export interface VariablesContext {
  defs: VariableDef[] | null;
  /** "operation "GetUser"" / "the query" */
  label: string;
  check: ((def: VariableDef, value: unknown) => string | null) | undefined;
}

/** Completion of the operation's variable names at key positions (AC-GQL-8). */
export function variablesCompletionSource(getCtx: () => VariablesContext): CompletionSource {
  return (ctx: CompletionContext): CompletionResult | null => {
    const { defs } = getCtx();
    if (!defs?.length) return null;
    const text = ctx.state.doc.toString();
    const spot = keyCompletionAt(text, ctx.pos);
    if (!spot) return null;
    if (!ctx.explicit && !spot.wrap && spot.from === spot.to && !/[{,\s]$/.test(text.slice(0, ctx.pos))) return null;
    // Names already used, except the key being edited.
    const present = new Set((jsonTopLevelKeys(text) ?? []).filter((k) => k.from !== spot.from).map((k) => k.name));
    const options = [...defs]
      .filter((d) => !present.has(d.name))
      .sort((a, b) => Number(b.required) - Number(a.required))
      .map(
        (d, i): Completion => ({
          label: d.name,
          detail: d.type + (d.defaultValue !== null ? ` = ${d.defaultValue}` : ""),
          type: "gql",
          boost: 50 - i,
          apply: (view, _c, from, to) => {
            const insert = variableCompletionText(d.name, spot);
            const caret = spot.wrap ? insert.length - 2 : insert.length;
            view.dispatch({ changes: { from, to, insert }, selection: { anchor: from + caret } });
          },
        }),
      );
    if (!options.length) return null;
    return { from: spot.from, to: spot.to, options, filter: true };
  };
}

export function variablesExtensions(getCtx: () => VariablesContext): Extension {
  return linter(
    (view) => {
      const ctx = getCtx();
      if (!ctx.defs) return [];
      return validateVariables(view.state.doc.toString(), ctx.defs, ctx.label, ctx.check);
    },
    { delay: 300 },
  );
}

/** Re-run the linters of `view` now (the schema or the operation changed). */
export function relint(view: EditorView): void {
  forceLinting(view);
}

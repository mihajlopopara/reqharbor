/**
 * Message helpers driven by a GrpcDescriptor (AC-GRPC-6): the "Example message" generator and the
 * JSON completion model (which fields or enum values fit at the cursor). Pure; loaded with the
 * gRPC editor chunk.
 */
import type { GrpcDescriptor, GrpcFieldInfo, GrpcScalarKind } from "../api/types";

const WRAPPERS: Record<string, GrpcScalarKind> = {
  "google.protobuf.DoubleValue": "double",
  "google.protobuf.FloatValue": "float",
  "google.protobuf.Int64Value": "int64",
  "google.protobuf.UInt64Value": "uint64",
  "google.protobuf.Int32Value": "int32",
  "google.protobuf.UInt32Value": "uint32",
  "google.protobuf.BoolValue": "bool",
  "google.protobuf.StringValue": "string",
  "google.protobuf.BytesValue": "bytes",
};

const LONG: ReadonlySet<string> = new Set(["int64", "uint64", "sint64", "fixed64", "sfixed64"]);

/** Nested messages are filled up to this many levels below the root. */
export const EXAMPLE_DEPTH = 3;

function scalarSample(kind: GrpcScalarKind): unknown {
  if (kind === "bool") return false;
  if (kind === "string" || kind === "bytes") return "";
  if (LONG.has(kind)) return "0";
  return 0;
}

/** A sample for a well-known type in its JSON form; `undefined` = not a WKT; `OMIT` = leave the field out. */
const OMIT = Symbol("omit");
function wktSample(typeName: string, now: Date): unknown {
  if (typeName in WRAPPERS) return scalarSample(WRAPPERS[typeName]);
  switch (typeName) {
    case "google.protobuf.Timestamp":
      return now.toISOString();
    case "google.protobuf.Duration":
      return "1s";
    case "google.protobuf.Struct":
    case "google.protobuf.Empty":
      return {};
    case "google.protobuf.Value":
      return null;
    case "google.protobuf.ListValue":
      return [];
    case "google.protobuf.FieldMask":
      return "";
    case "google.protobuf.Any":
      return OMIT;
    default:
      return undefined;
  }
}

function mapKeySample(kind: GrpcScalarKind): string {
  if (kind === "string") return "key";
  if (kind === "bool") return "false";
  return "0";
}

/**
 * An example of message `typeName` (AC-GRPC-6): every field set (numbers 0, int64 "0", strings "",
 * bools false, the first enum value, one element for repeated fields and maps, nested messages
 * up to EXAMPLE_DEPTH levels), the first member of each oneof only, well-known types in JSON form,
 * Any and recursive types left out.
 */
export function exampleValue(d: GrpcDescriptor, typeName: string, now = new Date()): Record<string, unknown> {
  const build = (name: string, depth: number, stack: string[]): Record<string, unknown> => {
    const msg = d.messages[name];
    const out: Record<string, unknown> = {};
    if (!msg) return out;
    const oneofs = new Set<string>();
    for (const f of msg.fields) {
      if (f.oneof && oneofs.has(f.oneof)) continue;
      const v = sample(f, depth, [...stack, name]);
      if (v === OMIT) continue;
      if (f.oneof) oneofs.add(f.oneof);
      out[f.jsonName] = v;
    }
    return out;
  };
  const single = (f: GrpcFieldInfo, depth: number, stack: string[]): unknown => {
    if (f.kind === "enum") return d.enums[f.typeName ?? ""]?.values[0] ?? 0;
    if (f.kind !== "message") return scalarSample(f.kind);
    const t = f.typeName ?? "";
    const wkt = wktSample(t, now);
    if (wkt !== undefined) return wkt;
    if (stack.includes(t) || depth >= EXAMPLE_DEPTH || !d.messages[t]) return OMIT;
    return build(t, depth + 1, stack);
  };
  const sample = (f: GrpcFieldInfo, depth: number, stack: string[]): unknown => {
    const v = single(f, depth, stack);
    if (v === OMIT) return OMIT;
    if (f.mapKey) return { [mapKeySample(f.mapKey)]: v };
    if (f.repeated) return [v];
    return v;
  };
  return build(typeName, 0, []);
}

export function exampleMessage(d: GrpcDescriptor, typeName: string, now = new Date()): string {
  return JSON.stringify(exampleValue(d, typeName, now), null, 2);
}

// ───────────── completion ─────────────

/** "string", "repeated demo.Item", "map<string, int32>", "enum demo.Mood". */
export function fieldTypeText(f: GrpcFieldInfo): string {
  const base = f.kind === "message" || f.kind === "enum" ? (f.typeName ?? f.kind) : f.kind;
  if (f.mapKey) return `map<${f.mapKey}, ${base}>`;
  return `${f.repeated ? "repeated " : ""}${base}`;
}

export interface JsonCompletionOption {
  label: string;
  detail: string;
  /** Text that replaces from..to. */
  apply: string;
  type: "property" | "enum" | "keyword";
}

export interface JsonCompletion {
  from: number;
  to: number;
  options: JsonCompletionOption[];
}

interface Frame {
  type: "obj" | "arr";
  /** The message type of an object; null = untyped (Struct, unknown). */
  typeName: string | null;
  /** obj: a map field's value (keys are free, values of this field). */
  mapOf: GrpcFieldInfo | null;
  /** arr: the repeated field. */
  field: GrpcFieldInfo | null;
  key: string | null;
  expectKey: boolean;
  seen: Set<string>;
}

function fieldOf(d: GrpcDescriptor, typeName: string | null, key: string): GrpcFieldInfo | null {
  if (!typeName) return null;
  return d.messages[typeName]?.fields.find((f) => f.jsonName === key || f.name === key) ?? null;
}

/** The frame a value of `f` opens with `{`, or as an array element. */
function objectFrame(f: GrpcFieldInfo | null, asElement: boolean): Frame {
  const base: Frame = { type: "obj", typeName: null, mapOf: null, field: null, key: null, expectKey: true, seen: new Set() };
  if (!f) return base;
  if (f.mapKey && !asElement) return { ...base, mapOf: f };
  if (f.kind === "message" && f.typeName && !(f.typeName in WRAPPERS) && !f.typeName.startsWith("google.protobuf.")) return { ...base, typeName: f.typeName };
  return base;
}

/**
 * What fits at `pos` in the JSON `text` of message `typeName`: field names at key positions (keys
 * already present left out; nested messages, repeated elements and map values followed), enum
 * names and true / false at value positions. Null when nothing applies.
 */
export function jsonCompletions(d: GrpcDescriptor, typeName: string, text: string, pos: number): JsonCompletion | null {
  const stack: Frame[] = [];
  let inString = false;
  let strStart = -1;
  let buf = "";
  const top = () => stack[stack.length - 1];
  const fieldForValue = (fr: Frame): GrpcFieldInfo | null => {
    if (fr.type === "arr") return fr.field;
    if (fr.mapOf) return { ...fr.mapOf, mapKey: undefined, repeated: false };
    return fr.key !== null ? fieldOf(d, fr.typeName, fr.key) : null;
  };
  for (let i = 0; i < pos; i++) {
    const c = text[i];
    if (inString) {
      if (c === "\\") {
        buf += text[i + 1] ?? "";
        i++;
      } else if (c === '"') {
        inString = false;
        const fr = top();
        if (fr?.type === "obj" && fr.expectKey) {
          fr.key = buf;
          fr.seen.add(buf);
        }
      } else buf += c;
      continue;
    }
    switch (c) {
      case '"':
        inString = true;
        strStart = i;
        buf = "";
        break;
      case "{": {
        const fr = top();
        if (!fr) stack.push({ type: "obj", typeName, mapOf: null, field: null, key: null, expectKey: true, seen: new Set() });
        else if (fr.type === "arr") stack.push(objectFrame(fr.field, true));
        else stack.push(objectFrame(fieldForValue(fr), false));
        break;
      }
      case "[": {
        const fr = top();
        const f = fr ? fieldForValue(fr) : null;
        stack.push({ type: "arr", typeName: null, mapOf: null, field: f && f.repeated ? { ...f, repeated: false } : null, key: null, expectKey: false, seen: new Set() });
        break;
      }
      case "}":
      case "]":
        stack.pop();
        break;
      case ":": {
        const fr = top();
        if (fr?.type === "obj") fr.expectKey = false;
        break;
      }
      case ",": {
        const fr = top();
        if (fr?.type === "obj") {
          fr.expectKey = true;
          fr.key = null;
        }
        break;
      }
      default:
        break;
    }
  }
  const fr = top();
  if (!fr) return null;
  const after = text[pos] === '"' ? 1 : 0;

  // Key position.
  if (fr.type === "obj" && fr.expectKey) {
    if (fr.mapOf || !fr.typeName) return null;
    const msg = d.messages[fr.typeName];
    if (!msg) return null;
    let from: number;
    let prefix: string;
    if (inString) {
      from = strStart;
      prefix = buf;
    } else {
      const m = /[A-Za-z0-9_]*$/.exec(text.slice(0, pos));
      prefix = m ? m[0] : "";
      from = pos - prefix.length;
    }
    // The key being typed doesn't count as present.
    const seen = new Set(fr.seen);
    if (inString) seen.delete(buf);
    const takenOneofs = new Set(msg.fields.filter((f) => f.oneof && (seen.has(f.jsonName) || seen.has(f.name))).map((f) => f.oneof));
    const p = prefix.toLowerCase();
    const options = msg.fields
      .filter((f) => !seen.has(f.jsonName) && !seen.has(f.name))
      .filter((f) => !p || f.jsonName.toLowerCase().startsWith(p) || f.name.toLowerCase().startsWith(p))
      .map<JsonCompletionOption>((f) => ({
        label: f.jsonName,
        detail: `${fieldTypeText(f)}${f.oneof ? ` · oneof ${f.oneof}` : ""}${f.oneof && takenOneofs.has(f.oneof) ? " (another member is set)" : ""}`,
        apply: `"${f.jsonName}": `,
        type: "property",
      }));
    return options.length ? { from, to: pos + (inString ? after : 0), options } : null;
  }

  // Value position.
  const f = fieldForValue(fr);
  if (!f || (f.repeated && fr.type !== "arr") || (f.mapKey && !fr.mapOf)) return null;
  const opts: JsonCompletionOption[] = [];
  if (f.kind === "enum") {
    for (const v of d.enums[f.typeName ?? ""]?.values ?? []) opts.push({ label: v, detail: f.typeName ?? "enum", apply: `"${v}"`, type: "enum" });
  } else if (f.kind === "bool") {
    opts.push({ label: "true", detail: "bool", apply: "true", type: "keyword" }, { label: "false", detail: "bool", apply: "false", type: "keyword" });
  } else return null;
  let from: number;
  let prefix: string;
  if (inString) {
    from = strStart;
    prefix = buf;
  } else {
    const m = /[A-Za-z0-9_]*$/.exec(text.slice(0, pos));
    prefix = m ? m[0] : "";
    from = pos - prefix.length;
  }
  const p = prefix.toLowerCase();
  const options = opts.filter((o) => !p || o.label.toLowerCase().startsWith(p));
  return options.length ? { from, to: pos + (inString ? after : 0), options } : null;
}

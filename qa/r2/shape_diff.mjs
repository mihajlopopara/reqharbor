#!/usr/bin/env node
// QA round 2 — compare the JSON *shapes* returned by the real Tauri backend and by the browser mock
// for the same contract probe (qa/r2/contract_probe.js).
//   node qa/r2/shape_diff.mjs <real.json> <mock.json>
// Reports per probe label: success-vs-error mismatches, different error codes, missing/extra keys and
// type mismatches (null vs a value is reported separately as "nullability", usually benign).
import { readFileSync } from "node:fs";

const [realPath, mockPath] = process.argv.slice(2);
const real = JSON.parse(readFileSync(realPath, "utf8"));
const mock = JSON.parse(readFileSync(mockPath, "utf8"));

const kind = (v) => (v === null ? "null" : Array.isArray(v) ? "array" : typeof v);
const diffs = [];
// Free-form maps whose keys are data, not schema.
const FREEFORM = new Set(["details", "extra"]);

function cmp(a, b, path, label, out) {
  const ka = kind(a), kb = kind(b);
  if (ka !== kb) {
    if (ka === "null" || kb === "null" || ka === "undefined" || kb === "undefined") out.push({ label, path, type: "nullability", real: ka, mock: kb });
    else out.push({ label, path, type: "type", real: ka, mock: kb });
    return;
  }
  if (ka === "array") {
    if (a.length && b.length) {
      // tuples ([name, value]) and homogeneous arrays: compare first elements
      cmp(a[0], b[0], path + "[0]", label, out);
    }
    return;
  }
  if (ka === "object") {
    const last = path.split(".").pop();
    if (FREEFORM.has(last)) return;
    const keys = new Set([...Object.keys(a), ...Object.keys(b)]);
    for (const k of keys) {
      if (!(k in a)) out.push({ label, path: `${path}.${k}`, type: "missing-in-real", mock: kind(b[k]) });
      else if (!(k in b)) out.push({ label, path: `${path}.${k}`, type: "missing-in-mock", real: kind(a[k]) });
      else cmp(a[k], b[k], `${path}.${k}`, label, out);
    }
  }
}

const labels = new Set([...Object.keys(real), ...Object.keys(mock)]);
const rows = [];
for (const label of labels) {
  const r = real[label], m = mock[label];
  const out = [];
  if (r === undefined) out.push({ label, type: "not-run-on-real" });
  else if (m === undefined) out.push({ label, type: "not-run-on-mock" });
  else {
    const re = r && typeof r === "object" && "__error" in r, me = m && typeof m === "object" && "__error" in m;
    if (re !== me) out.push({ label, type: "outcome", real: re ? JSON.stringify(r.__error) : "ok", mock: me ? JSON.stringify(m.__error) : "ok" });
    else if (re && me) {
      if (r.__error.code !== m.__error.code || !!r.__error.__string !== !!m.__error.__string)
        out.push({ label, type: "error-code", real: r.__error.code ?? `string:${String(r.__error.__string).slice(0, 120)}`, mock: m.__error.code ?? `string:${String(m.__error.__string).slice(0, 120)}` });
      if (JSON.stringify(r.__error.details ? Object.keys(r.__error.details).sort() : null) !== JSON.stringify(m.__error.details ? Object.keys(m.__error.details).sort() : null))
        out.push({ label, type: "error-details", real: JSON.stringify(r.__error.details ?? null), mock: JSON.stringify(m.__error.details ?? null) });
    } else cmp(r, m, "$", label, out);
  }
  rows.push(...out);
}
const sev = (d) => (d.type === "nullability" ? 1 : d.type.startsWith("not-run") ? 1 : 2);
rows.sort((a, b) => sev(b) - sev(a) || a.label.localeCompare(b.label));
for (const d of rows) console.log(`${sev(d) === 2 ? "DIFF" : "info"}  ${d.label}  ${d.type}  ${d.path ?? ""}  real=${d.real ?? ""}  mock=${d.mock ?? ""}`);
console.log(`\nlabels: real=${Object.keys(real).length} mock=${Object.keys(mock).length}; differences: ${rows.filter((d) => sev(d) === 2).length} significant, ${rows.filter((d) => sev(d) === 1).length} informational`);

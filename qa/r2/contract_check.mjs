#!/usr/bin/env node
// QA round 2 — static IPC contract check (run in Docker: node qa/r2/contract_check.mjs).
// Compares, for every command:
//   (1) Rust #[tauri::command] fns in src-tauri/src/commands.rs + their registration in main.rs
//   (2) the `Commands` map in docs/types.ts and ui/src/lib/api/types.ts (arg names / optionality)
//   (3) the browser mock's handler table (ui/src/lib/api/mock/index.ts)
//   (4) every `call("<cmd>", {...})` site in ui/src (top-level arg keys)
//   (5) event names emitted by src-tauri vs `Events` in types.ts and `listen(...)` sites.
// Tauri converts JS camelCase arg keys to Rust snake_case params. Exit code = number of problems.
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const root = new URL("../../", import.meta.url).pathname;
const read = (p) => readFileSync(join(root, p), "utf8");
const camel = (s) => s.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());
const problems = [];
const note = (m) => problems.push(m);

// ---------- (1) Rust commands
const rs = read("src-tauri/src/commands.rs");
const rust = {};
const re = /#\[tauri::command\]\s*pub\s+(async\s+)?fn\s+(\w+)\s*\(([\s\S]*?)\)\s*(?:->\s*([^{]+))?\{/g;
for (const m of rs.matchAll(re)) {
  const [, isAsync, name, params, ret] = m;
  const args = {};
  // split top-level commas
  let depth = 0, cur = "";
  const parts = [];
  for (const ch of params) {
    if (ch === "<" || ch === "(") depth++;
    if (ch === ">" || ch === ")") depth--;
    if (ch === "," && depth === 0) { parts.push(cur); cur = ""; } else cur += ch;
  }
  if (cur.trim()) parts.push(cur);
  for (const p of parts) {
    const mm = p.trim().match(/^(\w+)\s*:\s*(.+)$/s);
    if (!mm) continue;
    const [, pn, ty] = mm;
    const t = ty.trim();
    if (/^(S<|State<|AppHandle|tauri::AppHandle|Window|WebviewWindow)/.test(t)) continue;
    args[camel(pn)] = { rustName: pn, type: t, optional: /^Option</.test(t) };
  }
  rust[name] = { async: !!isAsync, args, ret: (ret || "()").trim() };
}
const mainRs = read("src-tauri/src/main.rs");
const registered = new Set([...mainRs.matchAll(/commands::(\w+)/g)].map((m) => m[1]));

// ---------- (2) TS Commands map
function tsCommands(file) {
  const src = read(file);
  const start = src.indexOf("export interface Commands {");
  let i = src.indexOf("{", start), depth = 0, end = i;
  for (; end < src.length; end++) {
    if (src[end] === "{") depth++;
    if (src[end] === "}") { depth--; if (depth === 0) break; }
  }
  const body = src.slice(i + 1, end);
  const out = {};
  // each entry: name: { args: X; result: Y };
  const entryRe = /(\w+):\s*\{\s*args:\s*([\s\S]*?);\s*result:\s*([\s\S]*?)\s*\};/g;
  for (const m of body.matchAll(entryRe)) {
    const [, name, argsTxt, result] = m;
    const args = {};
    let a = argsTxt.trim();
    if (a === "SendRequestArgs") {
      const si = src.indexOf("export interface SendRequestArgs");
      a = src.slice(src.indexOf("{", si), src.indexOf("}", si) + 1).replace(/\/\*\*[\s\S]*?\*\//g, "");
    }
    for (const am of a.matchAll(/(\w+)(\?)?\s*:/g)) args[am[1]] = { optional: !!am[2] };
    out[name] = { args, result: result.trim().replace(/;$/, "") };
  }
  return out;
}
const tsDocs = tsCommands("docs/types.ts");
const tsUi = tsCommands("ui/src/lib/api/types.ts");

// ---------- (3) mock handlers
const mockSrc = read("ui/src/lib/api/mock/index.ts");
const hStart = mockSrc.indexOf("handlers: Handlers = {");
const mockNames = new Set();
if (hStart >= 0) {
  for (const m of mockSrc.slice(hStart).matchAll(/^    (\w+): (?:async )?\(/gm)) mockNames.add(m[1]);
}

// ---------- (4) UI call sites
function walk(d, acc = []) {
  for (const f of readdirSync(d)) {
    const p = join(d, f);
    if (statSync(p).isDirectory()) { if (f !== "mock" && f !== "node_modules") walk(p, acc); }
    else if (/\.(ts|svelte)$/.test(f)) acc.push(p);
  }
  return acc;
}
const sites = [];
for (const f of walk(join(root, "ui/src"))) {
  const src = readFileSync(f, "utf8");
  for (const m of src.matchAll(/\bcall\(\s*"(\w+)"\s*,\s*/g)) {
    // capture the balanced argument expression
    let i = m.index + m[0].length, depth = 0, j = i;
    for (; j < src.length; j++) {
      const ch = src[j];
      if ("({[".includes(ch)) depth++;
      if (")}]".includes(ch)) { if (depth === 0) break; depth--; }
      if (ch === "," && depth === 0) break;
    }
    const expr = src.slice(i, j).trim();
    let keys = null;
    if (expr.startsWith("{")) {
      keys = [];
      let d = 0, tok = "";
      const inner = expr.slice(1, -1);
      const top = [];
      for (const ch of inner) {
        if ("({[".includes(ch)) d++;
        if (")}]".includes(ch)) d--;
        if (ch === "," && d === 0) { top.push(tok); tok = ""; } else tok += ch;
      }
      if (tok.trim()) top.push(tok);
      for (const t of top) {
        const s = t.trim();
        if (!s) continue;
        if (s.startsWith("...")) { keys.push(s); continue; }
        const km = s.match(/^(\w+)\s*(:|$)/);
        if (km) keys.push(km[1]);
      }
    }
    const line = src.slice(0, m.index).split("\n").length;
    sites.push({ cmd: m[1], keys, expr, at: `${f.replace(root, "")}:${line}` });
  }
}

// ---------- compare
const rows = [];
const allNames = new Set([...Object.keys(rust), ...Object.keys(tsDocs), ...Object.keys(tsUi)]);
for (const name of [...allNames].sort()) {
  const r = rust[name], d = tsDocs[name], u = tsUi[name];
  const issues = [];
  if (!r) issues.push("no Rust #[tauri::command]");
  if (r && !registered.has(name)) issues.push("not registered in generate_handler!");
  if (!d) issues.push("missing in docs/types.ts");
  if (!u) issues.push("missing in ui types.ts");
  if (!mockNames.has(name)) issues.push("no mock handler");
  if (r && u) {
    for (const [k, v] of Object.entries(r.args)) {
      if (!u.args[k]) issues.push(`Rust arg ${v.rustName} (${k}) not in TS`);
      else if (u.args[k].optional && !v.optional) issues.push(`TS marks ${k} optional but Rust ${v.rustName}: ${v.type} is required`);
    }
    for (const k of Object.keys(u.args)) if (!r.args[k]) issues.push(`TS arg ${k} unknown to Rust (silently ignored)`);
  }
  if (d && u && JSON.stringify(d) !== JSON.stringify(u)) issues.push("docs/types.ts and ui types.ts differ");
  const callSites = sites.filter((s) => s.cmd === name);
  for (const s of callSites) {
    if (!s.keys) continue; // non-literal arg (typed by TS)
    for (const k of s.keys) {
      if (k.startsWith("...")) continue;
      if (r && !r.args[k]) issues.push(`call site ${s.at} passes unknown key ${k}`);
    }
    if (r) for (const [k, v] of Object.entries(r.args)) {
      if (!v.optional && !s.keys.includes(k) && !s.keys.some((x) => x.startsWith("..."))) issues.push(`call site ${s.at} omits required ${k}`);
    }
  }
  rows.push({ name, rustArgs: r ? Object.keys(r.args).join(", ") : "—", ret: r?.ret ?? "—", tsResult: u?.result ?? "—", sites: callSites.length, issues });
  for (const i of issues) note(`${name}: ${i}`);
}
for (const s of sites) if (!rust[s.cmd]) note(`call site ${s.at} calls unknown command ${s.cmd}`);

// ---------- (5) events
const rustEvents = new Set([...read("src-tauri/src/sync_rt.rs").matchAll(/"((?:sync|app)[^"]*:\/\/[^"]+)"/g)].map((m) => m[1]));
const tsEvents = new Set([...read("ui/src/lib/api/types.ts").matchAll(/"(\w+:\/\/[\w-]+)":/g)].map((m) => m[1]));
const listened = new Set();
for (const f of walk(join(root, "ui/src"))) for (const m of readFileSync(f, "utf8").matchAll(/listen\(\s*"([^"]+)"/g)) listened.add(m[1]);
for (const e of rustEvents) if (!tsEvents.has(e)) note(`event ${e} emitted by Rust but not in Events`);
for (const e of tsEvents) if (!rustEvents.has(e)) note(`event ${e} in Events but never emitted by Rust`);
for (const e of listened) if (!rustEvents.has(e)) note(`UI listens to ${e}, never emitted`);

if (process.argv.includes("--table")) {
  console.log("| command | Rust args (camelCase) | Rust return | TS result | UI call sites | issues |");
  console.log("|---|---|---|---|---|---|");
  for (const r of rows) console.log(`| \`${r.name}\` | ${r.rustArgs || "–"} | \`${r.ret.replace(/\|/g, "\\|")}\` | \`${r.tsResult.replace(/\|/g, "\\|")}\` | ${r.sites} | ${r.issues.join("; ") || "OK"} |`);
}
console.log(`\nRust commands: ${Object.keys(rust).length}, registered: ${registered.size}, TS commands: ${Object.keys(tsUi).length}, mock handlers matched: ${[...allNames].filter((n) => mockNames.has(n)).length}, UI call sites: ${sites.length}`);
console.log(`events: rust=${[...rustEvents].join(",")} ts=${[...tsEvents].join(",")} listened=${[...listened].join(",")}`);
console.log(problems.length ? `PROBLEMS (${problems.length}):\n  ` + problems.join("\n  ") : "No static contract problems found.");
process.exit(problems.length);

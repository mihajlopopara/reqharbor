// DR-002 check: every foreground token >= 4.5:1 on each surface, per theme block of tokens.css.
import fs from "node:fs";
const css = fs.readFileSync("/app/docs/design/tokens.css", "utf8");
const block = (sel) => { const i = css.indexOf(sel); const j = css.indexOf("}", i); return css.slice(i, j); };
const parse = (b) => Object.fromEntries([...b.matchAll(/--([\w-]+):\s*(#[0-9a-fA-F]{6})\b/g)].map((m) => [m[1], m[2]]));
const themes = { dark: parse(block(":root[data-theme=\"dark\"] {")), light: parse(block(":root[data-theme=\"light\"] {")), "os-light": parse(block(":root:not([data-theme=\"dark\"]) {")) };
const L = (h) => { const c = [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16) / 255).map((v) => (v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4)); return 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]; };
const cr = (a, b) => { const x = L(a), y = L(b); return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05); };
let fail = 0;
for (const [name, t] of Object.entries(themes)) {
  const fgs = Object.keys(t).filter((k) => /^(text-[123]$|method-|status-|syn-|var-(fg|unresolved-fg)|accent-fg|danger$|warning$|success$|info$)/.test(k));
  const surfaces = ["bg-pane", "bg-sidebar", "bg-input", "bg-raised", "bg-input-raised"].filter((s) => t[s]);
  let min = { r: 99 };
  for (const s of surfaces) for (const f of fgs) {
    const r = cr(t[f], t[s]);
    if (r < min.r) min = { r, f, s };
    if (r < 4.5) { fail++; console.log(`FAIL ${name}: --${f} ${t[f]} on --${s} ${t[s]} = ${r.toFixed(2)}`); }
  }
  for (const f of ["text-1", "text-2"]) { const r = cr(t[f], t["bg-seg-selected"]); if (r < 4.5) { fail++; console.log(`FAIL ${name}: --${f} on --bg-seg-selected = ${r.toFixed(2)}`); } else console.log(`${name}: --${f} on --bg-seg-selected ${r.toFixed(2)}`); }
  const step = (a, b) => cr(t[a], t[b]).toFixed(2);
  console.log(`${name}: ${fgs.length} fg x ${surfaces.length} surfaces, min ${min.r.toFixed(2)} (--${min.f} on --${min.s}); input-raised/raised ${step("bg-input-raised", "bg-raised")}, seg-selected/input-raised ${step("bg-seg-selected", "bg-input-raised")}, seg-selected/input ${step("bg-seg-selected", "bg-input")}`);
}
process.exit(fail ? 1 : 0);

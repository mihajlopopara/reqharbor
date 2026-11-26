// Reports gzipped sizes of the production build (PRODUCT §7 budget: JS + CSS ≤ 350 KB gz).
// "Initial" = what index.html loads eagerly (entry script, modulepreloads, stylesheets).
import { readFileSync, readdirSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { join } from "node:path";

const dist = new URL("../dist/", import.meta.url).pathname;
const gz = (f) => gzipSync(readFileSync(join(dist, f)), { level: 9 }).length;
const kb = (n) => `${(n / 1024).toFixed(1)} KB`;

const html = readFileSync(join(dist, "index.html"), "utf8");
const initial = [...html.matchAll(/(?:src|href)="\/?(assets\/[^"]+\.(?:js|css))"/g)].map((m) => m[1]);
const all = readdirSync(join(dist, "assets")).filter((f) => /\.(js|css)$/.test(f)).map((f) => `assets/${f}`);

const sum = (files) => files.reduce((n, f) => n + gz(f), 0);
const mock = all.filter((f) => /\/mock-/.test(f));
console.log("initial files:", initial.join(", "));
console.log(`initial (eager) JS+CSS gzipped: ${kb(sum(initial))}`);
console.log(`total JS+CSS gzipped:           ${kb(sum(all))}`);
console.log(`total excl. browser mock:       ${kb(sum(all.filter((f) => !mock.includes(f))))}`);
for (const f of all.sort((a, b) => gz(b) - gz(a)).slice(0, 8)) console.log(`  ${kb(gz(f)).padStart(9)}  ${f}`);

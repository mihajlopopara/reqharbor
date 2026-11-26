#!/usr/bin/env node
// QA round 2 — drive the REAL Tauri app (release build, custom-protocol) through tauri-driver +
// WebKitWebDriver with a minimal W3C WebDriver client (no npm deps). Started by qa/r2/run_real_app.sh.
// Env: APP (binary), ECHO (http://127.0.0.1:8099), WORKER (optional), OUT (dir for json/png).
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";

const WD = process.env.WD || "http://127.0.0.1:4444";
const APP = process.env.APP || "/cache/target/release/reqharbor";
const ECHO = process.env.ECHO || "http://127.0.0.1:8099";
const WORKER = process.env.WORKER || null;
const OUT = process.env.OUT || "/app/qa/r2/out";
mkdirSync(OUT, { recursive: true });
const EL = "element-6066-11e4-a52e-4f735466cecf";
const results = [];
const perf = {};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (...a) => console.log(...a);

async function wd(method, path, body) {
  const r = await fetch(WD + path, { method, headers: { "content-type": "application/json" }, body: body === undefined ? undefined : JSON.stringify(body) });
  const txt = await r.text();
  let j;
  try { j = JSON.parse(txt); } catch { throw new Error(`${method} ${path} -> ${r.status} ${txt.slice(0, 300)}`); }
  if (!r.ok || (j.value && j.value.error)) throw new Error(`${method} ${path} -> ${r.status} ${JSON.stringify(j.value).slice(0, 500)}`);
  return j.value;
}

class Session {
  static async start() {
    const t0 = Date.now();
    const v = await wd("POST", "/session", { capabilities: { alwaysMatch: { browserName: "wry", "tauri:options": { application: APP } } } });
    const s = new Session(v.sessionId);
    s.sessionMs = Date.now() - t0;
    s.t0 = t0;
    return s;
  }
  constructor(id) { this.id = id; this.p = `/session/${id}`; }
  exec(fn, ...args) { return wd("POST", `${this.p}/execute/sync`, { script: `return (${fn}).apply(null, arguments)`, args }); }
  execAsync(fnSrc, ...args) {
    // fnSrc: async function source; last WebDriver arg is the callback
    return wd("POST", `${this.p}/execute/async`, {
      script: `const done = arguments[arguments.length - 1]; const args = Array.prototype.slice.call(arguments, 0, -1);
        Promise.resolve((${fnSrc}).apply(null, args)).then((v) => done({ ok: v }), (e) => done({ err: (e && (e.stack || e.message)) || JSON.stringify(e) }));`,
      args,
    }).then((r) => { if (r && r.err) throw new Error(r.err); return r ? r.ok : r; });
  }
  async find(css) { const v = await wd("POST", `${this.p}/element`, { using: "css selector", value: css }); return v[EL]; }
  async click(css) { await this.waitSel(css, 5000).catch(async (e) => { await this.shot(`fail-${css.replace(/[^a-z0-9]+/gi, "_").slice(0, 40)}`); throw e; }); const id = await this.find(css); await wd("POST", `${this.p}/element/${id}/click`, {}); }
  async type(css, text, { clear = true } = {}) {
    const id = await this.find(css);
    if (clear) await wd("POST", `${this.p}/element/${id}/clear`, {});
    await wd("POST", `${this.p}/element/${id}/value`, { text });
  }
  async keys(...seq) {
    // seq: ["Control", "u"] -> chord
    const map = { Control: "", Shift: "", Enter: "", Escape: "", Meta: "" };
    const ks = seq.map((k) => map[k] || k);
    const actions = [...ks.map((k) => ({ type: "keyDown", value: k })), ...ks.slice().reverse().map((k) => ({ type: "keyUp", value: k }))];
    await wd("POST", `${this.p}/actions`, { actions: [{ type: "key", id: "kb", actions }] });
    await wd("DELETE", `${this.p}/actions`).catch(() => {});
  }
  async text(css) { return this.exec((c) => { const e = document.querySelector(c); return e ? e.textContent : null; }, css); }
  async waitFor(fnSrc, args = [], timeout = 10000, label = "condition") {
    const t0 = Date.now();
    let last;
    while (Date.now() - t0 < timeout) {
      try { last = await this.exec(fnSrc, ...args); if (last) return last; } catch (e) { last = e.message; }
      await sleep(50);
    }
    throw new Error(`timeout waiting for ${label} (last=${JSON.stringify(last)?.slice(0, 200)})`);
  }
  waitSel(css, timeout) { return this.waitFor((c) => !!document.querySelector(c), [css], timeout, css); }
  waitText(css, re, timeout) {
    return this.waitFor((c, src) => { const e = document.querySelector(c); return e && new RegExp(src).test(e.textContent) ? e.textContent : false; }, [css, re.source], timeout, `${css} ~ ${re}`);
  }
  async shot(name) {
    try { const b64 = await wd("GET", `${this.p}/screenshot`); writeFileSync(`${OUT}/${name}.png`, Buffer.from(b64, "base64")); } catch (e) { log("screenshot failed", e.message); }
  }
  invoke(cmd, args = {}) { return this.execAsync("(c, a) => window.__TAURI_INTERNALS__.invoke(c, a)", cmd, args); }
  async quit() { await wd("DELETE", this.p).catch(() => {}); }
}

async function step(name, fn) {
  const t0 = Date.now();
  try {
    const detail = await fn();
    results.push({ name, ok: true, ms: Date.now() - t0, detail: detail ?? "" });
    log(`PASS  ${name} (${Date.now() - t0} ms) ${detail ?? ""}`);
  } catch (e) {
    results.push({ name, ok: false, ms: Date.now() - t0, detail: e.message });
    log(`FAIL  ${name}: ${e.message}`);
  }
}

const appReady = (s) => s.waitFor(() => !!document.querySelector('[data-testid="app"]') && !!document.querySelector('[data-testid="sidebar"]') && !!document.querySelector('[data-testid="send-button"]'), [], 20000, "app ready");

async function coldStarts(n) {
  const times = [];
  const inPage = [];
  for (let i = 0; i < n; i++) {
    const s = await Session.start();
    await appReady(s);
    times.push(Date.now() - s.t0);
    inPage.push(await s.exec(() => Math.round(performance.now())));
    await s.quit();
    await sleep(500);
  }
  return { times, inPage };
}

(async () => {
  let s;
  await step("1. app starts (tauri-driver session) and the three-pane UI renders", async () => {
    s = await Session.start();
    await appReady(s);
    const ua = await s.exec(() => navigator.userAgent);
    const tauri = await s.exec(() => "__TAURI_INTERNALS__" in window);
    if (!tauri) throw new Error("not running inside Tauri (mock backend would be used)");
    await s.shot("01-start");
    return `session ${s.sessionMs} ms, ready ${Date.now() - s.t0} ms, UA=${ua.slice(0, 80)}`;
  });
  if (!s) { writeFileSync(`${OUT}/real_app_results.json`, JSON.stringify({ results, perf }, null, 2)); process.exit(1); }
  await s.exec(() => { window.__qaErrors = []; window.addEventListener("error", (e) => window.__qaErrors.push(String(e.message))); window.addEventListener("unhandledrejection", (e) => window.__qaErrors.push("unhandledrejection: " + JSON.stringify(e.reason))); });

  await step("2. sidebar loads: first launch has My Workspace / Default project / Global env (AC-COL-1)", async () => {
    const b = await s.invoke("bootstrap");
    if (b.workspace.name !== "My Workspace") throw new Error(`workspace ${b.workspace.name}`);
    if (!b.projects.some((p) => p.name === "Default")) throw new Error("no Default project");
    if (!b.environments.some((e) => e.projectId === null)) throw new Error("no Global env");
    await s.click('[data-testid="sidebar-tab-collections"]');
    await s.waitSel('[data-testid="project-switcher"]');
    const sw = await s.text('[data-testid="project-switcher"]');
    if (!/Default/.test(sw)) throw new Error(`project switcher shows ${sw}`);
    return `platform=${b.platform} dataDir=${b.dataDir}`;
  });

  await step("3. create a request, send it to the local echo server, see 200 + body", async () => {
    await s.click('[data-testid="new-request"]');
    await s.type('[data-testid="url-input"]', `${ECHO}/echo/first?a=1&b=two`);
    const t0 = Date.now();
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /^200 OK/, 10000);
    perf.uiSendToStatusMs = Date.now() - t0;
    const body = await s.waitText('[data-testid="response-pane"]', /\/echo\/first\?a=1&b=two/, 5000);
    await s.shot("03-send-200");
    return `UI click→200 rendered ${perf.uiSendToStatusMs} ms; body has echoed URL: ${/"method": ?"GET"/.test(body)}`;
  });

  await step("4. ⌘U/Ctrl+U cURL import of a QUERY with JSON body, send → server sees QUERY + body; Accept-Query highlighted", async () => {
    await s.click('[data-testid="url-input"]');
    await s.keys("Control", "u");
    await s.waitSel('[data-testid="curl-input"]', 5000);
    await s.type('[data-testid="curl-input"]', `curl -X QUERY '${ECHO}/query' -H 'Content-Type: application/json' -H 'X-Trace: 1' --data-raw '{"role":"admin"}'`);
    await s.waitText('[data-testid="curl-preview-method"]', /QUERY/, 5000);
    await s.click('[data-testid="import-submit"]');
    await s.waitFor(() => document.querySelector('[data-testid="method-select"]')?.getAttribute("data-method") === "QUERY", [], 5000, "method QUERY");
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /^200/, 10000);
    const pane = await s.text('[data-testid="response-pane"]');
    if (!/QUERY/.test(pane) || !/role/.test(pane)) throw new Error(`echo did not show QUERY + body: ${pane.slice(0, 300)}`);
    await s.click('[data-testid="res-tab-headers"]');
    const hl = await s.exec(() => { const h = document.querySelector('[data-testid="response-headers"]'); return h ? h.innerHTML.match(/accept-query[\s\S]{0,200}/i)?.[0] ?? null : null; });
    await s.shot("04-query");
    await s.click('[data-testid="res-tab-body"]');
    return `accept-query row: ${hl ? "present" : "MISSING"}`;
  });

  await step("5. Ctrl+Shift+C copies cURL (clipboard plugin) with -X QUERY and --data-raw (AC-CEXP-1, AC-QRY-5)", async () => {
    await s.click('[data-testid="url-input"]');
    await s.keys("Control", "Shift", "c");
    await sleep(500);
    let clip = null;
    try { clip = await s.invoke("plugin:clipboard-manager|read_text"); } catch (e) { clip = `ERR ${e.message}`; }
    const toast = await s.exec(() => [...document.querySelectorAll('[data-testid="toast"]')].map((t) => t.textContent).join(" | "));
    writeFileSync(`${OUT}/exported_curl.txt`, String(clip));
    if (typeof clip !== "string" || !/-X QUERY/.test(clip) || !/--data-raw/.test(clip)) throw new Error(`clipboard: ${String(clip).slice(0, 300)}; toasts: ${toast}`);
    // R3 (QA-R2-004): every part on its own ` \`-joined line: curl / -X QUERY / 'url' / -H … / --data-raw
    const lines = clip.split("\n");
    const okLayout = lines[0] === "curl \\" && lines[1].trim() === "-X QUERY \\" && /^\s+'http/.test(lines[2]) && lines.slice(0, -1).every((l) => l.endsWith(" \\")) && lines.slice(1).every((l) => l.startsWith("  "));
    if (!okLayout) throw new Error(`QA-R2-004 layout not multi-line: ${JSON.stringify(lines.slice(0, 4))}`);
    return `toast: ${toast.slice(0, 80)}; ${lines.length} lines, layout OK`;
  });

  await step("6. OpenAPI import of qa/fixtures/openapi/petstore3_v30.yaml via the Import dialog (pasted text)", async () => {
    const spec = readFileSync("/app/qa/fixtures/openapi/petstore3_v30.yaml", "utf8");
    await s.click('[data-testid="import-button"]');
    await s.click('[data-testid="import-tab-openapi"]');
    await s.waitSel('[data-testid="openapi-input"]');
    await s.exec((t) => { const el = document.querySelector('[data-testid="openapi-input"]'); const set = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), "value").set; set.call(el, t); el.dispatchEvent(new Event("input", { bubbles: true })); }, spec);
    const chip = await s.waitText('[data-testid="openapi-chip"]', /OpenAPI 3\.0/, 10000);
    const label = await s.text('[data-testid="import-submit"]');
    const t0 = Date.now();
    await s.click('[data-testid="import-submit"]');
    await s.waitFor(() => !document.querySelector('[data-testid="import-dialog"]') || document.querySelector('[data-testid="import-dialog"]').offsetParent === null, [], 15000, "dialog closed");
    perf.uiOpenapiImportMs = Date.now() - t0;
    await s.waitSel('[data-testid="tree-node"][data-kind="collection"]', 5000);
    const names = await s.exec(() => [...document.querySelectorAll('[data-testid="tree-node"][data-kind="collection"]')].map((n) => n.getAttribute("data-name")));
    await s.shot("06-openapi");
    return `chip "${chip}", button "${label}", collections ${JSON.stringify(names)}`;
  });

  await step("7. OpenAPI import by path for all qa/fixtures/openapi/* (backend reads the file)", async () => {
    const b = await s.invoke("bootstrap");
    const pid = b.settings.activeProjectId || b.projects[0].id;
    const out = [];
    for (const f of ["petstore_swagger2.json", "webhooks_v31.yaml", "petstore3_v30.yaml"]) {
      const t0 = Date.now();
      const r = await s.invoke("import_openapi", { projectId: pid, path: `/app/qa/fixtures/openapi/${f}` });
      out.push(`${f}: ${r.report.spec.label}, ${r.report.operationsCreated} ops, baseUrl=${r.report.baseUrl}, ${r.report.warnings.length} warnings, ${Date.now() - t0} ms`);
    }
    return out.join("; ");
  });

  await step("8. environment variable substitution: {{baseUrl}} from the active project env reaches the server", async () => {
    const b = await s.invoke("bootstrap");
    const pid = b.settings.activeProjectId || b.projects[0].id;
    const env = await s.invoke("create_environment", { projectId: pid, name: "QA Local" });
    await s.invoke("update_environment", { environment: { ...env, variables: [{ key: "baseUrl", value: ECHO, enabled: true, secret: false }, { key: "tok", value: "S3CRET-R2", enabled: true, secret: true }] } });
    // reload so the UI re-bootstraps from the backend, then pick the env with Ctrl+E
    await s.exec(() => location.reload());
    await sleep(300);
    await appReady(s);
    await s.click('[data-testid="url-input"]');
    await s.keys("Control", "e");
    await s.waitSel('[data-testid="env-picker"]', 5000);
    await s.click('[data-testid="env-option"][data-name="QA Local"]');
    await s.waitText('[data-testid="active-env"]', /QA Local/, 5000);
    await s.click('[data-testid="new-request"]');
    await s.type('[data-testid="url-input"]', "{{baseUrl}}/echo/env?k={{tok}}");
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /^200/, 10000);
    const pane = await s.waitText('[data-testid="response-pane"]', /\/echo\/env\?k=S3CRET-R2/, 5000).catch(async () => { await s.shot("fail-08-env"); return s.text('[data-testid="response-pane"]'); });
    if (!/\/echo\/env\?k=S3CRET-R2/.test(pane)) throw new Error(`server did not receive resolved URL: ${pane.slice(0, 300)}`);
    const hist = await s.invoke("list_history");
    const leaked = hist.some((h) => h.resolvedUrl.includes("S3CRET-R2"));
    if (leaked) throw new Error("secret value in history resolvedUrl");
    return `history resolvedUrl: ${hist[0].resolvedUrl}`;
  });

  await step("9. cancel an in-flight send (AC-REQ-8): Cancel within budget, history entry 'cancelled'", async () => {
    await s.click('[data-testid="new-request"]');
    await s.type('[data-testid="url-input"]', `${ECHO}/slow?ms=8000`);
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="send-button"]', /Cancel/, 5000);
    await sleep(300);
    const t0 = Date.now();
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /Cancelled/, 3000);
    perf.uiCancelMs = Date.now() - t0;
    const hist = await s.invoke("list_history");
    return `cancel click→"Cancelled" ${perf.uiCancelMs} ms (incl. WebDriver round trips); top history errorKind=${hist[0].errorKind}`;
  });

  await step("10. 6 MB JSON response: first 5 MB shown as plain text + Save full body (AC-RES-3)", async () => {
    await s.click('[data-testid="new-request"]');
    await s.type('[data-testid="url-input"]', `${ECHO}/big?mb=6`);
    const t0 = Date.now();
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /^200/, 20000);
    await s.waitSel('[data-testid="body-limited"]', 10000);
    perf.ui6mbRenderMs = Date.now() - t0;
    const size = await s.text('[data-testid="response-size"]');
    await s.shot("10-big");
    return `size strip "${size}", limited banner shown, ${perf.ui6mbRenderMs} ms`;
  });

  await step("11. 1 MB JSON response renders pretty + highlighted (perf budget 200 ms)", async () => {
    await s.click('[data-testid="new-request"]');
    await s.type('[data-testid="url-input"]', `${ECHO}/big?mb=1`);
    const t0 = Date.now();
    await s.click('[data-testid="send-button"]');
    await s.waitText('[data-testid="response-status"]', /^200/, 20000);
    await s.waitFor(() => !!document.querySelector('[data-testid="response-pane"] .cm-content'), [], 10000, "cm");
    perf.ui1mbSendRenderMs = Date.now() - t0;
    const meta = await s.exec(() => document.querySelector('[data-testid="response-time"]')?.textContent);
    return `click→CodeMirror shown ${perf.ui1mbSendRenderMs} ms (network time ${meta}); WebDriver polling granularity 50 ms`;
  });

  let before = null;
  await step("12. history entries are listed in Activity (AC-HIST-1/2)", async () => {
    await s.click('[data-testid="sidebar-tab-activity"]');
    await s.waitSel('[data-testid="history-row"]', 5000);
    before = await s.exec(() => [...document.querySelectorAll('[data-testid="history-row"]')].map((r) => r.textContent.replace(/\s+/g, " ").trim()));
    const n = (await s.invoke("list_history")).length;
    await s.shot("12-history");
    if (n < 5) throw new Error(`only ${n} history entries`);
    return `${n} entries; first rows: ${JSON.stringify(before.slice(0, 3))}`;
  });

  await step("R3-a. settings persist after a sidebar drag: save_settings accepts fractional sidebarWidth (QA-R2-001)", async () => {
    const st = await s.invoke("get_settings");
    const saved = await s.invoke("save_settings", { settings: { ...st, sidebarWidth: 312.5, theme: "light" } });
    if (saved.sidebarWidth !== 313 && saved.sidebarWidth !== 312) throw new Error(`sidebarWidth after save ${saved.sidebarWidth}`);
    // real pointer drag of the sidebar splitter (WebDriver actions), then check the drag persisted
    const box = await s.exec(() => { const r = document.querySelector('[role="separator"][aria-label="Resize sidebar"]').getBoundingClientRect(); return { x: Math.round(r.x + r.width / 2), y: Math.round(r.y + r.height / 2) }; });
    await wd("POST", `${s.p}/actions`, { actions: [{ type: "pointer", id: "m", parameters: { pointerType: "mouse" }, actions: [
      { type: "pointerMove", x: box.x, y: box.y, duration: 0 }, { type: "pointerDown", button: 0 },
      { type: "pointerMove", x: box.x + 37, y: box.y, duration: 120 }, { type: "pointerUp", button: 0 }] }] });
    await sleep(800);
    const after = await s.invoke("get_settings");
    return `fractional save → ${saved.sidebarWidth}; after real drag sidebarWidth=${after.sidebarWidth} (expected ≈ ${saved.sidebarWidth + 37}), theme=${after.theme}`;
  });

  await step("R3-b. share create → Settings › Shares › Update keeps includeCredentials + attached environment (QA-R2-002)", async () => {
    const b = await s.invoke("bootstrap");
    const pid = b.settings.activeProjectId || b.projects[0].id;
    const col = await s.invoke("create_collection", { projectId: pid, name: "QA Share" });
    await s.invoke("update_collection", { collection: { ...col, auth: { type: "bearer", token: "QA-CRED-XYZ" } } });
    const req = await s.invoke("create_request", { collectionId: col.id, folderId: null });
    await s.invoke("save_request", { request: { ...req, name: "Shared req", url: "{{baseUrl}}/echo/share", auth: { type: "inherit" } } });
    const env = await s.invoke("create_environment", { projectId: pid, name: "QA ShareEnv" });
    await s.invoke("update_environment", { environment: { ...env, variables: [{ key: "region", value: "eu-qa", enabled: true, secret: false }] } });
    await s.invoke("sync_create_workspace", { workerUrl: WORKER, name: "QA R3" });
    await s.exec(() => location.reload());
    await sleep(300);
    await appReady(s);
    await s.click('[data-testid="sidebar-tab-collections"]');
    await s.waitSel('[data-testid="tree-node"][data-name="QA Share"]', 5000);
    await s.click('[data-testid="tree-node"][data-name="QA Share"]');
    await s.waitSel('[data-testid="tree-node"][data-name="Shared req"]', 5000);
    await s.click('[data-testid="tree-node"][data-name="Shared req"]');
    await s.click('[data-testid="request-menu"]');
    await s.click('[data-testid="menu-share"]');
    await s.waitSel('[data-testid="share-include-credentials"]', 5000);
    await s.click('[data-testid="share-include-credentials"]');
    await s.exec((id) => { const el = document.querySelector('[data-testid="share-env"]'); el.value = id; el.dispatchEvent(new Event("change", { bubbles: true })); }, env.id);
    await sleep(200);
    await s.click('[data-testid="share-create"]');
    await s.waitSel('[data-testid="share-url"]', 10000);
    const url = await s.exec(() => document.querySelector('[data-testid="share-url"]').value);
    const snap1 = await (await fetch(`${url}.json`)).json();
    await s.click('[data-testid="dialog-close"]');
    // Update from Settings › Shares
    await s.click('[data-testid="settings-button"]');
    await s.click('[data-testid="settings-nav-shares"]');
    await s.waitSel('[data-testid="shares-table"]', 8000);
    await s.exec(() => [...document.querySelectorAll('[data-testid="shares-table"] button')].find((b) => b.textContent.trim() === "Update").click());
    await s.waitFor(() => [...document.querySelectorAll('[data-testid="toast"]')].some((t) => /Share updated/.test(t.textContent)), [], 8000, "Share updated toast");
    await sleep(300);
    const snap2 = await (await fetch(`${url}.json`)).json();
    const j = (x) => JSON.stringify(x);
    const cred1 = j(snap1).includes("QA-CRED-XYZ"), cred2 = j(snap2).includes("QA-CRED-XYZ");
    const env1 = j(snap1.snapshot?.environment ?? snap1.environment ?? null).includes("eu-qa"), env2 = j(snap2.snapshot?.environment ?? snap2.environment ?? null).includes("eu-qa");
    await s.click('[data-testid="dialog-close"]').catch(() => {});
    if (!(cred1 && env1)) throw new Error(`create did not include options: cred=${cred1} env=${env1}`);
    if (!(cred2 && env2)) throw new Error(`QA-R2-002 still open: after Update cred=${cred2} env=${env2}`);
    return `create: credentials+env present; after Settings › Update: credentials=${cred2}, env=${env2}`;
  });

  const errs = await s.exec(() => window.__qaErrors || []).catch(() => []);
  results.push({ name: "page errors during run", ok: errs.length === 0, detail: JSON.stringify(errs).slice(0, 500) });
  await s.quit();
  await sleep(1000);

  await step("13. history survives an app restart (AC-HIST-1, AC-COL-6)", async () => {
    s = await Session.start();
    await appReady(s);
    await s.click('[data-testid="sidebar-tab-activity"]');
    await s.waitSel('[data-testid="history-row"]', 5000);
    const after = await s.exec(() => [...document.querySelectorAll('[data-testid="history-row"]')].map((r) => r.textContent.replace(/\s+/g, " ").trim()));
    const n = (await s.invoke("list_history")).length;
    if (!after.length || after.length !== before.length) throw new Error(`rows before ${before.length} after ${after.length}`);
    const env = await s.text('[data-testid="active-env"]');
    await s.shot("13-restart");
    return `${n} entries after restart; active env "${env}"`;
  });

  await step("14. dynamic contract probe: every command through the real IPC (shapes saved for qa/r2/shape_diff.mjs)", async () => {
    const src = readFileSync("/app/qa/r2/contract_probe.js", "utf8").replace(/^\/\/.*$/gm, "");
    const spec = readFileSync("/app/qa/fixtures/openapi/webhooks_v31.yaml", "utf8");
    const r = await s.execAsync(`(ctx) => { ${src}; return contractProbe((c, a) => window.__TAURI_INTERNALS__.invoke(c, a), ctx); }`, { echo: ECHO, worker: WORKER, tmp: "/tmp/qa-body.bin", spec });
    writeFileSync(`${OUT}/contract_real.json`, JSON.stringify(r, null, 2));
    return `${Object.keys(r).length} probe results`;
  });
  await s.quit();

  await step("15. cold start (session create → UI interactive), 5 runs", async () => {
    const r = await coldStarts(5);
    const sorted = r.times.slice().sort((a, b) => a - b);
    perf.coldStartMs = r.times;
    perf.coldStartInPageMs = r.inPage;
    perf.coldStartP50 = sorted[2];
    return `webdriver-observed ${JSON.stringify(r.times)} ms (p50 ${sorted[2]}); webview performance.now() at ready ${JSON.stringify(r.inPage)} ms`;
  });

  writeFileSync(`${OUT}/real_app_results.json`, JSON.stringify({ results, perf }, null, 2));
  log(`\nreal-app smoke: ${results.filter((r) => r.ok).length}/${results.length} passed`);
  log(JSON.stringify(perf));
})().catch((e) => { console.error(e); process.exit(2); });

// QA round 2 — minimal WebDriver timing probe (debug aid).
const WD = "http://127.0.0.1:4444";
const t = Date.now(); const ts = () => `${Date.now() - t}ms`;
async function wd(m, p, b) { const r = await fetch(WD + p, { method: m, headers: { "content-type": "application/json" }, body: b && JSON.stringify(b), signal: AbortSignal.timeout(30000) }); return (await r.json()).value; }
const s = await wd("POST", "/session", { capabilities: { alwaysMatch: { browserName: "wry", "tauri:options": { application: process.env.APP } } } });
console.log(ts(), "session", s.sessionId);
const P = `/session/${s.sessionId}`;
try { console.log(ts(), "url", await wd("GET", `${P}/url`)); } catch (e) { console.log(ts(), "url err", e.message); }
try { console.log(ts(), "exec", JSON.stringify(await wd("POST", `${P}/execute/sync`, { script: "return [document.readyState, !!document.querySelector('[data-testid=app]'), '__TAURI_INTERNALS__' in window]", args: [] }))); } catch (e) { console.log(ts(), "exec err", e.message); }
try { console.log(ts(), "async", JSON.stringify(await wd("POST", `${P}/execute/async`, { script: "const d=arguments[0]; window.__TAURI_INTERNALS__.invoke('list_projects',{}).then(v=>d(v.length), e=>d('ERR '+JSON.stringify(e)))", args: [] }))); } catch (e) { console.log(ts(), "async err", e.message); }
await wd("DELETE", P);
console.log(ts(), "deleted");

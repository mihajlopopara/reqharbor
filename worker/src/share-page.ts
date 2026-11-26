// Server-rendered, read-only share page (AC-SHARE-3, AC-QRY-1). No client framework.
// All user content is HTML-escaped; it never enters <script>/<style> or URL attributes.
// The only inline <style> and <script> are static and allowed by their CSP hashes.

import { CurlExportError, toCurl } from "./curl";
import type { ViewFolder, ViewRequest, ViewSnapshot } from "./snapshot";
import { VAR_RE, variableMap } from "./variables";

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/** Escapes text and wraps `{{variables}}` in a highlight span. */
function withVars(text: string): string {
  let out = "";
  let last = 0;
  for (const m of text.matchAll(VAR_RE)) {
    out += escapeHtml(text.slice(last, m.index)) + `<span class="var">${escapeHtml(m[0])}</span>`;
    last = m.index + m[0].length;
  }
  return out + escapeHtml(text.slice(last));
}

const KNOWN_METHODS = new Set(["GET", "POST", "PUT", "PATCH", "DELETE", "QUERY", "HEAD", "OPTIONS"]);

function methodBadge(raw: string): string {
  const method = raw.trim();
  const upper = method.toUpperCase();
  if (KNOWN_METHODS.has(upper)) return `<span class="m m-${upper.toLowerCase()}">${upper}</span>`;
  const short = method.length > 7 ? method.slice(0, 7) + "…" : method;
  return `<span class="m m-custom" title="${escapeHtml(method)}">${escapeHtml(short)}</span>`;
}

function formatTime(ms: number): string {
  return new Date(ms).toISOString().slice(0, 16).replace("T", " ") + " UTC";
}

function kvTable(rows: { key: string; value: string; enabled: boolean; extra?: string }[], label: string, extraHead?: string): string {
  if (rows.length === 0) return "";
  const body = rows
    .map(
      (r) =>
        `<tr${r.enabled ? "" : ' class="off"'}><td>${withVars(r.key)}</td><td>${withVars(r.value)}</td>${
          extraHead ? `<td class="ui">${escapeHtml(r.extra ?? "")}</td>` : ""
        }</tr>`,
    )
    .join("");
  return `<table class="kv"><caption>${escapeHtml(label)} <span class="cnt">${rows.filter((r) => r.enabled).length}</span></caption>
<thead><tr><th scope="col">Key</th><th scope="col">Value</th>${extraHead ? `<th scope="col">${escapeHtml(extraHead)}</th>` : ""}</tr></thead><tbody>${body}</tbody></table>`;
}

function prettyJson(text: string): string {
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
}

function codeBlock(label: string, text: string): string {
  return `<div class="sublabel">${escapeHtml(label)}</div><pre class="code">${withVars(text)}</pre>`;
}

function renderAuth(req: ViewRequest, snap: ViewSnapshot): string {
  const a = req.auth;
  const row = (k: string, v: string) => `<div class="field"><span class="label">${escapeHtml(k)}</span><code>${withVars(v)}</code></div>`;
  switch (a.type) {
    case "basic":
      return `<p class="meta">Basic auth</p>${row("Username", a.username ?? "")}${row("Password", a.password ?? "")}`;
    case "bearer":
      return `<p class="meta">Bearer token</p>${row("Token", a.token ?? "")}`;
    case "apikey":
      return `<p class="meta">API key in ${a.apiKey?.in === "query" ? "query" : "header"}</p>${row("Key", a.apiKey?.key ?? "")}${row("Value", a.apiKey?.value ?? "")}`;
    case "inherit": {
      const t = snap.collectionAuth?.type ?? "none";
      return `<p class="meta">Inherits ${escapeHtml(t === "none" ? "no auth" : t)} from the collection</p>`;
    }
    default:
      return "";
  }
}

function renderBody(req: ViewRequest): string {
  const b = req.body;
  switch (b.type) {
    case "json":
      return codeBlock("JSON Content", prettyJson(b.text ?? ""));
    case "xml":
      return codeBlock("XML Content", b.text ?? "");
    case "text":
      return codeBlock("Text Content", b.text ?? "");
    case "graphql": {
      const v = b.graphql?.variables;
      const vars = typeof v === "string" ? prettyJson(v) : v === undefined || v === null ? "" : JSON.stringify(v, null, 2);
      return codeBlock("GraphQL Query", b.graphql?.query ?? "") + (vars ? codeBlock("GraphQL Variables", vars) : "");
    }
    case "formEncoded":
      return kvTable(b.formEncoded ?? [], "Form-encode");
    case "form":
      return kvTable(
        (b.form ?? []).map((f) => ({ ...f, extra: f.kind === "file" ? "File" : "Text" })),
        "Form",
        "Kind",
      );
    case "binary":
      return `<div class="sublabel">Binary file</div><pre class="code">${withVars(b.binaryPath ?? "")}</pre>`;
    default:
      return "";
  }
}

function renderRequest(req: ViewRequest, snap: ViewSnapshot, crumbs: string[], vars: Map<string, string>): string {
  const hasEnv = snap.environment !== null;
  const curlOpts = { inheritedAuth: snap.collectionAuth };
  let raw: string | null = null;
  let resolved: string | null = null;
  let exportError = "";
  try {
    raw = toCurl(req, { ...curlOpts, keepVariables: true });
    resolved = hasEnv ? toCurl(req, { ...curlOpts, variables: vars }) : raw;
  } catch (e) {
    if (!(e instanceof CurlExportError)) throw e;
    exportError = e.message;
  }
  const s = req.settings;
  const notes = [
    !s.followRedirects ? "Redirects not followed" : "",
    s.timeoutMs !== 30000 ? `Timeout ${s.timeoutMs} ms` : "",
    !s.verifyTls ? "TLS verification off" : "",
  ].filter(Boolean);
  const queryHint =
    req.method.toUpperCase() === "QUERY" && req.body.type === "none"
      ? `<p class="hint">QUERY requests normally carry a body</p>`
      : "";
  const auth = renderAuth(req, snap);
  const body = renderBody(req);
  return `<section class="req" id="${req.key}" aria-labelledby="${req.key}-h">
<div class="crumbs">${crumbs.map(escapeHtml).join(' <span aria-hidden="true">›</span> ')}</div>
<h2 id="${req.key}-h">${escapeHtml(req.name)}</h2>
<div class="reqbar">${methodBadge(req.method)}<code class="url">${withVars(req.url)}</code></div>
<div class="actions">${
    resolved !== null
      ? `<button type="button" class="btn btn-primary" data-copy="${req.key}">Copy as cURL</button>`
      : `<span class="hint">Can't export as cURL: ${escapeHtml(exportError)}</span>`
  }${notes.length ? `<span class="meta">${notes.map(escapeHtml).join(" · ")}</span>` : ""}</div>
${kvTable(req.params, "Query")}
${kvTable(req.headers, "Headers")}
${auth ? `<div class="block"><h3>Auth</h3>${auth}</div>` : ""}
<div class="block"><h3>Body</h3>${queryHint}${body || '<p class="meta">This request has no body.</p>'}</div>
${
    resolved !== null && raw !== null
      ? `<details class="curl"><summary>cURL</summary><pre class="code curl-resolved" id="${req.key}-curl">${escapeHtml(resolved)}</pre>${
          hasEnv ? `<pre class="code curl-raw" id="${req.key}-curl-raw">${escapeHtml(raw)}</pre>` : ""
        }</details>`
      : ""
  }
</section>`;
}

interface Tree {
  folders: Map<string | null, ViewFolder[]>;
  requests: Map<string | null, ViewRequest[]>;
}

function buildTree(snap: ViewSnapshot): Tree {
  const folderIds = new Set(snap.folders.map((f) => f.id));
  const byOrder = <T extends { sortOrder: number; name: string }>(a: T, b: T) =>
    a.sortOrder - b.sortOrder || a.name.localeCompare(b.name);
  const folders = new Map<string | null, ViewFolder[]>();
  for (const f of snap.folders) {
    const parent = f.parentFolderId && folderIds.has(f.parentFolderId) && f.parentFolderId !== f.id ? f.parentFolderId : null;
    folders.set(parent, [...(folders.get(parent) ?? []), f]);
  }
  const requests = new Map<string | null, ViewRequest[]>();
  for (const r of snap.requests) {
    const parent = r.folderId && folderIds.has(r.folderId) ? r.folderId : null;
    requests.set(parent, [...(requests.get(parent) ?? []), r]);
  }
  for (const list of folders.values()) list.sort(byOrder);
  for (const list of requests.values()) list.sort(byOrder);
  return { folders, requests };
}

const MAX_DEPTH = 32;

export interface ShareMeta {
  shareId: string;
  createdAt: number;
  updatedAt: number;
}

export function renderSharePage(snap: ViewSnapshot, meta: ShareMeta): string {
  const vars = snap.environment ? variableMap(snap.environment.variables) : new Map<string, string>();
  const tree = buildTree(snap);
  const nav: string[] = [];
  const sections: string[] = [];
  const visited = new Set<string>();

  const walk = (parent: string | null, depth: number, crumbs: string[]) => {
    if (depth > MAX_DEPTH) return;
    for (const f of tree.folders.get(parent) ?? []) {
      if (visited.has(f.id)) continue; // guards against parent cycles in client data
      visited.add(f.id);
      nav.push(`<li><span class="trow folder depth-${Math.min(depth, 8)}"><span class="name">${escapeHtml(f.name)}</span></span><ul>`);
      walk(f.id, depth + 1, [...crumbs, f.name]);
      nav.push("</ul></li>");
    }
    for (const r of tree.requests.get(parent) ?? []) {
      nav.push(
        `<li><a class="trow depth-${Math.min(depth, 8)}" href="#${r.key}">${methodBadge(r.method)}<span class="name">${escapeHtml(r.name)}</span></a></li>`,
      );
      sections.push(renderRequest(r, snap, crumbs, vars));
    }
  };
  walk(null, 0, snap.kind === "collection" ? [snap.name] : []);

  const isCollection = snap.kind === "collection";
  const env = snap.environment;
  const envBlock = env
    ? `<section class="env"><h2>Environment <span class="meta">${escapeHtml(env.name)}</span></h2>${kvTable(env.variables, "Variables")}
<label class="toggle"><input type="checkbox" id="keep-vars"> Keep {{variables}} in copied cURL</label></section>`
    : "";

  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow">
<meta name="referrer" content="no-referrer">
<meta name="color-scheme" content="dark light">
<title>${escapeHtml(snap.name)} · ReqHarbor</title>
<style>${PAGE_CSS}</style>
</head>
<body>
<header class="top">
<div class="brand">${LOGO_SVG}<span>ReqHarbor</span></div>
<div class="title"><h1>${escapeHtml(snap.name)}</h1>
<p class="meta">Read-only shared ${isCollection ? "collection" : "request"} · ${snap.requests.length} request${snap.requests.length === 1 ? "" : "s"} · Updated ${formatTime(meta.updatedAt)}</p></div>
</header>
<div class="layout${isCollection ? "" : " single"}">
${isCollection ? `<nav class="tree" aria-label="Requests"><ul>${nav.join("")}</ul></nav>` : ""}
<main>
${snap.description ? `<p class="desc">${escapeHtml(snap.description)}</p>` : ""}
${envBlock}
${sections.join("\n") || '<p class="empty">This collection has no requests.</p>'}
</main>
</div>
<div id="live" class="sr" role="status" aria-live="polite"></div>
<script>${PAGE_JS}</script>
</body>
</html>`;
}

export function renderErrorPage(status: 404 | 410): string {
  const title = status === 410 ? "This link has been revoked" : "Share not found";
  const text =
    status === 410
      ? "The owner revoked this share link. Ask them for a new one."
      : "This share link does not exist. Check that you copied the whole URL.";
  return `<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex, nofollow"><meta name="color-scheme" content="dark light">
<title>${title} · ReqHarbor</title><style>${PAGE_CSS}</style></head>
<body><main class="gone"><div class="brand">${LOGO_SVG}<span>ReqHarbor</span></div><h1>${title}</h1><p class="meta">${text}</p></main></body></html>`;
}

const LOGO_SVG = `<svg class="i" viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M4 12h10M4 17h6"/><path d="M17 14l3 3-3 3"/></svg>`;

/** Static client script: copy-to-clipboard and the Keep {{variables}} toggle. Hash-allowed by CSP. */
export const PAGE_JS = `(function(){var d=document,live=d.getElementById("live"),keep=d.getElementById("keep-vars");
if(keep){keep.addEventListener("change",function(){d.body.classList.toggle("keep-vars",keep.checked)})}
function fallback(el){var det=el.closest("details");if(det)det.open=true;var r=d.createRange();r.selectNodeContents(el);var s=getSelection();s.removeAllRanges();s.addRange(r);try{return d.execCommand("copy")}catch(e){return false}}
d.addEventListener("click",function(e){var b=e.target&&e.target.closest?e.target.closest("button[data-copy]"):null;if(!b)return;
var id=b.getAttribute("data-copy"),raw=d.body.classList.contains("keep-vars");
var el=(raw&&d.getElementById(id+"-curl-raw"))||d.getElementById(id+"-curl");if(!el)return;
function done(ok){b.textContent=ok?"Copied":"Press Ctrl+C to copy";if(live)live.textContent=ok?"cURL copied to clipboard":"";setTimeout(function(){b.textContent="Copy as cURL"},1600)}
if(navigator.clipboard&&navigator.clipboard.writeText){navigator.clipboard.writeText(el.textContent).then(function(){done(true)},function(){done(fallback(el))})}else{done(fallback(el))}})})();`;

/** Trimmed copy of docs/design/tokens.css plus share-page components (DESIGN.md §9). */
export const PAGE_CSS = `:root{--font-ui:-apple-system,BlinkMacSystemFont,"Segoe UI Variable Text","Segoe UI",system-ui,Ubuntu,Cantarell,"Noto Sans","Helvetica Neue",Arial,sans-serif;--font-mono:ui-monospace,"SF Mono",SFMono-Regular,Menlo,"Cascadia Mono",Consolas,"Liberation Mono","DejaVu Sans Mono",monospace;
--fs-xs:10.5px;--fs-sm:11.5px;--fs-md:12.5px;--fs-lg:14px;--fs-xl:16px;--fs-code:12.5px;--lh-ui:18px;--lh-code:19px;
--sp-1:2px;--sp-2:4px;--sp-3:6px;--sp-4:8px;--sp-5:12px;--sp-6:16px;--sp-7:20px;--sp-8:24px;--sp-9:32px;
--r-xs:3px;--r-sm:4px;--r-md:6px;--h-control:28px;--h-control-lg:30px;--h-row-tree:26px;--h-row-kv:28px;--w-method-col:54px;--indent-tree:12px;--dur-fast:80ms;
color-scheme:dark;--bg-pane:#15171b;--bg-sidebar:#1a1c21;--bg-input:#1f2228;--bg-hover:rgba(255,255,255,.045);--bg-active:rgba(255,255,255,.08);
--border-subtle:#262a31;--border-strong:#3a404a;--text-1:#e4e7ec;--text-2:#aab1bd;--text-3:#8a929f;--text-on-accent:#fff;
--accent:#2f6fdf;--accent-hover:#2a69d8;--accent-active:#2861c7;--accent-fg:#7cadff;--accent-soft:rgba(124,173,255,.14);--focus-ring:#7cadff;--warning:#e9b04f;--warning-soft:rgba(233,176,79,.14);
--method-get:#5cc98a;--method-post:#e9b04f;--method-put:#6ea5ff;--method-patch:#b694ff;--method-delete:#ff7b72;--method-query:#2fc9c0;--method-head:#93a6c2;--method-options:#c3aa94;--method-custom:#a4a8ae;
--var-fg:#79c0ff;--var-bg:rgba(121,192,255,.13)}
@media (prefers-color-scheme:light){:root{color-scheme:light;--bg-pane:#fff;--bg-sidebar:#f6f7f9;--bg-input:#f1f3f6;--bg-hover:rgba(15,23,42,.045);--bg-active:rgba(15,23,42,.08);
--border-subtle:#e4e7ec;--border-strong:#c9ced6;--text-1:#1a1d23;--text-2:#4a5260;--text-3:#626a78;
--accent:#2563eb;--accent-hover:#1f56d6;--accent-active:#1a4abb;--accent-fg:#1d5bd6;--accent-soft:rgba(37,99,235,.1);--focus-ring:#2563eb;--warning:#9a5800;--warning-soft:rgba(154,88,0,.1);
--method-get:#167246;--method-post:#9a5800;--method-put:#1f5fd1;--method-patch:#7339d0;--method-delete:#bd2f28;--method-query:#08746f;--method-head:#4a5f7d;--method-options:#75604c;--method-custom:#5c636e;
--var-fg:#0b62c4;--var-bg:rgba(11,98,196,.1)}}
@media (prefers-reduced-motion:reduce){:root{--dur-fast:0ms}}
*,*::before,*::after{box-sizing:border-box}
html,body{margin:0}
body{background:var(--bg-pane);color:var(--text-1);font:400 var(--fs-md)/var(--lh-ui) var(--font-ui);-webkit-font-smoothing:antialiased}
:focus-visible{outline:2px solid var(--focus-ring);outline-offset:1px}
h1,h2,h3{margin:0}
code,pre{font-family:var(--font-mono);font-size:var(--fs-code)}
svg.i{width:16px;height:16px;flex:none;stroke:currentColor;fill:none;stroke-width:1.5;stroke-linecap:round;stroke-linejoin:round}
.sr{position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
.meta{color:var(--text-3);font-size:var(--fs-sm);font-weight:400}
.top{display:flex;align-items:center;gap:var(--sp-6);padding:var(--sp-5) var(--sp-6);border-bottom:1px solid var(--border-subtle);background:var(--bg-sidebar)}
.brand{display:flex;align-items:center;gap:var(--sp-3);color:var(--text-2);font-weight:600;white-space:nowrap}
.title{min-width:0}.title h1{font-size:var(--fs-lg);line-height:20px;font-weight:600;overflow-wrap:anywhere}.title .meta{margin:0}
.layout{display:grid;grid-template-columns:300px 1fr;min-height:calc(100vh - 57px)}
.layout.single{grid-template-columns:1fr}
.tree{background:var(--bg-sidebar);border-right:1px solid var(--border-subtle);padding:var(--sp-2) 0;overflow:auto;position:sticky;top:0;max-height:100vh}
.tree ul{list-style:none;margin:0;padding:0}
.trow{height:var(--h-row-tree);display:flex;align-items:center;gap:var(--sp-3);padding:0 var(--sp-4) 0 var(--sp-5);white-space:nowrap;color:var(--text-1);text-decoration:none}
a.trow:hover{background:var(--bg-hover)}
.trow.folder{color:var(--text-2);font-weight:500}
.trow .name{flex:1;overflow:hidden;text-overflow:ellipsis}
.trow .m{width:var(--w-method-col);flex:none}
.depth-1{padding-left:24px}.depth-2{padding-left:36px}.depth-3{padding-left:48px}.depth-4{padding-left:60px}.depth-5{padding-left:72px}.depth-6{padding-left:84px}.depth-7{padding-left:96px}.depth-8{padding-left:108px}
main{min-width:0;padding:var(--sp-6) var(--sp-8) var(--sp-9);max-width:1100px}
.desc{white-space:pre-wrap;color:var(--text-2);margin:0 0 var(--sp-6)}
.req,.env{padding:var(--sp-6) 0 var(--sp-8);border-bottom:1px solid var(--border-subtle)}
.req h2,.env h2{font-size:var(--fs-lg);line-height:20px;font-weight:600;margin:var(--sp-2) 0 var(--sp-4);overflow-wrap:anywhere}
.crumbs{color:var(--text-3);font-size:var(--fs-sm);overflow-wrap:anywhere}
.m{font:600 var(--fs-xs)/1 var(--font-mono);letter-spacing:.02em;text-transform:uppercase}
.m-get{color:var(--method-get)}.m-post{color:var(--method-post)}.m-put{color:var(--method-put)}.m-patch{color:var(--method-patch)}.m-delete{color:var(--method-delete)}.m-query{color:var(--method-query)}.m-head{color:var(--method-head)}.m-options{color:var(--method-options)}.m-custom{color:var(--method-custom);text-transform:none}
.reqbar{display:flex;align-items:center;min-height:var(--h-control-lg);border-radius:var(--r-sm);background:var(--bg-input)}
.reqbar .m{font-size:var(--fs-md);flex:none;min-width:92px;padding:0 var(--sp-4);border-right:1px solid var(--border-subtle);line-height:var(--h-control-lg)}
.url{flex:1;min-width:0;padding:var(--sp-3) var(--sp-4);overflow-wrap:anywhere}
.var{color:var(--var-fg);background:var(--var-bg);border-radius:var(--r-xs);padding:1px 2px}
.actions{display:flex;align-items:center;gap:var(--sp-5);margin:var(--sp-4) 0 var(--sp-2)}
.btn{height:var(--h-control);display:inline-flex;align-items:center;padding:0 var(--sp-5);border:1px solid transparent;border-radius:var(--r-sm);font:500 var(--fs-md) var(--font-ui);cursor:pointer;transition:background-color var(--dur-fast)}
.btn-primary{background:var(--accent);color:var(--text-on-accent);min-width:112px;justify-content:center}.btn-primary:hover{background:var(--accent-hover)}.btn-primary:active{background:var(--accent-active)}
.kv{width:100%;table-layout:fixed;border-collapse:collapse;margin-top:var(--sp-6)}
.kv caption{text-align:left;font-weight:600;padding-bottom:var(--sp-2)}
.kv .cnt{color:var(--accent-fg);font-size:var(--fs-xs);vertical-align:super}
.kv th{height:24px;text-align:left;font-weight:500;font-size:var(--fs-sm);color:var(--text-3);padding:0 var(--sp-3)}
.kv th:first-child{width:34%}
.kv td{height:var(--h-row-kv);padding:var(--sp-2) var(--sp-3);border-top:1px solid var(--border-subtle);font-family:var(--font-mono);font-size:var(--fs-code);overflow-wrap:anywhere;vertical-align:top}
.kv td.ui{font-family:var(--font-ui)}
.kv tr.off td{color:var(--text-3);text-decoration:line-through}
.block{margin-top:var(--sp-6)}.block h3{font-size:var(--fs-md);font-weight:600;margin-bottom:var(--sp-2)}
.field{display:flex;gap:var(--sp-4);align-items:baseline;margin:var(--sp-2) 0}.field .label{color:var(--text-2);font-size:var(--fs-sm);min-width:72px}
.sublabel{color:var(--text-3);font-size:var(--fs-sm);margin:var(--sp-4) 0 var(--sp-2)}
pre.code{margin:0;padding:var(--sp-4) var(--sp-5);background:var(--bg-input);border-radius:var(--r-sm);line-height:var(--lh-code);white-space:pre-wrap;overflow-wrap:anywhere;max-height:480px;overflow:auto}
.hint{margin:0 0 var(--sp-2);padding:var(--sp-2) var(--sp-4);border-radius:var(--r-sm);background:var(--warning-soft);color:var(--warning);font-size:var(--fs-sm)}
details.curl{margin-top:var(--sp-6)}details.curl summary{cursor:pointer;color:var(--accent-fg);font-weight:500;width:max-content}
details.curl pre{margin-top:var(--sp-3)}
.curl-raw{display:none}.keep-vars .curl-raw{display:block}.keep-vars .curl-resolved{display:none}
.toggle{display:inline-flex;align-items:center;gap:var(--sp-3);margin-top:var(--sp-5);color:var(--text-2)}
.empty{color:var(--text-3)}
.gone{max-width:520px;margin:15vh auto;padding:0 var(--sp-6)}.gone h1{font-size:var(--fs-xl);margin:var(--sp-6) 0 var(--sp-2)}
@media (max-width:820px){.layout{grid-template-columns:1fr}.tree{position:static;max-height:none;border-right:0;border-bottom:1px solid var(--border-subtle)}main{padding:var(--sp-5) var(--sp-6) var(--sp-8)}.top{flex-wrap:wrap;gap:var(--sp-4)}}`;

let hashes: Promise<{ script: string; style: string }> | null = null;
async function sha256Base64(text: string): Promise<string> {
  const d = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  let bin = "";
  for (const b of new Uint8Array(d)) bin += String.fromCharCode(b);
  return btoa(bin);
}

/** Strict CSP: only the exact inline style/script above may run; nothing else loads. */
export async function sharePageCsp(): Promise<string> {
  hashes ??= Promise.all([sha256Base64(PAGE_JS), sha256Base64(PAGE_CSS)]).then(([script, style]) => ({ script, style }));
  const h = await hashes;
  return [
    "default-src 'none'",
    `script-src 'sha256-${h.script}'`,
    `style-src 'sha256-${h.style}'`,
    "img-src 'none'",
    "connect-src 'none'",
    "base-uri 'none'",
    "form-action 'none'",
    "frame-ancestors 'none'",
  ].join("; ");
}

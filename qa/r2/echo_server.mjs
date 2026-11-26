#!/usr/bin/env node
// QA round 2 — tiny local HTTP target for the real-app smoke and perf runs (runs in Docker).
//   node qa/r2/echo_server.mjs [port=8099]
// Routes (any method, incl. QUERY):
//   /echo*            200 JSON {method,url,headers,rawHeaders,body}
//   /query            QUERY -> 200 JSON echo + `Accept-Query: application/json`; other methods -> 405
//   /big?mb=N         200 application/json of ~N MB (default 6)
//   /slow?ms=N        waits N ms (default 5000) then 200
//   /gzip             gzip-encoded JSON
//   /cookies          two Set-Cookie headers
//   /status/NNN       that status with a small JSON body
//   /redirect?to=URL&code=NNN  redirect
import http from "node:http";
import { gzipSync } from "node:zlib";

const port = Number(process.argv[2] || 8099);
const json = (res, status, obj, extra = {}) => {
  const body = Buffer.from(JSON.stringify(obj));
  res.writeHead(status, { "Content-Type": "application/json", "Content-Length": body.length, ...extra });
  res.end(body);
};

http
  .createServer((req, res) => {
    const chunks = [];
    req.on("data", (c) => chunks.push(c));
    req.on("end", () => {
      const body = Buffer.concat(chunks).toString("utf8");
      const u = new URL(req.url, `http://${req.headers.host || "localhost"}`);
      const echo = { method: req.method, url: req.url, headers: req.headers, rawHeaders: req.rawHeaders, body };
      const p = u.pathname;
      // CORS so the browser mock's real fetch (Playwright) can reach this server too
      res.setHeader("Access-Control-Allow-Origin", "*");
      res.setHeader("Access-Control-Expose-Headers", "*");
      if (req.method === "OPTIONS" && req.headers["access-control-request-method"]) {
        res.writeHead(204, { "Access-Control-Allow-Methods": "*", "Access-Control-Allow-Headers": "*", "Access-Control-Max-Age": "600" });
        return res.end();
      }
      if (p.startsWith("/echo")) return json(res, 200, echo);
      if (p === "/query") {
        if (req.method !== "QUERY") return json(res, 405, { error: "use QUERY" }, { Allow: "QUERY" });
        return json(res, 200, echo, { "Accept-Query": "application/json" });
      }
      if (p === "/big") {
        const mb = Number(u.searchParams.get("mb") || 6);
        const row = JSON.stringify({ id: 1, name: "row".padEnd(80, "x"), tags: ["a", "b", "c"], n: 3.14 });
        const n = Math.ceil((mb * 1024 * 1024) / (row.length + 1));
        const buf = Buffer.from("[" + Array(n).fill(row).join(",") + "]");
        res.writeHead(200, { "Content-Type": "application/json", "Content-Length": buf.length });
        return res.end(buf);
      }
      if (p === "/slow") {
        const ms = Number(u.searchParams.get("ms") || 5000);
        const t = setTimeout(() => json(res, 200, { slept: ms }), ms);
        res.on("close", () => clearTimeout(t));
        return;
      }
      if (p === "/gzip") {
        const buf = gzipSync(Buffer.from(JSON.stringify({ zipped: true, pad: "z".repeat(4000) })));
        res.writeHead(200, { "Content-Type": "application/json", "Content-Encoding": "gzip", "Content-Length": buf.length });
        return res.end(buf);
      }
      if (p === "/cookies") {
        res.setHeader("Set-Cookie", ["sid=abc; Path=/; HttpOnly", 'pref="dark"; Domain=127.0.0.1; Secure; SameSite=Lax; Max-Age=60']);
        return json(res, 200, { ok: true });
      }
      const st = p.match(/^\/status\/(\d{3})$/);
      if (st) return json(res, Number(st[1]), { status: Number(st[1]) });
      if (p === "/redirect") {
        res.writeHead(Number(u.searchParams.get("code") || 302), { Location: u.searchParams.get("to") || "/echo" });
        return res.end();
      }
      json(res, 404, { error: "not found", path: p });
    });
  })
  .listen(port, "0.0.0.0", () => console.log(`echo server on :${port}`));

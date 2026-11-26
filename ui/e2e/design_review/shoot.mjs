// Throwaway design-QA screenshot script (DESIGN_REVIEW_R1). Not part of the e2e suite.
// Run inside the dev container with a vite dev server on :5173:
//   docker compose run --rm -w /app/ui dev sh -c "npx vite --host 127.0.0.1 --port 5173 >/dev/null 2>&1 & sleep 6; node e2e/design_review/shoot.mjs"
import { chromium } from "@playwright/test";
import fs from "node:fs";

const BASE = process.env.BASE ?? "http://127.0.0.1:5173";
const OUT = process.env.OUT ?? "/app/docs/qa/design/screens";
const ONLY = process.env.ONLY ? new RegExp(process.env.ONLY) : null;
fs.mkdirSync(OUT, { recursive: true });

const VP = { L: { width: 1440, height: 900 }, M: { width: 1024, height: 700 }, S: { width: 800, height: 600 } };
const metrics = {};
const log = (...a) => console.log(...a);

const browser = await chromium.launch();

async function open(theme, vp, qs = "") {
  const ctx = await browser.newContext({ viewport: vp, colorScheme: theme, deviceScaleFactor: 1 });
  const page = await ctx.newPage();
  page.on("pageerror", (e) => log("  pageerror:", e.message));
  await page.goto(BASE + "/" + qs);
  await page.getByTestId("app").waitFor();
  await page.waitForTimeout(300);
  return { ctx, page };
}
const tid = (page, id) => page.getByTestId(id);
const node = (page, name) => page.locator(`[data-testid="tree-node"][data-name="${name}"]`).first();

async function mainState(page) {
  await tid(page, "sidebar-tab-collections").click();
  await node(page, "Users API").click();
  await page.waitForTimeout(150);
  if (await node(page, "Create user").count() === 0) await node(page, "Users").click();
  await node(page, "Create user").click();
  await tid(page, "req-tab-body").click();
  await tid(page, "send-button").click();
  await tid(page, "response-status").waitFor();
  await page.waitForTimeout(250);
  await tid(page, "sidebar-tab-activity").click();
  await page.mouse.move(5, 895);
  await page.waitForTimeout(200);
}

async function shot(page, name) {
  await page.waitForTimeout(180);
  await page.screenshot({ path: `${OUT}/${name}.png` });
  log("  ->", name);
}

async function measure(page, label) {
  metrics[label] = await page.evaluate(() => {
    const q = (s) => document.querySelector(s);
    const box = (el) => (el ? (({ x, y, width, height }) => ({ x: Math.round(x), y: Math.round(y), w: Math.round(width), h: Math.round(height) }))(el.getBoundingClientRect()) : null);
    const cs = (el, props) => (el ? Object.fromEntries(props.map((p) => [p, getComputedStyle(el).getPropertyValue(p)])) : null);
    const panes = ["[data-testid=sidebar]", "[data-testid=request-pane]", "[data-testid=response-pane]"];
    const rows = panes.map((p) => {
      const pane = q(p);
      if (!pane) return null;
      return [...pane.children].slice(0, 4).map((c) => ({ cls: c.className, ...box(c) }));
    });
    const tabs = [...document.querySelectorAll('[role=tablist]')].map((t) => ({ label: t.getAttribute("aria-label"), ...box(t) }));
    return {
      rows,
      tabs,
      send: box(q("[data-testid=send-button]")),
      sendCs: cs(q("[data-testid=send-button]"), ["background-color", "color", "border-radius", "font-weight", "height"]),
      method: box(q("[data-testid=method-select]")),
      methodCs: cs(q("[data-testid=method-select]"), ["color", "font-family", "font-size", "font-weight"]),
      methodBadge: cs(q("[data-testid=method-select] .m, [data-testid=method-select] span"), ["color", "font-family", "font-size", "font-weight"]),
      url: cs(q("[data-testid=url-input]"), ["font-family", "font-size", "color", "background-color"]),
      reqbar: cs(q("[data-testid=url-input]")?.closest(".reqbar") ?? null, ["height", "background-color", "border", "border-radius"]),
      status: cs(q("[data-testid=response-status]"), ["color", "background-color", "font-size", "height", "border-radius", "font-family"]),
      statusBox: box(q("[data-testid=response-status]")),
      historyRow: box(q("[data-testid=history-row]")),
      historyRowCs: cs(q("[data-testid=history-row]"), ["padding-left", "padding-right", "font-size"]),
      historySel: cs(q('[data-testid=history-row][aria-selected=true]'), ["background-color"]),
      bodyFont: cs(document.body, ["font-family", "font-size", "background-color", "color"]),
      cmContent: cs(q(".cm-content"), ["font-family", "font-size", "line-height"]),
      cmGutter: cs(q(".cm-gutters"), ["background-color", "color", "border-right", "width"]),
      selectedTab: cs(q('[role=tab][aria-selected=true]'), ["color", "font-weight", "font-size"]),
      themeAttr: document.documentElement.getAttribute("data-theme"),
    };
  });
}

async function contrast(page, label, selectors) {
  metrics[label] = await page.evaluate((sels) => {
    const parse = (c) => {
      const m = c.match(/rgba?\(([^)]+)\)/);
      if (!m) return null;
      const p = m[1].split(/[ ,/]+/).filter(Boolean).map(Number);
      return { r: p[0], g: p[1], b: p[2], a: p[3] ?? 1 };
    };
    const blend = (fg, bg) => ({ r: fg.r * fg.a + bg.r * (1 - fg.a), g: fg.g * fg.a + bg.g * (1 - fg.a), b: fg.b * fg.a + bg.b * (1 - fg.a), a: 1 });
    const bgOf = (el) => {
      const stack = [];
      for (let e = el; e; e = e.parentElement) {
        const c = parse(getComputedStyle(e).backgroundColor);
        if (c && c.a > 0) stack.push(c);
        if (c && c.a === 1) break;
      }
      let acc = { r: 0, g: 0, b: 0, a: 1 };
      for (let i = stack.length - 1; i >= 0; i--) acc = blend(stack[i], acc);
      return acc;
    };
    const lum = ({ r, g, b }) => {
      const f = (v) => ((v /= 255) <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4);
      return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
    };
    const out = {};
    for (const s of sels) {
      const el = document.querySelector(s);
      if (!el) { out[s] = "missing"; continue; }
      const bg = bgOf(el);
      let fg = parse(getComputedStyle(el).color);
      fg = blend(fg, bg);
      const op = Number(getComputedStyle(el).opacity);
      const L1 = lum(fg), L2 = lum(bg);
      out[s] = { ratio: +((Math.max(L1, L2) + 0.05) / (Math.min(L1, L2) + 0.05)).toFixed(2), color: getComputedStyle(el).color, opacity: op, text: (el.textContent ?? "").trim().slice(0, 30) };
    }
    return out;
  }, selectors);
}

const scenes = [];
const scene = (name, fn) => scenes.push({ name, fn });

for (const theme of ["dark", "light"]) {
  for (const [k, vp] of Object.entries(VP)) {
    scene(`main-${k}-${theme}`, async () => {
      const { ctx, page } = await open(theme, vp);
      await mainState(page);
      await shot(page, `01-main-${vp.width}-${theme}`);
      await measure(page, `main-${vp.width}-${theme}`);
      if (k === "L") {
        await contrast(page, `contrast-main-${theme}`, [
          "[data-testid=history-row] .meta", "[data-testid=history-row][aria-selected=true] .meta", "[data-testid=response-info]",
          "[data-testid=breadcrumb]", "[role=tab][aria-selected=false]", "[data-testid=sidebar-filter]", ".cm-gutterElement", "[data-testid=send-button]",
          "[data-testid=response-status]", "[data-testid=env-selector]", ".group", ".sublabel", "[data-testid=history-status]",
        ]);
        // hover on history row
        await page.locator("[data-testid=history-row]").nth(2).hover();
        await shot(page, `02-history-hover-${theme}`);
        // focus ring tour
        await page.mouse.move(5, 895);
        await page.locator("body").click({ position: { x: 700, y: 600 } }).catch(() => {});
        await tid(page, "project-switcher").focus();
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-import-${theme}`);
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-newreq-${theme}`);
        await tid(page, "sidebar-tab-activity").focus();
        await page.keyboard.press("ArrowRight");
        await page.keyboard.press("ArrowLeft");
        await shot(page, `03-focus-sidebar-tab-${theme}`);
        await page.keyboard.press("Tab");
        await page.keyboard.press("Tab");
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-list-${theme}`);
        await tid(page, "method-select").focus();
        await page.keyboard.press("Shift+Tab");
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-method-${theme}`);
        await tid(page, "send-button").focus();
        await page.keyboard.press("Shift+Tab");
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-send-${theme}`);
        await page.keyboard.press("Tab");
        await shot(page, `03-focus-reqtabs-${theme}`);
        await tid(page, "res-tab-body").focus();
        await page.keyboard.press("ArrowRight");
        await page.keyboard.press("ArrowLeft");
        await shot(page, `03-focus-restabs-${theme}`);
      }
      await ctx.close();
    });
  }

  scene(`sidebar-tabs-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await mainState(page);
    await tid(page, "sidebar-tab-collections").click();
    await node(page, "Billing").click().catch(() => {});
    await page.mouse.move(5, 895);
    await shot(page, `04-sidebar-collections-${theme}`);
    await node(page, "Health").hover().catch(() => {});
    await shot(page, `04-sidebar-collections-hover-${theme}`);
    await tid(page, "sidebar-tab-env").click();
    await page.mouse.move(5, 895);
    await shot(page, `04-sidebar-env-${theme}`);
    await tid(page, "sidebar-filter").fill("zzz");
    await shot(page, `04-sidebar-filter-nomatch-${theme}`);
    await tid(page, "sidebar-filter").fill("");
    await tid(page, "sidebar-tab-activity").click();
    await tid(page, "sidebar-filter").fill("users");
    await shot(page, `04-sidebar-activity-filter-${theme}`);
    await ctx.close();
  });

  scene(`request-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await mainState(page);
    await tid(page, "method-select").click();
    await tid(page, "method-option-QUERY").hover();
    await shot(page, `05-method-dropdown-${theme}`);
    await page.keyboard.press("Escape");
    await tid(page, "req-tab-headers").click();
    await page.mouse.move(5, 895);
    await shot(page, `06-headers-${theme}`);
    await page.locator('[data-testid$="-row"]').nth(1).hover().catch(() => {});
    await shot(page, `06-headers-hover-${theme}`);
    await tid(page, "req-tab-auth").click();
    await shot(page, `07-auth-inherit-${theme}`);
    await tid(page, "auth-type").click().catch(() => {});
    await page.waitForTimeout(150);
    await shot(page, `07-auth-type-open-${theme}`);
    await page.keyboard.press("Escape");
    // try to pick bearer
    try {
      const at = tid(page, "auth-type");
      const tag = await at.evaluate((e) => e.tagName);
      if (tag === "SELECT") await at.selectOption("bearer");
      else { await at.click(); await page.getByRole("option", { name: /bearer/i }).first().click(); }
      await page.waitForTimeout(150);
      await shot(page, `07-auth-bearer-${theme}`);
    } catch (e) { log("  bearer failed", e.message); }
    await tid(page, "req-tab-query").click();
    await shot(page, `08-query-empty-${theme}`);
    await tid(page, "req-tab-tests").click().catch(() => {});
    await shot(page, `08-tests-coming-soon-${theme}`);
    await tid(page, "request-settings").click().catch(() => {});
    await shot(page, `08-request-settings-${theme}`);
    await page.keyboard.press("Escape");
    // response headers
    await tid(page, "res-tab-headers").click();
    await shot(page, `09-response-headers-${theme}`);
    await tid(page, "res-tab-cookies").click();
    await shot(page, `09-response-cookies-${theme}`);
    // request menu
    await tid(page, "request-menu").click();
    await shot(page, `10-request-menu-${theme}`);
    await page.keyboard.press("Escape");
    // context menu on tree
    await tid(page, "sidebar-tab-collections").click();
    await node(page, "Users API").click({ button: "right" });
    await page.waitForTimeout(100);
    await page.keyboard.press("ArrowDown");
    await shot(page, `11-context-menu-${theme}`);
    await page.keyboard.press("Escape");
    // new menu
    await tid(page, "new-menu").click();
    await shot(page, `11-new-menu-${theme}`);
    await page.keyboard.press("Escape");
    // toast via copy body
    await tid(page, "copy-body").click();
    await shot(page, `12-toast-${theme}`);
    await ctx.close();
  });

  scene(`error-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await tid(page, "url-input").click();
    await tid(page, "url-input").fill("https://nope.invalid/x");
    await tid(page, "send-button").click();
    await tid(page, "response-error").waitFor();
    await page.mouse.move(5, 895);
    await shot(page, `13-error-dns-${theme}`);
    await tid(page, "url-input").fill("https://api.example.com/delay/5");
    await tid(page, "send-button").click();
    await page.waitForTimeout(700);
    await shot(page, `13-sending-${theme}`);
    await tid(page, "send-button").click();
    await shot(page, `13-cancelled-${theme}`);
    await tid(page, "url-input").fill("https://api.example.com/status/404");
    await tid(page, "send-button").click();
    await page.waitForTimeout(800);
    await shot(page, `13-404-${theme}`);
    await ctx.close();
  });

  scene(`import-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await tid(page, "import-button").click();
    await tid(page, "curl-input").fill(`curl -X POST 'https://api.example.com/v1/users' -H 'Content-Type: application/json' -H 'Authorization: Bearer abc123' -H 'X-Trace: 1' --data-raw '{"name":"Ada"}' --compressed --retry 3`);
    await page.waitForTimeout(300);
    await shot(page, `14-import-curl-${theme}`);
    await tid(page, "curl-input").fill("not a curl");
    await shot(page, `14-import-curl-invalid-${theme}`);
    await tid(page, "import-tab-openapi").click();
    await shot(page, `15-import-openapi-empty-${theme}`);
    const spec = {
      openapi: "3.1.0",
      info: { title: "Store API" },
      servers: [{ url: "https://store.dev" }],
      components: { securitySchemes: { o: { type: "oauth2", flows: {} } } },
      paths: {
        "/items": { get: { tags: ["items"], summary: "List items" }, post: { tags: ["items"], summary: "Add item" } },
        "/items/{id}": { get: { tags: ["items"] }, delete: { tags: ["items"], operationId: "deleteItem" } },
        "/orders": { get: { tags: ["orders"], summary: "List orders" } },
        "/orders/{id}": { patch: { tags: ["orders"], summary: "Update order" }, $ref: "https://x/y.yaml" },
      },
    };
    await tid(page, "openapi-input").fill(JSON.stringify(spec, null, 2));
    await page.waitForTimeout(300);
    await shot(page, `15-import-openapi-preview-${theme}`);
    await tid(page, "import-tab-link").click();
    await shot(page, `15-import-link-${theme}`);
    await page.keyboard.press("Escape");
    await ctx.close();
  });

  scene(`env-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await tid(page, "sidebar-tab-env").click();
    await page.locator('[data-testid="env-row"][data-name="Staging"]').click();
    await page.mouse.move(5, 895);
    await shot(page, `16-env-editor-${theme}`);
    await page.keyboard.press("ControlOrMeta+e");
    await page.waitForTimeout(150);
    await shot(page, `16-env-picker-${theme}`);
    await page.keyboard.press("Escape");
    await ctx.close();
  });

  scene(`share-settings-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    // share without sync
    await tid(page, "sidebar-tab-collections").click();
    await node(page, "Billing").click({ button: "right" });
    await page.getByTestId("tree-share").click();
    await shot(page, `17-share-needs-sync-${theme}`);
    await page.keyboard.press("Escape");
    await tid(page, "settings-button").click();
    await shot(page, `18-settings-general-${theme}`);
    await tid(page, "settings-nav-sync").click();
    await shot(page, `18-settings-sync-${theme}`);
    await tid(page, "sync-worker-url").fill("https://sync.example.dev");
    await tid(page, "sync-connect").click();
    await tid(page, "sync-token").waitFor();
    await shot(page, `18-settings-sync-connected-${theme}`);
    await tid(page, "settings-nav-shortcuts").click().catch(() => {});
    await shot(page, `18-settings-shortcuts-${theme}`);
    await tid(page, "dialog-close").click();
    await node(page, "Billing").click({ button: "right" });
    await page.getByTestId("tree-share").click();
    await shot(page, `17-share-${theme}`);
    await tid(page, "share-include-credentials").check().catch(() => tid(page, "share-include-credentials").click());
    await shot(page, `17-share-credentials-${theme}`);
    await tid(page, "share-create").click();
    await tid(page, "share-url").waitFor();
    await shot(page, `17-share-result-${theme}`);
    await tid(page, "dialog-close").click();
    await shot(page, `18-sync-indicator-${theme}`);
    // confirm delete
    await node(page, "Billing").click({ button: "right" });
    await page.getByTestId("tree-delete").click();
    await shot(page, `19-confirm-delete-${theme}`);
    await page.keyboard.press("Escape");
    await ctx.close();
  });

  scene(`empty-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L, "?seed=empty");
    await shot(page, `20-empty-activity-${theme}`);
    await tid(page, "sidebar-tab-collections").click();
    await shot(page, `20-empty-collections-${theme}`);
    await tid(page, "sidebar-tab-env").click();
    await shot(page, `20-empty-env-${theme}`);
    await ctx.close();
  });

  scene(`rail-${theme}`, async () => {
    const { ctx, page } = await open(theme, VP.L);
    await mainState(page);
    await tid(page, "url-input").click();
    await page.keyboard.press("ControlOrMeta+b");
    await page.mouse.move(5, 895);
    await shot(page, `21-rail-${theme}`);
    await tid(page, "layout-toggle").click();
    await shot(page, `22-stacked-forced-1440-${theme}`);
    await ctx.close();
  });
}

// mockup
for (const theme of ["dark", "light"]) {
  scene(`mockup-${theme}`, async () => {
    const ctx = await browser.newContext({ viewport: VP.L, colorScheme: theme });
    const page = await ctx.newPage();
    await page.goto("file:///app/docs/design/mockup.html");
    await page.evaluate((t) => document.documentElement.setAttribute("data-theme", t), theme);
    await page.waitForTimeout(200);
    await page.screenshot({ path: `${OUT}/00-mockup-1440-${theme}.png` });
    await page.screenshot({ path: `${OUT}/00-mockup-full-${theme}.png`, fullPage: true });
    log("  -> mockup", theme);
    metrics[`mockup-${theme}`] = await page.evaluate(() => {
      const q = (s) => document.querySelector(s);
      const panes = [...document.querySelectorAll(".app > .pane")];
      return panes.map((p) => [...p.children].slice(0, 4).map((c) => { const r = c.getBoundingClientRect(); return { cls: c.className, y: Math.round(r.y), h: Math.round(r.height) }; }));
    });
    await ctx.close();
  });
}

for (const s of scenes) {
  if (ONLY && !ONLY.test(s.name)) continue;
  log("scene", s.name);
  try { await s.fn(); } catch (e) { log("  FAILED", s.name, e.message.split("\n")[0]); }
}
fs.writeFileSync(`${OUT}/../metrics.json`, JSON.stringify(metrics, null, 2));
await browser.close();

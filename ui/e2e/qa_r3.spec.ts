import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { mkdirSync, writeFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { openApp, noHorizontalOverflow, recordCalls, calls } from "./qa_helpers";

/** QA round 3 — a11y (axe-core), narrow widths / zoom (DR-001), Send↔Cancel width (DR-005), long inputs, rapid Send/Cancel. */
const repo = fileURLToPath(new URL("../../", import.meta.url));
const OUT = `${repo}qa/r2/out`;

async function inViewport(page: Page, testid: string) {
  const b = await page.getByTestId(testid).boundingBox();
  const vw = page.viewportSize()!.width;
  return { box: b, ok: !!b && b.x >= 0 && b.x + b.width <= vw + 0.5 };
}

// ───────────────────────── accessibility
const AXE_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa"];
async function axe(page: Page, name: string) {
  const r = await new AxeBuilder({ page }).withTags(AXE_TAGS).analyze();
  const v = r.violations.map((x) => ({ id: x.id, impact: x.impact, nodes: x.nodes.length, sample: x.nodes.slice(0, 3).map((n) => n.target.join(" ")), help: x.help }));
  mkdirSync(OUT, { recursive: true });
  writeFileSync(`${OUT}/axe-${name}.json`, JSON.stringify(v, null, 2));
  return v;
}

for (const scheme of ["dark", "light"] as const) {
  test(`axe: main screen + dialogs have no serious/critical WCAG A/AA violations (${scheme})`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    await openApp(page);
    const all: Record<string, unknown[]> = {};
    await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
    await page.getByTestId("send-button").click();
    await expect(page.getByTestId("response-status")).toHaveText(/^200/);
    all.main = await axe(page, `main-${scheme}`);
    await page.getByTestId("sidebar-tab-collections").click();
    all.collections = await axe(page, `collections-${scheme}`);
    await page.getByTestId("import-button").click();
    await expect(page.getByTestId("import-dialog")).toBeVisible();
    all.importCurl = await axe(page, `import-${scheme}`);
    await page.keyboard.press("Escape");
    await page.getByTestId("settings-button").click();
    await expect(page.getByTestId("settings-dialog")).toBeVisible();
    all.settings = await axe(page, `settings-${scheme}`);
    await page.keyboard.press("Escape");
    await page.getByTestId("env-selector").click();
    await expect(page.getByTestId("env-picker")).toBeVisible();
    all.envPicker = await axe(page, `envpicker-${scheme}`);
    await page.keyboard.press("Escape");
    const serious = Object.entries(all).flatMap(([screen, v]) =>
      (v as { id: string; impact: string; nodes: number; sample: string[] }[]).filter((x) => x.impact === "serious" || x.impact === "critical").map((x) => `${screen}: ${x.id} (${x.impact}, ${x.nodes} nodes) e.g. ${x.sample[0]}`),
    );
    console.log(`axe ${scheme}:`, JSON.stringify(Object.fromEntries(Object.entries(all).map(([k, v]) => [k, (v as { id: string; impact: string }[]).map((x) => `${x.id}/${x.impact}`)]))));
    expect(serious, serious.join("\n")).toEqual([]);
  });
}

// ───────────────────────── narrow widths and zoom (DR-001)
for (const vp of [
  { width: 800, height: 600, label: "800×600" },
  { width: 853, height: 533, label: "1280×800 at 150 % zoom" },
  { width: 768, height: 480, label: "960×600 min window at 125 % zoom" },
]) {
  test(`DR-001: ${vp.label} — Send, Save, ⋯ on screen, no horizontal overflow, send/import flows work`, async ({ page }) => {
    await page.setViewportSize({ width: vp.width, height: vp.height });
    const errors = await openApp(page);
    for (const id of ["send-button", "save-request", "request-menu", "url-input"]) {
      const r = await inViewport(page, id);
      expect(r.ok, `${id} at ${JSON.stringify(r.box)} in ${vp.width}px`).toBe(true);
    }
    const o = await noHorizontalOverflow(page);
    expect(o.scrollW, `offenders: ${o.offenders.join(", ")}`).toBeLessThanOrEqual(o.innerW);
    await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
    await page.getByTestId("send-button").click();
    await expect(page.getByTestId("response-status")).toHaveText(/^200/);
    await expect(page.getByTestId("response-status")).toBeInViewport();
    await page.keyboard.press("ControlOrMeta+u");
    await expect(page.getByTestId("import-dialog")).toBeVisible();
    const sub = await inViewport(page, "import-submit");
    expect(sub.ok, `import submit ${JSON.stringify(sub.box)}`).toBe(true);
    await page.keyboard.press("Escape");
    expect(errors).toEqual([]);
  });
}

// ───────────────────────── DR-005: Send ↔ Cancel keeps its width
test("DR-005: Send and Cancel have the same width; the URL field does not move", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").fill("https://api.example.com/delay/5");
  const send = await page.getByTestId("send-button").boundingBox();
  const url = await page.getByTestId("url-input").boundingBox();
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("send-button")).toHaveText(/Cancel/);
  const cancel = await page.getByTestId("send-button").boundingBox();
  const url2 = await page.getByTestId("url-input").boundingBox();
  await page.getByTestId("send-button").click();
  expect(Math.abs(cancel!.width - send!.width)).toBeLessThanOrEqual(0.5);
  expect(Math.abs(url2!.width - url!.width)).toBeLessThanOrEqual(0.5);
});

// ───────────────────────── very long URL / headers
test("10 KB URL and a 4 KB header value: layout holds, send works, export contains them", async ({ page }) => {
  const errors = await openApp(page);
  const long = "https://api.example.com/v1/users?q=" + "x".repeat(10_000);
  await page.getByTestId("url-input").fill(long);
  await page.getByTestId("req-tab-headers").click();
  await page.getByTestId("headers-ghost").getByTestId("headers-key").fill("X-Long");
  await page.getByTestId("headers-row").first().getByTestId("headers-value").fill("v".repeat(4096));
  const o = await noHorizontalOverflow(page);
  expect(o.scrollW, `offenders: ${o.offenders.join(", ")}`).toBeLessThanOrEqual(o.innerW);
  expect((await inViewport(page, "send-button")).ok).toBe(true);
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/^\d{3}/);
  await page.keyboard.press("ControlOrMeta+Shift+c");
  await expect.poll(() => page.evaluate(() => ((window as unknown as { __lastCopied?: string }).__lastCopied ?? "").length)).toBeGreaterThan(14_000);
  await page.getByTestId("sidebar-tab-activity").click();
  const o2 = await noHorizontalOverflow(page);
  expect(o2.scrollW).toBeLessThanOrEqual(o2.innerW);
  expect(errors).toEqual([]);
});

// ───────────────────────── rapid Send / Cancel
test("rapid Send/Cancel ×20: ends idle, one history entry per send, no stuck Cancel, no errors", async ({ page }) => {
  const errors = await openApp(page, "?seed=empty");
  await recordCalls(page);
  await page.getByTestId("url-input").fill("https://api.example.com/delay/2");
  const btn = page.getByTestId("send-button");
  for (let i = 0; i < 20; i++) {
    await btn.click(); // send
    await btn.click(); // cancel
  }
  await expect(btn).toHaveText(/Send/, { timeout: 5000 });
  await expect(page.getByTestId("response-status")).toHaveText("Cancelled");
  const sends = (await calls(page, "send_request")).length;
  const hist = await page.evaluate(async () => {
    const api = await import("/src/lib/api/index.ts" as string);
    return (await api.call("list_history", {})).length;
  });
  expect(sends).toBe(20);
  expect(hist).toBe(20);
  // keyboard variant: Mod+Enter ×2 quickly, then a real send completes
  await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
  for (let i = 0; i < 10; i++) await page.keyboard.press("ControlOrMeta+Enter");
  await expect(btn).toHaveText(/Send/, { timeout: 5000 });
  await btn.click();
  await expect(page.getByTestId("response-status")).toHaveText(/^200/);
  expect(errors).toEqual([]);
});

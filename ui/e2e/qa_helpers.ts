import { expect, type Page } from "@playwright/test";

/** QA round 2 helpers (browser mock). */
export async function openApp(page: Page, qs = "") {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto("/" + qs);
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

/** Record every backend call (command + args) made by the UI from now on. */
export async function recordCalls(page: Page) {
  await page.evaluate(async () => {
    const api = await import("/src/lib/api/index.ts" as string);
    const b = await api.getBackend();
    const w = window as unknown as { __calls: [string, unknown][] };
    w.__calls = [];
    const orig = b.invoke.bind(b);
    b.invoke = (c: string, a: unknown) => {
      w.__calls.push([c, JSON.parse(JSON.stringify(a ?? {}))]);
      return orig(c, a);
    };
  });
}

export async function calls(page: Page, cmd?: string): Promise<[string, Record<string, unknown>][]> {
  const all = await page.evaluate(() => (window as unknown as { __calls: [string, Record<string, unknown>][] }).__calls ?? []);
  return cmd ? all.filter(([c]) => c === cmd) : all;
}

export const node = (page: Page, name: string) => page.locator(`[data-testid="tree-node"][data-name="${name}"]`);

export async function noHorizontalOverflow(page: Page) {
  return page.evaluate(() => {
    const d = document.documentElement;
    const offenders = [...document.querySelectorAll<HTMLElement>("body *")]
      .filter((el) => {
        const r = el.getBoundingClientRect();
        return r.width > 0 && r.right > window.innerWidth + 1 && getComputedStyle(el).position !== "fixed";
      })
      .slice(0, 5)
      .map((el) => `${el.tagName.toLowerCase()}.${el.className}`.slice(0, 80));
    return { scrollW: d.scrollWidth, innerW: window.innerWidth, offenders };
  });
}

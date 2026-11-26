import { expect, test, type Page } from "@playwright/test";

/** DR-001 regression: the app must reflow (never clip) at narrow widths and at ⌘+ zoom (DESIGN §2.1, §8). */

async function sendIsUsable(page: Page) {
  const send = page.getByTestId("send-button");
  await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
  await expect(send).toBeVisible();
  await expect(send).toBeInViewport({ ratio: 1 });
  const box = (await send.boundingBox())!;
  const vw = await page.evaluate(() => window.innerWidth);
  expect(box.x + box.width).toBeLessThanOrEqual(vw);
  await send.click();
  await expect(page.getByTestId("response-status")).toHaveText(/^200 OK/);
  // nothing is clipped horizontally
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth + 1)).toBe(true);
}

const cases = [
  { name: "800×600 @1x", width: 800, height: 600, dsf: 1 },
  { name: "1280×800 @1.5 (853 CSS px)", width: 853, height: 533, dsf: 1.5 },
  { name: "960×600 @1.5 (640 CSS px, auto-rail)", width: 640, height: 400, dsf: 1.5 },
];

for (const c of cases) {
  test.describe(c.name, () => {
    test.use({ viewport: { width: c.width, height: c.height }, deviceScaleFactor: c.dsf });
    test("Send stays visible and clickable", async ({ page }) => {
      await page.goto("/");
      await expect(page.getByTestId("app")).toBeVisible();
      await sendIsUsable(page);
      if (c.width - 300 < 360) await expect(page.getByTestId("sidebar-rail")).toBeVisible();
    });
  });
}

test.describe("1280×800 with 150 % page zoom", () => {
  test.use({ viewport: { width: 1280, height: 800 }, deviceScaleFactor: 1.5 });
  test("Send stays visible and clickable under CSS zoom", async ({ page }) => {
    await page.goto("/");
    await expect(page.getByTestId("app")).toBeVisible();
    await page.evaluate(() => ((document.documentElement.style as CSSStyleDeclaration & { zoom: string }).zoom = "1.5"));
    const send = page.getByTestId("send-button");
    await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
    const box = (await send.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(1280);
    await send.click();
    await expect(page.getByTestId("response-status")).toHaveText(/^200 OK/);
  });
});

test("auto-rail is not persisted and the sidebar returns when there is room", async ({ page }) => {
  await page.setViewportSize({ width: 640, height: 500 });
  await page.goto("/");
  await expect(page.getByTestId("sidebar-rail")).toBeVisible();
  await page.setViewportSize({ width: 1280, height: 800 });
  await expect(page.getByTestId("sidebar")).toBeVisible();
});

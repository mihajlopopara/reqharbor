import { expect, test, type Page } from "@playwright/test";

/** Smoke tests against `vite dev` + the in-memory browser mock backend. */

async function openApp(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto("/");
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

test("loads the three-pane app with seeded history", async ({ page }) => {
  const errors = await openApp(page);
  await expect(page.getByTestId("sidebar-tab-activity")).toHaveAttribute("aria-selected", "true");
  await expect(page.getByTestId("history-row").first()).toBeVisible();
  await expect(page.getByTestId("response-empty")).toBeVisible();
  expect(errors).toEqual([]);
});

test("select a history row, edit the URL, send and see 200", async ({ page }) => {
  const errors = await openApp(page);
  const row = page.locator('[data-testid="history-row"][data-method="GET"]').first();
  await row.click();
  await expect(page.getByTestId("response-status")).toBeVisible();

  const url = page.getByTestId("url-input");
  await url.fill("https://api.example.com/v1/users?limit=5");
  await page.getByTestId("req-tab-query").click();
  await expect(page.getByTestId("query-row")).toHaveCount(1);
  await expect(page.getByTestId("query-row").getByTestId("query-key").first()).toHaveValue("limit");

  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/^200 OK/);
  await expect(page.getByTestId("response-size")).toContainText(/B$/);
  await expect(page.getByTestId("response-time")).toContainText("ms");
  await expect(page.getByTestId("response-body")).toContainText("Ada Lovelace");
  expect(errors).toEqual([]);
});

test("query tab edits rewrite the URL", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").fill("https://api.example.com/search?q=a&q=b");
  await page.getByTestId("req-tab-query").click();
  await expect(page.getByTestId("query-row")).toHaveCount(2);
  await page.getByTestId("query-row").nth(1).getByTestId("query-enabled").uncheck();
  await expect(page.getByTestId("url-input")).toHaveValue("https://api.example.com/search?q=a");
  await page.getByTestId("query-ghost").getByTestId("query-key").fill("page");
  await expect(page.getByTestId("url-input")).toHaveValue("https://api.example.com/search?q=a&page");
});

test("QUERY method is available and keyboard accessible", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("method-select").click();
  await expect(page.getByTestId("method-option-QUERY")).toBeVisible();
  await page.getByTestId("method-option-QUERY").click();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "QUERY");
  await page.getByTestId("req-tab-body").click();
  await expect(page.getByTestId("query-body-hint")).toBeVisible();
  // type-ahead on the trigger
  await page.getByTestId("method-select").focus();
  await page.keyboard.press("g");
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "GET");
  await page.keyboard.press("q");
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "QUERY");
});

test("custom method validation", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-CUSTOM").click();
  const input = page.getByTestId("method-custom-input");
  await input.fill("1bad");
  await expect(page.getByTestId("method-custom-error")).toBeVisible();
  await input.fill("purge");
  await input.press("Enter");
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "PURGE");
});

test("import a cURL command via the ⌘U dialog", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").click();
  await page.keyboard.press("ControlOrMeta+u");
  const dlg = page.getByTestId("import-dialog");
  await expect(dlg).toBeVisible();
  await page.getByTestId("curl-input").fill(
    `curl -X QUERY 'https://api.example.com/v1/users/search' \\\n  -H 'Content-Type: application/json' \\\n  -H 'X-Trace: 1' \\\n  --data-raw '{"role":"admin"}'`,
  );
  await expect(page.getByTestId("curl-preview-method")).toHaveText("QUERY");
  await expect(page.getByTestId("curl-preview-summary")).toContainText("2 headers · JSON body");
  await page.getByTestId("import-submit").click();
  await expect(dlg).toBeHidden();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "QUERY");
  await expect(page.getByTestId("url-input")).toHaveValue("https://api.example.com/v1/users/search");
  await expect(page.getByTestId("badge-headers")).toHaveText("2");
});

test("not a curl command shows an error and changes nothing", async ({ page }) => {
  await openApp(page);
  await page.keyboard.press("ControlOrMeta+u");
  await page.getByTestId("curl-input").fill("wget https://example.com");
  await expect(page.getByTestId("curl-error")).toHaveText("Not a cURL command");
  await expect(page.getByTestId("import-submit")).toBeDisabled();
});

test("collections tree, environment selector and share dialog", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("sidebar-tab-collections").click();
  const users = page.locator('[data-testid="tree-node"][data-name="Users API"]');
  await users.click();
  await expect(page.getByTestId("collection-settings")).toBeVisible();
  await page.locator('[data-testid="tree-node"][data-name="Users"]').click();
  await page.locator('[data-testid="tree-node"][data-name="Search users"]').click();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "QUERY");
  await expect(page.getByTestId("active-env")).toHaveText("Staging");
  await page.getByTestId("env-selector").click();
  await page.locator('[data-testid="env-option"][data-name="Local"]').click();
  await expect(page.getByTestId("active-env")).toHaveText("Local");
  await page.getByTestId("request-menu").click();
  await page.getByTestId("menu-share").click();
  await expect(page.getByTestId("share-dialog")).toBeVisible();
  await expect(page.getByTestId("share-needs-sync")).toBeVisible();
});

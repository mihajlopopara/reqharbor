import { expect, test, type Page } from "@playwright/test";

async function openApp(page: Page, qs = "") {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/" + qs);
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

const node = (page: Page, name: string) => page.locator(`[data-testid="tree-node"][data-name="${name}"]`);

test("tree context menu: rename, duplicate, delete with child counts", async ({ page }) => {
  const errors = await openApp(page);
  await page.getByTestId("sidebar-tab-collections").click();
  await node(page, "Billing").click({ button: "right" });
  await page.getByTestId("tree-rename").click();
  const input = page.getByTestId("tree-rename-input");
  await input.fill("Payments");
  await input.press("Enter");
  await expect(node(page, "Payments")).toBeVisible();

  await node(page, "Payments").click({ button: "right" });
  await page.getByTestId("tree-duplicate").click();
  await expect(node(page, "Payments copy")).toBeVisible();

  await node(page, "Payments copy").click({ button: "right" });
  await page.getByTestId("tree-delete").click();
  await expect(page.getByTestId("confirm-dialog")).toContainText("This also deletes 1 folder and 3 requests.");
  await page.getByTestId("confirm-ok").click();
  await expect(node(page, "Payments copy")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("dirty dot, ⌘S save and scratch Save-to picker", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("sidebar-tab-collections").click();
  await node(page, "Users API").click();
  await node(page, "Health").click();
  await page.getByTestId("url-input").fill("{{baseUrl}}/healthz");
  await expect(page.getByTestId("dirty-dot")).toBeVisible();
  await expect(node(page, "Health").getByTestId("tree-dirty")).toBeVisible();
  await page.getByTestId("url-input").press("ControlOrMeta+s");
  await expect(page.getByTestId("dirty-dot")).toHaveCount(0);

  await page.getByTestId("new-request").click();
  await page.getByTestId("url-input").fill("https://api.example.com/ping");
  await page.getByTestId("save-request").click();
  await page.getByTestId("picker-name").fill("Ping");
  await page.locator('[data-testid="picker-node"][data-name="Users"]').click();
  await page.getByTestId("picker-submit").click();
  await expect(node(page, "Ping")).toBeVisible();
  await expect(page.getByTestId("breadcrumb")).toContainText("Users API");
});

test("pasting a cURL command into the URL bar imports it (with Undo)", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").focus();
  await page.evaluate(() => {
    const dt = new DataTransfer();
    dt.setData("text/plain", "curl -X PUT https://api.example.com/v1/teams/1 -H 'X-A: 1' --data-raw '{\"n\":1}'");
    document.activeElement!.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  });
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "PUT");
  await expect(page.getByTestId("url-input")).toHaveValue("https://api.example.com/v1/teams/1");
  const toast = page.getByTestId("toast").filter({ hasText: "Imported from cURL" });
  await toast.getByRole("button", { name: "Undo" }).click();
  await expect(page.getByTestId("url-input")).toHaveValue("");
});

test("OpenAPI import from pasted JSON", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("import-button").click();
  await page.getByTestId("import-tab-openapi").click();
  const spec = {
    openapi: "3.1.0",
    info: { title: "Store API" },
    servers: [{ url: "https://store.dev" }],
    paths: {
      "/items": { get: { tags: ["items"], summary: "List items" }, post: { tags: ["items"], summary: "Add item" } },
      "/items/{id}": { delete: { tags: ["items"], operationId: "deleteItem" } },
    },
  };
  await page.getByTestId("openapi-input").fill(JSON.stringify(spec));
  await expect(page.getByTestId("openapi-chip")).toHaveText("OpenAPI 3.1 · JSON");
  await expect(page.getByTestId("import-submit")).toHaveText("Import 3 requests");
  await page.getByTestId("import-submit").click();
  await expect(page.getByTestId("import-dialog")).toBeHidden();
  await expect(node(page, "Store API")).toBeVisible();
  await expect(page.getByTestId("collection-settings")).toBeVisible();
});

test("environment editor masks secrets and history filter works", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("sidebar-tab-env").click();
  await page.locator('[data-testid="env-row"][data-name="Staging"]').click();
  const tokenRow = page.getByTestId("env-vars-row").nth(1);
  await expect(tokenRow.getByTestId("env-vars-value")).toHaveAttribute("type", "password");
  await page.getByTestId("env-vars-ghost").getByTestId("env-vars-key").fill("region");
  await expect(page.getByTestId("env-vars-row")).toHaveCount(5);

  await page.getByTestId("sidebar-tab-activity").click();
  await page.getByTestId("sidebar-filter").fill("503");
  await expect(page.getByTestId("history-row")).toHaveCount(1);
  await page.getByTestId("sidebar-filter").fill("QUERY");
  await expect(page.getByTestId("history-row")).toHaveCount(1);
  await expect(page.getByTestId("history-row").first()).toHaveAttribute("data-method", "QUERY");
});

test("network error panel and cancel", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").fill("https://nope.invalid/x");
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-error")).toContainText("Couldn't resolve host");
  await page.getByTestId("url-input").fill("https://api.example.com/delay/5");
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("send-button")).toHaveText(/Cancel/);
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText("Cancelled");
});

test("drag and drop moves a request into another collection", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("sidebar-tab-collections").click();
  await node(page, "Users API").click();
  const health = node(page, "Health");
  const billing = node(page, "Billing");
  const hb = (await health.boundingBox())!;
  const bb = (await billing.boundingBox())!;
  await page.mouse.move(hb.x + 60, hb.y + hb.height / 2);
  await page.mouse.down();
  await page.mouse.move(hb.x + 60, hb.y + hb.height / 2 + 10, { steps: 3 });
  await page.mouse.move(bb.x + 60, bb.y + bb.height / 2, { steps: 8 });
  await page.mouse.up();
  await expect(node(page, "Users API")).toContainText("5");
  await expect(node(page, "Billing")).toContainText("4");
  await expect(node(page, "Health")).toHaveAttribute("data-kind", "request");
});

test("sync setup, share a collection, import it from the link", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await page.getByTestId("sync-worker-url").fill("https://sync.example.dev");
  await page.getByTestId("sync-connect").click();
  await expect(page.getByTestId("sync-token")).toHaveValue(/^rh_/);
  await expect(page.getByTestId("sync-usage")).toContainText("of 50 MB");
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("sync-indicator")).toContainText("Synced");

  await page.getByTestId("sidebar-tab-collections").click();
  await node(page, "Billing").click({ button: "right" });
  await page.getByTestId("tree-share").click();
  await expect(page.getByTestId("share-dialog")).toContainText("Removed before sharing");
  await page.getByTestId("share-create").click();
  const url = await page.getByTestId("share-url").inputValue();
  expect(url).toMatch(/^https:\/\/sync\.example\.dev\/s\//);
  await page.getByTestId("dialog-close").click();

  await page.getByTestId("new-menu").click();
  await page.getByTestId("menu-import-link").click();
  await page.getByTestId("link-input").fill(url);
  await expect(page.getByTestId("link-preview")).toContainText("Billing");
  await expect(page.getByTestId("link-preview")).toContainText("cleared");
  await page.getByTestId("import-submit").click();
  await expect(node(page, "Billing")).toHaveCount(2);
});

test("sidebar collapses to a rail and back (⌘B / ⌘1)", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("url-input").click();
  await page.keyboard.press("ControlOrMeta+b");
  await expect(page.getByTestId("sidebar-rail")).toBeVisible();
  await page.keyboard.press("ControlOrMeta+2");
  await expect(page.getByTestId("sidebar")).toBeVisible();
  await expect(page.getByTestId("sidebar-tab-collections")).toHaveAttribute("aria-selected", "true");
});

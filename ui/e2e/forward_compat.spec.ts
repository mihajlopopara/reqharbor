import { expect, test } from "@playwright/test";
import { calls, node, openApp, recordCalls } from "./qa_helpers";

/**
 * Accounts plan M0 / AC-FWD-3: requests written by a newer ReqHarbor (a gRPC kind, an unknown
 * body type) show a "Requires a newer ReqHarbor" badge and open view-only. `?newer=1` seeds them.
 */
const NEEDS = "This request needs a newer version of ReqHarbor";

test.beforeEach(async ({ page }) => {
  const errors = await openApp(page, "?newer=1&platform=linux");
  test.info().annotations.push({ type: "errors", description: errors.join("\n") });
  await page.getByTestId("sidebar-tab-collections").click();
  await node(page, "Users API").click();
});

test("AC-FWD-3: tree badge, kind label and Open/Delete-only menu", async ({ page }) => {
  const grpc = node(page, "Say hello");
  await expect(grpc).toBeVisible();
  await expect(grpc.getByTestId("newer-badge")).toHaveAttribute("aria-label", "Requires a newer ReqHarbor");
  await expect(grpc.getByTestId("tree-kind")).toHaveText("MQTT");
  const proto = node(page, "Upload protobuf");
  await expect(proto.getByTestId("newer-badge")).toBeVisible();
  await expect(proto.getByTestId("tree-kind")).toHaveText("NEW");
  // Ordinary requests carry no badge.
  await expect(node(page, "Health")).toBeVisible();
  await expect(node(page, "Health").getByTestId("newer-badge")).toHaveCount(0);

  await grpc.click({ button: "right" });
  const menu = page.getByTestId("context-menu");
  await expect(menu.getByRole("menuitem")).toHaveText(["Open", "Delete"]);
  await page.keyboard.press("Escape");

  // F2 / double-click never start a rename.
  await grpc.dblclick();
  await expect(page.getByTestId("tree-rename-input")).toHaveCount(0);
});

test("AC-FWD-3: a newer request opens read-only; Save and Send never reach the backend", async ({ page }) => {
  await recordCalls(page);
  await node(page, "Say hello").click();
  await expect(page.getByTestId("read-only-banner")).toContainText("Requires a newer ReqHarbor");
  await expect(page.getByTestId("url-input")).toBeDisabled();
  await expect(page.getByTestId("send-button")).toBeDisabled();
  await expect(page.getByTestId("send-button")).toHaveAttribute("title", NEEDS);
  await expect(page.getByTestId("save-request")).toBeDisabled();
  await expect(page.getByTestId("save-request")).toHaveAttribute("title", /newer version of ReqHarbor/);
  // Body tab explains instead of showing an editor.
  await expect(page.getByTestId("body-needs-newer")).toContainText(NEEDS);
  await expect(page.getByTestId("body-editor")).toHaveCount(0);

  // Other tabs are visible but every control is disabled.
  await page.getByTestId("req-tab-auth").click();
  await expect(page.getByTestId("auth-type")).toBeDisabled();
  await page.getByTestId("req-tab-headers").click();
  const inputs = page.getByTestId("req-panel-headers").locator("input");
  const n = await inputs.count();
  expect(n).toBeGreaterThan(0);
  for (let i = 0; i < n; i++) await expect(inputs.nth(i)).toBeDisabled();
  await page.getByTestId("request-settings").click();
  await expect(page.getByTestId("timeout-input")).toBeDisabled();
  await page.keyboard.press("Escape");

  // Keyboard shortcuts are refused with an explanation.
  await page.getByTestId("req-tab-query").click();
  await page.keyboard.press("Control+s");
  await page.keyboard.press("Control+Enter");
  await expect(page.getByTestId("send-blocked")).toHaveText(NEEDS);
  const touched = (await calls(page)).filter(([c]) => /save_request|send_request|rename_request|move_node|duplicate_request/.test(c));
  expect(touched).toEqual([]);

  // The request menu only allows Delete.
  await page.getByTestId("request-menu").click();
  const items = page.getByTestId("context-menu").getByRole("menuitem");
  for (const label of ["Copy as cURL", "Share…", "Rename", "Duplicate", "Move to…"]) {
    await expect(items.filter({ hasText: label })).toHaveAttribute("aria-disabled", "true");
  }
  await expect(items.filter({ hasText: "Delete" })).not.toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("Escape");
});

test("AC-FWD-3: an unknown body type is read-only too, and ordinary requests stay editable", async ({ page }) => {
  await node(page, "Upload protobuf").click();
  await expect(page.getByTestId("read-only-banner")).toBeVisible();
  await expect(page.getByTestId("body-needs-newer")).toContainText("protobuf");
  await expect(page.getByTestId("send-button")).toBeDisabled();

  await node(page, "Health").click();
  await expect(page.getByTestId("read-only-banner")).toHaveCount(0);
  await expect(page.getByTestId("url-input")).toBeEnabled();
  await expect(page.getByTestId("send-button")).toBeEnabled();
});

test("AC-FWD-3: a newer request can still be deleted", async ({ page }) => {
  await node(page, "Say hello").click({ button: "right" });
  await page.getByTestId("tree-delete").click();
  await page.getByTestId("confirm-ok").click();
  await expect(node(page, "Say hello")).toHaveCount(0);
});

test("AC-FWD-3/4: a pull that makes the open, edited request read-only switches the editor and offers a copy", async ({ page }) => {
  await node(page, "Health").click();
  const url = page.getByTestId("url-input");
  await url.click();
  await page.keyboard.press("End");
  await page.keyboard.type("/edited");
  await expect(page.getByTestId("dirty-dot")).toBeVisible();

  // Another device running a newer ReqHarbor raised `minClientVersion` (the body stays JSON, so the
  // reason must name the version, not the body type), then the pull lands.
  await page.evaluate(() => {
    type M = { requests: Array<Record<string, unknown>>; emit: (e: string, p: unknown) => void };
    const m = (window as unknown as { __mock: M }).__mock;
    const r = m.requests.find((x) => x.name === "Health")!;
    r.minClientVersion = "9.0.0";
    r.body = { type: "json", text: "{}" };
    r.updatedAt = Date.now();
    m.emit("sync://changed", { pushed: 0, pulled: 1, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 1 });
  });

  await expect(page.getByTestId("read-only-banner")).toBeVisible();
  await expect(page.getByTestId("dirty-dot")).toHaveCount(0);
  await expect(url).toBeDisabled();
  await expect(url).not.toHaveValue(/\/edited/);
  await expect(page.getByTestId("send-button")).toBeDisabled();
  await expect(node(page, "Health").getByTestId("newer-badge")).toBeVisible();
  await page.getByTestId("req-tab-body").click();
  await expect(page.getByTestId("read-only-reason")).toHaveText("It needs ReqHarbor 9.0.0 or later");

  const toast = page.getByTestId("toast").filter({ hasText: "now read-only here" });
  await expect(toast).toContainText("“Health” was changed with a newer version of ReqHarbor");
  await toast.getByRole("button", { name: "Save my edits as a copy" }).click();
  await expect(node(page, "Health (my edits)")).toBeVisible();
  await expect(page.getByTestId("read-only-banner")).toHaveCount(0);
  await expect(page.getByTestId("url-input")).toHaveValue(/\/edited$/);
  await expect(page.getByTestId("url-input")).toBeEnabled();
});

test("AC-FWD-3: a folder or collection holding a newer request can't be deleted with it", async ({ page }) => {
  await node(page, "Users API").click({ button: "right" });
  await page.getByTestId("tree-delete").click();
  await page.getByTestId("confirm-ok").click();
  await expect(page.getByTestId("toast").filter({ hasText: "Delete “Say hello” on its own first" })).toBeVisible();
  await expect(node(page, "Users API")).toBeVisible();
  await expect(node(page, "Say hello")).toBeVisible();
});

test("AC-FWD-3: a collection holding a newer request is only shared with credentials included", async ({ page }) => {
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await page.getByTestId("sync-worker-url").fill("https://sync.example.dev");
  await page.getByTestId("sync-connect").click();
  await expect(page.getByTestId("sync-token")).toHaveValue(/^rh_/);
  await page.getByTestId("dialog-close").click();

  await node(page, "Users API").click({ button: "right" });
  await page.getByTestId("tree-share").click();
  const dialog = page.getByTestId("share-dialog");
  await expect(dialog.getByRole("alert")).toContainText("“Say hello” was made with a newer version of ReqHarbor");
  await expect(dialog.getByRole("alert")).toContainText("Turn on Include credentials");
  await expect(page.getByTestId("share-create")).toBeDisabled();
  await page.getByTestId("share-include-credentials").check();
  await expect(dialog.getByRole("alert")).toHaveCount(0);
  await expect(page.getByTestId("share-create")).toBeEnabled();
  await page.getByTestId("share-create").click();
  await expect(page.getByTestId("share-url")).toHaveValue(/\/s\//);
});

test("AC-FWD-4: a collection with a newer minClientVersion shows the badge and can't be renamed", async ({ page }) => {
  // A pull from a device running a newer ReqHarbor raised the collection's `minClientVersion`.
  await page.evaluate(() => {
    type M = { collections: Array<Record<string, unknown>>; emit: (e: string, p: unknown) => void };
    const m = (window as unknown as { __mock: M }).__mock;
    m.collections.find((c) => c.name === "Billing")!.minClientVersion = "99.0.0";
    m.emit("sync://changed", { pushed: 0, pulled: 1, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 1 });
  });
  const billing = node(page, "Billing");
  await expect(billing.getByTestId("newer-badge")).toBeVisible();
  await billing.click({ button: "right" });
  const menu = page.getByTestId("context-menu");
  await expect(menu.getByRole("menuitem")).toHaveText(["New request", "New folder", "Share…", "Delete"]);
  await page.keyboard.press("Escape");
  await billing.dblclick();
  await expect(page.getByTestId("tree-rename-input")).toHaveCount(0);
});

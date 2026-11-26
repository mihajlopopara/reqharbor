import { expect, test } from "@playwright/test";
import { openApp, recordCalls, calls } from "./qa_helpers";

/**
 * QA round 2 — UI ↔ Rust contract repros found by the real-app shape diff (qa/r2/shape_diff.mjs).
 * The real `save_settings` deserialises `sidebarWidth` as u32: a fractional value is rejected with
 *   "invalid args `settings` for command `save_settings`: invalid type: floating point `312.5`, expected u32"
 * and the UI swallows the error, so no setting is persisted any more. The browser mock accepts it.
 */
test("BUG QA-R2-001: dragging the sidebar splitter by a fractional pixel amount must save an integer sidebarWidth (Rust u32)", async ({ page }) => {
  await openApp(page);
  await recordCalls(page);
  const sep = page.getByRole("separator", { name: "Resize sidebar" });
  const box = (await sep.boundingBox())!;
  const x = box.x + box.width / 2;
  const y = box.y + box.height / 2;
  await page.mouse.move(x, y);
  await page.mouse.down();
  await page.mouse.move(x + 20.5, y, { steps: 3 }); // HiDPI (e.g. Windows 125 %/150 %) pointer deltas are fractional
  await page.mouse.up();
  await expect.poll(async () => (await calls(page, "save_settings")).length, { timeout: 3000 }).toBeGreaterThan(0);
  const widths = (await calls(page, "save_settings")).map(([, a]) => (a.settings as { sidebarWidth: number }).sidebarWidth);
  for (const w of widths) expect(Number.isInteger(w), `sidebarWidth sent to save_settings: ${w}`).toBe(true);
});

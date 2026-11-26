import { expect, test, type Page } from "@playwright/test";
import { openApp, node } from "./qa_helpers";

/**
 * QA round 2 — PRODUCT §6 keyboard shortcuts, AC-KEY-1 (focus anywhere incl. CodeMirror) and AC-KEY-2.
 * Runs once per platform flavour: ?platform=linux (Ctrl) and ?platform=macos (⌘ = Meta).
 */
for (const [platform, mod] of [
  ["linux", "Control"],
  ["macos", "Meta"],
] as const) {
  test.describe(`shortcuts on ${platform}`, () => {
    const press = (page: Page, combo: string) => page.keyboard.press(combo.replace("Mod", mod));

    test(`AC-KEY-2: empty state lists Send/Import/Env/New with the ${platform} modifier`, async ({ page }) => {
      await openApp(page, `?platform=${platform}`);
      await page.getByTestId("new-request").click();
      const empty = page.getByTestId("response-empty");
      await expect(empty).toBeVisible();
      for (const label of ["Send Request", "Import cURL", "Change Environment", "New Request"]) await expect(empty).toContainText(label);
      if (platform === "macos") {
        await expect(empty).toContainText("⌘");
        await expect(empty).not.toContainText("Ctrl");
      } else {
        await expect(empty).toContainText("Ctrl");
        await expect(empty).not.toContainText("⌘");
      }
    });

    test(`every §6 shortcut works from the URL bar (${platform})`, async ({ page }) => {
      const errors = await openApp(page, `?platform=${platform}`);
      const url = page.getByTestId("url-input");

      // Mod+N new request, Mod+L focus URL
      await press(page, "Mod+n");
      await expect(url).toHaveValue("");
      await page.getByTestId("sidebar-filter").focus();
      await press(page, "Mod+l");
      await expect(url).toBeFocused();

      // Mod+Enter send, Mod+Enter again while in flight cancels
      await url.fill("https://api.example.com/v1/users");
      await press(page, "Mod+Enter");
      await expect(page.getByTestId("response-status")).toHaveText(/^200/);
      await url.fill("https://api.example.com/delay/5");
      await press(page, "Mod+Enter");
      await expect(page.getByTestId("send-button")).toHaveText(/Cancel/);
      await press(page, "Mod+Enter");
      await expect(page.getByTestId("response-status")).toHaveText("Cancelled");

      // Mod+U import dialog, Esc closes
      await press(page, "Mod+u");
      await expect(page.getByTestId("import-dialog")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("import-dialog")).toBeHidden();

      // Mod+E env picker, Esc closes
      await url.focus();
      await press(page, "Mod+e");
      await expect(page.getByTestId("env-picker")).toBeVisible();
      await page.keyboard.press("Escape");
      await expect(page.getByTestId("env-picker")).toBeHidden();

      // Mod+Shift+C copy as cURL
      await url.fill("https://api.example.com/v1/users?limit=1");
      await press(page, "Mod+Shift+c");
      await expect.poll(() => page.evaluate(() => (window as unknown as { __lastCopied?: string }).__lastCopied ?? "")).toContain("api.example.com/v1/users?limit=1");

      // Mod+Shift+E focuses the sidebar filter
      await url.focus();
      await press(page, "Mod+Shift+e");
      await expect(page.getByTestId("sidebar-filter")).toBeFocused();

      // Mod+1/2/3 switch sidebar tabs
      await url.focus();
      await press(page, "Mod+2");
      await expect(page.getByTestId("sidebar-tab-collections")).toHaveAttribute("aria-selected", "true");
      await press(page, "Mod+3");
      await expect(page.getByTestId("sidebar-tab-env")).toHaveAttribute("aria-selected", "true");
      await press(page, "Mod+1");
      await expect(page.getByTestId("sidebar-tab-activity")).toHaveAttribute("aria-selected", "true");
      expect(errors).toEqual([]);
    });

    test(`AC-KEY-1: shortcuts work with focus inside the CodeMirror body editor (${platform})`, async ({ page }) => {
      await openApp(page, `?platform=${platform}`);
      await press(page, "Mod+n");
      await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
      await page.getByTestId("req-tab-body").click();
      await page.getByTestId("body-type-json").click();
      const editor = page.getByTestId("body-editor").locator(".cm-content");
      await editor.click();
      await page.keyboard.type('{"a":1,"b":[1,2]}');
      // Mod+Shift+F formats (2-space JSON) without leaving the editor
      await press(page, "Mod+Shift+f");
      await expect(editor).toContainText('"a": 1');
      // Mod+Enter sends from inside the editor (must not insert a blank line)
      await editor.click();
      await press(page, "Mod+Enter");
      await expect(page.getByTestId("response-status")).toHaveText(/^\d{3}/);
      // Mod+U / Mod+S / Mod+2 from inside the editor
      await editor.click();
      await press(page, "Mod+u");
      await expect(page.getByTestId("import-dialog")).toBeVisible();
      await page.keyboard.press("Escape");
      await editor.click();
      await press(page, "Mod+2");
      await expect(page.getByTestId("sidebar-tab-collections")).toHaveAttribute("aria-selected", "true");
      // Mod+F stays local: opens CodeMirror search in the editor
      await editor.click();
      await page.keyboard.press(process.platform === "darwin" ? "Meta+f" : "Control+f"); // CodeMirror derives Mod from the HOST OS, not ?platform=
      await expect(page.locator(".cm-search")).toBeVisible();
    });

    test(`AC-RES-6: Mod+F inside the response body opens CodeMirror search (${platform})`, async ({ page }) => {
      await openApp(page, `?platform=${platform}`);
      await page.getByTestId("url-input").fill("https://api.example.com/v1/users");
      await page.getByTestId("send-button").click();
      await expect(page.getByTestId("response-status")).toHaveText(/^200/);
      await page.getByTestId("response-pane").locator(".cm-content").click();
      await page.keyboard.press(process.platform === "darwin" ? "Meta+f" : "Control+f"); // CodeMirror derives Mod from the HOST OS, not ?platform=
      await expect(page.getByTestId("response-pane").locator(".cm-search")).toBeVisible();
    });

    test(`F2 renames the selected tree node; Mod+S saves (${platform})`, async ({ page }) => {
      await openApp(page, `?platform=${platform}`);
      await page.getByTestId("sidebar-tab-collections").click();
      await node(page, "Billing").click();
      await page.keyboard.press("F2");
      await expect(page.getByTestId("tree-rename-input")).toBeVisible();
      await page.getByTestId("tree-rename-input").fill("QA renamed");
      await page.keyboard.press("Enter");
      await expect(node(page, "QA renamed")).toBeVisible();
    });
  });
}

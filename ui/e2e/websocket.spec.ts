import { expect, test, type Page } from "@playwright/test";

/** WebSocket and Socket.IO requests against the mock echo server (AC-WS-1..8). */

async function openApp(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto("/");
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

async function compose(page: Page, text: string) {
  const content = page.getByTestId("ws-message-editor").locator(".cm-content");
  await content.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.insertText(text);
}

const frames = (page: Page) => page.getByTestId("ws-frame");

test("AC-WS-3: connect, send, see the echo, disconnect; one history entry per session", async ({ page }) => {
  const errors = await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await expect(page.getByTestId("method-option-WS")).toBeVisible();
  await page.getByTestId("method-option-WS").click();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "WS");
  // WebSocket tabs and the message log replace the HTTP ones.
  await expect(page.getByTestId("req-tab-message")).toHaveAttribute("aria-selected", "true");
  await expect(page.getByTestId("req-tab-body")).toHaveCount(0);
  await expect(page.getByTestId("ws-log-empty")).toBeVisible();

  await page.getByTestId("url-input").fill("ws://localhost:8080/echo");
  await expect(page.getByTestId("ws-send")).toBeDisabled();
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await expect(page.getByTestId("ws-connect")).toHaveText("Disconnect");
  await expect(frames(page).first()).toHaveAttribute("data-kind", "open");
  await expect(page.getByTestId("ws-log-info")).toContainText("ws://localhost:8080/echo");

  await compose(page, "hello from e2e");
  await page.getByTestId("ws-send").click();
  await expect(page.locator('[data-testid="ws-frame"][data-dir="out"]')).toHaveCount(1);
  await expect(page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="text"]').last()).toContainText("hello from e2e");
  await expect(page.getByTestId("ws-counts")).toHaveText("↑1 ↓2");

  // ⌘Enter sends the composed message while connected.
  await compose(page, '{"n": 1}');
  await page.getByTestId("ws-kind-json").click();
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(page.getByTestId("ws-counts")).toHaveText("↑2 ↓3");

  // Expand a JSON message: pretty-printed; copy puts the raw payload on the clipboard.
  const echoed = page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="text"]').last();
  await echoed.locator("button.line").click();
  await expect(echoed.getByTestId("ws-frame-detail")).toHaveText('{\n  "n": 1\n}');
  await echoed.getByTestId("ws-frame-copy").click();
  expect(await page.evaluate(() => (window as unknown as { __lastCopied?: string }).__lastCopied)).toBe('{"n": 1}');

  // Filter: direction and text.
  await page.getByTestId("ws-dir-out").click();
  await expect(frames(page)).toHaveCount(2);
  await page.getByTestId("ws-dir-all").click();
  await expect(page.getByTestId("ws-filter")).toHaveAttribute("title", /first 8 KB of each message/);
  await expect(page.getByTestId("ws-filter")).toHaveAttribute("placeholder", /first 8 KB/);
  await page.getByTestId("ws-filter").fill("hello from");
  await expect(frames(page)).toHaveCount(2);
  await page.getByTestId("ws-filter").fill("no such text");
  await expect(page.getByTestId("ws-log-none")).toHaveText("No messages match the filter");
  await page.getByTestId("ws-filter").fill("");

  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");
  await expect(page.getByTestId("ws-connect")).toHaveText("Connect");
  await expect(frames(page).last()).toContainText("Disconnected · 1000");

  // Clear empties the log.
  await page.getByTestId("ws-clear").click();
  await expect(page.getByTestId("ws-log-none")).toHaveText("No messages yet");

  // History: one WS entry with the counts.
  await page.getByTestId("sidebar-tab-activity").click();
  const row = page.locator('[data-testid="history-row"][data-method="WS"]').first();
  await expect(row).toBeVisible();
  await expect(row.getByTestId("history-ws")).toHaveText("↑2 ↓3");
  await row.click();
  await expect(page.getByTestId("ws-history-summary")).toContainText("2 sent · 3 received");
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "WS");
  expect(errors).toEqual([]);
});

test("AC-WS-8: Socket.IO emits events with acks; settings show namespace and path", async ({ page }) => {
  const errors = await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-SIO").click();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "SIO");
  await page.getByTestId("url-input").fill("http://localhost:3000");

  await page.getByTestId("req-tab-settings").click();
  await page.getByTestId("sio-namespace").fill("chat");
  await page.getByTestId("sio-namespace").blur();
  await expect(page.getByTestId("sio-namespace")).toHaveValue("/chat");
  await expect(page.getByTestId("ws-url-preview")).toHaveText("ws://localhost:3000/socket.io/?EIO=4&transport=websocket");

  await page.getByTestId("req-tab-message").click();
  await page.getByTestId("ws-event").fill("say");
  await page.getByTestId("ws-ack").check();
  await compose(page, '{"text":"hi"}');
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await expect(frames(page).first()).toContainText("namespace /chat");
  await page.getByTestId("ws-send").click();
  await expect(page.locator('[data-testid="ws-frame"][data-kind="ack"]')).toContainText("Ack #0");
  const ev = page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="event"]');
  await expect(ev).toContainText("say");
  await expect(ev).toContainText('{"text":"hi"}');

  // Reserved names are refused before sending.
  await page.getByTestId("ws-event").fill("");
  await expect(page.getByTestId("ws-message-error")).toHaveText(/Enter an event name/);
  await expect(page.getByTestId("ws-send")).toBeDisabled();
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");
  expect(errors).toEqual([]);
});

test("AC-WS-4: saved messages are kept with the request and survive a save", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-WS").click();
  await page.getByTestId("url-input").fill("ws://localhost:8080/echo");
  await compose(page, "saved hello");
  await page.getByTestId("ws-save-message").click();
  await page.getByTestId("ws-save-name").fill("Greeting");
  await page.getByTestId("ws-save-name").press("Enter");
  await expect(page.getByTestId("badge-saved")).toHaveText("1");

  await page.getByTestId("save-request").click();
  await page.getByTestId("picker-name").fill("Echo socket");
  await page.locator('[data-testid="picker-node"][data-name="Users"]').click();
  await page.getByTestId("picker-submit").click();
  await page.getByTestId("sidebar-tab-collections").click();
  const node = page.locator('[data-testid="tree-node"][data-name="Echo socket"]');
  await expect(node).toBeVisible();
  await expect(node.getByTestId("tree-ws")).toHaveText("WS");

  await page.getByTestId("req-tab-saved").click();
  await expect(page.getByTestId("ws-saved-row")).toHaveCount(1);
  await expect(page.getByTestId("ws-saved-send")).toBeDisabled();
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await page.getByTestId("ws-saved-send").click();
  await expect(page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="text"]').last()).toContainText("saved hello");
  // Load copies it into the composer.
  await page.getByTestId("ws-saved-load").click();
  await expect(page.getByTestId("req-tab-message")).toHaveAttribute("aria-selected", "true");
  await expect(page.getByTestId("ws-message-editor").locator(".cm-content")).toHaveText("saved hello");
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");

  // Back to HTTP keeps the WebSocket settings for later.
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-GET").click();
  await expect(page.getByTestId("req-tab-body")).toBeVisible();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-WS").click();
  await page.getByTestId("req-tab-saved").click();
  await expect(page.getByTestId("ws-saved-row")).toHaveCount(1);
});

test("AC-WS-2: a refused connection shows the error and Retry", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-WS").click();
  await page.getByTestId("url-input").fill("ws://refused.test/socket");
  await page.getByTestId("url-input").press("Enter");
  await expect(page.getByTestId("ws-error")).toContainText("Connection refused");
  await expect(page.getByTestId("ws-state")).toHaveText("Failed");
  await page.getByTestId("url-input").fill("ftp://nope");
  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("send-blocked")).toContainText("Invalid WebSocket URL");
});

test("AC-WS-3: the tree marks a live connection; deleting the request closes it", async ({ page }) => {
  const errors = await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-WS").click();
  await page.getByTestId("url-input").fill("ws://localhost:8080/live");
  await page.getByTestId("save-request").click();
  await page.getByTestId("picker-name").fill("Live socket");
  await page.locator('[data-testid="picker-node"][data-name="Users"]').click();
  await page.getByTestId("picker-submit").click();
  await page.getByTestId("sidebar-tab-collections").click();
  const node = page.locator('[data-testid="tree-node"][data-name="Live socket"]');
  await expect(node).toBeVisible();
  await expect(node.getByTestId("tree-live")).toHaveCount(0);

  await page.getByTestId("ws-connect").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await expect(node.getByTestId("tree-live")).toHaveAttribute("data-state", "open");

  await node.click({ button: "right" });
  await page.getByTestId("tree-delete").click();
  await page.getByTestId("confirm-ok").click();
  await expect(node).toHaveCount(0);
  // The session ended (one history entry), instead of running on without its request.
  await page.getByTestId("sidebar-tab-activity").click();
  const row = page.locator('[data-testid="history-row"][data-method="WS"]').first();
  await expect(row).toBeVisible();
  await expect(row).toContainText("localhost:8080");
  expect(errors).toEqual([]);
});

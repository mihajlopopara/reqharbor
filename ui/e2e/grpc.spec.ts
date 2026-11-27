import { expect, test, type Page } from "@playwright/test";

/** gRPC requests against the mock demo.Greeter service (AC-GRPC-1..10). */

async function openApp(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto("/");
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

/** A new request switched to gRPC, pointed at the mock server, with its services fetched. */
async function newGrpcRequest(page: Page, url = "grpc://localhost:50051") {
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-GRPC").click();
  await expect(page.getByTestId("method-select")).toHaveAttribute("data-method", "GRPC");
  await expect(page.getByTestId("req-tab-message")).toHaveAttribute("aria-selected", "true");
  await expect(page.getByTestId("req-tab-proto")).toBeVisible();
  await expect(page.getByTestId("req-tab-body")).toHaveCount(0);
  await page.getByTestId("url-input").fill(url);
  await page.getByTestId("grpc-method-picker").click();
  await expect(page.getByTestId("grpc-method-list")).toBeVisible();
  await page.getByTestId("grpc-picker-fetch").click();
}

async function pick(page: Page, method: string) {
  await page.getByTestId("grpc-method-picker").click();
  await page.getByTestId("grpc-method-search").fill(method.split("/")[1]);
  await page.locator(`[data-testid="grpc-method-option"][data-method="${method}"]`).click();
  await expect(page.getByTestId("grpc-method-name")).toHaveText(method.split("/")[1]);
}

async function compose(page: Page, text: string) {
  const content = page.getByTestId("grpc-message-editor").locator(".cm-content");
  await content.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.insertText(text);
}

const frames = (page: Page) => page.getByTestId("ws-frame");

test("AC-GRPC-5/7: pick a method, invoke, see the response, status, headers and history", async ({ page }) => {
  const errors = await openApp(page);
  await newGrpcRequest(page);
  await pick(page, "demo.Greeter/SayHello");
  await expect(page.getByTestId("grpc-call-kind")).toHaveText("unary");
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Invoke");
  await compose(page, '{"name":"Playwright"}');
  await page.getByTestId("grpc-invoke").click();
  await expect(page.getByTestId("grpc-status")).toHaveAttribute("data-code", "0");
  await expect(page.getByTestId("grpc-status")).toHaveText("0 OK");
  await expect(page.getByTestId("grpc-response-pane")).toContainText("Hello, Playwright!");
  await page.getByTestId("grpc-res-tab-headers").click();
  await expect(page.getByTestId("grpc-headers")).toContainText("content-type");
  // One history entry, shown with the gRPC badge.
  await page.getByTestId("sidebar-tab-activity").click();
  await expect(page.getByTestId("history-row").first()).toHaveAttribute("data-method", "GRPC");
  await expect(page.getByTestId("history-row").first()).toContainText("gRPC");
  // Copy as grpcurl replaces the cURL items.
  await page.getByTestId("request-menu").click();
  await expect(page.getByTestId("menu-copy-grpcurl")).toBeVisible();
  await expect(page.getByTestId("menu-copy-curl")).toBeDisabled();
  await page.keyboard.press("Escape");
  expect(errors).toEqual([]);
});

test("AC-GRPC-7: an error status shows its code and details", async ({ page }) => {
  await openApp(page);
  await newGrpcRequest(page);
  await pick(page, "demo.Failing/Fail");
  await page.getByTestId("grpc-invoke").click();
  await expect(page.getByTestId("grpc-status")).toHaveText("5 NOT_FOUND");
  await expect(page.getByTestId("grpc-status-panel")).toBeVisible();
  await expect(page.getByTestId("grpc-status-detail").first()).toContainText("BadRequest");
});

test("AC-GRPC-8: a server stream fills the message log", async ({ page }) => {
  await openApp(page);
  await newGrpcRequest(page);
  await pick(page, "demo.Greeter/LotsOfReplies");
  await expect(page.getByTestId("grpc-call-kind")).toHaveText("server stream");
  await compose(page, '{"name":"Bo"}');
  await page.getByTestId("grpc-invoke").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");
  await expect(page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="message"]')).toHaveCount(3);
  await expect(page.getByTestId("grpc-stream-status")).toHaveText("0 OK");
  await expect(page.getByTestId("ws-counts")).toHaveText("↑1 ↓3");
  await expect(frames(page).first()).toHaveAttribute("data-kind", "open");
  await expect(page.getByTestId("ws-log-info")).toContainText("gRPC");
});

test("AC-GRPC-8: a bidi stream connects, sends, receives the echo and ends", async ({ page }) => {
  await openApp(page);
  await newGrpcRequest(page);
  await pick(page, "demo.Greeter/Chat");
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Connect");
  await page.getByTestId("grpc-invoke").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await expect(page.getByTestId("grpc-invoke")).toHaveText(/Cancel/);
  await compose(page, '{"user":"me","text":"hello stream"}');
  await page.getByTestId("grpc-send").click();
  await expect(page.locator('[data-testid="ws-frame"][data-dir="out"][data-kind="message"]')).toHaveCount(1);
  await expect(page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="message"]').last()).toContainText("hello stream");
  // ⌘Enter sends the composed message while the stream is open.
  await compose(page, '{"user":"me","text":"second"}');
  await page.keyboard.press("ControlOrMeta+Enter");
  await expect(page.locator('[data-testid="ws-frame"][data-dir="in"][data-kind="message"]')).toHaveCount(2);
  await page.getByTestId("grpc-end").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");
  await expect(page.getByTestId("grpc-stream-status")).toHaveText("0 OK");
  await expect(page.getByTestId("ws-counts")).toHaveText("↑2 ↓2");
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Connect");
});

test("AC-GRPC-7/8: a saved bidi request with no cached services doesn't hang on Invoke, the button becomes Connect", async ({ page }) => {
  const errors = await openApp(page);
  await newGrpcRequest(page);
  await pick(page, "demo.Greeter/Chat");
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Connect");
  // Forget the services: the kind is unknown again, so the button says Invoke.
  await page.getByTestId("req-tab-proto").click();
  await page.getByTestId("grpc-forget").click();
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Invoke");
  await page.getByTestId("grpc-invoke").click();
  // Rejected before anything is sent (no spinner, no response), and the kind is learned.
  await expect(page.getByTestId("grpc-invoke")).toHaveText("Connect", { timeout: 3000 });
  await expect(page.getByTestId("grpc-call-kind")).toHaveText("bidi");
  await expect(page.getByTestId("ws-state")).toHaveCount(0);
  // Connect now opens the stream.
  await page.getByTestId("grpc-invoke").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "open");
  await page.getByTestId("req-tab-message").click();
  await page.getByTestId("grpc-end").click();
  await expect(page.getByTestId("ws-state")).toHaveAttribute("data-state", "closed");
  expect(errors).toEqual([]);
});

test("AC-GRPC-3: a server without reflection offers .proto files", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-GRPC").click();
  await page.getByTestId("url-input").fill("grpc://noreflection.test:50051");
  await page.getByTestId("req-tab-proto").click();
  await page.getByTestId("grpc-fetch").click();
  await expect(page.getByTestId("grpc-proto-error")).toHaveAttribute("data-kind", "unsupported");
  await page.getByTestId("grpc-use-files").click();
  await page.getByTestId("grpc-add-file-input").fill("/protos/greeter.proto");
  await page.getByTestId("grpc-add-file").click();
  await expect(page.getByTestId("grpc-proto-file")).toHaveCount(1);
  await expect(page.getByTestId("grpc-services-status")).toContainText("2 services");
});

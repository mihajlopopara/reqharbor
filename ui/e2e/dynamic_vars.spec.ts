import { expect, test, type Page } from "@playwright/test";

/** Dynamic variables {{$name}} (AC-ENV-8 .. AC-ENV-10), against the mock backend. */

async function openApp(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/");
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

const UUID = /[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}/;

async function jsonBody(page: Page, text: string) {
  await page.getByTestId("req-tab-body").click();
  await page.getByTestId("body-type-json").click();
  const content = page.getByTestId("body-editor").locator(".cm-content");
  await content.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.insertText(text);
}

test("AC-ENV-8: {{$randomEmail}} sends a generated email; history shows the URL that was sent", async ({ page }) => {
  const errors = await openApp(page);
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-POST").click();
  await page.getByTestId("url-input").fill("https://api.example.com/v1/users?rid={{$uuid}}");
  // highlighted as a dynamic variable, with what it generates on hover
  const urlBar = page.getByTestId("url-input").locator("..");
  await expect(urlBar.locator(".overlay .var-dyn")).toHaveText("{{$uuid}}");
  await jsonBody(page, '{"email":"{{$randomEmail}}","rid":"{{$uuid}}"}');
  await expect(page.getByTestId("body-editor").locator(".cm-var-dyn")).toHaveCount(2);

  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/201/);
  const body = JSON.parse((await page.getByTestId("response-body").locator(".cm-content").innerText()).trim());
  expect(body.email).toMatch(/^[a-z]+\.[a-z]+@example\.(com|org|net)$/);
  expect(body.rid).toMatch(UUID);
  await expect(page.getByTestId("unresolved-warning")).toHaveCount(0);

  await page.getByTestId("sidebar-tab-activity").click();
  const row = page.getByTestId("history-row").first();
  // one value per send: the URL in history carries the same {{$uuid}} as the body
  await expect(row).toHaveAttribute("title", `https://api.example.com/v1/users?rid=${body.rid}`);
  expect(errors).toEqual([]);
});

test("AC-ENV-9: typing {{ offers env variables and the $ catalogue; hover explains", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  const url = page.getByTestId("url-input");
  await url.click();
  await url.pressSequentially("https://x.dev/{{$randomEm");
  const list = page.getByTestId("var-autocomplete");
  await expect(list).toBeVisible();
  await expect(list.getByRole("option").first()).toContainText("$randomEmail");
  await expect(list.getByRole("option").first()).toContainText("Random email");
  await url.press("Enter");
  await expect(list).toHaveCount(0);
  await expect(url).toHaveValue("https://x.dev/{{$randomEmail}}");

  // env vars come first after a bare {{
  await url.press("End");
  await url.pressSequentially("?h={{");
  await expect(list.getByRole("option").first()).toContainText("baseUrl");
  await url.press("Escape");
  await expect(list).toHaveCount(0);

  const seg = url.locator("..").locator(".overlay .var-dyn");
  const box = (await seg.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await expect(page.getByTestId("dynamic-tip")).toHaveText("Random email · e.g. jane.doe@example.com · new value each send");
});

test("AC-ENV-9: {{ autocomplete in the body editor", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  await jsonBody(page, '{"id":"');
  await page.keyboard.type("{{$uu");
  const option = page.locator(".cm-tooltip-autocomplete li").first();
  await expect(option).toContainText("$uuid");
  await expect(option).toHaveAttribute("aria-selected", "true");
  // CodeMirror ignores Enter for 75 ms after the list opens (interactionDelay)
  await page.waitForTimeout(150);
  await page.keyboard.press("Enter");
  await expect(page.getByTestId("body-editor").locator(".cm-content")).toHaveText('{"id":"{{$uuid}}');
});

test("AC-ENV-10: Insert fake data puts {{$name}} at the caret", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("new-request").click();
  await jsonBody(page, '{"name":""}');
  // caret between the quotes
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowLeft");
  await page.getByTestId("insert-fake").click();
  await page.getByTestId("fake-randomFullName").click();
  await expect(page.getByTestId("body-editor").locator(".cm-content")).toHaveText('{"name":"{{$randomFullName}}"}');
});

test("AC-ENV-8: environment variable names can't start with $", async ({ page }) => {
  await openApp(page);
  await page.getByTestId("sidebar-tab-env").click();
  await page.locator('[data-testid="env-row"][data-name="Staging"]').click();
  await page.getByTestId("env-vars-ghost").getByTestId("env-vars-key").fill("$uuid");
  await expect(page.getByTestId("env-reserved-name")).toContainText("can't start with $");
});

import { expect, test, type Page } from "@playwright/test";
import { node } from "./qa_helpers";

/** GraphQL beyond sending a body (PRODUCT §5.14, AC-GQL-*) against the mock's fake endpoint. */

async function openApp(page: Page) {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
  await page.goto("/");
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

/** A new POST request with a GraphQL body to `url`. */
async function graphqlRequest(page: Page, url = "https://api.example.com/graphql") {
  await page.getByTestId("new-request").click();
  await page.getByTestId("method-select").click();
  await page.getByTestId("method-option-POST").click();
  await page.getByTestId("url-input").fill(url);
  await page.getByTestId("req-tab-body").click();
  await page.getByTestId("body-type-graphql").click();
  await expect(page.getByTestId("graphql-schema-bar")).toBeVisible();
}

const query = (page: Page) => page.getByTestId("graphql-query").locator(".cm-content");
const variables = (page: Page) => page.getByTestId("graphql-variables").locator(".cm-content");

async function setEditor(page: Page, which: "query" | "variables", text: string) {
  const content = which === "query" ? query(page) : variables(page);
  await content.click();
  await page.keyboard.press("ControlOrMeta+a");
  await page.keyboard.press("Delete");
  if (text) await page.keyboard.insertText(text);
}

/** The editor's text, lines joined with "\n". */
const textOf = (page: Page, which: "query" | "variables") => (which === "query" ? query(page) : variables(page)).innerText();

async function fetchSchema(page: Page) {
  await page.getByTestId("graphql-fetch-schema").click();
  await expect(page.getByTestId("graphql-schema-status")).toContainText(/\d+ types · fetched just now/);
}

test("AC-GQL-2/6/11: fetch schema, autocomplete a field, send, errors badge", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page);
  await expect(page.getByTestId("graphql-schema-bar")).toContainText("No schema");
  await fetchSchema(page);

  // Autocomplete from the schema (typing opens it; Enter accepts the best match).
  await setEditor(page, "query", "");
  await page.keyboard.type("{ me { na");
  const popup = page.locator(".cm-tooltip-autocomplete");
  await expect(popup).toBeVisible();
  await expect(popup.locator("li").first()).toContainText("name");
  await expect(popup).toContainText("String!");
  await page.waitForTimeout(150); // CodeMirror ignores Enter right after the popup opens (interactionDelay)
  await page.keyboard.press("Enter");
  await page.keyboard.type(" } }");
  await expect(query(page)).toHaveText("{ me { name } }");

  // Validation against the schema: an unknown field is underlined with the server-side message.
  await setEditor(page, "query", "{ me { nope } }");
  await expect(page.getByTestId("graphql-query").locator(".cm-lintRange-error")).toHaveCount(1);
  await page.getByTestId("graphql-query").locator(".cm-lintRange-error").hover();
  await expect(page.locator(".cm-tooltip-lint")).toContainText('Cannot query field "nope" on type "User"');

  // A clean answer: no badge.
  await setEditor(page, "query", "{ me { name email } }");
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/200/);
  await expect(page.getByTestId("response-body")).toContainText("ada@example.com");
  await expect(page.getByTestId("graphql-errors-badge")).toHaveCount(0);

  // `errors` in a 200 answer: a visible badge, listing the messages.
  await setEditor(page, "query", "{ me { name } failing }");
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/200/);
  const badge = page.getByTestId("graphql-errors-badge");
  await expect(badge).toHaveText("1 GraphQL error");
  await badge.click();
  await expect(page.getByTestId("graphql-errors-list")).toContainText("Something went wrong in the resolver");
  await expect(page.getByTestId("graphql-errors-list")).toContainText("failing");
  // The response viewer is unchanged: the body is still there.
  await expect(page.getByTestId("response-body")).toContainText('"failing": null');
  expect(errors).toEqual([]);
});

test("AC-GQL-10: explorer browses types, searches and inserts a field at the caret", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page);
  await fetchSchema(page);
  await page.getByTestId("graphql-docs-toggle").click();
  const docs = page.getByTestId("schema-explorer");
  await expect(docs).toBeVisible();
  await expect(docs.getByTestId("docs-root-query")).toContainText("Query");
  await expect(docs.getByTestId("docs-root-mutation")).toContainText("Mutation");

  await docs.getByTestId("docs-root-query").click();
  await expect(docs.getByTestId("docs-title")).toHaveText("Query");
  await expect(docs.getByTestId("docs-field-user")).toContainText("user(id: ID!): User");
  await expect(docs.getByTestId("docs-field-user")).toContainText("One user by id");

  // Insert at the caret: required args and braces, the caret lands after `id: `.
  await setEditor(page, "query", "{  }");
  await query(page).click();
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("ArrowLeft");
  await docs.getByTestId("docs-field-user").hover();
  await docs.getByTestId("docs-insert-user").click();
  await expect.poll(() => textOf(page, "query")).toBe("{ user(id: ) {  } }");
  await page.keyboard.type('"u1"');
  await expect.poll(() => textOf(page, "query")).toBe('{ user(id: "u1") {  } }');

  // Type links and back.
  await docs.getByTestId("docs-type-link-User").first().click();
  await expect(docs.getByTestId("docs-title")).toHaveText("User");
  await expect(docs.getByTestId("docs-description")).toHaveText("A person with an account.");
  await expect(docs.getByTestId("docs-field-username")).toContainText("Deprecated: Use email");
  await docs.getByTestId("docs-back").click();
  await expect(docs.getByTestId("docs-title")).toHaveText("Query");

  // Search: fields and types, case-insensitive.
  await docs.getByTestId("docs-search").fill("EMAIL");
  await expect(docs.getByTestId("docs-hit").first()).toContainText("CreateUserInput.email");
  await docs.getByTestId("docs-hit").filter({ hasText: "User.email" }).first().click();
  await expect(docs.getByTestId("docs-title")).toHaveText(/User/);
  await expect(docs.locator(".field-row.hl")).toHaveCount(1);

  await docs.getByTestId("docs-close").click();
  await expect(docs).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("AC-GQL-9/8/12: operation picker sends operationName; variables follow the operation", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page);
  await fetchSchema(page);
  await expect(page.getByTestId("graphql-operation")).toHaveCount(0);
  await setEditor(page, "query", "query Me { me { name } }\nquery One($id: ID!) { user(id: $id) { email } }");
  const picker = page.getByTestId("graphql-operation");
  await expect(picker).toBeVisible();
  await expect(picker).toHaveValue("Me");
  await picker.selectOption("One");

  // Variables: the missing required one is an error; completion offers it.
  await setEditor(page, "variables", "{ }");
  await expect(page.getByTestId("graphql-variables").locator(".cm-lintRange-error")).toHaveCount(1);
  await variables(page).click();
  await page.keyboard.press("End");
  await page.keyboard.press("ArrowLeft");
  await page.keyboard.press("Control+Space");
  const popup = page.locator(".cm-tooltip-autocomplete");
  await expect(popup).toContainText("id");
  await expect(popup).toContainText("ID!");
  await page.waitForTimeout(150); // CodeMirror ignores Enter right after the popup opens (interactionDelay)
  await page.keyboard.press("Enter");
  await page.keyboard.type('"u2"');
  await expect.poll(() => textOf(page, "variables")).toBe('{ "id": "u2"}');
  await expect(page.getByTestId("graphql-variables").locator(".cm-lintRange-error")).toHaveCount(0);
  // Wrong type: flagged at the value.
  await setEditor(page, "variables", '{"id": true, "extra": 1}');
  await expect(page.getByTestId("graphql-variables").locator(".cm-lintRange-error")).toHaveCount(1);
  await expect(page.getByTestId("graphql-variables").locator(".cm-lintRange-warning")).toHaveCount(1);
  await setEditor(page, "variables", '{"id": "u2"}');

  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/200/);
  await expect(page.getByTestId("response-body")).toContainText("grace@example.com");

  // cURL export carries operationName.
  await page.getByTestId("request-menu").click();
  await page.getByTestId("menu-copy-curl").click();
  await expect.poll(() => page.evaluate(() => (window as unknown as { __lastCopied?: string }).__lastCopied ?? "")).toContain('"operationName":"One"');

  // Renaming the chosen operation falls back to the first one.
  await setEditor(page, "query", "query Me { me { name } }\nquery Two { users { id } }");
  await expect(picker).toHaveValue("Me");
  // One operation: no picker, nothing sent.
  await setEditor(page, "query", "{ me { name } }");
  await expect(picker).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("AC-GQL-3/5/7: introspection disabled, schema from a file, Format", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page, "https://nointrospection.example.com/graphql");
  await page.getByTestId("graphql-fetch-schema").click();
  const err = page.getByTestId("graphql-schema-error");
  await expect(err).toHaveAttribute("data-kind", "disabled");
  await expect(err).toContainText("Introspection is disabled on this server.");
  await expect(err).toContainText("not allowed by Apollo Server");

  // Load the SDL from a file instead.
  const chooser = page.waitForEvent("filechooser");
  await err.getByRole("button", { name: "Load from file…" }).click();
  await (await chooser).setFiles({ name: "schema.graphql", mimeType: "text/plain", buffer: Buffer.from('type Query { "Hi" hello(name: String): String }') });
  await expect(page.getByTestId("graphql-schema-status")).toContainText(/\d+ types · from schema\.graphql · loaded just now/);
  await expect(err).toHaveCount(0);
  await setEditor(page, "query", "{ he");
  await expect(page.locator(".cm-tooltip-autocomplete")).toContainText("hello");
  await page.keyboard.press("Escape");

  // A file that isn't a schema is refused.
  await page.getByTestId("graphql-schema-menu").click();
  const chooser2 = page.waitForEvent("filechooser");
  await page.getByTestId("graphql-menu-file").click();
  await (await chooser2).setFiles({ name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("just some notes") });
  await expect(err).toHaveAttribute("data-kind", "invalid");
  await expect(err).toContainText("This file isn't a GraphQL schema");

  // Format prettifies the query; a syntax error points at it and changes nothing.
  await setEditor(page, "query", "{hello(name:\"x\")}");
  await page.getByTestId("graphql-format-query").click();
  await expect.poll(() => textOf(page, "query")).toBe('{\n  hello(name: "x")\n}');
  await setEditor(page, "query", "{ hello(");
  await page.getByTestId("graphql-format-query").click();
  await expect(page.getByTestId("graphql-error")).toContainText(/Invalid GraphQL at line 1, col \d+/);
  await expect(query(page)).toHaveText("{ hello(");
  expect(errors).toEqual([]);
});

/** Expand tree containers until `name` shows. */
async function reveal(page: Page, name: string, parents: string[]) {
  for (const p of parents) {
    if (await node(page, name).isVisible()) return;
    if (!(await node(page, p).getAttribute("aria-expanded")?.then((x) => x === "true"))) await node(page, p).click();
  }
  await expect(node(page, name)).toBeVisible();
}

/** Rewrite seeded requests in the mock as if a sync pull brought them in. */
async function pullRequests(page: Page, patches: Record<string, Record<string, unknown>>) {
  await page.evaluate((p) => {
    type M = { requests: Array<Record<string, unknown>>; emit: (e: string, payload: unknown) => void };
    const m = (window as unknown as { __mock: M }).__mock;
    for (const [name, patch] of Object.entries(p)) Object.assign(m.requests.find((x) => x.name === name)!, patch, { updatedAt: Date.now() });
    m.emit("sync://changed", { pushed: 0, pulled: 1, deleted: 0, conflicts: [], skipped: [], tooLarge: [], unsupported: [], restored: false, cursor: 1 });
  }, patches);
}

test("AC-GQL-9: a stored request without (or with a stale) operationName sends what the picker shows", async ({ page }) => {
  const errors = await openApp(page);
  await pullRequests(page, {
    Health: { method: "POST", url: "https://api.example.com/graphql", body: { type: "graphql", graphql: { query: "query Me { me { name } }\nquery Everyone { users { email } }", variables: "" } } },
    "List users": { method: "POST", url: "https://api.example.com/graphql", auth: { type: "none" }, body: { type: "graphql", graphql: { query: "query Me { me { name } }", variables: "", operationName: "Gone" } } },
  });
  await page.getByTestId("sidebar-tab-collections").click();
  await reveal(page, "Health", ["Users API"]);
  await node(page, "Health").click();
  await page.getByTestId("req-tab-body").click();
  await expect(page.getByTestId("graphql-operation")).toHaveValue("Me");
  await expect(page.getByTestId("dirty-dot")).toHaveCount(0);
  await page.getByTestId("send-button").click();
  // Without operationName the endpoint would answer 400 "Must provide operation name…".
  await expect(page.getByTestId("response-status")).toHaveText(/200/);
  await expect(page.getByTestId("response-body")).toContainText("Ada Lovelace");
  await expect(page.getByTestId("dirty-dot")).toHaveCount(0);

  // A stale name next to a single operation isn't sent (graphql-js would refuse "Gone").
  await reveal(page, "List users", ["Users API", "Users"]);
  await node(page, "List users").click();
  await page.getByTestId("req-tab-body").click();
  await expect(page.getByTestId("graphql-operation")).toHaveCount(0);
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/200/);
  await expect(page.getByTestId("response-body")).toContainText("Ada Lovelace");
  expect(errors).toEqual([]);
});

test("AC-GQL-3/5: a bad schema never replaces the good one (file, then fetch)", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page);
  await fetchSchema(page);
  const status = page.getByTestId("graphql-schema-status");
  const good = (await status.textContent()) ?? "";
  expect(good).toMatch(/\d+ types · fetched/);
  const err = page.getByTestId("graphql-schema-error");

  // SDL that doesn't build.
  await page.getByTestId("graphql-schema-menu").click();
  const chooser = page.waitForEvent("filechooser");
  await page.getByTestId("graphql-menu-file").click();
  await (await chooser).setFiles({ name: "broken.graphql", mimeType: "text/plain", buffer: Buffer.from("type Query { a: Missing }") });
  await expect(err).toContainText("This file isn't a GraphQL schema");
  await expect(status).toHaveText(good);

  // An introspection answer graphql-js can't build, same URL.
  await page.getByTestId("req-tab-headers").click();
  await page.getByTestId("headers-ghost").getByTestId("headers-key").fill("X-Mock-Schema");
  await page.getByTestId("headers-row").last().getByTestId("headers-value").fill("broken");
  await page.getByTestId("req-tab-body").click();
  await page.getByTestId("graphql-refresh-schema").click();
  await expect(err).toContainText("The server's schema can't be used");
  await expect(status).toHaveText(good);
  // Still completing from the good schema, and still cached: reopening the body keeps it.
  await page.getByTestId("req-tab-query").click();
  await page.getByTestId("req-tab-body").click();
  await expect(status).toContainText(/\d+ types · fetched/);
  await setEditor(page, "query", "{ me { na");
  await expect(page.locator(".cm-tooltip-autocomplete")).toContainText("name");
  expect(errors).toEqual([]);
});

test("AC-GQL-11/7: errors badge on a 400 answer; Format says when comments were removed", async ({ page }) => {
  const errors = await openApp(page);
  await graphqlRequest(page);
  await setEditor(page, "query", "{ me { nope } }");
  await page.getByTestId("send-button").click();
  await expect(page.getByTestId("response-status")).toHaveText(/400/);
  await expect(page.getByTestId("graphql-errors-badge")).toHaveText("1 GraphQL error");

  await setEditor(page, "query", "# who am I\n{me{name}}");
  await page.getByTestId("graphql-format-query").click();
  await expect(page.getByTestId("graphql-format-notice")).toHaveText(/Comments were removed\. .*Z to undo\./);
  await expect.poll(() => textOf(page, "query")).toBe("{\n  me {\n    name\n  }\n}");
  await query(page).click();
  await page.keyboard.type(" ");
  await expect(page.getByTestId("graphql-format-notice")).toHaveCount(0);
  expect(errors).toEqual([]);
});

/**
 * Settings › Account against the mock backend (04 §12 M1c, 06-go-server §9):
 * server entry states, the plain-http warning, method buttons, the browser
 * sign-in states (including no_port), Devices and Sign out.
 */
import { expect, test, type Page } from "@playwright/test";

async function openAccount(page: Page, qs = "") {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/" + qs);
  await expect(page.getByTestId("app")).toBeVisible();
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-account").click();
  return errors;
}

async function enterServer(page: Page, url: string) {
  const input = page.getByTestId("signin-server");
  await input.fill(url);
  await input.press("Enter");
}

async function finishBrowser(page: Page) {
  // The fake browser only finishes a sign-in that is waiting for it.
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await finishBrowserNow(page);
}

const finishBrowserNow = (page: Page) => page.evaluate(() => (window as unknown as { __mock: { accounts: { finishBrowser(): Promise<void> } } }).__mock.accounts.finishBrowser());

test("Account is the first Settings section and starts signed out with no default server", async ({ page }) => {
  const errors = await openAccount(page);
  const nav = page.locator("nav[aria-label='Settings sections'] button");
  await expect(nav.first()).toHaveText("Account");
  await expect(page.getByTestId("signin-server")).toHaveValue("");
  await expect(page.getByTestId("signin-server")).toHaveAttribute("placeholder", "https://reqharbor.example.com");
  await expect(page.getByTestId("account-signin")).toBeDisabled();
  // No avatar and no sync chip while signed out.
  await expect(page.getByTestId("account-avatar")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("the accounts flag off hides the section", async ({ page }) => {
  await page.goto("/?mockAccounts=off");
  await page.getByTestId("settings-button").click();
  await expect(page.getByTestId("settings-nav-account")).toHaveCount(0);
  await expect(page.getByTestId("settings-nav-general")).toBeVisible();
});

test("server entry states from discovery", async ({ page }) => {
  await openAccount(page);
  await enterServer(page, "https://reqharbor.acme.dev/api");
  await expect(page.getByTestId("signin-url-error")).toHaveText("Enter the server's address without a path");

  await enterServer(page, "https://unreachable.example");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("Can't reach unreachable.example. Check the address and your connection.");

  await enterServer(page, "https://tokenonly.example");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("This server only supports token sync. Use Sync › Advanced.");
  await expect(page.getByTestId("account-signin")).toBeDisabled();
  await page.getByTestId("signin-open-advanced").click();
  await expect(page.getByTestId("sync-worker-url")).toBeVisible();
  await page.getByTestId("settings-nav-account").click();

  await enterServer(page, "https://legacyonly.example");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("token sync");

  await enterServer(page, "https://notreqharbor.example");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("This doesn't look like a ReqHarbor server.");

  await enterServer(page, "https://tooold.example");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("This server needs ReqHarbor 9.0.0 or newer.");

  await enterServer(page, "https://setup.example");
  await expect(page.getByTestId("signin-notice")).toContainText("setup code in the server log");
  await expect(page.getByTestId("signin-method-password")).toBeEnabled();

  await enterServer(page, "https://futureapi.example");
  await expect(page.getByTestId("signin-server-ok")).toBeVisible();
  await expect(page.getByTestId("signin-method-github")).toBeEnabled();
});

test("method buttons follow the server, unknown ids included", async ({ page }) => {
  await openAccount(page, "?mockMethods=password,github,oidc,passkey");
  await enterServer(page, "reqharbor.acme.dev");
  await expect(page.getByTestId("signin-server-ok")).toContainText("Acme ReqHarbor");
  await expect(page.getByTestId("signin-method-password")).toHaveText("Sign in with email");
  await expect(page.getByTestId("signin-method-github")).toHaveText("Continue with GitHub");
  await expect(page.getByTestId("signin-method-oidc")).toHaveText("Continue with Acme SSO");
  await expect(page.getByTestId("signin-method-passkey")).toHaveText("Continue with passkey");
});

test("no methods: sign-in stays disabled with the admin hint", async ({ page }) => {
  await openAccount(page, "?mockMethods=");
  await enterServer(page, "reqharbor.acme.dev");
  await expect(page.getByTestId("signin-discovery-error")).toContainText("This server doesn't have sign-in set up.");
  await expect(page.getByTestId("account-signin")).toBeDisabled();
});

test("canonical address prompt: use it, or keep what I typed", async ({ page }) => {
  await openAccount(page);
  await enterServer(page, "https://other-public.example");
  await expect(page.getByTestId("signin-canonical")).toContainText("This server's address is https://rh.acme.dev. Use that address?");
  await expect(page.getByTestId("signin-method-github")).toBeDisabled();
  await page.getByTestId("signin-keep-typed").click();
  await expect(page.getByTestId("signin-canonical")).toHaveCount(0);
  await expect(page.getByTestId("signin-method-github")).toBeEnabled();
  // The choice is remembered per origin.
  await enterServer(page, "https://reqharbor.acme.dev");
  await enterServer(page, "https://other-public.example");
  await expect(page.getByTestId("signin-server-ok")).toBeVisible();
  await expect(page.getByTestId("signin-canonical")).toHaveCount(0);

  await enterServer(page, "https://other-publicurl2.example");
  await page.getByTestId("signin-use-canonical").click();
  await expect(page.getByTestId("signin-server")).toHaveValue("https://rh.acme.dev");
});

test("plain-http warning needs Continue anyway, for sign-in and token sync", async ({ page }) => {
  await openAccount(page, "?mockAuthDelay=0");
  await enterServer(page, "http://192.168.1.5:8080");
  await expect(page.getByTestId("signin-insecure")).toContainText("This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.");
  await expect(page.getByTestId("signin-method-password")).toBeDisabled();
  await page.getByTestId("signin-insecure-ack").check();
  await page.getByTestId("signin-method-password").click();
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await page.getByTestId("signin-cancel").click();

  // Loopback http is never warned about.
  await enterServer(page, "http://localhost:8080");
  await expect(page.getByTestId("signin-server-ok")).toBeVisible();
  await expect(page.getByTestId("signin-insecure")).toHaveCount(0);

  await page.getByTestId("settings-nav-sync").click();
  await page.getByTestId("sync-worker-url").fill("http://10.0.0.2:8080");
  await expect(page.getByTestId("sync-insecure")).toBeVisible();
  await expect(page.getByTestId("sync-connect")).toBeDisabled();
  await page.getByTestId("sync-insecure-ack").check();
  await expect(page.getByTestId("sync-connect")).toBeEnabled();

  // The token-sync acknowledgement is kept per origin in knownServers, like sign-in's.
  await page.keyboard.press("Escape");
  await expect(page.getByTestId("sync-worker-url")).toHaveCount(0);
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await page.getByTestId("sync-worker-url").fill("http://10.0.0.2:8080");
  await expect(page.getByTestId("sync-insecure")).toBeVisible();
  await expect(page.getByTestId("sync-insecure-ack")).toBeChecked();
  await expect(page.getByTestId("sync-connect")).toBeEnabled();
  // Another origin is asked again.
  await page.getByTestId("sync-worker-url").fill("http://10.0.0.3:8080");
  await expect(page.getByTestId("sync-insecure-ack")).not.toBeChecked();
  await expect(page.getByTestId("sync-connect")).toBeDisabled();
  // The sign-in flow reads the same entry.
  await page.getByTestId("settings-nav-account").click();
  await enterServer(page, "http://10.0.0.2:8080");
  await expect(page.getByTestId("signin-insecure-ack")).toBeChecked();
});

test("a browser that can't be opened offers the sign-in link to copy", async ({ page }) => {
  const errors = await openAccount(page, "?mockAuthDelay=0&mockAuth=no_browser");
  await enterServer(page, "reqharbor.acme.dev");
  await page.getByTestId("signin-method-github").click();
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await expect(page.getByTestId("signin-browser-failed")).toContainText("Couldn't open your browser. Copy the sign-in link and open it in your browser.");
  await expect(page.getByTestId("signin-waiting")).not.toContainText("We opened");
  await page.getByTestId("signin-copy-link").click();
  const copied = await page.evaluate(() => (window as unknown as { __lastCopied?: string }).__lastCopied);
  expect(copied).toContain("https://reqharbor.acme.dev/auth/authorize?client_id=reqharbor-desktop");
  await expect(page.getByText("Sign-in link copied")).toBeVisible();
  await finishBrowserNow(page);
  await expect(page.getByTestId("account-name")).toHaveText("Priya Shah");
  expect(errors).toEqual([]);
});

test("browser sign-in: waiting, success, avatar menu, devices, sign out", async ({ page }) => {
  const errors = await openAccount(page, "?mockAuthDelay=0");
  await enterServer(page, "reqharbor.acme.dev");
  await page.getByTestId("signin-method-github").click();
  await expect(page.getByTestId("signin-waiting")).toContainText("Waiting for you to finish in your browser…");
  await expect(page.getByTestId("signin-waiting")).toContainText("We opened reqharbor.acme.dev in your browser.");
  await expect(page.getByTestId("signin-elapsed")).toHaveText(/Waiting 0:0\d/);
  await expect(page.getByTestId("signin-reopen")).toBeFocused();
  await page.getByTestId("signin-reopen").click();
  await finishBrowser(page);
  await expect(page.getByTestId("account-name")).toHaveText("Priya Shah");
  await expect(page.getByTestId("account-meta")).toHaveText("priya@acme.dev · Signed in with GitHub · reqharbor.acme.dev");
  await expect(page.getByText("Signed in as Priya Shah")).toBeVisible();
  await expect(page.getByTestId("device-row")).toHaveCount(2);
  await expect(page.getByTestId("devices-table")).toContainText("This device");

  // Sign out another device.
  await page.getByTestId("device-signout").click();
  await expect(page.getByTestId("confirm-dialog")).toContainText("Sign out ubuntu-desk (Linux)?");
  await page.getByTestId("confirm-ok").click();
  await expect(page.getByTestId("device-row")).toHaveCount(1);

  // Footer avatar menu. Closing Settings brings up the first-sync checklist (UX §10): keep all.
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("first-sync-dialog")).toBeVisible();
  await page.getByTestId("first-sync-keep").click();
  await expect(page.getByTestId("first-sync-dialog")).toHaveCount(0);
  await expect(page.getByTestId("account-avatar")).toHaveText("PS");
  await page.getByTestId("account-avatar").click();
  await expect(page.getByTestId("account-menu-heading")).toContainText("priya@acme.dev · reqharbor.acme.dev");
  await page.getByTestId("account-menu-signout").click();
  await expect(page.getByTestId("confirm-dialog")).toContainText("Your projects stay on this device.");
  await page.getByTestId("confirm-cancel").click();
  await page.getByTestId("account-avatar").click();
  await page.getByTestId("account-menu-settings").click();
  await expect(page.getByTestId("account-profile")).toBeVisible();

  await page.getByTestId("account-signout").click();
  await page.getByTestId("confirm-ok").click();
  await expect(page.getByTestId("signin-flow")).toBeVisible();
  await expect(page.getByTestId("account-avatar")).toHaveCount(0);
  // The server is now a suggestion.
  await expect(page.locator("#known-servers option")).toHaveAttribute("value", "https://reqharbor.acme.dev");
  expect(errors).toEqual([]);
});

test("cancel returns to the server entry", async ({ page }) => {
  await openAccount(page, "?mockAuthDelay=0");
  await enterServer(page, "reqharbor.acme.dev");
  await page.getByTestId("signin-method-password").click();
  await page.getByTestId("signin-cancel").click();
  await expect(page.getByTestId("signin-server")).toHaveValue("reqharbor.acme.dev");
  await expect(page.getByTestId("signin-method-password")).toBeEnabled();
});

for (const [mode, text, button] of [
  ["timeout", "Sign-in timed out. Try again.", "Sign in"],
  ["denied", "Sign-in was cancelled in the browser.", "Sign in"],
  ["offline", "Can't reach the server.", "Try again"],
  ["keychain", "ReqHarbor couldn't save your sign-in to the system keychain.", "Try again"],
] as const) {
  test(`sign-in error: ${mode}`, async ({ page }) => {
    await openAccount(page, `?mockAuth=${mode}&mockAuthDelay=0`);
    await enterServer(page, "reqharbor.acme.dev");
    await page.getByTestId("signin-method-github").click();
    await finishBrowser(page);
    await expect(page.getByTestId("signin-error")).toContainText(text);
    await expect(page.getByTestId("signin-retry")).toHaveText(button);
    await expect(page.getByTestId("signin-retry")).toBeFocused();
  });
}

test("sign-in error: no_port offers token sync instead", async ({ page }) => {
  await openAccount(page, "?mockAuth=no_port");
  await enterServer(page, "reqharbor.acme.dev");
  await page.getByTestId("signin-method-github").click();
  await expect(page.getByTestId("signin-error")).toContainText("ReqHarbor couldn't start sign-in on this computer. You can still sync with a token under Sync › Advanced.");
  await page.getByTestId("signin-open-advanced").last().click();
  await expect(page.getByTestId("sync-worker-url")).toBeVisible();
});

test("email_in_use: the app keeps waiting while the browser explains", async ({ page }) => {
  await openAccount(page, "?mockAuth=email_in_use&mockAuthDelay=0");
  await enterServer(page, "reqharbor.acme.dev");
  await page.getByTestId("signin-method-github").click();
  await finishBrowser(page);
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
});

test("session ended: banner and Sign in again", async ({ page }) => {
  await openAccount(page, "?mockAccount=signInRequired&mockAuthDelay=0");
  await expect(page.getByTestId("account-session-ended")).toContainText("Your session ended. Sign in again to keep syncing. Your changes are saved on this device.");
  await page.getByTestId("account-signin-again").click();
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await finishBrowser(page);
  await expect(page.getByTestId("account-session-ended")).toHaveCount(0);
  await expect(page.getByTestId("device-row")).toHaveCount(2);
});

test("keychain unreadable and file store banners", async ({ page }) => {
  await openAccount(page, "?mockAccount=keychainUnreadable");
  await expect(page.getByTestId("account-keychain-unreadable")).toContainText("ReqHarbor couldn't read your sign-in from the keychain.");
  await expect(page.getByTestId("signin-flow")).toBeVisible();
  await page.goto("/?mockAccount=file");
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-account").click();
  await expect(page.getByTestId("account-file-store")).toContainText("stored in a file because no system keychain was found");
});

test("token sync under Sync › Advanced still works while signed in", async ({ page }) => {
  await openAccount(page, "?mockAccount=signedIn");
  await expect(page.getByTestId("account-profile")).toBeVisible();
  await page.getByTestId("settings-nav-sync").click();
  // Signed in, token sync sits under a collapsed Advanced (UX §11).
  await expect(page.getByTestId("sync-worker-url")).toHaveCount(0);
  await page.getByTestId("sync-advanced").click();
  await page.getByTestId("sync-worker-url").fill("https://sync.example.dev");
  await page.getByTestId("sync-connect").click();
  await expect(page.getByTestId("sync-token")).toHaveValue(/^rh_/);
  await expect(page.getByTestId("legacy-block")).toContainText("Synced with a token");
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("sync-indicator")).toContainText("Synced");
  await expect(page.getByTestId("account-avatar")).toBeVisible();
});

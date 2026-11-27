/**
 * Personal sync and bindings against the mock backend (04 §12 M1d, 06-go-server §10 C2, UX §4.3,
 * §5, §6.6, §10, §11, §12.3): the first-sync checklist, the grouped switcher, New project and
 * Sync location, the reworked Settings › Sync, the legacy merge, the different-account notice
 * and the per-space footer tooltip.
 */
import { expect, test, type Page } from "@playwright/test";

const PERSONAL = "01JSPACEPERSONAL000000000";

async function open(page: Page, qs = "") {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/" + qs);
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

async function signIn(page: Page) {
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-account").click();
  const input = page.getByTestId("signin-server");
  await input.fill("reqharbor.acme.dev");
  await input.press("Enter");
  await page.getByTestId("signin-method-github").click();
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await page.evaluate(() => (window as unknown as { __mock: { accounts: { finishBrowser(): Promise<void> } } }).__mock.accounts.finishBrowser());
  await expect(page.getByTestId("account-name")).toHaveText("Priya Shah");
  await page.getByTestId("dialog-close").click();
}

async function openSwitcher(page: Page) {
  await page.getByTestId("project-switcher").click();
  await expect(page.getByTestId("context-menu")).toBeVisible();
}

test("first sign-in asks what to sync; nothing is bound before the answer", async ({ page }) => {
  const errors = await open(page, "?mockAuthDelay=0");
  // Signed out: today's flat switcher, layers icon, no headings.
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "layers");
  await openSwitcher(page);
  await expect(page.locator("[data-testid^=project-group-]")).toHaveCount(0);
  await page.keyboard.press("Escape");

  await signIn(page);
  const dlg = page.getByTestId("first-sync-dialog");
  await expect(dlg).toContainText("Sync these projects to your account?");
  await expect(dlg.getByTestId("first-sync-project")).toHaveCount(2);
  await expect(dlg.getByTestId("first-sync-apply")).toHaveText("Sync 2 projects");
  await dlg.locator("[data-testid=first-sync-project][data-name='Side project']").uncheck();
  await expect(dlg).toContainText("This device only");
  await expect(dlg.getByTestId("first-sync-apply")).toHaveText("Sync 1 project");
  await dlg.getByTestId("first-sync-apply").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByText("Syncing 1 project to your account")).toBeVisible();

  // The switcher is grouped now.
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");
  await expect(page.getByTestId("project-switcher")).toHaveAttribute("title", /Default · Personal · /);
  await openSwitcher(page);
  await expect(page.getByTestId(`project-group-${PERSONAL}`)).toHaveText("Personal");
  await expect(page.getByTestId("project-group-local")).toHaveText("On this device only");
  const options = page.getByTestId("project-option");
  await expect(options).toHaveText(["Default", "Side project"]);
  await expect(options.nth(1)).toHaveAttribute("aria-label", "Side project, On this device only");
  await expect(page.getByTestId("project-sync-location")).toBeVisible();
  expect(errors).toEqual([]);
});

test("closing the checklist keeps everything on this device", async ({ page }) => {
  await open(page, "?mockAuthDelay=0");
  await signIn(page);
  await expect(page.getByTestId("first-sync-dialog")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.getByText("Your projects stay on this device. Change any project's sync location from the project switcher.")).toBeVisible();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "device");
});

test("New project picks a location; Sync location moves it back and forth", async ({ page }) => {
  await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-new").click();
  const dlg = page.getByTestId("new-project-dialog");
  await expect(dlg.getByTestId("new-project-location")).toHaveValue(PERSONAL);
  await expect(dlg.getByTestId("new-project-hint")).toHaveText("Synced to your devices.");
  await dlg.getByTestId("new-project-name").fill("Scratch");
  await dlg.getByTestId("new-project-location").selectOption("local");
  await expect(dlg.getByTestId("new-project-hint")).toHaveText("Never leaves this device.");
  await dlg.getByTestId("new-project-create").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByTestId("project-switcher")).toContainText("Scratch");
  await expect(page.getByTestId("project-switcher")).toHaveAttribute("title", "Scratch · This device only");

  // This device only → Personal.
  await openSwitcher(page);
  await page.getByTestId("project-sync-location").click();
  const loc = page.getByTestId("sync-location-dialog");
  await expect(loc).toContainText("Sync location for “Scratch”");
  await loc.getByTestId("sync-location-personal").check();
  await expect(loc.getByTestId("sync-location-apply")).toHaveText("Start syncing");
  await loc.getByTestId("sync-location-apply").click();
  await expect(page.getByText("“Scratch” now syncs to Personal")).toBeVisible();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");

  // Personal → this device: the warning and a danger "Stop syncing".
  await openSwitcher(page);
  await page.getByTestId("project-sync-location").click();
  await loc.getByTestId("sync-location-local").check();
  await expect(loc.getByTestId("sync-location-warn")).toHaveText("Removes it from your other devices. This copy stays.");
  await expect(loc.getByTestId("sync-location-apply")).toHaveText("Stop syncing");
  await loc.getByTestId("sync-location-apply").click();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "device");
});

test("Settings › Sync signed in: spaces, Advanced collapsed; signed out: pointer to Account", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn");
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  const row = page.getByTestId(`sync-space-${PERSONAL}`);
  await expect(row).toContainText("Personal");
  await expect(row).toContainText("2 projects");
  await expect(row).toContainText("12 MB of 100 MB");
  await expect(page.getByTestId("sync-now")).toBeEnabled();
  await expect(page.getByTestId("sync-advanced")).toHaveAttribute("aria-expanded", "false");
  await expect(page.getByTestId("sync-worker-url")).toHaveCount(0);
  await page.getByTestId("sync-now").click();
  await expect(page.getByText(/Synced · \d+ pushed/)).toBeVisible();

  // Sign out: the spaces pause, the section points to Account, Advanced is open.
  await page.getByTestId("settings-nav-account").click();
  await page.getByTestId("account-signout").click();
  await page.getByTestId("confirm-ok").click();
  await page.getByTestId("settings-nav-sync").click();
  await expect(page.getByTestId("sync-signin-note")).toContainText("Sign in to sync across devices.");
  await expect(page.getByTestId(`sync-space-msg-${PERSONAL}`)).toHaveText("Paused: signed out");
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("sync-indicator")).toHaveCount(0);
  await openSwitcher(page);
  await expect(page.getByTestId(`project-group-${PERSONAL}`)).toHaveText("Personal · Paused: signed out");
  expect(errors).toEqual([]);
});

test("token sync is merged into the personal space", async ({ page }) => {
  await open(page, "?mockLegacy=1&mockAuthDelay=0");
  await signIn(page);
  // Every project is legacy-bound: no checklist.
  await expect(page.getByTestId("first-sync-dialog")).toHaveCount(0);
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "key");
  await openSwitcher(page);
  await expect(page.getByTestId("project-group-legacy")).toHaveText("Legacy workspace");
  await page.keyboard.press("Escape");

  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await expect(page.getByTestId("legacy-block")).toContainText("Synced with a token · 2 projects");
  await page.getByTestId("merge-legacy").click();
  const dlg = page.getByTestId("merge-legacy-dialog");
  await expect(dlg).toContainText("Your 2 token-synced projects and your Global environment move into your personal space.");
  await dlg.getByTestId("merge-legacy-confirm").click();
  await expect(page.getByText("Merged into your personal space.")).toBeVisible();
  await expect(page.getByTestId("legacy-block")).toContainText("Shares only · syncing stopped");
  await expect(page.getByTestId("legacy-disconnect")).toBeVisible();
  await page.getByTestId("legacy-disconnect").click();
  await expect(page.getByTestId("confirm-dialog")).toContainText("share links keep working");
  await page.getByTestId("confirm-ok").click();
  await expect(page.getByTestId("legacy-block")).toHaveCount(0);
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");
});

test("signing in as a different account: Keep is the default, Remove asks first", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn&mockForeign=1");
  const dlg = page.getByTestId("space-notice-dialog");
  await expect(dlg).toContainText("You signed in as priya@acme.dev");
  await expect(dlg).toContainText("These projects were synced by sam@acme.dev.");
  await expect(dlg.getByTestId("space-notice-row")).toContainText("Personal (2 projects)");
  await expect(dlg.getByTestId("space-notice-choice")).toHaveValue("keep");
  await expect(dlg.getByTestId("space-notice-continue")).toBeFocused();
  await dlg.getByTestId("space-notice-choice").selectOption("remove");
  await dlg.getByTestId("space-notice-continue").click();
  await expect(page.getByTestId("confirm-dialog")).toContainText("Remove 2 projects from this device?");
  await page.getByTestId("confirm-cancel").click();
  await dlg.getByTestId("space-notice-choice").selectOption("keep");
  await dlg.getByTestId("space-notice-continue").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByText("Kept on this device only")).toBeVisible();
  await openSwitcher(page);
  await expect(page.getByTestId("project-group-local")).toBeVisible();
  await expect(page.getByTestId("project-option")).toHaveCount(2);
  expect(errors).toEqual([]);
});

test("Cancel sign-in on the different-account notice signs the new account out", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockForeign=1");
  await page.getByTestId("space-notice-cancel-signin").click();
  await expect(page.getByTestId("account-avatar")).toHaveCount(0);
  await expect(page.getByText("Signed out on this device")).toBeVisible();
});

test("the footer chip lists every space and flags issues", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockSpace=quota&mockTeam=1");
  const chip = page.getByTestId("sync-indicator");
  await expect(chip).toContainText("Synced");
  const title = await chip.getAttribute("title");
  expect(title?.split("\n").slice(1)).toEqual(["Personal · Personal storage full (100 MB).", "Acme · Synced"]);
  await expect(chip).toHaveAttribute("aria-label", title ?? "");
  await expect(page.getByTestId("sync-warning")).toHaveAttribute("title", /Personal: Personal storage full/);
  await page.getByTestId("sync-warning").click();
  await expect(page.getByTestId(`sync-space-msg-${PERSONAL}`)).toContainText("Personal storage full (100 MB).");
  await page.getByTestId("dialog-close").click();

  // A team group with its role; team locations wait for teams.
  await openSwitcher(page);
  await expect(page.getByTestId("project-group-01JSPACEACME0000000000000")).toContainText("Acme");
  await expect(page.getByTestId("project-group-01JSPACEACME0000000000000")).toContainText("Editor");
  await page.getByTestId("project-new").click();
  // M2: an editor may create projects in the team.
  await expect(page.locator("[data-testid=new-project-location] option[disabled]")).toHaveCount(0);
  await expect(page.locator(`[data-testid=new-project-location] option[value="01JSPACEACME0000000000000"]`)).toHaveText("Acme");
});

test("sharing a device-only project offers Sync to Personal", async ({ page }) => {
  await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-sync-location").click();
  await page.getByTestId("sync-location-local").check();
  await page.getByTestId("sync-location-apply").click();
  await page.getByTestId("sidebar-tab-collections").click();
  await page.locator('[data-testid="tree-node"][data-name="Users API"]').click();
  await page.locator('[data-testid="tree-node"][data-name="Users"]').click();
  await page.locator('[data-testid="tree-node"][data-name="Search users"]').click();
  await page.getByTestId("request-menu").click();
  await page.getByTestId("menu-share").click();
  const share = page.getByTestId("share-needs-sync");
  await expect(share).toContainText("Sharing needs this project to sync. Change its sync location to Personal first.");
  await page.getByTestId("share-sync-personal").click();
  await expect(page.getByText(/now syncs to Personal/)).toBeVisible();
  await expect(share).toHaveCount(0);
  await expect(page.getByTestId("share-dialog")).toContainText("Removed before sharing");
});

test("signing back in as the original account resumes its projects without a notice", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn&mockForeign=1&mockAuthDelay=0");
  // Priya signed in over Sam's projects: the notice; Cancel sign-in.
  await expect(page.getByTestId("space-notice-dialog")).toBeVisible();
  await page.getByTestId("space-notice-cancel-signin").click();
  await expect(page.getByTestId("account-avatar")).toHaveCount(0);
  // Sam (who synced them) signs in again.
  await page.evaluate(() => {
    (window as unknown as { __mock: { accounts: { identity: unknown } } }).__mock.accounts.identity = { userId: "01JUSERSAM000000000000000", email: "sam@acme.dev", name: "Sam Lee" };
  });
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-account").click();
  const input = page.getByTestId("signin-server");
  await input.fill("reqharbor.acme.dev");
  await input.press("Enter");
  await page.getByTestId("signin-method-github").click();
  await expect(page.getByTestId("signin-waiting")).toBeVisible();
  await page.evaluate(() => (window as unknown as { __mock: { accounts: { finishBrowser(): Promise<void> } } }).__mock.accounts.finishBrowser());
  await expect(page.getByTestId("account-name")).toHaveText("Sam Lee");
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");
  await page.waitForTimeout(300);
  await expect(page.getByTestId("space-notice-dialog")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("a fresh device says what syncs after sign-in", async ({ page }) => {
  await open(page, "?seed=empty&mockAuthDelay=0");
  await signIn(page);
  await expect(page.getByText("Signed in · syncing 1 project from your account.")).toBeVisible();
  await expect(page.getByTestId("first-sync-dialog")).toHaveCount(0);
});

test("a token-synced project moves to Personal only through the merge", async ({ page }) => {
  await open(page, "?mockLegacy=1&mockAuthDelay=0");
  await signIn(page);
  await openSwitcher(page);
  await page.getByTestId("project-sync-location").click();
  const dlg = page.getByTestId("sync-location-dialog");
  await expect(dlg.getByTestId("sync-location-personal")).toBeDisabled();
  await expect(dlg.getByTestId("sync-location-legacy-note")).toContainText("This project syncs with a token.");
  // Legacy → this device only stays possible, with the warning.
  await dlg.getByTestId("sync-location-local").check();
  await expect(dlg.getByTestId("sync-location-warn")).toContainText("Removes it from your other devices.");
  await page.getByTestId("dialog-close").click();
  await expect(dlg).toHaveCount(0);
  // The pointer opens the merge.
  await openSwitcher(page);
  await page.getByTestId("project-sync-location").click();
  await page.getByTestId("sync-location-merge").click();
  await expect(page.getByTestId("sync-location-dialog")).toHaveCount(0);
  await expect(page.getByTestId("merge-legacy-dialog")).toBeVisible();
});

test("a merge whose follow-up sync fails is still a merge", async ({ page }) => {
  await open(page, "?mockLegacy=1&mockMergeSyncFails=1&mockAuthDelay=0");
  await signIn(page);
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await page.getByTestId("merge-legacy").click();
  await page.getByTestId("merge-legacy-confirm").click();
  await expect(page.getByTestId("merge-legacy-dialog")).toHaveCount(0);
  await expect(page.getByText(/Merged\. Syncing didn't finish/)).toBeVisible();
  await expect(page.getByTestId("legacy-block")).toContainText("Shares only · syncing stopped");
});

test("× on the different-account notice is remembered as decide later", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockForeign=1");
  await expect(page.getByTestId("space-notice-dialog")).toBeVisible();
  await page.getByTestId("dialog-close").click();
  await expect(page.getByTestId("space-notice-dialog")).toHaveCount(0);
  await expect
    .poll(() => page.evaluate(() => (window as unknown as { __mock: { spaces: { bindings: { decideLaterAt?: number | null }[] } } }).__mock.spaces.bindings.some((b) => typeof b.decideLaterAt === "number")))
    .toBe(true);
});

test("sharing from a project paused for another account points to Sync settings", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockForeign=1");
  await page.getByTestId("dialog-close").click();
  await page.getByTestId("sidebar-tab-collections").click();
  await page.locator('[data-testid="tree-node"][data-name="Users API"]').click();
  await page.locator('[data-testid="tree-node"][data-name="Users"]').click();
  await page.locator('[data-testid="tree-node"][data-name="Search users"]').click();
  await page.getByTestId("request-menu").click();
  await page.getByTestId("menu-share").click();
  await expect(page.getByTestId("share-paused")).toContainText("This project's sync is paused");
  await expect(page.getByTestId("share-sync-personal")).toHaveCount(0);
});

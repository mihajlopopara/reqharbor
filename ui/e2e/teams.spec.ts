/**
 * Teams against the mock backend (04 §12 M2b, UX §5–§9, §12): joining with an invite link, a
 * viewer's read-only affordances, the Move to team pre-flight, the removal notice, and
 * Settings › Teams (create, invite, roles, remove, delete).
 */
import { expect, test, type Page } from "@playwright/test";

const TEAM = "01JSPACEACME0000000000000";
const SAM = "01JUSERSAM000000000000000";
const OLU = "01JUSEROLU000000000000000";

async function open(page: Page, qs = "") {
  const errors: string[] = [];
  page.on("pageerror", (e) => errors.push(e.message));
  await page.goto("/" + qs);
  await expect(page.getByTestId("app")).toBeVisible();
  return errors;
}

async function openSwitcher(page: Page) {
  await page.getByTestId("project-switcher").click();
  await expect(page.getByTestId("context-menu")).toBeVisible();
}

async function switchTo(page: Page, name: string) {
  await openSwitcher(page);
  await page.getByTestId("project-option").filter({ hasText: name }).click();
  await expect(page.getByTestId("project-switcher")).toContainText(name);
}

test("join a team with an invite link", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-join-team").click();
  const dlg = page.getByTestId("join-team-dialog");
  await expect(dlg.getByTestId("join-team-input")).toBeFocused();
  // Not an invite link.
  await dlg.getByTestId("join-team-input").fill("https://reqharbor.acme.dev/s/abc");
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-error")).toHaveText("That isn't a ReqHarbor invite link.");
  // A valid one: preview, then join.
  await dlg.getByTestId("join-team-input").fill("https://reqharbor.acme.dev/i#k3J9Qz");
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg).toContainText("Join Globex?");
  await expect(dlg.getByTestId("join-preview")).toContainText("Hank Scorpio invited you · you'll join as Editor");
  await expect(dlg).toContainText("Team data is visible to all members and stored on reqharbor.acme.dev.");
  await dlg.getByTestId("join-team-confirm").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByText("Joined Globex · syncing 2 projects")).toBeVisible();
  await openSwitcher(page);
  await expect(page.locator("[data-testid^=project-group-]").filter({ hasText: "Globex" })).toBeVisible();
  await expect(page.getByTestId("project-option").filter({ hasText: "Globex API" })).toHaveAttribute("aria-label", "Globex API, Globex");
  await page.keyboard.press("Escape");
  // The same link again: already a member.
  await openSwitcher(page);
  await page.getByTestId("project-join-team").click();
  await dlg.getByTestId("join-team-input").fill("https://reqharbor.acme.dev/i#k3J9Qz");
  await dlg.getByTestId("join-team-continue").click();
  await dlg.getByTestId("join-team-confirm").click();
  await expect(dlg.getByTestId("join-already")).toHaveText("You're already in Globex as Editor. Your role hasn't changed.");
  await expect(dlg.getByTestId("join-team-confirm")).toHaveText("Open Globex");
  expect(errors).toEqual([]);
});

test("an expired invite and a link for another server", async ({ page }) => {
  await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-join-team").click();
  const dlg = page.getByTestId("join-team-dialog");
  await dlg.getByTestId("join-team-input").fill("https://reqharbor.acme.dev/i#expired");
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-error")).toHaveText("This invite link is no longer valid. Ask the person who sent it for a new one.");
  await dlg.getByTestId("join-team-input").fill("https://api.globex.dev/i#tok");
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-other-server")).toHaveText("This invite is for api.globex.dev. You're signed in to reqharbor.acme.dev.");
  await expect(dlg.getByTestId("join-switch-server")).toHaveText("Sign in to api.globex.dev");
});

test("a viewer sees view-only affordances and can only copy out", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn&mockTeam=1&mockRole=viewer");
  await switchTo(page, "Side project");
  await expect(page.getByTestId("view-only-chip")).toHaveText("View only");
  await page.getByTestId("sidebar-tab-collections").click();
  await expect(page.getByTestId("viewer-line")).toContainText("View only in Acme · Duplicate to personal to make changes");
  // Project menu: Rename and Delete are disabled with the reason.
  await openSwitcher(page);
  await expect(page.getByTestId("project-rename")).toHaveAttribute("aria-disabled", "true");
  await expect(page.getByTestId("project-delete")).toHaveAttribute("aria-description", "Viewers can't change team projects");
  await expect(page.getByTestId("project-move-team")).toHaveCount(0);
  await page.keyboard.press("Escape");
  // Tree menu: only copy out.
  const node = page.getByTestId("tree-node").first();
  await node.click({ button: "right" });
  await expect(page.getByTestId("tree-rename")).toHaveAttribute("aria-disabled", "true");
  await expect(page.getByTestId("tree-share")).toHaveAttribute("aria-description", "Viewers can't create share links");
  await expect(page.getByTestId("tree-duplicate-personal")).toBeVisible();
  await page.keyboard.press("Escape");
  // Save explains instead of saving.
  await page.locator("[data-testid=tree-node][data-kind=collection]").first().click();
  await page.locator("[data-testid=tree-node][data-kind=request]").first().click();
  await expect(page.getByTestId("save-request")).toHaveAttribute("aria-disabled", "true");
  await page.keyboard.press("ControlOrMeta+s");
  await expect(page.getByText("Viewers can't change team projects. Duplicate to personal to experiment.")).toBeVisible();
  // Duplicate to personal: a Personal copy with new ids.
  await page.getByTestId("viewer-duplicate").click();
  await expect(page.getByText("Duplicated to Personal as “Side project copy”")).toBeVisible();
  await expect(page.getByTestId("project-switcher")).toContainText("Side project copy");
  await expect(page.getByTestId("view-only-chip")).toHaveCount(0);
  expect(errors).toEqual([]);
});

test("Move to team shows the pre-flight, then moves with new ids", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn&mockTeam=1");
  await openSwitcher(page);
  await page.getByTestId("project-move-team").click();
  const dlg = page.getByTestId("move-team-dialog");
  await expect(dlg).toContainText("Move “Default” to a team");
  await expect(dlg.getByTestId("move-team-target")).toHaveValue(TEAM);
  await expect(dlg.getByTestId("move-secrets")).toContainText("secret variable");
  await expect(dlg.getByTestId("move-secrets")).toContainText("Each member sets their own value.");
  await expect(dlg).toContainText("Visible to everyone in Acme (4 members)");
  await expect(dlg.getByTestId("move-clean")).toHaveText("No plain-text credentials found");
  await expect(dlg).toContainText("Existing share links stay with your personal space and keep working.");
  await expect(dlg.getByTestId("move-team-confirm")).toHaveText("Move to Acme");
  await dlg.getByTestId("move-team-confirm").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByText(/Moved “Default” to Acme/)).toBeVisible();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "users");
  await openSwitcher(page);
  await expect(page.getByTestId("project-option").filter({ hasText: "Default" })).toHaveAttribute("aria-label", "Default, Acme");
  expect(errors).toEqual([]);
});

test("a move that fails moves nothing", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockMove=offline");
  await openSwitcher(page);
  await page.getByTestId("project-move-team").click();
  const dlg = page.getByTestId("move-team-dialog");
  await dlg.getByTestId("move-team-confirm").click();
  await expect(dlg.getByTestId("move-error")).toContainText("You're offline. Nothing was moved.");
  await expect(dlg.getByTestId("move-team-confirm")).toHaveText("Retry");
  await dlg.getByTestId("move-cancel").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");
});

test("credentials typed as plain text are listed before a move", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1");
  await page.evaluate(() => {
    const m = (window as unknown as { __mock: { requests: { headers: { key: string; value: string; enabled: boolean }[]; name: string }[] } }).__mock;
    m.requests[0].headers.push({ key: "Authorization", value: "Bearer s3cr3t", enabled: true });
  });
  await openSwitcher(page);
  await page.getByTestId("project-move-team").click();
  const dlg = page.getByTestId("move-team-dialog");
  await expect(dlg.getByTestId("move-credentials")).toHaveText("1 credential typed as plain text:");
  await expect(dlg.getByTestId("move-credential-row")).toContainText("· Authorization header");
  await expect(dlg).toContainText("Teammates will see these values.");
});

test("the removal notice keeps a personal copy by default", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn&mockTeam=1&mockSpace=removed");
  const dlg = page.getByTestId("space-notice-dialog");
  await expect(dlg).toContainText("You were removed from Acme");
  await expect(dlg).toContainText("Acme's projects no longer sync to this device.");
  await expect(dlg).toContainText("1 project is still here: Side project.");
  await expect(dlg.getByTestId("space-notice-keep")).toBeFocused();
  await dlg.getByTestId("space-notice-keep").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByText("Kept on this device only")).toBeVisible();
  await openSwitcher(page);
  await expect(page.getByTestId("project-option").filter({ hasText: "Side project (from Acme)" })).toHaveAttribute("aria-label", "Side project (from Acme), On this device only");
  expect(errors).toEqual([]);
});

test("a removal noticed by sync opens the notice; Remove asks first", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1");
  await page.evaluate(() => {
    const m = (window as unknown as { __mock: { teams: { removeMe(n: string): void }; emitStatus(): void } }).__mock;
    m.teams.removeMe("Acme");
    m.emitStatus();
  });
  const dlg = page.getByTestId("space-notice-dialog");
  await expect(dlg).toContainText("You were removed from Acme");
  await dlg.getByTestId("space-notice-remove").click();
  const confirm = page.getByTestId("confirm-dialog");
  await expect(confirm).toContainText("Remove 1 project from this device?");
  await confirm.getByTestId("confirm-ok").click();
  await expect(page.getByText("Removed 1 project from this device")).toBeVisible();
  await openSwitcher(page);
  await expect(page.getByTestId("project-option").filter({ hasText: "Side project" })).toHaveCount(0);
});

test("the role-changed banner offers Save as personal copy and Discard", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockSpace=roleChanged");
  await switchTo(page, "Side project");
  await page.getByTestId("sidebar-tab-collections").click();
  const banner = page.getByTestId("role-banner");
  await expect(banner).toContainText("Your role in Acme changed to Viewer. You have 2 unsynced changes.");
  await banner.getByTestId("role-banner-save").click();
  await expect(page.getByText("Saved your changes as a personal copy")).toBeVisible();
  await expect(banner).toHaveCount(0);
  await openSwitcher(page);
  await expect(page.getByTestId("project-option").filter({ hasText: "Side project (my changes)" })).toHaveAttribute("aria-label", "Side project (my changes), Personal");
});

test("Settings › Teams: create a team and an invite link", async ({ page }) => {
  const errors = await open(page, "?mockAccount=signedIn");
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-teams").click();
  await expect(page.getByTestId("teams-empty")).toContainText("No teams yet");
  await page.getByTestId("team-create-open").first().click();
  const create = page.getByTestId("create-team-dialog");
  await create.getByTestId("create-team-name").fill("Globex Labs");
  await create.getByTestId("create-team-confirm").click();
  await expect(create).toHaveCount(0);
  await expect(page.getByTestId("team-name")).toHaveText("Globex Labs");
  await expect(page.getByTestId("team-my-role")).toHaveText("Owner");
  await page.getByTestId("team-invite-open").click();
  const inv = page.getByTestId("invite-dialog");
  await expect(inv.getByTestId("invite-sentence")).toContainText("One person can join as Editor until");
  // Admin is single-use: more uses reset the role.
  await inv.getByTestId("invite-role").selectOption("admin");
  await inv.getByTestId("invite-uses").selectOption("5");
  await expect(inv.getByTestId("invite-role")).toHaveValue("editor");
  await expect(inv.getByTestId("invite-sentence")).toContainText("Admin links are single-use, so the role is now Editor.");
  await inv.getByTestId("invite-create").click();
  await expect(inv.getByTestId("invite-link")).toHaveValue(/^https:\/\/reqharbor\.acme\.dev\/i#k/);
  await expect(inv).toContainText("Copy it now. For security, the link can't be shown again.");
  await inv.getByTestId("invite-done").click();
  await expect(page.getByTestId("invite-row")).toContainText("Editor · 5 of 5 left · expires in 7 days · by Priya");
  await page.getByTestId("invite-revoke").click();
  await expect(page.getByTestId("invites-empty")).toBeVisible();
  expect(errors).toEqual([]);
});

test("Settings › Teams: roles, remove with revoke, delete with type-to-confirm", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockRole=owner");
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-teams").click();
  await page.getByTestId("team-row").filter({ hasText: "Acme" }).click();
  await expect(page.getByTestId("team-members")).toContainText("Priya Shah (you)");
  // My own row is the billing owner: read-only.
  await page.getByTestId(`member-role-${SAM}`).selectOption("admin");
  await expect(page.getByText("Sam is now Admin")).toBeVisible();
  await expect(page.getByTestId(`member-role-${SAM}`)).toHaveValue("admin");
  await page.getByTestId(`member-more-${OLU}`).click();
  await page.getByTestId("member-remove").click();
  const confirm = page.getByTestId("confirm-dialog");
  await expect(confirm).toContainText("Remove Olu Okafor from Acme?");
  await expect(confirm.getByTestId("remove-revoke-invites")).toContainText("Also revoke all active invite links (1)");
  await expect(confirm.getByTestId("remove-revoke-invites").locator("input")).toBeChecked();
  await expect(confirm.getByTestId("confirm-cancel")).toBeFocused();
  await confirm.getByTestId("confirm-ok").click();
  await expect(page.getByText("Removed Olu Okafor from Acme")).toBeVisible();
  await expect(page.getByTestId("member-row")).toHaveCount(2);
  await expect(page.getByTestId("invites-empty")).toBeVisible();
  // Delete: type the name to confirm.
  await page.getByTestId("team-delete").click();
  await expect(confirm.getByTestId("confirm-details")).toContainText("1 project and everything in them");
  await expect(confirm.getByTestId("confirm-ok")).toBeDisabled();
  await confirm.getByTestId("confirm-type").fill("Acme");
  await confirm.getByTestId("confirm-ok").click();
  await expect(page.getByText("Deleted Acme")).toBeVisible();
  await expect(page.getByTestId("teams-empty")).toBeVisible();
});

test("teams are hidden when the server doesn't advertise them", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeams=off");
  await page.getByTestId("settings-button").click();
  await expect(page.getByTestId("settings-nav-teams")).toHaveCount(0);
  await page.getByTestId("dialog-close").click();
  await openSwitcher(page);
  await expect(page.getByTestId("project-join-team")).toHaveCount(0);
});

test("an unfinished move is offered from the project menu and Settings › Sync", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockMove=offline");
  await openSwitcher(page);
  await page.getByTestId("project-move-team").click();
  const dlg = page.getByTestId("move-team-dialog");
  await dlg.getByTestId("move-team-confirm").click();
  await expect(dlg.getByTestId("move-error")).toContainText("Nothing was moved.");
  // Close with ×: the move stays pending, nothing changed here.
  await dlg.getByTestId("dialog-close").click();
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "cloud");
  await openSwitcher(page);
  await expect(page.getByTestId("project-finish-move")).toHaveText("Finish move to Acme…");
  await expect(page.getByTestId("project-move-team")).toHaveCount(0);
  await page.keyboard.press("Escape");
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-sync").click();
  await expect(page.getByTestId("pending-move")).toContainText("Moving “Default” to Acme didn't finish");
  // Finish: the dialog opens on the same target and the retry completes.
  await page.getByTestId("pending-move-finish").click();
  await expect(dlg.getByTestId("move-team-target")).toHaveValue(TEAM);
  await dlg.getByTestId("move-team-confirm").click();
  await expect(dlg).toHaveCount(0);
  await expect(page.getByTestId("project-binding-icon")).toHaveAttribute("data-icon", "users");
});

test("Cancel move after a failure undoes it", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockMove=quota");
  await openSwitcher(page);
  await page.getByTestId("project-move-team").click();
  const dlg = page.getByTestId("move-team-dialog");
  await dlg.getByTestId("move-team-confirm").click();
  await expect(dlg.getByTestId("move-error")).toContainText("Acme's storage is full");
  await dlg.getByTestId("move-cancel").click();
  await expect(page.getByText("Move cancelled. Nothing was moved.")).toBeVisible();
  await openSwitcher(page);
  await expect(page.getByTestId("project-finish-move")).toHaveCount(0);
  await expect(page.getByTestId("project-move-team")).toBeVisible();
});

test("Create team is disabled at the server's owned-team limit", async ({ page }) => {
  await open(page, "?mockAccount=signedIn&mockTeam=1&mockRole=owner");
  await page.evaluate(() => {
    const m = (window as unknown as { __mock: { teams: { create(n: string): unknown } } }).__mock;
    m.teams.create("Two");
    m.teams.create("Three");
  });
  await page.getByTestId("settings-button").click();
  await page.getByTestId("settings-nav-teams").click();
  await expect(page.getByTestId("team-row")).toHaveCount(3);
  await expect(page.getByTestId("team-create-open")).toBeDisabled();
  await expect(page.getByTestId("teams-create-lock")).toHaveText("You can own up to 3 teams on this server.");
});

test("an invite for a plain-http server asks to continue first", async ({ page }) => {
  await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-join-team").click();
  const dlg = page.getByTestId("join-team-dialog");
  await dlg.getByTestId("join-team-input").fill("http://192.168.1.5:8080/i#tok");
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-insecure")).toContainText("This server doesn't use HTTPS.");
  await expect(dlg.getByTestId("join-team-continue")).toBeDisabled();
  await dlg.getByTestId("join-insecure-ok").check();
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-other-server")).toContainText("This invite is for 192.168.1.5:8080.");
});

test("Continue anyway applies only to the link it was given for", async ({ page }) => {
  await open(page, "?mockAccount=signedIn");
  await openSwitcher(page);
  await page.getByTestId("project-join-team").click();
  const dlg = page.getByTestId("join-team-dialog");
  await dlg.getByTestId("join-team-input").fill("http://192.168.1.5:8080/i#tok");
  await dlg.getByTestId("join-team-continue").click();
  await dlg.getByTestId("join-insecure-ok").check();
  // Another plain-http server: the confirmation is gone and asked again.
  await dlg.getByTestId("join-team-input").fill("http://10.0.0.9:8080/i#tok");
  await expect(dlg.getByTestId("join-insecure")).toHaveCount(0);
  await dlg.getByTestId("join-team-continue").click();
  await expect(dlg.getByTestId("join-insecure")).toBeVisible();
  await expect(dlg.getByTestId("join-insecure-ok")).not.toBeChecked();
  await expect(dlg.getByTestId("join-team-continue")).toBeDisabled();
});

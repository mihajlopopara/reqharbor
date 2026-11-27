<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import SignInFlow from "../components/SignInFlow.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type InviteCheck } from "../api";
  import { serverHost } from "../util/account";
  import { plural, roleName } from "../util/teams";

  /**
   * Join a team (UX §8, AC-INV-3…5), 480 px: paste the link, preview, join. Signed out, the
   * sign-in runs inline (with a signup ticket, 06 §10 M2) and the join continues after it.
   */
  let { url: preset = "" }: { url?: string } = $props();
  const close = () => (ui.joinTeam = null);
  // svelte-ignore state_referenced_locally
  let url = $state(preset);
  let step = $state<"paste" | "signin" | "preview">("paste");
  let check = $state<InviteCheck | null>(null);
  let ticket = $state<string | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let already = $state<{ team: string; role: string } | null>(null);
  let offline = $state(false);
  /** Plain http to a non-loopback server: the token goes there only after "Continue anyway" (06 §9). */
  let insecure = $state(false);
  let insecureOk = $state(false);
  // "Continue anyway" applies to the link it was given for: a new link asks again.
  let confirmedFor = "";
  $effect(() => {
    const u = url.trim();
    if (u !== confirmedFor) {
      confirmedFor = u;
      insecure = false;
      insecureOk = false;
    }
  });

  const inviter = $derived(check?.preview.inviterName ?? null);
  const expires = $derived(check?.preview.expiresAt ? new Date(check.preview.expiresAt).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : null);
  const signedHost = $derived(app.auth.account ? serverHost(app.auth.account.serverUrl) : "");

  async function preview() {
    if (busy || !url.trim()) return;
    busy = true;
    error = null;
    offline = false;
    try {
      check = await call("invite_preview", { url: url.trim(), insecureConfirmed: insecureOk });
      insecure = false;
      if (!app.signedIn && !check.otherServer) {
        ticket = await call("invite_ticket", { url: url.trim(), insecureConfirmed: insecureOk }).catch(() => null);
        step = "signin";
      } else step = "preview";
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }

  function fail(e: unknown) {
    const err = toCommandError(e);
    if (err.code === "insecure_unconfirmed") {
      insecure = true;
      error = null;
      return;
    }
    if (err.code === "invite_invalid") error = `This invite link is no longer valid. ${inviter ? `Ask ${inviter} for a new one.` : "Ask the person who sent it for a new one."}`;
    else if (err.code === "server_unreachable") {
      offline = true;
      error = `Can't reach ${(() => {
        try {
          return serverHost(new URL(url.trim().includes("://") ? url.trim() : `https://${url.trim()}`).origin);
        } catch {
          return "the server";
        }
      })()}.`;
    } else error = err.message;
  }

  // Signed in inline: continue to the preview.
  $effect(() => {
    if (step === "signin" && app.signedIn) step = "preview";
  });

  async function join() {
    if (busy || !check) return;
    if (already) {
      close();
      ui.settingsDialog = { section: "teams" };
      return;
    }
    busy = true;
    error = null;
    try {
      const r = await call("invite_accept", { url: url.trim() });
      if (r.alreadyMember) {
        already = { team: r.teamName, role: r.role };
        return;
      }
      app.onSyncStatus(await call("get_sync_status", {}));
      await app.reloadAll();
      close();
      toasts.success(`Joined ${r.teamName} · syncing ${plural(r.projects, "project")}`, {
        actionLabel: "Show",
        action: () => document.querySelector<HTMLElement>("[data-testid=project-switcher]")?.click(),
      });
    } catch (e) {
      fail(e);
    } finally {
      busy = false;
    }
  }

  async function switchServer() {
    if (!check) return;
    try {
      app.auth = await call("auth_sign_out", {});
      ticket = await call("invite_ticket", { url: url.trim(), insecureConfirmed: insecureOk }).catch(() => null);
      step = "signin";
    } catch (e) {
      app.reportError(e);
    }
  }
  const title = $derived(step === "preview" && check && !already ? `Join ${check.preview.teamName}?` : step === "signin" ? "Sign in to join" : "Join a team");
</script>

<Dialog {title} width={480} testid="join-team-dialog" onclose={close} onsubmit={step === "paste" ? preview : step === "preview" ? join : undefined} initialFocus="[data-testid=join-team-input]">
  {#if step === "paste"}
    <form
      class="form"
      onsubmit={(e) => {
        e.preventDefault();
        void preview();
      }}
    >
      <label class="field">
        <span>Invite link</span>
        <input class="input mono" bind:value={url} placeholder="https://reqharbor.example.com/i#…" spellcheck="false" autocomplete="off" data-testid="join-team-input" />
      </label>
      <p class="faint small">Paste the link a teammate sent you.</p>
      {#if insecure}
        <div class="hint warn" data-testid="join-insecure">
          <Icon name="alert" /><span>This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.</span>
        </div>
        <label class="checkbox"><input type="checkbox" bind:checked={insecureOk} data-testid="join-insecure-ok" />Continue anyway</label>
      {/if}
      <button type="submit" hidden aria-hidden="true" tabindex="-1"></button>
    </form>
  {:else if step === "signin"}
    <p class="faint small">You'll join {check?.preview.teamName ?? "the team"} once you're signed in to {check ? serverHost(check.serverUrl) : "its server"}.</p>
    <SignInFlow presetServer={check?.serverUrl ?? ""} inviteTicket={ticket} />
  {:else if check}
    {#if check.otherServer}
      <div class="hint warn" data-testid="join-other-server">
        <Icon name="alert" />
        <span>This invite is for {serverHost(check.serverUrl)}. You're signed in to {signedHost}.</span>
      </div>
    {:else if already}
      <div class="hint" data-testid="join-already">You're already in {already.team} as {roleName(already.role)}. Your role hasn't changed.</div>
    {:else}
      <p class="msg" data-testid="join-preview">
        {inviter ? `${inviter} invited you` : "You're invited"} · you'll join as {roleName(check.preview.role)}{expires ? ` · link expires ${expires}` : ""}
      </p>
      <p class="faint small">Team data is visible to all members and stored on {serverHost(check.serverUrl)}.</p>
    {/if}
  {/if}
  {#if error}
    <div class="hint danger" role="alert" data-testid="join-error">
      <Icon name="alert" /><span>{error}</span>
      {#if offline}<button class="link" type="button" onclick={() => void preview()}>Try again</button>{/if}
    </div>
  {/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    {#if step === "paste"}
      <button class="btn btn-primary" type="button" disabled={busy || !url.trim() || (insecure && !insecureOk)} onclick={preview} data-testid="join-team-continue">Continue</button>
    {:else if step === "preview" && check?.otherServer}
      <button class="btn btn-primary" type="button" onclick={switchServer} data-testid="join-switch-server">Sign in to {check ? serverHost(check.serverUrl) : ""}</button>
    {:else if step === "preview" && check}
      <button class="btn btn-primary" type="button" disabled={busy} onclick={join} data-testid="join-team-confirm">{already ? `Open ${already.team}` : `Join ${check.preview.teamName}`}</button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .msg {
    margin: 0 0 var(--sp-3);
  }
  p {
    margin: 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .hint {
    margin-top: var(--sp-4);
  }
</style>

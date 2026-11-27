<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type CreatedInvite, type TeamRole } from "../api";
  import { copyText } from "../api/platform";
  import { inviteSentence } from "../util/teams";
  import { serverHost } from "../util/account";

  /**
   * Invite to a team (UX §7.4, AC-INV-1/2/6), 460 px. Uses comes first because it constrains Role:
   * admin links are single-use, and only owners can make them. The link is shown once.
   */
  let { teamId, teamName, role: myRole }: { teamId: string; teamName: string; role: string } = $props();
  const close = () => (ui.invite = null);
  const USES: { v: string; label: string }[] = [
    { v: "1", label: "1 person" },
    { v: "5", label: "5 people" },
    { v: "25", label: "25 people" },
    { v: "unlimited", label: "Unlimited" },
  ];
  let uses = $state("1");
  let role = $state<TeamRole>("editor");
  let days = $state(7);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let created = $state<CreatedInvite | null>(null);
  let note = $state("");

  const maxUses = $derived(uses === "unlimited" ? null : Number(uses));
  const owner = $derived(myRole === "owner");
  const adminDisabled = $derived(!owner || maxUses !== 1);
  const adminReason = $derived(!owner ? "Only owners can create admin links" : maxUses !== 1 ? "Admin links are single-use." : null);
  const sentence = $derived(inviteSentence(role, maxUses, days, app.now));

  function setUses(v: string) {
    uses = v;
    if (role === "admin" && v !== "1") {
      role = "editor";
      note = "Admin links are single-use, so the role is now Editor.";
    } else note = "";
  }

  async function create() {
    if (busy || created) return;
    busy = true;
    error = null;
    try {
      created = await call("invite_create", { teamId, role, maxUses, expiresInDays: days });
      await copyText(created.url);
      toasts.success("Invite link copied");
    } catch (e) {
      const err = toCommandError(e);
      error = err.code === "limit_invites" ? `${teamName} already has 20 active invite links. Revoke some first.` : err.code === "rate_limited" ? "You've created a lot of invites today. Try again tomorrow." : err.message;
    } finally {
      busy = false;
    }
  }
  const host = $derived(app.auth.account ? serverHost(app.auth.account.serverUrl) : "");
</script>

<Dialog title="Invite to {teamName}" width={460} testid="invite-dialog" onclose={close} onsubmit={created ? close : create}>
  {#if created}
    <div class="result">
      <div class="link-row">
        <input class="input mono" readonly value={created.url} aria-label="Invite link" onfocus={(e) => e.currentTarget.select()} data-testid="invite-link" />
        <button class="btn btn-secondary" type="button" onclick={async () => { await copyText(created!.url); toasts.success("Invite link copied"); }} data-testid="invite-copy">Copy</button>
      </div>
      <div class="hint warn"><Icon name="alert" />Copy it now. For security, the link can't be shown again.</div>
      <p class="faint small">They'll open ReqHarbor › Teams › Join team and paste it{host ? ` (signed in to ${host})` : ""}.</p>
    </div>
  {:else}
    <div class="form">
      <label class="field">
        <span>Uses</span>
        <select class="input" value={uses} onchange={(e) => setUses(e.currentTarget.value)} data-testid="invite-uses">
          {#each USES as u (u.v)}<option value={u.v}>{u.label}</option>{/each}
        </select>
      </label>
      <label class="field">
        <span>Role</span>
        <select class="input" bind:value={role} data-testid="invite-role">
          <option value="viewer">Viewer</option>
          <option value="editor">Editor</option>
          <option value="admin" disabled={adminDisabled} title={adminReason ?? undefined}>Admin</option>
        </select>
      </label>
      {#if adminReason && owner}<p class="faint small sub">{adminReason}</p>{/if}
      <div class="field">
        <span>Expires</span>
        <div class="seg" role="group" aria-label="Expires">
          {#each [1, 7, 30] as d (d)}
            <button type="button" aria-pressed={days === d} onclick={() => (days = d)} data-testid="invite-expires-{d}">{d === 1 ? "1 day" : `${d} days`}</button>
          {/each}
        </div>
      </div>
      <p class="faint small" aria-live="polite" data-testid="invite-sentence">{note ? `${note} ` : ""}{sentence}</p>
      {#if error}<div class="hint danger" role="alert" data-testid="invite-error"><Icon name="alert" />{error}</div>{/if}
    </div>
  {/if}
  {#snippet footer()}
    {#if created}
      <button class="btn btn-primary" type="button" onclick={close} data-testid="invite-done">Done</button>
    {:else}
      <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
      <button class="btn btn-primary" type="button" disabled={busy} onclick={create} data-testid="invite-create">Create link</button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .form,
  .result {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .link-row {
    display: flex;
    gap: var(--sp-3);
  }
  .sub {
    margin: calc(var(--sp-2) * -1) 0 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  p {
    margin: 0;
  }
</style>

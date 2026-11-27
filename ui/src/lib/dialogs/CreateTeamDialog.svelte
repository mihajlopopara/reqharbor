<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError } from "../api";

  /** Create team (UX §7.1), 420 px: a name of 1–80 characters; the owned-team limit is a server refusal. */
  const close = () => (ui.createTeam = false);
  let name = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);
  let limited = $state(false);
  const len = $derived(name.trim().length);

  async function create() {
    if (busy || !len || len > 80 || limited) return;
    busy = true;
    error = null;
    try {
      const team = await call("team_create", { name: name.trim() });
      app.onSyncStatus(await call("get_sync_status", {}));
      close();
      toasts.success(`Created ${team.name}. Next: move a project into ${team.name}, or invite teammates.`, {
        actionLabel: "Invite…",
        action: () => (ui.invite = { teamId: team.teamId, teamName: team.name, role: "owner" }),
      });
      ui.settingsDialog = { section: "teams", teamId: team.teamId };
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "limit_teams") limited = true;
      error = err.message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Create team" width={420} testid="create-team-dialog" onclose={close} onsubmit={create} initialFocus="[data-testid=create-team-name]">
  <form
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      void create();
    }}
  >
    <label class="field">
      <span>Team name</span>
      <input class="input" bind:value={name} maxlength="80" placeholder="Acme" data-testid="create-team-name" />
    </label>
    {#if len > 70}<p class="faint small counter" aria-live="polite">{len}/80</p>{/if}
    {#if error}<div class="hint {limited ? 'warn' : 'danger'}" role="alert" data-testid="create-team-error"><Icon name="alert" />{error}</div>{/if}
    <button type="submit" hidden aria-hidden="true" tabindex="-1"></button>
  </form>
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={busy || !len || len > 80 || limited} title={limited ? (error ?? undefined) : undefined} onclick={create} data-testid="create-team-confirm">Create team</button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .counter {
    margin: calc(var(--sp-2) * -1) 0 0;
    text-align: right;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

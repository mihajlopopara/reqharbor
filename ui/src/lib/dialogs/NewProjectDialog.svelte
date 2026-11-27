<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";

  /**
   * New project, signed in (UX §5.2): a name and a location. Personal is the default (D11);
   * team locations arrive with teams, so bound teams are listed disabled.
   */
  const close = () => (ui.newProject = false);
  const personal = $derived(app.sync.spaces.find((s) => s.kind === "personal" && s.state === "active") ?? null);
  const teams = $derived(app.sync.spaces.filter((s) => s.kind === "team"));
  let name = $state("New project");
  // svelte-ignore state_referenced_locally
  let location = $state<string>(personal?.spaceId ?? "local");
  let busy = $state(false);

  const hint = $derived(location === "local" ? "Never leaves this device." : "Synced to your devices.");

  async function create() {
    if (!name.trim() || busy) return;
    const target = location;
    const issue = personal?.issue;
    busy = true;
    const ok = await app.createProject(name.trim(), target);
    busy = false;
    if (!ok) return;
    close();
    if (target !== "local" && issue && (issue.code === "quota" || issue.code === "serverFull")) toasts.warning(`Saved on this device. ${issue.message}`);
  }
</script>

<Dialog title="New project" width={420} testid="new-project-dialog" onclose={close} onsubmit={create} initialFocus="[data-testid=new-project-name]">
  <form
    class="form"
    onsubmit={(e) => {
      e.preventDefault();
      void create();
    }}
  >
    <label class="field">
      <span>Name</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input" bind:value={name} autofocus onfocus={(e) => e.currentTarget.select()} data-testid="new-project-name" />
    </label>
    <label class="field">
      <span>Location</span>
      <select class="input" bind:value={location} data-testid="new-project-location">
        {#if personal}<option value={personal.spaceId}>Personal</option>{/if}
        {#each teams as t (t.spaceId)}
          <option value={t.spaceId} disabled>{t.name} (team projects arrive with teams)</option>
        {/each}
        <option value="local">This device only</option>
      </select>
    </label>
    <p class="hint-line faint small" data-testid="new-project-hint">{hint}</p>
    <button type="submit" hidden aria-hidden="true" tabindex="-1"></button>
  </form>
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={!name.trim() || busy} onclick={create} data-testid="new-project-create">Create</button>
  {/snippet}
</Dialog>

<style>
  .form {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .hint-line {
    margin: calc(var(--sp-2) * -1) 0 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

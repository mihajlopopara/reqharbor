<script lang="ts">
  import type { Environment } from "../api/types";
  import { app } from "../stores/app.svelte";
  import KvTable from "./KvTable.svelte";
  import Icon from "./Icon.svelte";

  /** Environment editor (AC-ENV-6): inline name, enabled + secret toggles, autosave. */
  let { id }: { id: string } = $props();

  let draft = $state<Environment | null>(null);
  let loadedId = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let nameError = $state(false);

  $effect(() => {
    const e = app.environments.find((x) => x.id === id);
    if (e && loadedId !== id) {
      loadedId = id;
      draft = structuredClone(e);
    }
  });

  const isGlobal = $derived(draft?.projectId === null);
  const active = $derived(app.activeEnv?.id === id);

  function changed() {
    if (!draft) return;
    nameError = !draft.name.trim();
    if (nameError) return;
    if (timer) clearTimeout(timer);
    const snap = $state.snapshot(draft) as Environment;
    timer = setTimeout(() => void app.updateEnvironment(snap), 400);
  }
</script>

<main class="pane ee" aria-label="Environment" data-testid="env-editor">
  {#if draft}
    <div class="row1">
      <span class="kind-label grow"><Icon name={isGlobal ? "globe" : "layers"} small />Environment</span>
      <button class="btn btn-ghost btn-sm" type="button" onclick={() => (app.middle = { kind: "request" })}>Close</button>
    </div>
    <div class="row2">
      {#if isGlobal}
        <span class="title-static grow">Global</span>
      {:else}
        <input
          class="title-input grow"
          class:invalid={nameError}
          bind:value={draft.name}
          oninput={changed}
          aria-label="Environment name"
          title={nameError ? "Name can't be empty" : undefined}
          data-testid="env-name"
        />
        <button class="btn" class:btn-secondary={!active} class:btn-ghost={active} type="button" disabled={active} onclick={() => void app.setActiveEnvironment(id)} data-testid="env-set-active"
          >{active ? "Active" : "Set active"}</button
        >
      {/if}
    </div>
    <div class="tabs" role="tablist" aria-label="Environment"><span class="tab" role="tab" aria-selected="true" tabindex="0">Variables</span></div>
    <div class="body">
      {#if isGlobal}
        <p class="faint note">Global variables apply to every project, after the active project environment.</p>
      {/if}
      <KvTable rows={draft.variables} kind="env" lookup={app.lookup} testid="env-vars" onchange={changed} />
      <p class="faint note"><Icon name="lock" small /> Secret values stay on this device: never synced, shared, or written to history.</p>
    </div>
  {:else}
    <div class="empty"><p>Environment not found.</p></div>
  {/if}
</main>

<style>
  .ee {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg-pane);
    min-width: 0;
  }
  .body {
    flex: 1;
    overflow: auto;
    padding: var(--sp-4) var(--sp-5);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .note {
    margin: 0;
    font-size: var(--fs-sm);
    display: flex;
    gap: var(--sp-2);
    align-items: center;
  }
</style>

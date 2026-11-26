<script lang="ts">
  import type { Collection } from "../api/types";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import AuthForm from "./AuthForm.svelte";
  import Icon from "./Icon.svelte";

  /** Collection settings in the middle pane (AC-COL-5): name, description, default auth. Autosaves (debounced). */
  let { id }: { id: string } = $props();

  let draft = $state<Collection | null>(null);
  let loadedId = "";
  let timer: ReturnType<typeof setTimeout> | null = null;
  let nameError = $state(false);

  $effect(() => {
    const c = app.index?.collectionById.get(id);
    if (c && loadedId !== id) {
      loadedId = id;
      draft = structuredClone(c);
    }
  });

  function changed() {
    if (!draft) return;
    nameError = !draft.name.trim();
    if (nameError) return;
    if (timer) clearTimeout(timer);
    const snap = $state.snapshot(draft) as Collection;
    timer = setTimeout(() => void app.updateCollection(snap), 400);
  }
</script>

<main class="pane cs" aria-label="Collection settings" data-testid="collection-settings">
  {#if draft}
    <div class="row1">
      <span class="kind-label grow"><Icon name="stack" small />Collection</span>
      <button class="btn btn-ghost btn-sm" type="button" onclick={() => (app.middle = { kind: "request" })}>Close</button>
    </div>
    <div class="row2">
      <input
        class="title-input grow"
        class:invalid={nameError}
        bind:value={draft.name}
        oninput={changed}
        aria-label="Collection name"
        title={nameError ? "Name can't be empty" : undefined}
        data-testid="collection-name"
      />
      <button class="btn btn-secondary" type="button" onclick={() => draft && (ui.shareDialog = { kind: "collection", entityId: draft.id, name: draft.name })}>Share…</button>
    </div>
    <div class="tabs" role="tablist" aria-label="Collection"><span class="tab" role="tab" aria-selected="true" tabindex="0">Settings</span></div>
    <div class="body">
      {#if nameError}<span class="field-error">Name can't be empty</span>{/if}
      <label class="field">
        <span>Description</span>
        <textarea class="input desc" rows="3" bind:value={draft.description} oninput={changed} data-testid="collection-description"></textarea>
      </label>
      <h4>Default auth</h4>
      <p class="faint note">Requests whose auth is set to <b>Inherit</b> use this.</p>
      <AuthForm auth={draft.auth} lookup={app.lookup} allowInherit={false} onchange={changed} />
    </div>
  {:else}
    <div class="empty"><p>Collection not found.</p></div>
  {/if}
</main>

<style>
  .cs {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg-pane);
    min-width: 0;
  }
  .body {
    flex: 1;
    overflow: auto;
    padding: var(--sp-6) var(--sp-5);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .desc {
    font-family: var(--font-ui);
    font-size: var(--fs-md);
  }
  h4 {
    margin: var(--sp-4) 0 0;
    font-size: var(--fs-md);
    font-weight: var(--fw-semibold);
  }
  .note {
    margin: calc(var(--sp-4) * -1) 0 0;
    font-size: var(--fs-sm);
  }
</style>

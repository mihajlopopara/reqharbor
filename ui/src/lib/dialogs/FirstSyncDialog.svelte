<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, type FirstSyncCandidate } from "../api";
  import { plural } from "../util/spaces";

  /**
   * First sign-in: what to sync (UX §10, AC-ONB-5). Nothing uploads before the answer; × and
   * "Keep all on this device" both keep everything local.
   */
  let candidates = $state<FirstSyncCandidate[] | null>(null);
  let checked = $state<Set<string>>(new Set());
  let busy = $state(false);

  onMount(async () => {
    try {
      candidates = await call("first_sync_candidates", {});
      checked = new Set(candidates.map((c) => c.projectId));
    } catch (e) {
      app.reportError(e);
      ui.firstSync = false;
    }
  });

  const all = $derived(!!candidates && candidates.length > 0 && checked.size === candidates.length);
  const hasLegacy = $derived(!!app.sync.legacy);

  function toggle(id: string, on: boolean) {
    const next = new Set(checked);
    if (on) next.add(id);
    else next.delete(id);
    checked = next;
  }

  async function answer(ids: string[]) {
    if (busy) return;
    busy = true;
    try {
      app.onSyncStatus(await call("first_sync_apply", { projectIds: ids }));
      await app.reloadAll();
      ui.firstSync = false;
      if (ids.length) toasts.success(`Syncing ${plural(ids.length, "project")} to your account`);
      else toasts.info("Your projects stay on this device. Change any project's sync location from the project switcher.");
    } catch (e) {
      app.reportError(e);
    } finally {
      busy = false;
    }
  }

  const keepAll = () => void answer([]);
</script>

<Dialog title="Sync these projects to your account?" width={520} testid="first-sync-dialog" onclose={keepAll} onsubmit={() => void answer([...checked])}>
  <p class="msg">Checked projects sync to your devices. Unchecked ones stay on this device only. You can change this later per project.</p>
  {#if candidates === null}
    <p class="faint">Loading…</p>
  {:else}
    <label class="checkbox all">
      <input type="checkbox" checked={all} indeterminate={checked.size > 0 && !all} onchange={(e) => (checked = e.currentTarget.checked ? new Set(candidates?.map((c) => c.projectId)) : new Set())} data-testid="first-sync-all" />
      Select all
    </label>
    <ul class="list" aria-label="Projects">
      {#each candidates as c (c.projectId)}
        <li>
          <label class="checkbox row">
            <input type="checkbox" checked={checked.has(c.projectId)} onchange={(e) => toggle(c.projectId, e.currentTarget.checked)} data-testid="first-sync-project" data-name={c.name} />
            <span class="name">{c.name}</span>
            <span class="faint small counts">{plural(c.collections, "collection")} · {plural(c.requests, "request")}</span>
            {#if !checked.has(c.projectId)}<span class="faint small local">This device only</span>{/if}
          </label>
        </li>
      {/each}
    </ul>
    <p class="faint small" data-testid="first-sync-global">
      {checked.size > 0 ? "Your Global environment syncs with them." : "Your Global environment stays on this device too."}
    </p>
    {#if hasLegacy}
      <p class="faint small">Projects synced with a token are handled under Sync › Merge.</p>
    {/if}
  {/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" disabled={busy} onclick={keepAll} data-testid="first-sync-keep">Keep all on this device</button>
    <button class="btn btn-primary" type="button" disabled={busy || checked.size === 0} onclick={() => void answer([...checked])} data-testid="first-sync-apply">Sync {plural(checked.size, "project")}</button>
  {/snippet}
</Dialog>

<style>
  .msg {
    margin: 0 0 var(--sp-4);
    color: var(--text-2);
  }
  .list {
    list-style: none;
    margin: var(--sp-2) 0 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .name {
    font-weight: var(--fw-medium);
  }
  .counts {
    font-variant-numeric: tabular-nums;
  }
  .local {
    margin-left: auto;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError } from "../api";
  import { plural } from "../util/spaces";

  /** Merge into your personal space (UX §11, AC-ONB-3): same ids, safe to repeat. */
  const close = () => (ui.mergeLegacy = false);
  const n = $derived(app.sync.legacy?.projectCount ?? 0);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function merge() {
    if (busy) return;
    busy = true;
    error = null;
    try {
      // The merge is done once this returns; a failed follow-up sync is not a failed merge.
      const res = await call("legacy_merge", {});
      app.onSyncStatus(await call("get_sync_status", {}));
      await app.reloadAll();
      close();
      const pending = app.sync.spaces.find((s) => s.kind === "personal")?.pendingChanges ?? 0;
      if (res.syncError) toasts.warning(`Merged. Syncing didn't finish (${res.syncError.message}); it retries automatically.`);
      else if (pending > 0) toasts.warning(`Merged. ${plural(pending, "change")} will sync when the server can take ${pending === 1 ? "it" : "them"}.`);
      else toasts.success("Merged into your personal space.");
    } catch (e) {
      error = toCommandError(e).message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Merge into your personal space?" width={480} testid="merge-legacy-dialog" onclose={close} onsubmit={merge}>
  <p class="msg">
    Your {plural(n, "token-synced project")} and your Global environment move into your personal space. Nothing is deleted on the old workspace, and its share links keep working.
  </p>
  <p class="msg">Other devices using this token keep syncing to the old workspace until you sign them in and merge there too.</p>
  {#if busy}<p class="faint" data-testid="merge-progress">Merging…</p>{/if}
  {#if error}
    <div class="hint danger" role="alert" data-testid="merge-error">{error} Merging again is safe.</div>
  {/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={busy} onclick={merge} data-testid="merge-legacy-confirm">{error ? "Merge again" : "Merge"}</button>
  {/snippet}
</Dialog>

<style>
  .msg {
    margin: 0 0 var(--sp-4);
    color: var(--text-2);
  }
</style>

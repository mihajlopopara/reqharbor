<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { ui } from "../stores/ui.svelte";

  const s = $derived(ui.confirmState);
  function done(ok: boolean) {
    const st = ui.confirmState;
    ui.confirmState = null;
    st?.resolve(ok);
  }
</script>

{#if s}
  <Dialog title={s.title} width={420} testid="confirm-dialog" onclose={() => done(false)} initialFocus="[data-testid=confirm-cancel]">
    <p class="msg">{s.body}</p>
    {#snippet footer()}
      <button class="btn btn-secondary" type="button" onclick={() => done(false)} data-testid="confirm-cancel">Cancel</button>
      <button class="btn {s.danger ? 'btn-danger-solid' : 'btn-primary'}" type="button" onclick={() => done(true)} data-testid="confirm-ok">{s.confirmLabel}</button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .msg {
    margin: 0;
    color: var(--text-2);
  }
</style>

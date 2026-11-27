<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { ui } from "../stores/ui.svelte";

  /** Confirm, with optional detail lines, one checkbox and type-to-confirm (UX §4.4, §7.2, §7.3). */
  const s = $derived(ui.confirmState);
  let typed = $state("");
  let checked = $state(false);
  let lastState: unknown = null;
  $effect(() => {
    const st = ui.confirmState;
    if (st && st !== lastState) {
      lastState = st;
      typed = "";
      checked = st.checkbox?.checked ?? false;
    }
  });
  const blocked = $derived(!!s?.typeToConfirm && typed.trim() !== s.typeToConfirm.trim());

  function done(ok: boolean) {
    const st = ui.confirmState;
    if (ok && blocked) return;
    ui.confirmState = null;
    st?.resolve({ ok, checked: st.checkbox ? checked : undefined });
  }
</script>

{#if s}
  <Dialog title={s.title} width={420} testid="confirm-dialog" onclose={() => done(false)} onsubmit={() => done(true)} initialFocus="[data-testid=confirm-cancel]">
    <p class="msg">{s.body}</p>
    {#if s.details?.length}
      <ul class="details" data-testid="confirm-details">
        {#each s.details as d (d)}<li>{d}</li>{/each}
      </ul>
    {/if}
    {#if s.checkbox}
      <label class="checkbox" data-testid={s.checkbox.testid ?? "confirm-checkbox"}>
        <input type="checkbox" bind:checked />
        {s.checkbox.label}
      </label>
    {/if}
    {#if s.typeToConfirm}
      <label class="field type">
        <span>Type <b class="mono">{s.typeToConfirm}</b> to confirm</span>
        <input class="input" bind:value={typed} autocomplete="off" spellcheck="false" data-testid="confirm-type" />
      </label>
    {/if}
    {#snippet footer()}
      <button class="btn btn-secondary" type="button" onclick={() => done(false)} data-testid="confirm-cancel">Cancel</button>
      <button class="btn {s.danger ? 'btn-danger-solid' : 'btn-primary'}" type="button" disabled={blocked} onclick={() => done(true)} data-testid="confirm-ok">{s.confirmLabel}</button>
    {/snippet}
  </Dialog>
{/if}

<style>
  .msg {
    margin: 0;
    color: var(--text-2);
  }
  .details {
    margin: var(--sp-4) 0 0;
    padding-left: var(--sp-6);
    color: var(--text-2);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .checkbox {
    margin-top: var(--sp-5);
  }
  .type {
    margin-top: var(--sp-5);
  }
</style>

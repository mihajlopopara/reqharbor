<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type TeamDetail } from "../api";
  import { formatSize } from "../util/format";

  /** Transfer ownership (UX §7.2), 420 px: they become owner, I become an admin. */
  let { teamId }: { teamId: string } = $props();
  const close = () => (ui.transferOwnership = null);
  let detail = $state<TeamDetail | null>(null);
  let target = $state("");
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    void call("team_get", { teamId }).then(
      (d) => {
        detail = d;
        target = d.members.find((m) => m.userId !== d.me)?.userId ?? "";
      },
      (e) => (error = toCommandError(e).message),
    );
  });
  const others = $derived(detail ? detail.members.filter((m) => m.userId !== detail!.me) : []);
  const chosen = $derived(others.find((m) => m.userId === target) ?? null);
  const first = $derived(chosen ? chosen.name.split(" ")[0] || chosen.email : "They");
  const size = $derived(detail?.team.bytesUsed != null ? `${formatSize(detail.team.bytesUsed)} ` : "");

  async function transfer() {
    if (busy || !chosen || !detail) return;
    busy = true;
    error = null;
    try {
      await call("team_transfer", { teamId, userId: chosen.userId });
      close();
      toasts.success(`${chosen.name || chosen.email} now owns ${detail.team.name}. You're an admin.`);
    } catch (e) {
      const err = toCommandError(e);
      error = err.code === "quota_exceeded" && !/room/.test(err.message) ? `${first} doesn't have room for ${detail.team.name}.` : err.message;
    } finally {
      busy = false;
    }
  }
</script>

<Dialog title="Transfer ownership" width={420} testid="transfer-dialog" onclose={close} onsubmit={transfer}>
  {#if detail}
    <label class="field">
      <span>New owner</span>
      <select class="input" bind:value={target} data-testid="transfer-target">
        {#each others as m (m.userId)}<option value={m.userId}>{m.name || m.email}</option>{/each}
      </select>
    </label>
    <p class="msg">{first} become{first === "They" ? "" : "s"} owner. You'll become an admin. {detail.team.name}'s {size}will count against their storage.</p>
  {:else if !error}
    <p class="faint">Loading…</p>
  {/if}
  {#if error}<div class="hint danger" role="alert" data-testid="transfer-error"><Icon name="alert" />{error}</div>{/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={busy || !chosen} onclick={transfer} data-testid="transfer-confirm">Transfer</button>
  {/snippet}
</Dialog>

<style>
  .msg {
    margin: var(--sp-4) 0 0;
    color: var(--text-2);
  }
  .hint {
    margin-top: var(--sp-4);
  }
</style>

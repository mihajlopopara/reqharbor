<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call } from "../api";
  import { plural } from "../util/spaces";

  /**
   * SpaceNoticeDialog (UX §4.3 "different account", §6.9 removal; 06 §9). Keep (local-only
   * copies) is always the focused default [DEV-6]; Remove is a deliberate choice. × means
   * "decide later": Settings › Sync keeps a "Decide" link, and Keep applies after 7 days.
   */
  let { mode, spaceIds }: { mode: "foreign" | "removed"; spaceIds: string[] } = $props();
  const close = () => (ui.spaceNotice = null);
  /** × = decide later: Settings › Sync keeps "Decide"; after 7 days Keep applies by itself. */
  async function later() {
    const ids = [...spaceIds];
    close();
    try {
      app.onSyncStatus(await call("space_notice_later", { spaceIds: ids }));
    } catch (e) {
      app.reportError(e);
    }
  }
  const spaces = $derived(app.sync.spaces.filter((s) => spaceIds.includes(s.spaceId)));
  const projectsOf = (id: string) => app.projects.filter((p) => app.locationById.get(p.id)?.spaceId === id);
  // svelte-ignore state_referenced_locally
  let choice = $state<Record<string, "keep" | "remove">>(Object.fromEntries(spaceIds.map((id) => [id, "keep"])));
  let busy = $state(false);

  const who = $derived(app.auth.account?.email ?? "");
  const by = $derived(spaces.find((s) => s.accountEmail)?.accountEmail ?? "another account");
  const removing = $derived(spaces.filter((s) => choice[s.spaceId] === "remove"));
  const removeCount = $derived(removing.reduce((n, s) => n + projectsOf(s.spaceId).length, 0));

  async function resolve(all: Record<string, "keep" | "remove">) {
    if (busy) return;
    const n = Object.entries(all)
      .filter(([, c]) => c === "remove")
      .reduce((acc, [id]) => acc + projectsOf(id).length, 0);
    // Edge case 3: unsynced edits in a space being removed are offered as a personal copy first.
    const unsynced = Object.entries(all)
      .filter(([, c]) => c === "remove")
      .reduce((acc, [id]) => acc + (spaces.find((s) => s.spaceId === id)?.pendingChanges ?? 0), 0);
    let save = false;
    if (n > 0) {
      const r = await ui.confirmEx({
        title: `Remove ${plural(n, "project")} from this device?`,
        body: "They are deleted here only. Nothing is deleted on the server.",
        confirmLabel: "Remove",
        checkbox: unsynced ? { label: `Save ${plural(unsynced, "change")} as a personal copy`, checked: true, testid: "space-notice-save-changes" } : undefined,
      });
      if (!r.ok) return;
      save = !!r.checked;
    }
    busy = true;
    try {
      for (const [spaceId, c] of Object.entries(all)) {
        app.onSyncStatus(await call("space_notice_resolve", { spaceId, keep: c === "keep", saveChanges: c === "remove" && save }));
      }
      await app.reloadAll();
      close();
      toasts.info(n > 0 ? `Removed ${plural(n, "project")} from this device` : "Kept on this device only");
    } catch (e) {
      app.reportError(e);
    } finally {
      busy = false;
    }
  }

  async function cancelSignIn() {
    close();
    try {
      app.auth = await call("auth_sign_out", {});
      toasts.info("Signed out on this device");
    } catch (e) {
      app.reportError(e);
    }
  }

  const title = $derived(
    mode === "foreign"
      ? `You signed in as ${who}`
      : spaces.length === 1
        ? spaces[0].reason === "deleted"
          ? `${spaces[0].name} was deleted`
          : spaces[0].reason === "left"
            ? `You left ${spaces[0].name}`
            : `You were removed from ${spaces[0].name}`
        : "Spaces you no longer have",
  );
</script>

<Dialog {title} width={mode === "foreign" ? 480 : 440} testid="space-notice-dialog" onclose={later} initialFocus="[data-testid=space-notice-keep], [data-testid=space-notice-continue]">
  {#if mode === "foreign"}
    <p class="msg">These projects were synced by {by}. Choose what happens on this device.</p>
    <div class="rows">
      {#each spaces as sp (sp.spaceId)}
        <label class="row" data-testid="space-notice-row">
          <span class="name">{sp.name} ({plural(projectsOf(sp.spaceId).length, "project")})</span>
          <select class="input" bind:value={choice[sp.spaceId]} data-testid="space-notice-choice">
            <option value="keep">Keep as local-only copy</option>
            <option value="remove">Remove from this device</option>
          </select>
        </label>
      {/each}
    </div>
  {:else}
    {#each spaces as sp (sp.spaceId)}
      <p class="msg">{sp.name}'s projects no longer sync to this device.</p>
      {#if projectsOf(sp.spaceId).length}
        <p class="msg">{plural(projectsOf(sp.spaceId).length, "project is", "projects are")} still here: {projectsOf(sp.spaceId).map((p) => p.name).join(", ")}.</p>
      {/if}
      {#if sp.pendingChanges}<p class="msg">{plural(sp.pendingChanges, "change")} hadn't synced yet.</p>{/if}
    {/each}
  {/if}
  {#snippet footer()}
    {#if mode === "foreign"}
      <button class="btn btn-secondary" type="button" disabled={busy} onclick={cancelSignIn} data-testid="space-notice-cancel-signin">Cancel sign-in</button>
      <button class="btn {removeCount ? 'btn-danger-solid' : 'btn-primary'}" type="button" disabled={busy} onclick={() => void resolve(choice)} data-testid="space-notice-continue">Continue</button>
    {:else}
      <button class="btn btn-secondary" type="button" disabled={busy} onclick={() => void resolve(Object.fromEntries(spaceIds.map((id) => [id, "remove"])))} data-testid="space-notice-remove">Remove from this device</button>
      <button class="btn btn-primary" type="button" disabled={busy} onclick={() => void resolve(Object.fromEntries(spaceIds.map((id) => [id, "keep"])))} data-testid="space-notice-keep">Keep a personal copy</button>
    {/if}
  {/snippet}
</Dialog>

<style>
  .msg {
    margin: 0 0 var(--sp-4);
    color: var(--text-2);
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    justify-content: space-between;
  }
  .row .input {
    width: auto;
  }
  .name {
    font-weight: var(--fw-medium);
  }
</style>

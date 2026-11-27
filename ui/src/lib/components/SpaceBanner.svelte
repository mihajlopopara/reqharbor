<script lang="ts">
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import Icon from "./Icon.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call } from "../api";
  import { plural } from "../util/teams";

  /**
   * Above the Collections list (UX §6.2, §6.8): the viewer line, and the role-changed banner that
   * stays until Save as personal copy or Discard is chosen. The same banner holds changes kept under
   * an item deleted on the server.
   */
  const space = $derived(app.activeSpace);
  const loc = $derived(app.activeProjectId ? app.locationById.get(app.activeProjectId) : undefined);
  const banner = $derived(!!space && (space.frozen > 0 || space.issue?.code === "roleChanged" || space.issue?.code === "heldDelete"));
  const text = $derived(space?.issue && (space.issue.code === "roleChanged" || space.issue.code === "heldDelete") ? space.issue.message : `You have ${plural(space?.frozen ?? 0, "unsynced change")}.`);
  let busy = $state(false);

  async function save() {
    if (!space || busy) return;
    busy = true;
    await app.resolveRoleChange(space.spaceId, "savePersonal");
    busy = false;
  }

  /** Duplicate to personal…: the whole active project, new ids, then switch to it. */
  async function openDuplicate() {
    const p = app.project;
    if (!p) return;
    const personal = app.sync.spaces.find((s) => s.kind === "personal" && s.state === "active");
    const id = await app.duplicateProjectTo(p.id, personal?.spaceId ?? null, `${p.name} copy`);
    if (!id) return;
    await app.switchProject(id);
    void call("get_sync_status", {}).then((s) => app.onSyncStatus(s));
    toasts.success(`Duplicated to ${personal ? "Personal" : "this device"} as “${p.name} copy”`);
  }

  async function discard() {
    if (!space || busy) return;
    const n = space.frozen;
    const ok = await ui.confirm({ title: n ? `Discard ${plural(n, "unsynced change")}?` : "Discard?", body: `The server's versions come back from ${space.name}. This can't be undone.`, confirmLabel: "Discard" });
    if (!ok) return;
    busy = true;
    await app.resolveRoleChange(space.spaceId, "discard");
    busy = false;
  }
</script>

{#if banner && space}
  <div class="hint warn banner" role="status" data-testid="role-banner">
    <Icon name="alert" />
    <div class="body">
      <span>{text}</span>
      <div class="acts">
        <button class="btn btn-secondary btn-sm" type="button" disabled={busy} onclick={save} data-testid="role-banner-save">Save as personal copy</button>
        <button class="btn btn-ghost btn-sm" type="button" disabled={busy} onclick={discard} data-testid="role-banner-discard">Discard</button>
      </div>
    </div>
  </div>
{/if}
{#if app.viewerHere && loc}
  <div class="viewer" data-testid="viewer-line">
    <Icon name="lock" small />
    <span>View only in {loc.label} ·</span>
    <button class="link" type="button" onclick={() => void openDuplicate()} data-testid="viewer-duplicate">Duplicate to personal</button>
    <span>to make changes</span>
  </div>
{/if}


<style>
  .banner {
    margin: 0 var(--sp-4) var(--sp-3) var(--sp-5);
    align-items: flex-start;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .acts {
    display: flex;
    gap: var(--sp-3);
  }
  .viewer {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    height: 24px;
    padding: 0 var(--sp-4) 0 var(--sp-5);
    color: var(--text-3);
    font-size: 11.5px;
    flex: none;
  }
  .viewer .link {
    font-size: 11.5px;
  }
</style>

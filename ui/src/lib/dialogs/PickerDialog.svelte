<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui, type PickerMode } from "../stores/ui.svelte";
  import { childFolders, isInside } from "../util/tree";
  import { call } from "../api";

  /** Save-to / Move-to picker: collection + folder tree and (for saves) a name field (AC-REQ-11). */
  let { mode }: { mode: PickerMode } = $props();

  type Target = { collectionId: string; folderId: string | null };
  const ix = $derived(app.index);
  let target = $state<Target | null>(null);
  let name = $state("");
  let historyName = $state<string | null>(null);

  const isSave = $derived(mode.kind === "saveRequest" || mode.kind === "saveHistory");
  const title = $derived(isSave ? "Save request" : mode.kind === "moveFolder" ? "Move folder" : "Move request");

  onMount(() => {
    if (mode.kind === "saveRequest") name = app.request.name === "Untitled request" ? "" : app.request.name;
    else if (mode.kind === "saveHistory" && historyName === null) {
      historyName = "";
      void call("open_history_entry", { historyId: mode.historyId }).then(({ entry }) => {
        name = entry.request.name && entry.request.name !== "Untitled request" ? entry.request.name : `${entry.method} ${entry.resolvedUrl.replace(/^[a-z]+:\/\/[^/]+/i, "") || "/"}`;
        historyName = name;
      });
    }
    if (!target && ix?.collections[0]) target = { collectionId: ix.collections[0].id, folderId: null };
  });

  type Row = { collectionId: string; folderId: string | null; name: string; depth: number; disabled: boolean };
  const rows = $derived.by(() => {
    const out: Row[] = [];
    if (!ix) return out;
    const movingFolder = mode.kind === "moveFolder" ? mode.folderId : null;
    const walk = (cid: string, parent: string | null, depth: number) => {
      for (const f of childFolders(ix, cid, parent)) {
        const disabled = !!movingFolder && (f.id === movingFolder || isInside(ix, f.id, movingFolder));
        out.push({ collectionId: cid, folderId: f.id, name: f.name, depth, disabled });
        walk(cid, f.id, depth + 1);
      }
    };
    for (const c of ix.collections) {
      out.push({ collectionId: c.id, folderId: null, name: c.name, depth: 0, disabled: false });
      walk(c.id, null, 1);
    }
    return out;
  });

  const valid = $derived(!!target && (!isSave || name.trim().length > 0));

  async function submit() {
    if (!valid || !target) return;
    const t = target;
    const m = mode;
    const n0 = name.trim();
    ui.picker = null;
    if (m.kind === "saveRequest") await app.saveScratchTo(t.collectionId, t.folderId, n0);
    else if (m.kind === "saveHistory") await app.saveHistoryTo(m.historyId, t.collectionId, t.folderId, n0);
    else {
      const kind = m.kind === "moveFolder" ? "folder" : "request";
      const id = m.kind === "moveFolder" ? m.folderId : m.requestId;
      const n = kind === "folder" ? childFolders(ix!, t.collectionId, t.folderId).length : (ix?.requestsByParent.get(`${t.collectionId}/${t.folderId ?? ""}`)?.length ?? 0);
      await app.moveNode(kind, id, t.collectionId, t.folderId, n);
    }
  }

  async function newCollection() {
    const c = await app.createCollection("New collection");
    ui.renaming = null;
    if (c) target = { collectionId: c.id, folderId: null };
  }
</script>

<Dialog {title} width={460} testid="picker-dialog" onclose={() => (ui.picker = null)} onsubmit={submit}>
  {#if isSave}
    <label class="field">
      <span>Name</span>
      <input class="input" bind:value={name} placeholder="Request name" data-testid="picker-name" onkeydown={(e) => e.key === "Enter" && submit()} />
    </label>
  {/if}
  <div class="label">{isSave ? "Save to" : "Move to"}</div>
  {#if rows.length === 0}
    <div class="empty">
      <p>No collections yet.</p>
      <button class="btn btn-secondary btn-sm" type="button" onclick={newCollection}>New collection</button>
    </div>
  {:else}
    <div class="tree" role="listbox" aria-label="Destination" data-testid="picker-tree">
      {#each rows as r (r.collectionId + "/" + (r.folderId ?? ""))}
        {@const sel = target?.collectionId === r.collectionId && target?.folderId === r.folderId}
        <button
          type="button"
          class="prow"
          role="option"
          aria-selected={sel}
          disabled={r.disabled}
          style:padding-left="{8 + r.depth * 14}px"
          data-testid="picker-node"
          data-name={r.name}
          onclick={() => (target = { collectionId: r.collectionId, folderId: r.folderId })}
          ondblclick={submit}
        >
          <span class="faint"><Icon name={r.folderId ? "folder" : "stack"} small /></span>{r.name}
        </button>
      {/each}
    </div>
  {/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={() => (ui.picker = null)}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={!valid} onclick={submit} data-testid="picker-submit">{isSave ? "Save" : "Move"}</button>
  {/snippet}
</Dialog>

<style>
  .tree {
    max-height: 280px;
    overflow: auto;
    background: var(--bg-input);
    border-radius: var(--r-md);
    padding: var(--sp-2);
  }
  .prow {
    width: 100%;
    height: 26px;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    border: 0;
    background: none;
    border-radius: var(--r-sm);
    text-align: left;
    color: var(--text-1);
  }
  .prow span {
    display: inline-flex;
  }
  .prow:hover:not(:disabled) {
    background: var(--bg-hover);
  }
  .prow[aria-selected="true"] {
    background: var(--bg-selected);
  }
  .prow:disabled {
    color: var(--text-disabled);
    cursor: not-allowed;
  }
</style>

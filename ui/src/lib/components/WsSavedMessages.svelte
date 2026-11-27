<script lang="ts">
  import type { SavedMessage } from "../api/types";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { ws } from "../stores/ws.svelte";
  import { isSocketIo, messageTitle } from "../util/ws";
  import Icon from "./Icon.svelte";

  /** Saved messages of a WebSocket request (AC-WS-4): load into the composer, send, rename, reorder, delete. */
  let { onload }: { onload: () => void } = $props();

  let renaming = $state<number | null>(null);
  let nameDraft = $state("");

  const r = $derived(app.request);
  const sio = $derived(isSocketIo(r));
  const messages = $derived(r.ws?.messages ?? []);
  const open = $derived(ws.session(r.id)?.state === "open");

  const KIND_LABEL: Record<string, string> = { text: "Text", json: "JSON", binary: "Binary" };

  function load(m: SavedMessage) {
    ws.setDraft(r.id, { ...$state.snapshot(m), event: m.event ?? (sio ? "message" : undefined) } as SavedMessage);
    onload();
  }

  async function send(m: SavedMessage) {
    await app.wsSend($state.snapshot(m) as SavedMessage);
  }

  function remove(i: number) {
    app.request.ws?.messages.splice(i, 1);
  }

  function move(i: number, d: number) {
    const list = app.request.ws?.messages;
    if (!list) return;
    const j = i + d;
    if (j < 0 || j >= list.length) return;
    const [m] = list.splice(i, 1);
    list.splice(j, 0, m);
  }

  function commitRename() {
    if (renaming === null) return;
    const m = app.request.ws?.messages[renaming];
    if (m) m.name = nameDraft.trim();
    renaming = null;
  }

  function menu(e: MouseEvent, i: number) {
    ui.openMenu(
      [
        {
          label: "Rename",
          disabled: app.readOnly,
          action: () => {
            nameDraft = messages[i].name;
            renaming = i;
          },
        },
        { label: "Duplicate", disabled: app.readOnly, action: () => app.request.ws?.messages.splice(i + 1, 0, structuredClone($state.snapshot(messages[i]) as SavedMessage)) },
        { label: "Move up", disabled: app.readOnly || i === 0, action: () => move(i, -1) },
        { label: "Move down", disabled: app.readOnly || i === messages.length - 1, action: () => move(i, 1) },
        { separator: true },
        { label: "Delete", danger: true, disabled: app.readOnly, action: () => remove(i), testid: "ws-saved-delete" },
      ],
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
  }
</script>

<div class="saved" data-testid="ws-saved">
  {#if messages.length === 0}
    <div class="empty" data-testid="ws-saved-empty">
      <h3>No saved messages</h3>
      <p>Compose a message and choose <b>Save message</b> to keep it with this request.</p>
    </div>
  {:else}
    <ul class="list" aria-label="Saved messages">
      {#each messages as m, i (i)}
        <li class="item" data-testid="ws-saved-row">
          <div class="main">
            {#if renaming === i}
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="input rename"
                bind:value={nameDraft}
                aria-label="Message name"
                autofocus
                onkeydown={(e) => {
                  if (e.key === "Enter") commitRename();
                  else if (e.key === "Escape") {
                    e.stopPropagation();
                    renaming = null;
                  }
                }}
                onblur={commitRename}
              />
            {:else}
              <button class="name" type="button" title="Load into the Message tab" onclick={() => load(m)} data-testid="ws-saved-load">{messageTitle(m)}</button>
            {/if}
            <span class="meta">
              {#if m.event}<span class="m m-ws">{m.event}</span>{/if}
              <span class="chip">{KIND_LABEL[m.kind] ?? m.kind}</span>
              {#if m.ack}<span class="chip">ack</span>{/if}
              <span class="mono faint preview">{m.body.replace(/\s+/g, " ").slice(0, 120)}</span>
            </span>
          </div>
          <button class="btn btn-secondary btn-sm" type="button" disabled={!open} onclick={() => void send(m)} title={open ? "Send now" : "Connect to send"} data-testid="ws-saved-send">{sio ? "Emit" : "Send"}</button>
          <button class="icon-btn" type="button" aria-label="Message actions" aria-haspopup="menu" onclick={(e) => menu(e, i)} data-testid="ws-saved-menu"><Icon name="more" /></button>
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .saved {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: var(--sp-3) var(--sp-5);
  }
  .item {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-3) var(--sp-2);
    border-top: 1px solid var(--border-subtle);
  }
  .item:first-child {
    border-top: 0;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .name {
    border: 0;
    background: none;
    padding: 0;
    text-align: left;
    color: var(--text-1);
    font-weight: var(--fw-medium);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name:hover {
    color: var(--accent-fg);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    font-size: var(--fs-sm);
  }
  .chip {
    color: var(--text-3);
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }
  .preview {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--fs-sm);
  }
  .rename {
    height: 24px;
    max-width: 260px;
  }
</style>

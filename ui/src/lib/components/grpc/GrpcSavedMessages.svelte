<script lang="ts">
  import type { GrpcSavedMessage } from "../../api/types";
  import { app } from "../../stores/app.svelte";
  import { ui } from "../../stores/ui.svelte";
  import { ws } from "../../stores/ws.svelte";
  import { sendsMessages } from "../../util/grpc";
  import Icon from "../Icon.svelte";

  /** Saved messages of a gRPC request (AC-GRPC-6): load, send on an open stream, rename, reorder, delete. */
  let { onload }: { onload: () => void } = $props();

  let renaming = $state<number | null>(null);
  let nameDraft = $state("");

  const r = $derived(app.request);
  const messages = $derived(r.grpc?.messages ?? []);
  const session = $derived(ws.session(r.id));
  const open = $derived(session?.transport === "grpc" && session.state === "open" && sendsMessages(session.callKind) && !session.halfClosed);

  function title(m: GrpcSavedMessage): string {
    if (m.name.trim()) return m.name.trim();
    const t = m.body.replace(/\s+/g, " ").trim();
    return t ? (t.length > 32 ? t.slice(0, 31) + "…" : t) : "Untitled message";
  }

  function list() {
    return app.request.grpc?.messages;
  }

  function load(m: GrpcSavedMessage) {
    if (app.request.grpc) app.request.grpc.message = m.body;
    onload();
  }

  function move(i: number, d: number) {
    const l = list();
    if (!l) return;
    const j = i + d;
    if (j < 0 || j >= l.length) return;
    const [m] = l.splice(i, 1);
    l.splice(j, 0, m);
  }

  function commitRename() {
    if (renaming === null) return;
    const m = list()?.[renaming];
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
        { label: "Duplicate", disabled: app.readOnly, action: () => list()?.splice(i + 1, 0, structuredClone($state.snapshot(messages[i]) as GrpcSavedMessage)) },
        { label: "Move up", disabled: app.readOnly || i === 0, action: () => move(i, -1) },
        { label: "Move down", disabled: app.readOnly || i === messages.length - 1, action: () => move(i, 1) },
        { separator: true },
        { label: "Delete", danger: true, disabled: app.readOnly, action: () => list()?.splice(i, 1), testid: "grpc-saved-delete" },
      ],
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
  }
</script>

<div class="saved" data-testid="grpc-saved">
  {#if messages.length === 0}
    <div class="empty" data-testid="grpc-saved-empty">
      <h3>No saved messages</h3>
      <p>Compose a message and choose <b>Save message</b> to keep it with this request.</p>
    </div>
  {:else}
    <ul class="list" aria-label="Saved messages">
      {#each messages as m, i (i)}
        <li class="item" data-testid="grpc-saved-row">
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
              <button class="name" type="button" title="Load into the Message tab" onclick={() => load(m)} data-testid="grpc-saved-load">{title(m)}</button>
            {/if}
            <span class="mono faint preview">{m.body.replace(/\s+/g, " ").slice(0, 120)}</span>
          </div>
          <button class="btn btn-secondary btn-sm" type="button" disabled={!open} onclick={() => void app.grpcSend(m.body)} title={open ? "Send on the open stream" : "Connect a client or bidi stream to send"} data-testid="grpc-saved-send">Send</button>
          <button class="icon-btn" type="button" aria-label="Message actions" aria-haspopup="menu" onclick={(e) => menu(e, i)} data-testid="grpc-saved-menu"><Icon name="more" /></button>
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

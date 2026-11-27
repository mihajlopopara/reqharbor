<script lang="ts">
  import type { EditorController } from "../cm/editor";
  import type { MessageKind } from "../api/types";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { ws } from "../stores/ws.svelte";
  import { shortcutText } from "../util/shortcuts";
  import { dynamicVariables } from "../util/vars";
  import { prettyJson } from "../util/json";
  import { isSocketIo, messageError } from "../util/ws";
  import CodeEditor from "./CodeEditor.svelte";
  import VarInput from "./VarInput.svelte";
  import Icon from "./Icon.svelte";

  /** Message tab of a WebSocket / Socket.IO request (AC-WS-4): compose, send, save. */
  let editor = $state<EditorController | null>(null);
  let saving = $state(false);
  let saveName = $state("");

  const r = $derived(app.request);
  const sio = $derived(isSocketIo(r));
  const draft = $derived(ws.draftOf(r));
  const session = $derived(ws.session(r.id));
  const open = $derived(session?.state === "open");
  const problem = $derived(draft.body.trim() || sio ? messageError(draft, sio) : null);

  const KINDS: { v: MessageKind; label: string; title: string }[] = $derived(
    sio
      ? [
          { v: "json", label: "JSON", title: "The argument is a JSON value (empty = no argument)" },
          { v: "text", label: "Text", title: "The argument is a string" },
        ]
      : [
          { v: "text", label: "Text", title: "A text frame" },
          { v: "json", label: "JSON", title: "A text frame holding JSON (checked before sending)" },
          { v: "binary", label: "Binary", title: "A binary frame: enter the bytes as base64" },
        ],
  );

  function edit(patch: Partial<typeof draft>) {
    Object.assign(ws.ensureDraft(app.request), patch);
  }

  async function send() {
    if (!open) return;
    await app.wsSendDraft();
  }

  function format() {
    const p = prettyJson(draft.body);
    if (p !== null) edit({ body: p });
  }

  function insertFake(e: MouseEvent) {
    ui.openMenu(
      dynamicVariables().map((d) => ({
        label: d.description,
        shortcut: d.name,
        action: () => editor?.insert(`{{${d.name}}}`),
      })),
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
  }

  function startSave() {
    saveName = draft.name || draft.event || "";
    saving = true;
  }

  function commitSave() {
    if (!saving) return;
    saving = false;
    app.wsSaveDraft(saveName);
  }

  function chipKey(e: KeyboardEvent, i: number) {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const n = (i + d + KINDS.length) % KINDS.length;
    edit({ kind: KINDS[n].v });
    ((e.currentTarget as HTMLElement).parentElement?.children[n] as HTMLElement | undefined)?.focus();
  }
</script>

<div class="composer" data-testid="ws-composer">
  <div class="tools">
    {#if sio}
      <div class="event">
        <VarInput
          variant="field"
          lookup={app.lookup}
          value={draft.event ?? ""}
          placeholder="Event name"
          ariaLabel="Event name"
          testid="ws-event"
          oninput={(v) => edit({ event: v })}
          onkeydown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
              e.preventDefault();
              void send();
            }
          }}
        />
      </div>
      <label class="checkbox" title="Ask the server to acknowledge this event; the ack shows in the log">
        <input type="checkbox" checked={!!draft.ack} onchange={(e) => edit({ ack: e.currentTarget.checked })} data-testid="ws-ack" />Ack
      </label>
    {/if}
    <div class="chips" role="radiogroup" aria-label="Message type" data-testid="ws-kinds">
      {#each KINDS as k, i (k.v)}
        <button
          type="button"
          role="radio"
          aria-checked={draft.kind === k.v}
          tabindex={draft.kind === k.v ? 0 : -1}
          title={k.title}
          data-testid="ws-kind-{k.v}"
          onclick={() => edit({ kind: k.v })}
          onkeydown={(e) => chipKey(e, i)}>{k.label}</button
        >
      {/each}
    </div>
  </div>

  <div class="box">
    <div class="box-head">
      <span>{sio ? "Argument" : draft.kind === "binary" ? "Bytes (base64)" : "Message"}</span>
      <span class="head-actions">
        <button class="link" type="button" aria-haspopup="menu" title="Insert a {'{{$…}}'} variable that generates a new value each send" onclick={insertFake}>Insert fake data</button>
        {#if draft.kind === "json"}<button class="link" type="button" onclick={format} data-testid="ws-format">Format</button>{/if}
      </span>
    </div>
    <CodeEditor
      value={draft.body}
      language={draft.kind === "json" ? "json" : "text"}
      lookup={app.lookup}
      wrap={draft.kind !== "json"}
      ariaLabel={sio ? "Event argument" : "Message"}
      placeholder={sio ? '{ "text": "hello" }' : draft.kind === "binary" ? "AAEC/w==" : "Type a message"}
      testid="ws-message-editor"
      bind:controller={editor}
      onchange={(v) => edit({ body: v })}
    />
  </div>

  <div class="foot">
    {#if problem}
      <span class="err" role="alert" data-testid="ws-message-error"><Icon name="alert" small />{problem}</span>
    {:else if !open}
      <span class="faint small">{session?.state === "connecting" ? "Connecting…" : "Connect to send messages"}</span>
    {:else}
      <span class="faint small">{shortcutText("Mod+Enter", app.platform)} to send</span>
    {/if}
    <span class="grow"></span>
    {#if saving}
      <!-- svelte-ignore a11y_autofocus -->
      <input
        class="input save-name"
        placeholder="Message name"
        aria-label="Message name"
        bind:value={saveName}
        autofocus
        data-testid="ws-save-name"
        onkeydown={(e) => {
          if (e.key === "Enter") commitSave();
          else if (e.key === "Escape") {
            e.stopPropagation();
            saving = false;
          }
        }}
        onblur={commitSave}
      />
    {:else}
      <button class="btn btn-ghost btn-sm" type="button" onclick={startSave} disabled={app.readOnly} title="Keep this message in Saved messages (saved with the request)" data-testid="ws-save-message">Save message</button>
    {/if}
    {#if !sio}
      <button class="btn btn-ghost btn-sm" type="button" disabled={!open} onclick={() => void ws.ping(r.id)} title="Send a ping frame" data-testid="ws-ping">Ping</button>
    {/if}
    <button class="btn btn-primary btn-sm" type="button" disabled={!open || !!problem} onclick={send} data-testid="ws-send">{sio ? "Emit" : "Send"}</button>
  </div>
</div>

<style>
  .composer {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .tools {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
    padding: var(--sp-4) var(--sp-5) var(--sp-2);
    flex: none;
    flex-wrap: wrap;
  }
  .event {
    flex: 1;
    min-width: 160px;
    max-width: 320px;
  }
  .box {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    margin: var(--sp-2) var(--sp-5) 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-pane);
    overflow: hidden;
    transition: border-color var(--dur-fast);
  }
  .box:focus-within {
    border-color: var(--border-strong);
  }
  .box-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    height: 30px;
    padding: 0 var(--sp-4);
    font-size: var(--fs-sm);
    color: var(--text-3);
    background: var(--bg-sidebar);
    border-bottom: 1px solid var(--border-subtle);
    flex: none;
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
  }
  .box :global(.cm-editor) {
    padding-top: var(--sp-2);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-4) var(--sp-5);
    flex: none;
    min-height: 44px;
  }
  .err {
    color: var(--danger);
    font-size: var(--fs-sm);
    display: inline-flex;
    gap: var(--sp-2);
    align-items: center;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .save-name {
    width: 180px;
    height: 24px;
  }
</style>

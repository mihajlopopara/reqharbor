<script lang="ts">
  import type { CompletionContext, CompletionResult } from "@codemirror/autocomplete";
  import type { EditorController } from "../../cm/editor";
  import { app } from "../../stores/app.svelte";
  import { grpc } from "../../stores/grpc.svelte";
  import { ui } from "../../stores/ui.svelte";
  import { ws } from "../../stores/ws.svelte";
  import { dynamicVariables } from "../../util/vars";
  import { prettyJson } from "../../util/json";
  import { shortcutText } from "../../util/shortcuts";
  import { findMethod, sendsMessages } from "../../util/grpc";
  import { exampleMessage, jsonCompletions } from "../../grpc/schema";
  import CodeEditor from "../CodeEditor.svelte";
  import Icon from "../Icon.svelte";

  /** Message tab of a gRPC request (AC-GRPC-6, AC-GRPC-8): the JSON message, completion, Example, Send / End. */
  let editor = $state<EditorController | null>(null);
  let saving = $state(false);
  let saveName = $state("");

  const r = $derived(app.request);
  const g = $derived(r.grpc);
  const d = $derived(grpc.state(r.id).descriptor);
  const method = $derived(g ? findMethod(d, g.service, g.method) : null);
  const inputType = $derived(method?.inputType ?? null);
  const session = $derived(ws.session(r.id));
  const streamOpen = $derived(session?.transport === "grpc" && session.state === "open" && sendsMessages(session.callKind) && !session.halfClosed);
  const problem = $derived.by(() => {
    const t = g?.message ?? "";
    if (!t.trim() || /\{\{/.test(t)) return null;
    try {
      JSON.parse(t);
      return null;
    } catch (e) {
      return `Invalid JSON: ${(e as Error).message}`;
    }
  });

  function setMessage(v: string) {
    if (app.request.grpc) app.request.grpc.message = v;
  }

  // Field-name / enum completion from the method's input type.
  $effect(() => {
    const ed = editor;
    const desc = d;
    const t = inputType;
    if (!ed) return;
    if (!desc || !t) {
      ed.setAddons([], null);
      return;
    }
    ed.setAddons([], (ctx: CompletionContext): CompletionResult | null => {
      const res = jsonCompletions(desc, t, ctx.state.doc.toString(), ctx.pos);
      if (!res || (!ctx.explicit && res.from === ctx.pos)) return null;
      return {
        from: res.from,
        to: res.to,
        filter: false,
        options: res.options.map((o) => ({ label: o.label, detail: o.detail, apply: o.apply, type: "gql" })),
      };
    });
  });

  function replaceAll(text: string) {
    const ed = editor;
    if (!ed) return setMessage(text);
    const doc = ed.view.state.doc;
    // A real transaction (not setValue): it reaches onChange and ⌘Z undoes it.
    ed.view.dispatch({ changes: { from: 0, to: doc.length, insert: text } });
    ed.focus();
  }

  function example() {
    if (!d || !inputType) return;
    replaceAll(exampleMessage(d, inputType));
  }

  function format() {
    const p = prettyJson(g?.message || "{}");
    if (p !== null) replaceAll(p);
  }

  function insertFake(e: MouseEvent) {
    ui.openMenu(
      dynamicVariables().map((dv) => ({ label: dv.description, shortcut: dv.name, action: () => editor?.insert(`{{${dv.name}}}`) })),
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
  }

  function startSave() {
    saveName = "";
    saving = true;
  }

  function commitSave() {
    if (!saving) return;
    saving = false;
    app.grpcSaveDraft(saveName);
  }
</script>

<div class="composer" data-testid="grpc-message">
  <div class="box">
    <div class="box-head">
      <span>Message{#if inputType}<span class="type mono"> · {inputType}</span>{/if}</span>
      <span class="head-actions">
        <button class="link" type="button" disabled={!inputType} title={inputType ? "Fill in every field of the message (⌘Z undoes it)" : "Pick a method first"} onclick={example} data-testid="grpc-example">Example message</button>
        <button class="link" type="button" aria-haspopup="menu" title="Insert a {'{{$…}}'} variable that generates a new value each send" onclick={insertFake}>Insert fake data</button>
        <button class="link" type="button" onclick={format} data-testid="grpc-format">Format</button>
      </span>
    </div>
    <CodeEditor
      value={g?.message ?? ""}
      language="json"
      lookup={app.lookup}
      ariaLabel="gRPC message"
      placeholder={"{ }"}
      testid="grpc-message-editor"
      bind:controller={editor}
      onchange={setMessage}
    />
  </div>

  <div class="foot">
    {#if problem}
      <span class="err" role="alert" data-testid="grpc-message-error"><Icon name="alert" small />{problem}</span>
    {:else if streamOpen}
      <span class="faint small">Sent on the open stream · {shortcutText("Mod+Enter", app.platform)}</span>
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
        data-testid="grpc-save-name"
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
      <button class="btn btn-ghost btn-sm" type="button" onclick={startSave} disabled={app.readOnly} title="Keep this message in Saved messages (saved with the request)" data-testid="grpc-save-message">Save message</button>
    {/if}
    {#if streamOpen}
      <button class="btn btn-ghost btn-sm" type="button" onclick={() => void app.grpcEnd()} title="Close the sending side: no more messages" data-testid="grpc-end">End stream</button>
      <button class="btn btn-primary btn-sm" type="button" disabled={!!problem} onclick={() => void app.grpcSendDraft()} data-testid="grpc-send">Send</button>
    {/if}
  </div>
</div>

<style>
  .composer {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding-top: var(--sp-4);
  }
  .box {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    margin: 0 var(--sp-5);
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-md);
    background: var(--bg-pane);
    overflow: hidden;
  }
  .box:focus-within {
    border-color: var(--border-strong);
  }
  .box-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-4);
    height: 30px;
    padding: 0 var(--sp-4);
    font-size: var(--fs-sm);
    color: var(--text-3);
    background: var(--bg-sidebar);
    border-bottom: 1px solid var(--border-subtle);
    flex: none;
    white-space: nowrap;
  }
  .box-head > span:first-child {
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .type {
    color: var(--text-3);
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

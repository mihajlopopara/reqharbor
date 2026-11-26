<script lang="ts">
  import type { BodyType } from "../api/types";
  import type { EditorController } from "../cm/editor";
  import { app } from "../stores/app.svelte";
  import { pickFile, fileNameOf } from "../api/platform";
  import CodeEditor from "./CodeEditor.svelte";
  import KvTable from "./KvTable.svelte";
  import Icon from "./Icon.svelte";

  let { editor = $bindable(null) }: { editor?: EditorController | null } = $props();

  const TYPES: { v: BodyType; label: string }[] = [
    { v: "none", label: "None" },
    { v: "json", label: "JSON" },
    { v: "xml", label: "XML" },
    { v: "text", label: "Text" },
    { v: "form", label: "Form" },
    { v: "formEncoded", label: "Form-encode" },
    { v: "graphql", label: "GraphQL" },
    { v: "binary", label: "Binary" },
  ];
  const LABELS: Partial<Record<BodyType, string>> = { json: "JSON Content", xml: "XML Content", text: "Text Content" };

  const body = $derived(app.request.body);
  let varsEditor = $state<EditorController | null>(null);
  let binSize = $state<number | null>(null);

  const gqlVarsError = $derived.by(() => {
    if (body.type !== "graphql" || !body.graphql?.variables.trim()) return null;
    try {
      JSON.parse(body.graphql.variables);
      return null;
    } catch (e) {
      return (e as Error).message;
    }
  });

  function setType(t: BodyType) {
    const b = app.request.body;
    b.type = t;
    if ((t === "json" || t === "xml" || t === "text") && b.text === undefined) b.text = "";
    if (t === "form") b.form ??= [];
    if (t === "formEncoded") b.formEncoded ??= [];
    if (t === "graphql") b.graphql ??= { query: "", variables: "" };
    app.formatError = null;
  }

  export async function format() {
    const pos = await app.formatBody();
    if (pos) (body.type === "graphql" ? varsEditor : editor)?.goTo(pos.line, pos.column);
  }

  async function chooseBinary() {
    const f = await pickFile({ title: "Choose file to send" });
    if (f) {
      app.request.body.binaryPath = f.path;
      binSize = f.size;
    }
  }

  function chipKey(e: KeyboardEvent, i: number) {
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const n = (i + d + TYPES.length) % TYPES.length;
    setType(TYPES[n].v);
    ((e.currentTarget as HTMLElement).parentElement?.children[n] as HTMLElement | undefined)?.focus();
  }
</script>

<div class="body-ed">
  <div class="body-tools">
    <div class="chips" role="radiogroup" aria-label="Body type" data-testid="body-types">
      {#each TYPES as t, i (t.v)}
        <button
          type="button"
          role="radio"
          aria-checked={body.type === t.v}
          tabindex={body.type === t.v ? 0 : -1}
          data-testid="body-type-{t.v}"
          onclick={() => setType(t.v)}
          onkeydown={(e) => chipKey(e, i)}>{t.label}</button
        >
      {/each}
    </div>
  </div>

  {#if app.request.method === "QUERY" && body.type === "none"}
    <div class="pad"><div class="hint" data-testid="query-body-hint"><Icon name="info" />QUERY requests normally carry a body.</div></div>
  {/if}

  {#if body.type === "none"}
    <p class="faint pad line">This request has no body.</p>
  {:else if body.type === "json" || body.type === "xml" || body.type === "text"}
    {#if app.formatError}
      <div class="pad"><div class="hint danger" role="alert" data-testid="format-error"><Icon name="alert" />{app.formatError.message}</div></div>
    {/if}
    <div class="box">
    <div class="box-head">
      <span>{LABELS[body.type]}</span>
      {#if body.type !== "text"}
        <button class="link" type="button" title="Format  {app.mod}⇧F" onclick={format} data-testid="format-body">Format</button>
      {/if}
    </div>
    <CodeEditor
      value={body.text ?? ""}
      language={body.type}
      lookup={app.lookup}
      ariaLabel="Request body"
      testid="body-editor"
      bind:controller={editor}
      onchange={(v) => {
        app.request.body.text = v;
        app.formatError = null;
      }}
    />
    </div>
  {:else if body.type === "form" || body.type === "formEncoded"}
    <div class="pad scroll">
      <KvTable rows={body.type === "form" ? (body.form ?? []) : (body.formEncoded ?? [])} kind={body.type} lookup={app.lookup} testid={body.type === "form" ? "form" : "form-encoded"} />
    </div>
  {:else if body.type === "graphql" && body.graphql}
    {@const gql = body.graphql}
    <div class="box gq">
      <div class="box-head"><span>Query</span></div>
      <CodeEditor value={gql.query} language="text" lookup={app.lookup} ariaLabel="GraphQL query" testid="graphql-query" placeholder={"query { … }"} onchange={(v) => (gql.query = v)} />
    </div>
    <div class="box gv">
      <div class="box-head">
        <span>Variables</span>
        <button class="link" type="button" title="Format  {app.mod}⇧F" onclick={format}>Format</button>
      </div>
      <CodeEditor value={gql.variables} language="json" lookup={app.lookup} ariaLabel="GraphQL variables" testid="graphql-variables" placeholder={"{ }"} bind:controller={varsEditor} onchange={(v) => (gql.variables = v)} />
    </div>
    {#if gqlVarsError || app.formatError}
      <div class="pad"><div class="hint danger" role="alert" data-testid="graphql-error"><Icon name="alert" />{app.formatError?.message ?? `Invalid variables JSON: ${gqlVarsError}`}</div></div>
    {/if}
  {:else if body.type === "binary"}
    <div class="pad">
      {#if body.binaryPath}
        <div class="bin-file">
          <Icon name="file" />
          <div class="grow">
            <div class="fname">{fileNameOf(body.binaryPath)}{#if binSize !== null}<span class="faint"> · {binSize.toLocaleString()} B</span>{/if}</div>
            <div class="mono faint path" title={body.binaryPath}>{body.binaryPath}</div>
          </div>
          <button class="btn btn-secondary btn-sm" type="button" onclick={chooseBinary}>Replace</button>
          <button class="btn btn-ghost btn-sm btn-danger" type="button" onclick={() => (app.request.body.binaryPath = "")}>Remove</button>
        </div>
      {:else}
        <div class="drop" data-testid="binary-drop">
          <span>Drop a file or <button class="link" type="button" onclick={chooseBinary}>Choose file…</button></span>
          <span class="faint small">Sent as application/octet-stream unless Content-Type is set</span>
        </div>
      {/if}
    </div>
  {/if}
</div>

<style>
  .body-ed {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .body-tools {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    padding: var(--sp-4) var(--sp-5) var(--sp-2);
    flex: none;
  }
  /* Bordered card around each editor; the header carries its label + actions. */
  .box {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    margin: var(--sp-2) var(--sp-5) var(--sp-5);
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
  .box :global(.cm-editor) {
    padding-top: var(--sp-2);
  }
  .pad {
    padding: var(--sp-2) var(--sp-5);
  }
  .scroll {
    overflow: auto;
    flex: 1;
  }
  .line {
    margin: var(--sp-4) 0;
  }
  .gq {
    flex: 2;
    min-height: 120px;
    margin-bottom: var(--sp-3);
  }
  .gv {
    flex: 1;
    min-height: 100px;
  }
  .drop {
    height: 120px;
    border: 1px dashed var(--border-strong);
    border-radius: var(--r-md);
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-2);
    color: var(--text-2);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .bin-file {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    padding: var(--sp-4);
    background: var(--bg-input);
    border-radius: var(--r-md);
  }
  .fname {
    font-weight: var(--fw-medium);
  }
  .path {
    font-size: var(--fs-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
</style>

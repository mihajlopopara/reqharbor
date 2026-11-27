<script lang="ts">
  import { untrack } from "svelte";
  import type { BodyType, Request } from "../api/types";
  import type { EditorController } from "../cm/editor";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { graphql, loadGql } from "../stores/graphql.svelte";
  import { dynamicVariables } from "../util/vars";
  import { agoPhrase, formatSize } from "../util/format";
  import { insertTextForField, operationLabel, pickOperationName, scanOperations, type FieldModel, type VariableDef } from "../util/graphql";
  import { pickFile, fileNameOf } from "../api/platform";
  import CodeEditor from "./CodeEditor.svelte";
  import KvTable from "./KvTable.svelte";
  import Icon from "./Icon.svelte";
  import SchemaExplorer from "./SchemaExplorer.svelte";

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

  export async function format(part?: "query" | "variables") {
    const pos = await app.formatBody(part);
    if (pos) (pos.target === "query" ? queryEditor : pos.target === "variables" ? varsEditor : editor)?.goTo(pos.line, pos.column);
  }

  // ───────────── GraphQL (AC-GQL-*) ─────────────
  let queryEditor = $state<EditorController | null>(null);
  const isGql = $derived(body.type === "graphql" && !!body.graphql);
  const reqId = $derived(app.request.id);
  const sch = $derived(graphql.state(reqId));
  const operations = $derived(isGql ? scanOperations(body.graphql?.query ?? "") : []);
  const namedOps = $derived(operations.filter((o) => o.name));
  /** What the picker shows and what is sent (AC-GQL-9): the stored choice, else the first named operation. */
  const pickedOp = $derived(pickOperationName(operations, body.graphql?.operationName));
  /** The operation the variables belong to: the picked one, else the only one. */
  const activeOp = $derived(pickedOp ?? (operations.length === 1 ? operations[0].name : namedOps[0]?.name) ?? null);

  function snapshot(): Request {
    return $state.snapshot(app.request) as Request;
  }

  /** Keep `operationName` in step with the document (AC-GQL-9): only with several operations. */
  function syncOperation(query: string) {
    const g = app.request.body.graphql;
    if (!g) return;
    const next = pickOperationName(scanOperations(query), g.operationName);
    if (next === g.operationName) return;
    if (next === undefined) delete g.operationName;
    else g.operationName = next;
  }

  function onQuery(v: string) {
    const g = app.request.body.graphql;
    if (!g) return;
    g.query = v;
    syncOperation(v);
    if (app.formatError?.target === "query") app.formatError = null;
    if (app.formatNotice) app.formatNotice = null;
  }

  function chooseOperation(name: string) {
    const g = app.request.body.graphql;
    if (g) g.operationName = name;
  }

  // The cached schema for the schema URL: on open, and whenever an input of that URL changes
  // (URL, params, auth incl. the collection's for Inherit, the active and Global environments' values).
  const lookupKey = $derived.by(() => {
    if (!isGql) return "";
    const r = app.request;
    const inherited = r.auth.type === "inherit" && r.collectionId ? (app.index?.collectionById.get(r.collectionId)?.auth ?? null) : null;
    const envs = app.environments.filter((e) => e.projectId === null || e.id === app.activeEnv?.id).map((e) => e.variables);
    return JSON.stringify([reqId, r.url, r.params, r.auth, inherited, envs, app.activeProjectId]);
  });
  $effect(() => {
    if (!lookupKey) return;
    const t = setTimeout(() => untrack(() => void graphql.refreshCached(snapshot(), app.activeProjectId)), 250);
    return () => clearTimeout(t);
  });

  // Editor addons (completion, hover, lint) from the lazy GraphQL chunk.
  let gqlMod = $state.raw<Awaited<ReturnType<typeof loadGql>> | null>(null);
  $effect(() => {
    if (isGql && !gqlMod) void loadGql().then((m) => (gqlMod = m));
  });
  const queryCtx = () => {
    const b = graphql.schemaOf(app.request.id);
    return { schema: b?.schema ?? null, schemaValid: !!b && !b.problems.length, onShowInDocs: (t: string, f: string | null) => graphql.showInDocs(t, f) };
  };
  let varsCtx: { defs: VariableDef[] | null; label: string; check: ((d: VariableDef, v: unknown) => string | null) | undefined } = { defs: null, label: "the query", check: undefined };
  $effect(() => {
    const m = gqlMod;
    const ed = queryEditor;
    if (m && ed) ed.setAddons(m.queryExtensions(queryCtx), m.queryCompletionSource(queryCtx));
  });
  $effect(() => {
    const m = gqlMod;
    const ed = varsEditor;
    if (m && ed && isGql) ed.setAddons(m.variablesExtensions(() => varsCtx), m.variablesCompletionSource(() => varsCtx));
  });
  // Re-check when the schema arrives or changes.
  $effect(() => {
    void sch.info?.fetchedAt;
    void sch.status;
    const m = gqlMod;
    if (m && queryEditor) m.relint(queryEditor.view);
  });
  // Variables follow the selected operation's definitions (AC-GQL-8).
  $effect(() => {
    const m = gqlMod;
    const q = body.graphql?.query ?? "";
    const op = activeOp;
    void sch.info?.fetchedAt;
    if (!m || !isGql) return;
    const defs = m.variableDefinitions(q, op ?? undefined);
    const built = graphql.schemaOf(reqId);
    varsCtx = {
      defs: defs ?? varsCtx.defs,
      label: operationLabel(op),
      check: built && !built.problems.length ? m.valueChecker(built.schema) : undefined,
    };
    if (varsEditor) m.relint(varsEditor.view);
  });

  function insertField(f: FieldModel) {
    const { text, caret } = insertTextForField(f);
    queryEditor?.insert(text, caret);
  }

  function schemaMenu(e: MouseEvent) {
    const r = snapshot();
    ui.openMenu(
      [
        { label: sch.info ? "Refresh schema" : "Fetch schema", action: () => void graphql.fetch(r, app.activeProjectId), testid: "graphql-menu-fetch" },
        { label: "Load from file…", action: () => void graphql.loadFile(r, app.activeProjectId), testid: "graphql-menu-file" },
        { separator: true },
        { label: "Forget schema", disabled: !sch.info, action: () => void graphql.forget(r, app.activeProjectId), testid: "graphql-menu-forget" },
      ],
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
  }

  /** "Insert fake data": a {{$dynamic}} variable at the caret (AC-ENV-10). */
  function insertFake(e: MouseEvent, target: EditorController | null) {
    ui.openMenu(
      dynamicVariables().map((d) => ({
        label: d.description,
        shortcut: d.name,
        testid: `fake-${d.name.slice(1)}`,
        action: () => target?.insert(`{{${d.name}}}`),
      })),
      e.currentTarget as HTMLElement,
      { alignRight: true },
    );
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
      <span class="head-actions">
        <button class="link" type="button" aria-haspopup="menu" title="Insert a {'{{$…}}'} variable that generates a new value each send" onclick={(e) => insertFake(e, editor)} data-testid="insert-fake">Insert fake data</button>
        {#if body.type !== "text"}
          <button class="link" type="button" title="Format  {app.mod}⇧F" onclick={() => format()} data-testid="format-body">Format</button>
        {/if}
      </span>
    </div>
    <CodeEditor
      value={body.text ?? ""}
      language={body.type as "json" | "xml" | "text"}
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
      <KvTable rows={body.type === "form" ? (body.form ?? []) : (body.formEncoded ?? [])} kind={body.type as "form" | "formEncoded"} lookup={app.lookup} testid={body.type === "form" ? "form" : "form-encoded"} />
    </div>
  {:else if body.type === "graphql" && body.graphql}
    {@const gql = body.graphql}
    <div class="gql-bar" data-testid="graphql-schema-bar" data-status={sch.status}>
      {#if sch.status === "fetching" || sch.status === "loading"}
        <span class="spinner" aria-hidden="true"></span>
        <span class="faint" role="status">{sch.status === "fetching" ? "Fetching schema…" : "Loading schema…"}</span>
        {#if sch.status === "fetching"}<button class="link" type="button" onclick={() => void graphql.cancelFetch(reqId)}>Cancel</button>{/if}
      {:else if sch.info}
        <Icon name="book" small />
        <span class="gql-status" data-testid="graphql-schema-status" title={`${sch.info.url} · ${formatSize(sch.info.sizeBytes)}`}
          >Schema<span class="faint"
            >{` · ${sch.typeCount} types · ${sch.info.source === "file" ? `from ${sch.info.fileName ?? "file"} · loaded` : "fetched"} ${agoPhrase(sch.info.fetchedAt, app.now)}`}</span
          ></span
        >
        {#if sch.problems.length}
          <span class="gql-problem" title={sch.problems.join("\n")}><Icon name="alert" small />Schema has problems: queries aren't validated</span>
        {/if}
        <span class="grow"></span>
        <button class="icon-btn" type="button" aria-label="Refresh schema" title="Refresh schema (introspection)" onclick={() => void graphql.fetch(snapshot(), app.activeProjectId)} data-testid="graphql-refresh-schema"><Icon name="refresh" small /></button>
        <button class="btn btn-ghost btn-sm" type="button" aria-pressed={graphql.docsOpen} title="Schema explorer" onclick={() => (graphql.docsOpen = !graphql.docsOpen)} data-testid="graphql-docs-toggle">Docs</button>
      {:else}
        <Icon name="book" small />
        <span class="faint gql-status" title="Completion, hover docs and checks need the schema">No schema · needed for completion and checks</span>
        <span class="grow"></span>
        <button class="link" type="button" onclick={() => void graphql.fetch(snapshot(), app.activeProjectId)} disabled={!app.request.url.trim()} title={app.request.url.trim() ? "Send the introspection query to this URL" : "Enter a URL first"} data-testid="graphql-fetch-schema">Fetch schema</button>
        <button class="link" type="button" onclick={() => void graphql.loadFile(snapshot(), app.activeProjectId)} data-testid="graphql-load-file">Load from file…</button>
      {/if}
      {#if sch.status !== "fetching" && sch.status !== "loading" && sch.info}
        <button class="icon-btn" type="button" aria-label="Schema actions" aria-haspopup="menu" onclick={schemaMenu} data-testid="graphql-schema-menu"><Icon name="more" small /></button>
      {/if}
    </div>
    {#if sch.error}
      <div class="pad">
        <div class="hint {sch.error.kind === 'disabled' ? 'warn' : 'danger'}" role="alert" data-testid="graphql-schema-error" data-kind={sch.error.kind}>
          <Icon name="alert" />
          <span class="grow">{sch.error.message}{#if sch.error.detail}<span class="mono faint server-msg"> {sch.error.detail}</span>{/if}</span>
          {#if sch.error.kind === "disabled"}
            <button class="link" type="button" onclick={() => void graphql.loadFile(snapshot(), app.activeProjectId)}>Load from file…</button>
          {:else if sch.error.kind === "network" || sch.error.kind === "failed"}
            <button class="link" type="button" onclick={() => void graphql.fetch(snapshot(), app.activeProjectId)}>Retry</button>
          {/if}
          <button class="icon-btn" type="button" aria-label="Dismiss" onclick={() => graphql.dismissError(reqId)}><Icon name="x" small /></button>
        </div>
      </div>
    {/if}
    <div class="gql-wrap">
      <div class="gql-inner">
      <div class="gql-eds">
        <div class="box gq">
          <div class="box-head">
            <span>Query</span>
            <span class="head-actions">
              {#if namedOps.length >= 2}
                <label class="op-pick">
                  <span>Operation</span>
                  <span class="select-wrap">
                    <select class="input op-select" value={pickedOp} onchange={(e) => chooseOperation(e.currentTarget.value)} data-testid="graphql-operation" aria-label="Operation to run">
                      {#each namedOps as o (o.from)}<option value={o.name}>{o.name} · {o.type}</option>{/each}
                    </select>
                    <Icon name="chev-d" small />
                  </span>
                </label>
              {/if}
              <button class="link" type="button" title="Prettify the query  {app.mod}⇧F" onclick={() => format("query")} data-testid="graphql-format-query">Format</button>
            </span>
          </div>
          <CodeEditor value={gql.query} language="graphql" lookup={app.lookup} ariaLabel="GraphQL query" testid="graphql-query" placeholder={"query { … }"} bind:controller={queryEditor} onchange={onQuery} />
        </div>
        <div class="box gv">
          <div class="box-head">
            <span>Variables{#if activeOp && namedOps.length >= 2}<span class="faint">{` · ${activeOp}`}</span>{/if}</span>
            <span class="head-actions">
              <button class="link" type="button" aria-haspopup="menu" title="Insert a {'{{$…}}'} variable that generates a new value each send" onclick={(e) => insertFake(e, varsEditor)}>Insert fake data</button>
              <button class="link" type="button" title="Format  {app.mod}⇧F" onclick={() => format("variables")}>Format</button>
            </span>
          </div>
          <CodeEditor value={gql.variables} language="json" lookup={app.lookup} ariaLabel="GraphQL variables" testid="graphql-variables" placeholder={"{ }"} bind:controller={varsEditor} onchange={(v) => (gql.variables = v)} />
        </div>
        {#if app.formatNotice?.requestId === reqId}
          <div class="pad"><div class="hint" role="status" data-testid="graphql-format-notice"><Icon name="info" />{app.formatNotice.message}</div></div>
        {/if}
        {#if gqlVarsError || app.formatError}
          <div class="pad"><div class="hint danger" role="alert" data-testid="graphql-error"><Icon name="alert" />{app.formatError?.message ?? `Invalid variables JSON: ${gqlVarsError}`}</div></div>
        {/if}
      </div>
      {#if graphql.docsOpen && sch.model}
        <SchemaExplorer model={sch.model} oninsert={insertField} onclose={() => (graphql.docsOpen = false)} />
      {/if}
      </div>
    </div>
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
  .head-actions {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
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
  /* GraphQL schema bar (AC-GQL-4): one 28px line, like the editors' label rows. */
  .gql-bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 28px;
    padding: 0 var(--sp-5);
    font-size: var(--fs-sm);
    color: var(--text-2);
    flex: none;
  }
  .gql-bar .link,
  .gql-bar .btn {
    white-space: nowrap;
    flex: none;
  }
  .gql-bar > :global(svg) {
    color: var(--text-3);
    flex: none;
  }
  .gql-status {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-1);
  }
  .gql-status.faint {
    color: var(--text-3);
  }
  .gql-problem {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--warning);
    white-space: nowrap;
  }
  .server-msg {
    margin-left: var(--sp-2);
    word-break: break-word;
  }
  .spinner {
    width: 12px;
    height: 12px;
    border: 1.5px solid var(--text-3);
    border-top-color: var(--text-1);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
    flex: none;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .spinner {
      animation: none;
    }
  }
  /* Editors beside the explorer; when the pane is narrower than 640px the explorer goes below them (AC-GQL-10). */
  .gql-wrap {
    flex: 1;
    min-height: 0;
    display: flex;
    container-type: inline-size;
  }
  .gql-inner {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
  }
  .gql-eds {
    flex: 1;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  @container (max-width: 640px) {
    .gql-inner {
      flex-direction: column;
    }
    .gql-inner > :global(.docs) {
      width: auto;
      height: 45%;
      min-height: 180px;
      border-left: 0;
      border-top: 1px solid var(--border-subtle);
    }
  }
  .op-pick {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .op-pick .select-wrap {
    width: auto;
  }
  .op-select {
    height: 22px;
    max-width: 200px;
    padding-left: var(--sp-3);
    font-family: var(--font-mono);
    font-size: var(--fs-sm);
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

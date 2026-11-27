<script lang="ts">
  import type { Request } from "../../api/types";
  import { isTauri } from "../../api";
  import { app } from "../../stores/app.svelte";
  import { grpc } from "../../stores/grpc.svelte";
  import { agoPhrase } from "../../util/format";
  import { methodCount, servicesLabel, tryTarget } from "../../util/grpc";
  import Icon from "../Icon.svelte";

  /** Proto tab (AC-GRPC-3/4): server reflection or local .proto files. */
  const r = $derived(app.request);
  const g = $derived(r.grpc);
  const st = $derived(grpc.state(r.id));
  const d = $derived(st.descriptor);
  const files = $derived(g?.protoSource === "files");
  const origin = $derived(tryTarget(r.url)?.origin ?? r.url);

  let newFile = $state("");
  let newImport = $state("");

  const snapshot = () => $state.snapshot(app.request) as Request;

  function setSource(s: "reflection" | "files") {
    if (app.request.grpc && app.request.grpc.protoSource !== s) {
      app.request.grpc.protoSource = s;
      grpc.dismissError(app.request.id);
    }
  }

  function add(list: "protoFiles" | "importPaths", paths: string[]) {
    const gg = app.request.grpc;
    if (!gg) return;
    for (const p of paths.map((x) => x.trim()).filter(Boolean)) if (!gg[list].includes(p)) gg[list].push(p);
  }

  async function pick(list: "protoFiles" | "importPaths") {
    if (!isTauri) return;
    const { open } = await import("@tauri-apps/plugin-dialog");
    const res =
      list === "protoFiles"
        ? await open({ multiple: true, directory: false, title: "Add .proto files", filters: [{ name: "Protocol Buffers", extensions: ["proto"] }] })
        : await open({ multiple: false, directory: true, title: "Add import path" });
    if (!res) return;
    add(list, (Array.isArray(res) ? res : [res]).map(String));
  }

  function remove(list: "protoFiles" | "importPaths", i: number) {
    app.request.grpc?.[list].splice(i, 1);
  }
</script>

<div class="proto" data-testid="grpc-proto">
  <div class="chips" role="radiogroup" aria-label="Services from">
    <button type="button" role="radio" aria-checked={!files} onclick={() => setSource("reflection")} data-testid="grpc-source-reflection">Server reflection</button>
    <button type="button" role="radio" aria-checked={files} onclick={() => setSource("files")} data-testid="grpc-source-files">.proto files</button>
  </div>

  {#if !files}
    <div class="bar" data-testid="grpc-reflection-bar">
      <Icon name="book" small />
      {#if st.status === "fetching"}
        <span class="spinner" aria-hidden="true"></span><span class="faint grow">Fetching services…</span>
        <button class="link" type="button" onclick={() => void grpc.cancelFetch(r.id)} data-testid="grpc-fetch-cancel">Cancel</button>
      {:else if d && d.source === "reflection"}
        <span class="grow ell" title={d.url} data-testid="grpc-services-status"><b>{servicesLabel(d.services.length)}</b><span class="faint"> · {methodCount(d)} methods · fetched {agoPhrase(d.fetchedAt, app.now)}</span></span>
        <button class="icon-btn" type="button" aria-label="Refresh services (reflection)" title="Refresh services (reflection)" onclick={() => void grpc.fetch(snapshot(), app.activeProjectId)} data-testid="grpc-refresh"><Icon name="refresh" small /></button>
        <button class="link" type="button" onclick={() => void grpc.forget(snapshot(), app.activeProjectId)} data-testid="grpc-forget">Forget</button>
      {:else}
        <span class="faint grow ell">No services · fetch them from the server</span>
        <button class="link" type="button" disabled={!r.url.trim()} title={r.url.trim() ? `Ask ${origin} for its services` : "Enter a URL first"} onclick={() => void grpc.fetch(snapshot(), app.activeProjectId)} data-testid="grpc-fetch">Fetch services</button>
      {/if}
    </div>
  {:else}
    <div class="lists">
      <section>
        <header><span>Proto files</span>{#if isTauri}<button class="link" type="button" onclick={() => void pick("protoFiles")} data-testid="grpc-add-files">Add files…</button>{/if}</header>
        <ul>
          {#each g?.protoFiles ?? [] as p, i (p)}
            <li data-testid="grpc-proto-file"><span class="mono ell" title={p}>{p}</span><button class="icon-btn" type="button" aria-label="Remove {p}" onclick={() => remove("protoFiles", i)}><Icon name="x" small /></button></li>
          {:else}
            <li class="faint">No files yet</li>
          {/each}
        </ul>
        {#if !isTauri}
          <form class="add" onsubmit={(e) => { e.preventDefault(); add("protoFiles", [newFile]); newFile = ""; }}>
            <input class="input mono" placeholder="/path/to/service.proto" aria-label="Proto file path" bind:value={newFile} data-testid="grpc-add-file-input" />
            <button class="btn btn-secondary btn-sm" type="submit" disabled={!newFile.trim()} data-testid="grpc-add-file">Add file</button>
          </form>
        {/if}
      </section>
      <section>
        <header><span>Import paths</span>{#if isTauri}<button class="link" type="button" onclick={() => void pick("importPaths")} data-testid="grpc-add-import">Add import path…</button>{/if}</header>
        <ul>
          {#each g?.importPaths ?? [] as p, i (p)}
            <li data-testid="grpc-import-path"><span class="mono ell" title={p}>{p}</span><button class="icon-btn" type="button" aria-label="Remove {p}" onclick={() => remove("importPaths", i)}><Icon name="x" small /></button></li>
          {:else}
            <li class="faint">Each file's own folder is searched too</li>
          {/each}
        </ul>
        {#if !isTauri}
          <form class="add" onsubmit={(e) => { e.preventDefault(); add("importPaths", [newImport]); newImport = ""; }}>
            <input class="input mono" placeholder="/path/to/protos" aria-label="Import path" bind:value={newImport} data-testid="grpc-add-import-input" />
            <button class="btn btn-secondary btn-sm" type="submit" disabled={!newImport.trim()}>Add path</button>
          </form>
        {/if}
      </section>
      <div class="bar">
        {#if st.status === "loading"}
          <span class="spinner" aria-hidden="true"></span><span class="faint">Compiling…</span>
        {:else if d && d.source === "files"}
          <span data-testid="grpc-services-status"><b>{servicesLabel(d.services.length)}</b><span class="faint"> from {d.files.length} file{d.files.length === 1 ? "" : "s"}</span></span>
        {:else}
          <span class="faint">Add .proto files to list their services</span>
        {/if}
      </div>
    </div>
  {/if}

  {#if st.error}
    <div class="hint {st.error.kind === 'unsupported' ? 'warn' : 'danger'}" role="alert" data-testid="grpc-proto-error" data-kind={st.error.kind}>
      <Icon name="alert" />
      <span class="grow">{st.error.message}</span>
      {#if st.error.kind === "unsupported"}
        <button class="link" type="button" onclick={() => setSource("files")} data-testid="grpc-use-files">Use .proto files</button>
      {:else if !files && st.error.kind !== "blocked"}
        <button class="link" type="button" onclick={() => void grpc.fetch(snapshot(), app.activeProjectId)}>Retry</button>
      {/if}
      <button class="icon-btn" type="button" aria-label="Dismiss" onclick={() => grpc.dismissError(r.id)}><Icon name="x" small /></button>
    </div>
  {/if}
</div>

<style>
  .proto {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    padding: var(--sp-4) var(--sp-5);
  }
  .chips {
    display: flex;
    gap: var(--sp-2);
  }
  .chips button {
    height: 22px;
    padding: 0 var(--sp-4);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .chips button:hover {
    background: var(--bg-hover);
  }
  .chips button[aria-checked="true"] {
    background: var(--accent-soft);
    color: var(--accent-fg);
  }
  .bar {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 28px;
    font-size: var(--fs-sm);
    color: var(--text-3);
    min-width: 0;
  }
  .bar b {
    color: var(--text-1);
    font-weight: var(--fw-medium);
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .lists {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    font-size: var(--fs-sm);
    color: var(--text-2);
    margin-bottom: var(--sp-2);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--sp-3);
    min-height: 28px;
    border-top: 1px solid var(--border-subtle);
    font-size: var(--fs-sm);
  }
  li:first-child {
    border-top: 0;
  }
  .add {
    display: flex;
    gap: var(--sp-3);
    margin-top: var(--sp-2);
  }
  .add .input {
    flex: 1;
    height: 26px;
  }
  .spinner {
    width: 12px;
    height: 12px;
  }
</style>

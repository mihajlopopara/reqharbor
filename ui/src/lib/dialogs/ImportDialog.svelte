<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui, type ImportTab } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type CurlImport, type OpenApiPreview, type ShareSnapshot, type ShareKind } from "../api";
  import { pickFile, onFileDrop, fileNameOf } from "../api/platform";
  import { methodClass, methodLabel } from "../util/format";
  import { requestSummary } from "../util/request";
  import { childFolders } from "../util/tree";

  let { tab: initialTab, text: initialText = "", path: initialPath = null }: { tab: ImportTab; text?: string; path?: string | null } = $props();

  // svelte-ignore state_referenced_locally
  let tab = $state<ImportTab>(initialTab);
  const close = () => (ui.importDialog = null);

  // ───────── cURL ─────────
  // svelte-ignore state_referenced_locally
  let curlText = $state(initialText);
  let curl = $state<CurlImport | null>(null);
  let curlError = $state<string | null>(null);
  let flagsDismissed = $state(false);
  let dest = $state<"open" | "save">("open");
  let destKey = $state("");
  let curlTimer: ReturnType<typeof setTimeout> | null = null;

  const destinations = $derived.by(() => {
    const ix = app.index;
    const out: { key: string; label: string; collectionId: string; folderId: string | null }[] = [];
    if (!ix) return out;
    const walk = (cid: string, parent: string | null, prefix: string) => {
      for (const f of childFolders(ix, cid, parent)) {
        out.push({ key: `${cid}/${f.id}`, label: `${prefix} › ${f.name}`, collectionId: cid, folderId: f.id });
        walk(cid, f.id, `${prefix} › ${f.name}`);
      }
    };
    for (const c of ix.collections) {
      out.push({ key: `${c.id}/`, label: c.name, collectionId: c.id, folderId: null });
      walk(c.id, null, c.name);
    }
    return out;
  });

  $effect(() => {
    if (dest === "save" && !destinations.some((d) => d.key === destKey) && destinations[0]) destKey = destinations[0].key;
  });

  function previewCurl() {
    if (curlTimer) clearTimeout(curlTimer);
    const t = curlText;
    curlTimer = setTimeout(async () => {
      if (!t.trim()) {
        curl = null;
        curlError = null;
        return;
      }
      try {
        curl = await call("import_curl", { text: t });
        curlError = null;
        flagsDismissed = false;
      } catch (e) {
        curl = null;
        const err = toCommandError(e);
        curlError = err.code === "not_curl" ? "Not a cURL command" : err.message;
      }
    }, 120);
  }

  async function importCurl() {
    if (!curl) return;
    const req = curl.request;
    if (dest === "open") {
      app.newRequest(req);
      close();
      toasts.success("Imported from cURL");
      return;
    }
    const d = destinations.find((x) => x.key === destKey) ?? destinations[0];
    if (!d) return;
    try {
      const created = await call("create_request", { collectionId: d.collectionId, folderId: d.folderId, request: req });
      await app.loadTree();
      app.openRequest(created.id, { reveal: true });
      close();
      toasts.success(`Saved “${created.name}”`);
    } catch (e) {
      app.reportError(e);
    }
  }

  // ───────── OpenAPI ─────────
  let specText = $state("");
  // svelte-ignore state_referenced_locally
  let specPath = $state<string | null>(initialPath);
  let spec = $state<OpenApiPreview | null>(null);
  let specError = $state<{ code: string; message: string } | null>(null);
  let specBusy = $state(false);
  // svelte-ignore state_referenced_locally
  let projectId = $state(app.activeProjectId ?? "");
  let specTimer: ReturnType<typeof setTimeout> | null = null;

  function previewSpec() {
    if (specTimer) clearTimeout(specTimer);
    specTimer = setTimeout(async () => {
      const text = specText.trim() ? specText : null;
      if (!text && !specPath) {
        spec = null;
        specError = null;
        return;
      }
      specBusy = true;
      try {
        spec = await call("preview_openapi", { text, path: text ? null : specPath });
        specError = null;
      } catch (e) {
        spec = null;
        specError = toCommandError(e);
      } finally {
        specBusy = false;
      }
    }, 200);
  }

  async function chooseSpec() {
    const f = await pickFile({ title: "Choose OpenAPI file", filters: [{ name: "OpenAPI", extensions: ["json", "yaml", "yml"] }], readText: true });
    if (!f) return;
    if (f.text !== undefined) {
      specText = f.text;
      specPath = null;
    } else {
      specPath = f.path;
      specText = "";
    }
    previewSpec();
  }

  async function importSpec() {
    if (!spec || !projectId) return;
    specBusy = true;
    try {
      const text = specText.trim() ? specText : null;
      const res = await call("import_openapi", { projectId, text, path: text ? null : specPath });
      close();
      if (projectId !== app.activeProjectId) await app.switchProject(projectId);
      app.setTree(res.tree);
      await app.loadEnvironments();
      ui.sidebarTab = "collections";
      ui.expandAll([res.collectionId]);
      app.openCollection(res.collectionId);
      toasts.success(`Imported ${res.report.operationsCreated} requests into “${res.report.collectionName}”`);
      if (res.report.warnings.length) toasts.warning(`${res.report.warnings.length} import warning${res.report.warnings.length === 1 ? "" : "s"}: ${res.report.warnings.slice(0, 3).join("; ")}`, { timeout: 12000 });
    } catch (e) {
      specError = toCommandError(e);
    } finally {
      specBusy = false;
    }
  }

  // ───────── Link ─────────
  let linkUrl = $state("");
  let share = $state<{ kind: ShareKind; snapshot: ShareSnapshot; warnings: string[] } | null>(null);
  let linkError = $state<string | null>(null);
  let linkBusy = $state(false);
  let linkTimer: ReturnType<typeof setTimeout> | null = null;

  function previewLink() {
    if (linkTimer) clearTimeout(linkTimer);
    linkTimer = setTimeout(async () => {
      if (!linkUrl.trim()) {
        share = null;
        linkError = null;
        return;
      }
      linkBusy = true;
      try {
        share = await call("preview_share", { url: linkUrl.trim() });
        linkError = null;
      } catch (e) {
        share = null;
        const err = toCommandError(e);
        linkError = err.code === "share_gone" ? "This share link has been revoked." : err.message;
      } finally {
        linkBusy = false;
      }
    }, 300);
  }

  async function importLink() {
    if (!share || !projectId) return;
    linkBusy = true;
    try {
      const res = await call("import_share", { url: linkUrl.trim(), projectId });
      close();
      if (projectId !== app.activeProjectId) await app.switchProject(projectId);
      app.setTree(res.tree);
      await app.loadEnvironments();
      ui.sidebarTab = "collections";
      toasts.success("Imported a copy from the share link");
      for (const w of res.warnings) toasts.warning(w, { timeout: 10000 });
    } catch (e) {
      linkError = toCommandError(e).message;
    } finally {
      linkBusy = false;
    }
  }

  const shareRequests = $derived(share?.snapshot.collection?.requests ?? (share?.snapshot.request ? [share.snapshot.request] : []));

  onMount(() => {
    if (curlText) previewCurl();
    if (specPath) previewSpec();
    let un: (() => void) | null = null;
    void onFileDrop((paths) => {
      const p = paths.find((x) => /\.(json|ya?ml)$/i.test(x));
      if (!p) return;
      tab = "openapi";
      specPath = p;
      specText = "";
      previewSpec();
    }).then((u) => (un = u));
    return () => un?.();
  });

  function submit() {
    if (tab === "curl") void importCurl();
    else if (tab === "openapi") void importSpec();
    else void importLink();
  }

  const canSubmit = $derived(tab === "curl" ? !!curl && (dest === "open" || destinations.length > 0) : tab === "openapi" ? !!spec && !specBusy && !!projectId : !!share && !linkBusy && !!projectId);
  const submitLabel = $derived(tab === "curl" ? "Import" : tab === "openapi" ? (spec ? `Import ${spec.report.operationsCreated} requests` : "Import") : "Import copy");

  function onDragOver(e: DragEvent) {
    e.preventDefault();
  }
  async function onDrop(e: DragEvent) {
    e.preventDefault();
    const f = e.dataTransfer?.files?.[0];
    if (f) {
      specText = await f.text();
      specPath = null;
      previewSpec();
    }
  }
</script>

<Dialog title="Import" width={720} height={560} testid="import-dialog" onclose={close} onsubmit={() => canSubmit && submit()}>
  <div class="seg tabs-seg" role="tablist" aria-label="Import source">
    <button type="button" role="tab" aria-selected={tab === "curl"} onclick={() => (tab = "curl")} data-testid="import-tab-curl">cURL</button>
    <button type="button" role="tab" aria-selected={tab === "openapi"} onclick={() => (tab = "openapi")} data-testid="import-tab-openapi">OpenAPI</button>
    <button type="button" role="tab" aria-selected={tab === "link"} onclick={() => (tab = "link")} data-testid="import-tab-link">Link</button>
  </div>

  {#if tab === "curl"}
    <div class="cols">
      <!-- svelte-ignore a11y_autofocus -->
      <textarea
        class="input src"
        bind:value={curlText}
        oninput={previewCurl}
        placeholder={"curl -X POST 'https://api.example.com/v1/users' \\\n  -H 'Content-Type: application/json' \\\n  --data-raw '{\"name\":\"Ada\"}'"}
        aria-label="cURL command"
        spellcheck="false"
        autofocus
        data-testid="curl-input"
      ></textarea>
      <div class="preview" data-testid="curl-preview">
        {#if curlError}
          <div class="hint danger" role="alert" data-testid="curl-error"><Icon name="alert" />{curlError}</div>
        {:else if curl}
          {@const r = curl.request}
          <div class="pv-line">
            <span class="m {methodClass(r.method)}" data-testid="curl-preview-method">{methodLabel(r.method)}</span>
            <span class="mono url" title={r.url} data-testid="curl-preview-url">{r.url}</span>
          </div>
          <div class="faint small" data-testid="curl-preview-summary">{requestSummary(r)}</div>
          {#if curl.unknownFlags.length && !flagsDismissed}
            <div class="hint warn">
              <Icon name="alert" /><span class="grow">Ignored unknown flags: <span class="mono">{curl.unknownFlags.join(" ")}</span></span>
              <button class="icon-btn" type="button" aria-label="Dismiss" onclick={() => (flagsDismissed = true)}><Icon name="x" small /></button>
            </div>
          {/if}
          {#each curl.warnings as w (w)}<div class="hint warn"><Icon name="alert" />{w}</div>{/each}
          <div class="dest">
            <label class="radio"><input type="radio" name="dest" value="open" bind:group={dest} />Open as new request</label>
            <label class="radio"><input type="radio" name="dest" value="save" bind:group={dest} disabled={!destinations.length} />Save to…</label>
            {#if dest === "save"}
              <span class="select-wrap">
                <select class="input" bind:value={destKey} aria-label="Destination">
                  {#each destinations as d (d.key)}<option value={d.key}>{d.label}</option>{/each}
                </select><Icon name="chev-d" small /></span>
            {/if}
          </div>
        {:else}
          <p class="faint small">Paste a command copied from a terminal, Chrome or Firefox DevTools (“Copy as cURL (bash)”).</p>
        {/if}
      </div>
    </div>
  {:else if tab === "openapi"}
    <div class="drop" role="region" aria-label="Drop a spec file" ondragover={onDragOver} ondrop={onDrop} data-testid="openapi-drop">
      <Icon name="upload" />
      <span>Drop a .json or .yaml file, or paste below</span>
      <button class="btn btn-secondary btn-sm" type="button" onclick={chooseSpec} data-testid="openapi-choose">Choose file…</button>
      {#if specPath}<span class="mono small faint">{fileNameOf(specPath)}</span>{/if}
    </div>
    <textarea
      class="input spec"
      bind:value={specText}
      oninput={() => {
        specPath = null;
        previewSpec();
      }}
      placeholder={"openapi: 3.1.0\ninfo:\n  title: Users API\npaths: …"}
      aria-label="OpenAPI document"
      spellcheck="false"
      data-testid="openapi-input"
    ></textarea>
    <div class="spec-preview" data-testid="openapi-preview">
      {#if specError}
        <div class="row">
          <span class="chip danger-chip">{specError.code === "openapi_unsupported" ? "Unsupported format" : "Invalid document"}</span>
          <span class="small err-text" data-testid="openapi-error">{specError.message}</span>
        </div>
      {:else if spec}
        <div class="row">
          <span class="chip" data-testid="openapi-chip">{spec.spec.label}</span>
          <b>{spec.report.collectionName}</b>
          <span class="faint small">{spec.report.operationsCreated} operations</span>
        </div>
        <div class="folders">
          {#each spec.report.folders as f (f.name)}
            <div class="frow"><Icon name="folder" small /><span class="grow">{f.name}</span><span class="faint">{f.requestCount}</span></div>
          {/each}
          {#if spec.report.rootRequestCount}
            <div class="frow"><Icon name="stack" small /><span class="grow faint">Collection root</span><span class="faint">{spec.report.rootRequestCount}</span></div>
          {/if}
        </div>
        <div class="small">Environment “{spec.report.collectionName}” with <span class="mono">baseUrl = {spec.report.baseUrl || "(empty)"}</span></div>
        {#each spec.report.warnings as w (w)}<div class="hint warn"><Icon name="alert" />{w}</div>{/each}
      {:else if specBusy}
        <p class="faint small">Reading spec…</p>
      {:else}
        <p class="faint small">Swagger 2.0 and OpenAPI 3.0 / 3.1 / 3.2, JSON or YAML.</p>
      {/if}
    </div>
  {:else}
    <label class="field wide">
      <span>Share link</span>
      <!-- svelte-ignore a11y_autofocus -->
      <input class="input mono" bind:value={linkUrl} oninput={previewLink} placeholder="https://share.example.workers.dev/s/…" autofocus data-testid="link-input" />
    </label>
    {#if linkError}
      <div class="hint danger" role="alert" data-testid="link-error"><Icon name="alert" />{linkError}</div>
    {:else if share}
      <div class="share-prev" data-testid="link-preview">
        <div class="row"><Icon name={share.kind === "collection" ? "stack" : "file"} small /><b>{share.snapshot.collection?.name ?? share.snapshot.request?.name}</b><span class="faint small">{shareRequests.length} request{shareRequests.length === 1 ? "" : "s"}</span></div>
        <div class="folders">
          {#each shareRequests as r (r.id)}
            <div class="frow"><span class="m {methodClass(r.method)} mcol">{methodLabel(r.method)}</span><span class="grow ell">{r.name}</span></div>
          {/each}
        </div>
        {#if share.snapshot.environment}<div class="small faint">Includes environment “{share.snapshot.environment.name}”</div>{/if}
        {#each share.warnings as w (w)}<div class="hint warn"><Icon name="alert" />{w}</div>{/each}
      </div>
    {:else if linkBusy}
      <p class="faint small">Loading preview…</p>
    {:else}
      <p class="faint small">Paste a ReqHarbor share link to import an editable copy into a project.</p>
    {/if}
  {/if}

  {#snippet footer()}
    {#if (tab === "openapi" && spec) || (tab === "link" && share)}
      <!-- DR-008: destination project always visible next to the primary action -->
      <label class="into">
        <span class="faint">Into</span>
        <span class="select-wrap">
          <select class="input" bind:value={projectId} aria-label="Import into project" data-testid="import-project">
            {#each app.projects as p (p.id)}<option value={p.id}>{p.name}</option>{/each}
          </select><Icon name="chev-d" small /></span>
      </label>
    {/if}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn btn-primary" type="button" disabled={!canSubmit} onclick={submit} data-testid="import-submit">{submitLabel}</button>
  {/snippet}
</Dialog>

<style>
  .tabs-seg {
    align-self: flex-start;
  }
  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-5);
    flex: 1;
    min-height: 0;
  }
  .src {
    min-height: 160px;
    height: 100%;
    resize: none;
  }
  .preview {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    min-width: 0;
    overflow: auto;
  }
  .pv-line {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    min-width: 0;
  }
  .pv-line .m {
    font-size: var(--fs-md);
  }
  .url {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .dest {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
    margin-top: var(--sp-2);
  }
  .radio {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
  }
  .drop {
    border: 1px dashed var(--border-strong);
    border-radius: var(--r-md);
    padding: var(--sp-5);
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    color: var(--text-2);
    flex-wrap: wrap;
  }
  .spec {
    min-height: 110px;
    flex: none;
  }
  .spec-preview,
  .share-prev {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
  }
  .row {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    flex-wrap: wrap;
  }
  .chip {
    display: inline-flex;
    align-items: center;
    height: 20px;
    padding: 0 var(--sp-4);
    border-radius: var(--r-pill);
    background: var(--accent-soft);
    color: var(--accent-fg);
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
  }
  .danger-chip {
    background: var(--danger-soft);
    color: var(--danger);
  }
  .err-text {
    color: var(--danger);
  }
  .folders {
    max-height: 200px;
    overflow: auto;
    background: var(--bg-input);
    border-radius: var(--r-md);
    padding: var(--sp-2) var(--sp-4);
  }
  .frow {
    height: 26px;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .mcol {
    width: var(--w-method-col);
    flex: none;
  }
  .ell {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .into {
    margin-right: auto;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  .into .select-wrap {
    width: 180px;
  }
  .wide {
    max-width: none;
  }
</style>

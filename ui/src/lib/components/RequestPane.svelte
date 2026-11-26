<script lang="ts">
  import { tick, untrack } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { shortcutText } from "../util/shortcuts";
  import { looksLikeCurl } from "../util/request";
  import type { EditorController } from "../cm/editor";
  import MethodSelect from "./MethodSelect.svelte";
  import VarInput from "./VarInput.svelte";
  import KvTable from "./KvTable.svelte";
  import AuthForm from "./AuthForm.svelte";
  import BodyEditor from "./BodyEditor.svelte";
  import Icon from "./Icon.svelte";

  type ReqTab = "query" | "headers" | "auth" | "body" | "tests" | "prerun";
  let tab = $state<ReqTab>("query");
  let lastId = "";
  let urlEl = $state<HTMLInputElement>();
  let bodyEd = $state<BodyEditor>();
  let bodyCm = $state<EditorController | null>(null);
  let settingsOpen = $state(false);
  let settingsBtn = $state<HTMLButtonElement>();
  let settingsEl = $state<HTMLDivElement>();
  let menuBtn = $state<HTMLButtonElement>();
  let renaming = $state(false);
  let nameDraft = $state("");

  const r = $derived(app.request);
  const ix = $derived(app.index);
  const collection = $derived(r.collectionId ? (ix?.collectionById.get(r.collectionId) ?? null) : null);
  const crumbs = $derived.by(() => {
    const out: string[] = [];
    let f = r.folderId ? ix?.folderById.get(r.folderId) : undefined;
    while (f) {
      out.unshift(f.name);
      f = f.parentFolderId ? ix?.folderById.get(f.parentFolderId) : undefined;
    }
    return out;
  });
  const queryCount = $derived(r.params.filter((p) => p.enabled).length);
  const headerCount = $derived(r.headers.filter((h) => h.enabled).length);
  const hasAuthHeader = $derived(r.headers.some((h) => h.enabled && h.key.trim().toLowerCase() === "authorization"));
  const queryHint = $derived(r.method === "QUERY" && r.body.type === "none");
  const inflight = $derived(app.currentInflight);

  // Pick a sensible tab when switching requests.
  $effect(() => {
    const id = r.id;
    if (id === lastId) return;
    lastId = id;
    tab = r.body.type !== "none" ? "body" : "query";
  });

  $effect(() => {
    if (ui.focusUrlTick) untrack(() => void tick().then(() => urlEl?.focus()));
  });

  $effect(() => {
    if (ui.renameRequestTick) untrack(() => !app.isScratch && startRename());
  });

  $effect(() => {
    if (ui.requestSettingsTick) settingsOpen = true;
  });

  $effect(() => {
    if (ui.formatTick) untrack(() => void format());
  });

  export function focusUrl() {
    urlEl?.focus();
    urlEl?.select();
  }

  export async function format() {
    tab = "body";
    await tick();
    await bodyEd?.format();
  }

  function startRename() {
    nameDraft = r.name;
    renaming = true;
  }

  function commitRename() {
    if (!renaming) return;
    renaming = false;
    if (nameDraft.trim() && nameDraft.trim() !== r.name) void app.rename("request", r.id, nameDraft);
  }

  async function onUrlPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text/plain") ?? "";
    if (looksLikeCurl(text)) {
      e.preventDefault();
      const ok = await app.importCurlInPlace(text);
      if (!ok) app.setUrl(text);
    }
  }

  function openMenu() {
    if (!menuBtn) return;
    const saved = !app.isScratch;
    ui.openMenu(
      [
        { label: "Copy as cURL", shortcut: shortcutText("Mod+Shift+C", app.platform), action: () => void app.copyCurl(), testid: "menu-copy-curl" },
        { label: "Export as cURL…", action: () => (ui.exportCurl = { anchor: menuBtn ?? null }), testid: "menu-export-curl" },
        { label: "Share…", disabled: !saved, action: () => (ui.shareDialog = { kind: "request", entityId: r.id, name: r.name }), testid: "menu-share" },
        { separator: true },
        { label: saved ? "Rename" : "Save to collection…", shortcut: saved ? "F2" : shortcutText("Mod+S", app.platform), action: () => (saved ? startRename() : (ui.picker = { kind: "saveRequest" })) },
        { label: "Duplicate", disabled: !saved, action: () => void app.duplicate("request", r.id) },
        { label: "Move to…", disabled: !saved, action: () => (ui.picker = { kind: "moveRequest", requestId: r.id }) },
        { separator: true },
        { label: "Delete", danger: true, disabled: !saved, action: () => void app.deleteNode("request", r.id) },
      ],
      menuBtn,
      { alignRight: true },
    );
  }

  function tabKey(e: KeyboardEvent, tabs: ReqTab[]) {
    const i = tabs.indexOf(tab);
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    tab = tabs[(i + d + tabs.length) % tabs.length];
    void tick().then(() => document.querySelector<HTMLElement>(`[data-testid="req-tab-${tab}"]`)?.focus());
  }

  const TABS: ReqTab[] = ["query", "headers", "auth", "body", "tests", "prerun"];
</script>

<svelte:window
  onmousedown={(e) => {
    if (settingsOpen && settingsEl && !settingsEl.contains(e.target as Node) && !settingsBtn?.contains(e.target as Node)) settingsOpen = false;
  }}
/>

<main class="pane req" aria-label="Request" data-testid="request-pane">
  <div class="row1">
    <div class="crumbs grow" data-testid="breadcrumb">
      {#if app.isScratch}
        <b>{r.name || "Untitled request"}</b><span class="faint notsaved">not saved</span>
      {:else}
        {#if collection}<span>{collection.name}</span><Icon name="chev-r" small />{/if}
        {#each crumbs as c, i (i)}<span>{c}</span><Icon name="chev-r" small />{/each}
        {#if renaming}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="rename input"
            bind:value={nameDraft}
            aria-label="Request name"
            autofocus
            onkeydown={(e) => {
              if (e.key === "Enter") commitRename();
              else if (e.key === "Escape") {
                e.stopPropagation();
                renaming = false;
              }
            }}
            onblur={commitRename}
          />
        {:else}
          <button class="name-btn" type="button" ondblclick={startRename} title="Double-click to rename">{r.name}</button>
        {/if}
        {#if app.dirty}<span class="unsaved" title="Unsaved changes" data-testid="dirty-dot"></span>{/if}
      {/if}
    </div>
    <button
      class="btn btn-ghost btn-sm"
      type="button"
      disabled={!app.isScratch && !app.dirty}
      onclick={() => void app.save()}
      title={app.isScratch ? "Save to a collection" : "Save request"}
      data-testid="save-request">Save<span class="kbd-inline">{shortcutText("Mod+S", app.platform)}</span></button
    >
    <button class="icon-btn" type="button" aria-label="More actions" aria-haspopup="menu" bind:this={menuBtn} onclick={openMenu} data-testid="request-menu"><Icon name="more" /></button>
  </div>

  <div class="row2">
    <div class="reqbar" class:invalid={!!app.sendBlocked && app.sendBlocked === "Enter a URL"}>
      <MethodSelect method={r.method} onchange={(m) => (app.request.method = m)} />
      <VarInput
        variant="url"
        lookup={app.lookup}
        value={r.url}
        placeholder="Enter a URL or paste a cURL command"
        ariaLabel="URL"
        testid="url-input"
        bind:inputEl={urlEl}
        oninput={(v) => app.setUrl(v)}
        onpaste={onUrlPaste}
        onkeydown={(e) => {
          if (e.key === "Enter" && !e.metaKey && !e.ctrlKey) {
            e.preventDefault();
            void app.send();
          }
        }}
      />
    </div>
    <button
      class="btn send"
      class:btn-primary={!inflight}
      class:btn-secondary={!!inflight}
      type="button"
      disabled={!inflight && !r.url.trim()}
      title={app.sendBlocked ?? (inflight ? "Cancel request" : `Send  ${shortcutText("Mod+Enter", app.platform)}`)}
      onclick={() => void app.sendOrCancel()}
      data-testid="send-button"
    >
      {#if inflight}<span class="spinner" aria-hidden="true"></span>Cancel{:else}Send{/if}
    </button>
  </div>
  {#if app.sendBlocked}
    <div class="blocked" role="alert" data-testid="send-blocked"><Icon name="alert" small />{app.sendBlocked}</div>
  {/if}

  <div class="tabs">
    <div class="tablist" role="tablist" aria-label="Request">
    {#each TABS as t (t)}
      <button
        class="tab"
        role="tab"
        type="button"
        aria-selected={tab === t}
        tabindex={tab === t ? 0 : -1}
        data-testid="req-tab-{t}"
        onclick={() => (tab = t)}
        onkeydown={(e) => tabKey(e, TABS)}
      >
        {#if t === "query"}Query{#if queryCount}<sup data-testid="badge-query">{queryCount}</sup>{/if}
        {:else if t === "headers"}Headers{#if headerCount}<sup data-testid="badge-headers">{headerCount}</sup>{/if}
        {:else if t === "auth"}Auth{#if r.auth.type !== "none"}<span class="dot" title={r.auth.type}></span>{/if}
        {:else if t === "body"}Body{#if r.body.type !== "none"}<span class="dot" title={r.body.type}></span>{:else if queryHint}<span class="dot warn" title="QUERY requests normally carry a body"></span>{/if}
        {:else if t === "tests"}Tests
        {:else}Pre Run{/if}
      </button>
    {/each}
    </div>
    <span class="end">
      <button
        class="icon-btn"
        type="button"
        bind:this={settingsBtn}
        aria-label="Request settings: redirects, timeout, TLS"
        title="Request settings: redirects, timeout, TLS"
        aria-expanded={settingsOpen}
        data-testid="request-settings"
        onclick={() => (settingsOpen = !settingsOpen)}><Icon name="gear" /></button
      >
      {#if settingsOpen}
        <div class="settings menu pop" bind:this={settingsEl} role="dialog" aria-label="Request settings" data-testid="request-settings-popover">
          <label class="checkbox">
            <input type="checkbox" checked={r.settings.followRedirects} onchange={(e) => (app.request.settings.followRedirects = e.currentTarget.checked)} />Follow redirects
          </label>
          <label class="field">
            <span>Timeout (ms) — 0 = no timeout</span>
            <input
              class="input mono"
              type="number"
              min="0"
              step="1000"
              value={r.settings.timeoutMs}
              data-testid="timeout-input"
              oninput={(e) => (app.request.settings.timeoutMs = Math.max(0, Math.round(Number(e.currentTarget.value) || 0)))}
            />
            {#if r.settings.timeoutMs === 0}<span class="faint small">No timeout: the request waits until it completes or is cancelled.</span>{/if}
          </label>
          <label class="checkbox">
            <input type="checkbox" checked={r.settings.verifyTls} onchange={(e) => (app.request.settings.verifyTls = e.currentTarget.checked)} />Verify TLS certificates
          </label>
          {#if !r.settings.verifyTls}
            <div class="hint warn"><Icon name="alert" />Self-signed and invalid certificates will be accepted.</div>
          {/if}
        </div>
      {/if}
    </span>
  </div>

  <div class="panel" role="tabpanel" data-testid="req-panel-{tab}">
    {#if tab === "query"}
      <div class="scroll">
        <KvTable rows={app.request.params} kind="query" lookup={app.lookup} testid="query" onchange={() => app.syncUrlFromParams()} />
      </div>
    {:else if tab === "headers"}
      <div class="scroll">
        <KvTable rows={app.request.headers} kind="headers" lookup={app.lookup} testid="headers" />
      </div>
    {:else if tab === "auth"}
      <div class="scroll">
        <AuthForm
          auth={app.request.auth}
          lookup={app.lookup}
          inheritFrom={collection}
          authHeaderOverride={hasAuthHeader}
          oneditcollection={collection ? () => app.openCollection(collection.id) : undefined}
        />
      </div>
    {:else if tab === "body"}
      <BodyEditor bind:this={bodyEd} bind:editor={bodyCm} />
    {:else}
      <div class="empty soon" data-testid="coming-soon">
        <h3>Coming soon</h3>
        <p>Scripts and assertions are on the roadmap.</p>
      </div>
    {/if}
  </div>
</main>

<style>
  .req {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    background: var(--bg-pane);
    height: 100%;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-3);
    font-size: var(--fs-sm);
    white-space: nowrap;
    overflow: hidden;
  }
  .crumbs b {
    color: var(--text-1);
    font-weight: var(--fw-medium);
    font-size: var(--fs-md);
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name-btn {
    border: 0;
    background: none;
    padding: 0;
    color: var(--text-1);
    font-weight: var(--fw-medium);
    font-size: var(--fs-md);
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: default;
  }
  .notsaved {
    margin-left: var(--sp-3);
  }
  .rename {
    height: 22px;
    max-width: 260px;
  }
  .unsaved {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-2);
    margin-left: var(--sp-2);
    flex: none;
  }
  .reqbar {
    display: flex;
    flex: 1;
    min-width: 0;
    height: var(--h-control-lg);
    border-radius: var(--r-sm);
    background: var(--bg-input);
    border: 1px solid transparent;
  }
  .reqbar:focus-within {
    border-color: var(--border-focus);
  }
  .reqbar.invalid {
    border-color: var(--danger);
  }
  .send {
    height: var(--h-control-lg);
    width: 88px; /* DR-005: Send and Cancel share one fixed width */
    padding: 0;
    justify-content: center;
    flex: none;
  }
  .spinner {
    width: 12px;
    height: 12px;
    border: 1.5px solid var(--text-3);
    border-top-color: var(--text-1);
    border-radius: 50%;
    animation: spin 0.8s linear infinite;
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
  .blocked {
    margin: -6px var(--sp-5) var(--sp-3);
    color: var(--danger);
    font-size: var(--fs-sm);
    display: flex;
    gap: var(--sp-2);
    align-items: center;
  }
  .tabs .end {
    position: relative;
  }
  .settings {
    position: absolute;
    right: 0;
    top: calc(100% + 4px);
    z-index: var(--z-popover);
    width: 280px;
    padding: var(--sp-5);
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .panel {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .scroll {
    flex: 1;
    overflow: auto;
    padding: var(--sp-4) var(--sp-5);
  }
  .soon {
    flex: 1;
  }
</style>

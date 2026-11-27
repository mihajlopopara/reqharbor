<script lang="ts">
  import { tick, untrack } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { shortcutText } from "../util/shortcuts";
  import { looksLikeCurl, NEEDS_NEWER_VERSION, readOnlyReason, REQUIRES_NEWER } from "../util/request";
  import type { EditorController } from "../cm/editor";
  import MethodSelect from "./MethodSelect.svelte";
  import VarInput from "./VarInput.svelte";
  import KvTable from "./KvTable.svelte";
  import AuthForm from "./AuthForm.svelte";
  import BodyEditor from "./BodyEditor.svelte";
  import Icon from "./Icon.svelte";
  import WsComposer from "./WsComposer.svelte";
  import WsSavedMessages from "./WsSavedMessages.svelte";
  import WsSettings from "./WsSettings.svelte";
  import { ws } from "../stores/ws.svelte";
  import { grpc } from "../stores/grpc.svelte";
  import { isSocketIo } from "../util/ws";
  import { GRPC_URL_PLACEHOLDER, sendsMessages, tryTarget } from "../util/grpc";
  import { prettyJson } from "../util/json";
  import { loadGrpcUi } from "./grpc/load";
  import type { Request } from "../api/types";
  import { call, type EditedBy } from "../api";
  import { editedByText } from "../util/teams";

  type ReqTab = "query" | "headers" | "auth" | "body" | "tests" | "prerun" | "message" | "saved" | "settings" | "proto";
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
  /** AC-WS-1: a WebSocket / Socket.IO request gets the connect bar and its own tabs. */
  const isWs = $derived(r.kind === "ws");
  const sio = $derived(isSocketIo(r));
  const wsSession = $derived(isWs ? ws.session(r.id) : null);
  const wsState = $derived(wsSession?.state ?? "idle");
  const wsLive = $derived(wsState === "connecting" || wsState === "open" || wsState === "closing");
  /** AC-GRPC-1: a gRPC request gets the method picker, Invoke / Connect and its own tabs. */
  const isGrpc = $derived(r.kind === "grpc");
  const grpcKind = $derived(isGrpc ? app.grpcCallKind() : null);
  const grpcLive = $derived(isGrpc && app.grpcLive());
  const grpcSession = $derived(isGrpc ? ws.session(r.id) : null);
  const grpcConnecting = $derived(grpcSession?.transport === "grpc" && grpcSession.state === "connecting");
  const grpcPathHint = $derived(isGrpc && !!tryTarget(r.url)?.extra);
  /** The lazy gRPC UI chunk, once a gRPC request was opened. */
  let grpcUi = $state.raw<Awaited<ReturnType<typeof loadGrpcUi>> | null>(null);
  $effect(() => {
    if (isGrpc && !grpcUi) void loadGrpcUi().then((m) => (grpcUi = m));
  });
  // AC-GRPC-3/4: the cached / compiled services on open and whenever their inputs change.
  const grpcLookupKey = $derived.by(() => {
    if (!isGrpc) return "";
    const g = r.grpc;
    const envs = app.environments.filter((e) => e.projectId === null || e.id === app.activeEnv?.id).map((e) => e.variables);
    return JSON.stringify([r.id, r.url, g?.protoSource, g?.protoFiles, g?.importPaths, envs, app.activeProjectId]);
  });
  $effect(() => {
    if (!grpcLookupKey) return;
    const t = setTimeout(() => untrack(() => void grpc.refreshCached($state.snapshot(app.request) as Request, app.activeProjectId)), 250);
    return () => clearTimeout(t);
  });
  const savedCount = $derived(isGrpc ? (r.grpc?.messages.length ?? 0) : (r.ws?.messages.length ?? 0));
  /** AC-FWD-3/4: a request from a newer ReqHarbor is view-only. */
  const ro = $derived(app.readOnly);
  /** Why (from the editor copy, else the saved copy), e.g. "This is a “grpc” request". */
  const roReason = $derived(
    ro ? (readOnlyReason(r, app.appVersion) ?? readOnlyReason(app.index?.requestById.get(r.id) ?? r, app.appVersion) ?? "It was made with a newer version of ReqHarbor") : null,
  );

  /** "Edited by Sam · 3h" (UX §6.5): team projects only, not my own last edit. */
  let editedBy = $state<EditedBy | null>(null);
  $effect(() => {
    const id = r.id;
    const saved = app.isScratch ? null : app.index?.requestById.get(id);
    void saved?.updatedAt;
    void app.sync.lastSyncAt;
    if (!saved || app.locationById.get(app.activeProjectId ?? "")?.kind !== "team") {
      editedBy = null;
      return;
    }
    call("request_edited_by", { requestId: id }).then(
      (e) => (editedBy = id === r.id ? e : editedBy),
      () => (editedBy = null),
    );
  });
  const edited = $derived(editedBy ? editedByText(editedBy, app.now) : null);
  /** UX §6.2: a viewer's edits are scratch; Save explains instead of saving. */
  const VIEWER_SAVE = "Viewers can't change team projects. Duplicate to personal to experiment.";

  // Pick a sensible tab when switching requests (or between HTTP, WebSocket and gRPC).
  let lastWs = "";
  $effect(() => {
    const id = r.id;
    const w = isWs ? "ws" : isGrpc ? "grpc" : "";
    if (id === lastId && w === lastWs) return;
    lastId = id;
    lastWs = w;
    if (w) tab = "message";
    else tab = app.readOnly || r.body.type !== "none" ? "body" : "query";
  });

  $effect(() => {
    if (ui.focusUrlTick) untrack(() => void tick().then(() => urlEl?.focus()));
  });

  $effect(() => {
    if (ui.renameRequestTick) untrack(() => !app.isScratch && !app.readOnly && startRename());
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
    if (isWs) return;
    if (isGrpc) {
      tab = "message";
      const g = app.request.grpc;
      const p = g ? prettyJson(g.message || "{}") : null;
      if (g && p !== null) g.message = p;
      return;
    }
    tab = "body";
    await tick();
    await bodyEd?.format();
  }

  function startRename() {
    if (ro) return;
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
        ...(isGrpc ? [{ label: "Copy as grpcurl", disabled: ro, shortcut: shortcutText("Mod+Shift+C", app.platform), action: () => void app.copyCurl(), testid: "menu-copy-grpcurl" }] : []),
        {
          label: "Copy as cURL",
          disabled: ro || isWs || isGrpc,
          reason: isWs ? "WebSocket requests have no cURL form" : isGrpc ? "gRPC requests have no cURL form" : undefined,
          shortcut: isGrpc ? undefined : shortcutText("Mod+Shift+C", app.platform),
          action: () => void app.copyCurl(),
          testid: "menu-copy-curl",
        },
        {
          label: "Export as cURL…",
          disabled: ro || isWs || isGrpc,
          reason: isWs ? "WebSocket requests have no cURL form" : isGrpc ? "gRPC requests have no cURL form" : undefined,
          action: () => (ui.exportCurl = { anchor: menuBtn ?? null }),
          testid: "menu-export-curl",
        },
        { label: "Share…", disabled: !saved || ro, action: () => (ui.shareDialog = { kind: "request", entityId: r.id, name: r.name }), testid: "menu-share" },
        { separator: true },
        { label: saved ? "Rename" : "Save to collection…", disabled: ro, shortcut: saved ? "F2" : shortcutText("Mod+S", app.platform), action: () => (saved ? startRename() : (ui.picker = { kind: "saveRequest" })) },
        { label: "Duplicate", disabled: !saved || ro, action: () => void app.duplicate("request", r.id) },
        { label: "Move to…", disabled: !saved || ro, action: () => (ui.picker = { kind: "moveRequest", requestId: r.id }) },
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

  const HTTP_TABS: ReqTab[] = ["query", "headers", "auth", "body", "tests", "prerun"];
  const WS_TABS: ReqTab[] = ["message", "saved", "headers", "auth", "query", "settings"];
  const GRPC_TABS: ReqTab[] = ["message", "saved", "headers", "auth", "proto", "settings"];
  const TABS = $derived(isWs ? WS_TABS : isGrpc ? GRPC_TABS : HTTP_TABS);
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
          <button class="name-btn" type="button" ondblclick={startRename} title={ro ? REQUIRES_NEWER : "Double-click to rename"}>{r.name}</button>
        {/if}
        {#if app.dirty}<span class="unsaved" title="Unsaved changes" data-testid="dirty-dot"></span>{/if}
      {/if}
    </div>
    {#if edited}<span class="edited-by" title={edited.title} data-testid="edited-by">{edited.label}</span>{/if}
    {#if app.viewerHere && !app.isScratch}
      <button class="btn btn-ghost btn-sm" type="button" aria-disabled="true" title={VIEWER_SAVE} onclick={() => void app.save()} data-testid="save-request">Save<span class="kbd-inline">{shortcutText("Mod+S", app.platform)}</span></button>
    {:else}
      <button
        class="btn btn-ghost btn-sm"
        type="button"
        disabled={ro || (!app.isScratch && !app.dirty)}
        onclick={() => void app.save()}
        title={ro ? `${NEEDS_NEWER_VERSION}: it can't be saved here` : app.isScratch ? "Save to a collection" : "Save request"}
        data-testid="save-request">Save<span class="kbd-inline">{shortcutText("Mod+S", app.platform)}</span></button
      >
    {/if}
    <button class="icon-btn" type="button" aria-label="More actions" aria-haspopup="menu" bind:this={menuBtn} onclick={openMenu} data-testid="request-menu"><Icon name="more" /></button>
  </div>

  {#if ro}
    <div class="readonly" role="status" data-testid="read-only-banner">
      <Icon name="alert" small /><b>{REQUIRES_NEWER}</b><span>{NEEDS_NEWER_VERSION}. You can view and delete it here; editing, saving and sending need an update.</span>
    </div>
  {/if}

  <div class="row2">
    <div class="reqbar" class:invalid={!!app.sendBlocked && app.sendBlocked === "Enter a URL"}>
      <fieldset class="ro-fs" disabled={ro}>
      <MethodSelect method={r.method} kind={isWs ? (sio ? "sio" : "ws") : isGrpc ? "grpc" : "http"} onchange={(m) => (app.request.method = m)} onkind={(k, m) => app.setKind(k, m)} />
      <VarInput
        variant="url"
        lookup={app.lookup}
        value={r.url}
        placeholder={isWs ? (sio ? "Enter a Socket.IO server URL, e.g. http://localhost:3000" : "Enter a ws:// or wss:// URL") : isGrpc ? GRPC_URL_PLACEHOLDER : "Enter a URL or paste a cURL command"}
        ariaLabel="URL"
        testid="url-input"
        disabled={ro}
        bind:inputEl={urlEl}
        oninput={(v) => app.setUrl(v)}
        onpaste={isWs || isGrpc ? undefined : onUrlPaste}
        onkeydown={(e) => {
          if (e.key === "Enter" && !e.metaKey && !e.ctrlKey) {
            e.preventDefault();
            if (isWs) {
              if (!wsLive) void app.wsConnect();
            } else if (isGrpc) {
              if (!grpcLive) void app.grpcRun();
            } else void app.send();
          }
        }}
      />
      {#if isGrpc && grpcUi}
        <grpcUi.GrpcMethodPicker onuseFiles={() => {
          if (app.request.grpc) app.request.grpc.protoSource = "files";
          tab = "proto";
        }} />
      {/if}
      </fieldset>
    </div>
    {#if isGrpc}
    <button
      class="btn send"
      class:btn-primary={!grpcLive}
      class:btn-secondary={grpcLive}
      type="button"
      disabled={!grpcLive && (ro || !r.url.trim())}
      title={ro ? NEEDS_NEWER_VERSION : (app.sendBlocked ?? (grpcLive ? "Cancel the call" : `${sendsMessages(grpcKind) ? "Connect" : "Invoke"}  ${shortcutText("Mod+Enter", app.platform)}`))}
      onclick={() => void app.grpcRunOrCancel()}
      data-testid="grpc-invoke"
      data-state={grpcLive ? "live" : "idle"}
    >
      {#if grpcLive}{#if grpcConnecting || app.currentInflight}<span class="spinner" aria-hidden="true"></span>{/if}Cancel{:else if sendsMessages(grpcKind)}Connect{:else}Invoke{/if}
    </button>
    {:else if isWs}
    <button
      class="btn send"
      class:btn-primary={!wsLive}
      class:btn-secondary={wsLive}
      type="button"
      disabled={wsState === "closing" || (!wsLive && (ro || !r.url.trim()))}
      title={ro ? NEEDS_NEWER_VERSION : (app.sendBlocked ?? (wsState === "connecting" ? "Stop connecting" : wsState === "open" ? "Close the connection" : "Connect"))}
      onclick={() => void app.wsConnectOrDisconnect()}
      data-testid="ws-connect"
      data-state={wsState}
    >
      {#if wsState === "connecting"}<span class="spinner" aria-hidden="true"></span>Cancel{:else if wsState === "open"}Disconnect{:else if wsState === "closing"}Closing…{:else}Connect{/if}
    </button>
    {:else}
    <button
      class="btn send"
      class:btn-primary={!inflight}
      class:btn-secondary={!!inflight}
      type="button"
      disabled={!inflight && (ro || !r.url.trim())}
      title={ro ? NEEDS_NEWER_VERSION : (app.sendBlocked ?? (inflight ? "Cancel request" : `Send  ${shortcutText("Mod+Enter", app.platform)}`))}
      onclick={() => void app.sendOrCancel()}
      data-testid="send-button"
    >
      {#if inflight}<span class="spinner" aria-hidden="true"></span>Cancel{:else}Send{/if}
    </button>
    {/if}
  </div>
  {#if app.sendBlocked}
    <div class="blocked" role="alert" data-testid="send-blocked"><Icon name="alert" small />{app.sendBlocked}</div>
  {:else if grpcPathHint}
    <div class="blocked info" data-testid="grpc-path-hint"><Icon name="info" small />gRPC uses only scheme, host and port</div>
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
        {:else if t === "headers"}{isGrpc ? "Metadata" : "Headers"}{#if headerCount}<sup data-testid="badge-headers">{headerCount}</sup>{/if}
        {:else if t === "auth"}Auth{#if r.auth.type !== "none"}<span class="dot" title={r.auth.type}></span>{/if}
        {:else if t === "body"}Body{#if r.body.type !== "none"}<span class="dot" title={r.body.type}></span>{:else if queryHint}<span class="dot warn" title="QUERY requests normally carry a body"></span>{/if}
        {:else if t === "tests"}Tests
        {:else if t === "message"}Message
        {:else if t === "saved"}Saved messages{#if savedCount}<sup data-testid="badge-saved">{savedCount}</sup>{/if}
        {:else if t === "settings"}Settings
        {:else if t === "proto"}Proto
        {:else}Pre Run{/if}
      </button>
    {/each}
    </div>
    <span class="end">
      {#if !isWs && !isGrpc}
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
          <fieldset class="ro-fs" disabled={ro}>
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
          </fieldset>
        </div>
      {/if}
      {/if}
    </span>
  </div>

  <div class="panel" role="tabpanel" data-testid="req-panel-{tab}">
    <fieldset class="ro-fs" disabled={ro}>
    {#if ro && (tab === "body" || tab === "message")}
      <div class="empty newer" data-testid="body-needs-newer">
        <h3>{NEEDS_NEWER_VERSION}</h3>
        <p>
          <span data-testid="read-only-reason">{roReason}</span>, which this version can’t edit. It’s kept intact and still syncs. Update ReqHarbor to open it.
        </p>
      </div>
    {:else if tab === "query"}
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
    {:else if isGrpc && (tab === "message" || tab === "saved" || tab === "proto" || tab === "settings")}
      {#if !grpcUi}
        <div class="empty"><p class="faint">Loading…</p></div>
      {:else if tab === "message"}
        <grpcUi.GrpcMessage />
      {:else if tab === "saved"}
        <grpcUi.GrpcSavedMessages onload={() => (tab = "message")} />
      {:else if tab === "proto"}
        <grpcUi.GrpcProto />
      {:else}
        <grpcUi.GrpcSettings />
      {/if}
    {:else if tab === "message"}
      <WsComposer />
    {:else if tab === "saved"}
      <WsSavedMessages onload={() => (tab = "message")} />
    {:else if tab === "settings"}
      <WsSettings />
    {:else}
      <div class="empty soon" data-testid="coming-soon">
        <h3>Coming soon</h3>
        <p>Scripts and assertions are on the roadmap.</p>
      </div>
    {/if}
    </fieldset>
  </div>
</main>

<style>
  .edited-by {
    color: var(--text-3);
    font-size: 11.5px;
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
  @media (max-width: 900px) {
    .edited-by {
      display: none;
    }
  }
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
  .blocked.info {
    color: var(--text-3);
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
  /* Disabling wrappers for view-only requests (AC-FWD-3): no box of their own. */
  fieldset {
    border: 0;
    margin: 0;
    padding: 0;
    min-width: 0;
  }
  .ro-fs {
    display: contents;
  }
  .readonly {
    margin: 0 var(--sp-5) var(--sp-3);
    padding: var(--sp-3) var(--sp-4);
    border-radius: var(--r-sm);
    background: var(--bg-input);
    color: var(--text-2);
    font-size: var(--fs-sm);
    display: flex;
    gap: var(--sp-3);
    align-items: center;
    flex-wrap: wrap;
  }
  .readonly b {
    color: var(--warning);
    font-weight: var(--fw-medium);
  }
  .newer {
    flex: 1;
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

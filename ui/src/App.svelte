<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "./lib/stores/app.svelte";
  import { ui } from "./lib/stores/ui.svelte";
  import { matchShortcut, type ShortcutAction } from "./lib/util/shortcuts";
  import { onFileDrop } from "./lib/api/platform";
  import { toasts } from "./lib/stores/toasts.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import RequestPane from "./lib/components/RequestPane.svelte";
  import ResponsePane from "./lib/components/ResponsePane.svelte";
  import CollectionSettings from "./lib/components/CollectionSettings.svelte";
  import EnvEditor from "./lib/components/EnvEditor.svelte";
  import Splitter from "./lib/components/Splitter.svelte";
  import MenuHost from "./lib/components/MenuHost.svelte";
  import Toasts from "./lib/components/Toasts.svelte";
  import ConfirmDialog from "./lib/dialogs/ConfirmDialog.svelte";
  import EnvPicker from "./lib/dialogs/EnvPicker.svelte";
  import SyncPrompts from "./lib/components/SyncPrompts.svelte";

  // Rarely used dialogs are code-split.
  const ImportDialog = () => import("./lib/dialogs/ImportDialog.svelte");
  const ShareDialog = () => import("./lib/dialogs/ShareDialog.svelte");
  const SettingsDialog = () => import("./lib/dialogs/SettingsDialog.svelte");
  const PickerDialog = () => import("./lib/dialogs/PickerDialog.svelte");
  const ExportCurl = () => import("./lib/dialogs/ExportCurl.svelte");
  const NewProjectDialog = () => import("./lib/dialogs/NewProjectDialog.svelte");
  const SyncLocationDialog = () => import("./lib/dialogs/SyncLocationDialog.svelte");
  const FirstSyncDialog = () => import("./lib/dialogs/FirstSyncDialog.svelte");
  const MergeLegacyDialog = () => import("./lib/dialogs/MergeLegacyDialog.svelte");
  const SpaceNoticeDialog = () => import("./lib/dialogs/SpaceNoticeDialog.svelte");

  const SIDEBAR = { def: 300, min: 220, max: 480, snap: 160, rail: 44 };
  const PANE_MIN = 360;
  const PANE_MIN_H = 200;

  let winW = $state(1280);
  let mainW = $state(980);
  let mainH = $state(760);
  let reqPane = $state<{ focusUrl(): void }>();
  let dragStart = 0;

  const fullSidebarW = $derived(Math.min(SIDEBAR.max, Math.max(SIDEBAR.min, app.settings.sidebarWidth || SIDEBAR.def)));
  // DR-001: when the main area would drop below PANE_MIN (narrow window or ⌘+ zoom), show the rail without persisting it.
  const autoRail = $derived(!app.settings.sidebarCollapsed && winW - fullSidebarW - 1 < PANE_MIN);
  const collapsed = $derived(app.settings.sidebarCollapsed || autoRail);
  const sidebarW = $derived(collapsed ? SIDEBAR.rail : fullSidebarW);
  const stacked = $derived(app.settings.layoutMode === "stacked" || (app.settings.layoutMode !== "side" && mainW < 760));
  const ratio = $derived(Math.min(0.9, Math.max(0.1, app.settings.splitRatio || 0.5)));
  const reqSize = $derived.by(() => {
    const total = (stacked ? mainH : mainW) - 1;
    const min = stacked ? PANE_MIN_H : PANE_MIN;
    if (total < min * 2) return Math.round(total / 2);
    return Math.round(Math.min(total - min, Math.max(min, total * ratio)));
  });

  // theme: System removes data-theme (follows OS), Dark/Light force it
  $effect(() => {
    const t = app.settings.theme;
    if (t === "dark" || t === "light") document.documentElement.setAttribute("data-theme", t);
    else document.documentElement.removeAttribute("data-theme");
  });

  onMount(() => {
    void app.init();
    // OS file drops (Tauri): OpenAPI specs open the import dialog; other files feed a Binary body.
    let unlisten: (() => void) | null = null;
    void onFileDrop((paths) => {
      if (ui.importDialog || !paths.length) return;
      const spec = paths.find((p) => /\.(json|ya?ml)$/i.test(p));
      if (app.middle.kind === "request" && app.request.body.type === "binary") {
        app.request.body.binaryPath = paths[0];
        toasts.info("File attached as binary body");
      } else if (spec) ui.importDialog = { tab: "openapi", path: spec };
    }).then((u) => (unlisten = u));
    return () => {
      unlisten?.();
      app.destroy();
    };
  });

  function sidebarDrag(delta: number, phase: "start" | "move") {
    if (phase === "start") {
      dragStart = sidebarW;
      if (autoRail) return;
      return;
    }
    if (autoRail) return;
    const w = dragStart + delta;
    if (w < SIDEBAR.snap) app.settings.sidebarCollapsed = true;
    else app.settings = { ...app.settings, sidebarCollapsed: false, sidebarWidth: Math.round(Math.min(SIDEBAR.max, Math.max(SIDEBAR.min, w))) };
  }

  function splitDrag(delta: number, phase: "start" | "move") {
    if (phase === "start") {
      dragStart = reqSize;
      return;
    }
    const total = (stacked ? mainH : mainW) - 1;
    app.settings.splitRatio = Math.min(0.9, Math.max(0.1, (dragStart + delta) / total));
  }

  const persist = () => app.updateSettings({});

  function toggleLayout() {
    app.updateSettings({ layoutMode: stacked ? "side" : "stacked" });
  }

  function expandSidebar() {
    if (collapsed) app.updateSettings({ sidebarCollapsed: false });
  }

  function cyclePane(dir: 1 | -1) {
    const panes = [...document.querySelectorAll<HTMLElement>("[data-pane]")];
    const cur = panes.findIndex((p) => p.contains(document.activeElement));
    const next = panes[(cur + dir + panes.length) % panes.length];
    const target = next?.querySelector<HTMLElement>('[tabindex="0"], button:not([disabled]), input, [contenteditable="true"]');
    target?.focus();
  }

  function rename() {
    if (ui.sidebarTab === "collections" && ui.selectedNode && document.activeElement?.closest("[data-pane=sidebar]")) {
      ui.renaming = { kind: ui.selectedNode.kind, id: ui.selectedNode.id };
    } else if (ui.sidebarTab === "env" && ui.selectedEnvId && document.activeElement?.closest("[data-pane=sidebar]")) {
      const env = app.environments.find((e) => e.id === ui.selectedEnvId);
      if (env?.projectId) ui.renaming = { kind: "environment", id: env.id };
    } else if (app.middle.kind === "request" && !app.isScratch) ui.renameRequestTick++;
    else if (ui.selectedNode) ui.renaming = { kind: ui.selectedNode.kind, id: ui.selectedNode.id };
  }

  function run(a: ShortcutAction) {
    switch (a) {
      case "send":
        void app.sendOrCancel();
        break;
      case "importCurl":
        ui.importDialog = { tab: "curl" };
        break;
      case "envPicker":
        ui.envPicker = true;
        break;
      case "newRequest":
        app.newRequest();
        break;
      case "save":
        void app.save();
        break;
      case "focusUrl":
        if (app.middle.kind !== "request") app.middle = { kind: "request" };
        queueMicrotask(() => reqPane?.focusUrl());
        break;
      case "copyCurl":
        void app.copyCurl();
        break;
      case "format":
        if (app.middle.kind === "request") ui.formatTick++;
        break;
      case "focusFilter":
        expandSidebar();
        ui.focusFilterTick++;
        break;
      case "tabActivity":
      case "tabCollections":
      case "tabEnv":
        expandSidebar();
        ui.sidebarTab = a === "tabActivity" ? "activity" : a === "tabCollections" ? "collections" : "env";
        app.updateSettings({ sidebarTab: ui.sidebarTab });
        break;
      case "rename":
        rename();
        break;
      case "toggleSidebar":
        app.updateSettings({ sidebarCollapsed: !collapsed });
        break;
      case "settings":
        ui.settingsDialog = { section: "general" };
        break;
      case "nextPane":
        cyclePane(1);
        break;
      case "prevPane":
        cyclePane(-1);
        break;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (!app.ready) return;
    const a = matchShortcut(e, app.platform);
    if (!a) return;
    // While a modal is open only its own keys apply (Esc / ⌘Enter are handled by the dialog).
    if (ui.anyModalOpen) return;
    if (a === "rename" && (e.target as HTMLElement).closest?.("input, textarea")) return;
    e.preventDefault();
    e.stopPropagation();
    ui.closeMenu(false);
    run(a);
  }
</script>

<svelte:window onkeydowncapture={onKeydown} bind:innerWidth={winW} />

{#if app.fatal}
  <div class="fatal empty" role="alert">
    <h3>ReqHarbor couldn't start</h3>
    <p class="mono">{app.fatal}</p>
  </div>
{:else if !app.ready}
  <div class="boot" aria-busy="true"></div>
{:else}
  <div class="app" data-testid="app" style:--sidebar-w="{sidebarW}px">
    <div class="side" data-pane="sidebar" style:width="{sidebarW}px">
      <Sidebar {collapsed} onexpand={expandSidebar} />
    </div>
    <Splitter
      label="Resize sidebar"
      value={sidebarW}
      min={SIDEBAR.min}
      max={SIDEBAR.max}
      ondrag={sidebarDrag}
      ondragend={persist}
      onreset={() => app.updateSettings({ sidebarWidth: SIDEBAR.def, sidebarCollapsed: false })}
      ontoggle={() => app.updateSettings({ sidebarCollapsed: !collapsed })}
      testid="sidebar-splitter"
    />
    <div class="main" class:stacked bind:clientWidth={mainW} bind:clientHeight={mainH}>
      <div class="mid" data-pane="request" style={stacked ? `height:${reqSize}px` : `width:${reqSize}px`}>
        {#if app.middle.kind === "collection"}
          <CollectionSettings id={app.middle.id} />
        {:else if app.middle.kind === "environment"}
          <EnvEditor id={app.middle.id} />
        {:else}
          <RequestPane bind:this={reqPane} />
        {/if}
      </div>
      <Splitter
        orientation={stacked ? "horizontal" : "vertical"}
        label="Resize response"
        value={Math.round(ratio * 100)}
        min={10}
        max={90}
        ondrag={splitDrag}
        ondragend={persist}
        onreset={() => app.updateSettings({ splitRatio: 0.5 })}
        testid="response-splitter"
      />
      <div class="right" data-pane="response">
        <ResponsePane {stacked} ontogglelayout={toggleLayout} />
      </div>
    </div>
  </div>
{/if}

<MenuHost />
<Toasts />
{#if ui.envPicker}<EnvPicker />{/if}
{#if ui.importDialog}
  {#await ImportDialog() then M}<M.default tab={ui.importDialog.tab} text={ui.importDialog.text} path={ui.importDialog.path} />{/await}
{/if}
{#if ui.shareDialog}
  {#await ShareDialog() then M}<M.default kind={ui.shareDialog.kind} entityId={ui.shareDialog.entityId} name={ui.shareDialog.name} />{/await}
{/if}
{#if ui.settingsDialog}
  {#await SettingsDialog() then M}<M.default section={ui.settingsDialog.section} />{/await}
{/if}
{#if ui.picker}
  {#await PickerDialog() then M}<M.default mode={ui.picker} />{/await}
{/if}
{#if ui.exportCurl}
  {#await ExportCurl() then M}<M.default anchor={ui.exportCurl.anchor} />{/await}
{/if}
<SyncPrompts />
{#if ui.newProject}
  {#await NewProjectDialog() then M}<M.default />{/await}
{/if}
{#if ui.syncLocation}
  {#await SyncLocationDialog() then M}<M.default projectId={ui.syncLocation?.projectId ?? ""} />{/await}
{/if}
{#if ui.firstSync}
  {#await FirstSyncDialog() then M}<M.default />{/await}
{/if}
{#if ui.mergeLegacy}
  {#await MergeLegacyDialog() then M}<M.default />{/await}
{/if}
{#if ui.spaceNotice}
  {#await SpaceNoticeDialog() then M}<M.default mode={ui.spaceNotice?.mode ?? "foreign"} spaceIds={ui.spaceNotice?.spaceIds ?? []} />{/await}
{/if}
<ConfirmDialog />

<style>
  .app {
    height: 100vh;
    display: flex;
    overflow: hidden;
    background: var(--bg-pane);
  }
  .side {
    flex: none;
    height: 100%;
    min-width: 0;
  }
  .main {
    flex: 1;
    min-width: 0;
    display: flex;
    height: 100%;
  }
  .main.stacked {
    flex-direction: column;
  }
  .mid {
    flex: none;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .right {
    flex: 1;
    min-width: 0;
    min-height: 0;
    overflow: hidden;
  }
  .boot {
    height: 100vh;
    background: var(--bg-pane);
  }
  .fatal {
    height: 100vh;
  }
</style>

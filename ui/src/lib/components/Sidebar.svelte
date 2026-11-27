<script lang="ts">
  import { tick } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui, type SidebarTab } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError } from "../api";
  import { shortcutText } from "../util/shortcuts";
  import { relativeTime, skippedWarning, unsupportedNotice } from "../util/format";
  import ActivityList from "./ActivityList.svelte";
  import CollectionsTree from "./CollectionsTree.svelte";
  import EnvList from "./EnvList.svelte";
  import Icon from "./Icon.svelte";

  let { collapsed, onexpand }: { collapsed: boolean; onexpand: () => void } = $props();

  let filterEl = $state<HTMLInputElement>();
  let projectBtn = $state<HTMLButtonElement>();
  let caretBtn = $state<HTMLButtonElement>();
  let filterMenuBtn = $state<HTMLButtonElement>();
  let projectName = $state("");

  const TABS: { id: SidebarTab; label: string }[] = [
    { id: "activity", label: "Activity" },
    { id: "collections", label: "Collections" },
    { id: "env", label: "Env" },
  ];
  const PLACEHOLDER: Record<SidebarTab, string> = { activity: "filter activity", collections: "filter collections", env: "filter environments" };

  $effect(() => {
    if (ui.focusFilterTick) void tick().then(() => filterEl?.focus());
  });

  $effect(() => {
    if (ui.renaming?.kind === "project") projectName = app.project?.name ?? "";
  });

  function setTab(t: SidebarTab) {
    ui.sidebarTab = t;
    app.updateSettings({ sidebarTab: t });
  }

  function tabKey(e: KeyboardEvent) {
    const i = TABS.findIndex((t) => t.id === ui.sidebarTab);
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    const n = TABS[(i + d + 3) % 3].id;
    setTab(n);
    void tick().then(() => document.querySelector<HTMLElement>(`[data-testid="sidebar-tab-${n}"]`)?.focus());
  }

  function projectMenu() {
    if (!projectBtn) return;
    const p = app.project;
    ui.openMenu(
      [
        ...app.projects.map((x) => ({ label: x.name, checked: x.id === app.activeProjectId, action: () => void app.switchProject(x.id), testid: "project-option" })),
        { separator: true },
        { label: "New project…", action: () => void app.createProject("New project").then(() => (ui.renaming = { kind: "project", id: app.activeProjectId ?? "" })), testid: "project-new" },
        { label: "Rename", shortcut: "F2", disabled: !p, action: () => p && (ui.renaming = { kind: "project", id: p.id }), testid: "project-rename" },
        { label: "Duplicate", disabled: !p, action: () => p && void app.duplicateProject(p.id), testid: "project-duplicate" },
        { label: "Delete…", danger: true, disabled: !p || app.projects.length <= 1, action: () => p && void app.deleteProject(p.id), testid: "project-delete" },
      ],
      projectBtn,
    );
  }

  function newMenu() {
    if (!caretBtn) return;
    const sel = ui.selectedNode;
    const ix = app.index;
    let folderTarget: { collectionId: string; parent: string | null } | null = null;
    if (sel && ix) {
      if (sel.kind === "collection") folderTarget = { collectionId: sel.id, parent: null };
      else if (sel.kind === "folder") folderTarget = { collectionId: sel.collectionId, parent: sel.id };
      else {
        const r = ix.requestById.get(sel.id);
        if (r) folderTarget = { collectionId: r.collectionId, parent: r.folderId };
      }
    }
    ui.openMenu(
      [
        { label: "New Request", shortcut: shortcutText("Mod+N", app.platform), action: () => app.newRequest() },
        { label: "New Collection", action: () => void app.createCollection(), testid: "new-collection-menu" },
        { label: "New Folder", disabled: !folderTarget, action: () => folderTarget && void app.createFolder(folderTarget.collectionId, folderTarget.parent) },
        { separator: true },
        { label: "Import cURL…", shortcut: shortcutText("Mod+U", app.platform), action: () => (ui.importDialog = { tab: "curl" }), testid: "menu-import-curl" },
        { label: "Import OpenAPI…", action: () => (ui.importDialog = { tab: "openapi" }), testid: "menu-import-openapi" },
        { label: "Import from link…", action: () => (ui.importDialog = { tab: "link" }), testid: "menu-import-link" },
      ],
      caretBtn,
    );
  }

  function filterMenu() {
    if (!filterMenuBtn) return;
    if (ui.sidebarTab === "activity") {
      ui.openMenu([{ label: "Clear history…", danger: true, disabled: app.history.length === 0, action: () => void app.clearHistory(), testid: "clear-history" }], filterMenuBtn, { alignRight: true });
    } else if (ui.sidebarTab === "collections") {
      const ix = app.index;
      ui.openMenu(
        [
          { label: "New collection", action: () => void app.createCollection() },
          { separator: true },
          { label: "Expand all", action: () => ix && ui.expandAll([...ix.collectionById.keys(), ...ix.folderById.keys()]) },
          { label: "Collapse all", action: () => (ui.expanded = new Set()) },
          {
            label: "Sort by name",
            action: async () => {
              if (!ix) return;
              const sorted = [...ix.collections].sort((a, b) => a.name.localeCompare(b.name));
              for (let i = 0; i < sorted.length; i++) await app.moveNode("collection", sorted[i].id, null, null, i);
            },
          },
        ],
        filterMenuBtn,
        { alignRight: true },
      );
    } else {
      ui.openMenu([{ label: "New environment", action: () => void app.createEnvironment() }], filterMenuBtn, { alignRight: true });
    }
  }

  function commitProjectRename() {
    const r = ui.renaming;
    ui.renaming = null;
    if (r?.kind === "project" && projectName.trim()) void app.rename("project", r.id, projectName);
  }

  async function syncNow() {
    try {
      const rep = await call("sync_now", {});
      toasts.success(`Synced · ${rep.pushed} pushed, ${rep.pulled} pulled`);
      const skipped = skippedWarning(rep.tooLarge);
      if (skipped) toasts.warning(skipped);
      const notice = unsupportedNotice(rep.unsupported?.length ?? 0);
      if (notice) toasts.warning(notice);
    } catch (e) {
      toasts.error(toCommandError(e).message);
    }
  }

  const sync = $derived(app.sync);
  const syncVisible = $derived(sync.connected || sync.state === "unauthorized");
  const retryIn = $derived(sync.nextRetryAt ? Math.max(0, Math.round((sync.nextRetryAt - app.now) / 1000)) : null);
</script>

{#if collapsed}
  <aside class="rail" aria-label="Sidebar" data-testid="sidebar-rail">
    <button class="icon-btn" type="button" aria-label="New request" title="New request  {shortcutText('Mod+N', app.platform)}" onclick={() => app.newRequest()}><Icon name="plus" /></button>
    {#each TABS as t (t.id)}
      <button
        class="icon-btn"
        class:on={ui.sidebarTab === t.id}
        type="button"
        aria-label={t.label}
        title={t.label}
        onclick={() => {
          setTab(t.id);
          onexpand();
        }}><Icon name={t.id === "activity" ? "clock" : t.id === "collections" ? "stack" : "sliders"} /></button
      >
    {/each}
    <span class="grow"></span>
    <button class="icon-btn" type="button" aria-label="Settings" title="Settings" onclick={() => (ui.settingsDialog = { section: "general" })}><Icon name="gear" /></button>
  </aside>
{:else}
  <aside class="sidebar" aria-label="Sidebar" data-testid="sidebar">
    <div class="row1">
      {#if ui.renaming?.kind === "project"}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="proj-rename"
          bind:value={projectName}
          aria-label="Project name"
          autofocus
          data-testid="project-rename-input"
          onfocus={(e) => e.currentTarget.select()}
          onkeydown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") commitProjectRename();
            else if (e.key === "Escape") ui.renaming = null;
          }}
          onblur={commitProjectRename}
        />
      {:else}
        <button class="project" type="button" aria-haspopup="menu" title="Switch project" bind:this={projectBtn} onclick={projectMenu} data-testid="project-switcher">
          <span class="faint"><Icon name="layers" small /></span><span class="pname">{app.project?.name ?? "Project"}</span><span class="faint"><Icon name="chev-d" small /></span>
        </button>
      {/if}
      <span class="grow"></span>
      <button class="icon-btn" type="button" aria-label="Import" title="Import (cURL, OpenAPI, link)  {shortcutText('Mod+U', app.platform)}" onclick={() => (ui.importDialog = { tab: "curl" })} data-testid="import-button"
        ><Icon name="import" /></button
      >
    </div>
    <div class="row2">
      <div class="split-btn">
        <button class="btn btn-primary main" type="button" onclick={() => app.newRequest()} title="New request  {shortcutText('Mod+N', app.platform)}" data-testid="new-request"><Icon name="plus" small />New Request</button>
        <button class="btn btn-primary caret" type="button" aria-label="More new options" aria-haspopup="menu" bind:this={caretBtn} onclick={newMenu} data-testid="new-menu"><Icon name="chev-d" small /></button>
      </div>
    </div>
    <div class="tabs" role="tablist" aria-label="Sidebar">
      {#each TABS as t, i (t.id)}
        <button
          class="tab"
          role="tab"
          type="button"
          aria-selected={ui.sidebarTab === t.id}
          tabindex={ui.sidebarTab === t.id ? 0 : -1}
          title="{t.label}  {shortcutText(`Mod+${i + 1}`, app.platform)}"
          data-testid="sidebar-tab-{t.id}"
          onclick={() => setTab(t.id)}
          onkeydown={tabKey}>{t.label}</button
        >
      {/each}
    </div>
    <div class="filter-row">
      <label class="pill">
        <Icon name="search" small />
        <input
          bind:this={filterEl}
          bind:value={ui.filters[ui.sidebarTab]}
          placeholder={PLACEHOLDER[ui.sidebarTab]}
          aria-label={PLACEHOLDER[ui.sidebarTab]}
          data-testid="sidebar-filter"
          spellcheck="false"
          onkeydown={(e) => {
            if (e.key === "Escape") {
              e.stopPropagation();
              if (ui.filters[ui.sidebarTab]) ui.filters[ui.sidebarTab] = "";
              else filterEl?.blur();
            } else if (e.key === "ArrowDown") {
              e.preventDefault();
              document.querySelector<HTMLElement>(".sidebar [role=option][tabindex='0'], .sidebar [role=treeitem][tabindex='0'], .sidebar [role=option]")?.focus();
            }
          }}
        />
      </label>
      <button class="icon-btn" type="button" aria-label="{TABS.find((t) => t.id === ui.sidebarTab)?.label} options" aria-haspopup="menu" bind:this={filterMenuBtn} onclick={filterMenu} data-testid="filter-menu"
        ><Icon name="menu" /></button
      >
    </div>
    <div class="list">
      {#if ui.sidebarTab === "activity"}
        <ActivityList />
      {:else if ui.sidebarTab === "collections"}
        <CollectionsTree />
      {:else}
        <EnvList />
      {/if}
    </div>
    <div class="footer">
      <button class="env-btn" type="button" title="Change environment  {shortcutText('Mod+E', app.platform)}" onclick={() => (ui.envPicker = true)} data-testid="env-selector">
        <span class="faint">Env</span>
        {#if app.activeEnv}<b data-testid="active-env">{app.activeEnv.name}</b>{:else}<span class="faint" data-testid="active-env">No environment</span>{/if}
        <Icon name="chev-d" small />
      </button>
      <span class="grow"></span>
      {#if sync.warning && sync.connected}
        <span class="sync warn" title={sync.warning} data-testid="sync-warning"><Icon name="alert" small /></span>
      {/if}
      {#if syncVisible}
        {#if sync.state === "unauthorized" || sync.state === "error"}
          <button class="sync danger" type="button" title={sync.lastError ?? "Sync is off"} onclick={() => (ui.settingsDialog = { section: "sync" })} data-testid="sync-indicator"
            ><Icon name="cloud-off" small />Sync off</button
          >
        {:else if sync.state === "offline"}
          <button class="sync warn" type="button" title={retryIn !== null ? `Retrying in ${retryIn} s — click to sync now` : "Offline — click to sync now"} onclick={syncNow} data-testid="sync-indicator"
            ><Icon name="cloud-off" small />Offline</button
          >
        {:else}
          <button
            class="sync"
            class:syncing={sync.state === "syncing"}
            type="button"
            title={sync.lastSyncAt ? `Synced ${relativeTime(sync.lastSyncAt, app.now)} — click to sync now` : "Click to sync now"}
            onclick={syncNow}
            data-testid="sync-indicator"><Icon name="cloud" small />{sync.state === "syncing" ? "Syncing" : "Synced"}</button
          >
        {/if}
      {/if}
      <button class="icon-btn" type="button" aria-label="Settings" title="Settings  {shortcutText('Mod+,', app.platform)}" onclick={() => (ui.settingsDialog = { section: "general" })} data-testid="settings-button"><Icon name="gear" /></button>
    </div>
  </aside>
{/if}

<style>
  .sidebar,
  .rail {
    height: 100%;
    display: flex;
    flex-direction: column;
    background: var(--bg-sidebar);
    min-width: 0;
  }
  .rail {
    align-items: center;
    gap: var(--sp-3);
    padding: var(--sp-4) 0;
  }
  .rail .icon-btn {
    width: 30px;
    height: 30px;
  }
  .rail .on {
    color: var(--accent-fg);
  }
  .project {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 24px;
    padding: 0 var(--sp-3) 0 var(--sp-2);
    margin-left: calc(var(--sp-2) * -1);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    font-weight: var(--fw-semibold);
    min-width: 0;
  }
  .project span {
    display: inline-flex;
  }
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .project:hover {
    background: var(--bg-hover);
  }
  .proj-rename {
    height: 24px;
    border: 1px solid var(--border-focus);
    border-radius: var(--r-sm);
    background: var(--bg-input);
    color: var(--text-1);
    padding: 0 var(--sp-3);
    font-weight: var(--fw-semibold);
    outline: none;
    min-width: 0;
    flex: 1;
  }
  .split-btn {
    display: flex;
    width: 100%;
  }
  .split-btn .main {
    border-radius: var(--r-sm) 0 0 var(--r-sm);
    flex: 1;
  }
  .split-btn .caret {
    flex: none;
    width: 30px;
    padding: 0;
    border-radius: 0 var(--r-sm) var(--r-sm) 0;
    border-left: 1px solid rgba(0, 0, 0, 0.22);
  }
  .filter-row {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-4) var(--sp-4) var(--sp-3) var(--sp-5);
    flex: none;
  }
  .list {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding-bottom: var(--sp-2);
  }
  .footer {
    height: var(--h-sidebar-footer);
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: 0 var(--sp-3) 0 var(--sp-4);
    border-top: 1px solid var(--border-subtle);
    flex: none;
    font-size: var(--fs-sm);
  }
  .env-btn {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 22px;
    padding: 0 var(--sp-3);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-2);
    font-size: var(--fs-sm);
    min-width: 0;
  }
  .env-btn:hover {
    background: var(--bg-hover);
    color: var(--text-1);
  }
  .env-btn b {
    color: var(--text-1);
    font-weight: var(--fw-medium);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sync {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-3);
    font-size: var(--fs-sm);
    padding: 0 var(--sp-2);
    border: 0;
    background: none;
    height: 22px;
    border-radius: var(--r-sm);
  }
  button.sync:hover {
    background: var(--bg-hover);
  }
  .sync.syncing {
    opacity: 0.6;
  }
  .sync.warn {
    color: var(--warning);
  }
  .sync.danger {
    color: var(--danger);
  }
</style>

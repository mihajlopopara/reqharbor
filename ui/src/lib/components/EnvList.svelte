<script lang="ts">
  import type { Environment } from "../api/types";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import Hl from "./Hl.svelte";
  import Icon from "./Icon.svelte";

  const filter = $derived(ui.filters.env.trim().toLowerCase());
  const envs = $derived.by(() => {
    const list = [app.globalEnv, ...app.projectEnvs].filter((e): e is Environment => !!e);
    return filter ? list.filter((e) => e.name.toLowerCase().includes(filter) || e.variables.some((v) => v.key.toLowerCase().includes(filter))) : list;
  });
  let renameValue = $state("");

  $effect(() => {
    const r = ui.renaming;
    if (r?.kind === "environment") renameValue = app.environments.find((e) => e.id === r.id)?.name ?? "";
  });

  function menu(env: Environment, anchor: HTMLElement | { x: number; y: number }) {
    const isGlobal = env.projectId === null;
    ui.selectedEnvId = env.id;
    ui.openMenu(
      [
        { label: "Open", action: () => app.openEnvironment(env.id) },
        { label: "Set active", disabled: isGlobal || app.activeEnv?.id === env.id, action: () => void app.setActiveEnvironment(env.id) },
        { separator: true },
        { label: "Rename", shortcut: "F2", disabled: isGlobal, action: () => (ui.renaming = { kind: "environment", id: env.id }) },
        { label: "Duplicate", disabled: isGlobal, action: () => void app.duplicateEnvironment(env.id) },
        { separator: true },
        { label: "Delete", danger: true, disabled: isGlobal, action: () => void app.deleteEnvironment(env.id) },
      ],
      anchor,
      { alignRight: anchor instanceof HTMLElement },
    );
  }

  function commit(env: Environment) {
    const v = renameValue.trim();
    ui.renaming = null;
    if (v && v !== env.name) void app.rename("environment", env.id, v);
  }

  function onkeydown(e: KeyboardEvent) {
    const rows = [...(e.currentTarget as HTMLElement).querySelectorAll<HTMLElement>("[role=option]")];
    const i = rows.indexOf(document.activeElement as HTMLElement);
    if (e.key === "ArrowDown") rows[Math.min(i + 1, rows.length - 1)]?.focus();
    else if (e.key === "ArrowUp") rows[Math.max(i - 1, 0)]?.focus();
    else return;
    e.preventDefault();
  }
</script>

<div class="envs" role="listbox" aria-label="Environments" tabindex="-1" data-testid="env-list" {onkeydown}>
  {#each envs as env (env.id)}
    {@const active = app.activeEnv?.id === env.id}
    <div
      class="erow"
      role="option"
      aria-selected={app.middle.kind === "environment" && app.middle.id === env.id}
      tabindex="0"
      data-testid="env-row"
      data-name={env.name}
      onclick={() => app.openEnvironment(env.id)}
      onkeydown={(e) => {
        if (e.key === "Enter") app.openEnvironment(env.id);
        else if (e.key === "Delete" && env.projectId) void app.deleteEnvironment(env.id);
      }}
      oncontextmenu={(e) => {
        e.preventDefault();
        menu(env, { x: e.clientX, y: e.clientY });
      }}
    >
      <span class="ic faint"><Icon name={env.projectId === null ? "globe" : "layers"} small /></span>
      {#if ui.renaming?.kind === "environment" && ui.renaming.id === env.id}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          class="rename"
          bind:value={renameValue}
          aria-label="Environment name"
          autofocus
          onclick={(e) => e.stopPropagation()}
          onkeydown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") commit(env);
            else if (e.key === "Escape") ui.renaming = null;
          }}
          onblur={() => commit(env)}
        />
      {:else}
        <span class="name" class:active><Hl text={env.name} query={ui.filters.env} /></span>
      {/if}
      <span class="cnt">{env.variables.length}</span>
      {#if active}<span class="check" title="Active environment"><Icon name="check" small /></span>{/if}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <span
        class="icon-btn more"
        aria-hidden="true"
        onclick={(e) => {
          e.stopPropagation();
          menu(env, e.currentTarget);
        }}><Icon name="more" small /></span
      >
    </div>
  {/each}
  {#if app.projectEnvs.length === 0 && !filter}
    <div class="empty small-empty" data-testid="env-empty">
      <p>No project environments</p>
      <button class="btn btn-secondary btn-sm" type="button" onclick={() => void app.createEnvironment()} data-testid="new-environment">New environment</button>
    </div>
  {:else if filter && envs.length === 0}
    <div class="empty">
      <p>No matches for “{ui.filters.env}”</p>
      <button class="link" type="button" onclick={() => (ui.filters.env = "")}>Clear filter</button>
    </div>
  {/if}
</div>

<style>
  .envs {
    flex: 1;
    overflow: auto;
    outline: none;
  }
  .erow {
    height: var(--h-row-tree);
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-3) 0 var(--sp-5);
    cursor: default;
    outline: none;
  }
  .erow:hover {
    background: var(--bg-hover);
  }
  .erow[aria-selected="true"] {
    background: var(--bg-selected);
  }
  .erow:focus-visible {
    box-shadow: inset 0 0 0 2px var(--focus-ring);
  }
  .ic {
    display: inline-flex;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name.active {
    color: var(--text-1);
    font-weight: var(--fw-medium);
  }
  .cnt {
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .check {
    color: var(--accent-fg);
    display: inline-flex;
  }
  .more {
    width: 20px;
    height: 20px;
    display: none;
  }
  .erow:hover .more,
  .erow:focus-within .more {
    display: inline-grid;
  }
  .rename {
    flex: 1;
    height: 22px;
    border: 1px solid var(--border-focus);
    border-radius: var(--r-xs);
    background: var(--bg-input);
    color: var(--text-1);
    padding: 0 var(--sp-2);
    outline: none;
  }
  .small-empty {
    padding: var(--sp-6) var(--sp-5);
  }
</style>

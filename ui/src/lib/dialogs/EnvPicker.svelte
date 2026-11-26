<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import Icon from "../components/Icon.svelte";

  /** ⌘E quick picker (AC-ENV-5, DESIGN §5.13). */
  let q = $state("");
  let hl = $state(0);
  let input: HTMLInputElement;
  const returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  type Opt = { id: string | null; label: string; manage?: boolean };
  const opts = $derived.by<Opt[]>(() => {
    const s = q.trim().toLowerCase();
    const list: Opt[] = [{ id: null, label: "No environment" }, ...app.projectEnvs.map((e) => ({ id: e.id, label: e.name }))].filter((o) => !s || o.label.toLowerCase().includes(s));
    list.push({ id: null, label: "Manage environments…", manage: true });
    return list;
  });

  onMount(() => {
    input.focus();
    const cur = opts.findIndex((o) => !o.manage && o.id === (app.activeEnv?.id ?? null));
    hl = Math.max(0, cur);
    return () => returnTo?.focus();
  });

  function choose(o: Opt) {
    ui.envPicker = false;
    if (o.manage) {
      ui.sidebarTab = "env";
      if (app.activeEnv) app.openEnvironment(app.activeEnv.id);
      return;
    }
    void app.setActiveEnvironment(o.id);
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown") hl = (hl + 1) % opts.length;
    else if (e.key === "ArrowUp") hl = (hl - 1 + opts.length) % opts.length;
    else if (e.key === "Enter") choose(opts[hl]);
    else if (e.key === "Escape") ui.envPicker = false;
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<div class="scrim" role="presentation" onmousedown={(e) => e.target === e.currentTarget && (ui.envPicker = false)}>
  <div class="palette pop" role="dialog" aria-modal="true" aria-label="Change environment" data-testid="env-picker">
    <div class="search">
      <Icon name="search" small />
      <input
        bind:this={input}
        bind:value={q}
        placeholder="Switch environment…"
        aria-label="Search environments"
        role="combobox"
        aria-expanded="true"
        aria-controls="env-picker-list"
        aria-activedescendant="env-opt-{hl}"
        oninput={() => (hl = 0)}
        {onkeydown}
        data-testid="env-picker-input"
      />
    </div>
    <div class="opts" role="listbox" id="env-picker-list">
      {#each opts as o, i (o.label + i)}
        {#if o.manage}<div class="sep" role="separator"></div>{/if}
        <div
          id="env-opt-{i}"
          class="mi"
          class:hl={hl === i}
          role="option"
          aria-selected={!o.manage && o.id === (app.activeEnv?.id ?? null)}
          tabindex="-1"
          data-testid="env-option"
          data-name={o.label}
          onmouseenter={() => (hl = i)}
          onclick={() => choose(o)}
          onkeydown={() => {}}
        >
          <span class="chk">{#if !o.manage && o.id === (app.activeEnv?.id ?? null)}<Icon name="check" small />{/if}</span>
          <span class:faint={o.id === null && !o.manage}>{o.label}</span>
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: var(--z-dialog);
  }
  .palette {
    position: absolute;
    top: 64px;
    left: 50%;
    width: min(420px, calc(100vw - 32px));
    transform: translateX(-50%);
    background: var(--bg-raised);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-dialog);
    overflow: hidden;
  }
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding: 0 var(--sp-5);
    height: 40px;
    border-bottom: 1px solid var(--border-subtle);
    color: var(--text-3);
  }
  .search input {
    flex: 1;
    border: 0;
    background: none;
    outline: none;
    color: var(--text-1);
    font-size: var(--fs-md);
  }
  .opts {
    padding: var(--sp-2);
    max-height: 320px;
    overflow: auto;
  }
  .chk {
    width: 14px;
    display: inline-flex;
  }
  .mi.hl .faint {
    color: var(--text-on-accent);
  }
</style>

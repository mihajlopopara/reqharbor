<script lang="ts">
  import { tick } from "svelte";
  import { ui, type MenuItem } from "../stores/ui.svelte";
  import Icon from "./Icon.svelte";

  let el = $state<HTMLDivElement>();
  let hl = $state(-1);
  let pos = $state({ x: 0, y: 0 });
  let typeahead = "";
  let typeTimer: ReturnType<typeof setTimeout> | null = null;

  const menu = $derived(ui.menu);
  const actionable = (it: MenuItem) => !it.separator && !it.disabled && !it.heading;

  $effect(() => {
    const m = menu;
    if (!m) return;
    hl = -1;
    pos = { x: m.x, y: m.y };
    void tick().then(() => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      let x = m.x;
      let y = m.y;
      if (x + r.width > window.innerWidth - 8) x = Math.max(8, window.innerWidth - r.width - 8);
      if (y + r.height > window.innerHeight - 8) y = Math.max(8, m.y - r.height - 8);
      pos = { x: Math.max(8, x), y };
      el.focus();
      hl = m.items.findIndex(actionable);
    });
  });

  function activate(it: MenuItem) {
    if (!actionable(it)) return;
    ui.closeMenu(true);
    it.action?.();
  }

  function move(delta: number) {
    if (!menu) return;
    const n = menu.items.length;
    let i = hl;
    for (let k = 0; k < n; k++) {
      i = (i + delta + n) % n;
      if (actionable(menu.items[i])) {
        hl = i;
        return;
      }
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (!menu) return;
    if (e.key === "ArrowDown") move(1);
    else if (e.key === "ArrowUp") move(-1);
    else if (e.key === "Home") {
      hl = -1;
      move(1);
    } else if (e.key === "End") {
      hl = menu.items.length;
      move(-1);
    } else if (e.key === "Enter" || e.key === " ") {
      if (hl >= 0) activate(menu.items[hl]);
    } else if (e.key === "Escape" || e.key === "Tab") ui.closeMenu(true);
    else if (e.key.length === 1 && /\S/.test(e.key)) {
      typeahead += e.key.toLowerCase();
      if (typeTimer) clearTimeout(typeTimer);
      typeTimer = setTimeout(() => (typeahead = ""), 600);
      const i = menu.items.findIndex((it) => actionable(it) && it.label?.toLowerCase().startsWith(typeahead));
      if (i >= 0) hl = i;
    } else return;
    e.preventDefault();
    e.stopPropagation();
  }

  function onBackdrop(e: MouseEvent) {
    if (el && !el.contains(e.target as Node)) ui.closeMenu(false);
  }
</script>

<svelte:window onmousedown={menu ? onBackdrop : undefined} onblur={() => menu && ui.closeMenu(false)} onresize={() => menu && ui.closeMenu(false)} />

{#if menu}
  <div
    class="menu pop host"
    role="menu"
    tabindex="-1"
    bind:this={el}
    style:left="{pos.x}px"
    style:top="{pos.y}px"
    style:min-width="{menu.minWidth ?? 200}px"
    {onkeydown}
    data-testid="context-menu"
  >
    {#each menu.items as it, i (i)}
      {#if it.separator}
        <div class="sep" role="separator"></div>
      {:else if it.heading}
        <div class="mh" class:group={it.group} role="presentation" data-testid={it.testid}>
          <span class="mh-row"><span class="lbl">{it.label}</span>{#if it.trailing}<span class="trail">{it.trailing}</span>{/if}</span>
          {#if it.sublabel}<span class="sub">{it.sublabel}</span>{/if}
        </div>
      {:else}
        <div
          class="mi"
          class:hl={hl === i}
          class:danger={it.danger}
          role={it.checked !== undefined ? "menuitemcheckbox" : "menuitem"}
          aria-checked={it.checked !== undefined ? it.checked : undefined}
          aria-disabled={it.disabled ? "true" : undefined}
          aria-label={it.ariaLabel}
          {...it.disabled && it.reason ? { "aria-description": it.reason } : {}}
          title={it.reason ?? it.title}
          tabindex="-1"
          data-testid={it.testid}
          onmouseenter={() => (hl = actionable(it) ? i : -1)}
          onclick={() => activate(it)}
          onkeydown={() => {}}
        >
          {#if it.icon}<span class="mi-icon"><Icon name={it.icon} small /></span>{/if}
          {#if it.checked !== undefined && !it.icon}
            <span class="chk">{#if it.checked}<Icon name="check" small />{/if}</span>
          {/if}
          <span class="lbl">{it.label}</span>
          {#if it.icon && it.checked}<span class="chk end"><Icon name="check" small /></span>{/if}
          {#if it.shortcut}<span class="sc">{it.shortcut}</span>{/if}
        </div>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .host {
    position: fixed;
    z-index: var(--z-popover);
    outline: none;
    max-height: calc(100vh - 16px);
    overflow-y: auto;
  }
  .chk {
    width: 14px;
    display: inline-flex;
  }
  .lbl {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mh {
    display: flex;
    flex-direction: column;
    padding: var(--sp-3) var(--sp-5);
    font-weight: var(--fw-semibold);
    cursor: default;
  }
  .mh .sub {
    font-weight: var(--fw-regular);
    font-size: var(--fs-sm);
    color: var(--text-3);
  }
  .mh-row {
    display: flex;
    align-items: baseline;
    gap: var(--sp-4);
  }
  .mh.group {
    padding: var(--sp-4) var(--sp-5) var(--sp-2);
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
  }
  .trail {
    margin-left: auto;
    text-transform: none;
    letter-spacing: normal;
    font-weight: var(--fw-regular);
  }
  .mi-icon {
    display: inline-flex;
    color: var(--text-3);
  }
  .chk.end {
    margin-left: auto;
    justify-content: flex-end;
  }
</style>

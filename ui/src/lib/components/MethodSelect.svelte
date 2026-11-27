<script lang="ts">
  import { tick } from "svelte";
  import { METHOD_RE, STANDARD_METHODS } from "../api/types";
  import { methodClass, methodLabel } from "../util/format";
  import Icon from "./Icon.svelte";

  /**
   * Method dropdown (DESIGN §5.5): listbox, type-ahead, Custom… inline input. The WS / SIO entries
   * switch the request kind to WebSocket / Socket.IO (AC-WS-1); `kind` says which one is current.
   */
  let {
    method,
    onchange,
    kind = "http",
    onkind,
  }: {
    method: string;
    onchange: (m: string) => void;
    kind?: "http" | "ws" | "sio";
    onkind?: (k: "http" | "ws" | "sio", method?: string) => void;
  } = $props();

  const HINTS: Record<string, string> = { QUERY: "safe, with body", HEAD: "headers only", OPTIONS: "capabilities", DELETE: "remove", WS: "WebSocket", SIO: "Socket.IO" };
  const OPTIONS: readonly string[] = $derived([...STANDARD_METHODS, ...(onkind ? ["WS", "SIO"] : []), "CUSTOM"]);
  /** What the trigger shows and the list marks as current. */
  const current = $derived(kind === "ws" ? "WS" : kind === "sio" ? "SIO" : method);

  let open = $state(false);
  let hl = $state(0);
  let custom = $state<string | null>(null);
  let trigger = $state<HTMLButtonElement>();
  let listEl = $state<HTMLDivElement>();
  let customEl = $state<HTMLInputElement>();
  const customValid = $derived(custom === null || METHOD_RE.test(custom));

  async function openList() {
    open = true;
    const i = OPTIONS.indexOf(current);
    hl = i >= 0 ? i : OPTIONS.length - 1;
    await tick();
    listEl?.focus();
  }

  function apply(m: string) {
    if (m === "WS" || m === "SIO") onkind?.(m === "WS" ? "ws" : "sio");
    else if (kind !== "http") onkind?.("http", m);
    else onchange(m);
  }

  function choose(m: string) {
    open = false;
    if (m === "CUSTOM") {
      custom = kind === "http" && !STANDARD_METHODS.includes(method as never) ? method : "";
      void tick().then(() => customEl?.focus());
      return;
    }
    apply(m);
    trigger?.focus();
  }

  function commitCustom() {
    if (custom !== null && custom !== "" && METHOD_RE.test(custom)) apply(custom);
    custom = null;
    void tick().then(() => trigger?.focus());
  }

  const TYPE_AHEAD: Record<string, string[]> = { g: ["GET"], p: ["POST", "PUT", "PATCH"], q: ["QUERY"], d: ["DELETE"], h: ["HEAD"], o: ["OPTIONS"] };

  function triggerKey(e: KeyboardEvent) {
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp" || e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      void openList();
      return;
    }
    const opts = TYPE_AHEAD[e.key.toLowerCase()];
    if (opts) {
      e.preventDefault();
      const i = kind === "http" ? opts.indexOf(method) : -1;
      apply(opts[(i + 1) % opts.length]);
    }
  }

  function listKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") hl = (hl + 1) % OPTIONS.length;
    else if (e.key === "ArrowUp") hl = (hl - 1 + OPTIONS.length) % OPTIONS.length;
    else if (e.key === "Home") hl = 0;
    else if (e.key === "End") hl = OPTIONS.length - 1;
    else if (e.key === "Enter" || e.key === " ") choose(OPTIONS[hl]);
    else if (e.key === "Escape" || e.key === "Tab") {
      open = false;
      trigger?.focus();
    } else {
      const k = e.key.toLowerCase();
      const i = OPTIONS.findIndex((o, idx) => idx > hl && o.toLowerCase().startsWith(k));
      const j = i >= 0 ? i : OPTIONS.findIndex((o) => o.toLowerCase().startsWith(k));
      if (j >= 0) hl = j;
      else return;
    }
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<svelte:window
  onmousedown={(e) => {
    if (open && listEl && !listEl.contains(e.target as Node) && !trigger?.contains(e.target as Node)) open = false;
  }}
/>

<div class="ms">
  {#if custom !== null}
    <input
      bind:this={customEl}
      class="custom"
      class:invalid={!customValid}
      value={custom}
      maxlength="32"
      aria-label="Custom method"
      aria-invalid={!customValid}
      title={customValid ? "Custom method — Enter to apply, Esc to cancel" : "Use A–Z, 0–9, _ or -, starting with a letter"}
      data-testid="method-custom-input"
      spellcheck="false"
      oninput={(e) => (custom = e.currentTarget.value.toUpperCase())}
      onkeydown={(e) => {
        if (e.key === "Enter") {
          e.preventDefault();
          if (customValid && custom) commitCustom();
        } else if (e.key === "Escape") {
          e.preventDefault();
          e.stopPropagation();
          custom = null;
          void tick().then(() => trigger?.focus());
        }
      }}
      onblur={() => (customValid && custom ? commitCustom() : (custom = null))}
    />
    {#if !customValid}
      <div class="err-tip" role="alert" data-testid="method-custom-error">Use A–Z, 0–9, _ or -, starting with a letter</div>
    {/if}
  {:else}
    <button
      bind:this={trigger}
      class="trigger"
      type="button"
      aria-haspopup="listbox"
      aria-expanded={open}
      aria-label="Method: {kind === 'ws' ? 'WebSocket' : kind === 'sio' ? 'Socket.IO' : method}"
      title={current.length > 7 ? current : undefined}
      data-testid="method-select"
      data-method={current}
      onclick={() => (open ? (open = false) : void openList())}
      onkeydown={triggerKey}
    >
      <span class="m {kind === 'http' ? methodClass(method) : 'm-ws'}">{methodLabel(current)}</span>
      <Icon name="chev-d" small />
    </button>
  {/if}
  {#if open}
    <div
      class="menu pop list"
      role="listbox"
      tabindex="-1"
      aria-label="Method"
      aria-activedescendant="method-opt-{hl}"
      bind:this={listEl}
      onkeydown={listKey}
      data-testid="method-list"
    >
      {#each OPTIONS as o, i (o)}
        {#if o === "CUSTOM" || o === "WS"}<div class="sep" aria-hidden="true"></div>{/if}
        <div
          id="method-opt-{i}"
          class="mi"
          class:hl={hl === i}
          role="option"
          aria-selected={o === current}
          tabindex="-1"
          data-testid="method-option-{o}"
          onmouseenter={() => (hl = i)}
          onclick={() => choose(o)}
          onkeydown={() => {}}
        >
          {#if o === "CUSTOM"}
            <span>Custom…</span>
            {#if kind === "http" && !STANDARD_METHODS.includes(method as never)}<span class="sc cur" aria-hidden="true"><span class="m m-custom">{method}</span><Icon name="check" small /></span>{/if}
          {:else}
            <span class="m {o === 'WS' || o === 'SIO' ? 'm-ws' : methodClass(o)} big">{o}</span>
            {#if o === current}<span class="sc cur" aria-hidden="true"><Icon name="check" small /></span>
            {:else if HINTS[o]}<span class="sc">{HINTS[o]}</span>{/if}
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

<style>
  .ms {
    position: relative;
    flex: none;
    width: var(--w-method-trigger);
    border-right: 1px solid var(--border-subtle);
  }
  .trigger,
  .custom {
    width: 100%;
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--sp-3) 0 var(--sp-4);
    border: 0;
    background: none;
    border-radius: var(--r-sm) 0 0 var(--r-sm);
    color: var(--text-3);
  }
  .trigger:hover {
    background: var(--bg-hover);
  }
  .trigger .m {
    font-size: var(--fs-md);
  }
  .custom {
    font: var(--fw-semibold) var(--fs-md) / 1 var(--font-mono);
    color: var(--method-custom);
    outline: none;
    text-transform: uppercase;
    border: 1px solid var(--border-focus);
  }
  .custom.invalid {
    border-color: var(--danger);
  }
  .err-tip {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: var(--z-tooltip);
    white-space: nowrap;
    padding: 4px 8px;
    background: var(--bg-raised);
    color: var(--danger);
    border-radius: var(--r-sm);
    box-shadow: var(--shadow-popover);
    font-size: var(--fs-sm);
  }
  .list {
    position: absolute;
    top: calc(100% + 6px);
    left: -1px;
    width: 200px;
    z-index: var(--z-popover);
    outline: none;
  }
  .list .m.big {
    font-size: var(--fs-code);
    min-width: 64px;
  }
  .cur {
    display: inline-flex;
    align-items: center;
    gap: var(--sp-2);
    color: var(--text-2);
  }
  .mi.hl .m {
    color: var(--text-on-accent);
  }
</style>

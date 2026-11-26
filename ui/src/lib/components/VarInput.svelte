<script lang="ts">
  /**
   * Single-line input with {{var}} highlighting (DESIGN §5.4): a transparent
   * <input> over a mirrored, decorated layer. Hover shows "value · from Env"
   * (secrets masked until clicked).
   */
  import { tokenizeVars, segmentAt, type VarLookup, type VarSegment } from "../util/vars";

  let {
    value = "",
    lookup,
    placeholder = "",
    ariaLabel,
    testid,
    variant = "field",
    invalid = false,
    disabled = false,
    strike = false,
    inputEl = $bindable(),
    oninput,
    onkeydown,
    onpaste,
    onfocus,
    onblur,
    list,
  }: {
    value?: string;
    lookup: VarLookup;
    placeholder?: string;
    ariaLabel?: string;
    testid?: string;
    /** "url" = request bar, "field" = form input, "cell" = KV cell */
    variant?: "url" | "field" | "cell";
    invalid?: boolean;
    disabled?: boolean;
    strike?: boolean;
    inputEl?: HTMLInputElement;
    oninput?: (v: string) => void;
    onkeydown?: (e: KeyboardEvent) => void;
    onpaste?: (e: ClipboardEvent) => void;
    onfocus?: (e: FocusEvent) => void;
    onblur?: (e: FocusEvent) => void;
    list?: string;
  } = $props();

  let overlay = $state<HTMLDivElement>();
  const segs = $derived(tokenizeVars(value, lookup));
  const hasVars = $derived(segs.some((s) => s.kind !== "text"));

  let tip = $state<{ seg: VarSegment; x: number; reveal: boolean } | null>(null);
  let hideTimer: ReturnType<typeof setTimeout> | null = null;
  let charWidth = 0;

  function syncScroll() {
    if (overlay && inputEl) overlay.scrollLeft = inputEl.scrollLeft;
  }

  function measure(): number {
    if (charWidth || !inputEl) return charWidth;
    const c = document.createElement("canvas").getContext("2d");
    if (!c) return 7.5;
    const cs = getComputedStyle(inputEl);
    c.font = `${cs.fontSize} ${cs.fontFamily}`;
    charWidth = c.measureText("0123456789").width / 10 || 7.5;
    return charWidth;
  }

  function onmove(e: MouseEvent) {
    if (!hasVars || !inputEl) return;
    const r = inputEl.getBoundingClientRect();
    const pl = parseFloat(getComputedStyle(inputEl).paddingLeft) || 0;
    const cw = measure();
    const idx = Math.floor((e.clientX - r.left - pl + inputEl.scrollLeft) / cw);
    const s = segmentAt(segs, idx);
    if (s && s.kind !== "text") {
      if (hideTimer) clearTimeout(hideTimer);
      if (tip?.seg.start !== s.start) {
        const x = Math.max(0, pl + s.start * cw - inputEl.scrollLeft);
        tip = { seg: s, x, reveal: false };
      }
    } else scheduleHide();
  }

  function scheduleHide() {
    if (hideTimer) clearTimeout(hideTimer);
    hideTimer = setTimeout(() => (tip = null), 150);
  }

  function keepTip() {
    if (hideTimer) clearTimeout(hideTimer);
  }
</script>

<div class="vi {variant}" class:invalid class:strike>
  {#if hasVars}
    <div class="overlay" aria-hidden="true" bind:this={overlay}>
      {#each segs as s (s.start)}{#if s.kind === "text"}{s.text}{:else}<span class={s.kind === "var" ? "var" : "var-bad"}>{s.text}</span>{/if}{/each}
    </div>
  {/if}
  <input
    bind:this={inputEl}
    class:clear={hasVars}
    type="text"
    spellcheck="false"
    autocomplete="off"
    autocapitalize="off"
    {value}
    {placeholder}
    {disabled}
    {list}
    aria-label={ariaLabel}
    aria-invalid={invalid || undefined}
    data-testid={testid}
    oninput={(e) => {
      oninput?.(e.currentTarget.value);
      requestAnimationFrame(syncScroll);
    }}
    onscroll={syncScroll}
    onkeyup={syncScroll}
    onselect={syncScroll}
    {onkeydown}
    {onpaste}
    {onfocus}
    onblur={(e) => {
      syncScroll();
      onblur?.(e);
    }}
    onmousemove={onmove}
    onmouseleave={scheduleHide}
  />
  {#if tip}
    <div
      class="tip"
      style:left="{tip.x}px"
      role="tooltip"
      onmouseenter={keepTip}
      onmouseleave={scheduleHide}
    >
      {#if tip.seg.kind === "var"}
        {#if tip.seg.info.secret && !tip.reveal}
          <button type="button" class="reveal" onclick={() => tip && (tip = { ...tip, reveal: true })}>
            <span class="mono">••••</span> · click to reveal
          </button>
        {:else}
          <span class="mono val">{tip.seg.info.value || "(empty)"}</span>
        {/if}
        <span class="src">· from {tip.seg.info.environmentName}</span>
      {:else}
        <span>Unresolved variable <b class="mono">{tip.seg.text}</b> — not defined in the active environment or Global</span>
      {/if}
    </div>
  {/if}
</div>

<style>
  .vi {
    position: relative;
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
  }
  input {
    width: 100%;
    min-width: 0;
    border: 1px solid transparent;
    background: transparent;
    outline: none;
    color: var(--text-1);
    font: var(--fs-code) / 1 var(--font-mono);
    padding: 0 var(--sp-4);
    position: relative;
    z-index: 1;
  }
  input.clear {
    color: transparent;
    caret-color: var(--text-1);
  }
  input.clear::selection {
    color: transparent;
    background: var(--accent-soft);
  }
  input::placeholder {
    color: var(--text-3);
  }
  .overlay {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    padding: 0 var(--sp-4);
    border: 1px solid transparent;
    font: var(--fs-code) / 1 var(--font-mono);
    color: var(--text-1);
    white-space: pre;
    overflow: hidden;
    pointer-events: none;
  }
  .var {
    color: var(--var-fg);
    background: var(--var-bg);
    border-radius: var(--r-xs);
    box-shadow: 0 0 0 1px var(--var-bg);
  }
  .var-bad {
    color: var(--var-unresolved-fg);
    background: var(--var-unresolved-bg);
    border-radius: var(--r-xs);
    text-decoration: underline wavy;
    text-underline-offset: 3px;
  }
  /* variants */
  .url {
    height: 100%;
  }
  .url input,
  .url .overlay {
    height: 100%;
  }
  .field input {
    height: var(--h-control);
    border-radius: var(--r-sm);
    background: var(--bg-input);
  }
  .field input.clear {
    background: transparent;
  }
  .field:has(input.clear) {
    background: var(--bg-input);
    border-radius: var(--r-sm);
  }
  .field input:hover {
    border-color: var(--border-strong);
  }
  .field input:focus {
    border-color: var(--border-focus);
  }
  .cell input {
    height: 26px;
    padding: 0 var(--sp-3);
    border-radius: var(--r-xs);
  }
  .cell .overlay {
    padding: 0 var(--sp-3);
  }
  .cell input:hover {
    background: var(--bg-hover);
  }
  .cell input:focus {
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-focus);
  }
  .cell input.clear:focus,
  .cell input.clear:hover {
    background: transparent;
  }
  .invalid input {
    border-color: var(--danger) !important;
  }
  .strike input,
  .strike .overlay {
    color: var(--text-3);
    text-decoration: line-through;
  }
  .strike input.clear {
    color: transparent;
  }
  /* DR-011: a disabled row isn't sent, so its vars raise no error */
  .strike .var,
  .strike .var-bad {
    color: var(--text-3);
    background: none;
    box-shadow: none;
    text-decoration: line-through;
  }
  .tip {
    position: absolute;
    top: calc(100% + 4px);
    z-index: var(--z-tooltip);
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    max-width: 420px;
    padding: 4px 8px;
    border-radius: var(--r-sm);
    background: var(--bg-raised);
    box-shadow: var(--shadow-popover);
    font-size: var(--fs-sm);
    color: var(--text-1);
    white-space: nowrap;
    animation: pop-in var(--dur-base) var(--ease-out);
  }
  .tip .val {
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .tip .src {
    color: var(--text-3);
  }
  .reveal {
    border: 0;
    background: none;
    padding: 0;
    color: var(--accent-fg);
    font-size: var(--fs-sm);
  }
</style>

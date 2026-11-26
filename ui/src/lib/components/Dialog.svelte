<script lang="ts">
  import { onMount, type Snippet } from "svelte";
  import Icon from "./Icon.svelte";

  /** Modal dialog shell (DESIGN §5.11): focus trap, Esc closes, ⌘/Ctrl+Enter submits. */
  let {
    title,
    width = 520,
    height,
    testid,
    onclose,
    onsubmit,
    initialFocus,
    children,
    footer,
  }: {
    title: string;
    width?: number;
    height?: number;
    testid?: string;
    onclose: () => void;
    onsubmit?: () => void;
    /** CSS selector inside the dialog to focus first. */
    initialFocus?: string;
    children: Snippet;
    footer?: Snippet;
  } = $props();

  let el: HTMLDivElement;
  const returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;
  const titleId = `dlg-${Math.random().toString(36).slice(2)}`;

  const FOCUSABLE = 'button:not([disabled]), [href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"]), [contenteditable="true"]';

  onMount(() => {
    const target = (initialFocus && el.querySelector<HTMLElement>(initialFocus)) || el.querySelector<HTMLElement>(`.body ${FOCUSABLE}`) || el;
    target.focus();
    return () => returnTo?.focus();
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      onclose();
    } else if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && onsubmit) {
      e.preventDefault();
      e.stopPropagation();
      onsubmit();
    } else if (e.key === "Tab") {
      const f = [...el.querySelectorAll<HTMLElement>(FOCUSABLE)].filter((x) => x.offsetParent !== null);
      if (!f.length) return;
      const first = f[0];
      const last = f[f.length - 1];
      if (e.shiftKey && document.activeElement === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  }
</script>

<div class="overlay" role="presentation" onmousedown={(e) => e.target === e.currentTarget && onclose()}>
  <div
    class="dialog pop"
    role="dialog"
    aria-modal="true"
    aria-labelledby={titleId}
    tabindex="-1"
    bind:this={el}
    style:width="min({width}px, calc(100vw - 32px))"
    style:height={height ? `min(${height}px, calc(100vh - 32px))` : undefined}
    data-testid={testid}
    {onkeydown}
  >
    <header>
      <h2 id={titleId}>{title}</h2>
      <button class="icon-btn" type="button" aria-label="Close" onclick={onclose} data-testid="dialog-close"><Icon name="x" /></button>
    </header>
    <div class="body">{@render children()}</div>
    {#if footer}<footer>{@render footer()}</footer>{/if}
  </div>
</div>

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: var(--z-dialog);
    background: var(--bg-overlay);
    display: grid;
    place-items: center;
    animation: fade var(--dur-base) var(--ease-out);
  }
  @keyframes fade {
    from {
      background: transparent;
    }
  }
  .dialog {
    /* DR-002: inputs, selects and seg tracks on the raised surface */
    --bg-input: var(--bg-input-raised);
    background: var(--bg-raised);
    border-radius: var(--r-lg);
    box-shadow: var(--shadow-dialog);
    display: flex;
    flex-direction: column;
    max-height: calc(100vh - 32px);
    outline: none;
  }
  header {
    height: 44px;
    flex: none;
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--sp-4) 0 var(--sp-6);
    border-bottom: 1px solid var(--border-subtle);
  }
  h2 {
    margin: 0;
    font-size: var(--fs-lg);
    font-weight: var(--fw-semibold);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .body {
    padding: var(--sp-6);
    overflow: auto;
    /* DR-008: scroll-shadow cue when the body overflows */
    background:
      linear-gradient(var(--bg-raised) 30%, transparent) top / 100% 24px no-repeat local,
      linear-gradient(transparent, var(--bg-raised) 70%) bottom / 100% 24px no-repeat local,
      radial-gradient(farthest-side at 50% 0, var(--border-strong), transparent) top / 100% 6px no-repeat scroll,
      radial-gradient(farthest-side at 50% 100%, var(--border-strong), transparent) bottom / 100% 6px no-repeat scroll;
    background-color: var(--bg-raised);
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  footer {
    height: 52px;
    flex: none;
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: var(--sp-4);
    padding: 0 var(--sp-6);
    border-top: 1px solid var(--border-subtle);
  }
</style>

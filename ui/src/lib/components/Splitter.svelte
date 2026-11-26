<script lang="ts">
  /** Pane splitter (DESIGN §2.2): 1px line, 7px hit area, keyboard resizable, double-click resets. */
  let {
    orientation = "vertical",
    label,
    value,
    min,
    max,
    ondrag,
    ondragend,
    onreset,
    ontoggle,
    testid,
  }: {
    /** "vertical" = a vertical line resizing horizontally */
    orientation?: "vertical" | "horizontal";
    label: string;
    value: number;
    min: number;
    max: number;
    /** pointer delta in px since drag start */
    ondrag: (delta: number, phase: "start" | "move") => void;
    ondragend?: () => void;
    onreset?: () => void;
    ontoggle?: () => void;
    testid?: string;
  } = $props();

  let dragging = $state(false);
  let start = 0;

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    dragging = true;
    start = orientation === "vertical" ? e.clientX : e.clientY;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    ondrag(0, "start");
  }
  function move(e: PointerEvent) {
    if (!dragging) return;
    ondrag((orientation === "vertical" ? e.clientX : e.clientY) - start, "move");
  }
  function up() {
    if (!dragging) return;
    dragging = false;
    ondragend?.();
  }
  function key(e: KeyboardEvent) {
    const step = e.shiftKey ? 64 : 16;
    const dec = orientation === "vertical" ? "ArrowLeft" : "ArrowUp";
    const inc = orientation === "vertical" ? "ArrowRight" : "ArrowDown";
    if (e.key === dec || e.key === inc) {
      e.preventDefault();
      ondrag(0, "start");
      ondrag(e.key === inc ? step : -step, "move");
      ondragend?.();
    } else if (e.key === "Enter" && ontoggle) {
      e.preventDefault();
      ontoggle();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  class="split {orientation}"
  class:dragging
  role="separator"
  aria-orientation={orientation}
  aria-label={label}
  aria-valuenow={Math.round(value)}
  aria-valuemin={min}
  aria-valuemax={max}
  tabindex="0"
  data-testid={testid}
  onpointerdown={down}
  onpointermove={move}
  onpointerup={up}
  onpointercancel={up}
  ondblclick={() => onreset?.()}
  onkeydown={key}
></div>

<style>
  .split {
    position: relative;
    background: var(--border-subtle);
    flex: none;
    z-index: var(--z-splitter);
    touch-action: none;
    transition: background-color var(--dur-fast);
  }
  .vertical {
    width: 1px;
    cursor: col-resize;
  }
  .horizontal {
    height: 1px;
    cursor: row-resize;
  }
  .split::after {
    content: "";
    position: absolute;
  }
  .vertical::after {
    inset: 0 -3px;
  }
  .horizontal::after {
    inset: -3px 0;
  }
  .split:hover,
  .split.dragging,
  .split:focus-visible {
    background: var(--accent-fg);
    outline: none;
  }
  .vertical:hover,
  .vertical.dragging,
  .vertical:focus-visible {
    box-shadow: 1px 0 0 var(--accent-fg);
  }
  .horizontal:hover,
  .horizontal.dragging,
  .horizontal:focus-visible {
    box-shadow: 0 1px 0 var(--accent-fg);
  }
</style>

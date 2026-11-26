<script lang="ts" generics="T">
  import type { Snippet } from "svelte";

  /** Windowed list with per-item fixed heights (history rows, tree rows). */
  let {
    items,
    itemHeight,
    row,
    overscan = 8,
    role = "list",
    label,
    testid,
    onkeydown,
    scrollToIndex = $bindable(-1),
    sticky,
  }: {
    items: T[];
    itemHeight: (item: T) => number;
    row: Snippet<[T, number]>;
    overscan?: number;
    role?: string;
    label?: string;
    testid?: string;
    onkeydown?: (e: KeyboardEvent) => void;
    scrollToIndex?: number;
    /** Optional sticky header for the first visible item. */
    sticky?: Snippet<[T]>;
  } = $props();

  let viewport: HTMLDivElement;
  let scrollTop = $state(0);
  let height = $state(400);

  const offsets = $derived.by(() => {
    const o = new Float64Array(items.length + 1);
    for (let i = 0; i < items.length; i++) o[i + 1] = o[i] + itemHeight(items[i]);
    return o;
  });
  const total = $derived(offsets[items.length] ?? 0);

  function find(y: number): number {
    let lo = 0;
    let hi = items.length;
    while (lo < hi) {
      const mid = (lo + hi) >> 1;
      if (offsets[mid + 1] <= y) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  const range = $derived.by(() => {
    const start = Math.max(0, find(scrollTop) - overscan);
    const end = Math.min(items.length, find(scrollTop + height) + overscan + 1);
    return { start, end };
  });
  const firstVisible = $derived(items.length ? find(scrollTop) : -1);

  $effect(() => {
    const i = scrollToIndex;
    if (i < 0 || !viewport || i >= items.length) return;
    const top = offsets[i];
    const bottom = offsets[i + 1];
    if (top < viewport.scrollTop) viewport.scrollTop = top;
    else if (bottom > viewport.scrollTop + viewport.clientHeight) viewport.scrollTop = bottom - viewport.clientHeight;
    scrollToIndex = -1;
  });

  export function getViewport(): HTMLDivElement {
    return viewport;
  }
</script>

<div
  class="vl"
  bind:this={viewport}
  bind:clientHeight={height}
  onscroll={() => (scrollTop = viewport.scrollTop)}
  {role}
  aria-label={label}
  data-testid={testid}
  tabindex="-1"
  {onkeydown}
>
  {#if sticky && firstVisible >= 0 && scrollTop > 0}
    <div class="sticky">{@render sticky(items[firstVisible])}</div>
  {/if}
  <div class="spacer" style:height="{total}px">
    {#each { length: range.end - range.start } as _, k (range.start + k)}
      {@const i = range.start + k}
      <div class="item" style:top="{offsets[i]}px" style:height="{offsets[i + 1] - offsets[i]}px">
        {@render row(items[i], i)}
      </div>
    {/each}
  </div>
</div>

<style>
  .vl {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    overflow-x: hidden;
    position: relative;
    outline: none;
  }
  .spacer {
    position: relative;
    width: 100%;
  }
  .item {
    position: absolute;
    left: 0;
    right: 0;
  }
  .sticky {
    position: sticky;
    top: 0;
    z-index: 2;
    height: 0;
    overflow: visible;
  }
</style>

<script lang="ts">
  import { onMount } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { copyText } from "../api/platform";
  import { toCommandError } from "../api";

  /** Export-as-cURL popover (AC-CEXP-1/5, DESIGN §5.11). */
  let { anchor }: { anchor: HTMLElement | null } = $props();

  let text = $state("");
  let error = $state<string | null>(null);
  let el: HTMLDivElement;
  let pos = $state({ top: 80, left: 200 });
  const returnTo = document.activeElement instanceof HTMLElement ? document.activeElement : null;

  async function load() {
    try {
      text = await app.exportCurl(app.exportKeepVariables);
      error = null;
    } catch (e) {
      error = toCommandError(e).message;
    }
  }

  onMount(() => {
    void load();
    const r = anchor?.getBoundingClientRect();
    if (r) pos = { top: r.bottom + 6, left: Math.max(8, Math.min(r.right - 480, window.innerWidth - 488)) };
    el.focus();
    return () => returnTo?.focus();
  });

  async function copy() {
    await copyText(text);
    toasts.success("cURL copied to clipboard");
    ui.exportCurl = null;
  }
</script>

<svelte:window onmousedown={(e) => el && !el.contains(e.target as Node) && (ui.exportCurl = null)} />

<div
  class="pop-ex menu pop"
  role="dialog"
  aria-label="Export as cURL"
  tabindex="-1"
  bind:this={el}
  style:top="{pos.top}px"
  style:left="{pos.left}px"
  data-testid="export-curl"
  onkeydown={(e) => {
    if (e.key === "Escape") {
      e.stopPropagation();
      ui.exportCurl = null;
    }
  }}
>
  <pre class="code selectable" data-testid="export-curl-text">{error ?? text}</pre>
  <div class="row">
    <label class="checkbox">
      <input
        type="checkbox"
        checked={app.exportKeepVariables}
        onchange={(e) => {
          app.exportKeepVariables = e.currentTarget.checked;
          app.updateSettings({ exportKeepVariables: e.currentTarget.checked });
          void load();
        }}
        data-testid="export-keep-vars"
      />Keep <span class="mono">{"{{variables}}"}</span>
    </label>
    <span class="grow"></span>
    <button class="btn btn-primary" type="button" onclick={copy} disabled={!text} data-testid="export-copy">Copy</button>
  </div>
</div>

<style>
  .pop-ex {
    position: fixed;
    z-index: var(--z-popover);
    width: 480px;
    padding: var(--sp-5);
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    outline: none;
  }
  .code {
    margin: 0;
    max-height: 240px;
    overflow: auto;
    background: var(--bg-input);
    border-radius: var(--r-sm);
    padding: var(--sp-4);
    font: var(--fs-code) / var(--lh-code) var(--font-mono);
    white-space: pre-wrap;
    word-break: break-all;
  }
  .row {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
  }
</style>

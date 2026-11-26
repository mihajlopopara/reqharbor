<script lang="ts">
  import { onMount } from "svelte";
  import type { EditorController, EditorLanguage } from "../cm/editor";
  import type { VarLookup } from "../util/vars";

  let {
    value = "",
    language = "text",
    readOnly = false,
    wrap = false,
    plain = false,
    lookup = null,
    placeholder = "",
    ariaLabel = "Editor",
    testid,
    onchange,
    controller = $bindable(null),
  }: {
    value?: string;
    language?: EditorLanguage;
    readOnly?: boolean;
    wrap?: boolean;
    plain?: boolean;
    lookup?: VarLookup | null;
    placeholder?: string;
    ariaLabel?: string;
    testid?: string;
    onchange?: (v: string) => void;
    controller?: EditorController | null;
  } = $props();

  let host: HTMLDivElement;
  let ctl = $state<EditorController | null>(null);
  /** Last value known to be in the editor (avoids doc.toString() on every update). */
  let known = "";

  onMount(() => {
    let destroyed = false;
    void import("../cm/editor").then(async (m) => {
      if (destroyed) return;
      known = value;
      const c = await m.createEditor(host, {
        value,
        language,
        readOnly,
        wrap,
        plain,
        lookup,
        placeholder,
        ariaLabel,
        onChange: (v) => {
          known = v;
          onchange?.(v);
        },
      });
      if (destroyed) return c.destroy();
      ctl = c;
      controller = c;
    });
    return () => {
      destroyed = true;
      ctl?.destroy();
      controller = null;
    };
  });

  $effect(() => {
    const v = value;
    if (ctl && v !== known) {
      known = v;
      ctl.setValue(v);
    }
  });
  $effect(() => {
    const l = language;
    const p = plain;
    void ctl?.setLanguage(l, p);
  });
  $effect(() => {
    ctl?.setWrap(wrap);
  });
  $effect(() => {
    ctl?.setLookup(lookup);
  });
</script>

<div class="cm-wrap" data-testid={testid}>
  <div class="cm-host" bind:this={host}></div>
  {#if !ctl}
    <pre class="fallback">{value.length > 20000 ? value.slice(0, 20000) : value}</pre>
  {/if}
</div>

<style>
  .cm-wrap {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .cm-host {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }
  .cm-host :global(.cm-editor) {
    flex: 1;
    min-height: 0;
  }
  .fallback {
    position: absolute;
    inset: 0;
    margin: 0;
    padding: 4px 12px 16px 44px;
    font: var(--fs-code) / var(--lh-code) var(--font-mono);
    color: var(--text-1);
    overflow: hidden;
    white-space: pre;
  }
</style>

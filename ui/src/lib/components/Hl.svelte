<script lang="ts">
  import { highlightParts } from "../util/highlight";
  let { text, query }: { text: string; query: string } = $props();
  const parts = $derived(highlightParts(text, query));
</script>

{#each parts as p, i (i)}{#if p.m}<mark>{p.t}</mark>{:else}{p.t}{/if}{/each}

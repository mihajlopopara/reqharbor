<script lang="ts">
  import { tick } from "svelte";
  import type { HistorySummary } from "../api/types";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { historyGroup, methodClass, methodLabel, relativeTime, statusClass, stripScheme, ERROR_LABEL, formatMs, formatDuration } from "../util/format";
  import { historyMatches } from "../util/highlight";
  import { call } from "../api";
  import VirtualList from "./VirtualList.svelte";
  import Hl from "./Hl.svelte";
  import Icon from "./Icon.svelte";

  type Item = { type: "group"; label: string } | { type: "row"; h: HistorySummary; idx: number };

  const filter = $derived(ui.filters.activity);
  const visible = $derived(app.history.filter((h) => !app.hiddenHistory.has(h.id) && historyMatches(h, filter)));
  const items = $derived.by(() => {
    const out: Item[] = [];
    let last = "";
    visible.forEach((h, idx) => {
      const g = historyGroup(h.timestamp, app.now);
      if (g !== last) {
        out.push({ type: "group", label: g });
        last = g;
      }
      out.push({ type: "row", h, idx });
    });
    return out;
  });
  /** DR-004: host truncates first so the path (and filter matches in it) stays visible. */
  function splitHostPath(url: string): { host: string; path: string } {
    const s = stripScheme(url);
    const i = s.search(/[/?#]/);
    return i < 0 ? { host: s, path: "" } : { host: s.slice(0, i), path: s.slice(i) };
  }

  let focusIdx = $state(0);
  let scrollTo = $state(-1);

  function itemIndexOf(rowIdx: number): number {
    return items.findIndex((it) => it.type === "row" && it.idx === rowIdx);
  }

  async function focusRow(rowIdx: number) {
    focusIdx = Math.max(0, Math.min(rowIdx, visible.length - 1));
    scrollTo = itemIndexOf(focusIdx);
    await tick();
    document.querySelector<HTMLElement>(`[data-testid="history-row"][data-index="${focusIdx}"]`)?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (!visible.length) return;
    if (e.key === "ArrowDown") void focusRow(focusIdx + 1);
    else if (e.key === "ArrowUp") void focusRow(focusIdx - 1);
    else if (e.key === "Home") void focusRow(0);
    else if (e.key === "End") void focusRow(visible.length - 1);
    else if (e.key === "Enter" || e.key === " ") void app.openHistory(visible[focusIdx].id);
    else if (e.key === "Delete" || (e.key === "Backspace" && (e.metaKey || e.ctrlKey))) {
      app.deleteHistory(visible[focusIdx].id);
      void focusRow(focusIdx);
    } else if ((e.key === "F10" && e.shiftKey) || e.key === "ContextMenu") {
      const el = document.querySelector<HTMLElement>(`[data-testid="history-row"][data-index="${focusIdx}"]`);
      if (el) rowMenu(visible[focusIdx], el);
    } else return;
    e.preventDefault();
  }

  function rowMenu(h: HistorySummary, anchor: HTMLElement | { x: number; y: number }) {
    ui.openMenu(
      [
        { label: "Open", action: () => void app.openHistory(h.id) },
        { label: "Save to collection…", action: () => (ui.picker = { kind: "saveHistory", historyId: h.id }), testid: "history-save" },
        {
          label: "Copy as cURL",
          action: async () => {
            const { entry } = await call("open_history_entry", { historyId: h.id });
            await app.copyRequestCurl(entry.request);
          },
        },
        { separator: true },
        { label: "Delete", shortcut: "Del", danger: true, action: () => app.deleteHistory(h.id), testid: "history-delete" },
      ],
      anchor,
      { alignRight: anchor instanceof HTMLElement },
    );
  }
</script>

{#if app.history.length === 0}
  <div class="empty" data-testid="activity-empty">
    <h3>No requests yet</h3>
    <p>Send a request and it lands here.</p>
  </div>
{:else if visible.length === 0}
  <div class="empty">
    <p>No matches for “{filter}”</p>
    <button class="link" type="button" onclick={() => (ui.filters.activity = "")}>Clear filter</button>
  </div>
{:else}
  <VirtualList {items} itemHeight={(it) => (it.type === "group" ? 30 : 52)} role="listbox" label="History" testid="history-list" {onkeydown} bind:scrollToIndex={scrollTo}>
    {#snippet row(it)}
      {#if it.type === "group"}
        <div class="group" role="presentation">{it.label}</div>
      {:else}
        {@const h = it.h}
        {@const hp = splitHostPath(h.resolvedUrl)}
        <div
          class="hrow"
          role="option"
          aria-selected={ui.selectedHistoryId === h.id}
          tabindex={it.idx === focusIdx ? 0 : -1}
          data-testid="history-row"
          data-index={it.idx}
          data-method={h.method}
          title={h.resolvedUrl}
          onclick={() => {
            focusIdx = it.idx;
            void app.openHistory(h.id);
          }}
          oncontextmenu={(e) => {
            e.preventDefault();
            rowMenu(h, { x: e.clientX, y: e.clientY });
          }}
          onkeydown={() => {}}
        >
          <span class="m {h.ws ? 'm-ws' : h.grpc ? 'm-grpc' : methodClass(h.method)}" title={h.ws ? (h.method === "SIO" ? "Socket.IO session" : "WebSocket session") : h.grpc ? "gRPC call" : h.method}><Hl text={h.grpc ? "gRPC" : methodLabel(h.method)} query={filter} /></span>
          <span class="hurl"
            ><span class="host"><Hl text={hp.host} query={filter} /></span>{#if hp.path}<span class="path"><Hl text={hp.path} query={filter} /></span>{/if}</span
          >
          <span class="meta">
            <span>{relativeTime(h.timestamp, app.now)}</span>
            {#if h.ws}
              <span class="code-chip ws-counts" title="{h.ws.sent} sent · {h.ws.received} received" data-testid="history-ws">↑{h.ws.sent} ↓{h.ws.received}</span>
            {:else if h.grpc}
              {#if h.grpc.callKind !== "unary"}<span class="code-chip ws-counts" title="{h.grpc.sent} sent · {h.grpc.received} received">↑{h.grpc.sent} ↓{h.grpc.received}</span>{/if}
              {#if h.grpc.status}
                <span class="code-chip {h.grpc.status.code === 0 ? 's2' : 'serr'}" title={h.grpc.status.message || h.grpc.status.name} data-testid="history-grpc-status"><Hl text={h.grpc.status.name} query={filter} /></span>
              {:else if h.errorKind}
                <span class="code-chip {h.errorKind === 'cancelled' ? 'scan' : 'serr'}">{ERROR_LABEL[h.errorKind]}</span>
              {/if}
            {:else if h.status !== null}
              <span class="code-chip {statusClass(h.status)}" data-testid="history-status"><Hl text={String(h.status)} query={filter} /></span>
            {:else if h.errorKind}
              <span class="code-chip {h.errorKind === 'cancelled' ? 'scan' : 'serr'}">{ERROR_LABEL[h.errorKind]}</span>
            {/if}
            {#if h.timeMs !== null}<span class="dur">{formatMs(h.timeMs)}</span>{:else if h.ws}<span class="dur">{formatDuration(h.ws.durationMs)}</span>{:else if h.grpc}<span class="dur">{h.grpc.callKind === "unary" ? formatMs(h.grpc.durationMs) : formatDuration(h.grpc.durationMs)}</span>{/if}
          </span>
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <span
            class="icon-btn more"
            aria-hidden="true"
            onclick={(e) => {
              e.stopPropagation();
              rowMenu(h, e.currentTarget);
            }}><Icon name="more" /></span
          >
        </div>
      {/if}
    {/snippet}
    {#snippet sticky(it)}
      {@const label = it.type === "group" ? it.label : historyGroup(it.h.timestamp, app.now)}
      <div class="group stick">{label}</div>
    {/snippet}
  </VirtualList>
{/if}

<style>
  .group {
    height: 30px;
    display: flex;
    align-items: flex-end;
    padding: 0 var(--sp-5) var(--sp-2);
    font-size: var(--fs-xs);
    font-weight: var(--fw-semibold);
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--text-3);
    background: var(--bg-sidebar);
  }
  .stick {
    box-shadow: 0 1px 0 var(--border-subtle);
  }
  .hrow {
    height: var(--h-row-history);
    display: grid;
    grid-template-columns: var(--w-method-col) 1fr;
    align-content: center;
    row-gap: 4px;
    padding: 0 var(--sp-4) 0 var(--sp-5);
    border-bottom: 1px solid var(--border-subtle);
    box-sizing: border-box;
    cursor: default;
    position: relative;
    outline: none;
  }
  .hrow:hover {
    background: var(--bg-hover);
  }
  .hrow[aria-selected="true"] {
    background: var(--bg-selected-muted);
  }
  :global(.vl:focus-within) .hrow[aria-selected="true"] {
    background: var(--bg-selected);
  }
  .hrow:focus-visible {
    box-shadow: inset 0 0 0 2px var(--focus-ring);
  }
  .hrow[aria-selected="true"] .meta {
    color: var(--text-2);
  }
  .hrow .m {
    align-self: center;
    overflow: hidden;
  }
  .hurl {
    display: flex;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
  }
  .hrow:hover .hurl,
  .hrow:focus-within .hurl {
    padding-right: 20px;
  }
  .host {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-2);
  }
  .path {
    flex: 0 0 auto;
    min-width: 0;
    max-width: 80%;
    overflow: hidden;
    text-overflow: ellipsis;
    color: var(--text-1);
  }
  .meta {
    grid-column: 2;
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    font-size: var(--fs-sm);
    line-height: var(--lh-tight);
    color: var(--text-3);
    font-variant-numeric: tabular-nums;
  }
  .more {
    position: absolute;
    right: var(--sp-3);
    top: 50%;
    transform: translateY(-50%);
    opacity: 0;
  }
  .hrow:hover .more,
  .hrow:focus-within .more {
    opacity: 1;
  }
</style>

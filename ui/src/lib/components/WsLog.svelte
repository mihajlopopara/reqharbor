<script lang="ts">
  import { tick } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ws, type LogEntry } from "../stores/ws.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { copyText } from "../api/platform";
  import { dateTime, formatDuration, formatSize } from "../util/format";
  import { shortcutKeys } from "../util/shortcuts";
  import { EMPTY_FILTER, FILTER_HINT, MAX_FRAME_DATA, SEARCH_PREVIEW, filterLog, frameCopyText, frameDetail, frameSummary, rowHeight, summaryText, timeOfDay, type DirFilter, type LogFilter } from "../util/ws";
  import VirtualList from "./VirtualList.svelte";
  import Icon from "./Icon.svelte";

  /** The response pane of a WebSocket / Socket.IO request and of gRPC streams: the message log (AC-WS-5, AC-GRPC-8). */
  let { stacked, ontogglelayout }: { stacked: boolean; ontogglelayout: () => void } = $props();

  let filter = $state<LogFilter>({ ...EMPTY_FILTER });
  let expanded = $state<ReadonlySet<number>>(new Set());
  let follow = $state(true);
  let list = $state<VirtualList<LogEntry>>();
  let scrollTo = $state(-1);
  let now = $state(Date.now());

  const r = $derived(app.request);
  const session = $derived(ws.session(r.id));
  /**
   * A fresh object per flush (`rev`): the log array grows in place (no copy per animation frame), so
   * `rev` is what tells the template and the list (its `version`) that it changed.
   */
  const view = $derived.by(() => {
    const rev = session?.rev ?? 0;
    const all = session ? ws.frames(r.id) : [];
    return { rev, all, shown: filterLog(all, filter), count: all.length };
  });
  const shown = $derived(view.shown);
  const shownCount = $derived(view.shown.length);
  const history = $derived(!session ? (app.currentResponse?.fromHistory ? app.currentResponse : null) : null);
  const live = $derived(session ? session.state !== "closed" : false);
  /** A gRPC stream (live or from history). */
  const isGrpc = $derived(session ? session.transport === "grpc" : app.request.kind === "grpc");
  const grpcSummary = $derived(history?.grpc?.summary ?? null);
  function reconnect() {
    void (isGrpc ? app.grpcRun() : app.wsConnect());
  }
  const filtered = $derived(filter.text.trim() !== "" || filter.dir !== "all" || filter.hideHeartbeats);

  const DIRS: { v: DirFilter; label: string }[] = [
    { v: "all", label: "All" },
    { v: "in", label: "Received" },
    { v: "out", label: "Sent" },
    { v: "system", label: "System" },
  ];

  // Duration ticker while connected.
  $effect(() => {
    if (!live) return;
    now = Date.now();
    const t = setInterval(() => (now = Date.now()), 1000);
    return () => clearInterval(t);
  });

  // Keep the newest frame in view while following.
  $effect(() => {
    const n = shownCount;
    if (!follow || !n) return;
    void tick().then(() => (scrollTo = n - 1));
  });

  // A new connection: collapse everything, follow again.
  let lastConn = "";
  $effect(() => {
    const id = session?.connectionId ?? "";
    if (id === lastConn) return;
    lastConn = id;
    expanded = new Set();
    follow = true;
  });

  function onScroll() {
    const vp = list?.getViewport();
    if (!vp) return;
    follow = vp.scrollHeight - vp.scrollTop - vp.clientHeight < 40;
  }

  function toggle(seq: number) {
    const s = new Set(expanded);
    if (s.has(seq)) s.delete(seq);
    else s.add(seq);
    expanded = s;
  }

  async function copyFrame(f: LogEntry) {
    await copyText(frameCopyText(f));
    toasts.success("Message copied");
  }

  async function copyAll() {
    const text = shown.map((f) => `${timeOfDay(f.ts)} ${f.dir === "in" ? "←" : f.dir === "out" ? "→" : "•"} ${f.kind}${f.event ? ` ${f.event}` : ""} ${f.data}`).join("\n");
    await copyText(text);
    toasts.success(`${shown.length} message${shown.length === 1 ? "" : "s"} copied`);
  }

  const stateLabel = $derived.by(() => {
    switch (session?.state) {
      case "connecting":
        return "Connecting";
      case "open":
        return "Connected";
      case "closing":
        return "Closing";
      case "closed":
        if (session.status) return session.status.code === 0 ? "Finished" : "Failed";
        return session.error ? "Failed" : "Closed";
      default:
        return "";
    }
  });
  const stateClass = $derived(
    session?.state === "open" ? "s2" : session?.state === "connecting" || session?.state === "closing" ? "s3" : session?.status ? (session.status.code === 0 ? "s2" : "serr") : session?.error ? "serr" : "scan",
  );
  const duration = $derived(session ? Math.max(0, (session.endedAt ?? now) - session.startedAt) : 0);
  /** While live the strip ticks once a second: whole seconds read calmer than milliseconds. */
  const durationText = $derived(live && duration < 60_000 ? `${Math.floor(duration / 1000)} s` : formatDuration(duration));

  const DIR_LABEL = { in: "Received", out: "Sent", system: "System" } as const;
  const KIND_LABEL: Record<string, string> = {
    text: "Text",
    binary: "Binary",
    ping: "Ping",
    pong: "Pong",
    close: "Close",
    event: "Event",
    ack: "Ack",
    error: "Error",
    open: "Open",
    reconnecting: "Retry",
    info: "Info",
    message: "Message",
    headers: "Headers",
    trailers: "Trailers",
    end: "End",
    status: "Status",
  };
  const shortcuts = $derived([
    { label: "Connect / Send message", keys: shortcutKeys("Mod+Enter", app.platform) },
    { label: "Change Environment", keys: shortcutKeys("Mod+E", app.platform) },
    { label: "New Request", keys: shortcutKeys("Mod+N", app.platform) },
  ]);
</script>

<section class="pane res" aria-label="Messages" data-testid="ws-log-pane">
  <div class="row1">
    <span class="faint grow info" data-testid="ws-log-info">
      {#if session}
        {#if session.method === "GRPC"}
          gRPC · <span class="mono">{session.title}</span> · <span class="mono" title={session.resolvedUrl}>{session.resolvedUrl.replace(/\/[^/]+\/[^/]+$/, "")}</span>
        {:else}
          {session.method === "SIO" ? "Socket.IO" : "WebSocket"} · <span class="mono" title={session.resolvedUrl}>{session.resolvedUrl}</span>
        {/if}
      {:else if history?.fromHistory}
        From history · {dateTime(history.fromHistory)}
        <button class="link" type="button" onclick={reconnect} data-testid="ws-reconnect">Reconnect</button>
      {/if}
    </span>
    <button class="icon-btn" type="button" aria-label="Copy messages" title="Copy the shown messages" disabled={!shownCount} onclick={copyAll} data-testid="ws-copy-all"><Icon name="copy" /></button>
    <button class="icon-btn" type="button" aria-label="Clear messages" title="Clear the log" disabled={!view.count} onclick={() => ws.clear(r.id)} data-testid="ws-clear"><Icon name="trash" /></button>
    <button
      class="icon-btn"
      type="button"
      aria-label={stacked ? "Show messages side by side" : "Stack messages below"}
      title={stacked ? "Show messages side by side" : "Stack messages below"}
      onclick={ontogglelayout}
      data-testid="layout-toggle"><Icon name={stacked ? "cols" : "rows"} /></button
    >
  </div>

  <div class="row2">
    <div class="strip" aria-live="polite" data-testid="ws-status-strip">
      {#if session}
        <span class="status-pill {stateClass}" data-testid="ws-state" data-state={session.state}>{stateLabel}</span>
        <span>Time <b data-testid="ws-duration">{durationText}</b></span>
        <span title="Data messages sent / received">Messages <b data-testid="ws-counts">↑{session.sent} ↓{session.received}</b></span>
        {#if session.status}
          <span class="status-pill {session.status.code === 0 ? 's2' : 'serr'}" title={session.status.message || undefined} data-testid="grpc-stream-status" data-code={session.status.code}>{session.status.code} {session.status.name}</span>
        {/if}
        {#if session.unresolved.length}
          <span class="unres" title={session.unresolved.map((u) => `{{${u}}}`).join(", ")} data-testid="unresolved-warning"><Icon name="alert" small />{session.unresolved.length} unresolved variable{session.unresolved.length === 1 ? "" : "s"}</span>
        {/if}
      {:else if history?.wsSummary}
        <span class="status-pill {history.error ? 'serr' : 'scan'}">{history.error ? "Failed" : "Closed"}</span>
        <span data-testid="ws-history-summary">{summaryText(history.wsSummary)}</span>
      {:else if grpcSummary}
        {#if grpcSummary.status}
          <span class="status-pill {grpcSummary.status.code === 0 ? 's2' : 'serr'}" data-testid="grpc-stream-status">{grpcSummary.status.code} {grpcSummary.status.name}</span>
        {:else}
          <span class="status-pill {history?.error ? 'serr' : 'scan'}">{history?.error ? "Failed" : "Closed"}</span>
        {/if}
        <span data-testid="ws-history-summary">{summaryText(grpcSummary)}</span>
      {/if}
    </div>
  </div>
  {#if session?.state === "connecting"}<div class="loading" aria-hidden="true"></div>{/if}

  {#if !session}
    {#if history?.error}
      <div class="pad">
        <div class="err" role="alert" data-testid="ws-history-error"><h3><Icon name="alert" />Connection failed</h3><code class="selectable">{history.error.message}</code></div>
      </div>
    {:else}
      <div class="empty cheat" data-testid="ws-log-empty">
        <p class="faint">{history ? "Messages of past sessions aren't kept. Reconnect to start a new one." : isGrpc ? "Invoke or connect to see messages here." : "Connect to see messages here."}</p>
        <div class="shortcuts">
          {#each shortcuts as s (s.label)}
            <span>{s.label}</span>
            <span>{#each s.keys as k (k)}<span class="kbd">{k}</span>{/each}</span>
          {/each}
        </div>
      </div>
    {/if}
  {:else}
    <div class="filters">
      <label class="search">
        <Icon name="search" small />
        <input
          class="input"
          placeholder="Filter messages (first {formatSize(SEARCH_PREVIEW)} of each)"
          aria-label="Filter messages"
          title={FILTER_HINT}
          value={filter.text}
          data-testid="ws-filter"
          oninput={(e) => (filter = { ...filter, text: e.currentTarget.value })}
          onkeydown={(e) => {
            if (e.key === "Escape" && filter.text) {
              e.stopPropagation();
              filter = { ...filter, text: "" };
            }
          }}
        />
      </label>
      <span class="seg" role="group" aria-label="Direction">
        {#each DIRS as d (d.v)}
          <button type="button" aria-pressed={filter.dir === d.v} onclick={() => (filter = { ...filter, dir: d.v })} data-testid="ws-dir-{d.v}">{d.label}</button>
        {/each}
      </span>
      {#if !isGrpc}
        <label class="checkbox small" title="Hide ping and pong frames">
          <input type="checkbox" checked={filter.hideHeartbeats} onchange={(e) => (filter = { ...filter, hideHeartbeats: e.currentTarget.checked })} data-testid="ws-hide-pings" />Hide pings
        </label>
      {/if}
    </div>

    {#if session.error && session.state === "closed" && !view.all.some((f) => f.kind === "open")}
      <div class="pad">
        <div class="err" role="alert" data-testid="ws-error"><h3><Icon name="alert" />Couldn't connect</h3><code class="selectable">{session.error}</code>
          <div class="actions"><button class="btn btn-secondary btn-sm" type="button" onclick={reconnect}>Retry</button>
          {#if /tls/i.test(session.error) && app.request.settings.verifyTls}
            <button class="btn btn-ghost btn-sm" type="button" onclick={() => { app.request.settings.verifyTls = false; toasts.info("TLS verification disabled for this request"); }}>Disable TLS verification for this request</button>
          {/if}</div>
        </div>
      </div>
    {/if}

    {#if shownCount === 0}
      <div class="empty"><p class="faint" data-testid="ws-log-none">{filtered ? "No messages match the filter" : session.state === "connecting" ? "Connecting…" : "No messages yet"}</p></div>
    {:else}
      <div class="log" onscrollcapture={onScroll}>
        <VirtualList bind:this={list} items={shown} itemHeight={(f) => rowHeight(f, expanded.has(f.seq))} label="Messages" testid="ws-log" version={view.rev} bind:scrollToIndex={scrollTo}>
          {#snippet row(f)}
            {@const open = expanded.has(f.seq)}
            <div class="frame d-{f.dir} k-{f.kind}" class:open data-testid="ws-frame" data-dir={f.dir} data-kind={f.kind}>
              <button class="line" type="button" aria-expanded={open} onclick={() => toggle(f.seq)} title={open ? "Collapse" : "Expand"}>
                <span class="dir" title={DIR_LABEL[f.dir]}><Icon name={f.dir === "in" ? "dir-in" : f.dir === "out" ? "dir-out" : "dir-sys"} small label={DIR_LABEL[f.dir]} /></span>
                <span class="ts mono">{timeOfDay(f.ts)}</span>
                <span class="kind">{f.kind === "event" && f.event ? "" : KIND_LABEL[f.kind] ?? f.kind}</span>
                <span class="sum mono" data-testid="ws-frame-text">{#if f.kind === "event" && f.event}<span class="m m-ws ev">{f.event}</span> {/if}{f.kind === "event" && f.event ? frameSummary(f).slice(f.event.length).trim() : frameSummary(f)}</span>
                {#if f.size}<span class="size faint" title={f.truncated ? `Cut at ${formatSize(MAX_FRAME_DATA)} in the log` : undefined}>{formatSize(f.size)}{f.truncated ? " ✂" : ""}</span>{/if}
              </button>
              {#if open}
                {@const d = frameDetail(f)}
                <div class="detail">
                  {#if f.truncated}<p class="cut faint" data-testid="ws-frame-truncated">Showing the first {formatSize(MAX_FRAME_DATA)} of {formatSize(f.size)}</p>{/if}
                  <pre class="mono selectable" class:json={d.json} data-testid="ws-frame-detail">{d.text}</pre>
                  <button class="icon-btn copy" type="button" aria-label="Copy message" title="Copy" onclick={() => void copyFrame(f)} data-testid="ws-frame-copy"><Icon name="copy" small /></button>
                </div>
              {/if}
            </div>
          {/snippet}
        </VirtualList>
        {#if !follow}
          <button class="btn btn-secondary btn-sm jump" type="button" onclick={() => { follow = true; scrollTo = shownCount - 1; }} data-testid="ws-follow">Jump to latest</button>
        {/if}
      </div>
    {/if}
  {/if}
</section>

<style>
  .res {
    display: flex;
    flex-direction: column;
    min-width: 0;
    min-height: 0;
    height: 100%;
    background: var(--bg-pane);
    position: relative;
  }
  .info {
    font-size: var(--fs-sm);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .strip {
    display: flex;
    align-items: center;
    gap: var(--sp-6);
    font-size: var(--fs-md);
    color: var(--text-2);
    white-space: nowrap;
    min-width: 0;
  }
  .strip b {
    font-weight: var(--fw-medium);
    color: var(--text-1);
    font-family: var(--font-mono);
    font-variant-numeric: tabular-nums;
  }
  .unres {
    display: inline-flex;
    gap: var(--sp-2);
    align-items: center;
    color: var(--warning);
  }
  .loading {
    height: 2px;
    margin-top: -2px;
    background: linear-gradient(90deg, transparent, var(--accent-fg), transparent);
    background-size: 40% 100%;
    background-repeat: no-repeat;
    animation: slide 1s linear infinite;
  }
  @keyframes slide {
    from {
      background-position: -40% 0;
    }
    to {
      background-position: 140% 0;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .loading {
      display: none;
    }
  }
  .cheat {
    flex: 1;
    gap: var(--sp-6);
  }
  .shortcuts {
    display: grid;
    grid-template-columns: auto auto;
    gap: var(--sp-4) var(--sp-8);
    align-items: center;
    font-size: var(--fs-md);
  }
  .shortcuts > span:nth-child(odd) {
    color: var(--text-2);
    text-align: right;
  }
  .shortcuts > span:nth-child(even) {
    display: flex;
    gap: 3px;
  }
  .filters {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    padding: var(--sp-2) var(--sp-5) var(--sp-3);
    border-bottom: 1px solid var(--border-subtle);
    flex: none;
    flex-wrap: wrap;
  }
  .search {
    position: relative;
    flex: 1;
    min-width: 140px;
    display: flex;
    align-items: center;
    color: var(--text-3);
  }
  .search :global(svg) {
    position: absolute;
    left: 8px;
    pointer-events: none;
  }
  .search .input {
    width: 100%;
    height: 26px;
    padding-left: 26px;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .pad {
    padding: var(--sp-4) var(--sp-5) 0;
  }
  .actions {
    display: flex;
    gap: var(--sp-4);
    flex-wrap: wrap;
  }
  .log {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    position: relative;
  }
  .frame {
    height: 100%;
    display: flex;
    flex-direction: column;
    border-bottom: 1px solid var(--border-subtle);
  }
  .line {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    height: 30px;
    flex: none;
    width: 100%;
    padding: 0 var(--sp-5);
    border: 0;
    background: none;
    text-align: left;
    color: var(--text-1);
    font-size: var(--fs-md);
    min-width: 0;
  }
  .line:hover {
    background: var(--bg-hover);
  }
  .dir {
    flex: none;
    display: inline-flex;
    color: var(--text-3);
  }
  .d-in .dir {
    color: var(--success);
  }
  .d-out .dir {
    color: var(--info);
  }
  .ts {
    flex: none;
    color: var(--text-3);
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .kind {
    flex: none;
    min-width: 40px;
    color: var(--text-3);
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.02em;
  }
  .sum {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--fs-code);
  }
  .d-system .sum {
    font-family: var(--font-ui);
    color: var(--text-2);
    font-size: var(--fs-md);
  }
  .k-error .sum,
  .k-error .kind {
    color: var(--danger);
  }
  .k-open .kind {
    color: var(--success);
  }
  .ev {
    font-size: var(--fs-code);
    text-transform: none;
    margin-right: var(--sp-3);
  }
  .size {
    flex: none;
    font-size: var(--fs-sm);
    font-variant-numeric: tabular-nums;
  }
  .detail {
    position: relative;
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 0 var(--sp-5) var(--sp-3) 52px;
  }
  .cut {
    margin: 0 0 var(--sp-2);
    font-size: var(--fs-sm);
  }
  .detail pre {
    margin: 0;
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: var(--sp-3) var(--sp-4);
    background: var(--bg-input);
    border-radius: var(--r-sm);
    font-size: var(--fs-code);
    line-height: 18px;
    white-space: pre-wrap;
    word-break: break-all;
    color: var(--text-1);
  }
  .copy {
    position: absolute;
    top: 4px;
    right: calc(var(--sp-5) + 4px);
  }
  .jump {
    position: absolute;
    bottom: var(--sp-5);
    left: 50%;
    transform: translateX(-50%);
    box-shadow: var(--shadow-popover);
  }
</style>

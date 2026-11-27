<script lang="ts">
  import { app } from "../../stores/app.svelte";
  import { ws } from "../../stores/ws.svelte";
  import { ui } from "../../stores/ui.svelte";
  import { toasts } from "../../stores/toasts.svelte";
  import { copyText } from "../../api/platform";
  import { agoPhrase, dateTime, formatMs, formatSize, formatDuration, ERROR_LABEL } from "../../util/format";
  import { shortcutKeys } from "../../util/shortcuts";
  import { isStreaming, shortName } from "../../util/grpc";
  import CodeEditor from "../CodeEditor.svelte";
  import Icon from "../Icon.svelte";
  import WsLog from "../WsLog.svelte";

  /**
   * The response pane of a gRPC request: the unary answer (AC-GRPC-7, DESIGN §5.10b), or the message
   * log for streams (AC-GRPC-8, the WebSocket log component).
   */
  let { stacked, ontogglelayout }: { stacked: boolean; ontogglelayout: () => void } = $props();

  type Tab = "body" | "headers" | "trailers";
  let tab = $state<Tab>("body");
  let elapsed = $state(0);

  const r = $derived(app.request);
  const view = $derived(app.currentResponse);
  const gv = $derived(view?.grpc ?? null);
  const inflight = $derived(app.currentInflight);
  const session = $derived(ws.session(r.id));
  /** Streams answer in the log: a stream method, its live (or finished) session, or a stream from history. */
  const streamView = $derived(isStreaming(app.grpcCallKind(r) ?? (session?.transport === "grpc" ? session.callKind : null) ?? gv?.callKind ?? null));
  const status = $derived(gv?.status ?? null);
  const title = $derived(gv ? `${shortName(gv.service)}/${gv.method}` : "");

  $effect(() => {
    const f = inflight;
    if (!f) return;
    elapsed = 0;
    const t = setInterval(() => (elapsed = Date.now() - f.startedAt), 100);
    return () => clearInterval(t);
  });

  const errorTitle = $derived.by(() => {
    const e = view?.error;
    if (!e) return "";
    switch (e.kind) {
      case "dns":
        return "Couldn't resolve host";
      case "connect":
        return "Couldn't connect";
      case "tls":
        return "TLS handshake failed";
      case "timeout":
        return "Timed out while connecting";
      case "cancelled":
        return "Call cancelled";
      default:
        return "Call failed";
    }
  });

  async function copyResponse() {
    if (!gv?.response) return;
    await copyText(gv.response);
    toasts.success("Response copied");
  }

  async function copyValue(name: string, value: string) {
    await copyText(value);
    toasts.success(`Copied ${name}`);
  }

  function tabKey(e: KeyboardEvent) {
    const tabs: Tab[] = ["body", "headers", "trailers"];
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    tab = tabs[(tabs.indexOf(tab) + d + 3) % 3];
    document.querySelector<HTMLElement>(`[data-testid="grpc-res-tab-${tab}"]`)?.focus();
  }

  const shortcuts = $derived([
    { label: "Invoke / Connect", keys: shortcutKeys("Mod+Enter", app.platform) },
    { label: "Copy as grpcurl", keys: shortcutKeys("Mod+Shift+C", app.platform) },
    { label: "Change Environment", keys: shortcutKeys("Mod+E", app.platform) },
    { label: "New Request", keys: shortcutKeys("Mod+N", app.platform) },
  ]);
</script>

{#if streamView}
  <WsLog {stacked} {ontogglelayout} />
{:else}
  <section class="pane res" aria-label="Response" data-testid="grpc-response-pane">
    <div class="row1">
      <span class="faint grow info" data-testid="response-info">
        {#if inflight}
          Calling…
        {:else if view?.fromHistory}
          From history · {dateTime(view.fromHistory)}
          <button class="link" type="button" onclick={() => void app.grpcRun()} data-testid="grpc-invoke-again">Invoke again</button>
        {:else if view && gv}
          gRPC · <span class="mono">{title}</span> · <span class="mono" title={view.resolvedUrl}>{view.resolvedUrl.replace(/\/[^/]+\/[^/]+$/, "")}</span> · {agoPhrase(view.sentAt, app.now)}
        {/if}
      </span>
      <button class="icon-btn" type="button" aria-label="Copy response" title="Copy response" disabled={!gv?.response} onclick={copyResponse} data-testid="copy-body"><Icon name="copy" /></button>
      <button
        class="icon-btn"
        type="button"
        aria-label={stacked ? "Show response side by side" : "Stack response below"}
        title={stacked ? "Show response side by side" : "Stack response below"}
        onclick={ontogglelayout}
        data-testid="layout-toggle"><Icon name={stacked ? "cols" : "rows"} /></button
      >
    </div>

    <div class="row2">
      <div class="strip" aria-live="polite" data-testid="status-strip">
        {#if status}
          <span>Status <span class="status-pill {status.code === 0 ? 's2' : 'serr'}" data-testid="grpc-status" data-code={status.code}>{status.code} {status.name}</span></span>
          {#if gv?.timeMs !== null && gv?.timeMs !== undefined}<span>Time <b data-testid="response-time">{gv.summary ? formatDuration(gv.timeMs) : formatMs(gv.timeMs)}</b></span>{/if}
          {#if gv?.sizeBytes !== null && gv?.sizeBytes !== undefined}<span>Size <b data-testid="response-size" title="Encoded response message">{formatSize(gv.sizeBytes)}</b></span>{/if}
        {:else if view?.error}
          <span
            >Status <span class="status-pill {view.error.kind === 'cancelled' ? 'scan' : 'serr'}" data-testid="grpc-status"
              >{view.error.kind === "cancelled" ? "Cancelled" : view.error.kind === "other" ? "Error" : ERROR_LABEL[view.error.kind]}</span
            ></span
          >
        {/if}
        {#if view && view.unresolved.length}
          <span class="unres" title={view.unresolved.map((u) => `{{${u}}}`).join(", ")} data-testid="unresolved-warning"><Icon name="alert" small />{view.unresolved.length} unresolved variable{view.unresolved.length === 1 ? "" : "s"}</span>
        {/if}
      </div>
    </div>
    {#if inflight}<div class="loading" aria-hidden="true"></div>{/if}

    {#if !view && !inflight}
      <div class="empty cheat" data-testid="response-empty">
        <div class="shortcuts">
          {#each shortcuts as s (s.label)}
            <span>{s.label}</span>
            <span>{#each s.keys as k (k)}<span class="kbd">{k}</span>{/each}</span>
          {/each}
        </div>
      </div>
    {:else}
      <div class="tabs">
        <div class="tablist" role="tablist" aria-label="Response">
          <button class="tab" role="tab" type="button" aria-selected={tab === "body"} tabindex={tab === "body" ? 0 : -1} onclick={() => (tab = "body")} onkeydown={tabKey} data-testid="grpc-res-tab-body">Response</button>
          <button class="tab" role="tab" type="button" aria-selected={tab === "headers"} tabindex={tab === "headers" ? 0 : -1} onclick={() => (tab = "headers")} onkeydown={tabKey} data-testid="grpc-res-tab-headers"
            >Headers{#if gv?.headers.length}<sup>{gv.headers.length}</sup>{/if}</button
          >
          <button class="tab" role="tab" type="button" aria-selected={tab === "trailers"} tabindex={tab === "trailers" ? 0 : -1} onclick={() => (tab = "trailers")} onkeydown={tabKey} data-testid="grpc-res-tab-trailers"
            >Trailers{#if gv?.trailers.length}<sup>{gv.trailers.length}</sup>{/if}</button
          >
        </div>
      </div>

      <div class="content" class:dim={!!inflight} role="tabpanel">
        {#if inflight && !view}
          <div class="empty"><p class="faint">{elapsed >= 300 ? `Calling… ${(elapsed / 1000).toFixed(1)} s` : ""}</p></div>
        {:else if view?.error && !status}
          <div class="pad">
            {#if view.error.kind === "cancelled"}
              <p class="muted" data-testid="response-cancelled">Call cancelled{view.elapsedMs !== null ? ` after ${(view.elapsedMs / 1000).toFixed(1)} s` : ""}</p>
            {:else}
              <div class="err" role="alert" data-testid="response-error" data-kind={view.error.kind}>
                <h3><Icon name="alert" />{errorTitle}</h3>
                <code class="selectable">{view.error.message}</code>
                <div class="actions">
                  <button class="btn btn-secondary btn-sm" type="button" onclick={() => void app.grpcRun()}>Retry</button>
                  {#if view.error.kind === "tls" && app.request.settings.verifyTls}
                    <button
                      class="btn btn-ghost btn-sm"
                      type="button"
                      onclick={() => {
                        app.request.settings.verifyTls = false;
                        toasts.info("TLS verification disabled for this request");
                      }}>Disable TLS verification for this request</button
                    >
                  {:else if view.error.kind === "timeout"}
                    <button class="btn btn-ghost btn-sm" type="button" onclick={() => ui.requestSettingsTick++}>Change deadline</button>
                  {/if}
                </div>
              </div>
            {/if}
          </div>
        {:else if tab === "body"}
          {#if status && status.code !== 0}
            <div class="pad">
              <div class="hint danger status-panel" role="alert" data-testid="grpc-status-panel">
                <Icon name="alert" />
                <div class="grow">
                  <b>{status.name} ({status.code})</b>
                  {#if status.message}<p class="mono msg selectable" data-testid="grpc-status-message">{status.message}</p>{/if}
                  {#each status.details as dt, i (i)}
                    <div class="detail" data-testid="grpc-status-detail">
                      <span class="dtype">{dt.typeUrl.replace(/^.*\//, "")}</span>
                      {#if dt.json}<pre class="mono selectable">{dt.json}</pre>{:else}<span class="faint mono">{dt.typeUrl} · base64 {dt.base64}</span>{/if}
                    </div>
                  {/each}
                </div>
              </div>
            </div>
          {/if}
          {#if gv?.response !== null && gv?.response !== undefined}
            {#if gv.responseTruncated}<div class="pad"><div class="hint warn"><Icon name="info" />History keeps the first 1 MB of the response.</div></div>{/if}
            <CodeEditor value={gv.response} language="json" readOnly ariaLabel="Response message" testid="response-body" />
          {:else if gv?.summary && isStreaming(gv.callKind)}
            <div class="empty"><p class="faint">Messages of past streams aren't kept.</p></div>
          {:else if status && status.code === 0}
            <div class="empty"><p class="faint" data-testid="body-empty">No response message</p></div>
          {:else if !status}
            <div class="empty"><p class="faint">No response</p></div>
          {/if}
        {:else}
          {@const rows = tab === "headers" ? (gv?.headers ?? []) : (gv?.trailers ?? [])}
          <div class="scroll">
            {#if rows.length === 0}
              <div class="empty"><p class="faint">No {tab}</p></div>
            {:else}
              <table class="tbl" data-testid="grpc-{tab}">
                <tbody>
                  {#each rows as [name, value], i (i)}
                    <tr onclick={() => copyValue(name, value)} title="Click to copy value">
                      <td class="hn">{name}</td>
                      <td class="hv mono selectable">{value}</td>
                    </tr>
                  {/each}
                </tbody>
              </table>
            {/if}
          </div>
        {/if}
      </div>
    {/if}
  </section>
{/if}

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
  .content {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    transition: opacity var(--dur-fast);
  }
  .content.dim {
    opacity: 0.5;
  }
  .pad {
    padding: var(--sp-4) var(--sp-5) var(--sp-2);
  }
  .actions {
    display: flex;
    gap: var(--sp-4);
    flex-wrap: wrap;
  }
  .status-panel {
    align-items: flex-start;
  }
  .msg {
    margin: var(--sp-2) 0 0;
    color: var(--text-1);
  }
  .detail {
    margin-top: var(--sp-3);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .dtype {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .detail pre {
    margin: 0;
    padding: var(--sp-3);
    background: var(--bg-input);
    border-radius: var(--r-sm);
    max-height: 200px;
    overflow: auto;
    font-size: var(--fs-sm);
  }
  .scroll {
    flex: 1;
    overflow: auto;
    padding: var(--sp-2) var(--sp-5) var(--sp-6);
  }
  .tbl {
    width: 100%;
    border-collapse: collapse;
    font-size: var(--fs-md);
  }
  .tbl td {
    padding: var(--sp-3) var(--sp-4);
    border-top: 1px solid var(--border-subtle);
    vertical-align: top;
  }
  .tbl tbody tr:first-child td {
    border-top: 0;
  }
  .tbl tbody tr:hover {
    background: var(--bg-hover);
    cursor: copy;
  }
  .hn {
    color: var(--text-2);
    width: 34%;
    word-break: break-all;
  }
  .hv {
    color: var(--text-1);
    word-break: break-all;
    font-size: var(--fs-code);
  }
</style>

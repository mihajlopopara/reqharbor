<script lang="ts">
  import { app, BODY_LIMIT, decodeBody } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call } from "../api";
  import { copyText, saveResponseToFile } from "../api/platform";
  import { formatSize, formatMs, statusClass, agoPhrase, dateTime, ERROR_LABEL } from "../util/format";
  import { prettyJson } from "../util/json";
  import { shortcutKeys } from "../util/shortcuts";
  import CodeEditor from "./CodeEditor.svelte";
  import Icon from "./Icon.svelte";

  let { stacked, ontogglelayout }: { stacked: boolean; ontogglelayout: () => void } = $props();

  type RTab = "body" | "headers" | "cookies";
  let tab = $state<RTab>("body");
  let mode = $state<"pretty" | "raw" | "preview">("pretty");
  let elapsed = $state(0);
  let showUnresolved = $state(false);

  const view = $derived(app.currentResponse);
  const meta = $derived(view?.response ?? null);
  const inflight = $derived(app.currentInflight);
  const kind = $derived(meta?.bodyKind ?? "empty");
  const text = $derived(view?.body && kind !== "image" && kind !== "binary" ? decodeBody(view.body) : null);
  const limited = $derived(!!view?.bodyLimited);
  const canPretty = $derived(!limited && (kind === "json" || kind === "xml" || kind === "html"));

  let xmlPretty = $state<{ src: string; out: string | null } | null>(null);
  const prettyText = $derived.by(() => {
    if (!text || !canPretty) return text;
    if (kind === "json") return prettyJson(text) ?? text;
    if (kind === "xml") return xmlPretty?.src === text ? (xmlPretty.out ?? text) : text;
    return text;
  });

  $effect(() => {
    const t = text;
    if (kind !== "xml" || !t || limited || xmlPretty?.src === t) return;
    void call("format_body", { kind: "xml", text: t })
      .then((out) => (xmlPretty = { src: t, out }))
      .catch(() => (xmlPretty = { src: t, out: null }));
  });

  // Elapsed "Sending… 1.2 s" ticker — only runs while a request is in flight.
  $effect(() => {
    const f = inflight;
    if (!f) return;
    elapsed = 0;
    const t = setInterval(() => (elapsed = Date.now() - f.startedAt), 100);
    return () => clearInterval(t);
  });

  // Default view mode per kind.
  let lastKey = "";
  $effect(() => {
    const key = meta?.responseId ?? "";
    if (key === lastKey) return;
    lastKey = key;
    mode = kind === "image" ? "preview" : "pretty";
    showUnresolved = false;
  });

  let imgUrl = $state<string | null>(null);
  let imgDims = $state<string>("");
  $effect(() => {
    if (kind !== "image" || !view?.body) {
      imgUrl = null;
      return;
    }
    const type = meta?.contentType?.split(";")[0] || "image/png";
    const url = URL.createObjectURL(new Blob([view.body.slice()], { type }));
    imgUrl = url;
    imgDims = "";
    return () => URL.revokeObjectURL(url);
  });

  const errorTitle = $derived.by(() => {
    const e = view?.error;
    if (!e) return "";
    switch (e.kind) {
      case "dns":
        return "Couldn't resolve host";
      case "connect":
        return "Connection refused";
      case "tls":
        return "TLS handshake failed";
      case "timeout":
        return `Timed out after ${(app.request.settings.timeoutMs / 1000).toLocaleString()} s`;
      case "cancelled":
        return "Request cancelled";
      default:
        return "Request failed";
    }
  });

  const cookieCount = $derived(meta?.cookies.length ?? 0);
  const headerCount = $derived(meta?.headers.length ?? 0);

  function fileName(): string {
    const ct = meta?.contentType ?? "";
    const ext = ct.includes("json") ? "json" : ct.includes("xml") ? "xml" : ct.includes("html") ? "html" : ct.includes("png") ? "png" : ct.includes("jpeg") ? "jpg" : ct.includes("pdf") ? "pdf" : ct.startsWith("text/") ? "txt" : "bin";
    return `response.${ext}`;
  }

  async function copyBody() {
    if (text === null) return;
    await copyText(mode === "pretty" && prettyText ? prettyText : text);
    toasts.success("Response body copied");
  }

  async function save() {
    if (!meta) return;
    try {
      if (await saveResponseToFile(meta.responseId, fileName())) toasts.success("Response saved");
    } catch (e) {
      app.reportError(e);
    }
  }

  async function copyHeader(name: string, value: string) {
    await copyText(value);
    toasts.success(`Copied ${name}`);
  }

  function tabKey(e: KeyboardEvent) {
    const tabs: RTab[] = ["body", "headers", "cookies"];
    const d = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!d) return;
    e.preventDefault();
    tab = tabs[(tabs.indexOf(tab) + d + 3) % 3];
    document.querySelector<HTMLElement>(`[data-testid="res-tab-${tab}"]`)?.focus();
  }

  const shortcuts = $derived([
    { label: "Send Request", keys: shortcutKeys("Mod+Enter", app.platform) },
    { label: "Import cURL", keys: shortcutKeys("Mod+U", app.platform) },
    { label: "Change Environment", keys: shortcutKeys("Mod+E", app.platform) },
    { label: "New Request", keys: shortcutKeys("Mod+N", app.platform) },
  ]);
</script>

<section class="pane res" aria-label="Response" data-testid="response-pane">
  <div class="row1">
    <span class="faint grow info" data-testid="response-info">
      {#if inflight}
        Sending…
      {:else if view?.fromHistory}
        From history · {dateTime(view.fromHistory)}
        <button class="link" type="button" onclick={() => void app.send()} data-testid="resend">Re-send</button>
      {:else if view}
        Sent {agoPhrase(view.sentAt, app.now)}{view.envName ? ` · ${view.envName}` : ""}
      {/if}
    </span>
    <button class="icon-btn" type="button" aria-label="Copy body" title="Copy body" disabled={text === null} onclick={copyBody} data-testid="copy-body"><Icon name="copy" /></button>
    <button class="icon-btn" type="button" aria-label="Save body to file" title="Save body to file" disabled={!meta} onclick={save} data-testid="save-body"><Icon name="save" /></button>
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
      {#if meta}
        <span>Status <span class="status-pill {statusClass(meta.status)}" data-testid="response-status">{meta.status} {meta.statusText}</span></span>
        <span
          >Size <b
            data-testid="response-size"
            title={meta.wireSizeBytes !== null ? `Wire size ${formatSize(meta.wireSizeBytes)}${meta.contentEncoding ? ` (${meta.contentEncoding})` : ""}` : "Decoded size"}
            >{formatSize(meta.sizeBytes)}</b
          ></span
        >
        <span>Time <b data-testid="response-time">{formatMs(meta.timeMs)}</b></span>
      {:else if view?.error}
        <span
          >Status <span class="status-pill {view.error.kind === 'cancelled' ? 'scan' : 'serr'}" data-testid="response-status"
            >{view.error.kind === "cancelled" ? "Cancelled" : view.error.kind === "other" ? "Error" : ERROR_LABEL[view.error.kind]}</span
          ></span
        >
      {:else if app.middle.kind === "environment"}
        <span class="faint" data-testid="env-resolve-hint">Variables resolve: project env → Global</span>
      {/if}
      {#if view && view.unresolved.length}
        <span class="unres-wrap">
          <button class="unres" type="button" onclick={() => (showUnresolved = !showUnresolved)} aria-expanded={showUnresolved} data-testid="unresolved-warning">
            <Icon name="alert" small />{view.unresolved.length} unresolved variable{view.unresolved.length === 1 ? "" : "s"}
          </button>
          {#if showUnresolved}
            <div class="menu pop unres-list" role="dialog" aria-label="Unresolved variables">
              {#each view.unresolved as u (u)}<div class="mono">{`{{${u}}}`}</div>{/each}
              <div class="faint small">Sent literally. Define them in the active environment or Global.</div>
            </div>
          {/if}
        </span>
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
      <button class="tab" role="tab" type="button" aria-selected={tab === "body"} tabindex={tab === "body" ? 0 : -1} onclick={() => (tab = "body")} onkeydown={tabKey} data-testid="res-tab-body">Response</button>
      <button class="tab" role="tab" type="button" aria-selected={tab === "headers"} tabindex={tab === "headers" ? 0 : -1} onclick={() => (tab = "headers")} onkeydown={tabKey} data-testid="res-tab-headers"
        >Headers{#if headerCount}<sup>{headerCount}</sup>{/if}</button
      >
      <button class="tab" role="tab" type="button" aria-selected={tab === "cookies"} tabindex={tab === "cookies" ? 0 : -1} onclick={() => (tab = "cookies")} onkeydown={tabKey} data-testid="res-tab-cookies"
        >Cookies{#if cookieCount}<sup>{cookieCount}</sup>{/if}</button
      >
      </div>
      {#if tab === "body" && meta}
        <span class="end">
          {#if canPretty}
            <span class="seg" role="group" aria-label="View">
              <button type="button" aria-pressed={mode === "pretty"} onclick={() => (mode = "pretty")} data-testid="view-pretty">Pretty</button>
              <button type="button" aria-pressed={mode === "raw"} onclick={() => (mode = "raw")} data-testid="view-raw">Raw</button>
              {#if kind === "html"}<button type="button" aria-pressed={mode === "preview"} onclick={() => (mode = "preview")} data-testid="view-preview">Preview</button>{/if}
            </span>
          {:else if kind === "image"}
            <span class="seg" role="group" aria-label="View">
              <button type="button" aria-pressed={mode === "preview"} onclick={() => (mode = "preview")} data-testid="view-preview">Preview</button>
              <button type="button" aria-pressed={mode === "raw"} onclick={() => (mode = "raw")} data-testid="view-raw">Raw</button>
            </span>
          {/if}
        </span>
      {/if}
    </div>

    <div class="content" class:dim={!!inflight} role="tabpanel">
      {#if inflight && !view}
        <div class="empty"><p class="faint">{elapsed >= 300 ? `Sending… ${(elapsed / 1000).toFixed(1)} s` : ""}</p></div>
      {:else if view?.error && !meta}
        <div class="pad">
          {#if view.error.kind === "cancelled"}
            <p class="muted" data-testid="response-cancelled">Request cancelled{view.elapsedMs !== null ? ` after ${(view.elapsedMs / 1000).toFixed(1)} s` : ""}</p>
          {:else}
            <div class="err" role="alert" data-testid="response-error" data-kind={view.error.kind}>
              <h3><Icon name="alert" />{errorTitle}</h3>
              <code class="selectable">{view.error.message}</code>
              <div class="actions">
                <button class="btn btn-secondary btn-sm" type="button" onclick={() => void app.send()}>Retry</button>
                {#if view.error.kind === "tls"}
                  <button
                    class="btn btn-ghost btn-sm"
                    type="button"
                    onclick={() => {
                      app.request.settings.verifyTls = false;
                      toasts.info("TLS verification disabled for this request");
                    }}>Disable TLS verification for this request</button
                  >
                {:else if view.error.kind === "timeout"}
                  <button class="btn btn-ghost btn-sm" type="button" onclick={() => ui.requestSettingsTick++}>Change timeout</button>
                {/if}
              </div>
            </div>
          {/if}
        </div>
      {:else if meta && tab === "body"}
        {#if limited}
          <div class="pad">
            <div class="hint warn" data-testid="body-limited">
              <Icon name="info" /><span class="grow">Showing first {formatSize(BODY_LIMIT)} as plain text</span>
              <button class="link" type="button" onclick={save}>Save full body to file</button>
            </div>
          </div>
        {/if}
        {#if meta.decodeError}
          <div class="pad"><div class="hint warn"><Icon name="alert" />Couldn't decode body ({meta.decodeError}); showing raw bytes.</div></div>
        {/if}
        {#if view?.bodyError}
          <div class="pad"><div class="hint danger"><Icon name="alert" />{view.bodyError}</div></div>
        {:else if kind === "empty"}
          <div class="empty"><p class="faint" data-testid="body-empty">No response body</p></div>
        {:else if !view?.body}
          <div class="empty"><p class="faint">Loading body…</p></div>
        {:else if kind === "binary" || (kind === "image" && mode === "raw" && !(meta.contentType ?? "").includes("svg"))}
          <div class="empty" data-testid="body-binary">
            <Icon name="file" />
            <p>{meta.contentType?.split(";")[0] ?? "application/octet-stream"} · {formatSize(meta.sizeBytes)}</p>
            <button class="btn btn-secondary" type="button" onclick={save}>Save to file</button>
          </div>
        {:else if kind === "image" && mode === "preview" && imgUrl}
          <div class="img-wrap" data-testid="body-image">
            <div class="checker">
              <img
                src={imgUrl}
                alt="Response preview"
                onload={(e) => {
                  const im = e.currentTarget as HTMLImageElement;
                  imgDims = `${im.naturalWidth} × ${im.naturalHeight}`;
                }}
              />
            </div>
            <span class="faint small">{imgDims}{imgDims ? " · " : ""}{formatSize(meta.sizeBytes)}</span>
          </div>
        {:else if kind === "html" && mode === "preview"}
          <iframe class="preview" title="HTML preview" sandbox="" srcdoc={text ?? ""} data-testid="body-html-preview"></iframe>
        {:else if kind === "image" && view.body}
          <CodeEditor value={decodeBody(view.body)} language="xml" readOnly wrap ariaLabel="Response body" testid="response-body" />
        {:else}
          <CodeEditor
            value={(mode === "pretty" ? prettyText : text) ?? ""}
            language={kind === "json" ? "json" : kind === "xml" ? "xml" : kind === "html" ? "html" : "text"}
            plain={limited || mode === "raw"}
            wrap={mode === "raw"}
            readOnly
            ariaLabel="Response body"
            testid="response-body"
          />
        {/if}
      {:else if meta && tab === "headers"}
        <div class="scroll">
          <table class="tbl" data-testid="response-headers">
            <tbody>
              {#each meta.headers as [name, value], i (i)}
                <tr class:aq={name === "accept-query"} onclick={() => copyHeader(name, value)} title="Click to copy value">
                  <td class="hn">{name}{#if name === "accept-query"}<span class="qtag m m-query">QUERY</span>{/if}</td>
                  <td class="hv mono selectable">{value}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
      {:else if meta && tab === "cookies"}
        <div class="scroll">
          {#if meta.cookies.length === 0}
            <div class="empty"><p class="faint">No cookies in this response</p></div>
          {:else}
            <table class="tbl cookies" data-testid="response-cookies">
              <thead><tr><th>Name</th><th>Value</th><th>Domain</th><th>Path</th><th>Expires</th><th>HttpOnly</th><th>Secure</th><th>SameSite</th></tr></thead>
              <tbody>
                {#each meta.cookies as c, i (i)}
                  <tr>
                    <td class="mono">{c.name}</td>
                    <td class="mono selectable">{c.value}</td>
                    <td>{c.domain ?? "—"}</td>
                    <td>{c.path ?? "—"}</td>
                    <td>{c.expires ?? (c.maxAge !== null ? `Max-Age ${c.maxAge}` : "Session")}</td>
                    <td>{c.httpOnly ? "✓" : "—"}</td>
                    <td>{c.secure ? "✓" : "—"}</td>
                    <td>{c.sameSite ?? "—"}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        </div>
      {:else if tab !== "body"}
        <div class="empty"><p class="faint">No response</p></div>
      {/if}
    </div>
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
  .unres-wrap {
    position: relative;
  }
  .unres {
    display: inline-flex;
    gap: var(--sp-2);
    align-items: center;
    border: 0;
    background: none;
    color: var(--warning);
    font-size: var(--fs-md);
    padding: 0;
  }
  .unres-list {
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: var(--z-popover);
    padding: var(--sp-4);
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    white-space: normal;
    width: 260px;
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
    padding: var(--sp-4) var(--sp-5) 0;
  }
  .actions {
    display: flex;
    gap: var(--sp-4);
    flex-wrap: wrap;
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
  .tbl th {
    text-align: left;
    font-weight: var(--fw-medium);
    font-size: var(--fs-sm);
    color: var(--text-3);
    padding: var(--sp-3) var(--sp-4);
    white-space: nowrap;
  }
  .tbl td {
    padding: var(--sp-3) var(--sp-4);
    border-top: 1px solid var(--border-subtle);
    vertical-align: top;
  }
  .tbl tbody tr:first-child td {
    border-top: 0; /* DR-016: hairlines only between rows */
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
  tr.aq {
    background: var(--accent-soft);
  }
  .qtag {
    margin-left: var(--sp-3);
  }
  .cookies td {
    white-space: nowrap;
  }
  .img-wrap {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--sp-4);
    padding: var(--sp-6);
    overflow: auto;
  }
  .checker {
    background-image: conic-gradient(var(--bg-input) 25%, var(--bg-pane) 0 50%, var(--bg-input) 0 75%, var(--bg-pane) 0);
    background-size: 16px 16px;
    padding: var(--sp-4);
    border-radius: var(--r-sm);
  }
  .checker img {
    max-width: 100%;
    display: block;
    image-rendering: auto;
  }
  .preview {
    flex: 1;
    border: 0;
    background: #fff;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

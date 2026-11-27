<script lang="ts">
  import { app } from "../stores/app.svelte";
  import { defaultSocketIo, defaultWs, isSocketIo, normalizeNamespace, wsUrl } from "../util/ws";
  import CodeEditor from "./CodeEditor.svelte";
  import Icon from "./Icon.svelte";

  /** Settings tab of a WebSocket / Socket.IO request (AC-WS-1/2/6). */
  const r = $derived(app.request);
  const cfg = $derived(r.ws ?? defaultWs());
  const sio = $derived(isSocketIo(r));
  const protocolsText = $derived(cfg.protocols.join(", "));

  const preview = $derived.by(() => {
    if (!r.url.trim() || r.url.trim().startsWith("{{")) return null;
    try {
      const withScheme = /^[a-z][a-z0-9+.-]*:\/\//i.test(r.url.trim()) ? r.url : `${/^(localhost|127\.0\.0\.1|\[::1\])/.test(r.url.trim()) ? "http" : "https"}://${r.url.trim()}`;
      return wsUrl(withScheme, r.ws?.socketIo ?? null);
    } catch (e) {
      return `⚠ ${(e as Error).message}`;
    }
  });

  const authError = $derived.by(() => {
    const t = r.ws?.socketIo?.auth?.trim();
    if (!t || t.includes("{{")) return null;
    try {
      const v = JSON.parse(t) as unknown;
      return v && typeof v === "object" && !Array.isArray(v) ? null : "The auth payload must be a JSON object";
    } catch (e) {
      return `Invalid JSON: ${(e as Error).message}`;
    }
  });

  function ws() {
    app.request.ws ??= defaultWs();
    return app.request.ws;
  }
  function socketIo() {
    const w = ws();
    w.socketIo ??= defaultSocketIo();
    return w.socketIo;
  }
</script>

<div class="settings scroll" data-testid="ws-settings">
  <section>
    <h4>Connection</h4>
    {#if preview}
      <div class="field">
        <span>Opens</span>
        <code class="mono url selectable" data-testid="ws-url-preview">{preview}</code>
      </div>
    {/if}
    {#if !sio}
      <label class="field">
        <span>Subprotocols — comma-separated, sent as Sec-WebSocket-Protocol</span>
        <input
          class="input mono"
          value={protocolsText}
          placeholder="graphql-transport-ws, chat.v2"
          spellcheck="false"
          data-testid="ws-protocols"
          onchange={(e) =>
            (ws().protocols = e.currentTarget.value
              .split(",")
              .map((p) => p.trim())
              .filter(Boolean))}
        />
      </label>
    {/if}
    <label class="checkbox">
      <input type="checkbox" checked={cfg.autoReconnect} onchange={(e) => (ws().autoReconnect = e.currentTarget.checked)} data-testid="ws-auto-reconnect" />Reconnect automatically when the connection drops
    </label>
    <span class="faint small">Up to 5 attempts, 1 s to 16 s apart. Disconnect never reconnects.</span>
    <label class="field">
      <span>Connect timeout (ms) — 0 = no timeout</span>
      <input
        class="input mono"
        type="number"
        min="0"
        step="1000"
        value={r.settings.timeoutMs}
        data-testid="ws-timeout"
        oninput={(e) => (app.request.settings.timeoutMs = Math.max(0, Math.round(Number(e.currentTarget.value) || 0)))}
      />
    </label>
    <label class="checkbox">
      <input type="checkbox" checked={r.settings.verifyTls} onchange={(e) => (app.request.settings.verifyTls = e.currentTarget.checked)} />Verify TLS certificates (wss://)
    </label>
    {#if !r.settings.verifyTls}
      <div class="hint warn"><Icon name="alert" />Self-signed and invalid certificates will be accepted.</div>
    {/if}
  </section>

  {#if sio && r.ws?.socketIo}
    {@const s = r.ws.socketIo}
    <section data-testid="sio-settings">
      <h4>Socket.IO</h4>
      <div class="two">
        <label class="field">
          <span>Path</span>
          <input class="input mono" value={s.path} placeholder="/socket.io/" spellcheck="false" data-testid="sio-path" oninput={(e) => (socketIo().path = e.currentTarget.value)} />
        </label>
        <label class="field">
          <span>Namespace</span>
          <input
            class="input mono"
            value={s.namespace}
            placeholder="/"
            spellcheck="false"
            data-testid="sio-namespace"
            oninput={(e) => (socketIo().namespace = e.currentTarget.value)}
            onblur={(e) => (socketIo().namespace = normalizeNamespace(e.currentTarget.value))}
          />
        </label>
      </div>
      <div class="field">
        <span>Auth payload — optional JSON object sent with the namespace CONNECT (variables resolve)</span>
        <div class="auth-box">
          <CodeEditor
            value={s.auth ?? ""}
            language="json"
            lookup={app.lookup}
            ariaLabel="Socket.IO auth payload"
            placeholder={'{ "token": "{{token}}" }'}
            testid="sio-auth"
            onchange={(v) => {
              const so = socketIo();
              if (v.trim()) so.auth = v;
              else delete so.auth;
            }}
          />
        </div>
        {#if authError}<span class="field-error" role="alert">{authError}</span>{/if}
      </div>
      <span class="faint small">Engine.IO v4 over WebSocket (Socket.IO 3 and 4 servers). The URL's path is replaced by the path above. Binary attachments aren't supported.</span>
    </section>
  {/if}
</div>

<style>
  .settings {
    flex: 1;
    overflow: auto;
    padding: var(--sp-4) var(--sp-5) var(--sp-6);
    display: flex;
    flex-direction: column;
    gap: var(--sp-7);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    max-width: 560px;
  }
  h4 {
    margin: 0;
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    color: var(--text-2);
  }
  .two {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-4);
  }
  .url {
    font-size: var(--fs-code);
    color: var(--text-2);
    word-break: break-all;
  }
  .auth-box {
    height: 96px;
    display: flex;
    flex-direction: column;
    border: 1px solid var(--border-subtle);
    border-radius: var(--r-sm);
    overflow: hidden;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

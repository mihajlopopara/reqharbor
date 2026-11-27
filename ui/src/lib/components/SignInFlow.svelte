<script lang="ts">
  /**
   * Server entry, discovery and the browser sign-in (UX §3; 06-go-server §9).
   * There is no default server: the field starts empty, earlier servers are
   * suggested from settings.json `knownServers`, and the method buttons stay
   * disabled until discovery succeeds.
   */
  import { onDestroy, onMount, tick } from "svelte";
  import { app } from "../stores/app.svelte";
  import { call, toCommandError, type ServerDiscovery } from "../api";
  import { copyText } from "../api/platform";
  import { toasts } from "../stores/toasts.svelte";
  import { elapsed, errorAction, methodButtonLabel, serverHost } from "../util/account";
  import Icon from "./Icon.svelte";

  let {
    presetServer = "",
    ongo,
  }: {
    /** Prefill (e.g. the account's server for "Sign in again"). */
    presetServer?: string;
    /** Navigate to another Settings section (Sync › Advanced). */
    ongo?: (section: "sync") => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let server = $state(presetServer || "");
  let discovery = $state<ServerDiscovery | null>(null);
  let discovering = $state(false);
  let urlError = $state<string | null>(null);
  let insecureAck = $state(false);
  /** "Keep what I typed" for the current discovery. */
  let keptTyped = $state(false);
  let lastMethod = $state<string | null>(null);
  let now = $state(Date.now());
  let reopenBtn = $state<HTMLButtonElement>();
  let actionBtn = $state<HTMLButtonElement>();
  let seq = 0;
  let debounce: ReturnType<typeof setTimeout> | null = null;
  let ticker: ReturnType<typeof setInterval> | null = null;

  const signIn = $derived(app.auth.signIn);
  const busy = $derived(signIn.state === "checking" || signIn.state === "waiting" || signIn.state === "exchanging");
  const known = $derived(app.knownServers);
  const remembered = $derived(discovery ? known.find((k) => k.url === discovery!.serverUrl) : undefined);
  const promptCanonical = $derived(!!discovery?.canonicalUrl && discovery.status === "ok" && !keptTyped && !remembered?.keepTyped);
  const methods = $derived(discovery?.status === "ok" && discovery.canSignIn ? (discovery.info?.auth.methods ?? []) : []);
  const ready = $derived(!!discovery && discovery.status === "ok" && discovery.canSignIn && !promptCanonical && (!discovery.insecure || insecureAck) && !busy);
  const shownError = $derived(signIn.state === "error" ? signIn : null);
  const discoveryProblem = $derived(discovery && (discovery.status !== "ok" || !discovery.canSignIn) ? discovery.message : null);

  async function discover(value: string) {
    const my = ++seq;
    if (debounce) clearTimeout(debounce);
    if (!value.trim()) {
      discovery = null;
      urlError = null;
      discovering = false;
      return;
    }
    discovering = true;
    try {
      const d = await call("server_discover", { serverUrl: value });
      if (my !== seq) return;
      discovery = d;
      urlError = null;
      keptTyped = false;
      insecureAck = !!known.find((k) => k.url === d.serverUrl)?.insecureAck;
    } catch (e) {
      if (my !== seq) return;
      discovery = null;
      urlError = toCommandError(e).message;
    } finally {
      if (my === seq) discovering = false;
    }
  }

  function oninput() {
    discovery = null;
    urlError = null;
    if (debounce) clearTimeout(debounce);
    debounce = setTimeout(() => void discover(server), 600);
  }

  function useCanonical() {
    if (!discovery?.canonicalUrl) return;
    server = discovery.canonicalUrl;
    void discover(server);
  }

  function keepTyped() {
    if (!discovery) return;
    keptTyped = true;
    app.rememberServer(discovery.serverUrl, { keepTyped: true, lastUsedAt: remembered?.lastUsedAt ?? Date.now() });
  }

  function setAck(on: boolean) {
    insecureAck = on;
    if (discovery) app.rememberServer(discovery.serverUrl, { insecureAck: on, lastUsedAt: remembered?.lastUsedAt ?? Date.now() });
  }

  async function start(method: string | null) {
    const target = discovery?.serverUrl ?? server;
    if (!target.trim()) return;
    lastMethod = method;
    try {
      app.auth = await call("auth_sign_in_start", { serverUrl: target, method, insecureConfirmed: insecureAck });
    } catch (e) {
      // The status event carries the error; keep a local copy in case it was missed.
      const err = toCommandError(e);
      if (app.auth.signIn.state !== "error") app.auth = { ...app.auth, signIn: { ...app.auth.signIn, state: "error", error: err.code, message: err.message } };
    }
  }

  async function retry() {
    await discover(discovery?.serverUrl ?? server);
    if (discovery?.status === "ok" && discovery.canSignIn) await start(lastMethod ?? signIn.method);
  }

  async function cancel() {
    app.auth = await call("auth_sign_in_cancel", {});
  }

  async function copyLink() {
    if (!signIn.authorizeUrl) return;
    try {
      await copyText(signIn.authorizeUrl);
      toasts.success("Sign-in link copied");
    } catch (e) {
      app.reportError(e);
    }
  }

  async function reopen() {
    try {
      await call("auth_sign_in_reopen", {});
    } catch (e) {
      app.reportError(e);
    }
  }

  $effect(() => {
    if (signIn.state === "waiting") {
      now = Date.now();
      ticker ??= setInterval(() => (now = Date.now()), 1000);
      void tick().then(() => reopenBtn?.focus());
    } else {
      if (ticker) clearInterval(ticker);
      ticker = null;
      if (signIn.state === "error") void tick().then(() => actionBtn?.focus());
    }
  });

  onMount(() => {
    if (server.trim()) void discover(server);
  });
  onDestroy(() => {
    if (debounce) clearTimeout(debounce);
    if (ticker) clearInterval(ticker);
  });
</script>

<div class="flow" data-testid="signin-flow">
  {#if signIn.state === "waiting" || signIn.state === "exchanging"}
    <div class="waiting" data-testid="signin-waiting">
      <div class="title" aria-live="polite">{signIn.state === "exchanging" ? "Signing in…" : "Waiting for you to finish in your browser…"}</div>
      {#if signIn.state === "waiting" && signIn.message}
        <div class="hint warn" role="alert" data-testid="signin-browser-failed">
          <Icon name="alert" /><span>{signIn.message}</span>
          {#if signIn.authorizeUrl}
            <div class="row"><button class="btn btn-secondary btn-sm" type="button" onclick={copyLink} data-testid="signin-copy-link">Copy link</button></div>
          {/if}
        </div>
        <p class="muted">Open the link to sign in to {serverHost(signIn.serverUrl ?? server)}. Come back here when it says you're signed in.</p>
      {:else}
        <p class="muted">We opened {serverHost(signIn.serverUrl ?? server)} in your browser. Come back here when it says you're signed in.</p>
      {/if}
      {#if signIn.state === "waiting" && signIn.startedAt}
        <p class="faint timer" aria-hidden="true" data-testid="signin-elapsed">Waiting {elapsed(now - signIn.startedAt)}</p>
      {/if}
      <div class="row">
        <button class="btn btn-secondary" type="button" bind:this={reopenBtn} disabled={signIn.state === "exchanging"} onclick={reopen} data-testid="signin-reopen">Open browser again</button>
        <button class="btn btn-ghost" type="button" disabled={signIn.state === "exchanging"} onclick={cancel} data-testid="signin-cancel">Cancel</button>
      </div>
    </div>
  {:else}
    <p class="muted lead">Sign in to sync across devices and work with a team.</p>
    <label class="field">
      <span>Server</span>
      <input
        class="input mono server"
        class:invalid={!!urlError}
        bind:value={server}
        {oninput}
        onblur={() => discovery?.serverUrl !== server && void discover(server)}
        onkeydown={(e) => e.key === "Enter" && void discover(server)}
        placeholder="https://reqharbor.example.com"
        list="known-servers"
        autocomplete="off"
        spellcheck="false"
        aria-invalid={urlError ? "true" : undefined}
        aria-describedby="signin-server-help"
        data-testid="signin-server"
      />
      <datalist id="known-servers">
        {#each known as k (k.url)}<option value={k.url}>{k.name ?? serverHost(k.url)}</option>{/each}
      </datalist>
    </label>
    <p class="faint small" id="signin-server-help">Only sign in to servers you trust. Your synced data is stored there.</p>

    {#if urlError}
      <div class="hint danger" role="alert" data-testid="signin-url-error"><Icon name="alert" />{urlError}</div>
    {:else if discovering}
      <p class="faint small" data-testid="signin-checking">Checking {serverHost(server)}…</p>
    {:else if discovery}
      {#if discoveryProblem}
        <div class="hint danger" role="alert" data-testid="signin-discovery-error">
          <Icon name="alert" /><span>{discoveryProblem}</span>
          {#if (discovery.status === "token_sync_only" || (discovery.status === "ok" && !discovery.canSignIn)) && ongo}
            <button class="link" type="button" onclick={() => ongo?.("sync")} data-testid="signin-open-advanced">Open Sync › Advanced</button>
          {:else if discovery.status === "unreachable"}
            <button class="link" type="button" onclick={() => discover(server)} data-testid="signin-discover-retry">Try again</button>
          {/if}
        </div>
      {:else}
        <p class="faint small" data-testid="signin-server-ok">
          <Icon name="check" small />{discovery.info?.name || serverHost(discovery.serverUrl)}{discovery.info?.version ? ` · server ${discovery.info.version}` : ""}{discovery.cached ? " · offline, last seen earlier" : ""}
        </p>
        {#if discovery.message}<div class="hint" data-testid="signin-notice"><Icon name="info" />{discovery.message}</div>{/if}
      {/if}
      {#if promptCanonical}
        <div class="hint canonical" data-testid="signin-canonical">
          <Icon name="info" />
          <span>This server's address is <span class="mono">{discovery.canonicalUrl}</span>. Use that address?</span>
          <div class="row">
            <button class="btn btn-primary btn-sm" type="button" onclick={useCanonical} data-testid="signin-use-canonical">Use {discovery.canonicalUrl}</button>
            <button class="btn btn-ghost btn-sm" type="button" onclick={keepTyped} data-testid="signin-keep-typed">Keep what I typed</button>
          </div>
        </div>
      {/if}
      {#if discovery.insecure && discovery.status === "ok"}
        <div class="hint warn" data-testid="signin-insecure">
          <Icon name="alert" />
          <span>This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.</span>
          <label class="checkbox"><input type="checkbox" checked={insecureAck} onchange={(e) => setAck(e.currentTarget.checked)} data-testid="signin-insecure-ack" />Continue anyway</label>
        </div>
      {/if}
    {/if}

    {#if shownError}
      <div class="hint danger" role="alert" data-testid="signin-error">
        <Icon name="alert" /><span>{shownError.message}</span>
        {#if errorAction(shownError.error) === "retry"}
          <button class="btn btn-secondary btn-sm" type="button" bind:this={actionBtn} onclick={retry} data-testid="signin-retry">Try again</button>
        {:else if errorAction(shownError.error) === "signIn"}
          <button class="btn btn-secondary btn-sm" type="button" bind:this={actionBtn} onclick={() => start(lastMethod ?? shownError.method)} data-testid="signin-retry">Sign in</button>
        {:else if errorAction(shownError.error) === "advanced" && ongo}
          <button class="link" type="button" bind:this={actionBtn} onclick={() => ongo?.("sync")} data-testid="signin-open-advanced">Open Sync › Advanced</button>
        {/if}
      </div>
    {/if}

    <div class="row methods">
      {#if methods.length === 0}
        <button class="btn btn-primary" type="button" disabled data-testid="account-signin">{signIn.state === "checking" ? "Checking…" : "Sign in"}</button>
      {:else}
        {#each methods as m, i (m.id)}
          <button class="btn {i === 0 ? 'btn-primary' : 'btn-secondary'}" type="button" disabled={!ready} onclick={() => start(m.id)} data-testid="signin-method-{m.id}">{methodButtonLabel(m)}</button>
        {/each}
      {/if}
    </div>
    <hr />
    <p class="faint small">Without an account, ReqHarbor works fully offline. Nothing is sent anywhere until you sign in or turn on sync.</p>
  {/if}
</div>

<style>
  .flow {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    max-width: 520px;
  }
  .lead {
    margin: 0;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .server {
    width: 360px;
    max-width: 100%;
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .row {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    flex-wrap: wrap;
  }
  .methods {
    margin-top: var(--sp-2);
  }
  .hint {
    flex-wrap: wrap;
  }
  .canonical .row,
  .hint .row,
  .hint .checkbox {
    flex-basis: 100%;
  }
  .waiting {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .waiting p {
    margin: 0;
  }
  .title {
    font-size: var(--fs-lg);
    font-weight: var(--fw-semibold);
  }
  .timer {
    font-variant-numeric: tabular-nums;
  }
  hr {
    border: 0;
    border-top: 1px solid var(--border-subtle);
    width: 100%;
    margin: var(--sp-2) 0;
  }
</style>

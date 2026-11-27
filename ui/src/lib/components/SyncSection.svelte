<script lang="ts">
  import { onMount } from "svelte";
  import Icon from "./Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type RemoteWorkspace, type SpaceStatus } from "../api";
  import { copyText } from "../api/platform";
  import { insecureAckFor, isInsecureUrl, parseServerUrl } from "../util/account";
  import { formatSize, relativeTime, unsupportedNotice } from "../util/format";
  import { plural, spaceState, usageText } from "../util/spaces";

  /**
   * Settings › Sync (UX §11). Signed in: the per-space rows, the legacy block with Merge, and
   * token sync under a collapsed "Advanced". Signed out: a pointer to Account, with today's
   * token-sync UI under Advanced (open). With the accounts flag off: today's UI unchanged.
   */
  let { ongo }: { ongo?: (section: "account" | "shares") => void } = $props();

  const sync = $derived(app.sync);
  const accounts = $derived(app.auth.enabled);
  const signedIn = $derived(app.signedIn);
  const tokenConnected = $derived(sync.accountMode ? !!sync.legacy : sync.connected);
  // svelte-ignore state_referenced_locally
  let advancedOpen = $state(ui.syncAdvanced || !app.signedIn);
  $effect(() => {
    ui.syncAdvanced = false;
  });

  // svelte-ignore state_referenced_locally
  let workerUrl = $state(app.sync.workerUrl ?? "");
  let mode = $state<"create" | "connect">("create");
  let token = $state("");
  let shownToken = $state<{ token: string; note: string | null } | null>(null);
  let busy = $state(false);
  let syncError = $state<{ code: string; message: string } | null>(null);
  let info = $state<RemoteWorkspace | null>(null);

  async function loadInfo() {
    if (!tokenConnected || sync.legacy?.mode === "sharesOnly") return;
    try {
      info = await call("sync_workspace_info", {});
    } catch (e) {
      const err = toCommandError(e);
      if (err.code !== "sync_not_connected") syncError = err;
    }
  }

  async function run<T>(fn: () => Promise<T>): Promise<T | null> {
    busy = true;
    syncError = null;
    try {
      return await fn();
    } catch (e) {
      syncError = toCommandError(e);
      return null;
    } finally {
      busy = false;
    }
  }

  async function refreshStatus() {
    app.onSyncStatus(await call("get_sync_status", {}));
  }

  /** 06 §9: plain http to a non-loopback host needs "Continue anyway" for token sync too. */
  const legacyInsecure = $derived(isInsecureUrl(workerUrl));
  const legacyAck = $derived(insecureAckFor(app.knownServers, workerUrl));

  function setLegacyAck(on: boolean) {
    const p = parseServerUrl(workerUrl);
    if (!p.ok) return;
    const prev = app.knownServers.find((k) => k.url === p.origin);
    app.rememberServer(p.origin, { insecureAck: on, lastUsedAt: prev?.lastUsedAt ?? Date.now() });
  }

  async function connectOrCreate() {
    if (!workerUrl.trim()) return;
    if (legacyInsecure && !legacyAck) return;
    if (mode === "create") {
      const res = await run(() => call("sync_create_workspace", { workerUrl: workerUrl.trim(), name: app.workspaceName }));
      if (res) {
        shownToken = { token: res.token, note: null };
        toasts.success("Sync workspace created");
      }
    } else {
      const res = await run(() => call("sync_connect", { workerUrl: workerUrl.trim(), token: token.trim() }));
      if (res) {
        app.onSyncStatus(res);
        token = "";
        toasts.success("Sync connected");
        await app.reloadAll();
      }
    }
    await refreshStatus();
    await loadInfo();
  }

  async function syncNow() {
    const rep = await run(() => call("sync_now", {}));
    await refreshStatus();
    if (rep) {
      toasts.success(`Synced · ${rep.pushed} pushed, ${rep.pulled} pulled`);
      if (rep.tooLarge.length) toasts.warning(`Not synced (too large): ${rep.tooLarge.map((s) => s.name).join(", ")}`);
      const notice = unsupportedNotice(rep.unsupported?.length ?? 0);
      if (notice) toasts.warning(notice);
      await loadInfo();
    }
  }

  async function rotate() {
    const ok = await ui.confirm({ title: "Rotate sync token?", body: "The current token stops working immediately on every other device. You'll get a new token to copy.", confirmLabel: "Rotate token", danger: false });
    if (!ok) return;
    busy = true;
    try {
      const res = await call("sync_rotate_token", {});
      shownToken = { token: res.token, note: null };
    } catch (e) {
      const err = toCommandError(e);
      const d = err.details as { token?: string } | undefined;
      if (d?.token) shownToken = { token: d.token, note: err.message };
      else syncError = err;
    } finally {
      busy = false;
    }
  }

  async function disconnect() {
    const res = await run(() => call("sync_disconnect", {}));
    if (res) {
      app.onSyncStatus(res);
      info = null;
      shownToken = null;
      toasts.info("Sync disconnected — local data is untouched");
    }
  }

  async function disconnectLegacy() {
    const ok = await ui.confirm({
      title: "Disconnect legacy workspace?",
      body: "Forget the token? Its share links keep working, but you can no longer update or revoke them from ReqHarbor.",
      confirmLabel: "Disconnect",
    });
    if (!ok) return;
    const res = await run(() => call("legacy_disconnect", {}));
    if (res) {
      app.onSyncStatus(res);
      await app.reloadAll();
      toasts.info("Legacy workspace disconnected");
    }
  }

  function decide(sp: SpaceStatus) {
    ui.spaceNotice = { mode: sp.state === "removed" ? "removed" : "foreign", spaceIds: [sp.spaceId] };
  }

  onMount(() => {
    void loadInfo();
  });

  const accountSpaces = $derived(sync.spaces.filter((s) => s.kind !== "legacy"));
  const legacySpace = $derived(sync.spaces.find((s) => s.kind === "legacy") ?? null);
  const usagePct = $derived(info?.bytesUsed != null && info.quotaBytes ? Math.min(100, (info.bytesUsed / info.quotaBytes) * 100) : null);
  const retryIn = $derived(sync.nextRetryAt ? Math.max(0, Math.round((sync.nextRetryAt - Date.now()) / 1000)) : null);
  const pct = (sp: SpaceStatus) => (sp.bytesUsed != null && sp.quotaBytes ? Math.min(100, (sp.bytesUsed / sp.quotaBytes) * 100) : null);
  const ago = (ts: number | null) => (ts ? (relativeTime(ts, app.now) === "now" ? "just now" : `${relativeTime(ts, app.now)} ago`) : "");
</script>

{#snippet statusLine()}
  <div class="status" data-testid="sync-status">
    <Icon name={sync.state === "offline" || sync.state === "signInRequired" || sync.state === "paused" ? "cloud-off" : "cloud"} />
    <span>
      {#if sync.state === "offline"}
        Offline — {retryIn !== null ? `retrying in ${retryIn} s` : "will retry"}
      {:else if sync.state === "syncing"}
        Syncing…
      {:else if sync.state === "error"}
        Sync error{sync.lastError ? `: ${sync.lastError}` : ""}
      {:else if sync.state === "signInRequired"}
        Your session ended. Sign in again to keep syncing.
      {:else if sync.state === "paused"}
        Sync is paused
      {:else}
        Synced {ago(sync.lastSyncAt)}
      {/if}
    </span>
  </div>
{/snippet}

{#snippet tokenSync()}
  {#if shownToken}
    <div class="token-box" data-testid="sync-token-box">
      <span class="label">Workspace token</span>
      <div class="row">
        <input class="input mono" readonly value={shownToken.token} aria-label="Workspace token" data-testid="sync-token" />
        <button class="btn btn-secondary" type="button" onclick={async () => { await copyText(shownToken?.token ?? ""); toasts.success("Token copied"); }}>Copy</button>
      </div>
      <div class="hint warn"><Icon name="alert" />{shownToken.note ? `${shownToken.note} ` : ""}Copy it now — it won't be shown again.</div>
    </div>
  {/if}
  {#if tokenConnected && !sync.accountMode}
    {@render statusLine()}
    <div class="faint small mono">{sync.workerUrl}</div>
    {#if info}
      <div class="usage" data-testid="sync-usage">
        <div class="row between small"><span>{info.entityCount} items synced</span>
          {#if info.bytesUsed != null}<span>{formatSize(info.bytesUsed)}{info.quotaBytes ? ` of ${formatSize(info.quotaBytes)}` : ""}</span>{/if}
        </div>
        {#if usagePct !== null}<div class="meter"><div class="fill" class:high={usagePct > 85} style:width="{usagePct}%"></div></div>{/if}
      </div>
    {/if}
    {#if sync.warning}
      <div class="hint warn" data-testid="sync-warning-text"><Icon name="alert" />{sync.warning}</div>
    {/if}
    <div class="row">
      <button class="btn btn-primary" type="button" disabled={busy} onclick={syncNow} data-testid="sync-now">Sync now</button>
      <button class="btn btn-secondary" type="button" disabled={busy} onclick={rotate} data-testid="sync-rotate">Rotate token…</button>
      <button class="btn btn-ghost btn-danger" type="button" disabled={busy} onclick={disconnect} data-testid="sync-disconnect">Disconnect</button>
    </div>
  {:else if tokenConnected}
    <p class="faint small">Token sync with <span class="mono">{sync.legacy?.workerUrl}</span>.</p>
    <div class="row">
      {#if sync.legacy?.mode === "sync"}<button class="btn btn-secondary" type="button" disabled={busy} onclick={rotate} data-testid="sync-rotate">Rotate token…</button>{/if}
      <button class="btn btn-ghost btn-danger" type="button" disabled={busy} onclick={disconnectLegacy} data-testid="sync-disconnect">Disconnect…</button>
    </div>
  {:else}
    {#if accounts}
      <p class="faint small" data-testid="sync-advanced-note">Sync without an account: connect this device to a server with a workspace token.</p>
    {/if}
    <label class="field">
      <span>Server URL</span>
      <input class="input mono" bind:value={workerUrl} placeholder="https://reqharbor.example.com" data-testid="sync-worker-url" />
    </label>
    {#if legacyInsecure}
      <div class="hint warn insecure" data-testid="sync-insecure">
        <Icon name="alert" /><span>This server doesn't use HTTPS. Your password, tokens and synced data can be read on the network.</span>
        <label class="checkbox"><input type="checkbox" checked={legacyAck} onchange={(e) => setLegacyAck(e.currentTarget.checked)} data-testid="sync-insecure-ack" />Continue anyway</label>
      </div>
    {/if}
    <div class="cards" role="radiogroup" aria-label="Connection mode">
      <label class="card" class:on={mode === "create"}>
        <input type="radio" name="sync-mode" value="create" bind:group={mode} data-testid="sync-mode-create" />
        <span><b>Create workspace</b><br /><span class="faint small">Start syncing this device; you get a token for others.</span></span>
      </label>
      <label class="card" class:on={mode === "connect"}>
        <input type="radio" name="sync-mode" value="connect" bind:group={mode} data-testid="sync-mode-connect" />
        <span><b>Connect with token</b><br /><span class="faint small">Join an existing workspace from another device.</span></span>
      </label>
    </div>
    {#if mode === "connect"}
      <label class="field">
        <span>Token</span>
        <input class="input mono" type="password" bind:value={token} placeholder="rh_…" data-testid="sync-token-input" />
      </label>
    {/if}
    <div class="row">
      <button class="btn btn-primary" type="button" disabled={busy || !workerUrl.trim() || (mode === "connect" && !token.trim()) || (legacyInsecure && !legacyAck)} onclick={connectOrCreate} data-testid="sync-connect">
        {mode === "create" ? "Create workspace" : "Connect"}
      </button>
    </div>
  {/if}
{/snippet}

{#snippet advanced()}
  <div class="advanced">
    <button class="disclosure" type="button" aria-expanded={advancedOpen} onclick={() => (advancedOpen = !advancedOpen)} data-testid="sync-advanced">
      <Icon name={advancedOpen ? "chev-d" : "chev-r"} small />Advanced: sync without an account
    </button>
    {#if advancedOpen}
      <div class="advanced-body">{@render tokenSync()}</div>
    {/if}
  </div>
{/snippet}

<h3>Sync</h3>
{#if sync.state === "unauthorized"}
  <div class="hint danger" role="alert" data-testid="sync-unauthorized"><Icon name="alert" />Token rejected — sync disconnected. Enter a new token.</div>
{/if}

{#if !accounts || (!sync.accountMode && sync.connected)}
  <!-- The accounts flag is off, or pure token sync is on: today's UI, expanded (UX §11). -->
  {@render tokenSync()}
{:else}
  {#if signedIn && sync.accountMode}
    <div class="row between">
      {@render statusLine()}
      <button class="btn btn-primary" type="button" disabled={busy || !sync.connected} onclick={syncNow} data-testid="sync-now">Sync now</button>
    </div>
  {:else}
    <p class="signin-note" data-testid="sync-signin-note">
      Sign in to sync across devices.
      <button class="link" type="button" onclick={() => ongo?.("account")} data-testid="sync-signin-link">Account ›</button>
    </p>
  {/if}

  {#if sync.legacy}
    <section class="block" data-testid="legacy-block">
      <h4>Legacy workspace</h4>
      <div class="space-row">
        <Icon name="key" />
        {#if sync.legacy.mode === "sync"}
          <span class="grow">Synced with a token · {plural(sync.legacy.projectCount, "project")}</span>
          {#if signedIn}
            <button class="btn btn-secondary btn-sm" type="button" onclick={() => (ui.mergeLegacy = true)} data-testid="merge-legacy">Merge into my personal space…</button>
          {/if}
        {:else}
          <span class="grow">Shares only · syncing stopped</span>
          <button class="link" type="button" onclick={() => ongo?.("shares")} data-testid="legacy-manage-shares">Manage in Shares ›</button>
        {/if}
      </div>
      {#if legacySpace?.issue}<div class="hint warn small" data-testid="legacy-issue"><Icon name="alert" />{legacySpace.issue.message}</div>{/if}
      {#if sync.legacy.mode === "sharesOnly"}
        <div class="row">
          <button class="btn btn-ghost btn-danger btn-sm" type="button" disabled={busy} onclick={disconnectLegacy} data-testid="legacy-disconnect">Disconnect legacy workspace…</button>
        </div>
      {/if}
    </section>
  {/if}

  {#if accountSpaces.length}
    <section class="block">
      <h4>Spaces</h4>
      {#each accountSpaces as sp (sp.spaceId)}
        <div class="space" data-testid="sync-space-{sp.spaceId}">
          <div class="space-row">
            <Icon name={sp.kind === "team" ? "users" : sp.state === "active" ? "cloud" : "cloud-off"} />
            <span class="sname">{sp.name}</span>
            <span class="faint small">{plural(sp.projectCount, "project")}</span>
            <span class="grow"></span>
            {#if usageText(sp, formatSize)}<span class="faint small tabular">{usageText(sp, formatSize)}</span>{/if}
            {#if pct(sp) !== null}<span class="meter mini"><span class="fill" class:high={(pct(sp) ?? 0) > 85} style:width="{pct(sp)}%"></span></span>{/if}
          </div>
          {#if sp.issue || sp.state !== "active"}
            <div class="space-msg small" class:warn={!!sp.issue && sp.issue.code !== "clockFixed"} data-testid="sync-space-msg-{sp.spaceId}">
              {#if sp.issue}<Icon name="alert" small />{/if}
              <span>{sp.state === "paused" && sp.reason === "foreignAccount" && sp.accountEmail ? `Synced by ${sp.accountEmail} · Paused` : spaceState(sp)}</span>
              {#if sp.state === "removed" || (sp.state === "paused" && sp.reason === "foreignAccount")}
                <button class="link" type="button" onclick={() => decide(sp)} data-testid="sync-space-decide-{sp.spaceId}">Decide</button>
              {/if}
            </div>
          {:else if sp.lastSyncAt}
            <div class="faint small">Synced {ago(sp.lastSyncAt)}</div>
          {/if}
        </div>
      {/each}
    </section>
  {/if}
  {#if sync.accountMode && sync.warning}
    <div class="hint warn" data-testid="sync-warning-text"><Icon name="alert" />{sync.warning}</div>
  {/if}
  {@render advanced()}
{/if}

{#if syncError}
  <div class="hint danger" role="alert" data-testid="sync-error">
    <Icon name="alert" />
    {#if syncError.code === "sync_quota_exceeded"}
      Workspace storage is full. {syncError.message}
    {:else if syncError.code === "sync_too_large"}
      An item is too large to sync. {syncError.message}
    {:else}
      {syncError.message}
    {/if}
  </div>
{/if}

<style>
  h3 {
    margin: 0;
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
  }
  h4 {
    margin: 0 0 var(--sp-3);
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    font-weight: var(--fw-semibold);
  }
  .row {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    flex-wrap: wrap;
  }
  .between {
    justify-content: space-between;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .tabular {
    font-variant-numeric: tabular-nums;
  }
  .status {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
  }
  .signin-note {
    margin: 0;
    color: var(--text-2);
  }
  .block {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .space {
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
    padding: var(--sp-2) 0;
  }
  .space-row {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
    min-height: 24px;
  }
  .sname {
    font-weight: var(--fw-medium);
  }
  .grow {
    flex: 1;
  }
  .space-msg {
    display: flex;
    gap: var(--sp-2);
    align-items: center;
    color: var(--text-3);
    padding-left: calc(16px + var(--sp-3));
  }
  .space-msg.warn {
    color: var(--warning);
  }
  .usage {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
    max-width: 420px;
  }
  .meter {
    height: 6px;
    border-radius: var(--r-pill);
    background: var(--bg-input);
    overflow: hidden;
  }
  .meter.mini {
    display: inline-block;
    width: 64px;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .fill.high {
    background: var(--warning);
  }
  .advanced {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    border-top: 1px solid var(--border-subtle);
    padding-top: var(--sp-4);
  }
  .advanced-body {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .disclosure {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    border: 0;
    background: none;
    padding: 0;
    color: var(--text-2);
    font-weight: var(--fw-medium);
    align-self: flex-start;
  }
  .disclosure:hover {
    color: var(--text-1);
  }
  .cards {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: var(--sp-4);
  }
  .card {
    display: flex;
    gap: var(--sp-4);
    align-items: flex-start;
    padding: var(--sp-5);
    border-radius: var(--r-md);
    background: var(--bg-input);
    border: 1px solid transparent;
    cursor: pointer;
  }
  .card.on {
    border-color: var(--border-focus);
  }
  .insecure {
    flex-wrap: wrap;
  }
  .insecure .checkbox {
    flex-basis: 100%;
  }
  .token-box {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .token-box .input {
    flex: 1;
  }
</style>

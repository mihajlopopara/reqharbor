<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type RemoteWorkspace, type ShareInfo, type Theme } from "../api";
  import { copyText } from "../api/platform";
  import { formatSize, relativeTime, dateTime } from "../util/format";
  import { SHORTCUT_LIST, shortcutKeys } from "../util/shortcuts";

  type Section = "general" | "sync" | "shares" | "shortcuts" | "about";
  let { section: initial }: { section: Section } = $props();
  // svelte-ignore state_referenced_locally
  let section = $state<Section>(initial);
  const close = () => (ui.settingsDialog = null);
  const NAV: { id: Section; label: string }[] = [
    { id: "general", label: "General" },
    { id: "sync", label: "Sync" },
    { id: "shares", label: "Shares" },
    { id: "shortcuts", label: "Shortcuts" },
    { id: "about", label: "About" },
  ];

  // ───────── sync ─────────
  const sync = $derived(app.sync);
  // svelte-ignore state_referenced_locally
  let workerUrl = $state(app.sync.workerUrl ?? "");
  let mode = $state<"create" | "connect">("create");
  let token = $state("");
  let shownToken = $state<{ token: string; note: string | null } | null>(null);
  let busy = $state(false);
  let syncError = $state<{ code: string; message: string } | null>(null);
  let info = $state<RemoteWorkspace | null>(null);

  async function loadInfo() {
    if (!app.sync.connected) return;
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

  async function connectOrCreate() {
    if (!workerUrl.trim()) return;
    if (mode === "create") {
      const res = await run(() => call("sync_create_workspace", { workerUrl: workerUrl.trim(), name: app.workspaceName }));
      if (res) {
        shownToken = { token: res.token, note: null };
        toasts.success("Sync workspace created");
      }
    } else {
      const res = await run(() => call("sync_connect", { workerUrl: workerUrl.trim(), token: token.trim() }));
      if (res) {
        app.sync = res;
        token = "";
        toasts.success("Sync connected");
        await app.reloadAll();
      }
    }
    app.sync = await call("get_sync_status", {});
    await loadInfo();
  }

  async function syncNow() {
    const rep = await run(() => call("sync_now", {}));
    app.sync = await call("get_sync_status", {});
    if (rep) {
      toasts.success(`Synced · ${rep.pushed} pushed, ${rep.pulled} pulled`);
      if (rep.tooLarge.length) toasts.warning(`Not synced (too large): ${rep.tooLarge.map((s) => s.name).join(", ")}`);
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
      app.sync = res;
      info = null;
      shownToken = null;
      toasts.info("Sync disconnected — local data is untouched");
    }
  }

  // ───────── shares ─────────
  let shares = $state<ShareInfo[] | null>(null);
  let sharesError = $state<string | null>(null);
  async function loadShares() {
    try {
      shares = await call("share_list", {});
      sharesError = null;
    } catch (e) {
      const err = toCommandError(e);
      shares = [];
      sharesError = err.code === "sync_not_connected" ? "Connect sync to create and manage share links." : err.message;
    }
  }
  /** QA-R2-002: re-upload with the share's original options; if unknown, ask via the Share dialog. */
  async function updateShare(s: ShareInfo) {
    if (!s.kind || !s.entityId) return;
    const opts = app.shareOptions(s.shareId);
    if (!opts) {
      ui.settingsDialog = null;
      ui.shareDialog = { kind: s.kind, entityId: s.entityId, name: s.name ?? "Shared item" };
      return;
    }
    try {
      await call("share_update", { shareId: s.shareId, kind: s.kind, entityId: s.entityId, includeCredentials: opts.includeCredentials, environmentId: opts.environmentId });
      toasts.success("Share updated");
      await loadShares();
    } catch (e) {
      app.reportError(e);
    }
  }
  async function revokeShare(s: ShareInfo) {
    try {
      await call("share_revoke", { shareId: s.shareId });
      app.rememberShareOptions(s.shareId, null);
      toasts.success("Share revoked");
      await loadShares();
    } catch (e) {
      app.reportError(e);
    }
  }

  $effect(() => {
    if (section === "shares" && shares === null) void loadShares();
  });

  onMount(() => {
    void loadInfo();
  });

  function setTheme(t: Theme) {
    app.updateSettings({ theme: t }, true);
  }

  const usagePct = $derived(info?.bytesUsed != null && info.quotaBytes ? Math.min(100, (info.bytesUsed / info.quotaBytes) * 100) : null);
  const retryIn = $derived(sync.nextRetryAt ? Math.max(0, Math.round((sync.nextRetryAt - Date.now()) / 1000)) : null);
</script>

<Dialog title="Settings" width={760} height={540} testid="settings-dialog" onclose={close}>
  <div class="layout">
    <nav aria-label="Settings sections">
      {#each NAV as n (n.id)}
        <button type="button" class="nav" aria-current={section === n.id ? "page" : undefined} onclick={() => (section = n.id)} data-testid="settings-nav-{n.id}">{n.label}</button>
      {/each}
    </nav>
    <div class="content">
      {#if section === "general"}
        <h3>General</h3>
        <div class="field">
          <span>Theme</span>
          <div class="seg" role="group" aria-label="Theme">
            {#each [["system", "System"], ["dark", "Dark"], ["light", "Light"]] as [v, l] (v)}
              <button type="button" aria-pressed={app.settings.theme === v} onclick={() => setTheme(v as Theme)} data-testid="theme-{v}">{l}</button>
            {/each}
          </div>
        </div>
        <label class="checkbox">
          <input type="checkbox" checked={app.exportKeepVariables} onchange={(e) => { app.exportKeepVariables = e.currentTarget.checked; app.updateSettings({ exportKeepVariables: e.currentTarget.checked }); }} />
          Copy as cURL keeps <span class="mono">{"{{variables}}"}</span>
        </label>
      {:else if section === "sync"}
        <h3>Sync</h3>
        {#if sync.state === "unauthorized"}
          <div class="hint danger" role="alert" data-testid="sync-unauthorized"><Icon name="alert" />Token rejected — sync disconnected. Enter a new token.</div>
        {/if}
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
        {#if sync.connected}
          <div class="status" data-testid="sync-status">
            <Icon name={sync.state === "offline" ? "cloud-off" : "cloud"} />
            <span>
              {#if sync.state === "offline"}
                Offline — {retryIn !== null ? `retrying in ${retryIn} s` : "will retry"}
              {:else if sync.state === "syncing"}
                Syncing…
              {:else if sync.state === "error"}
                Sync error{sync.lastError ? `: ${sync.lastError}` : ""}
              {:else}
                Synced {sync.lastSyncAt ? relativeTime(sync.lastSyncAt, app.now) === "now" ? "just now" : `${relativeTime(sync.lastSyncAt, app.now)} ago` : ""}
              {/if}
            </span>
          </div>
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
        {:else}
          <label class="field">
            <span>Worker URL</span>
            <input class="input mono" bind:value={workerUrl} placeholder="https://reqharbor.example.workers.dev" data-testid="sync-worker-url" />
          </label>
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
            <button class="btn btn-primary" type="button" disabled={busy || !workerUrl.trim() || (mode === "connect" && !token.trim())} onclick={connectOrCreate} data-testid="sync-connect">
              {mode === "create" ? "Create workspace" : "Connect"}
            </button>
          </div>
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
      {:else if section === "shares"}
        <h3>Shares</h3>
        {#if sharesError}
          <p class="faint">{sharesError}</p>
        {:else if shares === null}
          <p class="faint">Loading…</p>
        {:else if shares.length === 0}
          <p class="faint" data-testid="shares-empty">No active shares.</p>
        {:else}
          <table class="tbl" data-testid="shares-table">
            <thead><tr><th>Name</th><th>Kind</th><th>Created</th><th>Updated</th><th></th></tr></thead>
            <tbody>
              {#each shares as s (s.shareId)}
                <tr>
                  <td>{s.name ?? s.shareId}</td>
                  <td class="faint">{s.kind ?? "—"}</td>
                  <td class="faint">{s.createdAt ? dateTime(s.createdAt) : "—"}</td>
                  <td class="faint">{s.updatedAt ? dateTime(s.updatedAt) : "—"}</td>
                  <td class="acts">
                    <button class="link" type="button" onclick={async () => { await copyText(s.url); toasts.success("Link copied"); }}>Copy link</button>
                    <button class="link" type="button" onclick={() => updateShare(s)}>Update</button>
                    <button class="link danger" type="button" onclick={() => revokeShare(s)}>Revoke</button>
                  </td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      {:else if section === "shortcuts"}
        <h3>Shortcuts</h3>
        <div class="shortcuts">
          {#each SHORTCUT_LIST as s (s.label)}
            <span class="muted">{s.label}</span>
            <span class="keys">{#each shortcutKeys(s.spec, app.platform) as k, i (i)}<span class="kbd">{k}</span>{/each}</span>
          {/each}
        </div>
      {:else}
        <h3>About</h3>
        <p><b>ReqHarbor</b> 0.1.0 — Postman's job, a tenth of the weight.</p>
        <p class="faint small">Workspace “{app.workspaceName}” · platform {app.platform}</p>
        <p class="faint small">No analytics, no telemetry. Your data stays in local JSON files unless you turn on sync.</p>
      {/if}
    </div>
  </div>
</Dialog>

<style>
  .layout {
    display: grid;
    grid-template-columns: 160px 1fr;
    gap: var(--sp-6);
    min-height: 100%;
  }
  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .nav {
    height: 28px;
    text-align: left;
    border: 0;
    background: none;
    border-radius: var(--r-sm);
    padding: 0 var(--sp-4);
    color: var(--text-2);
  }
  .nav:hover {
    background: var(--bg-hover);
    color: var(--text-1);
  }
  .nav[aria-current="page"] {
    background: var(--bg-selected);
    color: var(--text-1);
    font-weight: var(--fw-medium);
  }
  .content {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: var(--fs-xl);
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
  .status {
    display: flex;
    gap: var(--sp-3);
    align-items: center;
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
  .fill {
    height: 100%;
    background: var(--accent);
  }
  .fill.high {
    background: var(--warning);
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
  .token-box {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .token-box .input {
    flex: 1;
  }
  .tbl {
    width: 100%;
    border-collapse: collapse;
  }
  .tbl th {
    text-align: left;
    font-size: var(--fs-sm);
    color: var(--text-3);
    font-weight: var(--fw-medium);
    padding: var(--sp-3);
  }
  .tbl td {
    padding: var(--sp-3);
    border-top: 1px solid var(--border-subtle);
  }
  .acts {
    white-space: nowrap;
    text-align: right;
  }
  .link.danger {
    color: var(--danger);
  }
  .shortcuts {
    display: grid;
    grid-template-columns: 1fr auto;
    gap: var(--sp-4) var(--sp-8);
    align-items: center;
    max-width: 440px;
  }
  .keys {
    display: flex;
    gap: 3px;
    justify-content: flex-end;
  }
</style>

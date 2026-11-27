<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui, type SettingsSection } from "../stores/ui.svelte";
  import AccountSection from "../components/AccountSection.svelte";
  import SyncSection from "../components/SyncSection.svelte";
  import TeamsSection from "../components/TeamsSection.svelte";
  import { createdByText, shareLockReason } from "../util/teams";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type ShareInfo, type Theme } from "../api";
  import { copyText } from "../api/platform";
  import { dateTime } from "../util/format";
  import { SHORTCUT_LIST, shortcutKeys } from "../util/shortcuts";

  type Section = SettingsSection;
  let { section: initial, teamId: initialTeam = null }: { section: Section; teamId?: string | null } = $props();
  // svelte-ignore state_referenced_locally
  let section = $state<Section>(initial === "account" && !app.auth.enabled ? "general" : initial);
  // svelte-ignore state_referenced_locally
  let teamId = $state<string | null>(initialTeam);
  // Another part of the app points Settings somewhere (Team settings ›, a new team).
  $effect(() => {
    const d = ui.settingsDialog;
    if (!d) return;
    section = d.section === "account" && !app.auth.enabled ? "general" : d.section;
    if (d.teamId !== undefined) teamId = d.teamId ?? null;
  });
  const close = () => (ui.settingsDialog = null);
  const NAV: { id: Section; label: string }[] = [
    ...(app.auth.enabled ? [{ id: "account" as const, label: "Account" }] : []),
    { id: "general", label: "General" },
    { id: "sync", label: "Sync" },
    ...(app.teamsOn ? [{ id: "teams" as const, label: "Teams" }] : []),
    { id: "shares", label: "Shares" },
    { id: "shortcuts", label: "Shortcuts" },
    { id: "about", label: "About" },
  ];

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
  /** My role in the space a share lives in (team shares only). */
  const roleOfShare = (s: ShareInfo) => app.sync.spaces.find((x) => x.spaceId === s.spaceId && x.kind === "team")?.role ?? null;

  /** QA-R2-002: re-upload with the share's original options; if unknown, ask via the Share dialog. */
  async function updateShare(s: ShareInfo) {
    if (!s.kind || !s.entityId) return;
    // Account spaces keep the options on the server (AC-TSHARE-3).
    const opts = s.options ?? app.shareOptions(s.shareId);
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
      await call("share_revoke", { shareId: s.shareId, spaceId: s.spaceId ?? null });
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

  function setTheme(t: Theme) {
    app.updateSettings({ theme: t }, true);
  }

  /** UX §12.1: shares grouped by space (PERSONAL, …, LEGACY) in account mode. */
  const spaceName = (id: string | null | undefined) => (id === "legacy" ? "Legacy" : (app.sync.spaces.find((s) => s.spaceId === id)?.name ?? ""));
  const shareGroups = $derived.by(() => {
    const list = shares ?? [];
    if (!app.sync.accountMode) return [{ name: "", items: list }];
    const names = [...new Set(list.map((s) => spaceName(s.spaceId)))];
    return names.map((name) => ({ name, items: list.filter((s) => spaceName(s.spaceId) === name) }));
  });
  function go(s: Section) {
    if (s === "sync") ui.syncAdvanced = true;
    section = s;
  }
</script>

<Dialog title="Settings" width={760} height={540} testid="settings-dialog" onclose={close}>
  <div class="layout">
    <nav aria-label="Settings sections">
      {#each NAV as n (n.id)}
        <button
          type="button"
          class="nav"
          aria-current={section === n.id ? "page" : undefined}
          onclick={() => {
            section = n.id;
            if (n.id === "teams") teamId = null;
          }}
          data-testid="settings-nav-{n.id}">{n.label}</button
        >
      {/each}
    </nav>
    <div class="content">
      {#if section === "account"}
        <AccountSection ongo={go} />
      {:else if section === "general"}
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
        <SyncSection ongo={(s) => (section = s)} />
      {:else if section === "teams"}
        <TeamsSection bind:teamId />
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
            <thead><tr><th>Name</th><th>Kind</th>{#if app.sync.accountMode}<th>Created by</th>{/if}<th>Created</th><th>Updated</th><th></th></tr></thead>
            <tbody>
              {#each shareGroups as g (g.name)}
              {#if g.name}<tr class="group"><th colspan={app.sync.accountMode ? 6 : 5} data-testid="shares-group">{g.name}</th></tr>{/if}
              {#each g.items as s (s.shareId)}
                {@const role = roleOfShare(s)}
                {@const mine = !s.createdBy || s.createdBy.userId === app.auth.account?.userId}
                {@const updLock = shareLockReason(role, mine, "update")}
                {@const revLock = shareLockReason(role, mine, "revoke")}
                <tr data-testid="share-row">
                  <td>{s.name ?? s.shareId}</td>
                  <td class="faint">{s.kind ?? "—"}</td>
                  {#if app.sync.accountMode}<td class="faint" data-testid="share-created-by">{s.createdBy ? createdByText(s.createdBy, app.auth.account?.userId ?? null) : s.spaceId === "legacy" ? "—" : "You"}</td>{/if}
                  <td class="faint">{s.createdAt ? dateTime(s.createdAt) : "—"}</td>
                  <td class="faint">{s.updatedAt ? dateTime(s.updatedAt) : "—"}</td>
                  <td class="acts">
                    <button class="link" type="button" onclick={async () => { await copyText(s.url); toasts.success("Link copied"); }}>Copy link</button>
                    <button class="link" type="button" aria-disabled={!!updLock} title={updLock ?? undefined} onclick={() => (updLock ? toasts.info(updLock) : updateShare(s))} data-testid="share-update">Update</button>
                    <button class="link danger" type="button" aria-disabled={!!revLock} title={revLock ?? undefined} onclick={() => (revLock ? toasts.info(revLock) : revokeShare(s))} data-testid="share-revoke">Revoke</button>
                  </td>
                </tr>
              {/each}
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
  .small {
    font-size: var(--fs-sm);
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
  .tbl .group th {
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    padding-top: var(--sp-5);
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

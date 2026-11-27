<script lang="ts">
  /** Settings › Account (UX §4.2): signed out → SignInFlow; signed in → profile, Devices, Sign out. */
  import { onMount } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type SessionInfo } from "../api";
  import { deviceLine, methodName, serverHost } from "../util/account";
  import Avatar from "./Avatar.svelte";
  import Icon from "./Icon.svelte";
  import SignInFlow from "./SignInFlow.svelte";

  let { ongo }: { ongo?: (section: "sync") => void } = $props();

  const auth = $derived(app.auth);
  const account = $derived(auth.keychainUnreadable ? null : auth.account);
  let devices = $state<SessionInfo[] | null>(null);
  let devicesError = $state<string | null>(null);
  let showLoading = $state(false);
  let reauth = $state(false);

  async function loadDevices() {
    if (!account || auth.signInRequired) return;
    devicesError = null;
    const t = setTimeout(() => (showLoading = true), 300);
    try {
      devices = await call("auth_sessions", {});
    } catch (e) {
      const err = toCommandError(e);
      devicesError = err.code === "server_unreachable" ? "You're offline" : err.message;
    } finally {
      clearTimeout(t);
      showLoading = false;
    }
  }

  async function refresh() {
    if (!account || auth.signInRequired) return;
    try {
      app.auth = await call("auth_me", {});
    } catch {
      /* the status event already carries signInRequired / offline */
    }
    await loadDevices();
  }

  async function signOut() {
    if (await app.signOut()) devices = null;
  }

  async function revoke(d: SessionInfo) {
    const ok = await ui.confirm({
      title: `Sign out ${d.deviceName || "this device"}?`,
      body: "It stops syncing on its next request. Its local data stays.",
      confirmLabel: "Sign out device",
      danger: false,
    });
    if (!ok) return;
    try {
      await call("auth_session_revoke", { sessionId: d.id });
      toasts.success(`Signed out ${d.deviceName || "the device"}`);
      await loadDevices();
    } catch (e) {
      app.reportError(e);
    }
  }

  async function signInAgain() {
    if (!account) return;
    reauth = true;
    try {
      app.auth = await call("auth_sign_in_start", { serverUrl: account.serverUrl, method: account.method || null });
    } catch {
      /* shown by SignInFlow from the status */
    }
  }

  // Load devices whenever a usable session appears (e.g. right after sign-in).
  $effect(() => {
    if (account && !auth.signInRequired && devices === null) void loadDevices();
    if (!account) devices = null;
  });
  $effect(() => {
    if (!auth.signInRequired) reauth = false;
  });

  onMount(() => {
    void refresh();
  });
</script>

<h3>Account</h3>
{#if auth.signInRequired && account}
  <div class="hint danger" role="alert" data-testid="account-session-ended">
    <Icon name="alert" /><span>Your session ended. Sign in again to keep syncing. Your changes are saved on this device.</span>
    {#if !reauth}<button class="btn btn-secondary btn-sm" type="button" onclick={signInAgain} data-testid="account-signin-again">Sign in again</button>{/if}
  </div>
{/if}
{#if auth.keychainUnreadable}
  <div class="hint danger" role="alert" data-testid="account-keychain-unreadable"><Icon name="alert" />ReqHarbor couldn't read your sign-in from the keychain.</div>
{/if}
{#if auth.keychain === "file" && auth.keychainNote && account}
  <div class="hint warn" data-testid="account-file-store"><Icon name="alert" />{auth.keychainNote}</div>
{/if}

{#if !account}
  <SignInFlow {ongo} />
{:else if auth.signInRequired && reauth}
  <SignInFlow presetServer={account.serverUrl} {ongo} />
{:else}
  <div class="who" data-testid="account-profile">
    <Avatar name={account.name} email={account.email} size={32} />
    <div class="id">
      <div class="name" data-testid="account-name">{account.name || account.email}</div>
      <div class="faint small" data-testid="account-meta">
        {[account.email, account.method ? `Signed in with ${methodName(account.method)}` : "", serverHost(account.serverUrl)].filter(Boolean).join(" · ")}
      </div>
    </div>
    <button class="btn btn-secondary" type="button" onclick={signOut} data-testid="account-signout">Sign out…</button>
  </div>
  {#if auth.offline}
    <p class="faint small" data-testid="account-offline">Offline · showing what was last synced</p>
  {/if}

  {#if !auth.signInRequired}
    <h4>Devices</h4>
    {#if devicesError}
      <p class="faint small" data-testid="devices-error">{devicesError}</p>
    {:else if devices === null}
      {#if showLoading}<p class="faint small">Loading…</p>{/if}
    {:else}
      <table class="tbl" data-testid="devices-table">
        <tbody>
          {#each devices as d (d.id)}
            <tr data-testid="device-row">
              <td>{d.deviceName || "Unnamed device"}</td>
              <td class="faint">{d.current ? "This device" : ""}</td>
              <td class="faint">{deviceLine(d, app.now)}</td>
              <td class="acts">
                {#if !d.current}
                  <button class="link" type="button" aria-disabled={auth.offline ? "true" : undefined} title={auth.offline ? "You're offline" : undefined} onclick={() => !auth.offline && revoke(d)} data-testid="device-signout">Sign out</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  {/if}
{/if}

<style>
  h3 {
    margin: 0;
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
  }
  h4 {
    margin: var(--sp-4) 0 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: var(--fw-semibold);
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-5);
  }
  .id {
    flex: 1;
    min-width: 0;
  }
  .name {
    font-weight: var(--fw-semibold);
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
  .hint {
    flex-wrap: wrap;
  }
  .tbl {
    width: 100%;
    border-collapse: collapse;
  }
  .tbl td {
    height: 28px;
    padding: 0 var(--sp-3);
    border-top: 1px solid var(--border-subtle);
  }
  .acts {
    text-align: right;
    white-space: nowrap;
  }
</style>

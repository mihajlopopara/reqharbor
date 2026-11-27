<script lang="ts">
  import { onMount } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type ShareInfo, type SharePreview, type ShareKind } from "../api";
  import { copyText } from "../api/platform";

  /** Share dialog (AC-SHARE-1/2/4, DESIGN §5.11). */
  let { kind, entityId, name }: { kind: ShareKind; entityId: string; name: string } = $props();

  let includeCredentials = $state(false);
  let environmentId = $state<string>("");
  let preview = $state<SharePreview | null>(null);
  let existing = $state<ShareInfo | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let needsSync = $state(!app.sync.connected);

  const close = () => (ui.shareDialog = null);

  async function loadPreview() {
    try {
      preview = await call("share_preview", { kind, entityId, includeCredentials, environmentId: environmentId || null });
      error = null;
    } catch (e) {
      // e.g. read_only_newer: an item from a newer ReqHarbor can only be shared with credentials.
      preview = null;
      error = toCommandError(e).message;
    }
  }

  onMount(() => {
    void loadPreview();
    if (!needsSync) {
      void call("share_list", {})
        .then((list) => {
          existing = list.find((s) => s.entityId === entityId) ?? null;
          // QA-R2-002: start from the options this share was uploaded with
          const opts = existing ? app.shareOptions(existing.shareId) : null;
          if (opts) {
            includeCredentials = opts.includeCredentials;
            environmentId = opts.environmentId && app.environments.some((e) => e.id === opts.environmentId) ? opts.environmentId : "";
            void loadPreview();
          }
        })
        .catch((e) => {
          if (toCommandError(e).code === "sync_not_connected") needsSync = true;
        });
    }
  });

  async function create() {
    busy = true;
    error = null;
    try {
      const args = { kind, entityId, includeCredentials, environmentId: environmentId || null };
      const info = existing ? await call("share_update", { shareId: existing.shareId, ...args }) : await call("share_create", args);
      // QA-R2-003: fill fields an older backend may return as null
      existing = { ...info, kind: info.kind ?? kind, entityId: info.entityId ?? entityId, name: info.name ?? name };
      app.rememberShareOptions(existing.shareId, { includeCredentials, environmentId: args.environmentId });
      await copyText(existing.url);
      toasts.success("Share link copied to clipboard");
    } catch (e) {
      const err = toCommandError(e);
      if (err.code === "sync_not_connected") needsSync = true;
      else error = err.message;
    } finally {
      busy = false;
    }
  }

  async function revoke() {
    if (!existing) return;
    const s = existing;
    const ok = await ui.confirm({ title: "Revoke share link?", body: "Anyone opening the link will get “410 Gone”. This can't be undone.", confirmLabel: "Revoke" });
    if (!ok) return;
    try {
      await call("share_revoke", { shareId: s.shareId });
      app.rememberShareOptions(s.shareId, null);
      existing = null;
      toasts.success("Share link revoked");
    } catch (e) {
      app.reportError(e);
    }
  }
</script>

<Dialog title="Share “{name}”" width={520} testid="share-dialog" footer={needsSync ? undefined : shareFooter} onclose={close} onsubmit={() => !needsSync && !busy && preview && void create()}>
  {#if needsSync}
    <div class="empty" data-testid="share-needs-sync">
      <Icon name="cloud-off" />
      <h3>Sharing needs sync</h3>
      <p>Connect a ReqHarbor Worker to create read-only share links.</p>
      <button
        class="btn btn-primary"
        type="button"
        onclick={() => {
          close();
          ui.settingsDialog = { section: "sync" };
        }}
        data-testid="share-setup-sync">Set up sync</button
      >
    </div>
  {:else}
    <section>
      <h4>Removed before sharing</h4>
      {#if preview?.secretVariablesRemoved}
        <div class="irow"><Icon name="lock" small /><span>{preview.secretVariablesRemoved} secret variable{preview.secretVariablesRemoved === 1 ? "" : "s"} — always removed</span></div>
      {:else}
        <div class="irow faint"><Icon name="lock" small /><span>No secret variables in the attached environment</span></div>
      {/if}
      <div class="irow">
        <Icon name="eye-off" small />
        <span>
          {#if includeCredentials}
            Credentials included · {preview?.credentialFields ?? 0} field{preview?.credentialFields === 1 ? "" : "s"}
          {:else}
            Auth headers and values → <span class="mono">{"{{placeholders}}"}</span> · {preview?.credentialsReplaced ?? 0} replaced
          {/if}
        </span>
      </div>
    </section>
    <label class="checkbox">
      <input
        type="checkbox"
        bind:checked={includeCredentials}
        onchange={loadPreview}
        data-testid="share-include-credentials"
      />Include credentials
    </label>
    {#if includeCredentials}
      <div class="hint warn"><Icon name="alert" />Anyone with the link can see these credentials.</div>
    {/if}
    <label class="field">
      <span>Attach environment <span class="faint">(non-secret values only)</span></span>
      <span class="select-wrap">
      <select class="input" bind:value={environmentId} onchange={loadPreview} data-testid="share-env">
        <option value="">None</option>
        {#each [app.globalEnv, ...app.projectEnvs].filter((e) => !!e) as e (e!.id)}<option value={e!.id}>{e!.name}</option>{/each}
      </select><Icon name="chev-d" small /></span>
    </label>
    {#if preview}
      <p class="faint small">Snapshot: {preview.requestCount} request{preview.requestCount === 1 ? "" : "s"}{preview.folderCount ? `, ${preview.folderCount} folder${preview.folderCount === 1 ? "" : "s"}` : ""}. The shared page is read-only.</p>
    {/if}
    {#if existing}
      <div class="result">
        <input class="input mono" readonly value={existing.url} aria-label="Share link" data-testid="share-url" />
        <button
          class="btn btn-secondary"
          type="button"
          onclick={async () => {
            if (existing) await copyText(existing.url);
            toasts.success("Link copied");
          }}
          data-testid="share-copy">Copy</button
        >
      </div>
    {/if}
    {#if error}<div class="hint danger" role="alert"><Icon name="alert" />{error}</div>{/if}
  {/if}
</Dialog>

{#snippet shareFooter()}
    {#if existing}
      <button class="btn btn-ghost btn-danger" type="button" onclick={revoke} data-testid="share-revoke">Revoke</button>
      <span class="grow"></span>
    {/if}
    <button class="btn btn-secondary" type="button" onclick={close}>Close</button>
    <button class="btn btn-primary" type="button" disabled={busy || !preview} onclick={create} data-testid="share-create">{existing ? "Update share" : "Create link"}</button>
{/snippet}

<style>
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  h4 {
    margin: 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
    font-weight: var(--fw-semibold);
  }
  .irow {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    color: var(--text-1);
  }
  .result {
    display: flex;
    gap: var(--sp-4);
  }
  .small {
    font-size: var(--fs-sm);
    margin: 0;
  }
</style>

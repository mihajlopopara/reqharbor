<script lang="ts">
  import { onDestroy } from "svelte";
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, listen, toCommandError, type TransferPreview } from "../api";
  import { plural, projectPermissions, writableTeam } from "../util/teams";

  /**
   * Move to team / move out (UX §9, AC-PROJ-4/5, D8), 620 px: the pre-flight from the share
   * scanner, then an online move with new ids. On failure nothing moved; Retry resumes with the
   * same ids and Cancel move undoes what already landed.
   */
  let { projectId, spaceId: preset = null }: { projectId: string; spaceId?: string | null } = $props();
  const close = () => (ui.moveToTeam = null);
  const project = $derived(app.projects.find((p) => p.id === projectId) ?? null);
  const loc = $derived(app.locationById.get(projectId));
  const fromTeam = $derived(loc?.kind === "team");
  const personal = $derived(app.sync.spaces.find((s) => s.kind === "personal" && s.state === "active") ?? null);
  /** Targets: teams I can write to (not the current one); out of a team (admins): Personal or this device. */
  const targets = $derived.by(() => {
    const teams = app.sync.spaces.filter((s) => s.kind === "team" && s.state === "active" && writableTeam(s.role) && s.spaceId !== loc?.spaceId).map((s) => ({ id: s.spaceId, name: s.name }));
    if (!fromTeam) return teams;
    return [...(personal ? [{ id: personal.spaceId, name: "Personal" }] : []), { id: "local", name: "This device only" }, ...teams];
  });
  // svelte-ignore state_referenced_locally
  let target = $state<string>(preset ?? "");
  $effect(() => {
    if (!target && targets.length) target = targets[0].id;
  });
  let preview = $state<TransferPreview | null>(null);
  let previewError = $state<string | null>(null);
  let busy = $state(false);
  let failed = $state<string | null>(null);
  let progress = $state<{ done: number; total: number } | null>(null);

  $effect(() => {
    const t = target;
    if (!t) return;
    preview = null;
    previewError = null;
    call("move_preview", { projectId, spaceId: t === "local" ? null : t }).then(
      (p) => (preview = p),
      (e) => (previewError = toCommandError(e).message),
    );
  });

  let unlisten: (() => void) | null = null;
  void listen("sync://move-progress", (p) => {
    if (p.projectId === projectId) progress = { done: p.done, total: p.total };
  }).then((u) => (unlisten = u));
  onDestroy(() => unlisten?.());

  const targetName = $derived(targets.find((t) => t.id === target)?.name ?? preview?.targetName ?? "");
  const outLock = $derived(fromTeam ? projectPermissions(loc).moveOut : null);
  const members = $derived(app.sync.spaces.find((s) => s.spaceId === loc?.spaceId)?.memberCount ?? null);

  async function move() {
    if (busy || !target || !preview || outLock) return;
    busy = true;
    failed = null;
    progress = null;
    // The project and the target list change once the move lands (new ids): keep the names.
    const what = project?.name ?? "";
    const where = targetName;
    try {
      const res = await app.moveProjectTo(projectId, target === "local" ? null : target);
      close();
      toasts.success(`Moved “${what}” to ${where} · ${plural(res.items, "item")}`);
    } catch (e) {
      const err = toCommandError(e);
      failed = /Nothing was moved/.test(err.message) ? err.message : `Nothing was moved. ${err.message}`;
    } finally {
      busy = false;
    }
  }

  /** After a failure, Cancel undoes what already reached the target (UX §9). */
  async function cancelMove() {
    if (await app.cancelMove(projectId)) {
      failed = null;
      close();
      toasts.info("Move cancelled. Nothing was moved.");
    }
  }

  function openCredential(id: string, tab: string) {
    close();
    if (tab === "collection") app.openCollection(id);
    else app.openRequest(id, { reveal: true });
  }
  const title = $derived(fromTeam ? `Move “${project?.name ?? ""}” out of ${loc?.label ?? "the team"}` : `Move “${project?.name ?? ""}” to a team`);
</script>

<Dialog {title} width={620} testid="move-team-dialog" onclose={close} onsubmit={move}>
  <label class="field target">
    <span>{fromTeam ? "Move to" : "Team"}</span>
    <select class="input" bind:value={target} disabled={busy} data-testid="move-team-target">
      {#each targets as t (t.id)}<option value={t.id}>{t.name}</option>{/each}
    </select>
  </label>
  {#if outLock}
    <div class="hint warn"><Icon name="alert" />{outLock}</div>
  {:else if previewError}
    <div class="hint danger" role="alert"><Icon name="alert" />{previewError}</div>
  {:else if !preview}
    <p class="faint">Checking…</p>
  {:else}
    {#if preview.secretVariables || preview.filePaths}
      <h4>Never uploaded</h4>
      {#if preview.secretVariables}<div class="irow" data-testid="move-secrets"><Icon name="lock" small />{plural(preview.secretVariables, "secret variable")}. Each member sets their own value.</div>{/if}
      {#if preview.filePaths}<div class="irow" data-testid="move-files"><Icon name="file" small />{plural(preview.filePaths, "file path")} stay on this device.</div>{/if}
    {/if}
    {#if preview.targetKind === "team"}
      <h4>Visible to everyone in {preview.targetName}{preview.memberCount ? ` (${plural(preview.memberCount, "member")})` : ""}</h4>
      {#if preview.credentials.length}
        <div class="irow warn" data-testid="move-credentials"><Icon name="alert" small />{plural(preview.credentials.length, "credential")} typed as plain text:</div>
        <ul class="creds">
          {#each preview.credentials as c (c.requestId + c.field)}
            <li><button class="link" type="button" onclick={() => openCredential(c.requestId, c.tab)} data-testid="move-credential-row">{c.requestName} · {c.field}</button></li>
          {/each}
        </ul>
        <p class="muted small">Teammates will see these values. Replace them with secret variables before moving if they're personal.</p>
      {:else}
        <div class="irow ok" data-testid="move-clean"><Icon name="check" small />No plain-text credentials found</div>
      {/if}
    {/if}
    {#if preview.sharesStay && !fromTeam}<p class="faint small">Existing share links stay with your personal space and keep working.</p>{/if}
    {#if fromTeam}
      <div class="hint warn small"><Icon name="alert" />Removes it from {loc?.label} for all {members ? plural(members, "member") : "members"}.</div>
    {:else}
      <p class="faint small">Your other devices will see this project move to {targetName}.</p>
    {/if}
  {/if}
  {#if busy}
    <p class="faint small" aria-live="polite" data-testid="move-progress">Moving…{progress ? ` ${progress.done} of ${plural(progress.total, "item")}` : ""}</p>
  {/if}
  {#if failed}
    <div class="hint danger" role="alert" data-testid="move-error">
      <Icon name="alert" /><span>{failed} Retry finishes it; Cancel undoes what already reached {targetName}. Closing keeps it under Settings › Sync.</span>
    </div>
  {/if}
  {#snippet footer()}
    {#if failed && !app.sync.pendingMoves?.find((m) => m.projectId === projectId)?.copySaved}
      <button class="btn btn-secondary" type="button" disabled={busy} onclick={cancelMove} data-testid="move-cancel">Cancel move</button>
    {:else}
      <button class="btn btn-secondary" type="button" disabled={busy && !!progress && progress.done > 0} onclick={close}>Cancel</button>
    {/if}
    <button class="btn {fromTeam ? 'btn-danger-solid' : 'btn-primary'}" type="button" disabled={busy || !preview || !!outLock} onclick={move} data-testid="move-team-confirm">{failed ? "Retry" : fromTeam ? "Move" : `Move to ${targetName}`}</button>
  {/snippet}
</Dialog>

<style>
  .target {
    margin-bottom: var(--sp-4);
  }
  h4 {
    margin: var(--sp-5) 0 var(--sp-3);
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    font-weight: var(--fw-medium);
  }
  .irow {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 24px;
  }
  .irow.warn {
    color: var(--warning);
  }
  .irow.ok {
    color: var(--success, var(--text-2));
  }
  .creds {
    margin: var(--sp-2) 0;
    padding-left: calc(16px + var(--sp-5));
    display: flex;
    flex-direction: column;
    gap: var(--sp-1, 2px);
  }
  p {
    margin: var(--sp-3) 0 0;
  }
  .small {
    font-size: var(--fs-sm);
  }
  .hint {
    margin-top: var(--sp-4);
  }
</style>

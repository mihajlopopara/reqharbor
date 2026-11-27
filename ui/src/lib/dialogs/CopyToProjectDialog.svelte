<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import Icon from "../components/Icon.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type Collection, type NodeKind, type TransferPreview } from "../api";
  import { plural, writableTeam } from "../util/teams";

  /**
   * Copy to project… / Duplicate to personal… (UX §6.1): one dialog, two labels. The copy gets new
   * ids and syncs with its target; the source is never touched. A team target first shows the
   * pre-flight (UX §9 copy mode) with the variables its environments don't define.
   */
  let {
    kind,
    id,
    name,
    mode,
    targetProjectId = undefined,
  }: { kind: NodeKind; id: string; name: string; mode: "copy" | "duplicate"; targetProjectId?: string } = $props();
  const close = () => (ui.copyToProject = null);

  /** Projects I can write to; Duplicate to personal: Personal and This device only. */
  const targets = $derived(
    app.projects.filter((p) => {
      const l = app.locationById.get(p.id);
      if (mode === "duplicate") return !l || l.kind === "personal" || l.kind === "local";
      if (!l || l.kind !== "team") return true;
      return writableTeam(l.role) && l.state === "active";
    }),
  );
  // svelte-ignore state_referenced_locally
  let target = $state<string>(targetProjectId ?? "");
  $effect(() => {
    if (!target || !targets.some((t) => t.id === target)) target = targets.find((t) => t.id !== app.activeProjectId)?.id ?? targets[0]?.id ?? "";
  });
  const tloc = $derived(app.locationById.get(target));
  const toTeam = $derived(tloc?.kind === "team");
  let collections = $state<Collection[]>([]);
  let collection = $state<string>("");
  $effect(() => {
    const t = target;
    if (!t || kind === "collection") return;
    call("get_project_tree", { projectId: t }).then(
      (tree) => {
        collections = tree.collections.filter((c) => !c.minClientVersion);
        collection = collections[0]?.id ?? "";
      },
      () => (collections = []),
    );
  });
  let review = $state<TransferPreview | null>(null);
  let busy = $state(false);
  let error = $state<string | null>(null);

  async function primary() {
    if (busy || !target) return;
    error = null;
    if (toTeam && !review) {
      try {
        review = await call("copy_preview", { kind, id, targetProjectId: target });
      } catch (e) {
        error = toCommandError(e).message;
      }
      return;
    }
    busy = true;
    const copyId = await app.copyNodeTo(kind, id, target, kind === "collection" ? null : collection || null);
    busy = false;
    if (!copyId) return;
    close();
    const tname = app.projects.find((p) => p.id === target)?.name ?? "the project";
    const dest = target;
    toasts.success(`Copied to ${tname}`, {
      actionLabel: "Open",
      action: async () => {
        await app.switchProject(dest);
        if (kind === "request") app.openRequest(copyId, { reveal: true });
        else if (kind === "collection") app.openCollection(copyId);
      },
    });
  }
  $effect(() => {
    void target;
    review = null;
  });
  const label = $derived(mode === "duplicate" ? "Duplicate" : toTeam ? (review ? `Copy to ${tloc?.label}` : "Review and copy…") : "Copy");
  const title = $derived(review ? `Copy “${name}” to “${review.targetName}” (${tloc?.label})` : mode === "duplicate" ? `Duplicate “${name}” to personal` : `Copy “${name}” to project`);
</script>

<Dialog {title} width={review ? 620 : 460} testid="copy-dialog" onclose={close} onsubmit={primary}>
  {#if !review}
    <label class="field">
      <span>Project</span>
      <select class="input" bind:value={target} data-testid="copy-target">
        {#each targets as p (p.id)}
          {@const l = app.locationById.get(p.id)}
          <option value={p.id}>{p.name}{l && l.kind !== "local" ? ` · ${l.label}` : ""}</option>
        {/each}
      </select>
    </label>
    {#if !targets.length}<p class="faint small">No project you can copy to. Create one in Personal first.</p>{/if}
    {#if kind !== "collection"}
      <label class="field">
        <span>Collection</span>
        <select class="input" bind:value={collection} data-testid="copy-collection">
          {#each collections as c (c.id)}<option value={c.id}>{c.name}</option>{/each}
          <option value="">New collection</option>
        </select>
      </label>
    {/if}
  {:else}
    {#if review.filePaths}
      <h4>Never uploaded</h4>
      <div class="irow"><Icon name="file" small />{plural(review.filePaths, "file path")} stay on this device.</div>
    {/if}
    <h4>Visible to everyone in {tloc?.label}{review.memberCount ? ` (${plural(review.memberCount, "member")})` : ""}</h4>
    {#if review.credentials.length}
      <div class="irow warn" data-testid="copy-credentials"><Icon name="alert" small />{plural(review.credentials.length, "credential")} typed as plain text:</div>
      <ul class="creds">{#each review.credentials as c (c.requestId + c.field)}<li>{c.requestName} · {c.field}</li>{/each}</ul>
    {:else}
      <div class="irow"><Icon name="check" small />No plain-text credentials found</div>
    {/if}
    {#if review.unresolvedVariables.length}
      <h4>Unresolved variables</h4>
      <p class="small" data-testid="copy-unresolved">{review.unresolvedVariables.map((v) => `{{${v}}}`).join(", ")} {review.unresolvedVariables.length === 1 ? "isn't" : "aren't"} defined in {review.targetName}'s environments. Environments aren't copied.</p>
    {/if}
  {/if}
  {#if error}<div class="hint danger" role="alert"><Icon name="alert" />{error}</div>{/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={() => (review ? (review = null) : close())}>{review ? "Back" : "Cancel"}</button>
    <button class="btn btn-primary" type="button" disabled={busy || !target} onclick={primary} data-testid="copy-confirm">{label}</button>
  {/snippet}
</Dialog>

<style>
  .field + .field {
    margin-top: var(--sp-4);
  }
  h4 {
    margin: var(--sp-5) 0 var(--sp-3);
    font-size: 10.5px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    font-weight: var(--fw-medium);
  }
  h4:first-child {
    margin-top: 0;
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
  .creds {
    margin: var(--sp-2) 0;
    padding-left: calc(16px + var(--sp-5));
  }
  .small {
    font-size: var(--fs-sm);
  }
  p {
    margin: 0;
  }
  .hint {
    margin-top: var(--sp-4);
  }
</style>

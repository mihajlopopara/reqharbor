<script lang="ts">
  import Dialog from "../components/Dialog.svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { plural } from "../util/spaces";
  import { projectPermissions, writableTeam } from "../util/teams";

  /**
   * Sync location for one project (UX §5.3). Personal ⇄ This device only switch in place; any move
   * into or out of a team goes through the pre-flight (MoveToTeamDialog) with new ids (04 §6.5).
   */
  let { projectId }: { projectId: string } = $props();
  const close = () => (ui.syncLocation = null);
  const project = $derived(app.projects.find((p) => p.id === projectId) ?? null);
  const loc = $derived(app.locationById.get(projectId) ?? null);
  const current = $derived(loc?.spaceId ?? "local");
  const personal = $derived(app.sync.spaces.find((s) => s.kind === "personal" && s.state === "active") ?? null);
  const teams = $derived(app.sync.spaces.filter((s) => s.kind === "team"));
  const inTeam = $derived(loc?.kind === "team");
  /** An admin may move a team project out; an editor copies instead (AC-PROJ-5). */
  const outLock = $derived(inTeam ? projectPermissions(loc ?? undefined).moveOut : null);
  const teamCard = (t: { role: import("../api").SpaceRole | null; state: string; spaceId: string }) => app.teamsOn && t.state === "active" && writableTeam(t.role) && t.spaceId !== loc?.spaceId;
  /** B6: a token-synced project reaches Personal only through the merge (token sync stops with it). */
  const inLegacy = $derived(loc?.kind === "legacy");
  // svelte-ignore state_referenced_locally
  let choice = $state<string>(app.locationById.get(projectId)?.spaceId ?? "local");
  let busy = $state(false);

  const toLocal = $derived(choice === "local" && current !== "local");
  const toPersonal = $derived(!!personal && choice === personal.spaceId && current !== choice);
  const toTeam = $derived(teams.some((t) => t.spaceId === choice) && choice !== current);
  /** Team → Personal / This device only (admins): a move with new ids, through the pre-flight. */
  const moveOut = $derived(inTeam && choice !== current);
  const primary = $derived(toTeam ? "Review and move…" : moveOut ? "Move" : toLocal ? "Stop syncing" : toPersonal ? "Start syncing" : "Done");

  async function apply() {
    if (busy) return;
    if (choice === current) return close();
    if (toTeam || moveOut) {
      const target = choice;
      close();
      ui.moveToTeam = { projectId, spaceId: target };
      return;
    }
    const name = project?.name ?? "The project";
    const target = choice;
    busy = true;
    const ok = await app.setProjectLocation(projectId, target === "local" ? null : target);
    busy = false;
    if (!ok) return;
    close();
    toasts.success(target === "local" ? `“${name}” now stays on this device only` : `“${name}” now syncs to Personal`);
  }
</script>

<Dialog title="Sync location for “{project?.name ?? ''}”" width={460} testid="sync-location-dialog" onclose={close} onsubmit={apply}>
  <div class="cards" role="radiogroup" aria-label="Sync location">
    {#if personal}
      <label class="card" class:on={choice === personal.spaceId} class:off={!!outLock || inLegacy}>
        <input type="radio" name="sync-location" value={personal.spaceId} bind:group={choice} disabled={!!outLock || inLegacy} data-testid="sync-location-personal" />
        <span class="t"><b>Personal</b><span class="faint small">{inLegacy ? "Token-synced projects move with Merge into my personal space…" : "Synced to your devices"}</span></span>
      </label>
    {/if}
    {#each teams as t (t.spaceId)}
      {@const ok = teamCard(t) || t.spaceId === loc?.spaceId}
      <label class="card" class:off={!ok || inLegacy} class:on={choice === t.spaceId}>
        <input type="radio" name="sync-location" value={t.spaceId} bind:group={choice} disabled={!ok || inLegacy} data-testid="sync-location-team-{t.spaceId}" />
        <span class="t"
          ><b>{t.name}</b><span class="faint small"
            >{!app.teamsOn ? "Teams aren't turned on for this server" : !writableTeam(t.role) ? "Viewers can't add projects" : `Everyone in ${t.name}${t.memberCount ? ` (${plural(t.memberCount, "member")})` : ""} can see it`}</span
          ></span
        >
      </label>
    {/each}
    <label class="card" class:on={choice === "local"} class:off={!!outLock}>
      <input type="radio" name="sync-location" value="local" bind:group={choice} disabled={!!outLock} data-testid="sync-location-local" />
      <span class="t"><b>This device only</b><span class="faint small">Never leaves this device</span></span>
    </label>
  </div>
  {#if outLock}
    <div class="hint small" data-testid="sync-location-team-note">{outLock}</div>
  {:else if moveOut}
    <div class="hint warn small" data-testid="sync-location-warn">Removes it from {loc?.label} for all {plural(teams.find((t) => t.spaceId === loc?.spaceId)?.memberCount ?? 0, "member")}.</div>
  {:else if inLegacy && !toLocal}
    <div class="hint small" data-testid="sync-location-legacy-note">
      This project syncs with a token. To move it to Personal, use
      <button
        class="link"
        type="button"
        onclick={() => {
          close();
          ui.mergeLegacy = true;
        }}
        data-testid="sync-location-merge">Merge into my personal space…</button
      >
    </div>
  {:else if toLocal}
    <div class="hint warn small" data-testid="sync-location-warn">Removes it from your other devices. This copy stays.</div>
  {/if}
  {#snippet footer()}
    <button class="btn btn-secondary" type="button" onclick={close}>Cancel</button>
    <button class="btn {toLocal || moveOut ? 'btn-danger-solid' : 'btn-primary'}" type="button" disabled={busy} onclick={apply} data-testid="sync-location-apply">{primary}</button>
  {/snippet}
</Dialog>

<style>
  .cards {
    display: flex;
    flex-direction: column;
    gap: var(--sp-3);
  }
  .card {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    padding: var(--sp-4) var(--sp-5);
    border-radius: var(--r-md);
    background: var(--bg-input);
    border: 1px solid transparent;
    cursor: pointer;
  }
  .card.on {
    border-color: var(--border-focus);
  }
  .card.off {
    cursor: default;
    opacity: 0.7;
  }
  .t {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

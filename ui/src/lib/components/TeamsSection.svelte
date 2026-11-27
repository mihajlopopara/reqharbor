<script lang="ts">
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { call, toCommandError, type TeamSummary } from "../api";
  import { formatSize } from "../util/format";
  import { plural, roleName } from "../util/teams";
  import Icon from "./Icon.svelte";
  import TeamDetail from "./TeamDetail.svelte";

  /**
   * Settings › Teams (UX §7.1): my teams, Create team and Join team…; a row opens its detail
   * (UX §7.2). Shown only when the account's server advertises `features.teams`.
   */
  let { teamId = $bindable(null) }: { teamId?: string | null } = $props();

  let teams = $state<TeamSummary[] | null>(null);
  /** "You can own up to 10 teams on this server." at the server's limit (UX §7.1). */
  let createLock = $state<string | null>(null);
  let error = $state<string | null>(null);

  async function load() {
    try {
      const o = await call("teams_list", {});
      teams = o.teams;
      const max = o.limits.ownedTeams;
      createLock = max !== null && o.ownedCount !== null && o.ownedCount >= max ? `You can own up to ${max} team${max === 1 ? "" : "s"} on this server.` : null;
      error = null;
    } catch (e) {
      const err = toCommandError(e);
      teams = teams ?? [];
      error = err.code === "server_unreachable" ? "Offline · Can't load your teams right now." : err.message;
    }
  }

  $effect(() => {
    // Reload when spaces change (created, joined, left) and when coming back from a detail.
    void app.sync.spaces.length;
    if (!teamId) void load();
  });

  const me = $derived(app.auth.account?.userId ?? "");
  /** "Your storage: 61 MB of 100 MB (personal space + 2 teams you own)" — owners only. */
  const owned = $derived((teams ?? []).filter((t) => t.ownerUserId === me));
  const personal = $derived(app.sync.spaces.find((s) => s.kind === "personal") ?? null);
  const storage = $derived.by(() => {
    if (!owned.length) return null;
    const used = (personal?.bytesUsed ?? 0) + owned.reduce((n, t) => n + (t.bytesUsed ?? 0), 0);
    return `Your storage: ${formatSize(used)} (personal space + ${plural(owned.length, "team")} you own)`;
  });
  const host = $derived(app.auth.server?.info?.name || app.auth.account?.serverName || "this server");
</script>

{#if teamId}
  <TeamDetail {teamId} onback={() => (teamId = null)} />
{:else}
  <div class="head">
    <h3>Teams</h3>
    <span class="grow"></span>
    {#if teams?.length}
      <button class="btn btn-secondary btn-sm" type="button" disabled={!!createLock} title={createLock ?? undefined} onclick={() => (ui.createTeam = true)} data-testid="team-create-open">Create team</button>
      <button class="btn btn-secondary btn-sm" type="button" onclick={() => (ui.joinTeam = {})} data-testid="team-join-open">Join team…</button>
    {/if}
  </div>
  {#if error}<p class="hint warn" data-testid="teams-error"><Icon name="alert" />{error}</p>{/if}
  {#if teams === null}
    <p class="faint">Loading…</p>
  {:else if teams.length === 0}
    <div class="empty" data-testid="teams-empty">
      <h3>No teams yet</h3>
      <p>Create a team to share projects with teammates, or join one with an invite link.</p>
      <div class="row">
        <button class="btn btn-primary" type="button" disabled={!!createLock} title={createLock ?? undefined} onclick={() => (ui.createTeam = true)} data-testid="team-create-open">Create team</button>
        <button class="btn btn-secondary" type="button" onclick={() => (ui.joinTeam = {})} data-testid="team-join-open">Join team…</button>
      </div>
    </div>
  {:else}
    <div class="list" aria-label="Your teams">
      {#each teams as t (t.teamId)}
        <button class="team-row" type="button" onclick={() => (teamId = t.teamId)} data-testid="team-row" data-team={t.name}>
          <Icon name="users" />
          <span class="tname">{t.name}</span>
          <span class="role">{roleName(t.role)}</span>
          <span class="faint small">{t.memberCount !== null ? plural(t.memberCount, "member") : ""}</span>
          <span class="grow"></span>
          {#if t.bytesUsed !== null}<span class="faint small tabular">{formatSize(t.bytesUsed)}{t.quotaBytes ? ` of ${formatSize(t.quotaBytes)}` : ""}</span>{/if}
          <Icon name="chev-r" small />
        </button>
      {/each}
    </div>
    {#if storage}<p class="faint small" data-testid="teams-storage">{storage}</p>{/if}
    {#if createLock}<p class="faint small" data-testid="teams-create-lock">{createLock}</p>{/if}
  {/if}
  <p class="faint note">Team data is visible to all members and stored on {host}. Use secret variables for tokens and passwords.</p>
{/if}

<style>
  .head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
  }
  h3 {
    margin: 0;
    font-size: var(--fs-xl);
    font-weight: var(--fw-semibold);
  }
  .row {
    display: flex;
    gap: var(--sp-4);
    justify-content: center;
  }
  .list {
    display: flex;
    flex-direction: column;
    border-top: 1px solid var(--border-subtle);
  }
  .team-row {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    height: 36px;
    padding: 0 var(--sp-3);
    border: 0;
    border-bottom: 1px solid var(--border-subtle);
    background: none;
    color: var(--text-1);
    text-align: left;
  }
  .team-row:hover {
    background: var(--bg-hover);
  }
  .tname {
    font-weight: var(--fw-medium);
  }
  .role {
    color: var(--text-2);
    font-size: var(--fs-sm);
  }
  .small {
    font-size: var(--fs-sm);
  }
  .tabular {
    font-variant-numeric: tabular-nums;
  }
  .note {
    font-size: 11.5px;
    margin: 0;
  }
</style>

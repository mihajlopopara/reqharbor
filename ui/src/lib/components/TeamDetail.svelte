<script lang="ts">
  import { tick } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui } from "../stores/ui.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { call, toCommandError, type TeamDetail, type TeamMember, type TeamRole } from "../api";
  import { formatSize } from "../util/format";
  import { inviteSummary, leaveLockReason, plural, rank, removeLockReason, roleLockReason, roleName, roleOptions, type MemberContext } from "../util/teams";
  import Avatar from "./Avatar.svelte";
  import Icon from "./Icon.svelte";

  /** Team detail (UX §7.2): members and roles, invite links, the name, and the danger zone. */
  let { teamId, onback }: { teamId: string; onback: () => void } = $props();

  let detail = $state<TeamDetail | null>(null);
  let error = $state<string | null>(null);
  let name = $state("");
  let busy = $state(false);

  async function load() {
    try {
      detail = await call("team_get", { teamId });
      name = detail.team.name;
      error = null;
    } catch (e) {
      error = toCommandError(e).message;
    }
  }
  $effect(() => {
    void teamId;
    void load();
  });
  // Reload after the invite or transfer dialog closes.
  let wasOpen = false;
  $effect(() => {
    const open = !!ui.invite || !!ui.transferOwnership;
    if (wasOpen && !open) void load();
    wasOpen = open;
  });

  const team = $derived(detail?.team ?? null);
  const members = $derived(detail?.members ?? []);
  const ctx = $derived<MemberContext>({
    actor: team?.role ?? "viewer",
    me: detail?.me ?? "",
    ownerUserId: team?.ownerUserId ?? null,
    owners: members.filter((m) => m.role === "owner").length,
  });
  const admin = $derived(rank(ctx.actor) >= 3);
  const billingOwner = $derived(!!team && team.ownerUserId === ctx.me);
  const pct = $derived(team?.bytesUsed != null && team.quotaBytes ? Math.min(100, (team.bytesUsed / team.quotaBytes) * 100) : null);
  const projects = $derived(team?.spaceId ? app.projects.filter((p) => app.locationById.get(p.id)?.spaceId === team.spaceId) : []);
  const leaveLock = $derived(leaveLockReason(ctx));

  async function setRole(m: TeamMember, role: string, undo = true) {
    if (!team || role === m.role) return;
    const before = m.role;
    try {
      await call("team_set_role", { teamId, userId: m.userId, role: role as TeamRole });
      await load();
      const who = m.userId === ctx.me ? "You are" : `${m.name.split(" ")[0] || m.email} is`;
      if (undo) toasts.success(`${who} now ${roleName(role)}`, { actionLabel: "Undo", action: () => void setRole({ ...m, role }, before, false) });
    } catch (e) {
      app.reportError(e);
      await load();
    }
  }

  function memberMenu(m: TeamMember, anchor: HTMLElement) {
    const self = m.userId === ctx.me;
    const removeLock = removeLockReason(m, ctx);
    ui.openMenu(
      self
        ? [{ label: "Leave team…", danger: true, disabled: !!leaveLock, reason: leaveLock ?? undefined, action: () => void leave(), testid: "member-leave" }]
        : [
            ...(billingOwner ? [{ label: "Transfer ownership…", action: () => (ui.transferOwnership = { teamId }), testid: "member-transfer" }] : []),
            { label: "Remove from team…", danger: true, disabled: !!removeLock, reason: removeLock ?? undefined, action: () => void remove(m), testid: "member-remove" },
          ],
      anchor,
      { alignRight: true },
    );
  }

  async function remove(m: TeamMember) {
    if (!team) return;
    const active = detail?.invites ?? [];
    const multi = active.some((i) => i.maxUses === null || i.maxUses > 1);
    const first = m.name.split(" ")[0] || m.email;
    const r = await ui.confirmEx({
      title: `Remove ${m.name || m.email} from ${team.name}?`,
      body: `${first} loses access now. Copies already on their devices can't be wiped. They can rejoin only with an invite link created after today.`,
      confirmLabel: "Remove",
      checkbox: active.length ? { label: `Also revoke all active invite links (${active.length})`, checked: multi, testid: "remove-revoke-invites" } : undefined,
    });
    if (!r.ok) return;
    try {
      await call("team_remove_member", { teamId, userId: m.userId, revokeInvites: !!r.checked });
      const idx = members.findIndex((x) => x.userId === m.userId);
      await load();
      toasts.success(`Removed ${m.name || m.email} from ${team.name}`);
      // Focus the next row's ⋯, or Invite… when the list is empty (UX §7.2).
      await tick();
      const next = members[Math.min(idx, members.length - 1)];
      document.querySelector<HTMLElement>(next ? `[data-testid="member-more-${next.userId}"]` : "[data-testid=team-invite-open]")?.focus();
    } catch (e) {
      app.reportError(e);
    }
  }

  async function leave() {
    if (!team) return;
    const n = projects.length;
    const r = await ui.confirmEx({
      title: `Leave ${team.name}?`,
      body: `Leave ${team.name}? Its projects stop syncing here.`,
      confirmLabel: "Leave team",
      checkbox: n ? { label: `Keep a personal copy of ${plural(n, "project")}`, checked: true, testid: "leave-keep-copy" } : undefined,
    });
    if (!r.ok) return;
    try {
      app.onSyncStatus(await call("team_leave", { teamId, keepCopy: r.checked ?? true }));
      await app.reloadAll();
      toasts.info(`You left ${team.name}`);
      onback();
    } catch (e) {
      app.reportError(e);
    }
  }

  async function del() {
    if (!team) return;
    const invites = detail?.invites?.length ?? 0;
    const r = await ui.confirmEx({
      title: `Delete ${team.name}?`,
      body: `This deletes ${team.name} for everyone. Members keep the copies already on their devices.`,
      confirmLabel: "Delete team",
      typeToConfirm: team.name,
      details: [`${plural(projects.length, "project")} and everything in them`, `${plural(members.length, "member")} lose access now`, `Its share links and ${plural(invites, "invite link")} stop working`],
    });
    if (!r.ok) return;
    try {
      app.onSyncStatus(await call("team_delete", { teamId }));
      await app.reloadAll();
      toasts.info(`Deleted ${team.name}`);
      onback();
    } catch (e) {
      app.reportError(e);
    }
  }

  async function rename() {
    if (!team || !name.trim() || name.trim() === team.name) return;
    busy = true;
    try {
      await call("team_rename", { teamId, name: name.trim() });
      await load();
      toasts.success("Team renamed");
    } catch (e) {
      app.reportError(e);
    } finally {
      busy = false;
    }
  }

  async function revoke(inviteId: string) {
    try {
      await call("invite_revoke", { teamId, inviteId });
      await load();
      toasts.success("Invite link revoked");
    } catch (e) {
      app.reportError(e);
    }
  }

  const joined = (at: number | null) => (at ? new Date(at).toLocaleDateString(undefined, { month: "short", day: "numeric" }) : "");
</script>

<button class="link back" type="button" onclick={onback} onkeydown={(e) => e.altKey && e.key === "ArrowLeft" && onback()} data-testid="team-back">‹ Teams</button>
{#if error && !detail}
  <p class="hint warn"><Icon name="alert" />{error}</p>
{:else if !team}
  <p class="faint">Loading…</p>
{:else}
  <div class="title">
    <h3 data-testid="team-name">{team.name}</h3>
    <span class="role" data-testid="team-my-role">{roleName(team.role)}</span>
  </div>
  <div class="meta">
    <span class="faint small">{plural(members.length, "member")}{team.bytesUsed !== null ? ` · ${formatSize(team.bytesUsed)}${team.quotaBytes ? ` of ${formatSize(team.quotaBytes)}` : ""}` : ""}</span>
    {#if pct !== null}<span class="meter"><span class="fill" class:high={pct > 85} style:width="{pct}%"></span></span>{/if}
  </div>

  <section>
    <div class="sec-head">
      <h4>Members</h4>
      <span class="grow"></span>
      {#if admin}<button class="btn btn-secondary btn-sm" type="button" onclick={() => (ui.invite = { teamId, teamName: team.name, role: team.role })} data-testid="team-invite-open">Invite…</button>{/if}
    </div>
    <table class="members" data-testid="team-members">
      <caption class="sr-only">Members of {team.name}</caption>
      <tbody>
        {#each members as m (m.userId)}
          {@const lock = roleLockReason(m, ctx)}
          <tr data-testid="member-row" data-user={m.userId}>
            <td class="who">
              <Avatar name={m.name} email={m.email} />
              <span class="mname">{m.name || m.email}{m.userId === ctx.me ? " (you)" : ""}</span>
              <span class="faint small email">{m.email}</span>
            </td>
            <td class="rolecell">
              {#if lock === null && (admin || m.userId === ctx.me)}
                <span class="select-wrap">
                  <select class="input" value={m.role} aria-label="Role for {m.name || m.email}" onchange={(e) => void setRole(m, e.currentTarget.value)} data-testid="member-role-{m.userId}">
                    {#each roleOptions(m, ctx) as o (o.value)}
                      <option value={o.value} disabled={o.disabled && o.value !== m.role} title={o.reason ?? undefined}>{roleName(o.value)}</option>
                    {/each}
                  </select>
                </span>
              {:else}
                <span class="plain" title={admin ? (lock ?? undefined) : undefined} data-testid="member-role-{m.userId}">{roleName(m.role)}</span>
              {/if}
            </td>
            <td class="faint small joined">{m.joinedAt ? `Joined ${joined(m.joinedAt)}` : ""}</td>
            <td class="acts">
              {#if admin || m.userId === ctx.me}
                <button class="icon-btn" type="button" aria-label="More actions for {m.name || m.email}" aria-haspopup="menu" onclick={(e) => memberMenu(m, e.currentTarget)} data-testid="member-more-{m.userId}"><Icon name="more" /></button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>

  {#if admin && detail?.invites}
    <section>
      <h4>Invite links</h4>
      {#if detail.invites.length === 0}
        <p class="faint small" data-testid="invites-empty">No active invite links.</p>
      {:else}
        {#each detail.invites as i (i.inviteId)}
          <div class="invite" data-testid="invite-row">
            <span>{inviteSummary(i, app.now)}</span>
            <span class="grow"></span>
            <button class="link danger" type="button" onclick={() => void revoke(i.inviteId)} data-testid="invite-revoke">Revoke</button>
          </div>
        {/each}
      {/if}
    </section>
  {/if}

  <section>
    <h4>Team</h4>
    {#if admin}
      <form class="name-row" onsubmit={(e) => { e.preventDefault(); void rename(); }}>
        <label class="sr-only" for="team-name-input">Team name</label>
        <input id="team-name-input" class="input" bind:value={name} maxlength="80" data-testid="team-rename-input" />
        <button class="btn btn-secondary btn-sm" type="submit" disabled={busy || !name.trim() || name.trim() === team.name} data-testid="team-rename-save">Save</button>
      </form>
    {:else}
      <p class="small">Name: {team.name}</p>
    {/if}
  </section>

  <section>
    <h4>Danger zone</h4>
    {#if leaveLock}<p class="hint warn small" data-testid="team-leave-lock"><Icon name="alert" />{leaveLock}</p>{/if}
    <div class="danger-row">
      <button class="btn btn-ghost btn-danger" type="button" disabled={!!leaveLock} title={leaveLock ?? undefined} onclick={() => void leave()} data-testid="team-leave">Leave team…</button>
      {#if billingOwner}<button class="btn btn-ghost btn-danger" type="button" onclick={() => void del()} data-testid="team-delete">Delete team…</button>{/if}
      {#if billingOwner && members.length > 1}<button class="link" type="button" onclick={() => (ui.transferOwnership = { teamId })} data-testid="team-transfer-open">Transfer ownership…</button>{/if}
    </div>
  </section>
{/if}

<style>
  .back {
    align-self: flex-start;
  }
  .title {
    display: flex;
    align-items: baseline;
    gap: var(--sp-4);
  }
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
    font-weight: var(--fw-medium);
  }
  .role {
    color: var(--text-2);
  }
  .meta {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
  }
  .meter {
    display: inline-block;
    width: 120px;
    height: 6px;
    border-radius: var(--r-pill);
    background: var(--bg-input);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: var(--accent);
  }
  .fill.high {
    background: var(--warning);
  }
  section {
    display: flex;
    flex-direction: column;
  }
  .sec-head {
    display: flex;
    align-items: center;
  }
  .members {
    width: 100%;
    border-collapse: collapse;
  }
  .members td {
    padding: var(--sp-2) var(--sp-3);
    border-top: 1px solid var(--border-subtle);
    vertical-align: middle;
  }
  .who {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    flex-wrap: wrap;
  }
  .mname {
    font-weight: var(--fw-medium);
  }
  .rolecell .select-wrap {
    width: 110px;
    display: inline-block;
  }
  .plain {
    color: var(--text-2);
  }
  .joined {
    white-space: nowrap;
  }
  .acts {
    text-align: right;
    width: 32px;
  }
  .invite {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-height: 28px;
    font-size: var(--fs-sm);
  }
  .link.danger {
    color: var(--danger);
  }
  .name-row {
    display: flex;
    gap: var(--sp-3);
    max-width: 360px;
  }
  .danger-row {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
  }
  .small {
    font-size: var(--fs-sm);
  }
  @container (max-width: 520px) {
    .email {
      flex-basis: 100%;
      padding-left: 28px;
    }
  }
</style>

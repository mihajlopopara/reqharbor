<script lang="ts">
  import { tick } from "svelte";
  import { app } from "../stores/app.svelte";
  import { ui, type MenuItem } from "../stores/ui.svelte";
  import { flattenTree, childFolders, childRequests, isInside, type TreeRow } from "../util/tree";
  import { methodClass, methodLabel, skippedReasonText } from "../util/format";
  import { shortcutText } from "../util/shortcuts";
  import { newerKindLabel, REQUIRES_NEWER } from "../util/request";
  import VirtualList from "./VirtualList.svelte";
  import Hl from "./Hl.svelte";
  import Icon from "./Icon.svelte";
  import { ws } from "../stores/ws.svelte";
  import { toasts } from "../stores/toasts.svelte";
  import { isConflictCopy, projectPermissions } from "../util/teams";

  const ROW = 26;
  const filter = $derived(ui.filters.collections);
  const rows = $derived(app.index ? flattenTree(app.index, ui.expanded, filter) : []);
  let focusIdx = $state(0);
  let scrollTo = $state(-1);
  let list = $state<VirtualList<TreeRow>>();
  let renameValue = $state("");
  let renameInvalid = $state(false);

  // reset rename buffer whenever a new rename starts
  $effect(() => {
    const r = ui.renaming;
    if (!r || r.kind === "environment" || r.kind === "project") return;
    const row = rows.find((x) => x.id === r.id);
    if (row && newer(row)) {
      ui.renaming = null; // F2 on a view-only request (AC-FWD-3)
      return;
    }
    if (row) {
      renameValue = row.name;
      renameInvalid = false;
      const i = rows.indexOf(row);
      scrollTo = i;
    }
  });

  /** DR-006: exactly one filled row — the item open in the middle pane. */
  function selected(row: TreeRow): boolean {
    if (app.middle.kind === "collection") return row.kind === "collection" && row.id === app.middle.id;
    if (app.middle.kind === "request") return row.kind === "request" && row.id === app.request.id;
    return false;
  }

  /** Context / keyboard target that isn't the open item: outlined, never filled. */
  function isTarget(row: TreeRow): boolean {
    return ui.selectedNode?.id === row.id && !selected(row);
  }

  function activate(row: TreeRow) {
    if (row.kind === "request") app.openRequest(row.id);
    else if (row.kind === "folder") {
      ui.selectedNode = { kind: "folder", id: row.id, collectionId: row.collectionId };
      ui.toggleExpanded(row.id);
    } else {
      app.openCollection(row.id);
      if (!row.expanded) ui.toggleExpanded(row.id, true);
    }
  }

  async function focusRow(i: number) {
    focusIdx = Math.max(0, Math.min(i, rows.length - 1));
    scrollTo = focusIdx;
    await tick();
    document.querySelector<HTMLElement>(`[data-testid="tree-node"][data-index="${focusIdx}"]`)?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (ui.renaming || !rows.length) return;
    const row = rows[focusIdx];
    if (e.key === "ArrowDown") void focusRow(focusIdx + 1);
    else if (e.key === "ArrowUp") void focusRow(focusIdx - 1);
    else if (e.key === "Home") void focusRow(0);
    else if (e.key === "End") void focusRow(rows.length - 1);
    else if (e.key === "ArrowRight") {
      if (row.hasChildren && !row.expanded) ui.toggleExpanded(row.id, true);
      else void focusRow(focusIdx + 1);
    } else if (e.key === "ArrowLeft") {
      if (row.expanded && row.kind !== "request") ui.toggleExpanded(row.id, false);
      else {
        const parentId = row.kind === "collection" ? null : (row.parentFolderId ?? row.collectionId);
        const p = rows.findIndex((r) => r.id === parentId);
        if (p >= 0) void focusRow(p);
      }
    } else if (e.key === "Enter" || e.key === " ") activate(row);
    else if (e.key === "Delete" || (e.key === "Backspace" && (e.metaKey || e.ctrlKey))) void app.deleteNode(row.kind, row.id);
    else if ((e.key === "F10" && e.shiftKey) || e.key === "ContextMenu") {
      const el = document.querySelector<HTMLElement>(`[data-testid="tree-node"][data-index="${focusIdx}"]`);
      if (el) openMenu(row, el);
    } else return;
    e.preventDefault();
  }

  function select(row: TreeRow) {
    ui.selectedNode = { kind: row.kind, id: row.id, collectionId: row.collectionId };
  }

  /** AC-FWD-3: a request from a newer ReqHarbor (view only). */
  function newer(row: TreeRow): boolean {
    if (row.kind === "request") return app.isReadOnly(row.id);
    return app.isContainerReadOnly(row.kind, row.id);
  }

  /** AC-ROLE-1 / UX §6.2: a viewer's team project is read-only here (copy out to change it). */
  const viewerReason = $derived(projectPermissions(app.activeProjectId ? app.locationById.get(app.activeProjectId) : undefined).edit);
  const VIEWER_TIP = "Viewers can't change team projects. Duplicate to personal to experiment.";

  function copyItems(row: TreeRow): MenuItem[] {
    if (app.projects.length < 2 && !viewerReason) return [];
    const target = { kind: row.kind, id: row.id, name: row.name };
    return viewerReason
      ? [{ label: "Duplicate to personal…", action: () => (ui.copyToProject = { ...target, mode: "duplicate" }), testid: "tree-duplicate-personal" }]
      : [{ label: "Copy to project…", action: () => (ui.copyToProject = { ...target, mode: "copy" }), testid: "tree-copy-to-project" }];
  }

  /** A viewer's menu: only Open, Settings, Copy as cURL and Duplicate to personal… are enabled. */
  function viewerMenu(row: TreeRow): MenuItem[] {
    const no = (label: string, extra: Partial<MenuItem> = {}): MenuItem => ({ label, disabled: true, reason: viewerReason ?? undefined, ...extra });
    const req = app.index?.requestById.get(row.id);
    return [
      ...(row.kind === "request" ? [{ label: "Open", action: () => app.openRequest(row.id) }] : []),
      ...(row.kind === "collection" ? [{ label: "Settings…", action: () => app.openCollection(row.id) }] : []),
      ...(row.kind === "request" && req ? [{ label: "Copy as cURL", action: () => void app.copyRequestCurl(row.id === app.request.id ? ($state.snapshot(app.request) as typeof req) : req) }] : []),
      ...(row.kind !== "folder" ? [no("Share…", { reason: "Viewers can't create share links", testid: "tree-share" })] : []),
      { separator: true },
      ...copyItems(row),
      no("Rename", { testid: "tree-rename" }),
      no("Duplicate", { testid: "tree-duplicate" }),
      { separator: true },
      no("Delete", { danger: true, testid: "tree-delete" }),
    ];
  }

  function startRename(row: TreeRow) {
    if (newer(row)) return;
    if (viewerReason) {
      toasts.info(VIEWER_TIP);
      return;
    }
    select(row);
    ui.renaming = { kind: row.kind, id: row.id };
  }

  function commitRename(row: TreeRow) {
    if (!ui.renaming || ui.renaming.id !== row.id) return;
    const v = renameValue.trim();
    if (!v) {
      renameInvalid = true;
      return;
    }
    ui.renaming = null;
    if (v !== row.name) void app.rename(row.kind, row.id, v);
  }

  function menuFor(row: TreeRow): MenuItem[] {
    const f2 = "F2";
    const ix = app.index!;
    if (viewerReason && !newer(row)) return viewerMenu(row);
    if (row.kind === "collection" && newer(row)) {
      // AC-FWD-4: a collection from a newer ReqHarbor can't be renamed, set up, copied or moved here.
      return [
        { label: "New request", action: () => void app.createRequestIn(row.id, null), testid: "tree-new-request" },
        { label: "New folder", action: () => void app.createFolder(row.id, null), testid: "tree-new-folder" },
        { separator: true },
        { label: "Share…", action: () => (ui.shareDialog = { kind: "collection", entityId: row.id, name: row.name }), testid: "tree-share" },
        { separator: true },
        { label: "Delete", danger: true, action: () => void app.deleteNode("collection", row.id), testid: "tree-delete" },
      ];
    }
    if (row.kind === "folder" && newer(row)) {
      return [
        { label: "New request", action: () => void app.createRequestIn(row.collectionId, row.id), testid: "tree-new-request" },
        { label: "New folder", action: () => void app.createFolder(row.collectionId, row.id), testid: "tree-new-folder" },
        { separator: true },
        { label: "Delete", danger: true, action: () => void app.deleteNode("folder", row.id), testid: "tree-delete" },
      ];
    }
    if (row.kind === "collection") {
      return [
        { label: "New request", action: () => void app.createRequestIn(row.id, null), testid: "tree-new-request" },
        { label: "New folder", action: () => void app.createFolder(row.id, null), testid: "tree-new-folder" },
        { separator: true },
        { label: "Settings…", action: () => app.openCollection(row.id) },
        { label: "Share…", action: () => (ui.shareDialog = { kind: "collection", entityId: row.id, name: row.name }), testid: "tree-share" },
        { separator: true },
        { label: "Rename", shortcut: f2, action: () => startRename(row), testid: "tree-rename" },
        { label: "Duplicate", action: () => void app.duplicate("collection", row.id), testid: "tree-duplicate" },
        ...copyItems(row),
        { separator: true },
        { label: "Delete", danger: true, action: () => void app.deleteNode("collection", row.id), testid: "tree-delete" },
      ];
    }
    if (row.kind === "folder") {
      return [
        { label: "New request", action: () => void app.createRequestIn(row.collectionId, row.id), testid: "tree-new-request" },
        { label: "New folder", action: () => void app.createFolder(row.collectionId, row.id), testid: "tree-new-folder" },
        { separator: true },
        { label: "Rename", shortcut: f2, action: () => startRename(row), testid: "tree-rename" },
        { label: "Duplicate", action: () => void app.duplicate("folder", row.id), testid: "tree-duplicate" },
        { label: "Move to…", action: () => (ui.picker = { kind: "moveFolder", folderId: row.id }), testid: "tree-move" },
        ...copyItems(row),
        { separator: true },
        { label: "Delete", danger: true, action: () => void app.deleteNode("folder", row.id), testid: "tree-delete" },
      ];
    }
    const req = ix.requestById.get(row.id);
    if (newer(row)) {
      // UX §6.3: only Open and Delete for requests this build can't edit.
      return [
        { label: "Open", action: () => app.openRequest(row.id) },
        { separator: true },
        { label: "Delete", danger: true, action: () => void app.deleteNode("request", row.id), testid: "tree-delete" },
      ];
    }
    return [
      { label: "Open", action: () => app.openRequest(row.id) },
      { label: "Copy as cURL", shortcut: shortcutText("Mod+Shift+C", app.platform), action: () => req && void app.copyRequestCurl(row.id === app.request.id ? ($state.snapshot(app.request) as typeof req) : req) },
      { label: "Share…", action: () => (ui.shareDialog = { kind: "request", entityId: row.id, name: row.name }), testid: "tree-share" },
      { separator: true },
      { label: "Rename", shortcut: f2, action: () => startRename(row), testid: "tree-rename" },
      { label: "Duplicate", action: () => void app.duplicate("request", row.id), testid: "tree-duplicate" },
      { label: "Move to…", action: () => (ui.picker = { kind: "moveRequest", requestId: row.id }), testid: "tree-move" },
      ...copyItems(row),
      { separator: true },
      { label: "Delete", danger: true, action: () => void app.deleteNode("request", row.id), testid: "tree-delete" },
    ];
  }

  function openMenu(row: TreeRow, anchor: HTMLElement | { x: number; y: number }) {
    select(row);
    ui.openMenu(menuFor(row), anchor, { alignRight: anchor instanceof HTMLElement });
  }

  // ───────────── drag & drop (pointer events; AC-COL-3) ─────────────
  type Drop = { index: number; pos: "before" | "after" | "inside"; valid: boolean };
  let drag = $state<{ row: TreeRow; x: number; y: number } | null>(null);
  let pending: { row: TreeRow; x: number; y: number; id: number } | null = null;
  let drop = $state<Drop | null>(null);
  let suppressClick = false;
  let expandTimer: ReturnType<typeof setTimeout> | null = null;
  let scrollRaf = 0;
  let lastPointerY = 0;

  function pointerdown(e: PointerEvent, row: TreeRow) {
    if (e.button !== 0 || ui.renaming || filter || newer(row) || viewerReason) return;
    if ((e.target as HTMLElement).closest("button, input, .chev, .more")) return;
    pending = { row, x: e.clientX, y: e.clientY, id: e.pointerId };
    window.addEventListener("pointermove", pointermove);
    window.addEventListener("pointerup", pointerup, { once: true });
  }

  function computeDrop(clientY: number): Drop | null {
    const vp = list?.getViewport();
    if (!vp || !drag) return null;
    const r = vp.getBoundingClientRect();
    const y = clientY - r.top + vp.scrollTop;
    const index = Math.floor(y / ROW);
    if (index < 0 || index >= rows.length) return null;
    const target = rows[index];
    const frac = (y - index * ROW) / ROW;
    const src = drag.row;
    const container = target.kind !== "request";
    let pos: Drop["pos"];
    if (src.kind === "collection") pos = frac < 0.5 ? "before" : "after";
    else if (container) pos = frac < 0.25 ? "before" : frac > 0.75 && target.kind !== "collection" ? "after" : "inside";
    else pos = frac < 0.5 ? "before" : "after";
    return { index, pos, valid: validDrop(src, target, pos) };
  }

  function validDrop(src: TreeRow, target: TreeRow, pos: Drop["pos"]): boolean {
    if (target.id === src.id) return false;
    if (src.kind === "collection") return target.kind === "collection" && pos !== "inside";
    if (target.kind === "collection" && pos !== "inside") return false;
    if (src.kind === "folder" && app.index) {
      const parent = pos === "inside" ? (target.kind === "folder" ? target.id : null) : target.kind === "folder" ? target.parentFolderId : target.parentFolderId;
      if (parent && (parent === src.id || isInside(app.index, parent, src.id))) return false;
      if (target.kind === "folder" && isInside(app.index, target.id, src.id)) return false;
    }
    return true;
  }

  function pointermove(e: PointerEvent) {
    lastPointerY = e.clientY;
    if (pending && !drag) {
      if (Math.hypot(e.clientX - pending.x, e.clientY - pending.y) < 4) return;
      drag = { row: pending.row, x: e.clientX, y: e.clientY };
      document.body.style.cursor = "grabbing";
      scrollLoop();
    }
    if (!drag) return;
    drag = { ...drag, x: e.clientX, y: e.clientY };
    const d = computeDrop(e.clientY);
    const prev = drop;
    drop = d;
    document.body.style.cursor = d && !d.valid ? "not-allowed" : "grabbing";
    if (d?.pos === "inside" && rows[d.index].hasChildren && !rows[d.index].expanded) {
      if (!expandTimer || prev?.index !== d.index || prev?.pos !== "inside") {
        if (expandTimer) clearTimeout(expandTimer);
        const id = rows[d.index].id;
        expandTimer = setTimeout(() => ui.toggleExpanded(id, true), 600);
      }
    } else if (expandTimer) {
      clearTimeout(expandTimer);
      expandTimer = null;
    }
  }

  function scrollLoop() {
    const vp = list?.getViewport();
    if (!drag || !vp) return;
    const r = vp.getBoundingClientRect();
    if (lastPointerY < r.top + 24) vp.scrollTop -= 8;
    else if (lastPointerY > r.bottom - 24) vp.scrollTop += 8;
    scrollRaf = requestAnimationFrame(scrollLoop);
  }

  function endDrag() {
    window.removeEventListener("pointermove", pointermove);
    cancelAnimationFrame(scrollRaf);
    if (expandTimer) clearTimeout(expandTimer);
    expandTimer = null;
    document.body.style.cursor = "";
    pending = null;
    drag = null;
    drop = null;
  }

  function pointerup() {
    const d = drop;
    const src = drag?.row;
    if (src) {
      suppressClick = true;
      setTimeout(() => (suppressClick = false), 0);
    }
    endDrag();
    if (!src || !d || !d.valid || !app.index) return;
    const ix = app.index;
    const target = rows[d.index];
    if (src.kind === "collection") {
      const cols = ix.collections.filter((c) => c.id !== src.id);
      let i = cols.findIndex((c) => c.id === target.id);
      if (d.pos === "after") i++;
      void app.moveNode("collection", src.id, null, null, i);
      return;
    }
    let collectionId: string;
    let folderId: string | null;
    if (d.pos === "inside") {
      collectionId = target.collectionId;
      folderId = target.kind === "folder" ? target.id : null;
    } else {
      collectionId = target.collectionId;
      folderId = target.parentFolderId;
    }
    const siblings = (src.kind === "folder" ? childFolders(ix, collectionId, folderId) : childRequests(ix, collectionId, folderId)).filter((x) => x.id !== src.id);
    let index: number;
    if (d.pos === "inside") index = siblings.length;
    else if (target.kind === src.kind) {
      index = siblings.findIndex((x) => x.id === target.id);
      if (d.pos === "after") index++;
    } else index = src.kind === "request" ? 0 : siblings.length;
    void app.moveNode(src.kind, src.id, collectionId, folderId, Math.max(0, index));
  }
</script>

{#if app.tree && app.tree.collections.length === 0}
  <div class="empty" data-testid="collections-empty">
    <h3>No collections in {app.project?.name ?? "this project"}</h3>
    <div class="row">
      <button class="btn btn-primary" type="button" onclick={() => void app.createCollection()} data-testid="new-collection">New collection</button>
      <button class="btn btn-secondary" type="button" onclick={() => (ui.importDialog = { tab: "openapi" })}>Import OpenAPI</button>
    </div>
  </div>
{:else if filter && rows.length === 0}
  <div class="empty">
    <p>No matches for “{filter}”</p>
    <button class="link" type="button" onclick={() => (ui.filters.collections = "")}>Clear filter</button>
  </div>
{:else}
  <VirtualList bind:this={list} items={rows} itemHeight={() => ROW} role="tree" label="Collections" testid="collections-tree" {onkeydown} bind:scrollToIndex={scrollTo}>
    {#snippet row(r, i)}
      {@const isDrop = drop && drop.index === i}
      <div
        class="trow"
        class:dragging={drag?.row.id === r.id}
        class:drop-inside={isDrop && drop?.pos === "inside" && drop.valid}
        class:drop-before={isDrop && drop?.pos === "before" && drop.valid}
        class:drop-after={isDrop && drop?.pos === "after" && drop.valid}
        role="treeitem"
        aria-level={r.depth + 1}
        aria-expanded={r.kind !== "request" ? r.expanded : undefined}
        aria-selected={selected(r)}
        class:target={isTarget(r)}
        data-target={isTarget(r) && ui.menu ? "true" : undefined}
        tabindex={i === focusIdx ? 0 : -1}
        style:padding-left="{8 + r.depth * 12}px"
        style:--drop-indent="{8 + r.depth * 12 + (drop?.pos === 'inside' ? 12 : 0)}px"
        data-testid="tree-node"
        data-kind={r.kind}
        data-id={r.id}
        data-name={r.name}
        data-index={i}
        title={r.kind === "request" ? r.url : undefined}
        onpointerdown={(e) => pointerdown(e, r)}
        onclick={() => {
          if (suppressClick) return;
          focusIdx = i;
          activate(r);
        }}
        ondblclick={() => r.kind !== "folder" && r.kind !== "collection" && startRename(r)}
        oncontextmenu={(e) => {
          e.preventDefault();
          focusIdx = i;
          openMenu(r, { x: e.clientX, y: e.clientY });
        }}
        onkeydown={() => {}}
      >
        {#if r.kind !== "request"}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <span
            class="chev"
            aria-hidden="true"
            onclick={(e) => {
              e.stopPropagation();
              ui.toggleExpanded(r.id);
            }}><Icon name={r.expanded ? "chev-d" : "chev-r"} small /></span
          >
          <span class="faint ic"><Icon name={r.kind === "collection" ? "stack" : "folder"} small /></span>
        {:else}
          {#if newer(r)}
            {@const req = app.index?.requestById.get(r.id)}
            <span class="m m-custom" title={req?.kind ?? "http"} data-testid="tree-kind">{newerKindLabel(req ?? {})}</span>
          {:else}
            <span class="m {r.ws ? 'm-ws' : methodClass(r.method ?? 'GET')}" title={r.ws ? (r.method === "SIO" ? "Socket.IO" : "WebSocket") : r.method} data-testid={r.ws ? "tree-ws" : undefined}>{methodLabel(r.method ?? "GET")}</span>
          {/if}
        {/if}
        {#if ui.renaming?.id === r.id}
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="rename"
            class:invalid={renameInvalid}
            bind:value={renameValue}
            aria-label="Name"
            aria-invalid={renameInvalid}
            title={renameInvalid ? "Name can't be empty" : undefined}
            data-testid="tree-rename-input"
            autofocus
            onfocus={(e) => e.currentTarget.select()}
            oninput={() => (renameInvalid = false)}
            onkeydown={(e) => {
              e.stopPropagation();
              if (e.key === "Enter") commitRename(r);
              else if (e.key === "Escape") {
                ui.renaming = null;
                void focusRow(i);
              }
            }}
            onblur={() => (renameValue.trim() ? commitRename(r) : (ui.renaming = null))}
            onclick={(e) => e.stopPropagation()}
          />
        {:else}
          <span class="name"><Hl text={r.name} query={filter} /></span>
        {/if}
        {#if isConflictCopy(r.name)}
          <span class="conflict" title="Conflict copy · Someone's change was newer and kept as the original." aria-label="Conflict copy" role="img" data-testid="conflict-badge"><Icon name="alert" small /></span>
        {/if}
        {#if newer(r)}
          <span class="newer" title={REQUIRES_NEWER} aria-label={REQUIRES_NEWER} role="img" data-testid="newer-badge"><Icon name="alert" small /></span>
        {/if}
        {#if r.kind === "request" && app.skippedReasons.has(r.id)}
          {@const why = skippedReasonText(app.skippedReasons.get(r.id) ?? "too_large")}
          <span class="skipped" title={why.title} data-testid="not-synced-badge"><Icon name="cloud-off" small />{why.label}</span>
        {/if}
        {#if r.kind === "request" && r.ws}
          {@const st = ws.session(r.id)?.state}
          {#if st === "open" || st === "connecting" || st === "closing"}
            {@const label = st === "open" ? "Connected" : st === "connecting" ? "Connecting" : "Closing"}
            <span class="live" class:pending={st !== "open"} title={label} aria-label={label} role="img" data-testid="tree-live" data-state={st}></span>
          {/if}
        {/if}
        {#if r.kind === "request" && app.isDirty(r.id)}<span class="unsaved" title="Unsaved changes" data-testid="tree-dirty"></span>{/if}
        {#if r.kind === "collection"}<span class="cnt">{r.count}</span>{/if}
        <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
        <span
          class="icon-btn more"
          aria-hidden="true"
          data-testid="tree-node-menu"
          onclick={(e) => {
            e.stopPropagation();
            focusIdx = i;
            openMenu(r, e.currentTarget);
          }}><Icon name="more" small /></span
        >
      </div>
    {/snippet}
  </VirtualList>
  {#if drag}
    <div class="ghost m-ghost" style:left="{drag.x + 12}px" style:top="{drag.y + 8}px">{drag.row.name}</div>
  {/if}
{/if}

<style>
  .conflict {
    display: inline-flex;
    color: var(--warning);
    flex: none;
  }
  .row {
    display: flex;
    gap: var(--sp-4);
  }
  .trow {
    height: var(--h-row-tree);
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    padding-right: var(--sp-3);
    white-space: nowrap;
    position: relative;
    outline: none;
    cursor: default;
  }
  .trow:hover {
    background: var(--bg-hover);
  }
  .trow[aria-selected="true"] {
    background: var(--bg-selected-muted);
  }
  :global(.vl:focus-within) .trow[aria-selected="true"] {
    background: var(--bg-selected);
  }
  .trow[data-target="true"],
  :global(.vl:focus-within) .trow.target {
    box-shadow: inset 0 0 0 1px var(--border-focus);
  }
  .trow:focus-visible {
    box-shadow: inset 0 0 0 2px var(--focus-ring);
  }
  .trow.dragging {
    opacity: 0.5;
  }
  .trow.drop-inside {
    background: var(--accent-soft);
  }
  .trow.drop-before::before,
  .trow.drop-after::after {
    content: "";
    position: absolute;
    left: var(--drop-indent);
    right: 0;
    height: 2px;
    background: var(--accent-fg);
  }
  .trow.drop-before::before {
    top: -1px;
  }
  .trow.drop-after::after {
    bottom: -1px;
  }
  .chev {
    width: 16px;
    height: 16px;
    display: inline-grid;
    cursor: pointer;
    place-items: center;
    padding: 0;
    border: 0;
    background: none;
    color: var(--text-3);
    flex: none;
  }
  .ic {
    display: inline-flex;
  }
  .m {
    width: var(--w-method-col);
    flex: none;
    padding-left: 2px;
  }
  .name {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cnt {
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .trow:hover .cnt {
    display: none;
  }
  .unsaved {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--text-2);
    flex: none;
  }
  /* An open WebSocket / Socket.IO connection (DESIGN §5.3). */
  .live {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--success);
    flex: none;
  }
  .live.pending {
    background: var(--warning);
  }
  .newer {
    display: inline-flex;
    width: 14px;
    height: 14px;
    align-items: center;
    justify-content: center;
    color: var(--warning);
    flex: none;
  }
  .skipped {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    color: var(--warning);
    font-size: var(--fs-xs);
    flex: none;
  }
  .more {
    width: 20px;
    height: 20px;
    display: none;
  }
  .trow:hover .more,
  .trow:focus-within .more {
    display: inline-grid;
  }
  .rename {
    flex: 1;
    min-width: 0;
    height: 22px;
    border: 1px solid var(--border-focus);
    border-radius: var(--r-xs);
    background: var(--bg-input);
    color: var(--text-1);
    padding: 0 var(--sp-2);
    outline: none;
    font-size: var(--fs-md);
  }
  .rename.invalid {
    border-color: var(--danger);
  }
  .ghost {
    position: fixed;
    z-index: var(--z-tooltip);
    pointer-events: none;
    padding: 2px 8px;
    background: var(--bg-raised);
    border-radius: var(--r-sm);
    box-shadow: var(--shadow-popover);
    font-size: var(--fs-sm);
  }
</style>

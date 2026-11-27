<script lang="ts">
  import { tick } from "svelte";
  import type { Request } from "../../api/types";
  import { app } from "../../stores/app.svelte";
  import { grpc } from "../../stores/grpc.svelte";
  import { CALL_KIND_LABEL, filterMethods, findMethod, shortName } from "../../util/grpc";
  import Icon from "../Icon.svelte";

  /** The request bar's service / method picker (AC-GRPC-5, DESIGN §5.20). */
  let { onuseFiles }: { onuseFiles: () => void } = $props();

  const r = $derived(app.request);
  const g = $derived(r.grpc);
  const st = $derived(grpc.state(r.id));
  const d = $derived(st.descriptor);
  const picked = $derived(g ? findMethod(d, g.service, g.method) : null);
  const kind = $derived(picked?.callKind ?? app.grpcCallKind());
  const missing = $derived(!!g?.method && !!d && !picked);

  let open = $state(false);
  let query = $state("");
  let hl = $state(0);
  let trigger = $state<HTMLButtonElement>();
  let pop = $state<HTMLDivElement>();
  let searchEl = $state<HTMLInputElement>();

  const groups = $derived(filterMethods(d?.services ?? [], query));
  const flat = $derived(groups.flatMap((gr) => gr.methods.map((m) => ({ service: gr.service, m }))));

  async function toggle() {
    if (open) {
      open = false;
      return;
    }
    open = true;
    query = "";
    const i = flat.findIndex((x) => x.service === g?.service && x.m.name === g?.method);
    hl = Math.max(0, i);
    await tick();
    searchEl?.focus();
  }

  function choose(service: string, method: string) {
    const req = app.request as Request;
    if (req.grpc) {
      req.grpc.service = service;
      req.grpc.method = method;
    }
    app.sendBlocked = null;
    open = false;
    trigger?.focus();
  }

  function key(e: KeyboardEvent) {
    if (e.key === "ArrowDown") hl = flat.length ? (hl + 1) % flat.length : 0;
    else if (e.key === "ArrowUp") hl = flat.length ? (hl - 1 + flat.length) % flat.length : 0;
    else if (e.key === "Enter") {
      const x = flat[hl];
      if (x) choose(x.service, x.m.name);
    } else if (e.key === "Escape") {
      open = false;
      trigger?.focus();
    } else return;
    e.preventDefault();
    e.stopPropagation();
    void tick().then(() => pop?.querySelector(".mi.hl")?.scrollIntoView({ block: "nearest" }));
  }

  /** Highlight the matched part of a name. */
  function parts(text: string): [string, string, string] {
    const q = query.trim().toLowerCase();
    const i = q ? text.toLowerCase().indexOf(q) : -1;
    return i < 0 ? [text, "", ""] : [text.slice(0, i), text.slice(i, i + q.length), text.slice(i + q.length)];
  }
</script>

<svelte:window
  onmousedown={(e) => {
    if (open && pop && !pop.contains(e.target as Node) && !trigger?.contains(e.target as Node)) open = false;
  }}
/>

<div class="picker">
  <button
    bind:this={trigger}
    class="trigger"
    type="button"
    aria-haspopup="listbox"
    aria-expanded={open}
    title={g?.method ? `${g.service}/${g.method}${missing ? " · Not in the current services" : ""}` : "Select a method"}
    data-testid="grpc-method-picker"
    onclick={() => void toggle()}
  >
    {#if g?.method}
      {#if missing}<span class="warn" data-testid="grpc-method-missing"><Icon name="alert" small label="Not in the current services" /></span>{/if}
      <span class="name mono"><span class="svc">{shortName(g.service)}</span> / <span data-testid="grpc-method-name">{g.method}</span></span>
      {#if kind}<span class="kind" data-testid="grpc-call-kind">{CALL_KIND_LABEL[kind]}</span>{/if}
    {:else}
      <span class="ph">Select a method</span>
    {/if}
    <Icon name="chev-d" small />
  </button>
  {#if open}
    <div class="menu pop list" bind:this={pop} data-testid="grpc-method-list">
      <label class="search">
        <Icon name="search" small />
        <input
          bind:this={searchEl}
          class="input"
          placeholder="Search services and methods"
          aria-label="Search services and methods"
          bind:value={query}
          oninput={() => (hl = 0)}
          onkeydown={key}
          data-testid="grpc-method-search"
        />
      </label>
      <div class="scroll" role="listbox" aria-label="Methods">
        {#if !d || d.services.length === 0}
          <div class="none" data-testid="grpc-method-none">
            <p>No services yet</p>
            <span>
              <button class="link" type="button" disabled={st.status === "fetching"} onclick={() => { open = false; void grpc.fetch($state.snapshot(app.request) as Request, app.activeProjectId); }} data-testid="grpc-picker-fetch">Fetch services</button>
              ·
              <button class="link" type="button" onclick={() => { open = false; onuseFiles(); }}>Use .proto files</button>
            </span>
          </div>
        {:else if !flat.length}
          <div class="none"><p>No methods match “{query.trim()}”</p></div>
        {:else}
          {#each groups as gr (gr.service)}
            <div class="group" role="presentation">{gr.service}</div>
            {#each gr.methods as m (m.name)}
              {@const i = flat.findIndex((x) => x.service === gr.service && x.m.name === m.name)}
              {@const p = parts(m.name)}
              <div
                class="mi"
                class:hl={hl === i}
                role="option"
                tabindex="-1"
                aria-selected={g?.service === gr.service && g?.method === m.name}
                data-testid="grpc-method-option"
                data-method="{gr.service}/{m.name}"
                onmouseenter={() => (hl = i)}
                onclick={() => choose(gr.service, m.name)}
                onkeydown={() => {}}
              >
                <span class="mono">{p[0]}<b>{p[1]}</b>{p[2]}</span>
                <span class="sc">{CALL_KIND_LABEL[m.callKind]}</span>
              </div>
            {/each}
          {/each}
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .picker {
    position: relative;
    flex: 0 1 auto;
    min-width: 120px;
    max-width: 42%;
    border-left: 1px solid var(--border-subtle);
    display: flex;
  }
  .trigger {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    min-width: 0;
    width: 100%;
    height: 100%;
    padding: 0 var(--sp-3) 0 var(--sp-4);
    border: 0;
    background: none;
    color: var(--text-3);
    border-radius: 0;
  }
  .trigger:hover {
    background: var(--bg-hover);
  }
  .name {
    color: var(--text-1);
    font-size: var(--fs-code);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-width: 0;
  }
  .svc {
    color: var(--text-2);
  }
  .ph {
    flex: 1;
    text-align: left;
    white-space: nowrap;
  }
  .kind {
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
    white-space: nowrap;
  }
  .warn {
    color: var(--warning);
    display: inline-flex;
  }
  .list {
    position: absolute;
    top: calc(100% + 6px);
    right: -1px;
    width: 360px;
    max-width: 80vw;
    z-index: var(--z-popover);
    display: flex;
    flex-direction: column;
    --bg-input: var(--bg-input-raised);
  }
  .search {
    display: flex;
    align-items: center;
    gap: var(--sp-2);
    padding: var(--sp-2);
    color: var(--text-3);
  }
  .search .input {
    flex: 1;
    height: 26px;
    border-radius: 13px;
  }
  .scroll {
    max-height: 360px;
    overflow: auto;
  }
  .group {
    padding: var(--sp-3) var(--sp-3) var(--sp-1);
    font-size: var(--fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
  }
  .mi b {
    color: var(--accent-fg);
    font-weight: var(--fw-semibold);
  }
  .mi.hl b {
    color: inherit;
  }
  .none {
    padding: var(--sp-4);
    color: var(--text-3);
    font-size: var(--fs-sm);
  }
  .none p {
    margin: 0 0 var(--sp-2);
  }
</style>

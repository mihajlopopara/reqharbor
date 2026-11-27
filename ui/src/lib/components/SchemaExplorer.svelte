<script lang="ts">
  /**
   * Schema explorer, the GraphQL "Docs" pane (AC-GQL-10): root types, all types, search, a type
   * page with fields / arguments / enum values / members, and Insert for selectable fields.
   */
  import { tick, untrack } from "svelte";
  import { graphql } from "../stores/graphql.svelte";
  import { KIND_LABEL, rootTypes, searchSchema, type ArgModel, type FieldModel, type SchemaModel, type SearchHit } from "../util/graphql";
  import Icon from "./Icon.svelte";

  let {
    model,
    oninsert,
    onclose,
  }: {
    model: SchemaModel;
    oninsert: (field: FieldModel) => void;
    onclose: () => void;
  } = $props();

  let stack = $state<string[]>([]);
  let focusName = $state<string | null>(null);
  let term = $state("");
  let bodyEl = $state<HTMLDivElement>();

  const current = $derived(stack.length ? (model.byName[stack[stack.length - 1]] ?? null) : null);
  const hits = $derived(term.trim() ? searchSchema(model, term) : []);
  const roots = $derived(rootTypes(model));
  const insertable = $derived(current?.kind === "OBJECT" || current?.kind === "INTERFACE");

  // A new schema without the open type: back to the start.
  $effect(() => {
    const m = model;
    untrack(() => {
      if (stack.some((t) => !m.byName[t])) stack = [];
    });
  });

  // Mod-click in the query editor asks for a type (and field).
  $effect(() => {
    const t = graphql.docsTarget;
    if (t) untrack(() => open(t.type, t.field));
  });

  function open(type: string, name: string | null = null) {
    if (!model.byName[type]) return;
    term = "";
    if (stack[stack.length - 1] !== type) stack = [...stack, type];
    focusName = name;
    void tick().then(() => {
      if (name) bodyEl?.querySelector<HTMLElement>(`[data-name="${CSS.escape(name)}"]`)?.scrollIntoView({ block: "nearest" });
      else bodyEl?.scrollTo({ top: 0 });
    });
  }

  function openHit(h: SearchHit) {
    open(h.type, h.field);
  }

  function back() {
    stack = stack.slice(0, -1);
    focusName = null;
  }

  /** "[User!]!" around its named type, so the name can be a link. */
  function typeParts(type: string, named: string): [string, string, string] {
    const i = type.indexOf(named);
    return i < 0 ? ["", type, ""] : [type.slice(0, i), named, type.slice(i + named.length)];
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (term) term = "";
      else if (stack.length) back();
      else onclose();
    }
  }
</script>

{#snippet typeLink(type: string, named: string)}
  {@const [pre, name, post] = typeParts(type, named)}
  <span class="tref"
    >{pre}<button type="button" class="tlink" onclick={() => open(name)} title="Open {name}" data-testid="docs-type-link-{name}">{name}</button>{post}</span
  >
{/snippet}

{#snippet argList(args: ArgModel[])}
  {#if args.length}
    <span class="punct">(</span>{#each args as a, i (a.name)}{#if i}<span class="punct">{", "}</span>{/if}<span class="aname">{a.name}</span><span class="punct">{": "}</span>{@render typeLink(a.type, a.namedType)}{#if a.defaultValue !== null}<span class="punct">{" = "}</span><span class="dval">{a.defaultValue}</span>{/if}{/each}<span class="punct">)</span>
  {/if}
{/snippet}

<!-- Esc inside the explorer: clear search, go back, then close (keyboard events bubble from its controls). -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<aside class="docs" aria-label="Schema explorer" data-testid="schema-explorer" onkeydown={onKey}>
  <div class="docs-head">
    {#if current}
      <button class="icon-btn" type="button" aria-label="Back" title="Back" onclick={back} data-testid="docs-back"><Icon name="chev-l" /></button>
    {:else}
      <Icon name="book" small />
    {/if}
    <span class="title" data-testid="docs-title">{current ? current.name : "Schema"}</span>
    <button class="icon-btn" type="button" aria-label="Close schema explorer" title="Close" onclick={onclose} data-testid="docs-close"><Icon name="x" /></button>
  </div>
  <div class="docs-search">
    <label class="pill">
      <Icon name="search" small />
      <input bind:value={term} placeholder="Search types and fields" aria-label="Search the schema" data-testid="docs-search" />
    </label>
  </div>
  <div class="docs-body" bind:this={bodyEl}>
    {#if term.trim()}
      {#if hits.length}
        <ul class="rows" aria-label="Search results">
          {#each hits as h (h.label)}
            <li>
              <button type="button" class="row" onclick={() => openHit(h)} data-testid="docs-hit">
                <span class="mono name">{h.label}</span><span class="faint mono detail">{h.detail}</span>
              </button>
            </li>
          {/each}
        </ul>
      {:else}
        <p class="faint empty-line">No matches for “{term.trim()}”</p>
      {/if}
    {:else if !current}
      {#if roots.length}
        <h4 class="group">Root types</h4>
        <ul class="rows">
          {#each roots as r (r.operation)}
            <li>
              <button type="button" class="row" onclick={() => open(r.type)} data-testid="docs-root-{r.operation}">
                <span class="mono kw">{r.operation}</span><span class="mono name t">{r.type}</span>
              </button>
            </li>
          {/each}
        </ul>
      {/if}
      <h4 class="group">All types <span class="count">{model.types.length}</span></h4>
      <ul class="rows">
        {#each model.types as t (t.name)}
          <li>
            <button type="button" class="row" onclick={() => open(t.name)} data-testid="docs-type-{t.name}">
              <span class="mono name t">{t.name}</span><span class="faint detail">{KIND_LABEL[t.kind]}</span>
            </button>
          </li>
        {/each}
      </ul>
    {:else}
      <div class="type-head">
        <span class="faint kind">{KIND_LABEL[current.kind]}</span>
        {#if current.interfaces.length}
          <span class="faint">implements {#each current.interfaces as i, n (i)}{#if n}, {/if}<button type="button" class="tlink" onclick={() => open(i)}>{i}</button>{/each}</span>
        {/if}
      </div>
      {#if current.description}<p class="desc" data-testid="docs-description">{current.description}</p>{/if}

      {#if current.fields.length}
        <h4 class="group">{current.kind === "INPUT_OBJECT" ? "Input fields" : "Fields"}</h4>
        <ul class="fields">
          {#each current.fields as f (f.name)}
            <li class="field-row" class:hl={focusName === f.name} data-name={f.name} data-testid="docs-field-{f.name}">
              <div class="sig mono">
                <span class="fname" class:dep={f.deprecated !== null}>{f.name}</span>{@render argList(f.args)}<span class="punct">{": "}</span>{@render typeLink(f.type, f.namedType)}
                {#if insertable}
                  <button class="icon-btn ins" type="button" aria-label="Insert {f.name} at the cursor" title="Insert at the cursor" onclick={() => oninsert(f)} data-testid="docs-insert-{f.name}"><Icon name="plus" small /></button>
                {/if}
              </div>
              {#if f.description}<p class="desc">{f.description}</p>{/if}
              {#if f.deprecated !== null}<p class="dep-note">Deprecated{f.deprecated ? `: ${f.deprecated}` : ""}</p>{/if}
              {#if f.args.some((a) => a.description || a.deprecated !== null)}
                <ul class="arg-docs">
                  {#each f.args.filter((a) => a.description || a.deprecated !== null) as a (a.name)}
                    <li><span class="mono aname">{a.name}</span> <span class="faint">{a.description ?? ""}{a.deprecated !== null ? ` (deprecated${a.deprecated ? `: ${a.deprecated}` : ""})` : ""}</span></li>
                  {/each}
                </ul>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if current.enumValues.length}
        <h4 class="group">Values</h4>
        <ul class="fields">
          {#each current.enumValues as v (v.name)}
            <li class="field-row" class:hl={focusName === v.name} data-name={v.name} data-testid="docs-value-{v.name}">
              <div class="sig mono"><span class="fname ev" class:dep={v.deprecated !== null}>{v.name}</span></div>
              {#if v.description}<p class="desc">{v.description}</p>{/if}
              {#if v.deprecated !== null}<p class="dep-note">Deprecated{v.deprecated ? `: ${v.deprecated}` : ""}</p>{/if}
            </li>
          {/each}
        </ul>
      {/if}

      {#if current.possibleTypes.length}
        <h4 class="group">{current.kind === "UNION" ? "Members" : "Implemented by"}</h4>
        <ul class="rows">
          {#each current.possibleTypes as p (p)}
            <li><button type="button" class="row" onclick={() => open(p)}><span class="mono name t">{p}</span></button></li>
          {/each}
        </ul>
      {/if}
      {#if current.kind === "SCALAR" && !current.description}
        <p class="faint empty-line">A scalar: values are sent as JSON.</p>
      {/if}
      {#if insertable && current.fields.length}
        <p class="faint tip">Insert adds a field at the query cursor.</p>
      {/if}
    {/if}
  </div>
</aside>

<style>
  .docs {
    width: 280px;
    flex: none;
    display: flex;
    flex-direction: column;
    min-height: 0;
    border-left: 1px solid var(--border-subtle);
    background: var(--bg-pane);
  }
  .docs-head {
    display: flex;
    align-items: center;
    gap: var(--sp-3);
    height: 30px;
    padding: 0 var(--sp-2) 0 var(--sp-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-sidebar);
    color: var(--text-3);
    flex: none;
  }
  .docs-head .title {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-1);
    font-weight: var(--fw-medium);
    font-size: var(--fs-md);
  }
  .docs-search {
    display: flex;
    padding: var(--sp-3) var(--sp-4);
    flex: none;
  }
  .docs-body {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 0 var(--sp-4) var(--sp-5);
  }
  .group {
    margin: var(--sp-4) 0 var(--sp-2);
    font-size: var(--fs-xs);
    line-height: var(--lh-xs, 16px);
    font-weight: var(--fw-semibold);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--text-3);
  }
  .count {
    font-weight: var(--fw-regular);
    margin-left: var(--sp-2);
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .row {
    width: 100%;
    display: flex;
    align-items: baseline;
    gap: var(--sp-3);
    min-height: 26px;
    padding: var(--sp-1) var(--sp-3);
    border: 0;
    border-radius: var(--r-sm);
    background: none;
    color: var(--text-1);
    text-align: left;
    font-size: var(--fs-sm);
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--fs-sm);
  }
  .name.t {
    color: var(--syn-tag);
  }
  .kw {
    color: var(--syn-bool);
    font-size: var(--fs-sm);
    min-width: 84px;
  }
  .detail {
    margin-left: auto;
    flex: none;
    max-width: 50%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: var(--fs-sm);
  }
  .type-head {
    display: flex;
    flex-wrap: wrap;
    gap: var(--sp-3);
    margin-top: var(--sp-3);
    font-size: var(--fs-sm);
  }
  .kind {
    text-transform: uppercase;
    font-size: var(--fs-xs);
    letter-spacing: 0.06em;
  }
  .desc {
    margin: var(--sp-2) 0 0;
    font-size: var(--fs-sm);
    color: var(--text-2);
    white-space: pre-wrap;
  }
  .field-row {
    padding: var(--sp-3) var(--sp-3);
    margin: 0 calc(-1 * var(--sp-3));
    border-radius: var(--r-sm);
    border-bottom: 1px solid var(--border-subtle);
  }
  .field-row:last-child {
    border-bottom: 0;
  }
  .field-row.hl {
    background: var(--accent-soft);
  }
  .sig {
    position: relative;
    font-size: var(--fs-sm);
    line-height: 18px;
    padding-right: 26px;
    word-break: break-word;
  }
  .fname {
    color: var(--syn-key);
  }
  .fname.ev {
    color: var(--syn-number);
  }
  .dep {
    text-decoration: line-through;
  }
  .aname {
    color: var(--syn-attr);
  }
  .punct {
    color: var(--syn-punct);
  }
  .dval {
    color: var(--syn-number);
  }
  .tlink {
    border: 0;
    background: none;
    padding: 0;
    font: inherit;
    color: var(--syn-tag);
    cursor: pointer;
    border-radius: var(--r-xs);
  }
  .tlink:hover {
    text-decoration: underline;
  }
  .ins {
    position: absolute;
    right: -2px;
    top: -3px;
    opacity: 0;
  }
  .field-row:hover .ins,
  .ins:focus-visible {
    opacity: 1;
  }
  @media (hover: none) {
    .ins {
      opacity: 1;
    }
  }
  .dep-note {
    margin: var(--sp-2) 0 0;
    font-size: var(--fs-sm);
    color: var(--warning);
  }
  .arg-docs {
    margin-top: var(--sp-2);
    font-size: var(--fs-sm);
    display: flex;
    flex-direction: column;
    gap: var(--sp-1);
  }
  .empty-line,
  .tip {
    margin: var(--sp-4) 0;
    font-size: var(--fs-sm);
  }
</style>

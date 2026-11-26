<script lang="ts" module>
  export interface KvRow {
    key: string;
    value: string;
    enabled: boolean;
    description?: string;
    kind?: "text" | "file";
    contentType?: string;
    secret?: boolean;
  }

  const COMMON_HEADERS = [
    "Accept", "Accept-Encoding", "Accept-Language", "Authorization", "Cache-Control", "Connection", "Content-Type",
    "Content-Length", "Cookie", "If-Match", "If-None-Match", "If-Modified-Since", "Origin", "Prefer", "Referer",
    "User-Agent", "X-API-Key", "X-Request-ID", "X-Requested-With", "X-Forwarded-For", "Idempotency-Key",
  ];
</script>

<script lang="ts">
  import { tick } from "svelte";
  import type { VarLookup } from "../util/vars";
  import VarInput from "./VarInput.svelte";
  import Icon from "./Icon.svelte";
  import { pickFile, fileNameOf } from "../api/platform";

  /**
   * Key-value grid (DESIGN §5.7). Mutates `rows` in place (they are reactive
   * store proxies) and calls `onchange` after every edit.
   */
  let {
    rows,
    kind,
    lookup,
    onchange,
    testid = "kv",
  }: {
    rows: KvRow[];
    kind: "query" | "headers" | "formEncoded" | "form" | "env";
    lookup: VarLookup;
    onchange?: () => void;
    testid?: string;
  } = $props();

  const NOUN: Record<typeof kind, [string, string]> = {
    query: ["param", "params"],
    headers: ["header", "headers"],
    formEncoded: ["field", "fields"],
    form: ["field", "fields"],
    env: ["variable", "variables"],
  };
  const noun = $derived(NOUN[kind]);
  const sep = $derived(kind === "headers" ? ": " : "=");
  let showDesc = $state(false);
  let bulk = $state(false);
  let bulkText = $state("");
  let focusedSecret = $state<number | null>(null);
  let tbody = $state<HTMLTableSectionElement>();
  const listId = `hdr-${Math.random().toString(36).slice(2)}`;

  const enabledCount = $derived(rows.filter((r) => r.enabled).length);

  function newRow(): KvRow {
    const r: KvRow = { key: "", value: "", enabled: true };
    if (kind === "form") r.kind = "text";
    if (kind === "env") r.secret = false;
    return r;
  }

  function edit(i: number, field: "key" | "value" | "description", v: string) {
    if (i === rows.length) {
      const r = newRow();
      r[field] = v;
      rows.push(r);
    } else rows[i][field] = v;
    onchange?.();
  }

  function remove(i: number) {
    rows.splice(i, 1);
    onchange?.();
  }

  async function focusCell(i: number, col: number) {
    await tick();
    tbody?.querySelectorAll("tr")[i]?.querySelectorAll<HTMLInputElement>("input[type=text], input[type=password]")[col]?.focus();
  }

  function cellKey(e: KeyboardEvent, i: number, col: number) {
    if (e.key === "Enter" && col >= 1) {
      e.preventDefault();
      void focusCell(i + 1, 0);
    } else if ((e.metaKey || e.ctrlKey) && e.key === "Backspace" && i < rows.length) {
      e.preventDefault();
      remove(i);
      void focusCell(Math.min(i, rows.length), col);
    }
  }

  function toBulk(): string {
    return rows.map((r) => `${r.enabled ? "" : "// "}${r.key}${sep}${r.value}`).join("\n");
  }

  function fromBulk(text: string) {
    const parsed: KvRow[] = [];
    for (const raw of text.split("\n")) {
      if (!raw.trim()) continue;
      let line = raw;
      let enabled = true;
      if (/^\s*\/\/\s?/.test(line)) {
        enabled = false;
        line = line.replace(/^\s*\/\/\s?/, "");
      }
      const idx = kind === "headers" ? line.indexOf(":") : line.indexOf("=");
      const key = (idx >= 0 ? line.slice(0, idx) : line).trim();
      const value = idx >= 0 ? line.slice(idx + 1).replace(/^ /, "") : "";
      const prev = rows.find((r) => r.key === key);
      parsed.push({ ...newRow(), ...(prev ? { kind: prev.kind, secret: prev.secret, description: prev.description } : {}), key, value, enabled });
    }
    rows.splice(0, rows.length, ...parsed);
    onchange?.();
  }

  function toggleBulk() {
    if (bulk) fromBulk(bulkText);
    else bulkText = toBulk();
    bulk = !bulk;
  }

  async function chooseFile(i: number) {
    const f = await pickFile({ title: "Choose file" });
    if (!f) return;
    if (i === rows.length) rows.push({ ...newRow(), kind: "file", key: "file", value: f.path });
    else rows[i].value = f.path;
    onchange?.();
  }

  const display = $derived([...rows, null]);
</script>

<div class="kvt" data-testid="{testid}-table">
  <div class="toolbar">
    <span class="faint count">{enabledCount} {enabledCount === 1 ? noun[0] : noun[1]}</span>
    <span class="grow"></span>
    {#if kind !== "env"}
      <button class="link" type="button" aria-pressed={showDesc} onclick={() => (showDesc = !showDesc)}>Description</button>
    {/if}
    {#if kind !== "form"}
      <button class="link" type="button" aria-pressed={bulk} onclick={toggleBulk} data-testid="{testid}-bulk">{bulk ? "Table" : "Bulk edit"}</button>
    {/if}
  </div>
  {#if bulk}
    <textarea
      class="input bulk"
      rows="8"
      spellcheck="false"
      aria-label="Bulk edit {noun[1]}"
      placeholder={kind === "headers" ? "Content-Type: application/json\n// X-Disabled: value" : "key=value\n// disabled=value"}
      bind:value={bulkText}
      onblur={() => fromBulk(bulkText)}
      data-testid="{testid}-bulk-text"
    ></textarea>
  {:else}
    <table class="kv">
      <colgroup>
        <col style="width:34px" />
        <col style="width:34%" />
        {#if kind === "form"}<col style="width:72px" />{/if}
        <col />
        {#if showDesc && kind !== "env"}<col style="width:24%" />{/if}
        {#if kind === "env"}<col style="width:34px" />{/if}
        <col style="width:28px" />
      </colgroup>
      <tbody bind:this={tbody}>
        {#each display as row, i}
          {@const ghost = row === null}
          {@const r = row ?? newRow()}
          <tr class:off={!ghost && !r.enabled} class:ghost data-testid={ghost ? `${testid}-ghost` : `${testid}-row`}>
            <td class="cbx">
              {#if !ghost}
                <input
                  type="checkbox"
                  checked={r.enabled}
                  aria-label="Enable {r.key || noun[0]}"
                  data-testid="{testid}-enabled"
                  onchange={(e) => {
                    rows[i].enabled = e.currentTarget.checked;
                    onchange?.();
                  }}
                />
              {/if}
            </td>
            <td>
              <VarInput
                variant="cell"
                {lookup}
                value={r.key}
                strike={!ghost && !r.enabled}
                placeholder={ghost ? `Add ${noun[0]}` : "Key"}
                ariaLabel="{noun[0]} name"
                testid="{testid}-key"
                list={kind === "headers" ? listId : undefined}
                oninput={(v) => edit(i, "key", v)}
                onkeydown={(e) => cellKey(e, i, 0)}
              />
            </td>
            {#if kind === "form"}
              <td>
                <select
                  class="kind"
                  aria-label="Field type"
                  value={r.kind ?? "text"}
                  onchange={(e) => {
                    if (ghost) rows.push({ ...newRow(), kind: e.currentTarget.value as "text" | "file" });
                    else {
                      rows[i].kind = e.currentTarget.value as "text" | "file";
                      rows[i].value = "";
                    }
                    onchange?.();
                  }}
                >
                  <option value="text">Text</option>
                  <option value="file">File</option>
                </select>
              </td>
            {/if}
            <td>
              {#if kind === "form" && r.kind === "file"}
                <div class="file">
                  <button class="btn btn-secondary btn-sm" type="button" onclick={() => chooseFile(i)}>Choose file…</button>
                  <span class="fname mono" title={r.value}>{r.value ? fileNameOf(r.value) : "No file"}</span>
                </div>
              {:else if kind === "env" && r.secret && focusedSecret !== i}
                <input
                  class="secret-mask"
                  type="password"
                  value={r.value}
                  aria-label="Secret value (focus to reveal)"
                  data-testid="{testid}-value"
                  onfocus={() => {
                    focusedSecret = i;
                    void focusCell(i, 1);
                  }}
                  readonly
                />
              {:else}
                <VarInput
                  variant="cell"
                  {lookup}
                  value={r.value}
                  strike={!ghost && !r.enabled}
                  placeholder="Value"
                  ariaLabel="{noun[0]} value"
                  testid="{testid}-value"
                  oninput={(v) => edit(i, "value", v)}
                  onkeydown={(e) => cellKey(e, i, 1)}
                  onblur={() => {
                    if (focusedSecret === i) focusedSecret = null;
                  }}
                />
              {/if}
            </td>
            {#if showDesc && kind !== "env"}
              <td>
                <input class="desc" type="text" value={r.description ?? ""} placeholder="Description" aria-label="Description" oninput={(e) => edit(i, "description", e.currentTarget.value)} />
              </td>
            {/if}
            {#if kind === "env"}
              <td class="cbx">
                {#if !ghost}
                  <button
                    class="icon-btn lock"
                    class:on={r.secret}
                    type="button"
                    aria-pressed={!!r.secret}
                    aria-label={r.secret ? "Secret (click to make visible)" : "Mark as secret"}
                    title={r.secret ? "Secret — never synced, shared or written to history" : "Mark as secret"}
                    data-testid="{testid}-secret"
                    onclick={() => {
                      rows[i].secret = !rows[i].secret;
                      onchange?.();
                    }}><Icon name={r.secret ? "lock" : "unlock"} small /></button
                  >
                {/if}
              </td>
            {/if}
            <td class="act">
              {#if !ghost}
                <button class="icon-btn del" type="button" aria-label="Delete {r.key || noun[0]}" data-testid="{testid}-delete" onclick={() => remove(i)}><Icon name="trash" small /></button>
              {/if}
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  {/if}
  {#if kind === "headers"}
    <datalist id={listId}>
      {#each COMMON_HEADERS as h (h)}<option value={h}></option>{/each}
    </datalist>
  {/if}
</div>

<style>
  .kvt {
    display: flex;
    flex-direction: column;
    gap: var(--sp-2);
  }
  .toolbar {
    height: 24px;
    display: flex;
    align-items: center;
    gap: var(--sp-2);
  }
  .count {
    font-size: var(--fs-sm);
  }
  .link[aria-pressed="true"] {
    background: var(--accent-soft);
  }
  .cbx {
    text-align: center;
  }
  tr:hover td {
    background: var(--bg-hover);
  }
  tr.off :global(.overlay) {
    color: var(--text-3);
  }
  .act {
    text-align: center;
  }
  .del {
    opacity: 0;
  }
  tr:hover .del,
  tr:focus-within .del,
  .del:focus-visible {
    opacity: 1;
  }
  .kind,
  .desc,
  .secret-mask {
    width: 100%;
    height: 26px;
    border: 1px solid transparent;
    background: transparent;
    border-radius: var(--r-xs);
    color: var(--text-1);
    font: var(--fs-code) var(--font-mono);
    padding: 0 var(--sp-3);
    outline: none;
  }
  .kind {
    font-family: var(--font-ui);
    font-size: var(--fs-sm);
  }
  .desc {
    font-family: var(--font-ui);
    font-size: var(--fs-md);
  }
  .kind:focus,
  .desc:focus {
    background: var(--bg-input);
    box-shadow: inset 0 0 0 1px var(--border-focus);
  }
  .file {
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    min-width: 0;
    padding: 0 var(--sp-3);
  }
  .fname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-2);
  }
  .lock.on {
    color: var(--accent-fg);
  }
  .bulk {
    min-height: 160px;
  }
</style>

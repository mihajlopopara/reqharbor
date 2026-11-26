<script lang="ts">
  import type { Auth, AuthType, Collection } from "../api/types";
  import type { VarLookup } from "../util/vars";
  import VarInput from "./VarInput.svelte";
  import Icon from "./Icon.svelte";

  /** Auth forms (DESIGN §5.8). Mutates `auth` in place, then calls onchange. */
  let {
    auth,
    lookup,
    allowInherit = true,
    inheritFrom = null,
    authHeaderOverride = false,
    onchange,
    oneditcollection,
  }: {
    auth: Auth;
    lookup: VarLookup;
    allowInherit?: boolean;
    inheritFrom?: Collection | null;
    authHeaderOverride?: boolean;
    onchange?: () => void;
    oneditcollection?: () => void;
  } = $props();

  const TYPES: { v: AuthType; label: string }[] = [
    { v: "none", label: "None" },
    { v: "inherit", label: "Inherit" },
    { v: "basic", label: "Basic" },
    { v: "bearer", label: "Bearer" },
    { v: "apikey", label: "API Key" },
  ];
  const LABEL: Record<AuthType, string> = { none: "None", inherit: "Inherit", basic: "Basic", bearer: "Bearer", apikey: "API Key" };
  let showSecret = $state(false);

  function setType(t: AuthType) {
    auth.type = t;
    if (t === "basic") {
      auth.username ??= "";
      auth.password ??= "";
    } else if (t === "bearer") auth.token ??= "";
    else if (t === "apikey") auth.apiKey ??= { key: "X-API-Key", value: "", in: "header" };
    onchange?.();
  }
</script>

<div class="auth" data-testid="auth-form">
  <label class="field type">
    <span>Type</span>
    <span class="select-wrap">
    <select class="input" value={auth.type} onchange={(e) => setType(e.currentTarget.value as AuthType)} data-testid="auth-type">
      {#each TYPES.filter((t) => allowInherit || t.v !== "inherit") as t (t.v)}
        <option value={t.v}>{t.label}</option>
      {/each}
    </select><Icon name="chev-d" small /></span>
  </label>

  {#if authHeaderOverride && auth.type !== "none"}
    <div class="hint"><Icon name="info" />An Authorization header in Headers overrides this.</div>
  {/if}

  {#if auth.type === "none"}
    <p class="faint line">No auth header will be added.</p>
  {:else if auth.type === "inherit"}
    <div class="inherit">
      {#if inheritFrom}
        <span>Inherits <b>{LABEL[inheritFrom.auth.type]}</b> from {inheritFrom.name}</span>
      {:else}
        <span class="faint">Inherits the collection's auth once this request is saved to a collection.</span>
      {/if}
      {#if inheritFrom && oneditcollection}
        <button class="link" type="button" onclick={oneditcollection}>Edit collection auth</button>
      {/if}
    </div>
  {:else if auth.type === "basic"}
    <label class="field">
      <span>Username</span>
      <VarInput {lookup} value={auth.username ?? ""} ariaLabel="Username" testid="auth-username" oninput={(v) => ((auth.username = v), onchange?.())} />
    </label>
    <div class="field">
      <span>Password</span>
      <div class="pw">
        {#if showSecret}
          <VarInput {lookup} value={auth.password ?? ""} ariaLabel="Password" testid="auth-password" oninput={(v) => ((auth.password = v), onchange?.())} />
        {:else}
          <input class="input mono" type="password" value={auth.password ?? ""} aria-label="Password" data-testid="auth-password" oninput={(e) => ((auth.password = e.currentTarget.value), onchange?.())} />
        {/if}
        <button class="icon-btn eye" type="button" aria-label={showSecret ? "Hide password" : "Show password"} aria-pressed={showSecret} onclick={() => (showSecret = !showSecret)}><Icon name={showSecret ? "eye-off" : "eye"} /></button>
      </div>
    </div>
  {:else if auth.type === "bearer"}
    <div class="field">
      <span>Token</span>
      <div class="pw">
        {#if showSecret}
          <VarInput {lookup} value={auth.token ?? ""} ariaLabel="Token" testid="auth-token" oninput={(v) => ((auth.token = v), onchange?.())} />
        {:else}
          <input class="input mono" type="password" value={auth.token ?? ""} aria-label="Token" data-testid="auth-token" oninput={(e) => ((auth.token = e.currentTarget.value), onchange?.())} />
        {/if}
        <button class="icon-btn eye" type="button" aria-label={showSecret ? "Hide token" : "Show token"} aria-pressed={showSecret} onclick={() => (showSecret = !showSecret)}><Icon name={showSecret ? "eye-off" : "eye"} /></button>
      </div>
    </div>
  {:else if auth.type === "apikey" && auth.apiKey}
    {@const ak = auth.apiKey}
    <label class="field">
      <span>Key</span>
      <VarInput {lookup} value={ak.key} ariaLabel="API key name" testid="auth-apikey-key" oninput={(v) => ((ak.key = v), onchange?.())} />
    </label>
    <label class="field">
      <span>Value</span>
      <VarInput {lookup} value={ak.value} ariaLabel="API key value" testid="auth-apikey-value" oninput={(v) => ((ak.value = v), onchange?.())} />
    </label>
    <div class="field">
      <span>Add to</span>
      <div class="seg" role="group" aria-label="Add API key to">
        <button type="button" aria-pressed={ak.in === "header"} onclick={() => ((ak.in = "header"), onchange?.())}>Header</button>
        <button type="button" aria-pressed={ak.in === "query"} onclick={() => ((ak.in = "query"), onchange?.())}>Query</button>
      </div>
    </div>
  {/if}
</div>

<style>
  .auth {
    display: flex;
    flex-direction: column;
    gap: var(--sp-5);
  }
  .type .select-wrap {
    width: 180px;
  }
  .line {
    margin: 0;
  }
  .inherit {
    display: flex;
    gap: var(--sp-4);
    align-items: center;
    flex-wrap: wrap;
  }
  /* DR-012: the eye toggle sits inside the field */
  .pw {
    position: relative;
    display: flex;
    align-items: center;
  }
  .pw > :global(input),
  .pw > :global(.vi) {
    flex: 1;
  }
  .pw :global(input) {
    padding-right: 28px;
  }
  .eye {
    position: absolute;
    right: 2px;
    top: 2px;
    z-index: 2;
  }
  .seg {
    align-self: flex-start;
  }
</style>

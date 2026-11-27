<script lang="ts">
  import { app } from "../../stores/app.svelte";
  import { tryTarget } from "../../util/grpc";
  import Icon from "../Icon.svelte";

  /** Settings tab of a gRPC request (AC-GRPC-9): deadline, Verify TLS and the TLS mode from the URL. */
  const r = $derived(app.request);
  const target = $derived(tryTarget(r.url));
</script>

<div class="settings" data-testid="grpc-settings">
  <section>
    <h4>Call</h4>
    <div class="field">
      <span>Target</span>
      {#if target}
        <code class="mono selectable" data-testid="grpc-tls-line">TLS · {target.tls ? "on" : "off"} ({target.origin})</code>
      {:else}
        <span class="faint small">Enter a grpc:// or grpcs:// URL</span>
      {/if}
    </div>
    <label class="field">
      <span>Deadline (ms) — 0 = none</span>
      <input
        class="input mono"
        type="number"
        min="0"
        step="1000"
        value={r.settings.timeoutMs}
        data-testid="grpc-deadline"
        oninput={(e) => (app.request.settings.timeoutMs = Math.max(0, Math.round(Number(e.currentTarget.value) || 0)))}
      />
      <span class="faint small">Unary calls only: streams have no deadline (it limits connecting).</span>
    </label>
    <label class="checkbox">
      <input type="checkbox" checked={r.settings.verifyTls} onchange={(e) => (app.request.settings.verifyTls = e.currentTarget.checked)} data-testid="grpc-verify-tls" />Verify TLS certificates (grpcs://)
    </label>
    {#if !r.settings.verifyTls}
      <div class="hint warn"><Icon name="alert" />Self-signed and invalid certificates will be accepted.</div>
    {/if}
  </section>
</div>

<style>
  .settings {
    flex: 1;
    overflow: auto;
    padding: var(--sp-4) var(--sp-5) var(--sp-6);
  }
  section {
    display: flex;
    flex-direction: column;
    gap: var(--sp-4);
    max-width: 560px;
  }
  h4 {
    margin: 0;
    font-size: var(--fs-sm);
    font-weight: var(--fw-medium);
    color: var(--text-2);
  }
  code {
    font-size: var(--fs-code);
    color: var(--text-2);
  }
  .small {
    font-size: var(--fs-sm);
  }
</style>

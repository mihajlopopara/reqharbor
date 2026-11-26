<script lang="ts">
  import { toasts } from "../stores/toasts.svelte";
  import Icon from "./Icon.svelte";

  const ICON = { success: "check-circle", info: "info", warning: "alert", error: "x-circle" } as const;
</script>

<div class="toasts" data-testid="toasts">
  {#each toasts.items as t (t.id)}
    <div
      class="toast pop {t.kind}"
      role={t.kind === "error" ? "alert" : "status"}
      data-testid="toast"
      onmouseenter={() => toasts.pause(t.id)}
      onmouseleave={() => toasts.resume(t.id)}
      onfocusin={() => toasts.pause(t.id)}
      onfocusout={() => toasts.resume(t.id)}
    >
      <span class="ic"><Icon name={ICON[t.kind]} /></span>
      <span class="msg">{t.message}</span>
      {#if t.action && t.actionLabel}
        <button
          class="link"
          type="button"
          onclick={() => {
            t.action?.();
            toasts.dismiss(t.id);
          }}>{t.actionLabel}</button
        >
      {/if}
      <button class="icon-btn" type="button" aria-label="Dismiss" onclick={() => toasts.dismiss(t.id)}><Icon name="x" small /></button>
    </div>
  {/each}
</div>

<style>
  .toasts {
    position: fixed;
    right: 16px;
    bottom: 16px;
    z-index: var(--z-toast);
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: flex-end;
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    display: flex;
    align-items: center;
    gap: var(--sp-4);
    min-height: 40px;
    max-width: 360px;
    padding: var(--sp-3) var(--sp-3) var(--sp-3) var(--sp-5);
    background: var(--bg-raised);
    border-radius: var(--r-md);
    box-shadow: var(--shadow-popover);
  }
  .msg {
    flex: 1;
    word-break: break-word;
  }
  .ic {
    display: inline-flex;
  }
  .success .ic {
    color: var(--success);
  }
  .info .ic {
    color: var(--info);
  }
  .warning .ic {
    color: var(--warning);
  }
  .error .ic {
    color: var(--danger);
  }
</style>

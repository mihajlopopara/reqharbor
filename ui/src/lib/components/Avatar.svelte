<script lang="ts">
  import { initials } from "../util/account";

  /** Initials only: provider images would mean third-party requests (UX-D2). */
  let { name, email = "", size = 20 }: { name: string; email?: string; size?: 20 | 32 } = $props();
</script>

<span class="av" class:big={size === 32} aria-hidden="true">{initials(name, email)}</span>

<style>
  .av {
    width: 20px;
    height: 20px;
    border-radius: 50%;
    background: var(--bg-active);
    color: var(--text-1);
    font-size: var(--fs-xs);
    font-weight: var(--fw-semibold);
    display: inline-flex;
    align-items: center;
    justify-content: center;
    flex: none;
    user-select: none;
  }
  .big {
    width: 32px;
    height: 32px;
    font-size: var(--fs-md);
  }
</style>

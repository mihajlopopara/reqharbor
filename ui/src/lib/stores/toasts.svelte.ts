export type ToastKind = "success" | "info" | "warning" | "error";

export interface Toast {
  id: number;
  kind: ToastKind;
  message: string;
  actionLabel?: string;
  action?: () => void;
  /** ms; 0 = persistent */
  timeout: number;
}

let nextId = 1;

class ToastStore {
  items = $state<Toast[]>([]);
  private timers = new Map<number, ReturnType<typeof setTimeout>>();
  private remaining = new Map<number, number>();
  private startedAt = new Map<number, number>();

  push(kind: ToastKind, message: string, opts: { actionLabel?: string; action?: () => void; timeout?: number } = {}): number {
    const id = nextId++;
    const timeout = opts.timeout ?? (kind === "error" ? 0 : opts.action ? 8000 : 4000);
    this.items = [...this.items, { id, kind, message, actionLabel: opts.actionLabel, action: opts.action, timeout }].slice(-3);
    if (timeout > 0) this.schedule(id, timeout);
    return id;
  }

  success = (m: string, o?: { actionLabel?: string; action?: () => void }) => this.push("success", m, o);
  info = (m: string, o?: { actionLabel?: string; action?: () => void }) => this.push("info", m, o);
  warning = (m: string, o?: { actionLabel?: string; action?: () => void; timeout?: number }) => this.push("warning", m, o);
  error = (m: string, o?: { actionLabel?: string; action?: () => void; timeout?: number }) => this.push("error", m, o);

  private schedule(id: number, ms: number) {
    this.remaining.set(id, ms);
    this.startedAt.set(id, Date.now());
    this.timers.set(id, setTimeout(() => this.dismiss(id), ms));
  }

  pause(id: number) {
    const t = this.timers.get(id);
    if (!t) return;
    clearTimeout(t);
    this.timers.delete(id);
    const left = (this.remaining.get(id) ?? 0) - (Date.now() - (this.startedAt.get(id) ?? Date.now()));
    this.remaining.set(id, Math.max(1000, left));
  }

  resume(id: number) {
    const left = this.remaining.get(id);
    if (left && !this.timers.has(id)) this.schedule(id, left);
  }

  dismiss(id: number) {
    const t = this.timers.get(id);
    if (t) clearTimeout(t);
    this.timers.delete(id);
    this.remaining.delete(id);
    this.items = this.items.filter((x) => x.id !== id);
  }
}

export const toasts = new ToastStore();

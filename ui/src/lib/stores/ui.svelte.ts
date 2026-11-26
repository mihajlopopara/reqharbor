/** Transient UI state: sidebar tab, menus, dialogs. */
import type { ShareKind } from "../api/types";

export type SidebarTab = "activity" | "collections" | "env";

export interface MenuItem {
  label?: string;
  shortcut?: string;
  danger?: boolean;
  disabled?: boolean;
  checked?: boolean;
  separator?: boolean;
  testid?: string;
  action?: () => void;
}

export interface MenuState {
  items: MenuItem[];
  x: number;
  y: number;
  /** element to restore focus to */
  returnFocus: HTMLElement | null;
  /** min width (e.g. anchored to the trigger) */
  minWidth?: number;
}

export interface ConfirmState {
  title: string;
  body: string;
  confirmLabel: string;
  danger: boolean;
  resolve: (ok: boolean) => void;
}

export type PickerMode =
  | { kind: "saveRequest" }
  | { kind: "saveHistory"; historyId: string }
  | { kind: "moveRequest"; requestId: string }
  | { kind: "moveFolder"; folderId: string };

export type ImportTab = "curl" | "openapi" | "link";

class UiStore {
  sidebarTab = $state<SidebarTab>("activity");
  filters = $state<Record<SidebarTab, string>>({ activity: "", collections: "", env: "" });
  menu = $state<MenuState | null>(null);
  confirmState = $state<ConfirmState | null>(null);
  importDialog = $state<{ tab: ImportTab; text?: string; path?: string } | null>(null);
  shareDialog = $state<{ kind: ShareKind; entityId: string; name: string } | null>(null);
  settingsDialog = $state<{ section: "general" | "sync" | "shares" | "shortcuts" | "about" } | null>(null);
  picker = $state<PickerMode | null>(null);
  envPicker = $state(false);
  exportCurl = $state<{ anchor: HTMLElement | null } | null>(null);
  /** Tree node or env being renamed inline. */
  renaming = $state<{ kind: "collection" | "folder" | "request" | "environment" | "project"; id: string } | null>(null);
  /** Selected tree node (for New Folder / F2). */
  selectedNode = $state<{ kind: "collection" | "folder" | "request"; id: string; collectionId: string } | null>(null);
  selectedEnvId = $state<string | null>(null);
  selectedHistoryId = $state<string | null>(null);
  expanded = $state<Set<string>>(new Set());
  focusFilterTick = $state(0);
  focusUrlTick = $state(0);
  renameRequestTick = $state(0);
  requestSettingsTick = $state(0);
  formatTick = $state(0);

  get anyModalOpen(): boolean {
    return !!(this.confirmState || this.importDialog || this.shareDialog || this.settingsDialog || this.picker || this.envPicker);
  }

  openMenu(items: MenuItem[], anchor: HTMLElement | { x: number; y: number }, opts: { alignRight?: boolean } = {}) {
    let x: number;
    let y: number;
    let minWidth: number | undefined;
    let returnFocus: HTMLElement | null = null;
    if (anchor instanceof HTMLElement) {
      const r = anchor.getBoundingClientRect();
      x = opts.alignRight ? r.right : r.left;
      y = r.bottom + 4;
      returnFocus = anchor;
      if (!opts.alignRight) minWidth = Math.max(200, r.width);
      if (opts.alignRight) x = r.right - 220;
    } else {
      x = anchor.x;
      y = anchor.y;
      returnFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    }
    this.menu = { items, x, y, returnFocus, minWidth };
  }

  closeMenu(restoreFocus = true) {
    const m = this.menu;
    this.menu = null;
    if (restoreFocus) m?.returnFocus?.focus();
  }

  confirm(opts: { title: string; body: string; confirmLabel?: string; danger?: boolean }): Promise<boolean> {
    return new Promise((resolve) => {
      this.confirmState = {
        title: opts.title,
        body: opts.body,
        confirmLabel: opts.confirmLabel ?? "Delete",
        danger: opts.danger ?? true,
        resolve,
      };
    });
  }

  toggleExpanded(id: string, open?: boolean) {
    const next = new Set(this.expanded);
    const want = open ?? !next.has(id);
    if (want) next.add(id);
    else next.delete(id);
    this.expanded = next;
  }

  expandAll(ids: string[]) {
    this.expanded = new Set([...this.expanded, ...ids]);
  }
}

export const ui = new UiStore();

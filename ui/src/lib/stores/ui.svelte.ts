/** Transient UI state: sidebar tab, menus, dialogs. */
import type { NodeKind, ShareKind } from "../api/types";

export type SidebarTab = "activity" | "collections" | "env";

export interface MenuItem {
  label?: string;
  /** A non-interactive heading row (skipped by focus and type-ahead). */
  heading?: boolean;
  /** Second line under the label. */
  sublabel?: string;
  /** A heading styled as a group title (uppercase, muted), e.g. "PERSONAL" (UX §5.1). */
  group?: boolean;
  /** Right-aligned text on a heading, e.g. the team role. */
  trailing?: string;
  /** 16 px icon before the label (binding icons). */
  icon?: string;
  /** Why the item is disabled: shown as `title` and `aria-description`. */
  reason?: string;
  title?: string;
  /** Accessible name when it should include more than the label ("Payments API, Acme"). */
  ariaLabel?: string;
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
  /** Type-to-confirm (UX §4.4 pattern): the confirm button stays disabled until this is typed. */
  typeToConfirm?: string;
  /** Bullet lines under the body ("2 projects and everything in them"). */
  details?: string[];
  /** One checkbox ("Also revoke all active invite links (3)"); its value is returned. */
  checkbox?: { label: string; checked: boolean; testid?: string };
  resolve: (r: { ok: boolean; checked?: boolean }) => void;
}

export interface ConfirmOptions {
  title: string;
  body: string;
  confirmLabel?: string;
  danger?: boolean;
  typeToConfirm?: string;
  details?: string[];
  checkbox?: { label: string; checked: boolean; testid?: string };
}

export type PickerMode =
  | { kind: "saveRequest" }
  | { kind: "saveHistory"; historyId: string }
  | { kind: "moveRequest"; requestId: string }
  | { kind: "moveFolder"; folderId: string };

export type SettingsSection = "account" | "general" | "sync" | "teams" | "shares" | "shortcuts" | "about";

export type ImportTab = "curl" | "openapi" | "link";

class UiStore {
  sidebarTab = $state<SidebarTab>("activity");
  filters = $state<Record<SidebarTab, string>>({ activity: "", collections: "", env: "" });
  menu = $state<MenuState | null>(null);
  confirmState = $state<ConfirmState | null>(null);
  importDialog = $state<{ tab: ImportTab; text?: string; path?: string } | null>(null);
  shareDialog = $state<{ kind: ShareKind; entityId: string; name: string } | null>(null);
  settingsDialog = $state<{ section: SettingsSection; teamId?: string } | null>(null);
  picker = $state<PickerMode | null>(null);
  envPicker = $state(false);
  exportCurl = $state<{ anchor: HTMLElement | null } | null>(null);
  /** Personal sync dialogs (UX §5.2, §5.3, §10, §11, §4.3/§6.9). */
  newProject = $state(false);
  syncLocation = $state<{ projectId: string } | null>(null);
  firstSync = $state(false);
  mergeLegacy = $state(false);
  spaceNotice = $state<{ mode: "foreign" | "removed"; spaceIds: string[] } | null>(null);
  /** Teams (UX §7–§9). */
  joinTeam = $state<{ url?: string } | null>(null);
  createTeam = $state(false);
  invite = $state<{ teamId: string; teamName: string; role: string } | null>(null);
  transferOwnership = $state<{ teamId: string } | null>(null);
  moveToTeam = $state<{ projectId: string; spaceId?: string | null } | null>(null);
  copyToProject = $state<{ kind: NodeKind; id: string; name: string; mode: "copy" | "duplicate"; targetProjectId?: string } | null>(null);
  /** Open Settings › Sync with Advanced expanded (token sync). */
  syncAdvanced = $state(false);
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
    return !!(
      this.confirmState ||
      this.importDialog ||
      this.shareDialog ||
      this.settingsDialog ||
      this.picker ||
      this.envPicker ||
      this.newProject ||
      this.syncLocation ||
      this.firstSync ||
      this.mergeLegacy ||
      this.spaceNotice ||
      this.joinTeam ||
      this.createTeam ||
      this.invite ||
      this.transferOwnership ||
      this.moveToTeam ||
      this.copyToProject
    );
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
    return this.confirmEx(opts).then((r) => r.ok);
  }

  /** `confirm` with type-to-confirm, detail lines and a checkbox (UX §7.2, §7.3). */
  confirmEx(opts: ConfirmOptions): Promise<{ ok: boolean; checked?: boolean }> {
    return new Promise((resolve) => {
      this.confirmState = {
        title: opts.title,
        body: opts.body,
        confirmLabel: opts.confirmLabel ?? "Delete",
        danger: opts.danger ?? true,
        typeToConfirm: opts.typeToConfirm,
        details: opts.details,
        checkbox: opts.checkbox ? { ...opts.checkbox } : undefined,
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
